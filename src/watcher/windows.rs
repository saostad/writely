use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use tao::event_loop::EventLoopProxy;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, EVENT_OBJECT_FOCUS, EVENT_OBJECT_LOCATIONCHANGE,
    EVENT_OBJECT_VALUECHANGE, EVENT_SYSTEM_FOREGROUND, OBJID_CARET, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS,
};

use crate::UserEvent;

static PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();
static PENDING: AtomicBool = AtomicBool::new(false);

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

// SAFETY: the hook, window and object/child ids are provided by the system for
// the duration of the call, and the callback only flags a caret change.
unsafe extern "system" fn win_event_callback(
    _hook: HWINEVENTHOOK,
    event: u32,
    _hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    match event {
        EVENT_SYSTEM_FOREGROUND | EVENT_OBJECT_FOCUS | EVENT_OBJECT_VALUECHANGE => {
            flag_caret_changed();
        }
        EVENT_OBJECT_LOCATIONCHANGE if id_object == OBJID_CARET.0 => {
            flag_caret_changed();
        }
        _ => {}
    }
}

fn foreground_pid() -> Option<u32> {
    // SAFETY: querying the foreground window takes no borrowed state.
    let window: HWND = unsafe { GetForegroundWindow() };
    if window.is_invalid() {
        return None;
    }
    let mut pid: u32 = 0;
    // SAFETY: window is a live window and pid is writable.
    unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
    Some(pid)
}

fn hook_single(event: u32, pid: u32) -> Result<HWINEVENTHOOK, String> {
    // SAFETY: out-of-context hook with no module; callbacks run on this thread,
    // which is the tao main thread with a running message pump.
    let hook = unsafe {
        SetWinEventHook(
            event,
            event,
            None,
            Some(win_event_callback),
            pid,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    if hook.is_invalid() {
        Err(format!(
            "Could not watch the text cursor: {}",
            windows::core::Error::from_thread()
        ))
    } else {
        Ok(hook)
    }
}

fn unhook(hook: HWINEVENTHOOK) {
    // SAFETY: the hook was returned by SetWinEventHook and is unhooked
    // exactly once here.
    if !unsafe { UnhookWinEvent(hook) }.as_bool() {
        eprintln!("Writely: could not remove a text cursor event hook.");
    }
}

pub struct WatcherInner {
    foreground_hook: Option<HWINEVENTHOOK>,
    object_hooks: Vec<HWINEVENTHOOK>,
    /// The process the object hooks follow; 0 means none are installed.
    hooked_pid: u32,
}

impl WatcherInner {
    pub fn start(proxy: EventLoopProxy<UserEvent>) -> Result<Self, String> {
        if PROXY.set(proxy).is_err() {
            // Keep the first proxy installed for this process.
        }
        // Foreground changes arrive for every process; the object events below
        // follow only the foreground process.
        let foreground_hook = hook_single(EVENT_SYSTEM_FOREGROUND, 0)?;
        let mut inner = Self {
            foreground_hook: Some(foreground_hook),
            object_hooks: Vec::new(),
            hooked_pid: 0,
        };
        inner.rehook();
        Ok(inner)
    }

    /// Follow the foreground process: unhook the old object hooks first, then
    /// hook focus/value/caret events for the current foreground process only.
    /// The mouse moving over other apps never wakes Writely this way.
    fn rehook(&mut self) {
        let pid = match foreground_pid() {
            Some(pid) => pid,
            None => return,
        };
        if pid == self.hooked_pid {
            return;
        }
        for hook in self.object_hooks.drain(..) {
            unhook(hook);
        }
        self.hooked_pid = 0;
        if pid == std::process::id() {
            // Never watch our own windows.
            return;
        }
        let mut hooks = Vec::new();
        for event in [
            EVENT_OBJECT_FOCUS,
            EVENT_OBJECT_VALUECHANGE,
            EVENT_OBJECT_LOCATIONCHANGE,
        ] {
            match hook_single(event, pid) {
                Ok(hook) => hooks.push(hook),
                Err(error) => {
                    eprintln!("Writely: {error}");
                    for hook in hooks.drain(..) {
                        unhook(hook);
                    }
                    return;
                }
            }
        }
        self.object_hooks = hooks;
        self.hooked_pid = pid;
    }

    pub fn handle_caret_changed(&mut self) {
        clear_pending();
        // The foreground app may have changed: re-follow it.
        self.rehook();
    }
}

impl Drop for WatcherInner {
    fn drop(&mut self) {
        if let Some(hook) = self.foreground_hook.take() {
            unhook(hook);
        }
        for hook in self.object_hooks.drain(..) {
            unhook(hook);
        }
    }
}
