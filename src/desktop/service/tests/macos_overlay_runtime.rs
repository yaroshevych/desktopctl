#![cfg(target_os = "macos")]

use desktop_core::{
    ipc::{read_framed_json, write_framed_json},
    protocol::{Command, RequestEnvelope, ResponseEnvelope},
};
use std::{
    fs,
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command as ProcessCommand},
    thread,
    time::{Duration, Instant},
};

struct Daemon {
    child: Child,
    root: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
#[ignore = "requires a logged-in macOS GUI session; briefly displays the overlay"]
fn overlay_dispatch_and_on_demand_exit() {
    // Keep the socket path below macOS sockaddr_un's small path limit.
    let root = PathBuf::from("/tmp").join(format!("dc-overlay-{}", uuid::Uuid::now_v7()));
    fs::create_dir_all(root.join("data/workspaces")).unwrap();
    // Prevent migration of the user's journal/policy settings into the fixture.
    fs::write(
        root.join("data/config.toml"),
        "[journal]\nenabled = false\n",
    )
    .unwrap();
    fs::write(
        root.join("data/workspaces/agent-sessions.json"),
        "{\"version\":1,\"sessions\":[]}",
    )
    .unwrap();
    let socket = root.join("ipc.sock");
    let child = ProcessCommand::new(env!("CARGO_BIN_EXE_desktopctld"))
        .arg("--on-demand")
        .env("DESKTOPCTL_HOME", root.join("data"))
        .env("DESKTOPCTL_SOCKET_PATH", &socket)
        .env("DESKTOPCTL_TRACE_PATH", root.join("trace.log"))
        .spawn()
        .unwrap();
    let mut daemon = Daemon { child, root };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(
            daemon.child.try_wait().unwrap().is_none(),
            "daemon exited before binding"
        );
        assert!(Instant::now() < deadline, "daemon never bound its socket");
        thread::sleep(Duration::from_millis(20));
    }
    for command in [
        Command::OverlayStart {
            duration_ms: Some(30_000),
        },
        Command::OverlayStop,
        Command::OverlayStart {
            duration_ms: Some(30_000),
        },
        Command::OverlayStop,
    ] {
        let started = Instant::now();
        let mut stream = UnixStream::connect(&socket).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let name = command.name().to_owned();
        write_framed_json(
            &mut stream,
            &RequestEnvelope::new("overlay-runtime-test".into(), command),
        )
        .unwrap();
        let response: ResponseEnvelope = read_framed_json(&mut stream).unwrap();
        assert!(
            matches!(response, ResponseEnvelope::Success(_)),
            "{name}: {response:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{name} stalled: {:?}",
            started.elapsed()
        );
        eprintln!("{name}: {}ms", started.elapsed().as_millis());
    }
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if let Some(status) = daemon.child.try_wait().unwrap() {
            assert!(status.success(), "daemon failed on idle shutdown");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "main event loop failed to stop after idle timeout"
        );
        thread::sleep(Duration::from_millis(25));
    }
    let trace = fs::read_to_string(daemon.root.join("trace.log")).unwrap();
    assert_eq!(trace.matches("overlay:start ok").count(), 2);
    assert!(!trace.contains("timed out waiting for main-thread overlay"));
}
