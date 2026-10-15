# Developer helpers (not part of lghub-lite)

These PowerShell scripts were used while reverse-engineering device behaviour. They need
Logitech G HUB installed and running, and they only make **read-only** `GET` requests to
G HUB's local agent (`ws://127.0.0.1:9010`). lghub-lite itself never talks to G HUB.

| Script | Purpose |
|---|---|
| `agent.ps1 <path> [outfile]` | Send one GET to the G HUB agent and print the JSON reply, e.g. `/devices/list` or `/battery/dev00000000/state`. Used as an oracle to cross-check readings. |
| `calibrate.ps1` | Read the headset battery with lghub-lite and with G HUB at the same moment; log both to `%APPDATA%\lghub-lite\battery-calibration.csv`. |
