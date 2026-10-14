//! D-Bus service for the Linux input method bridge protocol (v1).
//!
//! Serves `dev.writely.ImeBridge1` at `/dev/writely/ImeBridge` under the
//! well-known name `dev.writely.Writely` on the session bus. See
//! `docs/linux-ime-bridge.md`. Runs on its own thread via the zbus blocking
//! API, so the tao event loop never blocks.

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use tao::event_loop::EventLoopProxy;
use zbus::blocking::{fdo::DBusProxy, Connection};
use zbus::message::Header;
use zbus::names::{BusName, UniqueName};
use zbus::DBusError;

use crate::UserEvent;

use super::{BridgeState, ClickAction, FocusedText};

const WELL_KNOWN_NAME: &str = "dev.writely.Writely";
const OBJECT_PATH: &str = "/dev/writely/ImeBridge";
const INTERFACE_NAME: &str = "dev.writely.ImeBridge1";

/// The serving connection, for the `emit_*` unicast signals below.
static CONNECTION: OnceLock<Connection> = OnceLock::new();

#[derive(Debug, DBusError)]
#[zbus(prefix = "dev.writely.ImeBridge1.Error")]
enum BridgeError {
    InvalidArgument(String),
}

/// A stored `Clicked` call, for round 2 wiring.
#[allow(dead_code)]
struct ClickedEvent {
    sender: String,
    context: String,
    action: ClickAction,
}

/// A stored `ReplaceResult` call, for round 2 wiring.
#[allow(dead_code)]
struct ReplaceResultEvent {
    sender: String,
    context: String,
    request: u32,
    ok: bool,
    error: String,
}

struct Shared {
    state: Mutex<BridgeState>,
    proxy: EventLoopProxy<UserEvent>,
    clicked: Mutex<Vec<ClickedEvent>>,
    replace_results: Mutex<Vec<ReplaceResultEvent>>,
}

/// What the focused context looks like, for change detection.
type Fingerprint = (Option<(String, String)>, Option<FocusedText>);

fn fingerprint(state: &BridgeState) -> Fingerprint {
    let key = state
        .focused_key()
        .map(|(sender, context)| (sender.to_string(), context.to_string()));
    (key, state.focused_text())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Same pattern as `watcher::linux` `flag_caret_changed`, but the proxy is
/// held directly instead of in a static.
fn flag_caret_changed(proxy: &EventLoopProxy<UserEvent>) {
    if let Err(error) = proxy.send_event(UserEvent::CaretChanged) {
        eprintln!("Writely: could not deliver the caret event: {error}");
    }
}

fn notify_if_changed(proxy: &EventLoopProxy<UserEvent>, state: &BridgeState, before: &Fingerprint) {
    if &fingerprint(state) != before {
        flag_caret_changed(proxy);
    }
}

fn sender_of(header: &Header<'_>) -> String {
    header
        .sender()
        .map(|name| name.as_str().to_string())
        .unwrap_or_default()
}

struct ImeBridge {
    shared: Arc<Shared>,
}

#[zbus::interface(name = "dev.writely.ImeBridge1")]
impl ImeBridge {
    fn hello(&self, #[zbus(header)] header: Header<'_>, bridge: &str, bridge_version: &str) -> u32 {
        let sender = sender_of(&header);
        let mut state = lock(&self.shared.state);
        let (version, message) = state.hello(&sender, bridge, bridge_version);
        if let Some(message) = message {
            eprintln!("Writely: {message}");
        }
        version
    }

    fn focus_in(
        &self,
        #[zbus(header)] header: Header<'_>,
        context: &str,
        program: &str,
        password: bool,
    ) {
        let sender = sender_of(&header);
        let mut state = lock(&self.shared.state);
        let before = fingerprint(&state);
        state.focus_in(&sender, context, program, password);
        notify_if_changed(&self.shared.proxy, &state, &before);
    }

    fn focus_out(&self, #[zbus(header)] header: Header<'_>, context: &str) {
        let sender = sender_of(&header);
        let mut state = lock(&self.shared.state);
        let before = fingerprint(&state);
        state.focus_out(&sender, context);
        notify_if_changed(&self.shared.proxy, &state, &before);
    }

    fn surrounding_text(
        &self,
        #[zbus(header)] header: Header<'_>,
        context: &str,
        text: &str,
        cursor: u32,
        anchor: u32,
    ) -> Result<(), BridgeError> {
        let sender = sender_of(&header);
        let mut state = lock(&self.shared.state);
        let before = fingerprint(&state);
        if let Err(message) = state.surrounding_text(&sender, context, text, cursor, anchor) {
            eprintln!("Writely: {message}");
            return Err(BridgeError::InvalidArgument(message));
        }
        notify_if_changed(&self.shared.proxy, &state, &before);
        Ok(())
    }

    fn clicked(
        &self,
        #[zbus(header)] header: Header<'_>,
        context: &str,
        action: &str,
    ) -> Result<(), BridgeError> {
        let sender = sender_of(&header);
        let validated = {
            let state = lock(&self.shared.state);
            match state.clicked(&sender, context, action) {
                Ok(validated) => validated,
                Err(message) => {
                    eprintln!("Writely: {message}");
                    return Err(BridgeError::InvalidArgument(message));
                }
            }
        };
        lock(&self.shared.clicked).push(ClickedEvent {
            sender,
            context: context.to_string(),
            action: validated,
        });
        eprintln!("Writely: input method bridge Clicked {action} for context \"{context}\".");
        Ok(())
    }

    fn replace_result(
        &self,
        #[zbus(header)] header: Header<'_>,
        context: &str,
        request: u32,
        ok: bool,
        error: &str,
    ) -> Result<(), BridgeError> {
        let sender = sender_of(&header);
        {
            let state = lock(&self.shared.state);
            if let Err(message) = state.replace_result(&sender, context, request, ok, error) {
                eprintln!("Writely: {message}");
                return Err(BridgeError::InvalidArgument(message));
            }
        }
        lock(&self.shared.replace_results).push(ReplaceResultEvent {
            sender,
            context: context.to_string(),
            request,
            ok,
            error: error.to_string(),
        });
        eprintln!(
            "Writely: input method bridge ReplaceResult for context \"{context}\" request {request}: ok={ok} error=\"{error}\"."
        );
        Ok(())
    }
}

fn emit<B>(signal: &str, destination: &str, body: &B)
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let Some(connection) = CONNECTION.get() else {
        eprintln!(
            "Writely: could not emit the {signal} signal: the input method bridge is not running."
        );
        return;
    };
    if let Err(error) =
        connection.emit_signal(Some(destination), OBJECT_PATH, INTERFACE_NAME, signal, body)
    {
        eprintln!("Writely: could not emit the {signal} signal: {error}");
    }
}

// Used from round 2 wiring.
#[allow(dead_code)]
pub fn emit_show_icon(destination: &str, context: &str) {
    emit("ShowIcon", destination, &(context,));
}

// Used from round 2 wiring.
#[allow(dead_code)]
pub fn emit_show_message(
    destination: &str,
    context: &str,
    text: &str,
    is_error: bool,
    can_apply: bool,
) {
    emit(
        "ShowMessage",
        destination,
        &(context, text, is_error, can_apply),
    );
}

// Used from round 2 wiring.
#[allow(dead_code)]
pub fn emit_hide(destination: &str, context: &str) {
    emit("Hide", destination, &(context,));
}

// Used from round 2 wiring.
#[allow(dead_code)]
pub fn emit_replace(
    destination: &str,
    context: &str,
    request: u32,
    start: u32,
    end: u32,
    text: &str,
) {
    emit(
        "Replace",
        destination,
        &(context, request, start, end, text),
    );
}

/// Starts the bridge on its own thread. Never blocks the caller.
pub fn start(proxy: EventLoopProxy<UserEvent>) {
    if let Err(error) = std::thread::Builder::new()
        .name("writely-ime-bridge".to_string())
        .spawn(move || serve(proxy))
    {
        eprintln!("Writely: could not start the input method bridge on D-Bus: {error}");
    }
}

fn serve(proxy: EventLoopProxy<UserEvent>) {
    let connection = match Connection::session() {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("Writely: could not start the input method bridge on D-Bus: {error}");
            return;
        }
    };
    let shared = Arc::new(Shared {
        state: Mutex::new(BridgeState::new()),
        proxy,
        clicked: Mutex::new(Vec::new()),
        replace_results: Mutex::new(Vec::new()),
    });
    if let Err(error) = connection.object_server().at(
        OBJECT_PATH,
        ImeBridge {
            shared: shared.clone(),
        },
    ) {
        eprintln!("Writely: could not start the input method bridge on D-Bus: {error}");
        return;
    }
    // Set up disconnect detection before claiming the name, so a failure
    // here still reads as "could not start".
    let dbus = match DBusProxy::new(&connection) {
        Ok(dbus) => dbus,
        Err(error) => {
            eprintln!("Writely: could not start the input method bridge on D-Bus: {error}");
            return;
        }
    };
    let names = match dbus.receive_name_owner_changed() {
        Ok(names) => names,
        Err(error) => {
            eprintln!("Writely: could not start the input method bridge on D-Bus: {error}");
            return;
        }
    };
    if let Err(error) = connection.request_name(WELL_KNOWN_NAME) {
        let detail = match error {
            zbus::Error::NameTaken => {
                format!("the name {WELL_KNOWN_NAME} is already taken (is another Writely running?)")
            }
            other => other.to_string(),
        };
        eprintln!("Writely: could not start the input method bridge on D-Bus: {detail}");
        return;
    }
    // The local `connection` stays alive for the `names` iterator below.
    if CONNECTION.set(connection.clone()).is_err() {
        // Keep the first connection installed for this process.
    }
    for signal in names {
        let args = match signal.args() {
            Ok(args) => args,
            Err(error) => {
                eprintln!("Writely: could not read a bus name change: {error}");
                continue;
            }
        };
        let BusName::Unique(unique) = args.name() else {
            continue;
        };
        let new_owner: Option<UniqueName<'_>> = args.new_owner().clone().into();
        if new_owner.is_some() {
            continue;
        }
        let mut state = lock(&shared.state);
        let before = fingerprint(&state);
        state.sender_vanished(unique.as_str());
        notify_if_changed(&shared.proxy, &state, &before);
    }
    eprintln!("Writely: the input method bridge lost its D-Bus connection.");
}
