//! macOS self-test for the viewer window's input handling. Synthetic events
//! go to the viewer's own window only (nothing is injected system-wide):
//! double-click -> full screen, Esc -> windowed, close -> `push` returns false.
//!
//!   cargo run -p relay-video --example viewer_input

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType, NSWindowStyleMask};
    use objc2_foundation::{NSPoint, NSString};
    use std::time::Duration;

    const TITLE: &str = "Relay viewer input test";

    /// Runs `f` with the viewer's NSWindow on the main thread and returns its result.
    fn on_window<R: Send + 'static>(f: impl FnOnce(&objc2_app_kit::NSWindow) -> R + Send + 'static) -> Option<R> {
        let (tx, rx) = crossbeam_channel::bounded(1);
        dispatch2::DispatchQueue::main().exec_async(move || {
            let mtm = MainThreadMarker::new().unwrap();
            let app = NSApplication::sharedApplication(mtm);
            let win = app.windows().iter().find(|w| w.title().to_string() == TITLE);
            let _ = tx.send(win.map(|w| f(&w)));
        });
        rx.recv_timeout(Duration::from_secs(5)).ok().flatten()
    }
    fn full_screen() -> Option<bool> {
        on_window(|w| w.styleMask().contains(NSWindowStyleMask::FullScreen))
    }

    tracing_subscriber::fmt().with_env_filter("info").init();
    relay_video::run_app(|| -> anyhow::Result<()> {
        let viewer = std::sync::Arc::new(relay_video::viewer::open(TITLE)?);
        let v2 = viewer.clone();
        let cap = relay_video::start(
            &relay_video::sources().first().map(|s| s.key.clone()).unwrap_or_default(),
            relay_video::Settings { max_width: 1280, max_height: 720, ..Default::default() },
            Box::new(move |f| {
                v2.push(f);
            }),
        )?;
        std::thread::sleep(Duration::from_secs(2));
        println!("windowed at start: full_screen={:?}", full_screen());

        on_window(|w| {
            let f = w.contentView().unwrap().frame();
            let at = NSPoint::new(f.size.width / 2.0, f.size.height / 2.0);
            for (kind, clicks) in [
                (NSEventType::LeftMouseDown, 1),
                (NSEventType::LeftMouseUp, 1),
                (NSEventType::LeftMouseDown, 2),
                (NSEventType::LeftMouseUp, 2),
            ] {
                let ev = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
                    kind, at, NSEventModifierFlags::empty(), 0.0, w.windowNumber(), None, 0, clicks, 1.0,
                );
                if let Some(ev) = ev {
                    w.sendEvent(&ev);
                }
            }
        });
        std::thread::sleep(Duration::from_secs(3));
        println!("after double-click: full_screen={:?}", full_screen());

        on_window(|w| {
            let esc = NSString::from_str("\u{1b}");
            let ev = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                NSEventType::KeyDown, NSPoint::ZERO, NSEventModifierFlags::empty(), 0.0, w.windowNumber(), None, &esc, &esc, false, 53,
            );
            if let Some(ev) = ev {
                w.sendEvent(&ev);
            }
        });
        std::thread::sleep(Duration::from_secs(3));
        println!("after Esc: full_screen={:?}", full_screen());

        on_window(|w| w.performClose(None));
        std::thread::sleep(Duration::from_millis(500));
        let pushed = viewer.push(relay_proto::EncodedVideo::default());
        println!("after close: is_open={} push returned {pushed}", viewer.is_open());
        drop(cap);
        Ok(())
    })
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("this self-test is macOS-only");
}
