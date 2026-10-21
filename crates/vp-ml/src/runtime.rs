//! ONNX Runtime, loaded at run time (nothing is linked or shipped), and models running on
//! the first execution provider that works: CUDA (NVIDIA), DirectML (any GPU on Windows),
//! then the CPU.

use std::path::{Path, PathBuf};

use crate::prep::Tensor;

/// File name of the ONNX Runtime library on this platform.
pub const LIBRARY: &str = if cfg!(windows) {
    "onnxruntime.dll"
} else if cfg!(target_os = "macos") {
    "libonnxruntime.dylib"
} else {
    "libonnxruntime.so"
};

/// Where to look for the library, in order: the configured file (or folder), the app
/// folder, `ORT_DYLIB_PATH`, then the bare name for the system search path.
pub fn candidates(configured: &str, app_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let configured = configured.trim();
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        out.push(if p.is_dir() { p.join(LIBRARY) } else { p });
    }
    if let Some(d) = app_dir {
        out.push(d.join(LIBRARY));
    }
    if let Some(p) = std::env::var_os("ORT_DYLIB_PATH") {
        out.push(PathBuf::from(p));
    }
    out.push(PathBuf::from(LIBRARY));
    out
}

/// Output tensors of one run, by name.
pub type Outputs = std::collections::HashMap<String, (Vec<i64>, Vec<f32>)>;

#[cfg(feature = "onnx")]
mod imp {
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};

    use ort::environment::Environment;
    use ort::session::Session;
    use ort::value::Tensor as OrtTensor;

    use super::{Outputs, Tensor};
    use crate::Error;

    static ENV: OnceLock<(Environment, PathBuf)> = OnceLock::new();

    /// Loads ONNX Runtime from the first candidate that works (once per process). Returns
    /// the library path.
    pub fn init(candidates: &[PathBuf]) -> Result<PathBuf, Error> {
        static LOCK: Mutex<()> = Mutex::new(());
        let _one = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, p)) = ENV.get() {
            return Ok(p.clone());
        }
        let mut errors = Vec::new();
        for c in candidates {
            // A path that is not there cannot load; the bare name goes to the system search.
            if c.components().count() > 1 && !c.exists() {
                continue;
            }
            match ort::init_from(c).map_err(|e| e.to_string()).and_then(|b| b.build().map_err(|e| e.to_string())) {
                Ok(env) => {
                    let _ = ENV.set((env, c.clone()));
                    return Ok(c.clone());
                }
                Err(e) => errors.push(format!("{}: {e}", c.display())),
            }
        }
        Err(Error::Runtime(if errors.is_empty() { "ONNX Runtime was not found".into() } else { errors.join("; ") }))
    }

    pub fn loaded() -> Option<PathBuf> {
        ENV.get().map(|(_, p)| p.clone())
    }

    pub struct Model {
        session: Mutex<Session>,
        input: String,
        pub provider: String,
    }

    fn builder(env: &Environment, provider: &str, device: i32, threads: u32) -> Result<ort::session::builder::SessionBuilder, String> {
        let mut b = Session::builder(env).map_err(|e| e.to_string())?;
        if threads > 0 {
            b = b.with_intra_threads(threads as usize).map_err(|e| e.to_string())?;
        }
        let ep = match provider {
            "cuda" => Some(ort::ep::CUDA::default().with_device_id(device).build().error_on_failure()),
            #[cfg(windows)]
            "directml" => Some(ort::ep::DirectML::default().with_device_id(device).build().error_on_failure()),
            "cpu" => None,
            other => return Err(format!("'{other}' is not available in this build")),
        };
        if let Some(ep) = ep {
            b = b.with_execution_providers([ep]).map_err(|e| e.to_string())?;
        }
        Ok(b)
    }

    impl Model {
        /// Opens `path` on the first of `providers` that can run it.
        pub fn open(path: &Path, providers: &[String], device: i32, threads: u32, input_name: Option<&str>) -> Result<Self, Error> {
            let (env, _) = ENV.get().ok_or_else(|| Error::Runtime("ONNX Runtime is not loaded".into()))?;
            let mut errors = Vec::new();
            let mut list: Vec<&str> = providers.iter().map(String::as_str).collect();
            if !list.contains(&"cpu") {
                list.push("cpu");
            }
            for p in list {
                let session = builder(env, p, device, threads).and_then(|mut b| b.commit_from_file(path).map_err(|e| e.to_string()));
                match session {
                    Ok(s) => {
                        let input = match input_name {
                            Some(n) => n.to_string(),
                            None => s.inputs().first().map(|i| i.name().to_string()).ok_or_else(|| Error::Model("the model has no input".into()))?,
                        };
                        return Ok(Self { session: Mutex::new(s), input, provider: p.to_string() });
                    }
                    Err(e) => errors.push(format!("{p}: {e}")),
                }
            }
            Err(Error::Model(errors.join("; ")))
        }

        /// Runs the model on one input; every float output comes back by name.
        pub fn run(&self, t: Tensor) -> Result<Outputs, Error> {
            let input = OrtTensor::from_array((t.shape, t.data)).map_err(|e| Error::Model(e.to_string()))?;
            let mut session = self.session.lock().unwrap_or_else(|e| e.into_inner());
            let outputs = session.run(ort::inputs![self.input.as_str() => input]).map_err(|e| Error::Model(e.to_string()))?;
            let mut out = Outputs::new();
            for (name, value) in &outputs {
                if let Ok((shape, data)) = value.try_extract_tensor::<f32>() {
                    out.insert(name.to_string(), (shape.to_vec(), data.to_vec()));
                }
            }
            Ok(out)
        }
    }
}

#[cfg(not(feature = "onnx"))]
mod imp {
    use std::path::{Path, PathBuf};

    use super::{Outputs, Tensor};
    use crate::Error;

    pub fn init(_candidates: &[PathBuf]) -> Result<PathBuf, Error> {
        Err(Error::Unavailable)
    }

    pub fn loaded() -> Option<PathBuf> {
        None
    }

    pub struct Model {
        pub provider: String,
    }

    impl Model {
        pub fn open(_path: &Path, _providers: &[String], _device: i32, _threads: u32, _input: Option<&str>) -> Result<Self, Error> {
            Err(Error::Unavailable)
        }

        pub fn run(&self, _t: Tensor) -> Result<Outputs, Error> {
            Err(Error::Unavailable)
        }
    }
}

pub use imp::{Model, init, loaded};

/// Whether this build can run models at all (`onnx` feature).
pub const SUPPORTED: bool = cfg!(feature = "onnx");

/// Providers this build knows, for the settings UI.
pub fn known_providers() -> &'static [&'static str] {
    if !SUPPORTED {
        &[]
    } else if cfg!(windows) {
        &["cuda", "directml", "cpu"]
    } else {
        &["cuda", "cpu"]
    }
}
