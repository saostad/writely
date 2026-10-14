//! D-Bus service for the Linux input method bridge protocol (v1).
//!
//! Serves `dev.writely.ImeBridge1` at `/dev/writely/ImeBridge` under the
//! well-known name `dev.writely.Writely` on the session bus. See
//! `docs/linux-ime-bridge.md`. Runs on its own thread via the zbus blocking
//! API, so the tao event loop never blocks.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use tao::event_loop::EventLoopProxy;
use zbus::blocking::{fdo::DBusProxy, Connection};
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::message::Header;
use zbus::names::{BusName, UniqueName};
use zbus::DBusError;

use crate::UserEvent;

use super::{BridgeState, FocusedText, ReplaceWait};

const WELL_KNOWN_NAME: &str = "dev.writely.Writely";
const OBJECT_PATH: &str = "/dev/writely/ImeBridge";
const INTERFACE_NAME: &str = "dev.writely.ImeBridge1";

/// The serving connection, for the `emit_*` unicast signals below.
static CONNECTION: OnceLock<Connection> = OnceLock::new();

/// The serving state, for bridge-mode reads from other threads.
static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

#[derive(Debug, DBusError)]
#[zbus(prefix = "dev.writely.ImeBridge1.Error")]
enum BridgeError {
    InvalidArgument(String),
}

struct Shared {
    state: Mutex<BridgeState>,
    proxy: EventLoopProxy<UserEvent>,
    replace_wait: Mutex<ReplaceWait>,
    replace_cv: Condvar,
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
        {
            let state = lock(&self.shared.state);
            if let Err(message) = state.clicked(&sender, context, action) {
                eprintln!("Writely: {message}");
                return Err(BridgeError::InvalidArgument(message));
            }
        }
        // Round 2 item 3 dispatches the action to the event loop.
        eprintln!("Writely: input method bridge Clicked {action} for context \"{context}\".");
        Ok(())
    }

    fn destroy(&self, #[zbus(header)] header: Header<'_>, context: &str) {
        let sender = sender_of(&header);
        let mut state = lock(&self.shared.state);
        let before = fingerprint(&state);
        state.destroy(&sender, context);
        notify_if_changed(&self.shared.proxy, &state, &before);
        // Wake any `Replace` waiter: the context may be gone.
        self.shared.replace_cv.notify_all();
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
        // Stored only when it answers the in-flight `Replace`; results for
        // any other request, context or sender are ignored.
        if lock(&self.shared.replace_wait).insert(&sender, context, request, ok, error) {
            self.shared.replace_cv.notify_one();
        }
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

fn shared() -> Option<Arc<Shared>> {
    SHARED.get().cloned()
}

/// The focused bridge context and its text, for bridge-mode reads.
/// `None` when nothing is focused, the field is a password, or no
/// surrounding text arrived yet — or when the bridge isn't serving.
pub(crate) fn focused_snapshot() -> Option<(String, String, FocusedText)> {
    let shared = shared()?;
    let state = lock(&shared.state);
    let (sender, context) = state.focused_key()?;
    let focused = state.focused_text()?;
    Some((sender.to_string(), context.to_string(), focused))
}

/// The latest surrounding text for one context, if it exists.
pub(crate) fn current_text(sender: &str, context: &str) -> Option<String> {
    let shared = shared()?;
    let state = lock(&shared.state);
    state.current_text(sender, context)
}

fn closed_error() -> String {
    "The input method closed the text field before the replacement finished.".to_string()
}

/// Emits `Replace` with the next request id and waits up to 3 seconds for
/// the matching `ReplaceResult`. Never holds the state lock while waiting,
/// and the service thread never waits on the caller, so this can't deadlock
/// the event loop.
pub(crate) fn replace_and_wait(
    sender: &str,
    context: &str,
    start: u32,
    end: u32,
    text: &str,
) -> Result<(), String> {
    let shared = shared().ok_or_else(closed_error)?;
    let request = lock(&shared.state).next_request();
    // Mark the request pending before emitting, so a fast answer can't be
    // missed between the emit and the wait.
    lock(&shared.replace_wait).begin(sender, context, request);
    emit_replace(sender, context, request, start, end, text);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some((ok, error)) = lock(&shared.replace_wait).take() {
            lock(&shared.replace_wait).clear();
            if ok {
                return Ok(());
            }
            return Err(format!(
                "The input method could not replace the text: {error}"
            ));
        }
        if !lock(&shared.state).context_exists(sender, context) {
            lock(&shared.replace_wait).clear();
            return Err(closed_error());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            lock(&shared.replace_wait).clear();
            return Err(
                "The input method didn't confirm the replacement within 3 seconds.".to_string(),
            );
        }
        let wait = lock(&shared.replace_wait);
        // Re-check under the lock before sleeping: the answer may have
        // landed between the take above and this lock.
        if wait.has_result() {
            continue;
        }
        let _ = shared
            .replace_cv
            .wait_timeout(wait, remaining)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
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
        replace_wait: Mutex::new(ReplaceWait::new()),
        replace_cv: Condvar::new(),
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
    // DoNotQueue: a second Writely must fail fast instead of queuing for
    // the name and believing it is serving.
    match connection.request_name_with_flags(WELL_KNOWN_NAME, RequestNameFlags::DoNotQueue.into()) {
        Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {}
        Ok(_) => {
            eprintln!(
                "Writely: could not start the input method bridge on D-Bus: \
                 the name {WELL_KNOWN_NAME} is already taken (is another Writely running?)"
            );
            return;
        }
        Err(error) => {
            let detail = match error {
                zbus::Error::NameTaken => {
                    format!(
                        "the name {WELL_KNOWN_NAME} is already taken (is another Writely running?)"
                    )
                }
                other => other.to_string(),
            };
            eprintln!("Writely: could not start the input method bridge on D-Bus: {detail}");
            return;
        }
    }
    // The local `connection` stays alive for the `names` iterator below.
    if CONNECTION.set(connection.clone()).is_err() {
        // Keep the first connection installed for this process.
    }
    if SHARED.set(shared.clone()).is_err() {
        // Keep the first state installed for this process.
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
        // Wake any `Replace` waiter: its context may be gone.
        shared.replace_cv.notify_all();
    }
    eprintln!("Writely: the input method bridge lost its D-Bus connection.");
}
