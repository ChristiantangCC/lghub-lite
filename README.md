# lghub-lite

A tiny Windows command-line tool for Logitech HID++ mice and headsets: DPI, report rate,
battery, presets applied on connect, and an event-driven background watcher. Around 300 KB,
no installer, no services, no network access, no telemetry, and **no Logitech G HUB required**.

> **Not affiliated with or endorsed by Logitech.** "Logitech", "G HUB", "LIGHTSPEED" and
> product names are trademarks of their owners and are used here only to describe
> compatibility. Use at your own risk; see [LICENSE](LICENSE).

**Status: 0.1 beta.** It works on the devices in the table below. Other HID++ devices will
usually be listed and readable, but writes are only verified where marked.

## Tested devices

| Device | Connection | What works |
|---|---|---|
| PRO X SUPERLIGHT 2 | LIGHTSPEED receiver, USB cable | list, info, battery, DPI read/write, report rate read/write, onboard mode, presets, connect/disconnect events, onboard memory read/backup |
| PRO X Wireless headset | its USB dongle | list, info, battery voltage (percentage with a curve file, see below), equalizer (read-only) |

Have a different device? Run `lghub-lite devices` and `lghub-lite info <dev>` and open a
[device report](../../issues/new?template=device-report.md). That is how the table grows.

## Install

1. Download `lghub-lite-<version>-windows-x64.zip` from [Releases](../../releases) and unzip
   it anywhere, for example `C:\Tools\lghub-lite\`.
2. Open PowerShell or Command Prompt in that folder.

The executables are not code-signed, so Windows SmartScreen may warn the first time; choose
"More info" → "Run anyway", or build from source (below).

If G HUB is installed it can keep running, but it may override settings you change here.
For predictable results, quit G HUB (tray icon → Quit) and disable it at login (Task
Manager → Startup apps).

## Quick start

```text
lghub-lite devices                 list devices, including those behind a receiver
lghub-lite info c54d:1             identity, firmware, supported features
lghub-lite battery c54d:1          battery
lghub-lite dpi c54d:1              current DPI and allowed range
```

`<dev>` is what `devices` prints: a USB product ID (`0aba`) or, for a device behind a receiver,
`receiver-PID:slot` (`c54d:1`).

### Changing DPI and report rate

Many Logitech mice run from **onboard profiles** stored in the mouse and refuse changes until
they are switched to **host mode**:

```text
lghub-lite mode c54d:1 host        allow changes from the PC
lghub-lite dpi c54d:1 1600         set DPI, read it back
lghub-lite rate c54d:1 1000        set report rate (Hz), read it back
lghub-lite mode c54d:1 onboard     go back to the mouse's own profile
```

Host-mode changes live in the mouse's memory only until it is switched back to onboard mode
(and possibly until it is power-cycled), so nothing permanent is written. To make settings stick,
use presets.

### Presets: apply your settings automatically

```text
lghub-lite preset c54d:1 dpi=1600 rate=1000    save for this mouse model
lghub-lite presets                              show saved presets
lghub-lite apply c54d:1                         apply now
lghub-lite autostart on                         apply on every connect, from login on
```

`autostart on` registers `lghub-lite-bg.exe` (a windowless `watch`) to start at login and starts
it now. Whenever the mouse connects, wakes up or is plugged in, it switches the mouse to host
mode and applies the preset. `autostart off` removes the login entry and stops it;
`autostart` alone shows the status.

Presets are stored in `%APPDATA%\lghub-lite\presets.txt`, one line per model, e.g.
`40a9 dpi=1600 rate=1000`. You can edit it by hand.

### Watching events

```text
lghub-lite watch
```

Prints, with timestamps: devices plugged in or removed, wireless connect/disconnect (including
sleep and wake), DPI, report rate and battery changes. It waits on Windows events and uses no
CPU while nothing happens. The background watcher writes the same lines to
`%APPDATA%\lghub-lite\watch.log`.

### Headset battery percentage

Headsets like the PRO X Wireless only report a battery **voltage**, and the voltage-to-percent
curve is not linear. lghub-lite does not ship Logitech's curve data. If you have (or had)
G HUB installed, copy your own copy of the curve file into the lghub-lite folder:

```powershell
Copy-Item "C:\ProgramData\LGHUB\depots\*\pro_headset\pro_x_wireless_headset_battery.xml" "$env:APPDATA\lghub-lite\"
```

Then `lghub-lite battery 0aba` shows the percentage and hours left, matching G HUB. Without the
file it shows the voltage. While the headset is charging the percentage is too high (charging
is not detected yet).

## All commands

| Command | Description |
|---|---|
| `devices` | List HID++ devices, including those behind receivers |
| `info <dev>` | Protocol version, name, type, firmware, feature list |
| `battery <dev>` | Battery state |
| `dpi <dev> [value]` | Read DPI, or set it and read it back |
| `rate <dev> [hz]` | Read report rate, or set it and read it back |
| `mode <dev> [host\|onboard]` | Read or set onboard-profile mode |
| `preset <dev> [dpi=N] [rate=HZ]` / `preset <dev> clear` | Save or remove a preset for this model |
| `presets` | Show saved presets |
| `apply <dev>` | Apply the saved preset now |
| `watch` | Print device and setting events until Ctrl+C |
| `autostart [on\|off]` | Run the background watcher at login, or stop it |
| `eq <dev>` | Show a headset's hardware equalizer (read-only) |
| `profiles <dev>` | Onboard profile memory overview (read-only) |
| `dump <dev> <file>` | Back up all onboard memory to a text file (read-only) |
| `collections` | List every Logitech HID collection (debugging) |
| `raw <dev> <hex…> --allow-raw` | Send one raw HID++ report (debugging, use with care) |

Options: `--trace` prints every report sent and received; `--verbose` prints extra
diagnostics; `--config FILE` uses another preset file (curve files and logs live in its
folder).

## What it does not do

- **No firmware updates**, by design. Use Logitech's own software for that, occasionally.
- **No writing to onboard profile memory.** It can be read and backed up (`dump`), not changed.
- **No lighting, macros or button remapping** yet.
- **No software audio effects** (equalizer, Blue VO!CE, DTS). Those are PC-side audio
  processing, not device settings. Free alternatives: Equalizer APO for EQ, Windows Sonic for
  spatial audio.
- **Headset sidetone** is not supported: the control the headset exposes had no audible effect.

## Build from source

Requires Rust 1.88 or newer (MSVC toolchain).

```text
cargo build --release
```

The binaries are `target\release\lghub-lite.exe` and `target\release\lghub-lite-bg.exe`. The only
dependency is [`windows-sys`](https://crates.io/crates/windows-sys) (raw Windows API bindings).
Tests run without hardware: `cargo test`.

## How it works

lghub-lite talks the HID++ protocol directly to the device's vendor HID collections. Devices
behind a LIGHTSPEED/Unifying receiver are reached through the receiver. Protocol details,
hardware traces and the evidence behind every constant are in [NOTES.md](NOTES.md).

Behaviour was learned for interoperability from public HID++ knowledge (Solaar, libratbag),
from studying how G HUB's device layer talks to the hardware, and from testing on real
devices. This repository contains no Logitech code or data.

## License

MIT, see [LICENSE](LICENSE).
