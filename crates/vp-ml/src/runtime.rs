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

/// One input of `tensors.len()` items, each of batch 1 and the same shape; `None` if they
/// differ.
pub fn stack(tensors: &[Tensor]) -> Option<Tensor> {
    let first = tensors.first()?;
    if first.shape.first() != Some(&1) || tensors.iter().any(|t| t.shape != first.shape) {
        return None;
    }
    let mut shape = first.shape.clone();
    shape[0] = tensors.len() as i64;
    let mut data = Vec::with_capacity(first.data.len() * tensors.len());
    for t in tensors {
        data.extend_from_slice(&t.data);
    }
    Some(Tensor { shape, data })
}

/// Splits the outputs of a batch of `n` into `n` outputs of batch 1; `None` when an output
/// does not lead with the batch dimension (the model cannot be run in batches).
pub fn split(out: Outputs, n: usize) -> Option<Vec<Outputs>> {
    let mut items = vec![Outputs::new(); n];
    for (name, (shape, data)) in out {
        if shape.first() != Some(&(n as i64)) || data.len() % n != 0 {
            return None;
        }
        let mut one = shape.clone();
        one[0] = 1;
        for (item, chunk) in items.iter_mut().zip(data.chunks_exact(data.len() / n.max(1))) {
            item.insert(name.clone(), (one.clone(), chunk.to_vec()));
        }
    }
    Some(items)
}

#[cfg(feature = "onnx")]
mod imp {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
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
            if let Some(dir) = c.parent().filter(|d| !d.as_os_str().is_empty()) {
                dll_search_dir(dir);
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

    /// The CUDA provider loads cuDNN (`cudnn64_9.dll`, ...) by bare name only when a model
    /// first runs, which does not search the provider's own folder: keeping cuDNN next to
    /// `onnxruntime.dll` works once that folder is in the process's DLL search.
    #[cfg(windows)]
    fn dll_search_dir(dir: &Path) {
        use std::os::windows::ffi::OsStrExt;
        unsafe extern "system" {
            fn SetDllDirectoryW(path: *const u16) -> i32;
        }
        let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe { SetDllDirectoryW(wide.as_ptr()) };
    }

    #[cfg(not(windows))]
    fn dll_search_dir(_dir: &Path) {}

    pub struct Model {
        session: Mutex<Session>,
        input: String,
        pub provider: String,
        /// The input's batch dimension is free, so several items run at once. Cleared when
        /// a batched run shows otherwise (outputs without the batch dimension).
        batch: AtomicBool,
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
                        // A dynamic (negative) leading dimension takes any batch size.
                        let batch = s
                            .inputs()
                            .iter()
                            .find(|i| i.name() == input)
                            .and_then(|i| i.dtype().tensor_shape().and_then(|sh| sh.first().copied()))
                            .is_some_and(|d| d < 0);
                        return Ok(Self { session: Mutex::new(s), input, provider: p.to_string(), batch: AtomicBool::new(batch) });
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

        /// Whether several items can run at once.
        pub fn batches(&self) -> bool {
            self.batch.load(Ordering::Relaxed)
        }

        /// Runs every tensor (each of batch 1), at most `max` per run when the model takes
        /// batches. One result per tensor: a batch that fails is retried item by item, so a
        /// bad input (or running out of GPU memory) only costs that item.
        pub fn run_many(&self, tensors: Vec<Tensor>, max: usize) -> Vec<Result<Outputs, Error>> {
            if tensors.len() < 2 || max < 2 || !self.batches() {
                return tensors.into_iter().map(|t| self.run(t)).collect();
            }
            let mut results = Vec::with_capacity(tensors.len());
            let mut rest = tensors;
            while !rest.is_empty() {
                let tail = rest.split_off(rest.len().min(max));
                let chunk = std::mem::replace(&mut rest, tail);
                let n = chunk.len();
                let batched = super::stack(&chunk).and_then(|t| self.run(t).ok()).map(|out| super::split(out, n));
                match batched {
                    Some(Some(items)) => results.extend(items.into_iter().map(Ok)),
                    other => {
                        if matches!(other, Some(None)) {
                            self.batch.store(false, Ordering::Relaxed);
                        }
                        results.extend(chunk.into_iter().map(|t| self.run(t)));
                    }
                }
            }
            results
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

        pub fn batches(&self) -> bool {
            false
        }

        pub fn run_many(&self, tensors: Vec<Tensor>, _max: usize) -> Vec<Result<Outputs, Error>> {
            tensors.into_iter().map(|_| Err(Error::Unavailable)).collect()
        }
    }
}

pub use imp::{Model, init, loaded};

/// Whether this build can run models at all (`onnx` feature).
pub const SUPPORTED: bool = cfg!(feature = "onnx");

#[cfg(test)]
mod tests {
    use super::*;

    fn t(shape: &[i64], v: f32) -> Tensor {
        Tensor { shape: shape.to_vec(), data: vec![v; shape.iter().product::<i64>() as usize] }
    }

    #[test]
    fn stacks_items_and_splits_outputs_back() {
        let s = stack(&[t(&[1, 3, 2, 2], 1.0), t(&[1, 3, 2, 2], 2.0)]).unwrap();
        assert_eq!(s.shape, vec![2, 3, 2, 2]);
        assert_eq!((s.data[0], s.data[12]), (1.0, 2.0));
        assert!(stack(&[t(&[1, 3, 2, 2], 1.0), t(&[1, 3, 4, 4], 1.0)]).is_none(), "shapes must match");
        assert!(stack(&[t(&[2, 3], 1.0)]).is_none(), "items are batch 1");

        let mut out = Outputs::new();
        out.insert("emb".into(), (vec![2, 4], vec![1.0, 1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 2.0]));
        let items = split(out, 2).unwrap();
        assert_eq!(items[1]["emb"], (vec![1, 4], vec![2.0; 4]));

        // An output without the batch dimension: the model does not batch.
        let mut out = Outputs::new();
        out.insert("x".into(), (vec![1, 8], vec![0.0; 8]));
        assert!(split(out, 2).is_none());
    }
}

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
