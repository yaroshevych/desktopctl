// Console-subsystem launcher so PowerShell waits for it and captures output.
// The real CLI is spawned with CREATE_NO_WINDOW to suppress the console window
// that Windows would otherwise allocate (since the parent has no visible console
// under SSH). Stdio::inherit() passes the parent's handles, so output flows back.

#[cfg(windows)]
fn main() {
    use std::{env, os::windows::process::CommandExt, process::Command};
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    let exe = env::current_exe().expect("current_exe failed");
    let dir = exe.parent().expect("launcher has no parent dir");
    let real = dir.join("desktopctl-cli.exe");

    let mut child = Command::new(real)
        .args(env::args_os().skip(1))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .expect("failed to spawn desktopctl-cli.exe");

    let status = child.wait().expect("failed to wait for child");
    std::process::exit(status.code().unwrap_or(1));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("desktopctl launcher is Windows-only");
    std::process::exit(1);
}
