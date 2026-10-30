//! Shared types for ViewPort crates.

pub mod edit;
pub mod settings;

use serde::{Deserialize, Serialize};

/// Kind of item. Stored as its lowercase name in the database. `NonMedia` is any other file
/// (kept, searched and exported, but never previewed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Image,
    Video,
    NonMedia,
}

impl MediaKind {
    pub const ALL: [MediaKind; 3] = [MediaKind::Image, MediaKind::Video, MediaKind::NonMedia];

    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Video => "video",
            MediaKind::NonMedia => "nonmedia",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    pub fn is_media(self) -> bool {
        self != MediaKind::NonMedia
    }
}

/// Version of the bundle format this build reads and writes.
pub const BUNDLE_FORMAT_VERSION: u32 = 1;
