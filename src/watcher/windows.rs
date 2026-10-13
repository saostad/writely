use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use tao::event_loop::EventLoopProxy;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_FOCUS, EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_VALUECHANGE,
    EVENT_SYSTEM_FOREGROUND, OBJID_CARET, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
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

fn hook_range(min: u32, max: u32) -> Result<HWINEVENTHOOK, String> {
    // SAFETY: out-of-context hook with no module; callbacks run on this thread,
    // which is the tao main thread with a running message pump.
    let hook = unsafe {
        SetWinEventHook(
            min,
            max,
            None,
            Some(win_event_callback),
            0,
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

pub struct WatcherInner {
    hooks: Vec<HWINEVENTHOOK>,
}

impl WatcherInner {
    pub fn start(proxy: EventLoopProxy<UserEvent>) -> Result<Self, String> {
        if PROXY.set(proxy).is_err() {
            // Keep the first proxy installed for this process.
        }
        let foreground_hook = hook_range(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND)?;
        let object_hook = match hook_range(EVENT_OBJECT_FOCUS, EVENT_OBJECT_VALUECHANGE) {
            Ok(hook) => hook,
            Err(error) => {
                // SAFETY: foreground_hook was just installed and is unhooked
                // exactly once here.
                if !unsafe { UnhookWinEvent(foreground_hook) }.as_bool() {
                    eprintln!("Writely: could not remove a text cursor event hook.");
                }
                return Err(error);
            }
        };
        Ok(Self {
            hooks: vec![foreground_hook, object_hook],
        })
    }

    pub fn handle_caret_changed(&mut self) {
        clear_pending();
    }
}

impl Drop for WatcherInner {
    fn drop(&mut self) {
        for hook in self.hooks.drain(..) {
            // SAFETY: the hook was returned by SetWinEventHook and is unhooked
            // exactly once here.
            if !unsafe { UnhookWinEvent(hook) }.as_bool() {
                eprintln!("Writely: could not remove a text cursor event hook.");
            }
        }
    }
}
