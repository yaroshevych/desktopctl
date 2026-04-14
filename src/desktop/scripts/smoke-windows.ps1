$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
Push-Location $root

$daemon = $null
try {
  $daemon = Start-Process -FilePath ".\target\x86_64-pc-windows-msvc\release\desktopctld.exe" -ArgumentList "--on-demand" -PassThru
  Start-Sleep -Milliseconds 700

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

  Write-Host "Windows smoke test passed."
}
finally {
  if ($daemon -and -not $daemon.HasExited) {
    Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue
  }
  Pop-Location
}
