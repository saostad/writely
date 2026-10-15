#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::{layered_copy_text, paste_suggestion};
#[cfg(target_os = "windows")]
pub use windows::{layered_copy_text, paste_suggestion};

#[derive(Debug)]
pub enum CopyResult {
    /// The newly copied text.
    Text(String),
    /// Nothing new was copied (or it was empty): the user should select text.
    NothingNew,
}
