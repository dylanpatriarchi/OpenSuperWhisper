//! App-data layout, settings persistence and first-run model provisioning.
//! Replaces the Swift app's `Application Support/<bundleId>` conventions with
//! Tauri's app-data dir; the bundled tiny model and the Silero VAD model ship
//! as Tauri resources and are provisioned into place on first run.

use std::path::PathBuf;
use std::sync::Arc;

use model_manager::ModelManager;
use recordings_store::RecordingsStore;
use tauri::path::BaseDirectory;
use tauri::{AppHandle, Manager};

use crate::dictation::DictationSettings;

pub struct Paths {
    pub recordings_dir: PathBuf,
    pub settings_path: PathBuf,
}

pub struct Storage {
    pub paths: Paths,
    pub store: Arc<RecordingsStore>,
    pub models: Arc<ModelManager>,
}

pub fn init(app: &AppHandle) -> Result<(Storage, DictationSettings), String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("no app data dir: {e}"))?;
    std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;

    let models_dir = data_dir.join("whisper-models");
    let recordings_dir = data_dir.join("recordings");
    let db_path = data_dir.join("recordings.sqlite");
    let settings_path = data_dir.join("settings.json");

    let bundled_tiny = app
        .path()
        .resolve("models/ggml-tiny.en.bin", BaseDirectory::Resource)
        .map_err(|e| format!("bundled tiny model missing: {e}"))?;
    let vad_model_path = app
        .path()
        .resolve("models/ggml-silero-v5.1.2.bin", BaseDirectory::Resource)
        .map_err(|e| format!("bundled VAD model missing: {e}"))?;

    let models = ModelManager::new(&models_dir).map_err(|e| e.to_string())?;
    let default_model = models
        .ensure_default_model(&bundled_tiny)
        .map_err(|e| format!("provisioning the default model failed: {e}"))?;

    let store = RecordingsStore::open(&db_path, &recordings_dir).map_err(|e| e.to_string())?;

    let mut settings = load_settings(&settings_path).unwrap_or_default();
    // Heal stale/empty paths: a missing model must never brick dictation.
    if settings.model_path.is_empty() || !PathBuf::from(&settings.model_path).is_file() {
        settings.model_path = default_model.to_string_lossy().into_owned();
    }
    settings.vad_path = vad_model_path.to_string_lossy().into_owned();

    let storage = Storage {
        paths: Paths {
            recordings_dir,
            settings_path,
        },
        store: Arc::new(store),
        models: Arc::new(models),
    };
    save_settings(&storage.paths.settings_path, &settings);
    Ok((storage, settings))
}

fn load_settings(path: &PathBuf) -> Option<DictationSettings> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn save_settings(path: &PathBuf, settings: &DictationSettings) {
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        // Best-effort: settings that fail to persist still apply in-memory.
        let _ = std::fs::write(path, json);
    }
}
