//! Local UI preferences, independent of the frontend and acquisition workers.
use crate::{AppResult, controller::Settings, files::parent};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::Duration,
};

const LIMIT: u64 = 128 * 1024;
const SAVE_DELAY: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub schema_version: u32,
    pub endpoint: String,
    pub settings: Settings,
    pub excluded_outputs: BTreeSet<String>,
    pub review_path: String,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema_version: 1,
            endpoint: "127.0.0.1:9000".into(),
            settings: Settings::default(),
            excluded_outputs: BTreeSet::new(),
            review_path: String::new(),
        }
    }
}
impl Preferences {
    fn validate(&self) -> AppResult<()> {
        if self.schema_version != 1 {
            return Err("unsupported settings version; existing file was kept".into());
        }
        let s = &self.settings;
        if self.excluded_outputs.len() > 512
            || [
                self.endpoint.as_str(),
                self.review_path.as_str(),
                s.device.as_deref().unwrap_or_default(),
                s.reference_output.as_deref().unwrap_or_default(),
                &s.microphone_id,
                &s.position_id,
                &s.calibration_path,
                &s.output_root,
            ]
            .into_iter()
            .chain(self.excluded_outputs.iter().map(String::as_str))
            .any(|v| v.len() > 4096 || v.contains('\0'))
            || [s.sweep.peak_dbfs, s.sweep.start_hz, s.sweep.end_hz]
                .iter()
                .any(|v| !v.is_finite())
        {
            return Err("invalid or oversized settings".into());
        }
        // Draft field values may be incomplete. Normal acquisition readiness and
        // fresh device/renderer checks still apply before any recording or sound.
        Ok(())
    }
}

pub fn default_path() -> AppResult<PathBuf> {
    let absolute_env = |key| {
        std::env::var_os(key)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let base = if cfg!(target_os = "windows") {
        absolute_env("APPDATA")
    } else if cfg!(target_os = "macos") {
        absolute_env("HOME").map(|p| p.join("Library/Application Support"))
    } else {
        absolute_env("XDG_CONFIG_HOME").or_else(|| absolute_env("HOME").map(|p| p.join(".config")))
    }
    .ok_or("cannot locate the user configuration directory")?;
    Ok(base.join("omniphony-companion/settings.json"))
}

#[derive(Default)]
struct Pending {
    latest: Option<Preferences>,
    error: Option<String>,
}

/// Debounced atomic saves on a separate worker. The queue retains only the latest
/// preferences. Dropping the store flushes pending changes, including quick exits.
pub struct PreferenceStore {
    path: PathBuf,
    current: Preferences,
    loaded: bool,
    pending: Arc<Mutex<Pending>>,
    sender: Option<mpsc::SyncSender<()>>,
    worker: Option<JoinHandle<()>>,
}
impl PreferenceStore {
    pub fn open(path: PathBuf) -> AppResult<Self> {
        let (current, loaded) = match File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(LIMIT + 1).read_to_end(&mut bytes)?;
                if bytes.len() as u64 > LIMIT {
                    return Err("settings file exceeds 128 KiB; existing file was kept".into());
                }
                let prefs: Preferences = serde_json::from_slice(&bytes)?;
                prefs.validate()?;
                (prefs, true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Preferences::default(), false),
            Err(e) => return Err(e.into()),
        };
        let pending = Arc::new(Mutex::new(Pending::default()));
        let state = pending.clone();
        let destination = path.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("companion-settings".into())
            .spawn(move || {
                while receiver.recv().is_ok() {
                    let disconnected = loop {
                        match receiver.recv_timeout(SAVE_DELAY) {
                            Ok(()) => continue,
                            Err(mpsc::RecvTimeoutError::Timeout) => break false,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break true,
                        }
                    };
                    let latest = state.lock().unwrap().latest.take();
                    if let Some(preferences) = latest {
                        let error = save(&destination, &preferences).err().map(|e| {
                            format!(
                                "Settings could not be saved to {}: {e}",
                                destination.display()
                            )
                        });
                        if let Some(error) = &error {
                            eprintln!("{error}");
                        }
                        state.lock().unwrap().error = error;
                    }
                    if disconnected {
                        break;
                    }
                }
            })?;
        Ok(Self {
            path,
            current,
            loaded,
            pending,
            sender: Some(sender),
            worker: Some(worker),
        })
    }
    pub fn preferences(&self) -> &Preferences {
        &self.current
    }
    pub fn loaded(&self) -> bool {
        self.loaded
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn error(&self) -> Option<String> {
        self.pending.lock().unwrap().error.clone()
    }
    pub fn retry(&self) {
        self.pending.lock().unwrap().latest = Some(self.current.clone());
        let _ = self.sender.as_ref().unwrap().try_send(());
    }

    pub fn update(
        &mut self,
        settings: &Settings,
        endpoint: &str,
        excluded: &BTreeSet<String>,
        review_path: &str,
    ) {
        if self.current.settings == *settings
            && self.current.endpoint == endpoint
            && self.current.excluded_outputs == *excluded
            && self.current.review_path == review_path
        {
            return;
        }
        self.current = Preferences {
            settings: settings.clone(),
            endpoint: endpoint.into(),
            excluded_outputs: excluded.clone(),
            review_path: review_path.into(),
            ..Preferences::default()
        };
        // A full wake queue already guarantees the latest state will be consumed.
        self.retry();
    }
}
impl Drop for PreferenceStore {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn save(path: &Path, preferences: &Preferences) -> AppResult<()> {
    preferences.validate()?;
    let mut bytes = serde_json::to_vec_pretty(preferences)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > LIMIT {
        return Err("settings exceed 128 KiB".into());
    }
    fs::create_dir_all(parent(path))?;
    let mut file = tempfile::NamedTempFile::new_in(parent(path))?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}
