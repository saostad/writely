use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};

use crate::config::{self, AppSettings, Provider};
use crate::providers::local;
use crate::settings;

use super::{install, server};

static LAST_ACTIVITY: Mutex<Option<Instant>> = Mutex::new(None);
static BUSY: AtomicUsize = AtomicUsize::new(0);
static IDLE_MINUTES: AtomicU32 = AtomicU32::new(0);
static IDLE_THREAD: Once = Once::new();

fn lock_last_activity() -> MutexGuard<'static, Option<Instant>> {
    match LAST_ACTIVITY.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn stamp_activity() {
    *lock_last_activity() = Some(Instant::now());
}

pub fn request_started() {
    BUSY.fetch_add(1, Ordering::SeqCst);
    stamp_activity();
}

pub fn request_finished() {
    BUSY.fetch_sub(1, Ordering::SeqCst);
    stamp_activity();
}

fn should_free(idle_for: Duration, idle_minutes: u32, busy: usize, running: bool) -> bool {
    if idle_minutes == 0 || busy > 0 || !running {
        return false;
    }
    idle_for >= Duration::from_secs(idle_minutes as u64 * 60)
}

fn start_idle_thread() {
    IDLE_THREAD.call_once(|| {
        let spawn = std::thread::Builder::new()
            .name("writely-local-idle".to_string())
            .spawn(|| loop {
                std::thread::sleep(Duration::from_secs(30));
                let minutes = IDLE_MINUTES.load(Ordering::SeqCst);
                let idle_for = lock_last_activity()
                    .map(|activity| activity.elapsed())
                    .unwrap_or(Duration::ZERO);
                if should_free(
                    idle_for,
                    minutes,
                    BUSY.load(Ordering::SeqCst),
                    server::is_running(),
                ) {
                    let unit = if minutes == 1 { "minute" } else { "minutes" };
                    server::stop_if(&format!("not used for {minutes} {unit}"), &|| {
                        let idle_for = lock_last_activity()
                            .map(|activity| activity.elapsed())
                            .unwrap_or(Duration::ZERO);
                        should_free(idle_for, minutes, BUSY.load(Ordering::SeqCst), true)
                    });
                }
            });
        if let Err(e) = spawn {
            eprintln!("Writely: could not start the local model idle thread: {e}");
        }
    });
}

/// Loads the helper and makes one tiny request so the first real check is
/// fast. A length cut-off on the 1-token request still means the model
/// loaded and answered, so it counts as success here.
fn warm_up(settings: &AppSettings) {
    let result = server::ensure_running(settings).and_then(|_| {
        match local::complete(settings, "OK", "Reply with OK.", 1) {
            Ok(_) => Ok(()),
            Err(e) if e == local::length_cutoff_message(1) => Ok(()),
            Err(e) => Err(e),
        }
    });
    if let Err(e) = result {
        eprintln!("Writely: could not load the local model: {e}");
        settings::set_provider_message(Some(format!("Could not load the local model: {e}")));
    }
}

pub fn app_started() {
    let spawn = std::thread::Builder::new()
        .name("writely-local-start".to_string())
        .spawn(|| {
            server::cleanup_stale_helper();
            let settings = match config::load_settings() {
                Ok(settings) => settings,
                Err(e) => {
                    eprintln!("Writely: {e}");
                    return;
                }
            };
            IDLE_MINUTES.store(settings.local_idle_minutes, Ordering::SeqCst);
            start_idle_thread();
            if settings.provider == Provider::Local
                && settings.local_warm_up
                && install::local_ready(&settings)
            {
                warm_up(&settings);
            }
        });
    if let Err(e) = spawn {
        eprintln!("Writely: could not start the local model startup thread: {e}");
    }
}

pub fn settings_changed() {
    let spawn = std::thread::Builder::new()
        .name("writely-local-settings".to_string())
        .spawn(|| {
            let settings = match config::load_settings() {
                Ok(settings) => settings,
                Err(e) => {
                    eprintln!("Writely: {e}");
                    return;
                }
            };
            IDLE_MINUTES.store(settings.local_idle_minutes, Ordering::SeqCst);
            start_idle_thread();
            if settings.provider != Provider::Local {
                if server::is_running() {
                    server::stop("switched to another provider");
                }
            } else if settings.local_warm_up && install::local_ready(&settings) {
                warm_up(&settings);
            }
        });
    if let Err(e) = spawn {
        eprintln!("Writely: could not start the local model settings thread: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::should_free;
    use std::time::Duration;

    #[test]
    fn should_free_never_when_minutes_zero_busy_or_not_running() {
        let long = Duration::from_secs(60 * 60);
        assert!(!should_free(long, 0, 0, true));
        assert!(!should_free(long, 10, 1, true));
        assert!(!should_free(long, 10, 0, false));
    }

    #[test]
    fn should_free_at_or_after_the_limit() {
        assert!(!should_free(Duration::from_secs(10 * 60 - 1), 10, 0, true));
        assert!(should_free(Duration::from_secs(10 * 60), 10, 0, true));
        assert!(should_free(Duration::from_secs(10 * 60 + 1), 10, 0, true));
        assert!(should_free(Duration::from_secs(60), 1, 0, true));
    }
}
