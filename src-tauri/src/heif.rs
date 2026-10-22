//! Optional HEIF/HEIC decoder: libheif, loaded at run time when present. It is not shipped
//! with the app. On Windows the user can download the build in `native/windows-x64/libheif/`
//! (made by `scripts/build-libheif.ps1`): its manifest is embedded here and only files
//! matching it are installed or loaded from the config folder. Linux and macOS use a system
//! libheif (package manager, Homebrew) when one is installed. Phones use the OS decoder.

use serde::Serialize;
use tauri::AppHandle;

use crate::error::{CmdError, CmdResult};

#[derive(Serialize)]
pub struct HeifStatus {
    pub available: bool,
    pub version: Option<String>,
    pub path: Option<String>,
    /// The app can download the decoder on this device.
    pub installable: bool,
    pub installing: bool,
    /// An image could not be shown for want of the decoder (since the app started).
    pub needed: bool,
    /// Download size in bytes.
    pub size: u64,
    /// Why it did not load from where it should have been (wrong or missing files).
    pub load_errors: Vec<String>,
}

#[tauri::command]
pub async fn heif_status() -> CmdResult<HeifStatus> {
    tauri::async_runtime::spawn_blocking(imp::status).await.map_err(|e| CmdError::new("internal", e))
}

/// Downloads and installs the decoder in the background (task `download_heif`); again
/// when it is already there (the files are checked and replaced).
#[tauri::command]
pub async fn heif_install(app: AppHandle) -> CmdResult<()> {
    imp::install(app)
}

/// Forgets every thumbnail of a HEIF image that could not be made, so they are made again
/// (with whichever decoder is there now). Returns how many.
#[tauri::command]
pub async fn heif_retry_failed(app: AppHandle) -> CmdResult<usize> {
    use tauri::{Emitter, Manager};
    let ws = app.state::<crate::AppState>().workspace.read().unwrap().clone().ok_or_else(|| CmdError::new("no_bundle", "no bundle is open"))?;
    let n = tauri::async_runtime::spawn_blocking(move || -> CmdResult<usize> {
        let n = ws.write_db.lock().unwrap().reset_image_thumbnails_with_ext(&EXTENSIONS)?;
        crate::applog::info("heif", format!("{n} HEIF thumbnails will be made again (decoder: {})", describe()));
        Ok(n)
    })
    .await
    .map_err(|e| CmdError::new("internal", e))??;
    // Library views drop the thumbnails they gave up on, as after an install.
    let _ = app.emit("heif-installed", ());
    Ok(n)
}

/// The HEIF decoders this device has, for the log.
pub fn describe() -> String {
    let s = imp::status();
    match (s.available, s.version) {
        (true, Some(v)) => format!("libheif {v}"),
        _ => "no libheif".into(),
    }
}

pub use imp::{available, decode, init, note_needed};

/// File extensions of HEIF images.
pub const EXTENSIONS: [&str; 3] = ["heic", "heif", "hif"];

/// Whether `bytes` start like a HEIF file (ISO BMFF `ftyp` with a HEIF brand).
pub fn looks_like_heif(bytes: &[u8]) -> bool {
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    const BRANDS: [&[u8; 4]; 11] =
        [b"heic", b"heix", b"heim", b"heis", b"hevc", b"hevx", b"hevm", b"hevs", b"mif1", b"msf1", b"mif2"];
    let size = (u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize).clamp(12, bytes.len());
    // Major brand, then compatible brands after the minor version.
    std::iter::once(&bytes[8..12]).chain(bytes[16.min(size)..size].chunks_exact(4)).any(|b| BRANDS.iter().any(|x| &x[..] == b))
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
mod imp {
    use std::ffi::{CStr, c_char, c_int, c_void};
    use std::io::Read as _;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};

    use image::{DynamicImage, RgbImage, RgbaImage};
    use serde::Deserialize;
    use sha2::{Digest, Sha256};
    use tauri::{AppHandle, Emitter, Manager};

    use super::HeifStatus;
    use crate::error::{CmdError, CmdResult};
    use crate::tasks::{self, Unit};

    #[derive(Deserialize)]
    struct Manifest {
        version: String,
        library: String,
        files: Vec<ManifestFile>,
    }

    #[derive(Deserialize)]
    struct ManifestFile {
        name: String,
        size: u64,
        sha256: String,
    }

    #[cfg(windows)]
    const MANIFEST: Option<&str> = Some(include_str!("../../native/windows-x64/libheif/manifest.json"));
    #[cfg(not(windows))]
    const MANIFEST: Option<&str> = None;

    fn manifest() -> Option<Manifest> {
        MANIFEST.map(|m| serde_json::from_str(m).expect("built-in libheif manifest is valid JSON"))
    }

    #[repr(C)]
    struct HeifError {
        code: c_int,
        subcode: c_int,
        message: *const c_char,
    }

    impl HeifError {
        fn check(self) -> Result<(), String> {
            if self.code == 0 {
                return Ok(());
            }
            let msg = if self.message.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(self.message) }.to_string_lossy().into_owned()
            };
            Err(format!("libheif {}/{}: {msg}", self.code, self.subcode))
        }
    }

    const COLORSPACE_RGB: c_int = 1;
    const CHROMA_INTERLEAVED_RGB: c_int = 10;
    const CHROMA_INTERLEAVED_RGBA: c_int = 11;
    const CHANNEL_INTERLEAVED: c_int = 10;

    type Ptr = *mut c_void;

    struct Api {
        _lib: libloading::Library,
        path: PathBuf,
        version: String,
        context_alloc: unsafe extern "C" fn() -> Ptr,
        context_free: unsafe extern "C" fn(Ptr),
        read_from_memory: unsafe extern "C" fn(Ptr, *const c_void, usize, *const c_void) -> HeifError,
        primary_handle: unsafe extern "C" fn(Ptr, *mut Ptr) -> HeifError,
        handle_release: unsafe extern "C" fn(Ptr),
        has_alpha: unsafe extern "C" fn(Ptr) -> c_int,
        decode_image: unsafe extern "C" fn(Ptr, *mut Ptr, c_int, c_int, *const c_void) -> HeifError,
        plane: unsafe extern "C" fn(Ptr, c_int, *mut c_int) -> *const u8,
        width: unsafe extern "C" fn(Ptr, c_int) -> c_int,
        height: unsafe extern "C" fn(Ptr, c_int) -> c_int,
        image_release: unsafe extern "C" fn(Ptr),
    }

    impl Api {
        fn load(path: &Path) -> Result<Self, String> {
            let lib = open(path).map_err(|e| e.to_string())?;
            unsafe {
                macro_rules! sym {
                    ($name:literal) => {
                        *lib.get(concat!($name, "\0").as_bytes()).map_err(|e| e.to_string())?
                    };
                }
                let init: unsafe extern "C" fn(*const c_void) -> HeifError = sym!("heif_init");
                init(std::ptr::null()).check()?;
                let get_version: unsafe extern "C" fn() -> *const c_char = sym!("heif_get_version");
                let v = get_version();
                let version = if v.is_null() { String::new() } else { CStr::from_ptr(v).to_string_lossy().into_owned() };
                Ok(Self {
                    path: path.to_path_buf(),
                    version,
                    context_alloc: sym!("heif_context_alloc"),
                    context_free: sym!("heif_context_free"),
                    read_from_memory: sym!("heif_context_read_from_memory_without_copy"),
                    primary_handle: sym!("heif_context_get_primary_image_handle"),
                    handle_release: sym!("heif_image_handle_release"),
                    has_alpha: sym!("heif_image_handle_has_alpha_channel"),
                    decode_image: sym!("heif_decode_image"),
                    plane: sym!("heif_image_get_plane_readonly"),
                    width: sym!("heif_image_get_width"),
                    height: sym!("heif_image_get_height"),
                    image_release: sym!("heif_image_release"),
                    _lib: lib,
                })
            }
        }

        /// Decodes the primary image of `bytes`, with its rotation and mirroring applied.
        fn decode(&self, bytes: &[u8]) -> Result<DynamicImage, String> {
            unsafe {
                let ctx = (self.context_alloc)();
                if ctx.is_null() {
                    return Err("libheif: out of memory".into());
                }
                let _ctx = Release(ctx, self.context_free);
                (self.read_from_memory)(ctx, bytes.as_ptr().cast(), bytes.len(), std::ptr::null()).check()?;
                let mut handle: Ptr = std::ptr::null_mut();
                (self.primary_handle)(ctx, &mut handle).check()?;
                let _handle = Release(handle, self.handle_release);
                let alpha = (self.has_alpha)(handle) != 0;
                let chroma = if alpha { CHROMA_INTERLEAVED_RGBA } else { CHROMA_INTERLEAVED_RGB };
                let mut img: Ptr = std::ptr::null_mut();
                (self.decode_image)(handle, &mut img, COLORSPACE_RGB, chroma, std::ptr::null()).check()?;
                let _img = Release(img, self.image_release);
                let w = (self.width)(img, CHANNEL_INTERLEAVED);
                let h = (self.height)(img, CHANNEL_INTERLEAVED);
                let mut stride: c_int = 0;
                let data = (self.plane)(img, CHANNEL_INTERLEAVED, &mut stride);
                if data.is_null() || w <= 0 || h <= 0 {
                    return Err("libheif: no image data".into());
                }
                let (w, h, stride) = (w as usize, h as usize, stride as usize);
                let px = if alpha { 4 } else { 3 };
                let mut out = Vec::with_capacity(w * h * px);
                for y in 0..h {
                    out.extend_from_slice(std::slice::from_raw_parts(data.add(y * stride), w * px));
                }
                let (w, h) = (w as u32, h as u32);
                Ok(if alpha {
                    DynamicImage::ImageRgba8(RgbaImage::from_raw(w, h, out).ok_or("libheif: bad size")?)
                } else {
                    DynamicImage::ImageRgb8(RgbImage::from_raw(w, h, out).ok_or("libheif: bad size")?)
                })
            }
        }
    }

    /// Frees a libheif object when dropped.
    struct Release(Ptr, unsafe extern "C" fn(Ptr));

    impl Drop for Release {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { (self.1)(self.0) }
            }
        }
    }

    // libheif objects are created and freed per call; the library itself is thread safe.
    unsafe impl Send for Api {}
    unsafe impl Sync for Api {}

    #[cfg(windows)]
    fn open(path: &Path) -> Result<libloading::Library, libloading::Error> {
        // libde265.dll sits next to heif.dll: search the library's own folder for it.
        use libloading::os::windows::{LOAD_WITH_ALTERED_SEARCH_PATH, Library};
        unsafe { Library::load_with_flags(path, LOAD_WITH_ALTERED_SEARCH_PATH) }.map(Into::into)
    }

    #[cfg(not(windows))]
    fn open(path: &Path) -> Result<libloading::Library, libloading::Error> {
        unsafe { libloading::Library::new(path) }
    }

    struct Loaded {
        tried: bool,
        api: Option<Arc<Api>>,
        /// Why the places it should have been did not work.
        errors: Vec<String>,
    }

    static LOADED: Mutex<Loaded> = Mutex::new(Loaded { tried: false, api: None, errors: Vec::new() });
    static INSTALL_DIR: OnceLock<PathBuf> = OnceLock::new();
    static NEEDED: AtomicBool = AtomicBool::new(false);
    static INSTALLING: AtomicBool = AtomicBool::new(false);
    static APP: OnceLock<AppHandle> = OnceLock::new();

    pub fn init(app: &AppHandle) {
        if let Ok(dir) = app.path().app_config_dir() {
            let dir = dir.join("libs").join("libheif");
            // Files a reinstall moved aside while they were loaded.
            for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                if e.file_name().to_string_lossy().contains(".old-") {
                    let _ = std::fs::remove_file(e.path());
                }
            }
            let _ = INSTALL_DIR.set(dir);
        }
        let _ = APP.set(app.clone());
    }

    /// Whether every manifest file in `dir` is present and matches its hash.
    #[cfg(test)]
    fn verified(dir: &Path, m: &Manifest) -> bool {
        check(dir, m).is_ok()
    }

    /// Why `dir` does not hold the manifest's files, if it does not.
    fn check(dir: &Path, m: &Manifest) -> Result<(), String> {
        for f in &m.files {
            let p = dir.join(&f.name);
            match std::fs::metadata(&p) {
                Err(_) => return Err(format!("{} is missing", f.name)),
                Ok(md) if md.len() != f.size => return Err(format!("{} has {} bytes, expected {}", f.name, md.len(), f.size)),
                Ok(_) => {}
            }
            let hash = std::fs::read(&p).map(|b| hex(&Sha256::digest(&b))).map_err(|e| format!("{}: {e}", f.name))?;
            if hash != f.sha256 {
                return Err(format!("{} does not match its SHA-256", f.name));
            }
        }
        Ok(())
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Where libheif may be, in order: the downloaded copy (checked against the manifest),
    /// next to the executable, then the system's library.
    fn candidates(errors: &mut Vec<String>) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let (Some(dir), Some(m)) = (INSTALL_DIR.get(), manifest()) {
            match check(dir, &m) {
                Ok(()) => out.push(dir.join(&m.library)),
                // Not installed is not worth reporting.
                Err(e) if dir.exists() => errors.push(format!("{}: {e}", dir.display())),
                Err(_) => {}
            }
        }
        let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
        if cfg!(windows) {
            if let Some(d) = exe_dir {
                out.push(d.join("libheif").join("heif.dll"));
                out.push(d.join("heif.dll"));
            }
        } else if cfg!(target_os = "macos") {
            if let Some(d) = exe_dir {
                out.push(d.join("../Frameworks/libheif.1.dylib"));
            }
            for p in ["/opt/homebrew/lib/libheif.1.dylib", "/usr/local/lib/libheif.1.dylib", "libheif.1.dylib"] {
                out.push(p.into());
            }
        } else {
            out.push("libheif.so.1".into());
        }
        out
    }

    fn api() -> Option<Arc<Api>> {
        let mut l = LOADED.lock().unwrap_or_else(|e| e.into_inner());
        if !l.tried {
            l.tried = true;
            let mut errors = Vec::new();
            for p in candidates(&mut errors) {
                // A bare name or missing file is only a place it might have been.
                let explicit = p.components().count() > 1;
                if explicit && !p.exists() {
                    continue;
                }
                match Api::load(&p) {
                    Ok(a) => {
                        crate::applog::info("heif", format!("libheif {} loaded from {}", a.version, p.display()));
                        l.api = Some(Arc::new(a));
                        break;
                    }
                    Err(e) if explicit => errors.push(format!("{}: {e}", p.display())),
                    Err(_) => {}
                }
            }
            if l.api.is_none() {
                crate::applog::info("heif", format!("libheif not loaded{}", if errors.is_empty() { String::new() } else { format!(": {}", errors.join("; ")) }));
            }
            l.errors = errors;
        }
        l.api.clone()
    }

    /// Forgets a failed lookup so the next use searches again (after an install).
    fn reload() -> bool {
        let mut l = LOADED.lock().unwrap_or_else(|e| e.into_inner());
        if l.api.is_none() {
            l.tried = false;
        }
        drop(l);
        api().is_some()
    }

    pub fn available() -> bool {
        api().is_some()
    }

    pub fn decode(bytes: &[u8]) -> Option<Result<DynamicImage, String>> {
        api().map(|a| a.decode(bytes))
    }

    /// Records that a HEIF image could not be shown without the decoder; the library
    /// window then offers to install it (`heif-needed`, sent once).
    pub fn note_needed() {
        if manifest().is_some() && api().is_none() && !NEEDED.swap(true, Ordering::Relaxed)
            && let Some(app) = APP.get()
        {
            let _ = app.emit("heif-needed", ());
        }
    }

    pub fn status() -> HeifStatus {
        let a = api();
        let m = manifest();
        HeifStatus {
            available: a.is_some(),
            version: a.as_ref().map(|a| a.version.clone()),
            path: a.as_ref().map(|a| a.path.display().to_string()),
            installable: m.is_some() && INSTALL_DIR.get().is_some(),
            installing: INSTALLING.load(Ordering::Relaxed),
            needed: NEEDED.load(Ordering::Relaxed),
            size: m.map(|m| m.files.iter().map(|f| f.size).sum()).unwrap_or(0),
            load_errors: LOADED.lock().unwrap_or_else(|e| e.into_inner()).errors.clone(),
        }
    }

    pub fn install(app: AppHandle) -> CmdResult<()> {
        let m = manifest().ok_or_else(|| CmdError::new("unsupported", "no HEIF decoder download for this platform"))?;
        let dir = INSTALL_DIR.get().cloned().ok_or_else(|| CmdError::new("io", "no config folder"))?;
        if INSTALLING.swap(true, Ordering::Relaxed) {
            return Ok(());
        }
        let urls = crate::recent::cached_app_settings().decoders.heif_download_urls;
        std::thread::spawn(move || {
            let total = m.files.iter().map(|f| f.size).sum();
            let task = tasks::begin("download_heif", format!("libheif {}", m.version), Unit::Bytes, Some(total));
            crate::applog::info("heif", format!("installing libheif {} into {} from {}", m.version, dir.display(), urls.join(", ")));
            let result = fetch_all(&m, &urls, &dir, &task);
            INSTALLING.store(false, Ordering::Relaxed);
            match result.and_then(|()| {
                if reload() {
                    Ok(())
                } else {
                    let why = LOADED.lock().unwrap_or_else(|e| e.into_inner()).errors.join("; ");
                    Err(CmdError::new("heif", format!("the downloaded decoder did not load: {why}")))
                }
            }) {
                Ok(()) => {
                    crate::applog::info("heif", format!("installed; {}", super::describe()));
                    task.finish();
                    // Images recorded as unreadable get another try; HEIF ones even if a
                    // decoder failed on them (FFmpeg cannot read some HEIF from a stream, and
                    // encrypted shards never hand it a file).
                    if let Some(ws) = app.state::<crate::AppState>().workspace.read().unwrap().as_ref() {
                        let db = ws.write_db.lock().unwrap();
                        let _ = db.reset_unsupported_image_thumbnails();
                        let _ = db.reset_image_thumbnails_with_ext(&super::EXTENSIONS);
                    }
                    let _ = app.emit("heif-installed", ());
                }
                Err(e) => {
                    crate::applog::warn("heif", format!("install failed: {}", e.message));
                    task.fail(e.message);
                }
            }
        });
        Ok(())
    }

    /// Downloads every manifest file from the first folder in `urls` that has them all,
    /// checking each against its hash before anything is moved into `dir`.
    fn fetch_all(m: &Manifest, urls: &[String], dir: &Path, task: &tasks::Task) -> CmdResult<()> {
        let mut last = CmdError::new("download", "no download location");
        for base in urls {
            let base = if base.ends_with('/') { base.clone() } else { format!("{base}/") };
            let mut files = Vec::new();
            let got = m.files.iter().try_for_each(|f| {
                task.detail(&f.name);
                let data = fetch(&format!("{base}{}", f.name), f.size, task)?;
                if data.len() as u64 != f.size || hex(&Sha256::digest(&data)) != f.sha256 {
                    return Err(CmdError::new("download", format!("{} does not match the expected file", f.name)));
                }
                files.push((f, data));
                Ok(())
            });
            match got {
                Ok(()) => {
                    std::fs::create_dir_all(dir)?;
                    for (f, data) in files {
                        let target = dir.join(&f.name);
                        let part = dir.join(format!("{}.part", f.name));
                        std::fs::write(&part, data)?;
                        // A loaded DLL cannot be replaced or deleted on Windows, but it can be
                        // moved aside (removed at the next start).
                        if target.exists() && std::fs::remove_file(&target).is_err() {
                            std::fs::rename(&target, dir.join(format!("{}.old-{}", f.name, crate::workspace::now_ms())))?;
                        }
                        std::fs::rename(&part, &target)?;
                    }
                    crate::applog::info("heif", format!("downloaded {} files from {base}", m.files.len()));
                    return Ok(());
                }
                Err(e) => {
                    crate::applog::warn("heif", format!("download from {base} failed: {}", e.message));
                    last = e;
                }
            }
        }
        Err(last)
    }

    fn fetch(url: &str, size: u64, task: &tasks::Task) -> CmdResult<Vec<u8>> {
        let mut resp = ureq::get(url).call().map_err(|e| CmdError::new("download", format!("{url}: {e}")))?;
        // Read one byte past the expected size, so a larger file is caught without
        // reading all of it.
        let mut body = resp.body_mut().with_config().limit(size + 1).reader();
        let mut out = Vec::with_capacity(size as usize);
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = body.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
            task.add(n as u64);
        }
        Ok(out)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[cfg(windows)]
        #[test]
        fn manifest_names_the_library() {
            let m = manifest().unwrap();
            assert!(m.files.iter().any(|f| f.name == m.library));
            assert!(m.files.iter().all(|f| f.sha256.len() == 64 && f.size > 0));
        }

        /// The committed build decodes a phone-style tiled HEIC with its rotation applied.
        #[cfg(windows)]
        #[test]
        fn committed_build_decodes_tiled_heic() {
            let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../native/windows-x64/libheif");
            let m = manifest().unwrap();
            assert!(verified(&dir, &m), "native/windows-x64/libheif matches its manifest");
            let a = Api::load(&dir.join(&m.library)).unwrap();
            assert_eq!(a.version, m.version);
            let heic = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../crates/vp-media/testdata/grid.heic")).unwrap();
            assert!(super::super::looks_like_heif(&heic));
            let img = a.decode(&heic).unwrap();
            assert!(img.width() > 0 && img.height() > 0);
            assert!(a.decode(b"not an image at all").is_err());
            // A file that did not get a thumbnail elsewhere (`VP_TEST_HEIC=<file>`).
            if let Some(f) = std::env::var_os("VP_TEST_HEIC") {
                let img = a.decode(&std::fs::read(f).unwrap()).unwrap();
                eprintln!("VP_TEST_HEIC: {}x{}", img.width(), img.height());
            }
        }

        /// Serves the files of `dir` by name on a local port.
        fn serve(dir: PathBuf) -> String {
            use std::io::Write;
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            std::thread::spawn(move || {
                for mut s in listener.incoming().flatten() {
                    let mut buf = [0u8; 2048];
                    let n = std::io::Read::read(&mut s, &mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let name = req.split_whitespace().nth(1).unwrap_or("/").rsplit('/').next().unwrap_or("").to_string();
                    match std::fs::read(dir.join(&name)) {
                        Ok(body) => {
                            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                            let _ = s.write_all(&body);
                        }
                        Err(_) => {
                            let _ = write!(s, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                        }
                    }
                }
            });
            format!("http://{addr}/")
        }

        /// Files come from the first location that has them all and match the manifest;
        /// a location serving other bytes installs nothing.
        #[cfg(windows)]
        #[test]
        fn download_checks_hashes_and_falls_through() {
            let m = manifest().unwrap();
            let good = serve(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../native/windows-x64/libheif"));
            let bad_dir = tempfile::tempdir().unwrap();
            for f in &m.files {
                std::fs::write(bad_dir.path().join(&f.name), vec![0u8; f.size as usize]).unwrap();
            }
            let bad = serve(bad_dir.path().to_path_buf());
            let task = tasks::begin("download_heif", "test", Unit::Bytes, None);

            let out = tempfile::tempdir().unwrap();
            let only_bad = out.path().join("a");
            assert!(fetch_all(&m, std::slice::from_ref(&bad), &only_bad, &task).is_err());
            assert!(!only_bad.exists());

            let target = out.path().join("b");
            fetch_all(&m, &[bad, "http://127.0.0.1:9/".into(), good.trim_end_matches('/').into()], &target, &task).unwrap();
            assert!(verified(&target, &m));
            assert!(Api::load(&target.join(&m.library)).is_ok());
        }
    }
}

#[cfg(any(target_os = "android", target_os = "ios"))]
mod imp {
    use super::HeifStatus;
    use crate::error::{CmdError, CmdResult};

    pub fn init(_app: &tauri::AppHandle) {}
    pub fn available() -> bool {
        false
    }
    pub fn decode(_bytes: &[u8]) -> Option<Result<image::DynamicImage, String>> {
        None
    }
    pub fn note_needed() {}
    pub fn status() -> HeifStatus {
        HeifStatus {
            available: false,
            version: None,
            path: None,
            installable: false,
            installing: false,
            needed: false,
            size: 0,
            load_errors: Vec::new(),
        }
    }
    pub fn install(_app: tauri::AppHandle) -> CmdResult<()> {
        Err(CmdError::new("unsupported", "the OS decodes HEIF on this device"))
    }
}
