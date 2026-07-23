import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

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

interface Recording {
  id: string;
  timestamp: number;
  fileName: string;
  transcription: string;
  duration: number;
  status: string;
}

interface CatalogModel {
  name: string;
  filename: string;
  sizeMb: number;
  description: string;
}

interface ModelsInfo {
  catalog: CatalogModel[];
  installed: string[];
  activeModelPath: string;
}

interface DownloadEvent {
  name: string;
  bytesDownloaded: number;
  totalBytes: number | null;
  done: boolean;
  error: string | null;
}

function App() {
  const [status, setStatus] = useState<Status>("idle");
  const [elapsed, setElapsed] = useState(0);
  const [progress, setProgress] = useState(0);
  const [text, setText] = useState("");
  const [error, setError] = useState("");
  const [settings, setSettings] = useState<Settings | null>(null);
  const [recordings, setRecordings] = useState<Recording[]>([]);
  const [models, setModels] = useState<ModelsInfo | null>(null);
  const [download, setDownload] = useState<DownloadEvent | null>(null);
  const [axTrusted, setAxTrusted] = useState(true);
  const statusRef = useRef(status);
  statusRef.current = status;

  async function refreshRecordings() {
    try {
      setRecordings(await invoke<Recording[]>("list_recordings"));
    } catch {
      /* storage may not be ready yet */
    }
  }

  async function refreshModels() {
    try {
      setModels(await invoke<ModelsInfo>("models_info"));
    } catch {
      /* ditto */
    }
  }

  useEffect(() => {
    invoke<Settings>("get_settings").then(setSettings);
    invoke<boolean>("accessibility_status").then(setAxTrusted);
    refreshRecordings();
    refreshModels();
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
      listen("recordings-changed", () => refreshRecordings()),
      listen<Settings>("settings-changed", (e) => setSettings(e.payload)),
      listen<DownloadEvent>("model-download", (e) => {
        setDownload(e.payload.done ? null : e.payload);
        if (e.payload.done) {
          if (e.payload.error) setError(e.payload.error);
          refreshModels();
        }
      }),
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
    if (!settings) return;
    const next = { ...settings, [key]: value };
    setSettings(next);
    invoke("set_settings", { settings: next }).catch((e) => setError(String(e)));
  }

  async function toggleRecording() {
    setError("");
    try {
      if (status === "idle") {
        await invoke("start_recording");
      } else if (status === "recording") {
        setProgress(0);
        const result = await invoke<string>("stop_and_transcribe", {
          pasteDelayMs: settings?.paste ? 2000 : 0,
        });
        setText(result);
      }
    } catch (e) {
      setError(String(e));
      setStatus("idle");
    }
  }

  const activeModelName = models?.activeModelPath.split("/").pop() ?? "";

  return (
    <main className="container">
      <h1>ItalianSuperWhisper</h1>
      {settings && (
        <p className="hint">
          Scorciatoia globale: <code>{settings.hotkey}</code> — tienila premuta
          per dettare, o tocco singolo per avviare/fermare. Modello attivo:{" "}
          <code>{activeModelName}</code>
        </p>
      )}

      <div className="controls">
        <button
          className={status === "recording" ? "record recording" : "record"}
          onClick={toggleRecording}
          disabled={status === "transcribing" || !settings}
        >
          {status === "idle" && "● Registra"}
          {status === "recording" && `■ Ferma (${elapsed.toFixed(1)}s)`}
          {status === "transcribing" &&
            `Trascrivo… ${Math.round(progress * 100)}%`}
        </button>
        {status === "recording" && (
          <button className="cancel" onClick={() => invoke("cancel_recording")}>
            Annulla
          </button>
        )}
      </div>

      {settings && (
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
      )}

      {!axTrusted && settings?.paste && (
        <p className="warning">
          Per incollare nelle altre app serve il permesso{" "}
          <strong>Accessibilità</strong>.{" "}
          <button
            onClick={async () => {
              const ok = await invoke<boolean>("request_accessibility");
              setAxTrusted(ok);
            }}
          >
            Concedi
          </button>
        </p>
      )}

      {error && <p className="error">{error}</p>}

      <textarea
        className="result"
        value={text}
        readOnly
        placeholder="La trascrizione apparirà qui…"
      />

      <details className="section" open>
        <summary>Storico ({recordings.length})</summary>
        <ul className="history">
          {recordings.map((r) => (
            <li key={r.id}>
              <div className="history-text">{r.transcription || "(vuota)"}</div>
              <div className="history-meta">
                {new Date(r.timestamp).toLocaleString("it-IT")} ·{" "}
                {r.duration.toFixed(1)}s
                <button
                  onClick={() => navigator.clipboard.writeText(r.transcription)}
                >
                  Copia
                </button>
                <button
                  onClick={async () => {
                    await invoke("delete_recording", { id: r.id });
                    refreshRecordings();
                  }}
                >
                  Elimina
                </button>
              </div>
            </li>
          ))}
          {recordings.length === 0 && <li className="empty">Nessuna dettatura.</li>}
        </ul>
      </details>

      {models && (
        <details className="section">
          <summary>Modelli</summary>
          <ul className="models">
            {models.catalog.map((m) => {
              const installed = models.installed.includes(m.filename);
              const active = models.activeModelPath.endsWith(m.filename);
              const downloading = download?.name === m.name;
              return (
                <li key={m.name}>
                  <div>
                    <strong>{m.name}</strong> — {m.sizeMb} MB
                    {active && <span className="badge">attivo</span>}
                    {installed && !active && (
                      <span className="badge installed">installato</span>
                    )}
                  </div>
                  <div className="model-desc">{m.description}</div>
                  {downloading && download && (
                    <progress
                      value={download.bytesDownloaded}
                      max={download.totalBytes ?? undefined}
                    />
                  )}
                  <div className="model-actions">
                    {!installed && !downloading && (
                      <button
                        onClick={() =>
                          invoke("download_model", { name: m.name }).catch((e) =>
                            setError(String(e)),
                          )
                        }
                      >
                        Scarica
                      </button>
                    )}
                    {downloading && (
                      <button onClick={() => invoke("cancel_model_download")}>
                        Annulla download
                      </button>
                    )}
                    {installed && !active && (
                      <>
                        <button
                          onClick={() =>
                            invoke("select_model", { name: m.filename })
                              .then(refreshModels)
                              .catch((e) => setError(String(e)))
                          }
                        >
                          Usa
                        </button>
                        <button
                          onClick={() =>
                            invoke("delete_model", { name: m.filename })
                              .then(refreshModels)
                              .catch((e) => setError(String(e)))
                          }
                        >
                          Elimina
                        </button>
                      </>
                    )}
                  </div>
                </li>
              );
            })}
          </ul>
        </details>
      )}
    </main>
  );
}

export default App;
