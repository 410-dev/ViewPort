//! Local image analysis: faces in photos, characters in illustrations, and image tags.
//!
//! Models are not shipped: each is a *model pack* (`pack`) the user installs, run through
//! ONNX Runtime loaded at run time (`runtime`) on the first execution provider that works
//! (CUDA, DirectML, CPU). Everything around the models (input tensors, reading detector
//! outputs, face alignment, tag labels, grouping) is plain Rust and works without them.
//! Like `vp-query`, this crate knows nothing about the database.

pub mod align;
pub mod cluster;
pub mod detect;
pub mod pack;
pub mod prep;
pub mod runtime;
pub mod tagger;

use image::RgbImage;
use serde::Serialize;

pub use detect::Detection;
pub use pack::{Pack, PackKind};
pub use tagger::{TagCategory, TagScore};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("image analysis is not available in this build")]
    Unavailable,
    #[error("ONNX Runtime: {0}")]
    Runtime(String),
    #[error("model: {0}")]
    Model(String),
    #[error("model pack: {0}")]
    Pack(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Photo faces (people) or illustration faces (characters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FaceKind {
    Real,
    Anime,
}

impl FaceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Real => "real",
            Self::Anime => "anime",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "real" => Some(Self::Real),
            "anime" => Some(Self::Anime),
            _ => None,
        }
    }
}

/// Tuning (from `AnalysisSettings`).
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub detect_score: f32,
    pub nms_iou: f32,
    pub min_face_px: f32,
    pub keep_score: f32,
    /// Character crops for the embedder: the face box grown by this factor.
    pub crop_scale: f32,
    /// Edge of the face picture kept for the people view.
    pub crop_px: u32,
}

/// One face found in an image.
#[derive(Debug, Clone)]
pub struct Found {
    pub kind: FaceKind,
    /// Box as fractions of the image: x, y, width, height.
    pub bbox: [f32; 4],
    pub score: f32,
    /// Unit identity vector; `None` without an embedder for this kind.
    pub embedding: Option<Vec<f32>>,
    /// The face, `crop_px` square (aligned for photos).
    pub crop: RgbImage,
}

/// A loaded model of a pack.
pub struct Loaded {
    pub pack: Pack,
    pub model: runtime::Model,
    labels: Vec<tagger::Label>,
}

impl Loaded {
    pub fn open(pack: &Pack, providers: &[String], device: i32, threads: u32) -> Result<Self> {
        let model = runtime::Model::open(&pack.model_path(), providers, device, threads, pack.manifest.input_name.as_deref())?;
        let labels = match pack.labels_path() {
            Some(p) => tagger::parse_labels(&std::fs::read_to_string(p)?)?,
            None => Vec::new(),
        };
        if pack.manifest.kind == PackKind::Tagger && labels.is_empty() {
            return Err(Error::Pack(format!("{}: a tagger needs its label file", pack.manifest.id)));
        }
        Ok(Self { pack: pack.clone(), model, labels })
    }

    fn detect(&self, img: &RgbImage, o: &Options) -> Result<Vec<Detection>> {
        let spec = &self.pack.manifest.input;
        let (t, place) = prep::to_tensor(img, spec);
        let out = self.model.run(t)?;
        let raw = match self.pack.manifest.format {
            pack::OutputFormat::Yunet => detect::decode_yunet(&out, spec.width, spec.height, o.detect_score),
            pack::OutputFormat::Yolo => match largest(&out) {
                Some((shape, data)) => detect::decode_yolo(shape, data, o.detect_score),
                None => Vec::new(),
            },
            other => return Err(Error::Pack(format!("{}: {other:?} is not a detector format", self.pack.manifest.id))),
        };
        Ok(detect::finish(raw, &place, img.width(), img.height(), o.nms_iou, o.min_face_px))
    }

    /// A unit vector for an image already cropped for this embedder.
    fn embed(&self, crop: &RgbImage) -> Result<Vec<f32>> {
        let (t, _) = prep::to_tensor(crop, &self.pack.manifest.input);
        let out = self.model.run(t)?;
        let (_, data) = largest(&out).ok_or_else(|| Error::Model("the embedder gave no output".into()))?;
        let mut v = data.to_vec();
        cluster::normalize(&mut v);
        Ok(v)
    }

    fn tag(&self, img: &RgbImage, keep: f32) -> Result<Vec<TagScore>> {
        let (t, _) = prep::to_tensor(img, &self.pack.manifest.input);
        let out = self.model.run(t)?;
        let (_, data) = largest(&out).ok_or_else(|| Error::Model("the tagger gave no output".into()))?;
        Ok(tagger::decode(&self.labels, data, keep))
    }
}

/// The output with the most values (embedders and taggers have one that matters).
fn largest(out: &runtime::Outputs) -> Option<(&[i64], &[f32])> {
    out.values().max_by_key(|(_, d)| d.len()).map(|(s, d)| (s.as_slice(), d.as_slice()))
}

/// The models chosen for a library, each optional.
#[derive(Default)]
pub struct Engine {
    pub face_detector: Option<Loaded>,
    pub face_embedder: Option<Loaded>,
    pub anime_detector: Option<Loaded>,
    pub anime_embedder: Option<Loaded>,
    pub tagger: Option<Loaded>,
}

/// Which packs to load (empty id: the first of the kind); `None` leaves the kind out.
#[derive(Debug, Clone, Default)]
pub struct Choice {
    pub face_detector: Option<String>,
    pub face_embedder: Option<String>,
    pub anime_detector: Option<String>,
    pub anime_embedder: Option<String>,
    pub tagger: Option<String>,
}

impl Engine {
    /// Loads the chosen packs; packs that are missing or fail to load are reported, not fatal.
    pub fn load(packs: &[Pack], choice: &Choice, providers: &[String], device: i32, threads: u32) -> (Self, Vec<String>) {
        let mut errors = Vec::new();
        let mut open = |kind: PackKind, id: &Option<String>| -> Option<Loaded> {
            let id = id.as_ref()?;
            let Some(pack) = pack::pick(packs, kind, id) else {
                errors.push(format!("no {kind:?} pack{}", if id.is_empty() { String::new() } else { format!(" '{id}'") }));
                return None;
            };
            Loaded::open(pack, providers, device, threads).map_err(|e| errors.push(format!("{}: {e}", pack.manifest.id))).ok()
        };
        let engine = Self {
            face_detector: open(PackKind::FaceDetector, &choice.face_detector),
            face_embedder: open(PackKind::FaceEmbedder, &choice.face_embedder),
            anime_detector: open(PackKind::AnimeDetector, &choice.anime_detector),
            anime_embedder: open(PackKind::AnimeEmbedder, &choice.anime_embedder),
            tagger: open(PackKind::Tagger, &choice.tagger),
        };
        (engine, errors)
    }

    /// Every loaded model, for stored results (keyed by `Manifest::key`).
    pub fn models(&self) -> impl Iterator<Item = &Loaded> {
        [&self.face_detector, &self.face_embedder, &self.anime_detector, &self.anime_embedder, &self.tagger].into_iter().flatten()
    }

    /// Faces of `kind` in `img`. Photos are aligned on their landmarks for the embedder;
    /// characters are cropped around the face, grown by `crop_scale`.
    pub fn faces(&self, kind: FaceKind, img: &RgbImage, o: &Options) -> Result<Vec<Found>> {
        let (det, emb) = match kind {
            FaceKind::Real => (&self.face_detector, &self.face_embedder),
            FaceKind::Anime => (&self.anime_detector, &self.anime_embedder),
        };
        let Some(det) = det else { return Ok(Vec::new()) };
        let (w, h) = (img.width().max(1) as f32, img.height().max(1) as f32);
        let mut found = Vec::new();
        for d in det.detect(img, o)? {
            let face_box = [d.x, d.y, d.w, d.h];
            let aligned = d.landmarks.as_ref().and_then(|l| align::align(img, l, 112));
            let crop = match &aligned {
                Some(a) => image::imageops::resize(a, o.crop_px, o.crop_px, image::imageops::FilterType::Triangle),
                None => prep::square_crop(img, face_box, 1.3, o.crop_px, [0, 0, 0]),
            };
            let embedding = match emb {
                None => None,
                Some(e) => {
                    let spec = &e.pack.manifest.input;
                    let input = match (kind, &aligned) {
                        (FaceKind::Real, Some(a)) => a.clone(),
                        (FaceKind::Real, None) => prep::square_crop(img, face_box, 1.0, spec.width, [0, 0, 0]),
                        (FaceKind::Anime, _) => prep::square_crop(img, face_box, o.crop_scale, spec.width, spec.pad),
                    };
                    Some(e.embed(&input)?)
                }
            };
            found.push(Found { kind, bbox: [d.x / w, d.y / h, d.w / w, d.h / h], score: d.score, embedding, crop });
        }
        Ok(found)
    }

    /// Tags of the whole image scoring at least `keep`.
    pub fn tags(&self, img: &RgbImage, keep: f32) -> Result<Vec<TagScore>> {
        match &self.tagger {
            Some(t) => t.tag(img, keep),
            None => Ok(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real run, only with `VP_TEST_ORT=<ONNX Runtime library>` and `VP_TEST_MODELS=<folder
    /// of model packs>` (optionally `VP_TEST_IMAGE=<photo>`): loads every pack found and runs
    /// each kind once.
    #[test]
    fn real_models_run_when_configured() {
        let (Some(lib), Some(models)) = (std::env::var_os("VP_TEST_ORT"), std::env::var_os("VP_TEST_MODELS")) else { return };
        if !runtime::SUPPORTED {
            return;
        }
        runtime::init(&[lib.into()]).expect("ONNX Runtime loads");
        let (packs, errors) = pack::find_packs(&[models.into()]);
        assert!(errors.is_empty(), "{errors:?}");
        let any = Some(String::new());
        let choice = Choice { face_detector: any.clone(), face_embedder: any.clone(), anime_detector: any.clone(), anime_embedder: any.clone(), tagger: any };
        let providers = ["cuda".to_string(), "directml".to_string(), "cpu".to_string()];
        let (engine, errors) = Engine::load(&packs, &choice, &providers, 0, 0);
        eprintln!("load errors: {errors:?}");
        for m in engine.models() {
            eprintln!("{} on {}", m.pack.manifest.key(), m.model.provider);
        }
        let img = match std::env::var_os("VP_TEST_IMAGE") {
            Some(p) => image::open(p).unwrap().to_rgb8(),
            None => RgbImage::from_pixel(640, 480, image::Rgb([128, 128, 128])),
        };
        let o = Options { detect_score: 0.6, nms_iou: 0.4, min_face_px: 24.0, keep_score: 0.2, crop_scale: 2.5, crop_px: 128 };
        for kind in [FaceKind::Real, FaceKind::Anime] {
            let found = engine.faces(kind, &img, &o).unwrap();
            eprintln!("{kind:?}: {} faces", found.len());
            for f in &found {
                assert!(f.embedding.as_ref().is_none_or(|e| (cluster::dot(e, e) - 1.0).abs() < 1e-3));
            }
        }
        eprintln!("tags: {:?}", engine.tags(&img, 0.35).unwrap().iter().take(10).collect::<Vec<_>>());
    }
}
