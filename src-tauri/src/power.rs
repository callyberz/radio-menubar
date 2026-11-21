//! Screen-lock and system-sleep tracking.
//!
//! Since macOS 27, WebKit interrupts the page's Media Session on system sleep and, on wake,
//! "resumes" it by calling the page's `play` action handler — even when the user had paused,
//! because WebKit's internal session state is still `Playing`. The page can't tell that call
//! apart from a real media-key press, so we tell it when the Mac is locked / asleep and it
//! ignores `play` commands that arrive while away or right after coming back.
//!
//! Emits `system-away` (bool): `true` when the screen locks or the system is about to sleep,
//! `false` once it is both unlocked and awake again.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};

use block2::RcBlock;
use objc2_app_kit::{NSWorkspace, NSWorkspaceDidWakeNotification, NSWorkspaceWillSleepNotification};
use objc2_foundation::{NSDistributedNotificationCenter, NSNotification, NSNotificationCenter, NSNotificationName, NSString};
use tauri::{AppHandle, Emitter};

static LOCKED: AtomicBool = AtomicBool::new(false);
static ASLEEP: AtomicBool = AtomicBool::new(false);
static AWAY: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
enum Change {
    Locked(bool),
    Asleep(bool),
}

fn apply(app: &AppHandle, change: Change) {
    let what = match change {
        Change::Locked(v) => {
            LOCKED.store(v, Ordering::SeqCst);
            if v { "screen locked" } else { "screen unlocked" }
        }
        Change::Asleep(v) => {
            ASLEEP.store(v, Ordering::SeqCst);
            if v { "system will sleep" } else { "system woke" }
        }
    };
    crate::diag::info("power", what);
    let away = LOCKED.load(Ordering::SeqCst) || ASLEEP.load(Ordering::SeqCst);
    if AWAY.swap(away, Ordering::SeqCst) != away {
        let _ = app.emit("system-away", away);
    }
}

fn observe(app: &AppHandle, center: &NSNotificationCenter, name: &NSNotificationName, change: Change) {
    let app = app.clone();
    let block = RcBlock::new(move |_n: NonNull<NSNotification>| apply(&app, change));
    // SAFETY: no sender filter; the block only touches thread-safe state (atomics, event emit).
    let token = unsafe { center.addObserverForName_object_queue_usingBlock(Some(name), None, None, &block) };
    // Observers live for the whole app run.
    std::mem::forget(token);
}

/// Call once, on the main thread.
pub fn install(app: &AppHandle) {
    let dnc = NSDistributedNotificationCenter::defaultCenter();
    observe(app, &dnc, &NSString::from_str("com.apple.screenIsLocked"), Change::Locked(true));
    observe(app, &dnc, &NSString::from_str("com.apple.screenIsUnlocked"), Change::Locked(false));

    let wnc = NSWorkspace::sharedWorkspace().notificationCenter();
    // SAFETY: AppKit-provided constant notification names.
    let (will_sleep, did_wake) = unsafe { (NSWorkspaceWillSleepNotification, NSWorkspaceDidWakeNotification) };
    observe(app, &wnc, will_sleep, Change::Asleep(true));
    observe(app, &wnc, did_wake, Change::Asleep(false));
}
