$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
Push-Location $root

$daemon = $null
try {
  $daemon = Start-Process -FilePath ".\target\x86_64-pc-windows-msvc\release\desktopctld.exe" -ArgumentList "--on-demand" -PassThru
  Start-Sleep -Milliseconds 700

  $pingOutput = & ".\target\x86_64-pc-windows-msvc\release\desktopctl.exe" ping
  if ($LASTEXITCODE -ne 0) {
    throw "desktopctl ping failed"
  }

  if (-not ($pingOutput -match '"ok"')) {
    throw "desktopctl ping output did not contain expected status: $pingOutput"
  }

  Write-Host "Windows smoke test passed."
}
finally {
  if ($daemon -and -not $daemon.HasExited) {
    Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue
  }
  Pop-Location
}
