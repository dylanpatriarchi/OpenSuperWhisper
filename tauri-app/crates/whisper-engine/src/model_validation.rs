//! Port of `WhisperModelManager.validateDownloadedModel` (Swift). Rejects a
//! download whose *content* is not a model even though the HTTP status was
//! 200 — without this, a captive-portal page or a Git-LFS pointer file gets
//! stored as a model and `is_model_downloaded` reports true forever.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// whisper.cpp's `GGML_FILE_MAGIC`, stored little-endian at the head of every
/// ggml model file.
const GGML_FILE_MAGIC: u32 = 0x6767_6d6c;

/// Smallest plausible whisper model. Captive-portal interstitials, CDN error
/// pages and Git-LFS pointer files are all well under this.
const MINIMUM_PLAUSIBLE_MODEL_SIZE: u64 = 1_000_000;

#[derive(Debug, thiserror::Error)]
pub enum ModelValidationError {
    #[error(
        "the downloaded file is only {0} bytes; the server most likely returned an error page instead of the model"
    )]
    TooSmall(u64),
    #[error(
        "the downloaded file is not a valid GGML model; the download may have been intercepted or corrupted"
    )]
    NotAGgmlFile,
    #[error("failed to read the downloaded file: {0}")]
    Io(#[from] std::io::Error),
}

/// Whether the file begins with `GGML_FILE_MAGIC`. Split out from the size
/// check so it can be exercised against small-but-valid ggml files such as
/// the bundled Silero VAD model.
pub fn has_ggml_magic(path: &Path) -> std::io::Result<bool> {
    let mut file = File::open(path)?;
    let mut head = [0u8; 4];
    if file.read_exact(&mut head).is_err() {
        return Ok(false);
    }
    Ok(u32::from_le_bytes(head) == GGML_FILE_MAGIC)
}

pub fn validate_downloaded_model(path: &Path) -> Result<(), ModelValidationError> {
    let size = std::fs::metadata(path)?.len();
    if size < MINIMUM_PLAUSIBLE_MODEL_SIZE {
        return Err(ModelValidationError::TooSmall(size));
    }
    if !has_ggml_magic(path)? {
        return Err(ModelValidationError::NotAGgmlFile);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(bytes).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn rejects_html_error_page() {
        let f = write_temp(b"<html><body>502 Bad Gateway</body></html>");
        let err = validate_downloaded_model(f.path()).unwrap_err();
        assert!(matches!(err, ModelValidationError::TooSmall(_)));
    }

    #[test]
    fn rejects_large_file_without_magic() {
        let mut bytes = vec![0u8; MINIMUM_PLAUSIBLE_MODEL_SIZE as usize + 16];
        bytes[0..4].copy_from_slice(b"\x00\x00\x00\x00");
        let f = write_temp(&bytes);
        let err = validate_downloaded_model(f.path()).unwrap_err();
        assert!(matches!(err, ModelValidationError::NotAGgmlFile));
    }

    #[test]
    fn accepts_large_file_with_magic() {
        let mut bytes = vec![0u8; MINIMUM_PLAUSIBLE_MODEL_SIZE as usize + 16];
        bytes[0..4].copy_from_slice(&GGML_FILE_MAGIC.to_le_bytes());
        let f = write_temp(&bytes);
        validate_downloaded_model(f.path()).unwrap();
    }

    #[test]
    fn has_ggml_magic_is_independent_of_size() {
        let mut bytes = vec![0u8; 32];
        bytes[0..4].copy_from_slice(&GGML_FILE_MAGIC.to_le_bytes());
        let f = write_temp(&bytes);
        assert!(has_ggml_magic(f.path()).unwrap());
    }

    #[test]
    fn has_ggml_magic_false_on_truncated_header() {
        let f = write_temp(b"ab");
        assert!(!has_ggml_magic(f.path()).unwrap());
    }
}
