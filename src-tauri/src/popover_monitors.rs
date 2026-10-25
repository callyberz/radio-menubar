//! Native event monitors that make the popover behave like a status-item menu.
//!
//! * A *global* mouse-down monitor closes the popover when the user clicks anywhere outside
//!   it: the desktop, another app, or the menu bar / other status items. Global monitors only
//!   see events delivered to *other* applications, so clicks inside the popover and on our own
//!   status item are not reported (the tray click handler toggles the popover instead).
//!   Mouse-down global monitors need no Accessibility permission.
//! * A *local* key-down monitor handles Escape (close) and Cmd-Q (quit) natively, whether or
//!   not the web view currently has keyboard focus, plus ⌥⌘L (open the Logs window).
//!
//! Both are installed when the popover is shown and removed when it is hidden.

use std::cell::RefCell;
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSEvent, NSEventMask, NSEventModifierFlags};
use tauri::AppHandle;

const KEY_ESCAPE: u16 = 53;
const KEY_Q: u16 = 12;
const KEY_L: u16 = 37;

thread_local! {
    // AppKit objects stay on the main thread.
    static MONITORS: RefCell<Vec<Retained<AnyObject>>> = const { RefCell::new(Vec::new()) };
}

pub fn install(app: &AppHandle) {
    let app = app.clone();
    let _ = app.clone().run_on_main_thread(move || {
        remove_now();

        let mouse_app = app.clone();
        let on_mouse = RcBlock::new(move |_event: NonNull<NSEvent>| {
            // Don't remove the monitor from inside its own callback; hop to the next main-loop turn.
            let a = mouse_app.clone();
            let _ = mouse_app.run_on_main_thread(move || crate::hide_popover_window(&a, "outside click"));
        });
        let mouse_mask = NSEventMask(
            NSEventMask::LeftMouseDown.0 | NSEventMask::RightMouseDown.0 | NSEventMask::OtherMouseDown.0,
        );

        let key_app = app.clone();
        let on_key = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit passes a valid event for the duration of the callback.
            let ev = unsafe { event.as_ref() };
            let flags = ev.modifierFlags().0;
            let cmd = flags & NSEventModifierFlags::Command.0 != 0;
            let opt = flags & NSEventModifierFlags::Option.0 != 0;
            let a = key_app.clone();
            match ev.keyCode() {
                KEY_ESCAPE => {
                    let _ = key_app.run_on_main_thread(move || crate::hide_popover_window(&a, "escape"));
                    std::ptr::null_mut() // consumed
                }
                KEY_L if cmd && opt => {
                    let _ = key_app.run_on_main_thread(move || crate::open_logs_window(&a));
                    std::ptr::null_mut()
                }
                KEY_Q if cmd => {
                    let _ = key_app.run_on_main_thread(move || a.exit(0));
                    std::ptr::null_mut()
                }
                _ => event.as_ptr(),
            }
        });

        let mut installed = Vec::new();
        match NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mouse_mask, &on_mouse) {
            Some(m) => installed.push(m),
            None => crate::diag::warn("window", "outside-click monitor could not be installed"),
        }
        // SAFETY: the block returns the event it was given, or null.
        match unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &on_key) } {
            Some(m) => installed.push(m),
            None => crate::diag::warn("window", "key monitor could not be installed"),
        }
        MONITORS.with(|m| *m.borrow_mut() = installed);
    });
}

pub fn remove(app: &AppHandle) {
    let _ = app.run_on_main_thread(remove_now);
}

fn remove_now() {
    MONITORS.with(|m| {
        for monitor in m.borrow_mut().drain(..) {
            // SAFETY: each object was returned by NSEvent's add…Monitor methods.
            unsafe { NSEvent::removeMonitor(&monitor) };
        }
    });
}
