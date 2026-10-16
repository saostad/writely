use tao::event_loop::EventLoopProxy;

use crate::providers::RewriteStyle;
use crate::text_access::CaretRect;
use crate::UserEvent;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Stores the event-loop proxy so the Replace button can send
/// `UserEvent::ReplaceClicked`. Call once at startup.
pub fn init(proxy: EventLoopProxy<UserEvent>) {
    #[cfg(target_os = "macos")]
    {
        macos::init(proxy);
    }
    #[cfg(target_os = "windows")]
    {
        windows::init(proxy);
    }
    #[cfg(target_os = "linux")]
    {
        linux::init(proxy);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = proxy;
    }
}

/// Shows the popup near the caret rectangle (top-left screen coords).
/// `is_error` renders the text in red; `show_button` shows Replace.
pub fn show_message(text: &str, is_error: bool, show_button: bool, caret: &CaretRect) {
    #[cfg(target_os = "macos")]
    {
        macos::show_message(text, is_error, show_button, caret);
    }
    #[cfg(target_os = "windows")]
    {
        windows::show_message(text, is_error, show_button, caret);
    }
    #[cfg(target_os = "linux")]
    {
        linux::show_message(text, is_error, show_button, caret);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        let _ = (text, is_error, show_button, caret);
    }
}

/// Shows the suggestion with the Cancel/Replace footer. Only used on Linux:
/// macOS and Windows show every suggestion through `show_tabbed`, while Linux
/// has no diff view and falls back to the plain suggestion text.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn show_suggestion(original: &str, suggestion: &str, caret: &CaretRect) {
    show_message(suggestion, false, true, caret);
    let _ = original;
}

/// The tabs of the suggestion popup: the grammar correction, or one rewrite
/// style. macOS and Windows show them in a menu button in the footer; Linux
/// keeps today's single-view popup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PopupTab {
    Correct,
    Rewrite(RewriteStyle),
}

/// The body shown above the style menu: a word diff, or a plain message (with
/// an optional spinner while a rewrite loads).
pub enum TabContent<'a> {
    Diff {
        original: &'a str,
        suggestion: &'a str,
    },
    Message {
        text: &'a str,
        is_error: bool,
        loading: bool,
    },
}

/// Shows the popup with the style menu button showing `selected`. On Linux
/// this falls back to today's behaviour: `Diff` shows the suggestion,
/// `Message` shows plain text without the Replace button.
pub fn show_tabbed(selected: PopupTab, content: TabContent, caret: &CaretRect) {
    #[cfg(target_os = "macos")]
    {
        macos::show_tabbed(selected, content, caret);
    }
    #[cfg(target_os = "windows")]
    {
        windows::show_tabbed(selected, content, caret);
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        match content {
            TabContent::Diff {
                original,
                suggestion,
            } => show_suggestion(original, suggestion, caret),
            TabContent::Message {
                text,
                is_error,
                loading,
            } => {
                let _ = loading;
                show_message(text, is_error, false, caret);
            }
        }
        let _ = selected;
    }
}

pub fn hide() {
    #[cfg(target_os = "macos")]
    {
        macos::hide();
    }
    #[cfg(target_os = "windows")]
    {
        windows::hide();
    }
    #[cfg(target_os = "linux")]
    {
        linux::hide();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {}
}

/// Sets the primary (Replace) button's label: `"Copy"` for terminal
/// apps, `"Replace"` otherwise. Windows sets the button text; a no-op
/// on macOS and Linux.
pub fn set_primary_label(label: &str) {
    #[cfg(target_os = "windows")]
    {
        windows::set_primary_label(label);
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = label;
    }
}

pub fn focus_handoff_recent() -> bool {
    #[cfg(target_os = "windows")]
    {
        windows::focus_handoff_recent()
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

pub fn is_visible() -> bool {
    #[cfg(target_os = "macos")]
    {
        macos::is_visible()
    }
    #[cfg(target_os = "windows")]
    {
        windows::is_visible()
    }
    #[cfg(target_os = "linux")]
    {
        linux::is_visible()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        false
    }
}
