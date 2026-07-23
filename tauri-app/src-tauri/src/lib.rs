//! Tauri backend: dictation core loop (record → transcribe → paste) driven
//! either from the UI or from the global hotkey. Model paths arrive from the
//! frontend for now; the model manager / app-data storage lands in phase 5.

use std::sync::Mutex;
use std::time::Duration;

use audio_capture::Recorder;
use paste_sim::SystemPaster;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use whisper_engine::transcribe::WhisperEngine;

mod dictation;
use dictation::{emit_state, transcribe_pipeline, DictationSettings, HotkeyMachine};

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
}

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
        transcribe_pipeline(
            &app,
            samples,
            &settings,
            Duration::from_millis(paste_delay_ms),
        )
    })
    .await
    .map_err(|e| e.to_string())?;

    result
}

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
    if hotkey_changed {
        register_hotkey(&app, &settings.hotkey)?;
    }
    Ok(())
}

fn register_hotkey(app: &AppHandle, hotkey: &str) -> Result<(), String> {
    let gs = app.global_shortcut();
    gs.unregister_all().map_err(|e| e.to_string())?;
    gs.register(hotkey)
        .map_err(|e| format!("cannot register hotkey \"{hotkey}\": {e}"))?;
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
        .setup(|app| {
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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_recording,
            cancel_recording,
            recording_elapsed,
            stop_and_transcribe,
            get_settings,
            set_settings
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
