//! Port of the Swift `WhisperModelManager` + the downloadable-model catalog
//! from `Settings.swift` (`SettingsDownloadableModels`).
//!
//! Core invariant carried over from the Swift implementation: a download is
//! validated (ggml magic + plausible size, via `whisper_engine::model_validation`)
//! **before** it is renamed into place. Without this, a few hundred bytes of
//! HTML from a captive portal or CDN error page get stored as
//! `ggml-large-v3-turbo.bin`, `is_model_downloaded` reports `true` forever,
//! and the user has no way to recover short of deleting the file by hand.
//!
//! Downloads are blocking (`reqwest::blocking`) by design — the Tauri layer
//! calls into this crate from `spawn_blocking`, which keeps this crate free of
//! an async runtime dependency.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;
use whisper_engine::model_validation::{validate_downloaded_model, ModelValidationError};

/// Suffix appended to the destination filename while a download is in flight.
/// A `.partial` file is never reported as installed and never loaded.
const PARTIAL_SUFFIX: &str = ".partial";

/// Read/copy chunk size. The cancel flag is checked between chunks.
const CHUNK_SIZE: usize = 64 * 1024;

/// One downloadable model, mirroring `SettingsDownloadableModel` in Swift.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogModel {
    /// Human-readable name shown in the UI (e.g. "Turbo V3 large").
    pub name: &'static str,
    /// Filename the model is stored under in the models directory.
    pub filename: &'static str,
    /// Direct download URL.
    pub url: &'static str,
    /// Approximate size in megabytes (for UI display only).
    pub size_mb: u32,
    pub description: &'static str,
    /// Language this model is fine-tuned for, if any (ISO 639-1).
    /// Selecting such a model should also switch the transcription language.
    pub preferred_language: Option<&'static str>,
}

/// The downloadable-model catalog, ported 1:1 from
/// `SettingsDownloadableModels.availableModels` in `Settings.swift`.
pub const AVAILABLE_MODELS: &[CatalogModel] = &[
    CatalogModel {
        name: "Turbo V3 large",
        filename: "ggml-large-v3-turbo.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo.bin?download=true",
        size_mb: 1624,
        description: "High accuracy, best quality",
        preferred_language: None,
    },
    CatalogModel {
        name: "Turbo V3 medium",
        filename: "ggml-large-v3-turbo-q8_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q8_0.bin?download=true",
        size_mb: 874,
        description: "Balanced speed and accuracy",
        preferred_language: None,
    },
    CatalogModel {
        name: "Turbo V3 small",
        filename: "ggml-large-v3-turbo-q5_0.bin",
        url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin?download=true",
        size_mb: 574,
        description: "Fastest processing",
        preferred_language: None,
    },
    CatalogModel {
        name: "Turbo V3 Hebrew",
        filename: "ggml-ivrit-large-v3-turbo.bin",
        url: "https://huggingface.co/ivrit-ai/whisper-large-v3-turbo-ggml/resolve/main/ggml-model.bin?download=true",
        size_mb: 1624,
        description: "Hebrew fine-tune of Turbo V3 by ivrit.ai. Sets the language to Hebrew.",
        preferred_language: Some("he"),
    },
];

/// Look up a catalog entry by its display name or its filename.
pub fn catalog_model(name: &str) -> Option<&'static CatalogModel> {
    AVAILABLE_MODELS
        .iter()
        .find(|m| m.name == name || m.filename == name)
}

/// Progress of an in-flight download.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct DownloadProgress {
    pub bytes_downloaded: u64,
    /// `None` when the server did not report a length.
    pub total_bytes: Option<u64>,
}

impl DownloadProgress {
    /// Fraction in `0.0..=1.0`, mirroring the Swift progress callback shape.
    pub fn fraction(&self) -> Option<f64> {
        match self.total_bytes {
            Some(total) if total > 0 => {
                Some((self.bytes_downloaded as f64 / total as f64).min(1.0))
            }
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ModelManagerError {
    #[error("unknown model: {0}")]
    UnknownModel(String),
    #[error("invalid model name: {0}")]
    InvalidName(String),
    #[error("model is not installed: {0}")]
    NotInstalled(String),
    #[error("download cancelled")]
    Cancelled,
    #[error("model server returned HTTP {0}; please try again later")]
    HttpStatus(u16),
    #[error(transparent)]
    Validation(#[from] ModelValidationError),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, ModelManagerError>;

/// Manages the models directory: install state, downloads, deletion.
///
/// Unlike the Swift singleton, the directory is caller-provided — the Tauri
/// layer resolves the platform app-data directory and passes it in.
pub struct ModelManager {
    models_dir: PathBuf,
}

impl ModelManager {
    /// Creates the manager, creating `models_dir` if it does not exist yet
    /// (Swift: `createModelsDirectoryIfNeeded`).
    pub fn new(models_dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let models_dir = models_dir.into();
        fs::create_dir_all(&models_dir)?;
        Ok(Self { models_dir })
    }

    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    /// Absolute path a model with this catalog name or filename is (or would
    /// be) stored at. Rejects names that could escape the models directory.
    pub fn model_path(&self, name: &str) -> Result<PathBuf> {
        let filename = resolve_filename(name)?;
        Ok(self.models_dir.join(filename))
    }

    /// Swift: `isModelDownloaded(name:)`.
    pub fn is_model_downloaded(&self, name: &str) -> bool {
        self.model_path(name).map(|p| p.is_file()).unwrap_or(false)
    }

    /// Installed model files (`.bin`), sorted by filename.
    /// Swift: `getAvailableModels()`. `.partial` files are excluded.
    pub fn installed_models(&self) -> std::io::Result<Vec<PathBuf>> {
        let mut models: Vec<PathBuf> = fs::read_dir(&self.models_dir)?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|p| p.is_file() && p.extension().is_some_and(|ext| ext == "bin"))
            .collect();
        models.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
        Ok(models)
    }

    /// Deletes an installed model and any leftover `.partial` for it.
    pub fn delete_model(&self, name: &str) -> Result<()> {
        let path = self.model_path(name)?;
        if !path.is_file() {
            return Err(ModelManagerError::NotInstalled(name.to_string()));
        }
        fs::remove_file(&path)?;
        let _ = fs::remove_file(partial_path(&path));
        Ok(())
    }

    /// Copies a bundled default model (the Swift app bundles `ggml-tiny.en.bin`)
    /// into the models directory on first run. No-op when a file with the same
    /// name is already installed. Swift: `ensureDefaultModelPresent` /
    /// `copyDefaultModelIfNeeded`.
    pub fn ensure_default_model(&self, bundled_path: &Path) -> Result<PathBuf> {
        let filename = bundled_path
            .file_name()
            .ok_or_else(|| ModelManagerError::InvalidName(bundled_path.display().to_string()))?;
        let dest = self.models_dir.join(filename);
        if dest.is_file() {
            return Ok(dest);
        }
        // Copy through a temp name so a crash mid-copy can't leave a truncated
        // file that is then reported as installed.
        let tmp = partial_path(&dest);
        fs::copy(bundled_path, &tmp)?;
        fs::rename(&tmp, &dest)?;
        Ok(dest)
    }

    /// Downloads a catalog model (by display name or filename), streaming to a
    /// `.partial` file next to the destination and validating the content
    /// before the final rename.
    ///
    /// - Resume: a leftover `.partial` is resumed with an HTTP `Range` header
    ///   (appending); if the server ignores the range (plain 200) the partial
    ///   is restarted from zero.
    /// - Cancellation: `cancel` is checked between chunks; on cancel the
    ///   `.partial` is kept for a later resume and `Cancelled` is returned.
    /// - Validation failure deletes the `.partial` — an HTML error page must
    ///   never become an installed model.
    ///
    /// Returns the installed model path. If the model is already installed,
    /// reports full progress and returns immediately (as the Swift does).
    pub fn download_model(
        &self,
        name: &str,
        cancel: Arc<AtomicBool>,
        mut progress: impl FnMut(DownloadProgress),
    ) -> Result<PathBuf> {
        let model =
            catalog_model(name).ok_or_else(|| ModelManagerError::UnknownModel(name.to_string()))?;
        let dest = self.models_dir.join(model.filename);
        if dest.is_file() {
            let size = fs::metadata(&dest)?.len();
            progress(DownloadProgress {
                bytes_downloaded: size,
                total_bytes: Some(size),
            });
            return Ok(dest);
        }
        self.download_url(model.url, &dest, cancel, &mut progress)
    }

    /// Lower-level download used by `download_model`; public so callers (and
    /// the ignored smoke test) can fetch a model that is not in the catalog.
    pub fn download_url(
        &self,
        url: &str,
        dest: &Path,
        cancel: Arc<AtomicBool>,
        progress: &mut impl FnMut(DownloadProgress),
    ) -> Result<PathBuf> {
        let partial = partial_path(dest);
        let resume_offset = fs::metadata(&partial).map(|m| m.len()).unwrap_or(0);

        let client = reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(60))
            // The blocking client's `timeout` spans the entire transfer, which
            // would abort a multi-GB download; disable it (the Swift used a
            // 24h resource timeout for the same reason).
            .timeout(None)
            .build()?;

        let mut request = client.get(url);
        if resume_offset > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={resume_offset}-"));
        }
        let response = request.send()?;
        let status = response.status();

        if status.as_u16() == 416 {
            // Requested range not satisfiable: the partial already covers the
            // full file. Validate and promote it.
            return finalize_download(&partial, dest);
        }
        if !status.is_success() {
            // Keep the partial: a transient server error should not throw away
            // already-downloaded bytes.
            return Err(ModelManagerError::HttpStatus(status.as_u16()));
        }

        let resumed = status.as_u16() == 206 && resume_offset > 0;
        let mut bytes_downloaded = if resumed {
            resume_offset
        } else {
            // Plain 200: the server ignored the range (or there was none);
            // the body is the whole file, so start the partial over.
            0
        };
        let total_bytes = response.content_length().map(|len| bytes_downloaded + len);

        let mut file = if resumed {
            OpenOptions::new().append(true).open(&partial)?
        } else {
            File::create(&partial)?
        };

        progress(DownloadProgress {
            bytes_downloaded,
            total_bytes,
        });

        let mut reader = response;
        let mut buf = vec![0u8; CHUNK_SIZE];
        loop {
            if cancel.load(Ordering::Relaxed) {
                // Keep the partial for a later resume.
                file.flush()?;
                return Err(ModelManagerError::Cancelled);
            }
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])?;
            bytes_downloaded += n as u64;
            progress(DownloadProgress {
                bytes_downloaded,
                total_bytes,
            });
        }
        file.flush()?;
        drop(file);

        finalize_download(&partial, dest)
    }
}

/// Validates the completed `.partial` and renames it into place. On validation
/// failure the partial is deleted — this is the guard against phantom
/// "installed" models (see the Swift comment in `WhisperModelManager`).
fn finalize_download(partial: &Path, dest: &Path) -> Result<PathBuf> {
    if let Err(err) = validate_downloaded_model(partial) {
        let _ = fs::remove_file(partial);
        return Err(err.into());
    }
    fs::rename(partial, dest)?;
    Ok(dest.to_path_buf())
}

/// The in-flight temp path for a destination model path.
fn partial_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(PARTIAL_SUFFIX);
    dest.with_file_name(name)
}

/// Maps a catalog name or bare filename to the on-disk filename, rejecting
/// anything that could traverse out of the models directory.
fn resolve_filename(name: &str) -> Result<String> {
    let filename = match catalog_model(name) {
        Some(model) => model.filename.to_string(),
        None => name.to_string(),
    };
    if filename.is_empty()
        || filename == "."
        || filename == ".."
        || filename.contains('/')
        || filename.contains('\\')
    {
        return Err(ModelManagerError::InvalidName(name.to_string()));
    }
    Ok(filename)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GGML_MAGIC_LE: [u8; 4] = 0x6767_6d6cu32.to_le_bytes();

    /// A file that passes model validation: ggml magic + 1 MB of zeros.
    fn write_fake_valid_model(path: &Path) {
        let mut bytes = vec![0u8; 1_000_000 + 4];
        bytes[..4].copy_from_slice(&GGML_MAGIC_LE);
        fs::write(path, bytes).unwrap();
    }

    fn manager() -> (tempfile::TempDir, ModelManager) {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ModelManager::new(dir.path().join("whisper-models")).unwrap();
        (dir, mgr)
    }

    #[test]
    fn catalog_matches_swift_settings() {
        assert_eq!(AVAILABLE_MODELS.len(), 4);
        let hebrew = catalog_model("Turbo V3 Hebrew").unwrap();
        assert_eq!(hebrew.filename, "ggml-ivrit-large-v3-turbo.bin");
        assert_eq!(hebrew.preferred_language, Some("he"));
        // Display name and filename both resolve to the same entry.
        assert_eq!(
            catalog_model("Turbo V3 large").unwrap().filename,
            catalog_model("ggml-large-v3-turbo.bin").unwrap().filename
        );
        assert!(catalog_model("no-such-model").is_none());
    }

    #[test]
    fn model_path_resolves_names_and_rejects_traversal() {
        let (_dir, mgr) = manager();
        let by_name = mgr.model_path("Turbo V3 small").unwrap();
        assert_eq!(
            by_name,
            mgr.models_dir().join("ggml-large-v3-turbo-q5_0.bin")
        );
        assert_eq!(by_name, mgr.model_path("ggml-large-v3-turbo-q5_0.bin").unwrap());

        for bad in ["../evil.bin", "a/b.bin", "..", ""] {
            assert!(
                matches!(mgr.model_path(bad), Err(ModelManagerError::InvalidName(_))),
                "expected InvalidName for {bad:?}"
            );
        }
    }

    #[test]
    fn is_downloaded_installed_delete_round_trip() {
        let (_dir, mgr) = manager();
        assert!(!mgr.is_model_downloaded("ggml-large-v3-turbo.bin"));
        assert!(mgr.installed_models().unwrap().is_empty());

        write_fake_valid_model(&mgr.models_dir().join("ggml-large-v3-turbo.bin"));
        // A stray .partial must not count as installed.
        fs::write(mgr.models_dir().join("ggml-tiny.bin.partial"), b"junk").unwrap();

        assert!(mgr.is_model_downloaded("ggml-large-v3-turbo.bin"));
        assert!(mgr.is_model_downloaded("Turbo V3 large"));
        let installed = mgr.installed_models().unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(
            installed[0].file_name().unwrap(),
            "ggml-large-v3-turbo.bin"
        );

        mgr.delete_model("Turbo V3 large").unwrap();
        assert!(!mgr.is_model_downloaded("ggml-large-v3-turbo.bin"));
        assert!(matches!(
            mgr.delete_model("Turbo V3 large"),
            Err(ModelManagerError::NotInstalled(_))
        ));
    }

    #[test]
    fn finalize_rejects_html_error_page_and_removes_partial() {
        // The core invariant: a bad body never becomes an installed model.
        let (_dir, mgr) = manager();
        let dest = mgr.models_dir().join("ggml-large-v3-turbo.bin");
        let partial = partial_path(&dest);
        fs::write(&partial, b"<html><body>502 Bad Gateway</body></html>").unwrap();

        let err = finalize_download(&partial, &dest).unwrap_err();
        assert!(matches!(err, ModelManagerError::Validation(_)));
        assert!(!partial.exists(), "failed partial must be deleted");
        assert!(!dest.exists(), "bad download must never reach the models dir");
        assert!(!mgr.is_model_downloaded("ggml-large-v3-turbo.bin"));
    }

    #[test]
    fn finalize_rejects_large_file_without_magic() {
        let (_dir, mgr) = manager();
        let dest = mgr.models_dir().join("ggml-large-v3-turbo.bin");
        let partial = partial_path(&dest);
        fs::write(&partial, vec![0u8; 2_000_000]).unwrap();

        let err = finalize_download(&partial, &dest).unwrap_err();
        assert!(matches!(err, ModelManagerError::Validation(_)));
        assert!(!partial.exists());
        assert!(!dest.exists());
    }

    #[test]
    fn finalize_promotes_valid_partial() {
        let (_dir, mgr) = manager();
        let dest = mgr.models_dir().join("ggml-large-v3-turbo.bin");
        let partial = partial_path(&dest);
        write_fake_valid_model(&partial);

        let installed = finalize_download(&partial, &dest).unwrap();
        assert_eq!(installed, dest);
        assert!(!partial.exists());
        assert!(mgr.is_model_downloaded("ggml-large-v3-turbo.bin"));
    }

    #[test]
    fn ensure_default_model_copies_once() {
        let (dir, mgr) = manager();
        let bundled = dir.path().join("ggml-tiny.en.bin");
        write_fake_valid_model(&bundled);

        let dest = mgr.ensure_default_model(&bundled).unwrap();
        assert_eq!(dest, mgr.models_dir().join("ggml-tiny.en.bin"));
        assert!(mgr.is_model_downloaded("ggml-tiny.en.bin"));

        // Second call is a no-op: it must not overwrite the installed file.
        fs::write(&dest, b"user-modified").unwrap();
        mgr.ensure_default_model(&bundled).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"user-modified");
    }

    #[test]
    fn download_of_installed_model_short_circuits_with_full_progress() {
        let (_dir, mgr) = manager();
        write_fake_valid_model(&mgr.models_dir().join("ggml-large-v3-turbo.bin"));

        let mut last = None;
        let path = mgr
            .download_model(
                "Turbo V3 large",
                Arc::new(AtomicBool::new(false)),
                |p| last = Some(p),
            )
            .unwrap();
        assert_eq!(path, mgr.models_dir().join("ggml-large-v3-turbo.bin"));
        let last = last.unwrap();
        assert_eq!(Some(last.bytes_downloaded), last.total_bytes);
        assert_eq!(last.fraction(), Some(1.0));
    }

    #[test]
    fn download_of_unknown_model_errors_without_network() {
        let (_dir, mgr) = manager();
        let err = mgr
            .download_model("no-such-model", Arc::new(AtomicBool::new(false)), |_| {})
            .unwrap_err();
        assert!(matches!(err, ModelManagerError::UnknownModel(_)));
    }

    #[test]
    fn progress_fraction_matches_swift_semantics() {
        let p = DownloadProgress {
            bytes_downloaded: 50,
            total_bytes: Some(200),
        };
        assert_eq!(p.fraction(), Some(0.25));
        // Unknown total -> no fraction (Swift returns nil).
        let p = DownloadProgress {
            bytes_downloaded: 50,
            total_bytes: None,
        };
        assert_eq!(p.fraction(), None);
        // Overshoot clamps to 1.0 (Swift min(..., 1.0)).
        let p = DownloadProgress {
            bytes_downloaded: 300,
            total_bytes: Some(200),
        };
        assert_eq!(p.fraction(), Some(1.0));
    }

    /// Real-network smoke test against the smallest real ggml model on
    /// Hugging Face (~78 MB, ggml-tiny.bin — smaller than anything in the
    /// user-facing catalog). Run with:
    ///
    /// ```sh
    /// cargo test -p model-manager -- --ignored real_download_smoke
    /// ```
    #[test]
    #[ignore = "hits the network; run explicitly with -- --ignored"]
    fn real_download_smoke() {
        let (_dir, mgr) = manager();
        let dest = mgr.models_dir().join("ggml-tiny.bin");
        let mut calls = 0u32;
        let path = mgr
            .download_url(
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin?download=true",
                &dest,
                Arc::new(AtomicBool::new(false)),
                &mut |p| {
                    calls += 1;
                    if let Some(total) = p.total_bytes {
                        assert!(p.bytes_downloaded <= total);
                    }
                },
            )
            .unwrap();
        assert!(calls > 1, "expected streaming progress callbacks");
        assert!(path.is_file());
        assert!(mgr.is_model_downloaded("ggml-tiny.bin"));
        validate_downloaded_model(&path).unwrap();
    }
}
