//! Atomic file writes: readers see the old file or the whole new one, never a partial file.

use std::ffi::OsString;
use std::fs;
use std::io::{self, ErrorKind};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};

/// Makes temp names unique per write, even when two writes in one run target the same path.
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) enum WriteOutcome {
    Written,
    TargetExists,
}

/// Writes a sibling temp file, then renames it over `path`, so readers never see a partial file.
///
/// No `File::sync_all`: the index is a cache, clangd treats an unreadable shard as missing and rebuilds
/// it, and syncing every one of 10k+ shards would dominate the run time.
///
/// `mode`, when given, sets the permission bits before the rename, so `path` never lacks them.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<()> {
    let temp_path = write_temp_file(path, bytes)?;
    let renamed = set_mode(&temp_path, mode).and_then(|()| fs::rename(&temp_path, path));
    if renamed.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    renamed.with_context(|| format!("writing {}", path.display()))
}

/// Like [`write_atomically`], but never replaces a file that exists, even one clangd creates meanwhile:
/// hard-linking the temp file to `path` fails when `path` exists, where `rename` would replace it.
pub(crate) fn write_unless_exists(path: &Path, bytes: &[u8]) -> Result<WriteOutcome> {
    let temp_path = write_temp_file(path, bytes)?;
    let outcome = link_unless_exists(&temp_path, path);
    // Gone already when the fallback renamed it; the name is unique to this write.
    let _ = fs::remove_file(&temp_path);
    outcome.with_context(|| format!("writing {}", path.display()))
}

fn set_mode(path: &Path, mode: Option<u32>) -> io::Result<()> {
    let Some(mode) = mode else {
        return Ok(());
    };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

fn link_unless_exists(temp_path: &Path, path: &Path) -> io::Result<WriteOutcome> {
    let error = match fs::hard_link(temp_path, path) {
        Ok(()) => return Ok(WriteOutcome::Written),
        Err(error) => error,
    };
    match error.kind() {
        ErrorKind::AlreadyExists => Ok(WriteOutcome::TargetExists),
        ErrorKind::Unsupported | ErrorKind::PermissionDenied => {
            log::debug!("{}: hard link failed ({error}); falling back to exists check and rename", path.display());
            rename_unless_exists(temp_path, path)
        }
        _ => Err(error),
    }
}

/// Racy fallback for file systems without hard links: clangd may create `path` between check and rename.
fn rename_unless_exists(temp_path: &Path, path: &Path) -> io::Result<WriteOutcome> {
    if path.exists() {
        return Ok(WriteOutcome::TargetExists);
    }
    fs::rename(temp_path, path)?;
    Ok(WriteOutcome::Written)
}

/// Writes `bytes` to a uniquely named hidden sibling of `path` and returns its path.
fn write_temp_file(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let file_name = path.file_name().context("target path has no file name")?;
    let mut temp_name = OsString::from(".");
    temp_name.push(file_name);
    temp_name.push(format!(".{}.{}.tmp", process::id(), TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)));
    let temp_path = path.with_file_name(temp_name);
    if let Err(error) = fs::write(&temp_path, bytes) {
        let _ = fs::remove_file(&temp_path);
        return Err(error).with_context(|| format!("writing {}", path.display()));
    }
    Ok(temp_path)
}
