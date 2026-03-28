#![cfg(target_os = "macos")]

use serde_json::Value;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Mutex, OnceLock},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static SMOKE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(25);

fn smoke_lock() -> &'static Mutex<()> {
    SMOKE_LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Debug)]
struct CmdOutcome {
    status: ExitStatus,
    stdout: String,
    stderr: String,
    json: Option<Value>,
}

struct SmokeCli {
    bin: PathBuf,
    artifact_dir: PathBuf,
    label: String,
}

impl SmokeCli {
    fn new(label: &str) -> Result<Option<Self>, String> {
        if env::var("DESKTOPCTL_SMOKE").is_err() {
            eprintln!("skipping smoke test {label}: set DESKTOPCTL_SMOKE=1 to enable");
            return Ok(None);
        }

        let bin = resolve_smoke_bin()?;
        let ts = now_millis();
        let artifact_dir = env::temp_dir().join(format!("desktopctl-smoke-{label}-{ts}"));
        fs::create_dir_all(&artifact_dir).map_err(|e| {
            format!(
                "failed to create artifact dir {}: {e}",
                artifact_dir.display()
            )
        })?;

        Ok(Some(Self {
            bin,
            artifact_dir,
            label: label.to_string(),
        }))
    }

    fn open_app(&self, app: &str, timeout_ms: u64) -> Result<Value, String> {
        self.run_json_ok(
            &[
                "app",
                "open",
                app,
                "--wait",
                "--timeout",
                &timeout_ms.to_string(),
            ],
            DEFAULT_TIMEOUT,
            "app_open",
        )
    }

    fn run_json_ok(&self, args: &[&str], timeout: Duration, step: &str) -> Result<Value, String> {
        let outcome = self.run_json(args, timeout)?;
        if !outcome.status.success() {
            let mut msg = format!(
                "command failed (step={step}): {} {}\nstatus={}\nstdout={}\nstderr={}",
                self.bin.display(),
                args.join(" "),
                outcome.status,
                outcome.stdout,
                outcome.stderr
            );
            if let Some(request_id) = outcome
                .json
                .as_ref()
                .and_then(|json| json.get("request_id"))
                .and_then(Value::as_str)
            {
                self.capture_failure_artifacts(request_id, step);
                msg.push_str(&format!("\nrequest_id={request_id}"));
            }
            return Err(msg);
        }

        let json = outcome
            .json
            .ok_or_else(|| format!("missing JSON output for step={step}: {}", outcome.stdout))?;
        let ok = json.get("ok").and_then(Value::as_bool).unwrap_or(false);
        if !ok {
            let request_id = json
                .get("request_id")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            self.capture_failure_artifacts(request_id, step);
            return Err(format!(
                "command returned ok=false (step={step}, request_id={request_id}): {}",
                pretty_json(&json)
            ));
        }

        Ok(json)
    }

    fn run_json(&self, args: &[&str], timeout: Duration) -> Result<CmdOutcome, String> {
        let mut full: Vec<String> = Vec::with_capacity(args.len() + 1);
        full.push("--json".to_string());
        full.extend(args.iter().map(|a| (*a).to_string()));

        let mut cmd = Command::new(&self.bin);
        cmd.args(&full);
        run_with_timeout(&mut cmd, timeout)
    }

    fn capture_failure_artifacts(&self, request_id: &str, step: &str) {
        if request_id.trim().is_empty() {
            return;
        }

        let show = self.run_json(&["request", "show", request_id], Duration::from_secs(6));
        match show {
            Ok(out) => {
                if let Some(json) = out.json {
                    eprintln!(
                        "[smoke:{step}] request show {request_id}: {}",
                        pretty_json(&json)
                    );
                } else {
                    eprintln!(
                        "[smoke:{step}] request show {request_id}: stdout={} stderr={}",
                        out.stdout, out.stderr
                    );
                }
            }
            Err(err) => eprintln!("[smoke:{step}] request show {request_id} failed: {err}"),
        }

        let out_path = self
            .artifact_dir
            .join(format!("{}-{}-{}.png", self.label, step, request_id));
        let path_text = out_path.to_string_lossy().to_string();
        let shot = self.run_json(
            &["request", "screenshot", request_id, "--out", &path_text],
            Duration::from_secs(6),
        );
        match shot {
            Ok(out) => {
                if out.status.success() && out_path.exists() {
                    eprintln!(
                        "[smoke:{step}] request screenshot {request_id}: {}",
                        out_path.display()
                    );
                } else {
                    eprintln!(
                        "[smoke:{step}] request screenshot {request_id} failed: stdout={} stderr={}",
                        out.stdout, out.stderr
                    );
                }
            }
            Err(err) => {
                eprintln!("[smoke:{step}] request screenshot {request_id} invocation failed: {err}")
            }
        }
    }

    fn best_effort_json(&self, args: &[&str], timeout: Duration) {
        let _ = self.run_json(args, timeout);
    }

    fn cleanup_overlay_and_apps(&self) {
        self.best_effort_json(&["debug", "overlay", "stop"], Duration::from_secs(4));
        for app in ["Calculator", "System Settings"] {
            self.best_effort_json(&["app", "hide", app], Duration::from_secs(4));
        }
    }

    fn begin_session(&self) -> SmokeSession<'_> {
        self.cleanup_overlay_and_apps();
        SmokeSession { cli: self }
    }
}

struct SmokeSession<'a> {
    cli: &'a SmokeCli,
}

impl Drop for SmokeSession<'_> {
    fn drop(&mut self) {
        self.cli.cleanup_overlay_and_apps();
    }
}

fn resolve_smoke_bin() -> Result<PathBuf, String> {
    if let Ok(path) = env::var("DESKTOPCTL_SMOKE_BIN") {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
        return Err(format!(
            "DESKTOPCTL_SMOKE_BIN points to missing path: {}",
            p.display()
        ));
    }

    if let Ok(path) = env::var("CARGO_BIN_EXE_desktopctl") {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
    }

    let fallback = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../dist/desktopctl");
    if fallback.exists() {
        return Ok(fallback);
    }

    Err(format!(
        "desktopctl binary not found; set DESKTOPCTL_SMOKE_BIN or build {}",
        fallback.display()
    ))
}

fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<CmdOutcome, String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn command: {e}"))?;

    let start = Instant::now();
    wait_for_exit(&mut child, timeout, start)?;

    let output = child
        .wait_with_output()
        .map_err(|e| format!("failed to read command output: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let json = serde_json::from_str::<Value>(&stdout).ok();

    Ok(CmdOutcome {
        status: output.status,
        stdout,
        stderr,
        json,
    })
}

fn wait_for_exit(child: &mut Child, timeout: Duration, start: Instant) -> Result<(), String> {
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("command timed out after {:?}", timeout));
                }
                thread::sleep(Duration::from_millis(25));
            }
            Err(err) => return Err(format!("failed polling child process: {err}")),
        }
    }
}

fn response_result<'a>(response: &'a Value, step: &str) -> Result<&'a Value, String> {
    if !response.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return Err(format!(
            "expected ok=true for step={step}, got: {}",
            pretty_json(response)
        ));
    }
    response.get("result").ok_or_else(|| {
        format!(
            "missing result in response for step={step}: {}",
            pretty_json(response)
        )
    })
}

fn request_id(response: &Value) -> Result<&str, String> {
    response
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("missing request_id in response: {}", pretty_json(response)))
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn retry_json<F>(attempts: usize, delay: Duration, mut op: F) -> Result<Value, String>
where
    F: FnMut() -> Result<Value, String>,
{
    let mut last_err = String::new();
    for _ in 0..attempts {
        match op() {
            Ok(v) => return Ok(v),
            Err(err) => {
                last_err = err;
                thread::sleep(delay);
            }
        }
    }
    Err(last_err)
}

fn png_dimensions(path: &Path) -> Result<(u32, u32), String> {
    let data = fs::read(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    if data.len() < 24 {
        return Err(format!("png too small: {}", path.display()));
    }
    let signature = &data[0..8];
    if signature != [137, 80, 78, 71, 13, 10, 26, 10] {
        return Err(format!("not a png file: {}", path.display()));
    }
    let ihdr = &data[12..16];
    if ihdr != b"IHDR" {
        return Err(format!("missing IHDR chunk: {}", path.display()));
    }
    let width = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
    let height = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
    Ok((width, height))
}

fn active_window_elements(tokenize_response: &Value) -> Result<Vec<Value>, String> {
    let result = response_result(tokenize_response, "tokenize")?;
    let windows = result
        .get("windows")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("tokenize result missing windows: {}", pretty_json(result)))?;
    let first = windows
        .first()
        .ok_or_else(|| format!("tokenize returned no windows: {}", pretty_json(result)))?;
    let elements = first
        .get("elements")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("tokenize window missing elements: {}", pretty_json(first)))?;
    Ok(elements.clone())
}

fn active_window(tokenize_response: &Value) -> Result<Value, String> {
    let result = response_result(tokenize_response, "tokenize")?;
    let windows = result
        .get("windows")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("tokenize result missing windows: {}", pretty_json(result)))?;
    windows
        .first()
        .cloned()
        .ok_or_else(|| format!("tokenize returned no windows: {}", pretty_json(result)))
}

fn text_signature(response: &Value) -> Result<Vec<String>, String> {
    let elements = active_window_elements(response)?;
    let mut texts: Vec<String> = elements
        .iter()
        .filter_map(|el| el.get("text").and_then(Value::as_str))
        .map(|s| s.trim().replace('\u{200e}', ""))
        .filter(|s| !s.is_empty())
        .collect();
    texts.sort();
    texts.dedup();
    Ok(texts)
}

fn click_and_wait_signature_change(
    cli: &SmokeCli,
    button_id: &str,
    previous_signature: &[String],
    click_step: &str,
    verify_step: &str,
) -> Result<(Value, Vec<String>), String> {
    let timeout = Duration::from_secs(4);
    let _ = cli.run_json_ok(
        &["pointer", "click", "--id", button_id],
        timeout,
        click_step,
    )?;
    let mut last_err = String::new();
    for _ in 0..3 {
        let response = cli.run_json_ok(
            &["screen", "tokenize", "--active-window"],
            timeout,
            verify_step,
        )?;
        let signature = element_signature(&response)?;
        if signature == previous_signature {
            last_err = "tokenize signature unchanged yet".to_string();
            thread::sleep(Duration::from_millis(60));
            continue;
        }
        return Ok((response, signature));
    }
    Err(last_err)
}

fn press_and_wait_signature_change(
    cli: &SmokeCli,
    key: &str,
    previous_signature: &[String],
    press_step: &str,
    verify_step: &str,
) -> Result<(Value, Vec<String>), String> {
    let timeout = Duration::from_secs(4);
    let _ = cli.run_json_ok(&["keyboard", "press", key], timeout, press_step)?;
    let mut last_err = String::new();
    for _ in 0..3 {
        let response = cli.run_json_ok(
            &["screen", "tokenize", "--active-window"],
            timeout,
            verify_step,
        )?;
        let signature = element_signature(&response)?;
        if signature == previous_signature {
            last_err = "tokenize signature unchanged yet".to_string();
            thread::sleep(Duration::from_millis(60));
            continue;
        }
        return Ok((response, signature));
    }
    Err(last_err)
}

fn element_signature(response: &Value) -> Result<Vec<String>, String> {
    let elements = active_window_elements(response)?;
    let mut sig: Vec<String> = elements
        .iter()
        .map(|el| {
            let id = el.get("id").and_then(Value::as_str).unwrap_or("");
            let text = el.get("text").and_then(Value::as_str).unwrap_or("");
            let bbox = el
                .get("bbox")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let bx = bbox.first().and_then(Value::as_f64).unwrap_or_default();
            let by = bbox.get(1).and_then(Value::as_f64).unwrap_or_default();
            let bw = bbox.get(2).and_then(Value::as_f64).unwrap_or_default();
            let bh = bbox.get(3).and_then(Value::as_f64).unwrap_or_default();
            format!(
                "{id}:{text}:{:.0}:{:.0}:{:.0}:{:.0}",
                bx.round(),
                by.round(),
                bw.round(),
                bh.round()
            )
        })
        .collect();
    sig.sort();
    sig.dedup();
    Ok(sig)
}

#[test]
fn smoke_screen_screenshot_active_window_region() -> Result<(), String> {
    let _guard = smoke_lock()
        .lock()
        .map_err(|_| "failed to acquire smoke lock".to_string())?;
    let Some(cli) = SmokeCli::new("screenshot_region")? else {
        return Ok(());
    };
    let _session = cli.begin_session();

    cli.open_app("Calculator", 12_000)?;
    thread::sleep(Duration::from_millis(500));

    let out_path = cli.artifact_dir.join("calculator-region.png");
    let out_text = out_path.to_string_lossy().to_string();

    let response = cli.run_json_ok(
        &[
            "screen",
            "screenshot",
            "--active-window",
            "--region",
            "0",
            "0",
            "120",
            "120",
            "--out",
            &out_text,
        ],
        DEFAULT_TIMEOUT,
        "screenshot_region",
    )?;

    let _ = request_id(&response)?;
    let result = response_result(&response, "screenshot_region")?;
    let window_bounds = result
        .get("window_bounds")
        .ok_or_else(|| format!("missing window_bounds: {}", pretty_json(result)))?;
    let logical_width = window_bounds
        .get("width")
        .and_then(Value::as_f64)
        .ok_or_else(|| {
            format!(
                "missing window_bounds.width: {}",
                pretty_json(window_bounds)
            )
        })?;
    let logical_height = window_bounds
        .get("height")
        .and_then(Value::as_f64)
        .ok_or_else(|| {
            format!(
                "missing window_bounds.height: {}",
                pretty_json(window_bounds)
            )
        })?;
    if (logical_width - 120.0).abs() > 0.01 || (logical_height - 120.0).abs() > 0.01 {
        return Err(format!(
            "unexpected window bounds for region capture: {}",
            pretty_json(window_bounds)
        ));
    }

    if !out_path.exists() {
        return Err(format!("screenshot file missing: {}", out_path.display()));
    }
    let (png_w, png_h) = png_dimensions(&out_path)?;
    let scale_w = png_w as f64 / logical_width.max(1.0);
    let scale_h = png_h as f64 / logical_height.max(1.0);
    if (scale_w - scale_h).abs() > 0.2 || scale_w < 0.9 {
        return Err(format!(
            "unexpected png scaling for region capture: logical={}x{}, png={}x{}, scale_w={:.2}, scale_h={:.2}",
            logical_width, logical_height, png_w, png_h, scale_w, scale_h
        ));
    }

    Ok(())
}

#[test]
fn smoke_screen_tokenize_active_window_region() -> Result<(), String> {
    let _guard = smoke_lock()
        .lock()
        .map_err(|_| "failed to acquire smoke lock".to_string())?;
    let Some(cli) = SmokeCli::new("tokenize_region")? else {
        return Ok(());
    };
    let _session = cli.begin_session();

    cli.open_app("Calculator", 12_000)?;
    thread::sleep(Duration::from_millis(500));

    let response = retry_json(6, Duration::from_millis(250), || {
        cli.run_json_ok(
            &[
                "screen",
                "tokenize",
                "--active-window",
                "--region",
                "0",
                "0",
                "160",
                "220",
            ],
            DEFAULT_TIMEOUT,
            "tokenize_region",
        )
    })?;

    let _ = request_id(&response)?;
    let result = response_result(&response, "tokenize_region")?;
    let windows = result
        .get("windows")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("missing windows: {}", pretty_json(result)))?;
    if windows.is_empty() {
        return Err(format!(
            "tokenize returned no windows: {}",
            pretty_json(result)
        ));
    }

    let first = windows[0].clone();
    let bounds = first
        .get("bounds")
        .ok_or_else(|| format!("missing bounds in first window: {}", pretty_json(&first)))?;
    let width = bounds
        .get("width")
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("missing width in bounds: {}", pretty_json(bounds)))?;
    let height = bounds
        .get("height")
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("missing height in bounds: {}", pretty_json(bounds)))?;
    let scale_w = width / 160.0;
    let scale_h = height / 220.0;
    if (scale_w - scale_h).abs() > 0.2 || scale_w < 0.9 {
        return Err(format!(
            "unexpected tokenize region scaling: bounds={}, scale_w={:.2}, scale_h={:.2}",
            pretty_json(bounds),
            scale_w,
            scale_h
        ));
    }

    let elements = first
        .get("elements")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("missing elements in first window: {}", pretty_json(&first)))?;
    if elements.is_empty() {
        return Err(format!(
            "tokenize region returned empty elements: {}",
            pretty_json(&first)
        ));
    }

    Ok(())
}

#[test]
fn smoke_pointer_click_id_calculator_flow() -> Result<(), String> {
    let _guard = smoke_lock()
        .lock()
        .map_err(|_| "failed to acquire smoke lock".to_string())?;
    let Some(cli) = SmokeCli::new("pointer_click_id")? else {
        return Ok(());
    };
    let _session = cli.begin_session();

    let timeout = Duration::from_secs(4);
    cli.open_app("Calculator", 4_000)?;
    thread::sleep(Duration::from_millis(40));
    let baseline = cli.run_json_ok(
        &["screen", "tokenize", "--active-window"],
        timeout,
        "baseline_signature",
    )?;
    let baseline_sig = element_signature(&baseline)?;

    // Step 1: type one digit and verify change.
    let (_, after_digit_sig) = click_and_wait_signature_change(
        &cli,
        "button_7",
        &baseline_sig,
        "type_digit_seed",
        "verify_type_digit_seed",
    )?;

    // Step 2: clear via Esc keypress and verify change.
    let (_, after_clear1_sig) = press_and_wait_signature_change(
        &cli,
        "escape",
        &after_digit_sig,
        "clear_first",
        "verify_clear_first",
    )?;

    // Step 3: type two digits and verify change.
    let _ = cli.run_json_ok(
        &["pointer", "click", "--id", "button_7"],
        timeout,
        "type_digit_first",
    )?;
    thread::sleep(Duration::from_millis(40));
    let (_, after_digits_sig) = click_and_wait_signature_change(
        &cli,
        "button_8",
        &after_clear1_sig,
        "type_digit_second",
        "verify_type_two_digits",
    )?;

    // Step 4: clear via Esc keypress again and verify change.
    let (final_state, _) = press_and_wait_signature_change(
        &cli,
        "escape",
        &after_digits_sig,
        "clear_second",
        "verify_clear_second",
    )?;
    let _ = request_id(&final_state)?;

    Ok(())
}

#[test]
fn smoke_screen_tokenize_overlay_contract_without_legacy_tokens() -> Result<(), String> {
    let _guard = smoke_lock()
        .lock()
        .map_err(|_| "failed to acquire smoke lock".to_string())?;
    let Some(cli) = SmokeCli::new("tokenize_overlay_contract")? else {
        return Ok(());
    };
    let _session = cli.begin_session();

    cli.open_app("Calculator", 10_000)?;
    thread::sleep(Duration::from_millis(400));

    let overlay_path = cli.artifact_dir.join("tokenize-overlay.png");
    let overlay_text = overlay_path.to_string_lossy().to_string();
    let response = cli.run_json_ok(
        &[
            "screen",
            "tokenize",
            "--active-window",
            "--overlay",
            &overlay_text,
        ],
        DEFAULT_TIMEOUT,
        "tokenize_overlay",
    )?;
    let _ = request_id(&response)?;
    let result = response_result(&response, "tokenize_overlay")?;
    if result.get("tokens").is_some() {
        return Err(format!(
            "legacy tokens field unexpectedly present: {}",
            pretty_json(result)
        ));
    }

    let image = result
        .get("image")
        .ok_or_else(|| format!("missing image in tokenize result: {}", pretty_json(result)))?;
    let width = image
        .get("width")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing image.width: {}", pretty_json(image)))?
        as u32;
    let height = image
        .get("height")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing image.height: {}", pretty_json(image)))?
        as u32;
    if width == 0 || height == 0 {
        return Err(format!(
            "invalid tokenize image dimensions: {}",
            pretty_json(image)
        ));
    }

    if !overlay_path.exists() {
        return Err(format!(
            "expected overlay output to exist: {}",
            overlay_path.display()
        ));
    }
    let (overlay_w, overlay_h) = png_dimensions(&overlay_path)?;
    if overlay_w != width || overlay_h != height {
        return Err(format!(
            "overlay dimensions mismatch tokenize image: overlay={}x{}, tokenize={}x{}",
            overlay_w, overlay_h, width, height
        ));
    }
    let win = active_window(&response)?;
    let elements = win
        .get("elements")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("missing elements in tokenize window: {}", pretty_json(&win)))?;
    if elements.is_empty() {
        return Err(format!(
            "tokenize overlay contract test got empty elements: {}",
            pretty_json(&win)
        ));
    }
    Ok(())
}

#[test]
fn smoke_pointer_scroll_changes_tokenized_region_long_text() -> Result<(), String> {
    let _guard = smoke_lock()
        .lock()
        .map_err(|_| "failed to acquire smoke lock".to_string())?;
    let Some(cli) = SmokeCli::new("pointer_scroll")? else {
        return Ok(());
    };
    let _session = cli.begin_session();

    cli.open_app("System Settings", 20_000)?;
    thread::sleep(Duration::from_millis(900));

    let screenshot = cli.run_json_ok(
        &["screen", "screenshot", "--active-window"],
        DEFAULT_TIMEOUT,
        "scroll_screenshot",
    )?;
    let result = response_result(&screenshot, "scroll_screenshot")?;
    let wb = result
        .get("window_bounds")
        .ok_or_else(|| format!("missing window_bounds: {}", pretty_json(result)))?;
    let wx = wb
        .get("x")
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("missing window_bounds.x: {}", pretty_json(wb)))?;
    let wy = wb
        .get("y")
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("missing window_bounds.y: {}", pretty_json(wb)))?;
    let ww = wb
        .get("width")
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("missing window_bounds.width: {}", pretty_json(wb)))?;
    let wh = wb
        .get("height")
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("missing window_bounds.height: {}", pretty_json(wb)))?;

    // In Settings, the left sidebar is the primary scrollable list.
    let center_x = (wx + ww * 0.12).round().max(0.0) as u32;
    let center_y = (wy + wh * 0.55).round().max(0.0) as u32;
    let _ = cli.run_json_ok(
        &[
            "pointer",
            "move",
            &center_x.to_string(),
            &center_y.to_string(),
        ],
        DEFAULT_TIMEOUT,
        "scroll_pointer_move",
    )?;

    let region_width = (ww * 0.25).max(120.0).min(420.0).floor() as u32;
    let max_region_height = (wh - 90.0).max(80.0);
    let region_height = max_region_height.min(520.0).floor() as u32;
    let region_y = 70_u32;

    let before = retry_json(6, Duration::from_millis(180), || {
        cli.run_json_ok(
            &[
                "screen",
                "tokenize",
                "--active-window",
                "--region",
                "0",
                &region_y.to_string(),
                &region_width.to_string(),
                &region_height.to_string(),
            ],
            DEFAULT_TIMEOUT,
            "scroll_tokenize_before",
        )
    })?;
    let mut before_sig = element_signature(&before)?;
    if before_sig.is_empty() {
        return Err("scroll baseline tokenize signature is empty".to_string());
    }
    let mut before_text_sig = text_signature(&before)?;
    if before_text_sig.is_empty() {
        return Err("scroll baseline tokenize text signature is empty".to_string());
    }

    let mut changed = false;
    let mut changed_text = false;
    for (step, dy) in [
        ("scroll_down_1", -1200),
        ("scroll_down_2", -1200),
        ("scroll_down_3", -1200),
        ("scroll_down_4", -1200),
        ("scroll_up_1", 1200),
    ] {
        let _ = cli.run_json_ok(
            &["pointer", "scroll", "0", &dy.to_string()],
            DEFAULT_TIMEOUT,
            step,
        )?;
        thread::sleep(Duration::from_millis(700));

        let after = retry_json(6, Duration::from_millis(180), || {
            cli.run_json_ok(
                &[
                    "screen",
                    "tokenize",
                    "--active-window",
                    "--region",
                    "0",
                    &region_y.to_string(),
                    &region_width.to_string(),
                    &region_height.to_string(),
                ],
                DEFAULT_TIMEOUT,
                "scroll_tokenize_after",
            )
        })?;
        let after_sig = element_signature(&after)?;
        let after_text_sig = text_signature(&after)?;
        if after_sig != before_sig {
            changed = true;
            changed_text = after_text_sig != before_text_sig;
            break;
        }
        before_sig = after_sig;
        before_text_sig = after_text_sig;
    }

    if !changed {
        return Err("pointer scroll did not change tokenize signature in region".to_string());
    }
    if !changed_text {
        return Err("pointer scroll changed structure but not long-text OCR in region".to_string());
    }

    Ok(())
}

#[test]
fn smoke_small_window_tokenize_region_perf_guard() -> Result<(), String> {
    let _guard = smoke_lock()
        .lock()
        .map_err(|_| "failed to acquire smoke lock".to_string())?;
    if env::var("DESKTOPCTL_SMOKE_PERF").is_err() {
        eprintln!(
            "skipping smoke test perf_guard: set DESKTOPCTL_SMOKE_PERF=1 to enable perf assertions"
        );
        return Ok(());
    }
    let Some(cli) = SmokeCli::new("tokenize_perf_guard")? else {
        return Ok(());
    };
    let _session = cli.begin_session();
    cli.open_app("Calculator", 10_000)?;
    thread::sleep(Duration::from_millis(350));

    let args = [
        "screen",
        "tokenize",
        "--active-window",
        "--region",
        "0",
        "0",
        "180",
        "220",
    ];
    let _ = cli.run_json_ok(&args, DEFAULT_TIMEOUT, "perf_warmup")?;
    thread::sleep(Duration::from_millis(80));

    let mut samples_ms: Vec<u128> = Vec::new();
    for idx in 0..3 {
        let start = Instant::now();
        let _ = cli.run_json_ok(&args, DEFAULT_TIMEOUT, &format!("perf_sample_{}", idx + 1))?;
        let elapsed = start.elapsed().as_millis();
        samples_ms.push(elapsed);
        thread::sleep(Duration::from_millis(60));
    }
    let mut sorted = samples_ms.clone();
    sorted.sort_unstable();
    let median = sorted[sorted.len() / 2];
    let max = sorted.iter().copied().max().unwrap_or(0);
    if median > 1_200 || max > 2_200 {
        return Err(format!(
            "small-window tokenize perf guard failed (median={}ms, max={}ms, samples={:?})",
            median, max, samples_ms
        ));
    }

    Ok(())
}
