// Console-subsystem launcher: PowerShell waits for it and captures its output.
// Under SSH, FreeConsole() detaches before the console window is ever painted,
// then the real CLI is spawned with CREATE_NO_WINDOW so no window appears.

#[cfg(windows)]
fn main() {
    use std::{
        env, os::windows::io::FromRawHandle, os::windows::process::CommandExt, process::Command,
    };
    use windows_sys::Win32::Foundation::{
        DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Console::{
        FreeConsole, GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, GetCurrentProcess};

    let under_ssh = ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        .iter()
        .any(|k| std::env::var_os(k).is_some());

    let exe = env::current_exe().expect("current_exe failed");
    let dir = exe.parent().expect("launcher has no parent dir");
    let real = dir.join("desktopctl-cli.exe");

    if under_ssh {
        // Grab stdio handles before detaching from the console.
        let stdin = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let stdout = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        let stderr = unsafe { GetStdHandle(STD_ERROR_HANDLE) };

        // Detach before the console window is painted.
        unsafe { FreeConsole() };

        fn dup_stdio(h: HANDLE) -> std::process::Stdio {
            if h.is_null() || h == INVALID_HANDLE_VALUE {
                return std::process::Stdio::null();
            }
            let proc = unsafe { GetCurrentProcess() };
            let mut duped: HANDLE = std::ptr::null_mut();
            let ok = unsafe {
                DuplicateHandle(proc, h, proc, &mut duped, 0, 1, DUPLICATE_SAME_ACCESS)
            };
            if ok == 0 || duped.is_null() || duped == INVALID_HANDLE_VALUE {
                return std::process::Stdio::null();
            }
            unsafe { std::process::Stdio::from_raw_handle(duped as _) }
        }

        let mut child = Command::new(real)
            .args(env::args_os().skip(1))
            .stdin(dup_stdio(stdin))
            .stdout(dup_stdio(stdout))
            .stderr(dup_stdio(stderr))
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("failed to spawn desktopctl-cli.exe");

        let status = child.wait().expect("failed to wait for child");
        std::process::exit(status.code().unwrap_or(1));
    } else {
        let mut child = Command::new(real)
            .args(env::args_os().skip(1))
            .spawn()
            .expect("failed to spawn desktopctl-cli.exe");

        let status = child.wait().expect("failed to wait for child");
        std::process::exit(status.code().unwrap_or(1));
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("desktopctl launcher is Windows-only");
    std::process::exit(1);
}
