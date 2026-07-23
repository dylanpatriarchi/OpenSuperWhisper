import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

// Dev-only defaults: the model manager (phase 5) will own real storage.
const DEFAULT_MODEL = "/Users/dylan/OpenSuperWhisper/ggml-tiny.en.bin";
const DEFAULT_VAD =
  "/Users/dylan/OpenSuperWhisper/OpenSuperWhisper/ggml-silero-v5.1.2.bin";

type Status = "idle" | "recording" | "transcribing";

interface Settings {
  modelPath: string;
  vadPath: string;
  language: string;
  applyItalianCorrections: boolean;
  paste: boolean;
  holdToRecord: boolean;
  hotkey: string;
}

const DEFAULT_SETTINGS: Settings = {
  modelPath: DEFAULT_MODEL,
  vadPath: DEFAULT_VAD,
  language: "it",
  applyItalianCorrections: true,
  paste: true,
  holdToRecord: true,
  hotkey: "alt+Backquote",
};

function loadSettings(): Settings {
  try {
    const raw = localStorage.getItem("settings");
    if (raw) return { ...DEFAULT_SETTINGS, ...JSON.parse(raw) };
  } catch {
    /* fall through */
  }
  return DEFAULT_SETTINGS;
}

function App() {
  const [status, setStatus] = useState<Status>("idle");
  const [elapsed, setElapsed] = useState(0);
  const [progress, setProgress] = useState(0);
  const [text, setText] = useState("");
  const [error, setError] = useState("");
  const [settings, setSettings] = useState<Settings>(loadSettings);
  const statusRef = useRef(status);
  statusRef.current = status;

  // Push settings to the backend on startup and whenever they change.
  useEffect(() => {
    localStorage.setItem("settings", JSON.stringify(settings));
    invoke("set_settings", { settings }).catch((e) => setError(String(e)));
  }, [settings]);

  useEffect(() => {
    const unlisteners = [
      listen<{ progress: number }>("transcribe-progress", (e) =>
        setProgress(e.payload.progress),
      ),
      listen<{ state: Status }>("dictation-state", (e) => {
        setStatus(e.payload.state);
        if (e.payload.state === "recording") setElapsed(0);
      }),
      listen<{ text: string }>("dictation-result", (e) => setText(e.payload.text)),
      listen<{ text: string }>("dictation-error", (e) => setError(e.payload.text)),
    ];
    return () => {
      unlisteners.forEach((u) => u.then((f) => f()));
    };
  }, []);

  useEffect(() => {
    if (status !== "recording") return;
    const timer = setInterval(async () => {
      if (statusRef.current !== "recording") return;
      setElapsed(await invoke<number>("recording_elapsed"));
    }, 250);
    return () => clearInterval(timer);
  }, [status]);

  function update<K extends keyof Settings>(key: K, value: Settings[K]) {
    setSettings((s) => ({ ...s, [key]: value }));
  }

  async function toggleRecording() {
    setError("");
    try {
      if (status === "idle") {
        await invoke("start_recording");
      } else if (status === "recording") {
        setProgress(0);
        // From the UI the paste target loses focus when clicking, so give
        // the tester 2s to refocus; the hotkey flow pastes immediately.
        const result = await invoke<string>("stop_and_transcribe", {
          pasteDelayMs: settings.paste ? 2000 : 0,
        });
        setText(result);
        setStatus("idle");
      }
    } catch (e) {
      setError(String(e));
      setStatus("idle");
    }
  }

  async function cancel() {
    await invoke("cancel_recording");
  }

  return (
    <main className="container">
      <h1>ItalianSuperWhisper</h1>
      <p className="hint">
        Scorciatoia globale: <code>{settings.hotkey}</code> — tienila premuta
        per dettare, o tocco singolo per avviare/fermare.
      </p>

      <div className="controls">
        <button
          className={status === "recording" ? "record recording" : "record"}
          onClick={toggleRecording}
          disabled={status === "transcribing"}
        >
          {status === "idle" && "● Registra"}
          {status === "recording" && `■ Ferma (${elapsed.toFixed(1)}s)`}
          {status === "transcribing" &&
            `Trascrivo… ${Math.round(progress * 100)}%`}
        </button>
        {status === "recording" && (
          <button className="cancel" onClick={cancel}>
            Annulla
          </button>
        )}
      </div>

      <div className="options">
        <label>
          Lingua{" "}
          <select
            value={settings.language}
            onChange={(e) => update("language", e.target.value)}
          >
            <option value="it">Italiano</option>
            <option value="en">English</option>
            <option value="auto">Auto</option>
          </select>
        </label>
        <label>
          <input
            type="checkbox"
            checked={settings.applyItalianCorrections}
            onChange={(e) => update("applyItalianCorrections", e.target.checked)}
          />{" "}
          Correzioni italiane
        </label>
        <label>
          <input
            type="checkbox"
            checked={settings.paste}
            onChange={(e) => update("paste", e.target.checked)}
          />{" "}
          Incolla nell'app attiva
        </label>
        <label>
          <input
            type="checkbox"
            checked={settings.holdToRecord}
            onChange={(e) => update("holdToRecord", e.target.checked)}
          />{" "}
          Tieni premuto per registrare
        </label>
        <label>
          Scorciatoia{" "}
          <input
            className="hotkey"
            value={settings.hotkey}
            onChange={(e) => update("hotkey", e.target.value)}
          />
        </label>
      </div>

      <details className="paths">
        <summary>Percorsi modello (dev)</summary>
        <label>
          Modello whisper
          <input
            value={settings.modelPath}
            onChange={(e) => update("modelPath", e.target.value)}
          />
        </label>
        <label>
          Modello VAD
          <input
            value={settings.vadPath}
            onChange={(e) => update("vadPath", e.target.value)}
          />
        </label>
      </details>

      {error && <p className="error">{error}</p>}

      <textarea
        className="result"
        value={text}
        readOnly
        placeholder="La trascrizione apparirà qui…"
      />
    </main>
  );
}

export default App;
