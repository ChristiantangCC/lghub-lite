# Package the Windows release zip from the release builds (run from the repo root after
# `cargo build --release` in the root and in gui/). Used by CI (to check it) and Release.
param([Parameter(Mandatory)][string]$Version)
$ErrorActionPreference = "Stop"
$name = "lghub-lite-$Version-windows-x64"
if (Test-Path $name) { Remove-Item -Recurse -Force $name }
New-Item -ItemType Directory "$name\fonts" | Out-Null
Copy-Item gui\target\release\lghub-lite-gui.exe, target\release\lghub-lite.exe, target\release\lghub-lite-bg.exe, README.md, LICENSE $name
Copy-Item gui\fonts\*.txt, gui\fonts\README.md "$name\fonts"
Compress-Archive -Path "$name\*" -DestinationPath "$name.zip" -Force
(Get-FileHash "$name.zip" -Algorithm SHA256).Hash.ToLower() + "  $name.zip" | Out-File "$name.zip.sha256" -Encoding ascii
Get-ChildItem "$name.zip", "$name.zip.sha256" | Format-Table Name, Length
