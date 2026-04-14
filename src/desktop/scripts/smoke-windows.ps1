$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
Push-Location $root

$daemon = $null
try {
  $daemon = Start-Process -FilePath ".\target\x86_64-pc-windows-msvc\release\desktopctld.exe" -ArgumentList "--on-demand" -PassThru
  $ready = $false
  for ($attempt = 0; $attempt -lt 30; $attempt++) {
    Start-Sleep -Milliseconds 250
    $probe = & ".\target\x86_64-pc-windows-msvc\release\desktopctl.exe" debug ping 2>&1
    if ($LASTEXITCODE -eq 0 -and ($probe -match 'pong')) {
      $ready = $true
      break
    }
  }
  if (-not $ready) {
    throw "desktopctld did not become ready in time"
  }

  $pingOutput = & ".\target\x86_64-pc-windows-msvc\release\desktopctl.exe" debug ping
  if ($LASTEXITCODE -ne 0) {
    throw "desktopctl ping failed"
  }

  if (-not ($pingOutput -match 'pong')) {
    throw "desktopctl ping output did not contain expected pong: $pingOutput"
  }

  $tokenizeOutput = & ".\target\x86_64-pc-windows-msvc\release\desktopctl.exe" screen tokenize --active-window
  if ($LASTEXITCODE -ne 0) {
    throw "desktopctl screen tokenize --active-window failed"
  }

  if (-not ($tokenizeOutput -match '#axid_')) {
    throw "tokenize output did not include AX/UIA ids (#axid_). output: $tokenizeOutput"
  }

  $permissionsOutput = & ".\target\x86_64-pc-windows-msvc\release\desktopctl.exe" debug permissions
  if ($LASTEXITCODE -ne 0) {
    throw "desktopctl debug permissions failed"
  }

  Write-Host "Windows smoke test passed."
}
finally {
  if ($daemon -and -not $daemon.HasExited) {
    Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue
  }
  Pop-Location
}
