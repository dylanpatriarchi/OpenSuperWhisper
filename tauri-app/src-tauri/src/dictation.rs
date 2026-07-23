//! The shared dictation pipeline (used by both the UI command and the global
//! hotkey) and the hotkey press/release state machine ported from
//! `ShortcutManager.swift`.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use audio_capture::Recorder;
use paste_sim::{system_paster, RESTORE_DELAY};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};
use whisper_engine::transcribe::WhisperEngine;

use crate::AppState;

/// Mirrors `ShortcutManager.holdThreshold`.
const HOLD_THRESHOLD: Duration = Duration::from_millis(300);

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationSettings {
    pub model_path: String,
    pub vad_path: String,
    pub language: String,
    pub apply_italian_corrections: bool,
    pub paste: bool,
    pub hold_to_record: bool,
    pub hotkey: String,
}

impl Default for DictationSettings {
    fn default() -> Self {
        Self {
            model_path: String::new(),
            vad_path: String::new(),
            language: "it".into(),
            apply_italian_corrections: true,
            paste: true,
            hold_to_record: true,
            // The Swift default is Option+` (backtick); "Backquote" is the
            // W3C code name the shortcut parser expects.
            hotkey: "alt+Backquote".into(),
        }
    }
}

#[derive(Clone, Serialize)]
struct StatePayload {
    state: &'static str,
}

#[derive(Clone, Serialize)]
struct ResultPayload {
    text: String,
}

#[derive(Clone, Serialize)]
struct ProgressPayload {
    progress: f32,
}

pub fn emit_state(app: &AppHandle, state: &'static str) {
    let _ = app.emit("dictation-state", StatePayload { state });
}

pub fn emit_progress(app: &AppHandle, progress: f32) {
    let _ = app.emit("transcribe-progress", ProgressPayload { progress });
}

/// Transcribes `samples`, applies the Italian corrector behind the same
/// explicit-"it" gate as the Swift app, optionally pastes (after
/// `paste_delay`) and schedules the guarded clipboard restore. Blocking —
/// call from a blocking task or dedicated thread.
pub fn transcribe_pipeline(
    app: &AppHandle,
    samples: Vec<f32>,
    settings: &DictationSettings,
    paste_delay: Duration,
) -> Result<String, String> {
    let state = app.state::<AppState>();

    emit_progress(app, 0.10);

    {
        let mut cache = state.engine.lock().unwrap();
        let stale = match cache.as_ref() {
            Some(c) => c.model_path != settings.model_path || c.vad_path != settings.vad_path,
            None => true,
        };
        if stale {
            let engine = WhisperEngine::load(
                Path::new(&settings.model_path),
                Path::new(&settings.vad_path),
            )
            .map_err(|e| e.to_string())?;
            *cache = Some(crate::EngineCache {
                model_path: settings.model_path.clone(),
                vad_path: settings.vad_path.clone(),
                engine,
            });
        }
    }

    let progress_app = app.clone();
    let on_progress = Box::new(move |p: f32| emit_progress(&progress_app, p));

    let lang = if settings.language == "auto" {
        None
    } else {
        Some(settings.language.as_str())
    };
    let mut cache = state.engine.lock().unwrap();
    let engine = &mut cache.as_mut().expect("engine cache populated above").engine;
    let mut text = engine
        .transcribe(&samples, lang, Some(on_progress))
        .map_err(|e| e.to_string())?;
    drop(cache);

    if settings.apply_italian_corrections && settings.language == "it" {
        text = italian_corrector::correct(&text);
    }

    // Persist the dictation BEFORE pasting — the paste must never be the only
    // surviving copy (the invariant the Swift app defends with its awaited
    // save). A failed save is surfaced but doesn't block the paste: at that
    // point the paste is the user's only way to keep their words.
    if !text.is_empty() {
        if let Err(e) = persist_recording(app, &samples, &text) {
            let _ = app.emit(
                "dictation-error",
                ResultPayload {
                    text: format!("salvataggio non riuscito: {e}"),
                },
            );
        }
    }

    emit_progress(app, 1.0);
    let _ = app.emit("dictation-result", ResultPayload { text: text.clone() });

    if settings.paste && !text.is_empty() {
        if !paste_delay.is_zero() {
            std::thread::sleep(paste_delay);
        }
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
}

/// Writes the 16kHz mono WAV alongside the DB row, mirroring the Swift
/// recordings/ directory convention, and notifies the UI.
fn persist_recording(app: &AppHandle, samples: &[f32], text: &str) -> Result<(), String> {
    let state = app.state::<AppState>();
    let storage = state.storage.lock().unwrap();
    let Some(storage) = storage.as_ref() else {
        return Err("storage not initialized".into());
    };

    let id = uuid::Uuid::new_v4();
    let file_name = format!("{id}.wav");
    let wav_path = storage.paths.recordings_dir.join(&file_name);

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: audio_capture::WHISPER_SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&wav_path, spec).map_err(|e| e.to_string())?;
    for &s in samples {
        writer
            .write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())?;

    let recording = recordings_store::Recording {
        id,
        timestamp: recordings_store::now_millis(),
        file_name,
        transcription: text.to_string(),
        duration: samples.len() as f64 / audio_capture::WHISPER_SAMPLE_RATE as f64,
        status: recordings_store::RecordingStatus::Completed,
        progress: 1.0,
        source_file_url: None,
        // Populated by the reformulation layer once it lands: raw is stored
        // only when it differs from the final text.
        raw_transcription: None,
        is_regeneration: false,
    };
    storage
        .store
        .add_recording(&recording)
        .map_err(|e| e.to_string())?;
    let _ = app.emit("recordings-changed", ());
    Ok(())
}

/// Stops the active recorder (if any) and runs the pipeline on a blocking
/// thread. Used by the hotkey flow; the UI command awaits its own task.
fn stop_and_transcribe_detached(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Some(recorder) = state.recorder.lock().unwrap().take() else {
        return;
    };
    emit_state(app, "transcribing");

    let settings = state.settings.lock().unwrap().clone();
    let app = app.clone();
    std::thread::spawn(move || {
        let result = recorder
            .stop()
            .map_err(|e| e.to_string())
            .and_then(|samples| transcribe_pipeline(&app, samples, &settings, Duration::ZERO));
        if let Err(e) = result {
            let _ = app.emit("dictation-error", ResultPayload { text: e });
        }
        emit_state(&app, "idle");
    });
}

fn start_recording_for_hotkey(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    let mut recorder = state.recorder.lock().unwrap();
    if recorder.is_some() {
        return false;
    }
    match Recorder::start() {
        Ok(r) => {
            *recorder = Some(r);
            drop(recorder);
            emit_state(app, "recording");
            true
        }
        Err(e) => {
            let _ = app.emit("dictation-error", ResultPayload { text: e.to_string() });
            false
        }
    }
}

/// Press/release state machine ported from `ShortcutManager.handleKeyDown` /
/// `handleKeyUp`: a press starts recording; holding past 0.3s makes the
/// release stop it (hold-to-record); a quick tap leaves it recording until
/// the next press (toggle). Hold mode is evaluated at release time — the
/// Swift arms a timer that flips `holdMode` at the threshold, but the flag
/// is only ever read on release, so elapsed-time comparison is equivalent.
#[derive(Default)]
pub struct HotkeyMachine {
    inner: Mutex<HotkeyStateInner>,
}

#[derive(Default)]
struct HotkeyStateInner {
    /// Set when the last press started a recording (mirrors the Swift's
    /// "arm hold mode only when this press starts a recording").
    press_started_recording: bool,
    press_time: Option<Instant>,
}

impl HotkeyMachine {
    pub fn key_down(&self, app: &AppHandle) {
        let mut inner = self.inner.lock().unwrap();
        let state = app.state::<AppState>();
        let is_recording = state.recorder.lock().unwrap().is_some();

        if !is_recording {
            inner.press_started_recording = start_recording_for_hotkey(app);
            inner.press_time = Some(Instant::now());
        } else {
            // A press while recording always stops — the user isn't forced
            // to hold or double-tap to end a toggle recording.
            inner.press_started_recording = false;
            inner.press_time = None;
            drop(inner);
            stop_and_transcribe_detached(app);
        }
    }

    pub fn key_up(&self, app: &AppHandle) {
        let mut inner = self.inner.lock().unwrap();
        let hold_to_record = {
            let state = app.state::<AppState>();
            let s = state.settings.lock().unwrap();
            s.hold_to_record
        };
        let held_long_enough = inner
            .press_time
            .map(|t| t.elapsed() >= HOLD_THRESHOLD)
            .unwrap_or(false);

        if inner.press_started_recording && hold_to_record && held_long_enough {
            inner.press_started_recording = false;
            inner.press_time = None;
            drop(inner);
            stop_and_transcribe_detached(app);
        }
    }
}
