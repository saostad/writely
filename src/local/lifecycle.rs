use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

static LAST_ACTIVITY: Mutex<Option<Instant>> = Mutex::new(None);
static BUSY: AtomicUsize = AtomicUsize::new(0);

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
