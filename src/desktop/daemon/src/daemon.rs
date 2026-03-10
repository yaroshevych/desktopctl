use std::{
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    os::unix::{fs::PermissionsExt, net::UnixListener},
    process::Command as ProcessCommand,
    thread,
};

use desktop_core::{
    automation::{Point, new_backend},
    error::AppError,
    ipc::socket_path,
    protocol::{Command, Request, Response},
};

pub fn start() -> Result<(), AppError> {
    let socket_path = socket_path();
    if socket_path.exists() {
        let _ = fs::remove_file(&socket_path);
    }

    let listener = UnixListener::bind(&socket_path)
        .map_err(|err| AppError::Ipc(format!("bind {} failed: {err}", socket_path.display())))?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))
        .map_err(|err| AppError::Ipc(format!("set socket permissions failed: {err}")))?;

    thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    thread::spawn(|| {
                        if let Err(err) = handle_client(stream) {
                            eprintln!("daemon error: {err}");
                        }
                    });
                }
                Err(err) => eprintln!("daemon accept error: {err}"),
            }
        }
    });

    Ok(())
}

fn handle_client(mut stream: UnixStream) -> Result<(), AppError> {
    let mut payload = Vec::new();
    stream
        .read_to_end(&mut payload)
        .map_err(|err| AppError::Ipc(format!("read request failed: {err}")))?;

    let request: Request = serde_json::from_slice(&payload)
        .map_err(|err| AppError::Ipc(format!("invalid request: {err}")))?;
    let response = match execute(request.command) {
        Ok(msg) => Response::ok(msg),
        Err(err) => Response::err(err.to_string()),
    };

    let encoded = serde_json::to_vec(&response)
        .map_err(|err| AppError::Ipc(format!("encode response failed: {err}")))?;
    stream
        .write_all(&encoded)
        .map_err(|err| AppError::Ipc(format!("write response failed: {err}")))?;
    Ok(())
}

fn execute(command: Command) -> Result<Option<String>, AppError> {
    match command {
        Command::Ping => Ok(Some("pong".to_string())),
        Command::OpenApp { name, args } => {
            let mut cmd = ProcessCommand::new("open");
            cmd.arg("-a").arg(&name);
            if !args.is_empty() {
                cmd.args(&args);
            }

            let output = cmd
                .output()
                .map_err(|err| AppError::AutomationCommand(err.to_string()))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                return Err(AppError::AutomationCommand(stderr));
            }

            let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
            let script = format!(r#"tell application "{escaped}" to activate"#);
            let activate = ProcessCommand::new("osascript")
                .arg("-e")
                .arg(script)
                .output()
                .map_err(|err| AppError::AutomationCommand(err.to_string()))?;
            if !activate.status.success() {
                let stderr = String::from_utf8_lossy(&activate.stderr).trim().to_string();
                return Err(AppError::AutomationCommand(stderr));
            }
            Ok(None)
        }
        Command::OpenSpotlight => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            backend.press_hotkey("cmd+space")?;
            Ok(None)
        }
        Command::OpenLaunchpad => {
            let output = ProcessCommand::new("open")
                .args(["-a", "Launchpad"])
                .output()
                .map_err(|err| AppError::AutomationCommand(err.to_string()))?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                return Err(AppError::AutomationCommand(stderr));
            }
            Ok(None)
        }
        Command::PointerMove { x, y } => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            backend.move_mouse(Point::new(x, y))?;
            Ok(None)
        }
        Command::PointerDown { x, y } => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            let point = Point::new(x, y);
            backend.move_mouse(point)?;
            backend.left_down(point)?;
            Ok(None)
        }
        Command::PointerUp { x, y } => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            let point = Point::new(x, y);
            backend.move_mouse(point)?;
            backend.left_up(point)?;
            Ok(None)
        }
        Command::PointerClick { x, y } => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            let point = Point::new(x, y);
            backend.move_mouse(point)?;
            backend.left_click(point)?;
            Ok(None)
        }
        Command::PointerDrag {
            x1,
            y1,
            x2,
            y2,
            hold_ms,
        } => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            let start = Point::new(x1, y1);
            let end = Point::new(x2, y2);
            backend.move_mouse(start)?;
            backend.left_down(start)?;
            backend.sleep_ms(hold_ms.max(30));
            backend.left_drag(end)?;
            backend.left_up(end)?;
            Ok(None)
        }
        Command::UiType { text } => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            backend.type_text(&text)?;
            Ok(None)
        }
        Command::KeyHotkey { hotkey } => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            backend.press_hotkey(&hotkey)?;
            Ok(None)
        }
        Command::KeyEnter => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            backend.press_enter()?;
            Ok(None)
        }
        Command::Wait { ms } => {
            let backend = new_backend()?;
            backend.sleep_ms(ms);
            Ok(None)
        }
    }
}
