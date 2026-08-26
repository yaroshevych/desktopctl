use std::{io::Write, process::Command};

use crate::{
    journal::{self, JournalConfig},
    trace,
};

pub fn show() {
    std::thread::spawn(|| {
        if let Err(err) = show_blocking() {
            trace::log(format!("journal:windows_dialog_err {err}"));
        }
    });
}

fn show_blocking() -> Result<(), String> {
    let cfg = journal::load_current_from_disk().config;
    let script = render_script(&cfg);
    let path = desktop_core::paths::AppPaths::resolve()
        .and_then(|paths| paths.ensure_cache_subdir("dialogs"))
        .map_err(|err| format!("resolve dialog cache directory failed: {err}"))?
        .join("journal-dialog.ps1");
    let mut file = std::fs::File::create(&path)
        .map_err(|err| format!("create {} failed: {err}", path.display()))?;
    file.write_all(script.as_bytes())
        .map_err(|err| format!("write {} failed: {err}", path.display()))?;

    let output = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            &path.display().to_string(),
        ])
        .output()
        .map_err(|err| format!("start PowerShell journal dialog failed: {err}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("PowerShell journal dialog failed: {stderr}"));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(line) = stdout
        .lines()
        .last()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    else {
        return Ok(());
    };
    if line == "cancelled" {
        return Ok(());
    }
    let mut next: JournalConfig = serde_json::from_str(line)
        .map_err(|err| format!("decode journal dialog response failed: {err}"))?;
    next.interval_seconds = next.interval_seconds.max(1);
    journal::apply(next)
}

fn render_script(cfg: &JournalConfig) -> String {
    let enabled = if cfg.enabled { "$true" } else { "$false" };
    let interval = cfg.interval_seconds;
    let output_dir = powershell_quote(&cfg.output_dir.display().to_string());
    format!(
        r#"
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing

$form = New-Object System.Windows.Forms.Form
$form.Text = 'Journal'
$form.Width = 520
$form.Height = 250
$form.StartPosition = 'CenterScreen'
$form.FormBorderStyle = 'FixedDialog'
$form.MaximizeBox = $false
$form.MinimizeBox = $false

$enabled = New-Object System.Windows.Forms.CheckBox
$enabled.Text = 'Enabled'
$enabled.Left = 20
$enabled.Top = 20
$enabled.Width = 460
$enabled.Checked = {enabled}
$form.Controls.Add($enabled)

$intervalLabel = New-Object System.Windows.Forms.Label
$intervalLabel.Text = 'Timeout seconds'
$intervalLabel.Left = 20
$intervalLabel.Top = 62
$intervalLabel.Width = 120
$form.Controls.Add($intervalLabel)

$interval = New-Object System.Windows.Forms.TextBox
$interval.Left = 150
$interval.Top = 58
$interval.Width = 80
$interval.Text = '{interval}'
$form.Controls.Add($interval)

$dirLabel = New-Object System.Windows.Forms.Label
$dirLabel.Text = 'Output directory'
$dirLabel.Left = 20
$dirLabel.Top = 102
$dirLabel.Width = 120
$form.Controls.Add($dirLabel)

$dir = New-Object System.Windows.Forms.TextBox
$dir.Left = 150
$dir.Top = 98
$dir.Width = 250
$dir.Text = {output_dir}
$form.Controls.Add($dir)

$choose = New-Object System.Windows.Forms.Button
$choose.Text = 'Choose...'
$choose.Left = 410
$choose.Top = 96
$choose.Width = 80
$choose.Add_Click({{
    $dialog = New-Object System.Windows.Forms.FolderBrowserDialog
    $dialog.SelectedPath = $dir.Text
    if ($dialog.ShowDialog($form) -eq [System.Windows.Forms.DialogResult]::OK) {{
        $dir.Text = $dialog.SelectedPath
    }}
}})
$form.Controls.Add($choose)

$warning = New-Object System.Windows.Forms.Label
$warning.Left = 20
$warning.Top = 138
$warning.Width = 470
$warning.ForeColor = [System.Drawing.Color]::DarkOrange
$form.Controls.Add($warning)

$cancel = New-Object System.Windows.Forms.Button
$cancel.Text = 'Cancel'
$cancel.Left = 300
$cancel.Top = 166
$cancel.Width = 88
$cancel.DialogResult = [System.Windows.Forms.DialogResult]::Cancel
$form.CancelButton = $cancel
$form.Controls.Add($cancel)

$save = New-Object System.Windows.Forms.Button
$save.Text = 'Save'
$save.Left = 402
$save.Top = 166
$save.Width = 88
$save.Add_Click({{
    $n = 0
    if (-not [UInt64]::TryParse($interval.Text.Trim(), [ref]$n) -or $n -lt 1) {{
        $warning.Text = 'Timeout must be a positive number of seconds.'
        return
    }}
    if ([string]::IsNullOrWhiteSpace($dir.Text)) {{
        $warning.Text = 'Choose an output directory.'
        return
    }}
    $form.Tag = [pscustomobject]@{{
        enabled = $enabled.Checked
        interval_seconds = $n
        output_dir = $dir.Text
    }}
    $form.DialogResult = [System.Windows.Forms.DialogResult]::OK
    $form.Close()
}})
$form.AcceptButton = $save
$form.Controls.Add($save)

if ($form.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {{
    $form.Tag | ConvertTo-Json -Compress
}} else {{
    'cancelled'
}}
"#
    )
}

fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
