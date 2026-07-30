//! Archive recognition and extraction.
//!
//! Every archive member is checked before it is written.  This module never delegates
//! extraction to a shell command, which avoids command injection and inconsistent tools.

use std::{fs, io::{self, Read}, path::{Component, Path, PathBuf}};
use anyhow::{bail, Context, Result};
use bzip2::read::BzDecoder;
use flate2::read::GzDecoder;
use tar::{Archive, EntryType};
use xz2::read::XzDecoder;

/// Formats which TarDrop can safely read today. Add a variant and extractor to extend it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat { Tar, TarGz, TarXz, TarBz2, Zip }

/// Identifies a supported type by extension; no archive is extracted based on a guessed command.
pub fn detect(path: &Path) -> Result<ArchiveFormat> {
    let lower = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_ascii_lowercase();
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") { Ok(ArchiveFormat::TarGz) }
    else if lower.ends_with(".tar.xz") { Ok(ArchiveFormat::TarXz) }
    else if lower.ends_with(".tar.bz2") { Ok(ArchiveFormat::TarBz2) }
    else if lower.ends_with(".tar") { Ok(ArchiveFormat::Tar) }
    else if lower.ends_with(".zip") { Ok(ArchiveFormat::Zip) }
    else { bail!("Unsupported archive type. Use tar, tar.gz, tgz, tar.xz, tar.bz2, or zip.") }
}

/// Extracts `source` into the empty, private `destination` directory.
/// Symlinks, hardlinks, device files, and traversal paths are rejected rather than followed.
pub fn extract(source: &Path, format: ArchiveFormat, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination).context("could not create extraction directory")?;
    match format {
        ArchiveFormat::Tar => extract_tar(Archive::new(fs::File::open(source)?), destination),
        ArchiveFormat::TarGz => extract_tar(Archive::new(GzDecoder::new(fs::File::open(source)?)), destination),
        ArchiveFormat::TarXz => extract_tar(Archive::new(XzDecoder::new(fs::File::open(source)?)), destination),
        ArchiveFormat::TarBz2 => extract_tar(Archive::new(BzDecoder::new(fs::File::open(source)?)), destination),
        ArchiveFormat::Zip => extract_zip(source, destination),
    }
}

/// Ensures an archive path is a relative normal path and turns it into a destination path.
fn safe_destination(root: &Path, archive_path: &Path) -> Result<PathBuf> {
    if archive_path.as_os_str().is_empty() { bail!("archive contains an empty path") }
    let mut result = root.to_path_buf();
    for component in archive_path.components() {
        match component {
            Component::Normal(piece) if piece != "." => result.push(piece),
            _ => bail!("archive contains unsafe path: {}", archive_path.display()),
        }
    }
    Ok(result)
}

/// Streams tar data into regular files only. Streaming limits memory use for large archives.
fn extract_tar<R: Read>(mut archive: Archive<R>, root: &Path) -> Result<()> {
    for item in archive.entries().context("invalid tar archive")? {
        let mut entry = item.context("could not read tar member")?;
        let relative = entry.path().context("invalid tar path")?.into_owned();
        let output = safe_destination(root, &relative)?;
        let kind = entry.header().entry_type();
        if kind == EntryType::Directory {
            fs::create_dir_all(&output)?;
        } else if kind == EntryType::Regular || kind == EntryType::GNUSparse {
            if let Some(parent) = output.parent() { fs::create_dir_all(parent)?; }
            let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&output)
                .with_context(|| format!("refusing to overwrite {}", output.display()))?;
            io::copy(&mut entry, &mut file)?;
            set_safe_mode(&output, entry.header().mode().unwrap_or(0));
        } else {
            bail!("archive contains unsupported link or special file: {}", relative.display());
        }
    }
    Ok(())
}

/// Extracts ZIP files with the same path and link policy as tar files.
fn extract_zip(source: &Path, root: &Path) -> Result<()> {
    let file = fs::File::open(source)?;
    let mut archive = zip::ZipArchive::new(file).context("invalid zip archive")?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let relative = entry.enclosed_name().ok_or_else(|| anyhow::anyhow!("archive contains unsafe zip path"))?.to_owned();
        let output = safe_destination(root, &relative)?;
        let mode = entry.unix_mode().unwrap_or(0o644);
        if (mode & 0o170000) == 0o120000 { bail!("archive contains a symbolic link: {}", relative.display()); }
        if entry.is_dir() {
            fs::create_dir_all(&output)?;
        } else {
            if let Some(parent) = output.parent() { fs::create_dir_all(parent)?; }
            let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&output)
                .with_context(|| format!("refusing to overwrite {}", output.display()))?;
            io::copy(&mut entry, &mut file)?;
            set_safe_mode(&output, mode);
        }
    }
    Ok(())
}

/// Retains only ordinary read/write/execute bits. This prevents setuid/setgid archives.
fn set_safe_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    { use std::os::unix::fs::PermissionsExt; let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777)); }
}
