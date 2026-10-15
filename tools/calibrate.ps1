# Dev-only: record one (headset voltage, G HUB battery %) pair for the voltage->percent table
# in src/audio.rs. Needs G HUB running (read-only GET on its agent). Not part of the crate.
# Usage: powershell -File tools/calibrate.ps1 [-Device 0aba] [-AgentId dev00000000]
param([string]$Device = "0aba", [string]$AgentId = "dev00000000")

$root = Split-Path $PSScriptRoot -Parent
$exe = Join-Path $root "target\release\lghub-lite.exe"
$csv = Join-Path $PSScriptRoot "battery-calibration.csv"

$line = & $exe battery $Device 2>&1 | Select-Object -First 1
if ("$line" -notmatch '^(\d+) mV.*flags=(0x[0-9a-f]+)') { Write-Error "unexpected output: $line"; exit 1 }
$mv = $Matches[1]; $flags = $Matches[2]

$reply = & powershell -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot "agent.ps1") "/battery/$AgentId/state" | ConvertFrom-Json
$pct = $reply.payload.percentage; $charging = [bool]$reply.payload.charging

if (-not (Test-Path $csv)) { "time,device,mv,flags,ghub_percent,ghub_charging" | Out-File $csv -Encoding ascii }
"{0},{1},{2},{3},{4},{5}" -f (Get-Date -Format s), $Device, $mv, $flags, $pct, $charging | Out-File $csv -Append -Encoding ascii
"recorded: $mv mV = $pct % (charging=$charging)"
