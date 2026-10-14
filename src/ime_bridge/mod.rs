//! Linux input method bridge (Wayland): Writely side of the D-Bus protocol.
//!
//! See `docs/linux-ime-bridge.md`. Round 1 builds the protocol state and the
//! D-Bus service; round 2 wires the signals into the check flow.

pub(crate) mod service;
mod state;

pub use service::start;
pub use state::{BridgeState, ClickAction, FocusedText};
