use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::c_void;
use std::ptr::{null_mut, NonNull};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2_app_kit::{
    NSRunningApplication, NSWorkspace, NSWorkspaceDidActivateApplicationNotification,
};
use objc2_application_services::{AXError, AXObserver, AXUIElement};
use objc2_core_foundation::{
    kCFRunLoopCommonModes, CFAbsoluteTimeGetCurrent, CFBoolean, CFRetained, CFRunLoop,
    CFRunLoopTimer, CFString, CFType, Type,
};
use objc2_foundation::{NSObjectProtocol, NSOperationQueue};

use tao::event_loop::EventLoopProxy;

use crate::text_access::macos_helpers;
use crate::UserEvent;

static PENDING: AtomicBool = AtomicBool::new(false);
static FOCUS_DIRTY: AtomicBool = AtomicBool::new(false);
static PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();

fn flag_caret_changed() {
    if !PENDING.swap(true, Ordering::SeqCst) {
        if let Some(proxy) = PROXY.get() {
            if proxy.send_event(UserEvent::CaretChanged).is_err() {
                PENDING.store(false, Ordering::SeqCst);
            }
        } else {
            PENDING.store(false, Ordering::SeqCst);
        }
    }
}

pub fn clear_pending() {
    PENDING.store(false, Ordering::SeqCst);
}

// SAFETY: the observer, element and notification are valid AX objects for the duration of the call and refcon is unused.
unsafe extern "C-unwind" fn ax_callback(
    _observer: NonNull<AXObserver>,
    _element: NonNull<AXUIElement>,
    notification: NonNull<CFString>,
    _refcon: *mut c_void,
) {
    // SAFETY: the notification is a valid CFString for the duration of the call.
    let name: &CFString = unsafe { notification.as_ref() };
    if name.to_string() == "AXFocusedUIElementChanged" {
        FOCUS_DIRTY.store(true, Ordering::SeqCst);
    }
    flag_caret_changed();
}

struct Inner {
    observer: Option<CFRetained<AXObserver>>,
    app_element: Option<CFRetained<AXUIElement>>,
    app_pid: u32,
    focused_element: Option<CFRetained<AXUIElement>>,
    /// Cached with `focused_element`, refreshed only when focus changes.
    focused_editable: bool,
    logged_manual_pids: HashSet<u32>,
    /// Pids where Writely set `AXEnhancedUserInterface` to true after
    /// reading false, so quit can put them back.
    enhanced_pids: HashSet<u32>,
    /// Pids where the enable/failure report already logged (once per pid).
    logged_report_pids: HashSet<u32>,
    /// Pids where the 4 s timeout report already logged (once per pid).
    logged_timeout_pids: HashSet<u32>,
    /// Pids where a retry window already started; cleared when the pid is
    /// deactivated so a re-activation may retry once more.
    retried_pids: HashSet<u32>,
    retry_timer: Option<CFRetained<CFRunLoopTimer>>,
    retry_pid: u32,
    retry_remaining: u8,
}

/// Retry cadence after enabling cursor reporting: every 500 ms for up to 4 s.
const RETRY_INTERVAL_SECS: f64 = 0.5;
const RETRY_ATTEMPTS: u8 = 8;

/// Whether the app reports a focused element, for `needs_enhanced_ui`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FocusedPresence {
    Present,
    Absent,
    NoValue,
}

/// Whether Writely should set `AXEnhancedUserInterface` on the app: the app
/// reports no focused element and `AXManualAccessibility` did not succeed.
/// Never set it on apps that already report a focused element (it slows
/// window animations), so `Present` is always false.
pub(crate) fn needs_enhanced_ui(manual: AXError, focused: FocusedPresence) -> bool {
    matches!(focused, FocusedPresence::Absent | FocusedPresence::NoValue)
        && manual != AXError::Success
}

thread_local! {
    static INNER: RefCell<Option<Inner>> = const { RefCell::new(None) };
    static WORKSPACE_TOKEN: RefCell<Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>> =
        const { RefCell::new(None) };
}

fn focused_element_for_app(app: &AXUIElement) -> Result<Option<CFRetained<AXUIElement>>, AXError> {
    let value = macos_helpers::copy_attr(app, "AXFocusedUIElement")?;
    let ptr = CFRetained::as_ptr(&value).cast::<AXUIElement>();
    // SAFETY: AXFocusedUIElement yields an AXUIElement valid while value is alive.
    let element_ref: &AXUIElement = unsafe { ptr.as_ref() };
    Ok(Some(element_ref.retain()))
}

fn read_bool_attr(app: &AXUIElement, name: &str) -> Option<bool> {
    let value = macos_helpers::copy_attr(app, name).ok()?;
    value.downcast_ref::<CFBoolean>().map(|flag| flag.as_bool())
}

fn set_bool_attr(app: &AXUIElement, name: &str, value: bool) -> AXError {
    let attr = CFString::from_str(name);
    let flag: &CFType = CFBoolean::new(value);
    // SAFETY: app, attr and flag are valid CF objects of the expected kinds.
    unsafe { app.set_attribute_value(&attr, flag) }
}

fn app_display_name(pid: u32, app: &AXUIElement) -> String {
    if let Some(running) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid as i32)
    {
        if let Some(name) = running.localizedName() {
            let text = name.to_string();
            if !text.trim().is_empty() {
                return text;
            }
        }
    }
    if let Ok(value) = macos_helpers::copy_attr(app, "AXTitle") {
        if let Some(title) = value.downcast_ref::<CFString>() {
            let text = title.to_string();
            if !text.trim().is_empty() {
                return text;
            }
        }
    }
    format!("pid {pid}")
}

fn cancel_retry(inner: &mut Inner) {
    if let Some(timer) = inner.retry_timer.take() {
        if let Some(run_loop) = CFRunLoop::main() {
            // SAFETY: kCFRunLoopCommonModes is a valid run loop mode.
            run_loop.remove_timer(Some(&timer), unsafe { kCFRunLoopCommonModes });
        }
        timer.invalidate();
    }
    inner.retry_pid = 0;
    inner.retry_remaining = 0;
}

// SAFETY: called by CFRunLoop on the main thread with a valid timer; it only touches main-thread state.
unsafe extern "C-unwind" fn retry_timer_fired(_timer: *mut CFRunLoopTimer, _info: *mut c_void) {
    retry_tick();
}

fn start_retry(inner: &mut Inner, pid: u32) {
    cancel_retry(inner);
    let fire = CFAbsoluteTimeGetCurrent() + RETRY_INTERVAL_SECS;
    // SAFETY: the callback only touches main-thread state and no context pointer is used.
    let timer = unsafe {
        CFRunLoopTimer::new(
            None,
            fire,
            RETRY_INTERVAL_SECS,
            0,
            0,
            Some(retry_timer_fired),
            null_mut(),
        )
    };
    let Some(timer) = timer else {
        eprintln!("Writely: could not start the cursor-reporting retry timer.");
        return;
    };
    if let Some(run_loop) = CFRunLoop::main() {
        // SAFETY: kCFRunLoopCommonModes is a valid run loop mode.
        run_loop.add_timer(Some(&timer), unsafe { kCFRunLoopCommonModes });
        inner.retry_timer = Some(timer);
        inner.retry_pid = pid;
        inner.retry_remaining = RETRY_ATTEMPTS;
    } else {
        eprintln!("Writely: could not add the cursor-reporting retry timer to the main run loop.");
    }
}

/// One 500 ms retry tick: re-register the focused element and flag a caret
/// change. Stops early once a focused element is registered or the frontmost
/// app changed; after 4 s logs that no cursor appeared.
fn retry_tick() {
    INNER.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let Some(inner) = borrow.as_mut() else {
            return;
        };
        if inner.retry_pid == 0 || inner.retry_timer.is_none() {
            return;
        }
        if inner.retry_pid != inner.app_pid {
            cancel_retry(inner);
            return;
        }
        refresh_focused_registration(inner);
        if inner.focused_element.is_some() {
            cancel_retry(inner);
            flag_caret_changed();
            return;
        }
        flag_caret_changed();
        if inner.retry_remaining > 0 {
            inner.retry_remaining -= 1;
        }
        if inner.retry_remaining == 0 {
            let pid = inner.retry_pid;
            let name = inner
                .app_element
                .as_ref()
                .map(|app| app_display_name(pid, app))
                .unwrap_or_else(|| format!("pid {pid}"));
            cancel_retry(inner);
            if inner.logged_timeout_pids.insert(pid) {
                eprintln!(
                    "Writely: turned on cursor reporting for {name} but no text cursor appeared within 4 s."
                );
            }
        }
    });
}

fn teardown_inner(inner: &mut Inner) {
    cancel_retry(inner);
    if let (Some(observer), Some(app)) = (&inner.observer, &inner.app_element) {
        let notification = CFString::from_str("AXFocusedUIElementChanged");
        // SAFETY: observer and app are valid AX objects registered above.
        let _ = unsafe { observer.remove_notification(app, &notification) };
    }
    if let (Some(observer), Some(focused)) = (&inner.observer, &inner.focused_element) {
        let selected = CFString::from_str("AXSelectedTextChanged");
        let value = CFString::from_str("AXValueChanged");
        // SAFETY: observer and focused are valid AX objects registered above.
        let _ = unsafe { observer.remove_notification(focused, &selected) };
        // SAFETY: observer and focused are valid AX objects registered above.
        let _ = unsafe { observer.remove_notification(focused, &value) };
    }
    if let Some(observer) = &inner.observer {
        // SAFETY: the observer's run loop source is valid while the observer is alive.
        let source = unsafe { observer.run_loop_source() };
        if let Some(run_loop) = CFRunLoop::main() {
            // SAFETY: kCFRunLoopCommonModes is a valid run loop mode.
            run_loop.remove_source(Some(&source), unsafe { kCFRunLoopCommonModes });
        }
    }
    inner.observer = None;
    inner.app_element = None;
    inner.focused_element = None;
    inner.focused_editable = false;
    inner.app_pid = 0;
}

fn refresh_focused_registration(inner: &mut Inner) {
    let (Some(observer), Some(app_element)) = (inner.observer.clone(), inner.app_element.clone())
    else {
        return;
    };
    let new_focused = match focused_element_for_app(&app_element) {
        Ok(element) => element,
        Err(error) if error == AXError::NoValue || error == AXError::AttributeUnsupported => None,
        Err(error) => {
            if error == AXError::CannotComplete {
                eprintln!(
                    "Writely: The focused app didn't respond to Writely in time (AXError {}).",
                    error.0
                );
            } else {
                eprintln!(
                    "Writely: Could not read the focused text field: AXError {}",
                    error.0
                );
            }
            None
        }
    };
    if let (Some(existing), Some(new)) = (&inner.focused_element, &new_focused) {
        // PartialEq on AXUIElement is CFEqual, not a pointer comparison: AX hands
        // back a new reference on every read of the same element.
        if existing == new {
            return;
        }
    }
    if let Some(old) = inner.focused_element.take() {
        let selected = CFString::from_str("AXSelectedTextChanged");
        let value = CFString::from_str("AXValueChanged");
        // SAFETY: observer and old are valid AX objects registered above.
        let _ = unsafe { observer.remove_notification(&old, &selected) };
        // SAFETY: observer and old are valid AX objects registered above.
        let _ = unsafe { observer.remove_notification(&old, &value) };
    }
    if let Some(new_element) = new_focused {
        macos_helpers::set_messaging_timeout(&new_element);
        inner.focused_editable = macos_helpers::element_is_editable(&new_element);
        let selected = CFString::from_str("AXSelectedTextChanged");
        let value = CFString::from_str("AXValueChanged");
        // SAFETY: observer and new_element are valid and refcon is unused.
        let selected_error =
            unsafe { observer.add_notification(&new_element, &selected, null_mut()) };
        if selected_error != AXError::Success
            && selected_error != AXError::NotificationAlreadyRegistered
        {
            eprintln!(
                "Writely: could not watch text changes (AXError {}).",
                selected_error.0
            );
        }
        // SAFETY: observer and new_element are valid and refcon is unused.
        let value_error = unsafe { observer.add_notification(&new_element, &value, null_mut()) };
        if value_error != AXError::Success && value_error != AXError::NotificationAlreadyRegistered
        {
            eprintln!(
                "Writely: could not watch text changes (AXError {}).",
                value_error.0
            );
        }
        inner.focused_element = Some(new_element);
        if inner.retry_pid != 0 && inner.retry_pid == inner.app_pid {
            cancel_retry(inner);
        }
    } else {
        inner.focused_editable = false;
    }
}

fn setup_for_pid(pid: u32) {
    INNER.with(|cell| {
        let mut borrow = cell.borrow_mut();
        let inner = borrow.get_or_insert_with(|| Inner {
            observer: None,
            app_element: None,
            app_pid: 0,
            focused_element: None,
            focused_editable: false,
            logged_manual_pids: HashSet::new(),
            enhanced_pids: HashSet::new(),
            logged_report_pids: HashSet::new(),
            logged_timeout_pids: HashSet::new(),
            retried_pids: HashSet::new(),
            retry_timer: None,
            retry_pid: 0,
            retry_remaining: 0,
        });
        if inner.app_pid == pid && inner.observer.is_some() {
            return;
        }
        let old_pid = inner.app_pid;
        teardown_inner(inner);
        // A re-activation may retry once more; a pid left behind can be
        // retried again when the user comes back to it.
        if old_pid != 0 && old_pid != pid {
            inner.retried_pids.remove(&old_pid);
        }
        if pid == std::process::id() {
            flag_caret_changed();
            return;
        }
        // SAFETY: pid comes from NSWorkspace and identifies a live process.
        let app_element = unsafe { AXUIElement::new_application(pid as i32) };
        macos_helpers::set_messaging_timeout(&app_element);
        let manual_attr = CFString::from_str("AXManualAccessibility");
        let manual_value: &CFType = CFBoolean::new(true);
        // SAFETY: app_element and manual_value are valid CF objects of the expected kinds.
        let manual_error = unsafe { app_element.set_attribute_value(&manual_attr, manual_value) };
        // Read the focused element before deciding about cursor reporting:
        // apps that already report one (TextEdit, Notes) are never touched.
        let presence = match focused_element_for_app(&app_element) {
            Ok(Some(_)) => FocusedPresence::Present,
            Ok(None) => FocusedPresence::Absent,
            Err(error)
                if error == AXError::NoValue || error == AXError::AttributeUnsupported =>
            {
                if error == AXError::NoValue {
                    FocusedPresence::NoValue
                } else {
                    FocusedPresence::Absent
                }
            }
            Err(_) => FocusedPresence::Present,
        };
        let mut newly_enabled = false;
        if presence == FocusedPresence::Present {
            if manual_error == AXError::AttributeUnsupported {
                // Apps that already report focus and do not expose this
                // attribute (e.g. Electron) need no action.
            } else if manual_error != AXError::Success && inner.logged_manual_pids.insert(pid) {
                eprintln!(
                    "Writely: could not enable manual accessibility for pid {pid}: AXError {}",
                    manual_error.0
                );
            }
        } else if manual_error == AXError::Success {
            let name = app_display_name(pid, &app_element);
            if inner.logged_report_pids.insert(pid) {
                eprintln!(
                    "Writely: turned on cursor reporting (AXManualAccessibility) for {name}."
                );
            }
            newly_enabled = true;
        } else if needs_enhanced_ui(manual_error, presence) {
            let name = app_display_name(pid, &app_element);
            let initial = read_bool_attr(&app_element, "AXEnhancedUserInterface");
            let set_error = set_bool_attr(&app_element, "AXEnhancedUserInterface", true);
            let sent = set_error == AXError::Success || set_error == AXError::CannotComplete;
            let readback = read_bool_attr(&app_element, "AXEnhancedUserInterface");
            let confirmed = readback == Some(true);
            if sent && confirmed {
                if initial == Some(false) {
                    inner.enhanced_pids.insert(pid);
                }
                if inner.logged_report_pids.insert(pid) {
                    eprintln!(
                        "Writely: turned on cursor reporting (AXEnhancedUserInterface) for {name}."
                    );
                }
                newly_enabled = true;
            } else if inner.logged_report_pids.insert(pid) {
                let readback_str = match readback {
                    Some(true) => "true",
                    Some(false) => "false",
                    None => "unreadable",
                };
                eprintln!(
                    "Writely: could not turn on cursor reporting for {name}: AXManualAccessibility AXError {}, AXEnhancedUserInterface AXError {} (readback {readback_str}).",
                    manual_error.0,
                    set_error.0
                );
            }
        } else if inner.logged_report_pids.insert(pid) {
            let name = app_display_name(pid, &app_element);
            eprintln!(
                "Writely: could not turn on cursor reporting for {name}: AXManualAccessibility AXError {}.",
                manual_error.0
            );
        }
        let mut raw: *mut AXObserver = null_mut();
        let out = NonNull::from(&mut raw);
        // SAFETY: out points at a live slot and the callback only flags a caret change.
        let create_error = unsafe { AXObserver::create(pid as i32, Some(ax_callback), out) };
        if create_error != AXError::Success {
            eprintln!(
                "Writely: could not create the Accessibility observer for pid {pid}: AXError {}",
                create_error.0
            );
            return;
        }
        let Some(observer_ptr) = NonNull::new(raw) else {
            eprintln!(
                "Writely: could not create the Accessibility observer for pid {pid}: AXError {}",
                AXError::Failure.0
            );
            return;
        };
        // SAFETY: a successful create returns a +1-retained observer owned by the caller.
        let observer = unsafe { CFRetained::from_raw(observer_ptr) };
        // SAFETY: the observer's run loop source is valid while the observer is alive.
        let source = unsafe { observer.run_loop_source() };
        if let Some(run_loop) = CFRunLoop::main() {
            // SAFETY: kCFRunLoopCommonModes is a valid run loop mode.
            run_loop.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
        } else {
            eprintln!("Writely: could not add the Accessibility observer to the main run loop.");
            return;
        }
        let focus_notification = CFString::from_str("AXFocusedUIElementChanged");
        // SAFETY: observer and app_element are valid and refcon is unused.
        let focus_error =
            unsafe { observer.add_notification(&app_element, &focus_notification, null_mut()) };
        if focus_error != AXError::Success && focus_error != AXError::NotificationAlreadyRegistered
        {
            eprintln!(
                "Writely: could not watch focus changes for pid {pid}: AXError {}",
                focus_error.0
            );
        }
        inner.observer = Some(observer);
        inner.app_element = Some(app_element);
        inner.app_pid = pid;
        // Chrome builds its accessibility tree asynchronously with no
        // focus-change event, so poll the focused element on the main run
        // loop for up to 4 s after newly enabling cursor reporting.
        if newly_enabled && inner.retried_pids.insert(pid) {
            start_retry(inner, pid);
        }
        // A re-activation was consumed above; allow a later activation of
        // another pid to retry again.
        FOCUS_DIRTY.store(true, Ordering::SeqCst);
        flag_caret_changed();
    });
}

/// Whether the cursor-reporting retry window is currently active for `pid`.
/// `text_access` uses this to skip the premature "can't find the text
/// cursor" message while a retry is still running; the window's own 4 s
/// timeout line covers a lasting failure.
pub(crate) fn retry_active_for(pid: u32) -> bool {
    INNER.with(|cell| {
        cell.borrow()
            .as_ref()
            .is_some_and(|inner| inner.retry_pid == pid && inner.retry_timer.is_some())
    })
}

/// The watcher's cached focused element, pid and per-focus editable flag for
/// `caret_info`. `None` when nothing is watched: the caller falls back to the
/// system-wide query.
pub(crate) fn cached_focused() -> Option<(CFRetained<AXUIElement>, u32, bool)> {
    INNER.with(|cell| {
        let borrow = cell.borrow();
        let inner = borrow.as_ref()?;
        let element = inner.focused_element.clone()?;
        Some((element, inner.app_pid, inner.focused_editable))
    })
}

pub struct WatcherInner {
    _private: (),
}

impl WatcherInner {
    pub fn start(proxy: EventLoopProxy<UserEvent>) -> Result<Self, String> {
        if PROXY.set(proxy).is_err() {
            // A proxy is already installed; keep the first one for this process.
        }
        INNER.with(|cell| {
            if cell.borrow().is_none() {
                *cell.borrow_mut() = Some(Inner {
                    observer: None,
                    app_element: None,
                    app_pid: 0,
                    focused_element: None,
                    focused_editable: false,
                    logged_manual_pids: HashSet::new(),
                    enhanced_pids: HashSet::new(),
                    logged_report_pids: HashSet::new(),
                    logged_timeout_pids: HashSet::new(),
                    retried_pids: HashSet::new(),
                    retry_timer: None,
                    retry_pid: 0,
                    retry_remaining: 0,
                });
            }
        });
        let workspace = NSWorkspace::sharedWorkspace();
        let center = workspace.notificationCenter();
        // SAFETY: NSWorkspaceDidActivateApplicationNotification is a framework-provided constant.
        let name = unsafe { NSWorkspaceDidActivateApplicationNotification };
        let queue = NSOperationQueue::mainQueue();
        let block = block2::RcBlock::new(
            move |_notification: NonNull<objc2_foundation::NSNotification>| {
                let workspace = NSWorkspace::sharedWorkspace();
                if let Some(app) = workspace.frontmostApplication() {
                    let pid = app.processIdentifier();
                    if pid > 0 {
                        setup_for_pid(pid as u32);
                    }
                }
            },
        );
        // SAFETY: the notification name and main queue are valid and the block only reads the frontmost app.
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(name),
                None,
                Some(&queue),
                &block,
            )
        };
        WORKSPACE_TOKEN.with(|cell| {
            *cell.borrow_mut() = Some(token);
        });
        if let Some(app) = workspace.frontmostApplication() {
            let pid = app.processIdentifier();
            if pid > 0 {
                setup_for_pid(pid as u32);
            }
        }
        Ok(Self { _private: () })
    }

    pub fn handle_caret_changed(&mut self) {
        clear_pending();
        if FOCUS_DIRTY.swap(false, Ordering::SeqCst) {
            INNER.with(|cell| {
                if let Some(inner) = cell.borrow_mut().as_mut() {
                    refresh_focused_registration(inner);
                }
            });
        }
    }
}

impl Drop for WatcherInner {
    fn drop(&mut self) {
        let enhanced: Vec<u32> = INNER.with(|cell| {
            if let Some(mut inner) = cell.borrow_mut().take() {
                let pids: Vec<u32> = inner.enhanced_pids.iter().copied().collect();
                teardown_inner(&mut inner);
                pids
            } else {
                Vec::new()
            }
        });
        // Put back what Writely changed: apps left with
        // AXEnhancedUserInterface on get it set back to false. Failures are
        // logged and never block quit.
        for pid in enhanced {
            if NSRunningApplication::runningApplicationWithProcessIdentifier(pid as i32).is_none() {
                continue;
            }
            // SAFETY: pid was a live frontmost app and identifies a process.
            let app = unsafe { AXUIElement::new_application(pid as i32) };
            macos_helpers::set_messaging_timeout(&app);
            let name = app_display_name(pid, &app);
            let error = set_bool_attr(&app, "AXEnhancedUserInterface", false);
            if error != AXError::Success {
                eprintln!(
                    "Writely: could not restore cursor reporting for {name}: AXError {}.",
                    error.0
                );
            }
        }
        WORKSPACE_TOKEN.with(|cell| {
            if let Some(token) = cell.borrow_mut().take() {
                let center = NSWorkspace::sharedWorkspace().notificationCenter();
                let ptr = Retained::as_ptr(&token).cast::<AnyObject>();
                // SAFETY: token is the observer returned when the workspace observer was added and ptr is valid while token is alive.
                let observer: &AnyObject = unsafe { &*ptr };
                // SAFETY: token is the observer returned when the workspace observer was added.
                unsafe { center.removeObserver(observer) };
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{needs_enhanced_ui, FocusedPresence};
    use objc2_application_services::AXError;

    #[test]
    fn never_sets_enhanced_ui_when_focus_is_reported() {
        for manual in [
            AXError::Success,
            AXError::Failure,
            AXError::AttributeUnsupported,
            AXError::CannotComplete,
        ] {
            assert!(
                !needs_enhanced_ui(manual, FocusedPresence::Present),
                "manual {manual:?} with a focused element must not set enhanced UI"
            );
        }
    }

    #[test]
    fn sets_enhanced_ui_only_when_manual_did_not_succeed() {
        assert!(needs_enhanced_ui(
            AXError::AttributeUnsupported,
            FocusedPresence::NoValue
        ));
        assert!(needs_enhanced_ui(
            AXError::AttributeUnsupported,
            FocusedPresence::Absent
        ));
        assert!(needs_enhanced_ui(
            AXError::Failure,
            FocusedPresence::NoValue
        ));
        assert!(!needs_enhanced_ui(
            AXError::Success,
            FocusedPresence::NoValue
        ));
        assert!(!needs_enhanced_ui(
            AXError::Success,
            FocusedPresence::Absent
        ));
    }

    #[test]
    fn retry_constants_cover_four_seconds() {
        assert_eq!(super::RETRY_ATTEMPTS, 8);
        assert_eq!(
            super::RETRY_ATTEMPTS as f64 * super::RETRY_INTERVAL_SECS,
            4.0
        );
    }
}
