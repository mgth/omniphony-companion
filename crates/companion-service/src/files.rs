use crate::AppResult;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};
pub fn read_json<T: DeserializeOwned>(path: &Path) -> AppResult<T> {
    const LIMIT: u64 = 8 * 1024 * 1024;
    let mut bytes = Vec::new();
    File::open(path)?.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(format!(
            "{} exceeds the 8 MiB observation-file limit",
            path.display()
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
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    let mut pending = tempfile::NamedTempFile::new_in(parent(path))?;
    pending.write_all(&bytes)?;
    pending.as_file().sync_all()?;
    pending.persist_noclobber(path)?;
    Ok(())
}
