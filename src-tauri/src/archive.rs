//! Bundles shipped as archives: a `.zip` (or zipped `.vpbun`) containing a bundle is
//! extracted next to the archive and opened from there.

use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

use crate::error::{CmdError, CmdResult};

/// Finds the bundle inside `archive` (its `info.json` at the root or one folder down),
/// extracts it beside the archive under a free name, and returns that folder.
pub fn extract_bundle(archive: &Path) -> CmdResult<PathBuf> {
    let file = std::fs::File::open(archive)?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|_| CmdError::new("not_a_bundle", "not a bundle or a zip archive"))?;
    let prefix = find_prefix(&mut zip)?;
    let parent = archive.parent().unwrap_or(Path::new("."));
    let stem = archive.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "bundle".into());
    let stem = stem.trim_end_matches(".vpbun").to_string();
    let dest = free_name(parent, &stem);
    std::fs::create_dir_all(&dest)?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| CmdError::new("archive", e))?;
        // enclosed_name rejects absolute paths and parent-directory components (zip slip).
        let Some(name) = entry.enclosed_name() else { continue };
        let Ok(rel) = name.strip_prefix(&prefix) else { continue };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut f = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut f)?;
    }
    Ok(dest)
}

fn find_prefix<R: Read + Seek>(zip: &mut zip::ZipArchive<R>) -> CmdResult<PathBuf> {
    let mut best: Option<PathBuf> = None;
    for i in 0..zip.len() {
        let entry = zip.by_index(i).map_err(|e| CmdError::new("archive", e))?;
        let Some(name) = entry.enclosed_name() else { continue };
        if name.file_name().is_some_and(|n| n == vp_bundle::INFO_FILE) {
            let parent = name.parent().map(Path::to_path_buf).unwrap_or_default();
            let depth = parent.components().count();
            if depth <= 1 && best.as_ref().is_none_or(|b| depth < b.components().count()) {
                best = Some(parent);
            }
        }
    }
    best.ok_or_else(|| CmdError::new("not_a_bundle", "the archive does not contain a ViewPort bundle"))
}

fn free_name(parent: &Path, stem: &str) -> PathBuf {
    let first = parent.join(format!("{stem}.{}", vp_bundle::EXTENSION));
    if !first.exists() {
        return first;
    }
    (2..)
        .map(|n| parent.join(format!("{stem} ({n}).{}", vp_bundle::EXTENSION)))
        .find(|p| !p.exists())
        .expect("some name is free")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_zip(path: &Path, files: &[(&str, &[u8])]) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, data) in files {
            z.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn extracts_nested_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Trip.zip");
        make_zip(
            &archive,
            &[
                ("Trip.vpbun/info.json", br#"{"bundle_version":1,"name":"Trip"}"#),
                ("Trip.vpbun/media/a.jpg", b"jpg"),
                ("../evil.txt", b"x"),
            ],
        );
        let out = extract_bundle(&archive).unwrap();
        assert_eq!(out, dir.path().join("Trip.vpbun"));
        assert!(out.join("info.json").is_file());
        assert_eq!(std::fs::read(out.join("media/a.jpg")).unwrap(), b"jpg");
        assert!(!dir.path().parent().unwrap().join("evil.txt").exists());
        // A second import gets a new name instead of overwriting.
        assert_eq!(extract_bundle(&archive).unwrap(), dir.path().join("Trip (2).vpbun"));
    }

    #[test]
    fn rejects_archives_without_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("x.zip");
        make_zip(&archive, &[("readme.txt", b"hi")]);
        assert!(matches!(extract_bundle(&archive), Err(CmdError { code: "not_a_bundle", .. })));
    }
}
