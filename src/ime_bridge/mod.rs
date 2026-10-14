//! Linux input method bridge (Wayland): Writely side of the D-Bus protocol.
//!
//! See `docs/linux-ime-bridge.md`. Round 1 builds the protocol state and the
//! D-Bus service; round 2 wires the signals into the check flow.

pub(crate) mod service;
mod state;

pub(crate) use service::{current_text, focused_snapshot, replace_and_wait};
pub use service::{emit_hide, emit_show_icon, emit_show_message, start};
pub use state::{BridgeState, ClickAction, FocusedText, ReplaceWait};
