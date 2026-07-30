//! Filesystem naming helpers. Keeping them here makes installer paths predictable.

use std::{fs, path::{Path, PathBuf}};
use anyhow::{bail, Result};

/// Returns TarDrop's user-only root, creating it with normal user permissions when needed.
pub fn applications_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not determine home directory"))?;
    let root = home.join("Applications"); fs::create_dir_all(&root)?; Ok(root)
}

/// Returns the standard XDG directory used by Plasma and other launchers.
pub fn applications_database_dir() -> Result<PathBuf> {
    let base = dirs::data_local_dir().ok_or_else(|| anyhow::anyhow!("could not determine XDG data directory"))?;
    let root = base.join("applications"); fs::create_dir_all(&root)?; Ok(root)
}

/// Reduces an archive name to a friendly, safe application name.
pub fn archive_stem(path: &Path) -> String {
    let mut name = path.file_name().and_then(|n| n.to_str()).unwrap_or("Application").to_owned();
    for suffix in [".tar.gz", ".tar.xz", ".tar.bz2", ".tgz", ".tar", ".zip"] {
        if name.to_ascii_lowercase().ends_with(suffix) { name.truncate(name.len() - suffix.len()); break; }
    }
    sanitize_name(&name)
}

/// Keeps install directory names simple and prevents surprising hidden or traversal directories.
pub fn sanitize_name(name: &str) -> String {
    let cleaned: String = name.chars().map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') { c } else { '_' }).collect();
    let trimmed = cleaned.trim_matches([' ', '.']);
    if trimmed.is_empty() { "Application".to_owned() } else { trimmed.chars().take(80).collect() }
}

/// Converts a machine-oriented executable or archive name into a readable display name.
/// Only separator characters are rewritten, so the raw name remains available as a fallback.
pub fn pretty_name(raw: &str) -> String {
    let words: Vec<String> = raw
        .split(|character: char| character == '-' || character == '_' || character.is_whitespace())
        .filter(|word| !word.is_empty())
        .map(capitalize_word)
        .collect();
    let result = words.join(" ");
    if result.is_empty() { String::new() } else { result.chars().take(80).collect() }
}

/// Capitalizes the first character while normalising an automatically generated word.
fn capitalize_word(word: &str) -> String {
    let mut characters = word.chars();
    let Some(first) = characters.next() else { return String::new(); };
    first.to_uppercase().chain(characters.flat_map(|character| character.to_lowercase())).collect()
}

/// Ensures a calculated child stays immediately below the owned root.
pub fn direct_child(root: &Path, name: &str) -> Result<PathBuf> {
    let candidate = root.join(name);
    if candidate.parent() != Some(root) || name.is_empty() { bail!("invalid installation name") }
    Ok(candidate)
}

/// Turns a display name into a stable, XDG-friendly desktop file identifier.
pub fn desktop_id(name: &str) -> String { format!("org.tardrop.{}", sanitize_name(name).replace(' ', "-")) }
