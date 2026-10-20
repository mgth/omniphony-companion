use crate::AppResult;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};
pub fn read_json<T: DeserializeOwned>(path: &Path) -> AppResult<T> {
    read_json_limited(path, 8 * 1024 * 1024)
}

pub(crate) fn read_json_limited<T: DeserializeOwned>(path: &Path, limit: u64) -> AppResult<T> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "{} exceeds the {} MiB observation-file limit",
            path.display(),
            limit / (1024 * 1024)
        )
        .into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Publish a complete document without overwriting an existing session/profile.
pub fn write_json(path: &Path, value: &impl Serialize) -> AppResult<()> {
    publish(path, serde_json::to_vec_pretty(value)?)
}

/// [`write_json`] without indentation, for reports dense with plot points: two
/// thirds of an indented capture report is whitespace that every restore reads.
pub fn write_json_compact(path: &Path, value: &impl Serialize) -> AppResult<()> {
    publish(path, serde_json::to_vec(value)?)
}

fn publish(path: &Path, mut bytes: Vec<u8>) -> AppResult<()> {
    bytes.push(b'\n');
    let mut pending = tempfile::NamedTempFile::new_in(parent(path))?;
    pending.write_all(&bytes)?;
    pending.as_file().sync_all()?;
    pending.persist_noclobber(path)?;
    Ok(())
}
