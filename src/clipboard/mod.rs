#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::{layered_copy_text, paste_suggestion, CopyResult};
