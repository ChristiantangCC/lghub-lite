# lghub-lite

A lightweight **command-line tool** for Logitech HID++ mice and headsets: DPI, report rate,
battery, presets applied on connect. A ~300 KB CLI and background watcher, with an optional
small desktop app on top. No installer, no services, no network access, no telemetry, and
**no Logitech G HUB required**.

<p align="center"><img src="docs/screenshots/widget.png" width="420" alt="The lghub-lite battery widget showing a mouse at 53 %"></p>

> **Not affiliated with or endorsed by Logitech.** "Logitech", "G HUB", "LIGHTSPEED" and
> product names are trademarks of their owners and are used here only to describe
> compatibility. Use at your own risk; see [LICENSE](LICENSE).

**Status: 0.2 beta.** It works on the devices in the table below. Other HID++ devices will
usually be listed and readable, but writes are only verified where marked.

## Platforms

- **Windows 10/11**: supported (the CLI, the background watcher and the app).
- **Linux**: planned. The device layer and the app are Windows-only today; a Linux backend
  (hidraw, udev) is on the list but not written yet. When it lands it will be untested on real
  hardware at first, so device reports from Linux users will be very welcome.

## CLI first, app optional

lghub-lite is a command-line tool at heart: everything the app does, the CLI can do too (see
[Command line: quick start](#command-line-quick-start)). The app lives in its own folder,
`gui/`, as a separate project. If you only want the CLI, you can simply **delete the `gui`
folder**; the rest builds and works the same, with a single dependency.

## Tested devices

| Device | Connection | What works |
|---|---|---|
| PRO X SUPERLIGHT 2 | LIGHTSPEED receiver, USB cable | list, info, battery, DPI read/write, report rate read/write, onboard mode, presets, connect/disconnect events, onboard memory read/backup |
| PRO X Wireless headset | its USB dongle | list, info, battery percentage from the voltage (estimated until a discharge is measured, see below), equalizer (read-only) |

Have a different device? Run `lghub-lite devices` and `lghub-lite info <dev>` and open a
[device report](../../issues/new?template=device-report.md). That is how the table grows.

## Install

1. Download `lghub-lite-<version>-windows-x64.zip` from [Releases](../../releases) and unzip
   it anywhere, for example `C:\Tools\lghub-lite\`.
2. Double-click `lghub-lite-gui.exe`, or open PowerShell in that folder for the command-line tool.

The executables are not code-signed, so Windows SmartScreen may warn the first time; choose
"More info" → "Run anyway", or build from source (below).

If G HUB is installed it can keep running, but it may override settings you change here.
For predictable results, quit G HUB (tray icon → Quit) and disable it at login (Task
Manager → Startup apps).

## The app

<p align="center"><img src="docs/screenshots/skins-strip.png" alt="The widget in four built-in skins: Gunmetal, Amber CRT, Arctic and Synthline"></p>

Double-click **`lghub-lite-gui.exe`**. It opens a small battery widget:

- **The display** shows the selected device: battery percentage, time left, drain per hour,
  charging state and details such as the connection and, for headsets, the voltage and where
  the percentage comes from (see [Headset battery percentage](#headset-battery-percentage)).
  The segment gauge on the right and the **history** strip below show the level; the history
  has one bar per hour for the last 24 hours. Drain and time left appear after about an hour
  of readings.
- **◀ ▶** or a click in the **Devices** list switches device.
- **Alert at** sets the low-battery level for this device (red line). When a device on battery
  drops to it, the widget pops up and its bar in the tray icon turns red.
- **Tray** starts the app at login, in the tray, so presets are applied from login on. That
  replaces the command-line `autostart` entry, so only one of them runs.
- **Settings** opens the device drawer. For a mouse: **DPI** (type a value, **Apply**),
  **report rate** and **mode**. "Onboard profile" means the mouse uses settings stored inside
  it; **Use host** lets this PC set DPI and rate. Every change is read back from the device and
  saved as the mouse's preset, so it is applied again whenever the mouse connects; **Use
  onboard** goes back and removes the preset. For a headset: the battery curve and its
  recording.

  <p align="center"><img src="docs/screenshots/drawer-mouse.png" width="380" alt="Settings drawer for a mouse: DPI, report rate, mode"> <img src="docs/screenshots/drawer-headset.png" width="380" alt="Settings drawer for a headset: battery curve"></p>

- **Skins** (click the **lghub-lite** title, or **Skins…** in the tray menu) opens the skin manager: five built-in looks, colour editing (edits are saved as your
  own skin), and **Load skin file…** / **Export current** to share skins as small `.json`
  files. You can also drop a skin file on the window. Custom skins live in
  `%APPDATA%\lghub-lite\skins\`.

  <p align="center"><img src="docs/screenshots/skins.png" width="800" alt="The skin manager next to the widget"></p>

The round buttons in the title bar minimise the widget or hide it to the **tray**; drag the
title bar to move it. Use the tray menu's **Quit** to exit.

The icons are live too. The **taskbar** battery shows the device with the lowest charge: one
lit cell per started 25 %, red at or under that device's alert level. The **tray** icon has one
bar per device (the first two in the list), filled to its charge: green, red when low, and a
grey checkerboard while a device is offline. Its tooltip lists every device's level.

The app is about 4.9 MB (including its fonts) and uses roughly 150 MB of memory, mostly the graphics driver (it
draws with OpenGL). It uses no CPU while idle. If you want the smallest footprint, use the
command-line tool and `lghub-lite autostart on` (about 5 MB of memory) instead.

## Command line: quick start

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
curve is not linear. lghub-lite turns the voltage into a percentage with the best curve it has,
in this order:

1. **Your own measured curve.** While the app, `watch` or the background watcher runs, every
   headset voltage reading is added to `%APPDATA%\lghub-lite\voltage-<model>.csv`. Once that
   recording holds one full discharge (from full, 4.0 V or more, down to nearly empty, 3.55 V
   or less) and the headset starts charging again, lghub-lite builds `curve-<model>.txt` from
   it: the percentage left is the share of the measured use time still ahead, and the time
   left comes from the measured runtime. Each later full discharge is averaged in, so the curve
   follows the battery as it ages. Time with the headset off or the PC asleep is not counted.
   The app's **Build from recording** button and `lghub-lite curve <dev> build` do the same
   on demand.
2. **A G HUB curve file**, if you have G HUB installed (or had it): copy your own copy into
   the lghub-lite folder, or use **Import from G HUB** in the app. It is never bundled.

   ```powershell
   Copy-Item "C:\ProgramData\LGHUB\depots\*\pro_headset\pro_x_wireless_headset_battery.xml" "$env:APPDATA\lghub-lite\"
   ```
3. **A generic Li-ion estimate**, shown as `≈`. It reads low under load (about 20 points
   below G HUB's figure on the PRO X Wireless at 3.85 V), so treat it as a rough guide until
   your own discharge has been recorded.

`lghub-lite curve <dev>` shows which curve is used and how far the recording has got. While
the headset is charging the percentage is too high (charging is not detected yet).

A measured `curve-<model>.txt` is your own data, so you can share it: attach it to a
[device report](../../issues/new?template=device-report.md) and it can ship built in for others.

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
| `curve <dev> [build]` | Show the headset battery curve and recording; `build` makes or refines the measured curve |
| `eq <dev>` | Show a headset's hardware equalizer (read-only) |
| `profiles <dev>` | Onboard profile memory overview (read-only) |
| `dump <dev> <file>` | Back up all onboard memory to a text file (read-only) |
| `collections` | List every Logitech HID collection (debugging) |
| `raw <dev> <hex…> --allow-raw` | Send one raw HID++ report (debugging, use with care) |

Options: `--trace` prints every report sent and received; `--verbose` prints extra
diagnostics; `--config FILE` uses another preset file (curve files, voltage recordings and
logs live in its folder).

## What it does not do

- **No firmware updates**, by design. Use Logitech's own software for that, occasionally.
- **No writing to onboard profile memory.** It can be read and backed up (`dump`), not changed.
- **No lighting, macros or button remapping** yet.
- **No software audio effects** (equalizer, Blue VO!CE, DTS). Those are PC-side audio
  processing, not device settings. Free alternatives: Equalizer APO for EQ, Windows Sonic for
  spatial audio.
- **Headset sidetone** is not supported: the control the headset exposes had no audible effect.

## Build from source

Requires the Rust MSVC toolchain.

```text
cargo build --release
```

builds `lghub-lite.exe` (CLI) and `lghub-lite-bg.exe` (background watcher) in
`target\release\`. They need Rust 1.88+ and depend only on
[`windows-sys`](https://crates.io/crates/windows-sys) (raw Windows API bindings).

The app is its own project in `gui/` (it uses the CLI's code as a library):

```text
cd gui
cargo build --release
```

builds `gui\target\release\lghub-lite-gui.exe`. It uses [egui](https://github.com/emilk/egui)
and [tray-icon](https://crates.io/crates/tray-icon) and needs Rust 1.95+.
`lghub-lite-gui.exe --demo` shows sample devices without touching any hardware (that is how the
screenshots above were made).

Tests run without hardware: `cargo test`, in the root and in `gui/`.

## How it works

lghub-lite talks the HID++ protocol directly to the device's vendor HID collections. Devices
behind a LIGHTSPEED/Unifying receiver are reached through the receiver. Protocol details,
hardware traces and the evidence behind every constant are in [NOTES.md](NOTES.md).

Behaviour was learned for interoperability from public HID++ knowledge (Solaar, libratbag),
from studying how G HUB's device layer talks to the hardware, and from testing on real
devices. This repository contains no Logitech code or data.

## License

MIT, see [LICENSE](LICENSE).

The app bundles these fonts under the SIL Open Font License 1.1 (licence texts in
[`gui/fonts/`](gui/fonts/) and in the release zip): IBM Plex Sans and IBM Plex Mono
(trimmed to the characters the app uses and renamed "LL Sans" / "LL Mono", as the licence
requires for modified copies of the Reserved Font Name "Plex"), Space Grotesk and VT323.
