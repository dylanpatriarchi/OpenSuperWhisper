//! Tauri backend: dictation core loop (record → transcribe → paste) driven
//! either from the UI or from the global hotkey, with SQLite history,
//! model management and a tray icon.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use audio_capture::Recorder;
use model_manager::{CatalogModel, DownloadProgress, AVAILABLE_MODELS};
use paste_sim::SystemPaster;
use recordings_store::Recording;
use serde::Serialize;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use uuid::Uuid;
use whisper_engine::transcribe::WhisperEngine;

mod dictation;
mod permissions;
mod storage;
use dictation::{emit_state, transcribe_pipeline, DictationSettings, HotkeyMachine};
use storage::Storage;

pub(crate) struct EngineCache {
    pub model_path: String,
    pub vad_path: String,
    pub engine: WhisperEngine,
}

#[derive(Default)]
pub(crate) struct AppState {
    pub recorder: Mutex<Option<Recorder>>,
    pub engine: Mutex<Option<EngineCache>>,
    pub paster: Mutex<Option<SystemPaster>>,
    pub settings: Mutex<DictationSettings>,
    pub hotkey_machine: HotkeyMachine,
    pub storage: Mutex<Option<Storage>>,
    pub download_cancel: Mutex<Option<Arc<AtomicBool>>>,
    pub reform_engine: Mutex<Option<(String, reformulation::engine::ReformulationEngine)>>,
}

// ---------------------------------------------------------------------------
// Recording commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn start_recording(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let mut recorder = state.recorder.lock().unwrap();
    if recorder.is_some() {
        return Err("already recording".into());
    }
    *recorder = Some(Recorder::start().map_err(|e| e.to_string())?);
    drop(recorder);
    emit_state(&app, "recording");
    Ok(())
}

#[tauri::command]
fn cancel_recording(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    // Dropping the Recorder tears the capture thread down; samples discarded.
    state.recorder.lock().unwrap().take();
    emit_state(&app, "idle");
    Ok(())
}

#[tauri::command]
fn recording_elapsed(state: State<'_, AppState>) -> Result<f32, String> {
    Ok(state
        .recorder
        .lock()
        .unwrap()
        .as_ref()
        .map(|r| r.elapsed_secs())
        .unwrap_or(0.0))
}

/// UI-driven stop: uses the stored settings; `paste_delay_ms` gives the
/// tester time to refocus the target app (the hotkey flow pastes at once).
#[tauri::command]
async fn stop_and_transcribe(app: AppHandle, paste_delay_ms: u64) -> Result<String, String> {
    let state = app.state::<AppState>();
    let recorder = state
        .recorder
        .lock()
        .unwrap()
        .take()
        .ok_or("not recording")?;
    emit_state(&app, "transcribing");
    let settings = state.settings.lock().unwrap().clone();

    let result = tauri::async_runtime::spawn_blocking(move || {
        let samples = recorder.stop().map_err(|e| e.to_string())?;
        let out = transcribe_pipeline(
            &app,
            samples,
            &settings,
            Duration::from_millis(paste_delay_ms),
        );
        emit_state(&app, "idle");
        out
    })
    .await
    .map_err(|e| e.to_string())?;

    result
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> DictationSettings {
    state.settings.lock().unwrap().clone()
}

#[tauri::command]
fn set_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: DictationSettings,
) -> Result<(), String> {
    let hotkey_changed = {
        let mut current = state.settings.lock().unwrap();
        let changed = current.hotkey != settings.hotkey;
        *current = settings.clone();
        changed
    };
    if let Some(storage) = state.storage.lock().unwrap().as_ref() {
        storage::save_settings(&storage.paths.settings_path, &settings);
    }
    if hotkey_changed {
        register_hotkey(&app, &settings.hotkey)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

#[tauri::command]
fn list_recordings(state: State<'_, AppState>) -> Result<Vec<Recording>, String> {
    let storage = state.storage.lock().unwrap();
    let storage = storage.as_ref().ok_or("storage not initialized")?;
    storage.store.get_all().map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_recording(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let id = Uuid::parse_str(&id).map_err(|e| e.to_string())?;
    let storage = state.storage.lock().unwrap();
    let storage = storage.as_ref().ok_or("storage not initialized")?;
    storage
        .store
        .delete_recording(id)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ModelsInfo {
    catalog: Vec<CatalogModel>,
    installed: Vec<String>,
    active_model_path: String,
}

#[tauri::command]
fn models_info(state: State<'_, AppState>) -> Result<ModelsInfo, String> {
    let storage = state.storage.lock().unwrap();
    let storage = storage.as_ref().ok_or("storage not initialized")?;
    let installed = storage
        .models
        .installed_models()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    Ok(ModelsInfo {
        catalog: AVAILABLE_MODELS.to_vec(),
        installed,
        active_model_path: state.settings.lock().unwrap().model_path.clone(),
    })
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct DownloadEvent {
    name: String,
    bytes_downloaded: u64,
    total_bytes: Option<u64>,
    done: bool,
    error: Option<String>,
}

#[tauri::command]
fn download_model(app: AppHandle, state: State<'_, AppState>, name: String) -> Result<(), String> {
    let models = {
        let storage = state.storage.lock().unwrap();
        storage
            .as_ref()
            .ok_or("storage not initialized")?
            .models
            .clone()
    };
    let mut cancel_slot = state.download_cancel.lock().unwrap();
    if cancel_slot.is_some() {
        return Err("another download is already running".into());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    *cancel_slot = Some(cancel.clone());
    drop(cancel_slot);

    std::thread::spawn(move || {
        let emit = |payload: DownloadEvent| {
            let _ = app.emit("model-download", payload);
        };
        let name_for_progress = name.clone();
        let progress_app = app.clone();
        let result = models.download_model(&name, cancel, move |p: DownloadProgress| {
            let _ = progress_app.emit(
                "model-download",
                DownloadEvent {
                    name: name_for_progress.clone(),
                    bytes_downloaded: p.bytes_downloaded,
                    total_bytes: p.total_bytes,
                    done: false,
                    error: None,
                },
            );
        });
        emit(DownloadEvent {
            name: name.clone(),
            bytes_downloaded: 0,
            total_bytes: None,
            done: true,
            error: result.err().map(|e| e.to_string()),
        });
        let state = app.state::<AppState>();
        state.download_cancel.lock().unwrap().take();
    });
    Ok(())
}

#[tauri::command]
fn cancel_model_download(state: State<'_, AppState>) -> Result<(), String> {
    if let Some(cancel) = state.download_cancel.lock().unwrap().as_ref() {
        cancel.store(true, Ordering::SeqCst);
    }
    Ok(())
}

#[tauri::command]
fn delete_model(state: State<'_, AppState>, name: String) -> Result<(), String> {
    let storage = state.storage.lock().unwrap();
    let storage = storage.as_ref().ok_or("storage not initialized")?;
    storage.models.delete_model(&name).map_err(|e| e.to_string())
}

#[tauri::command]
fn select_model(
    app: AppHandle,
    state: State<'_, AppState>,
    name: String,
) -> Result<String, String> {
    let path = {
        let storage = state.storage.lock().unwrap();
        let storage = storage.as_ref().ok_or("storage not initialized")?;
        let path = storage.models.model_path(&name).map_err(|e| e.to_string())?;
        if !path.is_file() {
            return Err(format!("model {name} is not installed"));
        }
        path.to_string_lossy().into_owned()
    };
    let settings = {
        let mut settings = state.settings.lock().unwrap();
        settings.model_path = path.clone();
        settings.clone()
    };
    if let Some(storage) = state.storage.lock().unwrap().as_ref() {
        storage::save_settings(&storage.paths.settings_path, &settings);
    }
    let _ = app.emit("settings-changed", settings);
    Ok(path)
}

// ---------------------------------------------------------------------------
// Reformulation model
// ---------------------------------------------------------------------------

/// Recommended Italian-capable instruct model in GGUF (see
/// docs/TAURI_REWRITE.md — the runtime replacement for the MLX
/// gemma model the Swift app used). ~1.7 GB.
const REFORMULATION_MODEL_URL: &str =
    "https://huggingface.co/bartowski/gemma-2-2b-it-GGUF/resolve/main/gemma-2-2b-it-Q4_K_M.gguf?download=true";
const REFORMULATION_MODEL_FILE: &str = "gemma-2-2b-it-Q4_K_M.gguf";

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct ReformulationInfo {
    installed: bool,
    model_file: String,
    size_mb: u32,
}

#[tauri::command]
fn reformulation_info(state: State<'_, AppState>) -> Result<ReformulationInfo, String> {
    let storage = state.storage.lock().unwrap();
    let storage = storage.as_ref().ok_or("storage not initialized")?;
    let path = storage.paths.llm_models_dir.join(REFORMULATION_MODEL_FILE);
    Ok(ReformulationInfo {
        installed: path.is_file(),
        model_file: REFORMULATION_MODEL_FILE.into(),
        size_mb: 1710,
    })
}

/// Downloads the recommended GGUF and, on success, points the settings at it.
#[tauri::command]
fn download_reformulation_model(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let (models, dest) = {
        let storage = state.storage.lock().unwrap();
        let storage = storage.as_ref().ok_or("storage not initialized")?;
        (
            storage.models.clone(),
            storage.paths.llm_models_dir.join(REFORMULATION_MODEL_FILE),
        )
    };
    let mut cancel_slot = state.download_cancel.lock().unwrap();
    if cancel_slot.is_some() {
        return Err("another download is already running".into());
    }
    let cancel = Arc::new(AtomicBool::new(false));
    *cancel_slot = Some(cancel.clone());
    drop(cancel_slot);

    std::thread::spawn(move || {
        let progress_app = app.clone();
        let mut on_progress = |p: DownloadProgress| {
            let _ = progress_app.emit(
                "model-download",
                DownloadEvent {
                    name: "reformulation".into(),
                    bytes_downloaded: p.bytes_downloaded,
                    total_bytes: p.total_bytes,
                    done: false,
                    error: None,
                },
            );
        };
        let result = models.download_url(
            REFORMULATION_MODEL_URL,
            &dest,
            cancel,
            &mut on_progress,
            model_manager::Validation::Gguf,
        );
        let error = result.as_ref().err().map(|e| e.to_string());

        if result.is_ok() {
            let state = app.state::<AppState>();
            let settings = {
                let mut settings = state.settings.lock().unwrap();
                settings.reformulation_model_path = dest.to_string_lossy().into_owned();
                settings.clone()
            };
            if let Some(storage) = state.storage.lock().unwrap().as_ref() {
                storage::save_settings(&storage.paths.settings_path, &settings);
            }
            let _ = app.emit("settings-changed", settings);
        }

        let _ = app.emit(
            "model-download",
            DownloadEvent {
                name: "reformulation".into(),
                bytes_downloaded: 0,
                total_bytes: None,
                done: true,
                error,
            },
        );
        let state = app.state::<AppState>();
        state.download_cancel.lock().unwrap().take();
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Permissions
// ---------------------------------------------------------------------------

#[tauri::command]
fn accessibility_status() -> bool {
    permissions::accessibility_trusted()
}

/// Triggers the system Accessibility prompt (macOS); returns current trust.
#[tauri::command]
fn request_accessibility() -> bool {
    permissions::request_accessibility()
}

// ---------------------------------------------------------------------------
// Setup
// ---------------------------------------------------------------------------

fn register_hotkey(app: &AppHandle, hotkey: &str) -> Result<(), String> {
    let gs = app.global_shortcut();
    gs.unregister_all().map_err(|e| e.to_string())?;
    gs.register(hotkey)
        .map_err(|e| format!("cannot register hotkey \"{hotkey}\": {e}"))?;
    Ok(())
}

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let open_item = MenuItemBuilder::with_id("open", "Apri ItalianSuperWhisper").build(app)?;
    let quit_item = MenuItemBuilder::with_id("quit", "Esci").build(app)?;
    let menu = MenuBuilder::new(app).items(&[&open_item, &quit_item]).build()?;

    let mut tray = TrayIconBuilder::new().menu(&menu).on_menu_event(|app, event| {
        match event.id().as_ref() {
            "open" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
            "quit" => app.exit(0),
            _ => {}
        }
    });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    let machine = &app.state::<AppState>().hotkey_machine;
                    match event.state() {
                        ShortcutState::Pressed => machine.key_down(app),
                        ShortcutState::Released => machine.key_up(app),
                    }
                })
                .build(),
        )
        .manage(AppState::default())
        // Closing the window hides to the tray instead of quitting,
        // mirroring the Swift app's hide-to-menu-bar behavior; quit lives
        // in the tray menu.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
            let handle = app.handle().clone();
            match storage::init(&handle) {
                Ok((storage, settings)) => {
                    let state = app.state::<AppState>();
                    *state.storage.lock().unwrap() = Some(storage);
                    *state.settings.lock().unwrap() = settings;
                }
                Err(e) => eprintln!("warning: storage init failed: {e}"),
            }

            let hotkey = app
                .state::<AppState>()
                .settings
                .lock()
                .unwrap()
                .hotkey
                .clone();
            // A hotkey the OS refuses at startup must not prevent launch;
            // the settings UI reports failures when re-registering.
            if let Err(e) = register_hotkey(app.handle(), &hotkey) {
                eprintln!("warning: {e}");
            }

            setup_tray(app)?;

            // Pre-built hidden, mirroring the Swift warmUp(): showing it on
            // the first dictation must not pay webview-creation latency.
            tauri::WebviewWindowBuilder::new(
                app,
                "indicator",
                tauri::WebviewUrl::App("indicator.html".into()),
            )
            .visible(false)
            .decorations(false)
            .transparent(true)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .inner_size(170.0, 44.0)
            .shadow(false)
            .focused(false)
            .build()?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_recording,
            cancel_recording,
            recording_elapsed,
            stop_and_transcribe,
            get_settings,
            set_settings,
            list_recordings,
            delete_recording,
            models_info,
            download_model,
            cancel_model_download,
            delete_model,
            select_model,
            reformulation_info,
            download_reformulation_model,
            accessibility_status,
            request_accessibility
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
