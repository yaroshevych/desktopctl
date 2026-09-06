use desktop_core::error::AppError;
use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventModifierFlags, NSEventType,
};
use objc2_foundation::NSPoint;

/// AppKit must service its main queue while the socket listener blocks on a
/// worker. Overlay operations dispatch to this queue and wait for completion.
pub(crate) fn run(config: crate::daemon::DaemonConfig) -> Result<(), AppError> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| AppError::backend_unavailable("daemon UI must run on main thread"))?;
    let app = NSApplication::sharedApplication(mtm);
    let _ = app.setActivationPolicy(NSApplicationActivationPolicy::Prohibited);
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new().name("daemon-listener".into()).spawn(move || {
        let result = std::panic::catch_unwind(|| crate::daemon::run_blocking(config))
            .unwrap_or_else(|_| Err(AppError::internal("daemon listener panicked")));
        let _ = tx.send(result);
        DispatchQueue::main().exec_async(|| {
            let _ = crate::overlay::stop_overlay();
            let mtm = MainThreadMarker::new().expect("main dispatch queue");
            let app = NSApplication::sharedApplication(mtm);
            app.stop(None);
            // stop() takes effect after the current event. Wake an otherwise
            // idle event loop so bind failures and on-demand exits return too.
            if let Some(event) = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
                NSEventType::ApplicationDefined, NSPoint::new(0.0, 0.0),
                NSEventModifierFlags::empty(), 0.0, 0, None, 0, 0, 0,
            ) {
                app.postEvent_atStart(&event, true);
            }
        });
    }).map_err(|error| AppError::backend_unavailable(format!("start daemon listener: {error}")))?;
    crate::trace::log("daemon:main_event_loop_start");
    app.run();
    rx.recv()
        .map_err(|_| AppError::internal("daemon listener exited without a result"))?
}
