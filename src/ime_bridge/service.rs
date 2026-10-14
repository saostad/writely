//! D-Bus service for the Linux input method bridge protocol (v1).
//!
//! Serves `dev.writely.ImeBridge1` at `/dev/writely/ImeBridge` under the
//! well-known name `dev.writely.Writely` on the session bus. See
//! `docs/linux-ime-bridge.md`. Runs on its own thread via the zbus blocking
//! API, so the tao event loop never blocks.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Condvar, Mutex, MutexGuard, OnceLock,
};
use std::time::{Duration, Instant};

use tao::event_loop::EventLoopProxy;
use zbus::blocking::{fdo::DBusProxy, Connection};
use zbus::fdo::{RequestNameFlags, RequestNameReply};
use zbus::message::Header;
use zbus::names::{BusName, UniqueName};
use zbus::DBusError;

use crate::UserEvent;

use super::{BridgeState, ClickAction, FocusedKind, ReplaceWait};

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
    bridge_connected: AtomicBool,
}

/// What the focused context looks like, for change detection.
type Fingerprint = (Option<(String, String)>, Option<FocusedKind>);

fn fingerprint(state: &BridgeState) -> Fingerprint {
    let key = state
        .focused_key()
        .map(|(sender, context)| (sender.to_string(), context.to_string()));
    (key, state.focused_kind())
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
        let first_from_sender = message.is_some();
        if let Some(message) = message {
            eprintln!("Writely: {message}");
        }
        drop(state);
        // Once, on the first Hello from any bridge, tell the event loop to
        // clear the waiting-for-add-on state.
        if first_from_sender && !self.shared.bridge_connected.swap(true, Ordering::SeqCst) {
            if let Err(error) = self.shared.proxy.send_event(UserEvent::BridgeConnected {
                bridge: bridge.to_string(),
            }) {
                eprintln!("Writely: could not deliver the bridge event: {error}");
            }
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

    fn text_unavailable(
        &self,
        #[zbus(header)] header: Header<'_>,
        context: &str,
        can_select: bool,
    ) -> Result<(), BridgeError> {
        let sender = sender_of(&header);
        let mut state = lock(&self.shared.state);
        let before = fingerprint(&state);
        if let Err(message) = state.text_unavailable(&sender, context, can_select) {
            eprintln!("Writely: {message}");
            return Err(BridgeError::InvalidArgument(message));
        }
        notify_if_changed(&self.shared.proxy, &state, &before);
        Ok(())
    }

    fn selected(
        &self,
        #[zbus(header)] header: Header<'_>,
        context: &str,
        request: u32,
        ok: bool,
        text: &str,
        error: &str,
    ) -> Result<(), BridgeError> {
        let sender = sender_of(&header);
        {
            let state = lock(&self.shared.state);
            if let Err(message) = state.selected(&sender, context, request, ok, text, error) {
                eprintln!("Writely: {message}");
                return Err(BridgeError::InvalidArgument(message));
            }
        }
        // Stored only when it answers the in-flight `SelectRequest`; results
        // for any other request, context or sender are ignored.
        let stored =
            lock(&self.shared.replace_wait).insert_full(&sender, context, request, ok, error, text);
        if stored {
            self.shared.replace_cv.notify_one();
        }
        eprintln!(
            "Writely: input method bridge Selected for context \"{context}\" request {request}: ok={ok} error=\"{error}\"."
        );
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
        // Linux has no Cancel button; Cancel only hides, which bridge-mode
        // popup::hide() does by emitting Hide. Dismiss takes the same path
        // macOS Cancel uses: CaretChanged hides the popup, bumps the
        // generation and clears the in-flight state.
        let event = match validated {
            ClickAction::Check => UserEvent::IconClicked,
            ClickAction::Apply => UserEvent::ReplaceClicked,
            ClickAction::Dismiss => UserEvent::CaretChanged,
        };
        if let Err(error) = self.shared.proxy.send_event(event) {
            eprintln!("Writely: could not deliver the bridge click event: {error}");
        }
        eprintln!("Writely: input method bridge Clicked {action} for context \"{context}\".");
        Ok(())
    }

    fn destroy(&self, #[zbus(header)] header: Header<'_>, context: &str) {
        let sender = sender_of(&header);
        let mut state = lock(&self.shared.state);
        let before = fingerprint(&state);
        state.destroy(&sender, context);
        notify_if_changed(&self.shared.proxy, &state, &before);
        // Wake any answer waiter: the context may be gone.
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
        let stored = lock(&self.shared.replace_wait).insert(&sender, context, request, ok, error);
        if stored {
            self.shared.replace_cv.notify_one();
        }
        eprintln!(
            "Writely: input method bridge ReplaceResult for context \"{context}\" request {request}: ok={ok} error=\"{error}\"."
        );
        Ok(())
    }
}

fn emit<B>(signal: &str, destination: &str, body: &B) -> Result<(), String>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let Some(connection) = CONNECTION.get() else {
        return Err(format!(
            "could not emit the {signal} signal: the input method bridge is not running."
        ));
    };
    connection
        .emit_signal(Some(destination), OBJECT_PATH, INTERFACE_NAME, signal, body)
        .map_err(|error| format!("could not emit the {signal} signal: {error}"))
}

pub fn emit_show_icon(destination: &str, context: &str) -> Result<(), String> {
    emit("ShowIcon", destination, &(context,))
}

pub fn emit_show_message(
    destination: &str,
    context: &str,
    text: &str,
    is_error: bool,
    can_apply: bool,
) -> Result<(), String> {
    emit(
        "ShowMessage",
        destination,
        &(context, text, is_error, can_apply),
    )
}

pub fn emit_hide(destination: &str, context: &str) -> Result<(), String> {
    emit("Hide", destination, &(context,))
}

pub fn emit_replace(
    destination: &str,
    context: &str,
    request: u32,
    start: u32,
    end: u32,
    text: &str,
) -> Result<(), String> {
    emit(
        "Replace",
        destination,
        &(context, request, start, end, text),
    )
}

pub fn emit_select_request(destination: &str, context: &str, request: u32) -> Result<(), String> {
    emit("SelectRequest", destination, &(context, request))
}

pub fn emit_replace_selection(
    destination: &str,
    context: &str,
    request: u32,
    text: &str,
) -> Result<(), String> {
    emit("ReplaceSelection", destination, &(context, request, text))
}

fn shared() -> Option<Arc<Shared>> {
    SHARED.get().cloned()
}

/// The focused bridge context and what it offers, for bridge-mode reads.
/// `None` when nothing is focused or usable — or when the bridge isn't
/// serving.
pub(crate) fn focused_snapshot() -> Option<(String, String, FocusedKind)> {
    let shared = shared()?;
    let state = lock(&shared.state);
    let (sender, context) = state.focused_key()?;
    let kind = state.focused_kind()?;
    Some((sender.to_string(), context.to_string(), kind))
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

fn select_closed_error() -> String {
    "The input method closed the text field before the selection finished.".to_string()
}

/// Waits for the in-flight bridge answer (`ReplaceResult` or `Selected`).
/// No D-Bus, no statics: the caller supplies the wait state, the wakeup, a
/// `context_exists` check and the error messages. Returns the text payload
/// (`Selected`'s text; empty for `ReplaceResult`).
/// Never holds a guard across another `lock` of the same mutex.
fn wait_for_answer(
    answer_wait: &Mutex<ReplaceWait>,
    answer_cv: &Condvar,
    timeout: Duration,
    context_exists: impl Fn() -> bool,
    fail_message: impl Fn(String) -> String,
    timeout_message: &str,
    closed_message: &str,
) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    loop {
        // Take into a local first: the guard must be dropped before the
        // `clear()` below locks the same mutex again.
        let result = lock(answer_wait).take_full();
        if let Some((ok, error, text)) = result {
            lock(answer_wait).clear();
            if ok {
                return Ok(text);
            }
            return Err(fail_message(error));
        }
        if !context_exists() {
            lock(answer_wait).clear();
            return Err(closed_message.to_string());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            lock(answer_wait).clear();
            return Err(timeout_message.to_string());
        }
        let wait = lock(answer_wait);
        // Re-check under the lock before sleeping: the answer may have
        // landed between the take above and this lock.
        if wait.has_result() {
            continue;
        }
        let _ = answer_cv
            .wait_timeout(wait, remaining)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
}

/// Waits for the in-flight `Replace` answer. No D-Bus, no statics: the
/// caller supplies the wait state, the wakeup and a `context_exists` check.
/// Never holds a guard across another `lock` of the same mutex.
fn wait_for_replace(
    replace_wait: &Mutex<ReplaceWait>,
    replace_cv: &Condvar,
    timeout: Duration,
    context_exists: impl Fn() -> bool,
) -> Result<(), String> {
    wait_for_answer(
        replace_wait,
        replace_cv,
        timeout,
        context_exists,
        |error| format!("The input method could not replace the text: {error}"),
        "The input method didn't confirm the replacement within 3 seconds.",
        &closed_error(),
    )
    .map(|_| ())
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
    if let Err(error) = emit_replace(sender, context, request, start, end, text) {
        lock(&shared.replace_wait).clear();
        return Err(format!(
            "Could not send the replacement to the input method: {error}"
        ));
    }
    wait_for_replace(
        &shared.replace_wait,
        &shared.replace_cv,
        Duration::from_secs(3),
        || lock(&shared.state).context_exists(sender, context),
    )
}

/// Emits `SelectRequest` with the next request id and waits up to 2 seconds
/// for the matching `Selected`, returning the selected text. Shares the one
/// in-flight wait with `replace_and_wait`; both run on the event-loop
/// thread, so they never overlap. Never holds the state lock while waiting.
pub(crate) fn select_and_wait(sender: &str, context: &str) -> Result<String, String> {
    let shared = shared().ok_or_else(select_closed_error)?;
    let request = lock(&shared.state).next_request();
    // Mark the request pending before emitting, so a fast answer can't be
    // missed between the emit and the wait.
    lock(&shared.replace_wait).begin(sender, context, request);
    if let Err(error) = emit_select_request(sender, context, request) {
        lock(&shared.replace_wait).clear();
        return Err(format!(
            "Could not send the selection request to the input method: {error}"
        ));
    }
    let text = wait_for_answer(
        &shared.replace_wait,
        &shared.replace_cv,
        Duration::from_secs(2),
        || lock(&shared.state).context_exists(sender, context),
        |error| format!("The input method could not select the text: {error}"),
        "The input method didn't answer the selection request within 2 seconds.",
        &select_closed_error(),
    )?;
    if text.is_empty() {
        return Err(
            "There is no text before the cursor on this line. Click at the end of the sentence you want to check."
                .to_string(),
        );
    }
    Ok(text)
}

/// Emits `ReplaceSelection` with the next request id and waits up to
/// 3 seconds for the matching `ReplaceResult`. Same messages as
/// `replace_and_wait`.
pub(crate) fn replace_selection_and_wait(
    sender: &str,
    context: &str,
    text: &str,
) -> Result<(), String> {
    let shared = shared().ok_or_else(closed_error)?;
    let request = lock(&shared.state).next_request();
    lock(&shared.replace_wait).begin(sender, context, request);
    if let Err(error) = emit_replace_selection(sender, context, request, text) {
        lock(&shared.replace_wait).clear();
        return Err(format!(
            "Could not send the replacement to the input method: {error}"
        ));
    }
    wait_for_replace(
        &shared.replace_wait,
        &shared.replace_cv,
        Duration::from_secs(3),
        || lock(&shared.state).context_exists(sender, context),
    )
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
        bridge_connected: AtomicBool::new(false),
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
        // Wake any answer waiter: its context may be gone.
        shared.replace_cv.notify_all();
    }
    eprintln!("Writely: the input method bridge lost its D-Bus connection.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// Runs the wait on a spawned thread so a regression fails after
    /// 5 seconds instead of hanging `cargo test`.
    fn run_wait(
        wait: Arc<Mutex<ReplaceWait>>,
        cv: Arc<Condvar>,
        timeout: Duration,
        context_exists: impl Fn() -> bool + Send + 'static,
    ) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = wait_for_replace(wait.as_ref(), cv.as_ref(), timeout, context_exists);
            let _ = tx.send(result);
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("wait_for_replace hung: no result within 5 seconds")
    }

    fn pending() -> (Arc<Mutex<ReplaceWait>>, Arc<Condvar>) {
        let wait = Arc::new(Mutex::new(ReplaceWait::new()));
        lock(&wait).begin(":1.1", "ctx", 1);
        (wait, Arc::new(Condvar::new()))
    }

    #[test]
    fn wait_returns_ok_when_result_arrives() {
        let (wait, cv) = pending();
        let deliver_wait = wait.clone();
        let deliver_cv = cv.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            lock(&deliver_wait).insert(":1.1", "ctx", 1, true, "");
            deliver_cv.notify_one();
        });
        assert_eq!(run_wait(wait, cv, Duration::from_secs(3), || true), Ok(()));
    }

    #[test]
    fn wait_returns_error_when_bridge_reports_failure() {
        let (wait, cv) = pending();
        let deliver_wait = wait.clone();
        let deliver_cv = cv.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            lock(&deliver_wait).insert(":1.1", "ctx", 1, false, "boom");
            deliver_cv.notify_one();
        });
        assert_eq!(
            run_wait(wait, cv, Duration::from_secs(3), || true),
            Err("The input method could not replace the text: boom".to_string())
        );
    }

    #[test]
    fn wait_returns_timeout_when_no_result() {
        let (wait, cv) = pending();
        assert_eq!(
            run_wait(wait, cv, Duration::from_millis(100), || true),
            Err("The input method didn't confirm the replacement within 3 seconds.".to_string())
        );
    }

    #[test]
    fn wait_returns_closed_when_context_disappears() {
        let (wait, cv) = pending();
        let exists = Arc::new(AtomicBool::new(true));
        let exists_writer = exists.clone();
        let notify_cv = cv.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            exists_writer.store(false, Ordering::SeqCst);
            notify_cv.notify_all();
        });
        assert_eq!(
            run_wait(wait, cv, Duration::from_secs(3), move || {
                exists.load(Ordering::SeqCst)
            }),
            Err(
                "The input method closed the text field before the replacement finished."
                    .to_string()
            )
        );
    }
}
