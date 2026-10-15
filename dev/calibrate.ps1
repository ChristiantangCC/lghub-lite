# Dev-only: compare lghub-lite's headset battery reading with G HUB's, and log both to
# %APPDATA%\lghub-lite\battery-calibration.csv (never into the repository).
# Needs G HUB running (read-only GET on its local agent, see agent.ps1). Not part of the app.
# Usage: powershell -File dev\calibrate.ps1 [-Device 0aba] [-AgentId dev00000000]
param([string]$Device = "0aba", [string]$AgentId = "dev00000000")

$root = Split-Path $PSScriptRoot -Parent
$exe = Join-Path $root "target\release\lghub-lite.exe"
$csv = Join-Path $env:APPDATA "lghub-lite\battery-calibration.csv"

$line = "$(& $exe battery $Device 2>&1 | Select-Object -First 1)"
if ($line -notmatch '(\d+) mV, flags=(0x[0-9a-f]+)|^(\d+) mV +flags=(0x[0-9a-f]+)') { Write-Error "unexpected output: $line"; exit 1 }
$mv = if ($Matches[1]) { $Matches[1] } else { $Matches[3] }
$flags = if ($Matches[2]) { $Matches[2] } else { $Matches[4] }
$ours = if ($line -match '^(\d+)%') { $Matches[1] } else { "" }

$reply = & powershell -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot "agent.ps1") "/battery/$AgentId/state" | ConvertFrom-Json
$pct = $reply.payload.percentage; $charging = [bool]$reply.payload.charging

New-Item -ItemType Directory -Force (Split-Path $csv) | Out-Null
if (-not (Test-Path $csv)) { "time,device,mv,flags,ghub_percent,ghub_charging" | Out-File $csv -Encoding ascii }
"{0},{1},{2},{3},{4},{5}" -f (Get-Date -Format s), $Device, $mv, $flags, $pct, $charging | Out-File $csv -Append -Encoding ascii
"recorded: $mv mV  flags=$flags  lghub-lite=$ours%  G HUB=$pct% (charging=$charging) -> $csv"
