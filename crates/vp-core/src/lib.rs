//! Shared types for ViewPort crates.

pub mod settings;

use serde::{Deserialize, Serialize};

/// Kind of media item. Stored as its lowercase name in the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Image,
    Video,
}

impl MediaKind {
    pub const ALL: [MediaKind; 2] = [MediaKind::Image, MediaKind::Video];

    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Video => "video",
        }
    }
}

/// Version of the bundle format this build reads and writes.
pub const BUNDLE_FORMAT_VERSION: u32 = 1;
