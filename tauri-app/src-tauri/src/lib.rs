//! Phase-2 core loop: record from the default mic, transcribe with the
//! whisper-engine crate, optionally apply the Italian corrector, paste into
//! the frontmost app and restore the clipboard. Model paths arrive from the
//! frontend for now; the model manager / app-data storage lands in phase 5.

use std::path::Path;
use std::sync::Mutex;

use audio_capture::Recorder;
use paste_sim::{system_paster, SystemPaster, RESTORE_DELAY};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use whisper_engine::transcribe::WhisperEngine;

struct EngineCache {
    model_path: String,
    vad_path: String,
    engine: WhisperEngine,
}

#[derive(Default)]
struct AppState {
    recorder: Mutex<Option<Recorder>>,
    engine: Mutex<Option<EngineCache>>,
    paster: Mutex<Option<SystemPaster>>,
}

#[derive(Clone, Serialize)]
struct ProgressPayload {
    progress: f32,
}

#[tauri::command]
fn start_recording(state: State<'_, AppState>) -> Result<(), String> {
    let mut recorder = state.recorder.lock().unwrap();
    if recorder.is_some() {
        return Err("already recording".into());
    }
    *recorder = Some(Recorder::start().map_err(|e| e.to_string())?);
    Ok(())
}

#[tauri::command]
fn cancel_recording(state: State<'_, AppState>) -> Result<(), String> {
    // Dropping the Recorder tears the capture thread down; samples discarded.
    state.recorder.lock().unwrap().take();
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

#[tauri::command]
async fn stop_and_transcribe(
    app: AppHandle,
    model_path: String,
    vad_path: String,
    language: String,
    apply_italian_corrections: bool,
    paste: bool,
    paste_delay_ms: u64,
) -> Result<String, String> {
    let recorder = app
        .state::<AppState>()
        .recorder
        .lock()
        .unwrap()
        .take()
        .ok_or("not recording")?;

    tauri::async_runtime::spawn_blocking(move || {
        let samples = recorder.stop().map_err(|e| e.to_string())?;
        let state = app.state::<AppState>();

        // 0-10% is the capture/convert phase, mirroring the Swift engine.
        let _ = app.emit("transcribe-progress", ProgressPayload { progress: 0.10 });

        {
            let mut cache = state.engine.lock().unwrap();
            let stale = match cache.as_ref() {
                Some(c) => c.model_path != model_path || c.vad_path != vad_path,
                None => true,
            };
            if stale {
                let engine = WhisperEngine::load(Path::new(&model_path), Path::new(&vad_path))
                    .map_err(|e| e.to_string())?;
                *cache = Some(EngineCache {
                    model_path: model_path.clone(),
                    vad_path: vad_path.clone(),
                    engine,
                });
            }
        }

        let progress_app = app.clone();
        let on_progress = Box::new(move |p: f32| {
            let _ = progress_app.emit("transcribe-progress", ProgressPayload { progress: p });
        });

        let lang = if language == "auto" { None } else { Some(language.as_str()) };
        let mut cache = state.engine.lock().unwrap();
        let engine = &mut cache.as_mut().expect("engine cache populated above").engine;
        let mut text = engine
            .transcribe(&samples, lang, Some(on_progress))
            .map_err(|e| e.to_string())?;
        drop(cache);

        // Same gate as the Swift app: corrections only when the language is
        // explicitly Italian.
        if apply_italian_corrections && language == "it" {
            text = italian_corrector::correct(&text);
        }

        let _ = app.emit("transcribe-progress", ProgressPayload { progress: 1.0 });

        if paste && !text.is_empty() {
            // Phase-2 manual-testing affordance: give the tester time to
            // refocus the target app before the keystroke fires. The real
            // hotkey-driven flow (phase 4) never moves focus, so the delay
            // will go away with it.
            std::thread::sleep(std::time::Duration::from_millis(paste_delay_ms));

            let state = app.state::<AppState>();
            {
                let mut paster = state.paster.lock().unwrap();
                if paster.is_none() {
                    *paster = Some(system_paster().map_err(|e| e.to_string())?);
                }
                paster
                    .as_ref()
                    .unwrap()
                    .insert_text(&text)
                    .map_err(|e| e.to_string())?;
            }

            let restore_app = app.clone();
            std::thread::spawn(move || {
                std::thread::sleep(RESTORE_DELAY);
                let state = restore_app.state::<AppState>();
                let paster = state.paster.lock().unwrap();
                if let Some(p) = paster.as_ref() {
                    let _ = p.restore_if_unchanged();
                }
            });
        }

        Ok(text)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![
            start_recording,
            cancel_recording,
            recording_elapsed,
            stop_and_transcribe
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
