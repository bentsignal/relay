//! AppKit main-thread event loop for viewer windows.
//!
//! Design: AppKit windows must be created and driven on the process's main
//! thread, but the engine is headless and opens viewers from background
//! threads. Rather than a helper process (extra IPC hop, a second binary to
//! sign and ship, and more latency), the engine calls [`run_app`] from `main`:
//! its own work (the control loop) moves to a secondary thread and the main
//! thread runs `NSApplication` with the *accessory* activation policy, so
//! there is no Dock icon or menu bar until a viewer window opens. Viewer code
//! dispatches window work to the main queue; decoded-frame submission stays on
//! the caller's thread (AVSampleBufferVideoRenderer is thread-safe), so frames
//! never wait on the UI thread.

use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventModifierFlags, NSEventType};
use objc2_foundation::NSPoint;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static LOOP_RUNNING: AtomicBool = AtomicBool::new(false);
static OPEN_WINDOWS: AtomicUsize = AtomicUsize::new(0);

/// True while [`run_app`] is servicing the main thread.
pub(crate) fn loop_running() -> bool {
    LOOP_RUNNING.load(Ordering::Acquire)
}

/// Runs `f` on a new thread while the calling (main) thread services AppKit,
/// so video viewer windows can be opened from any thread. Returns `f`'s
/// result once it finishes. Called from anywhere but the main thread, it just
/// runs `f` there (viewers then fail to open).
pub fn run_app<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    let Some(mtm) = MainThreadMarker::new() else { return f() };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    struct StopOnExit;
    impl Drop for StopOnExit {
        fn drop(&mut self) {
            DispatchQueue::main().exec_async(stop_loop);
        }
    }
    let (tx, rx) = crossbeam_channel::bounded::<R>(1);
    std::thread::Builder::new()
        .name("relay-main".into())
        .spawn(move || {
            let _stop = StopOnExit;
            let _ = tx.send(f());
        })
        .expect("spawn main work thread");
    LOOP_RUNNING.store(true, Ordering::Release);
    app.run();
    LOOP_RUNNING.store(false, Ordering::Release);
    match rx.recv() {
        Ok(r) => r,
        Err(_) => panic!("the main work thread panicked"),
    }
}

fn stop_loop() {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    app.stop(None);
    // -stop: takes effect after the next event, so post one.
    let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
        NSEventType::ApplicationDefined,
        NSPoint::ZERO,
        NSEventModifierFlags::empty(),
        0.0,
        0,
        None,
        0,
        0,
        0,
    );
    if let Some(event) = event {
        app.postEvent_atStart(&event, true);
    }
}

/// A viewer window opened: show Relay in the Dock and bring it forward.
pub(crate) fn window_opened(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    if OPEN_WINDOWS.fetch_add(1, Ordering::Relaxed) == 0 {
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    }
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
}

/// The last viewer closed: back to a background process.
pub(crate) fn window_closed(mtm: MainThreadMarker) {
    if OPEN_WINDOWS.fetch_sub(1, Ordering::Relaxed) == 1 {
        NSApplication::sharedApplication(mtm).setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    }
}
