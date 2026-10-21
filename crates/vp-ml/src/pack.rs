//! Model packs: a folder with `manifest.json` and the ONNX file(s) it names. The app ships
//! no models; packs are installed by the user (licenses differ per model), so the manifest
//! describes everything the app needs to feed and read the model.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const MANIFEST: &str = "manifest.json";

/// What a pack does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackKind {
    /// Finds faces in photos (with five landmarks for alignment).
    FaceDetector,
    /// Turns an aligned photo face into an identity vector.
    FaceEmbedder,
    /// Finds character faces in illustrations.
    AnimeDetector,
    /// Turns a character crop into an identity vector.
    AnimeEmbedder,
    /// Scores tags (general, character, rating) for a whole image.
    Tagger,
}

/// How the model's output is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    /// OpenCV YuNet: `cls_*`, `obj_*`, `bbox_*`, `kps_*` per stride (8, 16, 32).
    Yunet,
    /// Ultralytics YOLO (v5/v8 style): `[1, 4 + classes, N]` or `[1, N, 4 + classes]`.
    Yolo,
    /// One vector per input.
    Embedding,
    /// One sigmoid score per label of `labels` (SmilingWolf WD taggers).
    WdTagger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    #[default]
    Nchw,
    Nhwc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channels {
    #[default]
    Rgb,
    Bgr,
}

/// How an image is fitted into the model's input size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// Scaled to fit, padded at the right and bottom (detectors).
    #[default]
    Letterbox,
    /// Padded to a centred square, then scaled (taggers).
    PadSquare,
    /// Scaled to the exact size (crops that already have the right shape).
    Stretch,
}

fn one() -> f32 {
    1.0
}
fn ones() -> [f32; 3] {
    [1.0; 3]
}

/// Input tensor: `value = (pixel * scale - mean) / std` per channel, pixel in 0..=255.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputSpec {
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub layout: Layout,
    #[serde(default)]
    pub channels: Channels,
    #[serde(default = "one")]
    pub scale: f32,
    #[serde(default)]
    pub mean: [f32; 3],
    #[serde(default = "ones")]
    pub std: [f32; 3],
    #[serde(default)]
    pub fit: Fit,
    /// Padding colour (RGB).
    #[serde(default)]
    pub pad: [u8; 3],
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct License {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Stable id, also what settings refer to. Change `version` when weights change:
    /// vectors of different models (or versions) cannot be compared.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    pub kind: PackKind,
    pub format: OutputFormat,
    /// ONNX file, relative to the pack folder.
    pub model: String,
    /// Label file (tagger CSV), relative to the pack folder.
    #[serde(default)]
    pub labels: Option<String>,
    pub input: InputSpec,
    /// Input tensor name; the model's first input when absent.
    #[serde(default)]
    pub input_name: Option<String>,
    #[serde(default)]
    pub license: License,
    /// Where the files come from (shown to the user).
    #[serde(default)]
    pub source: String,
}

impl Manifest {
    /// `id@version`: what stored results are keyed by.
    pub fn key(&self) -> String {
        if self.version.is_empty() { self.id.clone() } else { format!("{}@{}", self.id, self.version) }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pack {
    pub manifest: Manifest,
    pub dir: PathBuf,
}

impl Pack {
    pub fn load(dir: &Path) -> Result<Self, crate::Error> {
        let text = std::fs::read_to_string(dir.join(MANIFEST))?;
        let manifest: Manifest = serde_json::from_str(&text).map_err(|e| crate::Error::Pack(format!("{}: {e}", dir.display())))?;
        let pack = Self { manifest, dir: dir.to_path_buf() };
        if !pack.model_path().is_file() {
            return Err(crate::Error::Pack(format!("{}: model file '{}' is missing", dir.display(), pack.manifest.model)));
        }
        Ok(pack)
    }

    pub fn model_path(&self) -> PathBuf {
        self.dir.join(&self.manifest.model)
    }

    pub fn labels_path(&self) -> Option<PathBuf> {
        self.manifest.labels.as_ref().map(|l| self.dir.join(l))
    }
}

/// Packs in `dirs`: each directory itself, or its direct subfolders, holding a manifest.
/// Folders that fail to load are returned as errors next to the packs.
pub fn find_packs(dirs: &[PathBuf]) -> (Vec<Pack>, Vec<String>) {
    let mut packs: Vec<Pack> = Vec::new();
    let mut errors = Vec::new();
    let mut try_dir = |d: &Path| {
        if !d.join(MANIFEST).is_file() {
            return;
        }
        match Pack::load(d) {
            Ok(p) if !packs.iter().any(|q| q.manifest.key() == p.manifest.key()) => packs.push(p),
            Ok(_) => {}
            Err(e) => errors.push(e.to_string()),
        }
    };
    for dir in dirs {
        try_dir(dir);
        let Ok(entries) = std::fs::read_dir(dir) else { continue };
        let mut subs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        subs.sort();
        for sub in subs {
            try_dir(&sub);
        }
    }
    (packs, errors)
}

/// The pack `id` of `kind`, or the first of that kind when `id` is empty.
pub fn pick<'a>(packs: &'a [Pack], kind: PackKind, id: &str) -> Option<&'a Pack> {
    packs.iter().filter(|p| p.manifest.kind == kind).find(|p| id.is_empty() || p.manifest.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_pack(root: &Path, name: &str, json: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST), json).unwrap();
        std::fs::write(dir.join("model.onnx"), b"onnx").unwrap();
        dir
    }

    #[test]
    fn finds_packs_and_reports_broken_ones() {
        let tmp = std::env::temp_dir().join(format!("vp-ml-packs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        write_pack(
            &tmp,
            "yunet",
            r#"{ "id": "yunet", "name": "YuNet", "version": "2023mar", "kind": "face_detector", "format": "yunet",
                 "model": "model.onnx", "input": { "width": 640, "height": 640, "channels": "bgr" },
                 "license": { "name": "MIT" } }"#,
        );
        write_pack(&tmp, "broken", r#"{ "id": "x" }"#);
        let (packs, errors) = find_packs(std::slice::from_ref(&tmp));
        assert_eq!(packs.len(), 1);
        assert_eq!(errors.len(), 1);
        let p = &packs[0];
        assert_eq!(p.manifest.key(), "yunet@2023mar");
        assert_eq!(p.manifest.input.channels, Channels::Bgr);
        assert_eq!(p.manifest.input.std, [1.0; 3]);
        assert!(pick(&packs, PackKind::FaceDetector, "").is_some());
        assert!(pick(&packs, PackKind::FaceDetector, "other").is_none());
        assert!(pick(&packs, PackKind::Tagger, "").is_none());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
