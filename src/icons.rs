//! Icon selection and copying into TarDrop's private XDG icon directory.

use std::{fs, path::{Path, PathBuf}};
use anyhow::Result;
use walkdir::WalkDir;
use crate::utils;

/// Finds an archive icon using the desktop Icon name when available, then conventional quality hints.
pub fn find_icon(root: &Path, app_name: &str, preferred_name: Option<&str>, desktop_file: Option<&Path>) -> Option<PathBuf> {
    let wanted = preferred_name.unwrap_or(app_name).to_ascii_lowercase().replace([' ', '-', '_'], "");
    // A relative path in a desktop file is occasionally used by portable bundles; resolve it safely.
    if let Some(name) = preferred_name.filter(|name| name.contains('/')) {
        let candidate = Path::new(name);
        let candidate = if candidate.is_absolute() { candidate.to_path_buf() } else { desktop_file?.parent()?.join(candidate) };
        if valid_icon(&candidate) && (candidate.is_absolute() || candidate.starts_with(root)) { return Some(candidate); }
    }
    let mut best: Option<(i64, PathBuf)> = None;
    for item in WalkDir::new(root).follow_links(false).into_iter().filter_map(Result::ok) {
        if !item.file_type().is_file() || !valid_icon(item.path()) { continue; }
        let path = item.path(); let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase().replace([' ', '-', '_'], "");
        let mut score = if stem == wanted { 10_000 } else if stem.contains(&wanted) || wanted.contains(&stem) { 4_000 } else if stem.contains("icon") { 1_000 } else { 0 };
        // Vector icons are resolution-independent; common launcher raster sizes follow them.
        score += match path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase().as_str() { "svg" => 3_000, _ if stem.contains("256x256") => 2_560, _ if stem.contains("128x128") => 1_280, _ if stem.contains("64x64") => 640, _ if stem.contains("48x48") => 480, _ => 0 };
        score += fs::metadata(path).map(|m| (m.len() / 1024).min(500) as i64).unwrap_or(0);
        if best.as_ref().is_none_or(|(old, _)| score > *old) { best = Some((score, path.to_path_buf())); }
    }
    best.map(|(_, path)| path)
}

/// Copies a verified image into one owned location so the launcher survives archive replacement.
pub fn copy_to_tardrop(source: &Path, id: &str) -> Result<PathBuf> {
    let root = dirs::data_local_dir().ok_or_else(|| anyhow::anyhow!("could not determine XDG data directory"))?.join("icons").join("TarDrop");
    fs::create_dir_all(&root)?;
    let extension = source.extension().and_then(|ext| ext.to_str()).filter(|ext| matches!(ext.to_ascii_lowercase().as_str(), "svg" | "png" | "xpm" | "ico")).unwrap_or("png");
    let target = root.join(format!("{}.{}", utils::sanitize_name(id).replace(' ', "-"), extension));
    fs::copy(source, &target)?; Ok(target)
}

/// Accepts only regular image files. The extractor has already rejected links inside archives.
fn valid_icon(path: &Path) -> bool { path.is_file() && path.extension().and_then(|e| e.to_str()).is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "svg" | "png" | "xpm" | "ico")) }
