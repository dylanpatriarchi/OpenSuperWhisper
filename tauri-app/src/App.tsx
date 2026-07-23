import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

// Dev-only defaults: the model manager (phase 5) will own real storage.
const DEFAULT_MODEL = "/Users/dylan/OpenSuperWhisper/ggml-tiny.en.bin";
const DEFAULT_VAD =
  "/Users/dylan/OpenSuperWhisper/OpenSuperWhisper/ggml-silero-v5.1.2.bin";

type Status = "idle" | "recording" | "transcribing";

function App() {
  const [status, setStatus] = useState<Status>("idle");
  const [elapsed, setElapsed] = useState(0);
  const [progress, setProgress] = useState(0);
  const [text, setText] = useState("");
  const [error, setError] = useState("");

  const [modelPath, setModelPath] = useState(DEFAULT_MODEL);
  const [vadPath, setVadPath] = useState(DEFAULT_VAD);
  const [language, setLanguage] = useState("it");
  const [correctItalian, setCorrectItalian] = useState(true);
  const [paste, setPaste] = useState(false);
  const statusRef = useRef(status);
  statusRef.current = status;

  useEffect(() => {
    const unlisten = listen<{ progress: number }>(
      "transcribe-progress",
      (e) => setProgress(e.payload.progress),
    );
    return () => {
      unlisten.then((f) => f());
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

  async function toggleRecording() {
    setError("");
    try {
      if (status === "idle") {
        await invoke("start_recording");
        setElapsed(0);
        setStatus("recording");
      } else if (status === "recording") {
        setStatus("transcribing");
        setProgress(0);
        const result = await invoke<string>("stop_and_transcribe", {
          modelPath,
          vadPath,
          language,
          applyItalianCorrections: correctItalian,
          paste,
          pasteDelayMs: paste ? 2000 : 0,
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
    setStatus("idle");
  }

  return (
    <main className="container">
      <h1>ItalianSuperWhisper</h1>

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
          <select value={language} onChange={(e) => setLanguage(e.target.value)}>
            <option value="it">Italiano</option>
            <option value="en">English</option>
            <option value="auto">Auto</option>
          </select>
        </label>
        <label>
          <input
            type="checkbox"
            checked={correctItalian}
            onChange={(e) => setCorrectItalian(e.target.checked)}
          />{" "}
          Correzioni italiane
        </label>
        <label>
          <input
            type="checkbox"
            checked={paste}
            onChange={(e) => setPaste(e.target.checked)}
          />{" "}
          Incolla dopo 2s (metti a fuoco l'app di destinazione)
        </label>
      </div>

      <details className="paths">
        <summary>Percorsi modello (dev)</summary>
        <label>
          Modello whisper
          <input value={modelPath} onChange={(e) => setModelPath(e.target.value)} />
        </label>
        <label>
          Modello VAD
          <input value={vadPath} onChange={(e) => setVadPath(e.target.value)} />
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
