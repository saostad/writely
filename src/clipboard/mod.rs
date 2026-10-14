#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::{copy_selected_text, paste_suggestion, CopyResult};
