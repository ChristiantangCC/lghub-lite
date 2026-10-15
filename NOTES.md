# lghub-lite — engineering log

Minimal Rust CLI that talks HID++ directly to Logitech hardware on Windows. No G HUB dependency at runtime.
Research docs live one level up: [`../protocol.md`](../protocol.md) (cited as §N) and `../reference/`.
Tags: `[OBS]` seen in G HUB, `[PUB]` public HID++ knowledge (Solaar/libratbag), `[INF]` inferred, `[UNK]` unknown, `[HW]` verified on hardware here.

## 1. Status

Works on real hardware (PRO X SUPERLIGHT 2 via LIGHTSPEED receiver c54d; PRO X Wireless headset 0aba over USB):

| Command | State |
|---|---|
| `devices` | ✅ lists direct devices, receivers, and paired devices behind receivers with online state |
| `info <dev>` | ✅ protocol version, name, type, firmware entities, full feature table |
| `battery <dev>` | ✅ SL2 via 0x1004. Headset has no 0x1000/1001/1004 (uses 0x1F20, not implemented) |
| `dpi <dev>` | ✅ read (0x2201 and 0x2202) |
| `dpi <dev> <v>` | ⛔ SL2 answers **NotAllowed** while in onboard-profile mode (see §6, §7) |
| `raw <dev> <hex…> --allow-raw` | implemented, debug only |
| `--trace` | prints every sent/received report in hex; reports not matching the pending request are marked `(ignored)` |

**MVP not complete:** the DPI write is blocked on the onboard-mode decision (pending test request below).

## 2. Protocol facts used by the code

### Architecture
Single binary crate, no traits, no async, no threads:
- `src/hid.rs`: SetupDi enumeration, HidP caps, CreateFileW (shared R/W, overlapped). It keeps one persistent overlapped read per collection, uses `WaitForMultipleObjects` for timeouts, and cancels and drains I/O on drop.
- `src/hidpp.rs`: framing, swid, reply matching, error decoding, Root/ping, feature names.
- `src/device.rs`: interface grouping, receiver slots, identity, battery, DPI, onboard mode.
- `src/main.rs`: argument parsing and output.

### Transport
- **Collection selection.** We accept page `FF00` usage 1 (7 B) or 2 (20 B), and page `FF43` where usage `HHLL` has LL a single bit and `HH&LL≠0`. [OBS §9.2]
  - We also require the length to match a HID++ report. This excludes the headset's `FF00/0001` 64-byte "AvServe" collection. [INF]
  - [HW] receiver c54d `mi_02`: `FF00/0001 in=7 out=7`, `FF00/0002 in=20 out=20`.
  - [HW] headset 0aba `mi_03`: `FF43/0202 in=20 out=20` (the collection G HUB opens, matching agent `/devices/list`), plus `FF00/0001 in=64 out=64` (excluded).
- **Grouping.** Collections of one interface are grouped by path with `&colNN` and the last `&NNNN` of the instance removed. [HW]
- **Write.** Each message goes to the first collection whose output length is at least the message length. It is zero-padded and byte 0 is rewritten to that collection's report ID. [OBS §9.3] [HW]
  - The headset only has a 20-byte collection, so short requests are sent as `11 …`; it answers correctly.
- **Reads.** We read both collections. [HW]
  - **SL2 behind the receiver answers short (`10`) requests with long (`11`) reports.** For example, `-> 10 01 00 13 00 00 5A` gets `<- 11 01 00 13 04 02 5A 00…`.
  - Receiver HID++ 1.0 short register replies come back as `10`.
- **Share mode** `FILE_SHARE_READ|WRITE` works while G HUB runs. [HW]

### Request/response
- **Software ID.** We use swid **0x03**, overridable with `--swid`. G HUB uses 0x0A..0x0E [OBS §9.4]. No collision was seen with G HUB running. [HW]
- **Matching.** We compare `rsp[1..3]==cmd[1..3]` plus per-call echo offsets. The report ID is ignored and replies must be at least 5 bytes. [OBS §9.5]
  - Echo offsets: ping byte 6; 0x2201 f1/f2 and 0x2202 f5/f6 byte 4; 0x2202 f2 bytes 4–6; B5 sub-address byte 4. [OBS dec_all.c FUN_140ccee80, FUN_140cd04e0, FUN_140cd0df0, FUN_140cd0ff0; §9.5 "match list"]
- **HID++ 2.0 error.** `rsp[2]==FF`, `rsp[3]==cmd[2]`, `rsp[4]==cmd[3]`, code in `rsp[5]`.
  - [HW] `11 01 FF 09 63 05` = NotAllowed for 0x2202 f6.
- **HID++ 1.0 error.** `rsp[2]==8F`, code in `rsp[5]`.
  - [HW] the receiver answers a 2.0 ping with `10 FF 8F 00 13 01 00` (code 1, invalid sub-ID). This is how we detect a receiver. [OBS §9.6 NotHpp20]
  - [HW] an empty pairing slot answers `10 FF 8F 83 B5 09 00` (0x09 resource error). The code names come from [PUB].
- **Duplicated replies.** Seen once on the SL2 right after wake: the same ping reply arrived twice. The second copy no longer matches a pending request and is ignored. [HW]
- **Timeout.** 2000 ms per request; G HUB uses 3000 ms with 9 retries [OBS §9.6].
  - [HW] a 1000 ms timeout was too short for the first request after the SL2 had been idle (asleep). No retries are implemented yet.

### Receiver (HID++ 1.0, device index 0xFF)
- **Reg 0x02 read.** `-> 10 FF 81 02 00 00 00`, `<- 10 FF 81 02 00 01 00`. `p[1]` = 1 connected device. [OBS §9.8 bytes; PUB meaning; HW]
- **Pairing info.** `-> 10 FF 83 B5 20 00 00`, `<- 11 FF 83 B5 20 08 08 40 A9 11 00 02 B1 01 00 00 7F 24 00 00`. [OBS §9.8 request; PUB layout; HW]
  - `p[3..5]` = wireless PID **0x40A9** (BE).
  - `p[7]&0x0F` = 2 = mouse.
  - The other bytes are not decoded.
  - Note: the 0x41 notification carries the PID little-endian (§9.8); the B5 reply is big-endian.
- **Online check.** Root ping to device index `slot+1`. [HW]

### Identity (HID++ 2.0)
- **Root f1 ping.** `10 idx 00 1s 00 00 5A` → `[major, minor, echo]`. [OBS §9.6] [HW: SL2 4.2, headset 4.2]
- **Root f0 GetFeature** `[idHi, idLo]` → `[index, flags, version]`. [OBS §9.7] [HW]
- **0x0001 FeatureSet.** f0 → count; f1 `[i]` → `[idHi, idLo, flags, version]`. [OBS §9.7] [HW]
  - Count excludes Root (SL2 reports 0x20 = 32, indices 1..32).
  - Flags bit 7 obsolete, 6 hidden, 5 engineering [PUB]. SL2 engineering features show `0x60`/`0x70`.
- **0x0005 DeviceName.** f0 → length; f1 `[offset]` → up to 16 chars; f2 → type (3 mouse, 8 headset [PUB]). [OBS §9.7] [HW]
  - SL2 name is "PRO X 2" (len 7). The headset name is 29 chars over 2 chunks.
- **0x0003 FirmwareInfo.** [OBS §9.7 + PUB layout] [HW]
  - f0 → `[count, unitId(4), transport(2), modelId(6), extModel, caps…]`. SL2 returns `02 7A F4 AD 28 00 0C 40 A9 C0 9B 00 00 00 01`.
  - f1 `[e]` → `[type&0x0F, prefix(3 ASCII), number, revision, build BE16, active, transportPid BE16, …]`. SL2 returns:
    - `01 42 4C 31 71 05 00 29 00 AB 1C…` = bootloader BL1 71.05 B0029;
    - `00 4D 50 4D 32 05 00 29 01 40 A9…` = main MPM 32.05 B0029.
  - **Cross-check with G HUB's agent:** the headset decodes to `U112.01.B0203`; the agent reports firmwareName `U112`, firmwareVersion `112.1.203`. ✅

### Battery
- **0x1004 f1** → `[soc %, level bits (b0 crit, b1 low, b2 good, b3 full), charging, external]`. [OBS §10.5 order, PUB bytes]
  - [HW SL2] `-> 10 01 06 13 00 00 00`, `<- 11 01 06 13 0D 01 00 00` = 13 %, critical, discharging. **The physical value still needs confirmation (pending §6).**

### DPI
- **0x2201** (layout from G HUB, not yet on hardware: no device here has it). [OBS §9.9; FUN_140ccee80 / 140ccf0e0 / 140ccf280 / 140ccf450]
  - f1 `[sensor]` → `[sensor, BE16 stream]`.
  - f2 `[sensor]` → `[sensor, cur BE16, def BE16]`.
  - f3 `[sensor, dpi BE16]`, plus a `dpiLevel` byte if version ≥ 2. The meaning of dpiLevel is [UNK]; we send 0. UNVERIFIED.
- **0x2202 v0** on the SL2 (index 9). [OBS FUN_140cd04e0, FUN_140cd0df0, FUN_140cd0ff0]
  - **f2 ranges `[sensor, dir, page]`** [HW]. Each reply echoes 3 bytes, then carries 13 bytes of a BE16 stream. **Values straddle pages**, so pages are concatenated. `0xE0xx` = step xx (low byte). `0x0000` ends the list. SL2 pages 0..3:
    ```
    11 01 09 23 00 00 00 00 64 E0 01 00 C8 E0 02 01 F4 E0 05 03
    11 01 09 23 00 00 01 E8 E0 0A 07 D0 E0 14 13 88 E0 32 27 10
    11 01 09 23 00 00 02 E0 64 4E 20 E0 7D 7D 00 E0 C8 AB E0 00
    11 01 09 23 00 00 03 00 00 …
    ```
    These decode to 100–200/1, 200–500/2, 500–1000/5, 1000–2000/10, 2000–5000/20, 5000–10000/50, 10000–20000/100, 20000–32000/125, 32000–44000/200.
  - **f5 get `[sensor]`** → `[sensor, x, y, defX, defY (BE16), lod]` [HW]. `<- 11 01 09 53 00 03 20 03 20 03 20 03 20 02` = 800/800, default 800/800, LOD 2.
  - **f6 set `[sensor, xH, xL, yH, yL, lod]`**. We send X=Y=value and keep LOD. [HW] `-> 11 01 09 63 00 06 40 06 40 02` gets `<- 11 01 FF 09 63 05` (**NotAllowed**) in onboard mode. The encoding is UNVERIFIED until the write succeeds.
  - `dir` 0 = X axis [INF].
- **0x8100 f2 get mode** → `p[0]` (1 = onboard [OBS §9.9], 2 = host [PUB]). [HW] SL2 returns `01` (onboard).

## 3. Corrections to protocol.md
- **§10.7:** the PRO X Wireless headset (0aba) has **no** 0x1000/0x1001/0x1004. Its HID++ features are 0000, 0001, 0003, 0005, 8310, 8300 and **1F20 ADCMeasurement**, so battery voltage must come from 0x1F20 (layout [UNK]). [HW]
- **§9.10 vs. headset:** 0aba speaks plain HID++ 2.0 (protocol 4.2) on `FF43/0202`, not Centurion framing. [HW]
- **§9.9 0x2202 f2:** the range stream continues across pages at byte granularity (13 bytes per page). It is not whole BE16 values per page. [HW + OBS FUN_140cd04e0 carries parity across pages]
- **§9.9 0x2201/0x2202 step:** G HUB takes the step from the low byte of `0xE0xx` only. [OBS]
- **New device, not in G HUB 2023.1's plaintext DB:** receiver **c54d** with **PRO X SUPERLIGHT 2**, wireless PID **0x40A9**, model IDs `40A9 C09B`. The agent does not list it. [HW]

## 4. Dependencies
| Crate | Why | Transitive |
|---|---|---|
| `windows-sys 0.61` (features: Foundation, HumanInterfaceDevice, DeviceAndDriverInstallation, Storage_FileSystem, System_IO, System_Threading, Security) | Raw Win32 bindings for SetupDi/HidD/HidP/CreateFileW/overlapped I/O. std has no HID or overlapped I/O. | +1 (`windows-link`) |

**Rejected `hidapi`:** it builds C via `cc`, hides share mode and overlapped timeout control, and needs its own per-collection handling for Windows anyway.

No `clap`, `serde`, logging crates or async runtime. Dev-only research tool: `tools/agent.ps1` (PowerShell, GET-only agent client, not part of the crate).

## 5. Tests run
| Date | Device | Connection | Test | Result |
|---|---|---|---|---|
| 2026-10-15 | LIGHTSPEED receiver c54d | USB | reg 0x02, B5 pairing 0..5, ping | ✅ 1 connected; slot 0 = 0x40A9 mouse; slots 1–5 error 0x09 |
| 2026-10-15 | PRO X SUPERLIGHT 2 (0x40A9) | LIGHTSPEED | ping, name, type, firmware, feature set | ✅ (see §2) |
| 2026-10-15 | PRO X SUPERLIGHT 2 | LIGHTSPEED | battery 0x1004 | ✅ read 13 %; physical confirmation pending |
| 2026-10-15 | PRO X SUPERLIGHT 2 | LIGHTSPEED | DPI read 0x2202 | ✅ 800, LOD 2 |
| 2026-10-15 | PRO X SUPERLIGHT 2 | LIGHTSPEED | DPI set 0x2202 f6 → 1600 | ⛔ NotAllowed (onboard mode); DPI unchanged (800) |
| 2026-10-15 | PRO X Wireless headset 0aba | USB (own dongle) | ping, name, type, firmware, features | ✅ firmware matches agent |
| — | all | — | `cargo test`: 16 offline byte-exact tests | ✅ |

All tests ran with G HUB running. G HUB does not manage the SL2.

## 6. Pending test requests

**TEST REQUEST 1: SL2 battery.**
- Command: `lghub-lite battery c54d:1`. G HUB can stay open.
- Expected output: `<N>% level=… discharging external=none`. N was 13 on 2026-10-15.
- What to observe: does that match the mouse's own low-battery indication (or another tool such as G HUB on a newer build or Solaar)?
- Restore: nothing changes.

**TEST REQUEST 2: DPI write.** Blocked on the approval in §7.
- Commands, in order:
  1. Fully quit G HUB, including the tray and agent (it does not manage this mouse, but rule it out).
  2. `lghub-lite dpi c54d:1` and note `dpi N`.
  3. Run the host-mode switch, once approved and implemented.
  4. `lghub-lite dpi c54d:1 400`.
- Expected output: `old=800`, then `new=400 readback=400`.
- What to observe: the cursor moves visibly slower.
- Restore: `lghub-lite dpi c54d:1 800`, then switch back to onboard mode or power-cycle the mouse.

## 7. Open questions
- **Onboard vs host mode (blocking the DPI write).** Switching to host mode means 0x8100 f1 `[mode]` with mode 2 = host [PUB libratbag; OBS §9.9 lists f1 set mode].
  - Unverified whether the mode change is volatile (RAM only) or persisted. [PUB] suggests it reverts on power cycle.
  - **Needs human approval before sending.**
- **0x8061 report rate** on the SL2 (v0): next feature slice.
- **0x1F20 ADCMeasurement** reply layout for the headset battery [UNK]. G HUB maps mV through the depot XML curve (§10.6).
- **B5 pairing reply** bytes p[1], p[2], p[5..] are not decoded.
- **Unknown SL2 feature IDs** without names in the G HUB table: 0x2251, 0x80E0, 0x1602, and the engineering IDs 0x1803/0x1817/0x1830/0x1875/0x1861/0x1E02/0x1E22/0x1EB0/0x18B1.
- **Retry policy:** G HUB retries 9× with backoff (§9.6). We do none yet; the 2 s timeout is enough so far.
- **Agent `GET /routes` diff** (§14 #1, 2026-10-15): it returned 457 route entries, 305 unique paths.
  - The 37 "new" paths are `%s` templates instantiated with `dev00000000`.
  - DPI/report-rate routes exist only as BROADCAST (`/mouse/dpi`) because no mouse is attached to G HUB.

## 8. Metrics (2026-10-15, release, opt-level "s", LTO, panic=abort, strip)
| Metric | Value |
|---|---|
| `lghub-lite.exe` size | **184,832 B** (opt-level 3: 201,728 B, so we keep "s") |
| Crates in tree | 3 (`lghub-lite`, `windows-sys`, `windows-link`) |
| `devices` wall time | median 116 ms, min 91 ms (10 runs, `Measure-Command`; includes wireless HID++ round trips) |
| `collections` wall time (enumeration only) | median 49 ms |
| Long-running modes | none yet |
