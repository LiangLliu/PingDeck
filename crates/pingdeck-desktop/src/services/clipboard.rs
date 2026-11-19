//! Pure-Rust OS clipboard helper backed by `arboard`.

use pingdeck_core::error::AppError;

/// Writes plain UTF-8 text to the OS clipboard.
pub fn set_text(text: &str) -> Result<(), AppError> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|error| AppError::io(format!("clipboard unavailable: {error}")))?;
    clipboard
        .set_text(text.to_owned())
        .map_err(|error| AppError::io(format!("clipboard write failed: {error}")))
}

/// Writes an environment or provider secret directly from Rust without touching UI logs.
pub fn set_secret_text(secret: &str) -> Result<(), AppError> {
    set_text(secret)
}
