# lghub-lite — engineering log

Minimal Rust CLI that talks HID++ directly to Logitech hardware on Windows. No G HUB dependency at runtime.
Research docs live one level up: [`../protocol.md`](../protocol.md) (cited as §N) and `../reference/`.
Tags: `[OBS]` seen in G HUB, `[PUB]` public HID++ knowledge (Solaar/libratbag), `[INF]` inferred, `[UNK]` unknown, `[HW]` verified on hardware here.

## 1. Status

**Hardware used:**
- PRO X SUPERLIGHT 2 ("SL2"):
  - over the LIGHTSPEED receiver `c54d` (selector `c54d:1`);
  - over its USB cable as `c09b`.
- PRO X Wireless headset `0aba` over USB (its own dongle).

| Command | State |
|---|---|
| `devices` | ✅ direct devices, receivers, and paired slots with online state |
| `info <dev>` | ✅ protocol version, name, type, firmware entities, full feature table |
| `battery <dev>` | ✅ SL2 via 0x1004 (receiver and cable). The headset has no 0x1000/1001/1004 (it would need 0x1F20, not implemented) |
| `dpi <dev> [v]` | ✅ read via 0x2201/0x2202. ✅ write via 0x2202 with read-back in host mode, over the cable and the receiver. 0x2201 write is unverified (no device) |
| `rate <dev> [hz]` | ✅ 0x8061 read/write with read-back in host mode, over the cable and the receiver. The Hz mapping is [PUB] and unmeasured |
| `mode <dev> [host\|onboard]` | ✅ 0x8100 get/set with read-back, over the cable and the receiver |
| `watch` | ✅ event-driven monitor: device arrival/removal, receiver slot connect/disconnect, DPI/rate/battery events. Idle CPU 0, one thread. Physical plug/unplug and power-off are not yet exercised (TEST REQUEST 5) |
| `raw <dev> <hex…> --allow-raw` | implemented, debug only |
| `--trace` | prints every sent/received report in hex; reports that don't match the pending request are marked `(ignored)` |

**MVP done** (2026-10-15). The full chain works on hardware, and the human confirmed the physical effect: the cursor was noticeably slower at 400 DPI and normal at 800 (SL2, USB cable).

## 2. Protocol facts used by the code

### Architecture
Single binary crate, no traits, no async:
- `src/hid.rs`: SetupDi enumeration, HidP caps, CreateFileW (shared R/W, overlapped).
  - One persistent overlapped read per collection, cancelled and drained on drop.
  - `wait_any` is a single `WaitForMultipleObjects` over all collections plus an optional extra event.
  - `Notifier` wraps `CM_Register_Notification` for HID interface arrival/removal.
- `src/hidpp.rs`: framing, swid, reply matching, error decoding, retries, queue of unsolicited reports, Root/ping, feature names.
- `src/device.rs`: interface grouping, receiver slots, identity, battery, DPI, report rate, onboard mode, event decoding.
- `src/watch.rs`: the `watch` loop.
- `src/main.rs`: argument parsing and output.

**Threads.** Our code runs on one thread. Windows runs the `CM_Register_Notification` callback on a thread-pool thread. The callback only calls `SetEvent`, and the main thread waits on that event; no window or message loop is needed. While idle the process has 1 thread [HW, see §8].

### Transport
- **Collection selection.** We accept page `FF00` usage 1 (7 B) or 2 (20 B), and page `FF43` where usage `HHLL` has LL a single bit and `HH&LL≠0`. [OBS §9.2]
  - We also require the length to match a HID++ report, which excludes the headset's 64-byte `FF00/0001` collection. [INF]
  - [HW] receiver c54d `mi_02` and SL2 wired c09b `mi_02` both have `FF00/0001 in=7 out=7` and `FF00/0002 in=20 out=20`.
  - [HW] headset 0aba `mi_03` has `FF43/0202 in=20 out=20`. That is the collection G HUB opens.
- **Grouping.** Collections of one interface are grouped by path with `&colNN` and the last `&NNNN` of the instance removed. [HW]
- **Write.** Each message goes to the first collection whose output length is at least the message length. It is zero-padded and byte 0 is rewritten to that collection's report ID. [OBS §9.3] [HW]
- **Reads.** We read both collections. [HW]
  - The SL2 answers short (`10`) requests with long (`11`) reports, over both the receiver and the cable.
  - Receiver HID++ 1.0 short register replies come back as `10`.
- **Share mode** `FILE_SHARE_READ|WRITE` works while G HUB runs. [HW]
- **Every open handle receives every input report.** A `watch` process sees the events caused by another process's writes. [HW]

### Request/response
- **Software ID.** We use swid **0x03** (`--swid` overrides). G HUB uses 0x0A..0x0E [OBS §9.4]. No collision was seen with G HUB running. [HW]
- **Matching.** We compare `rsp[1..3]==cmd[1..3]` plus per-call echo offsets. The report ID is ignored and replies must be at least 5 bytes. [OBS §9.5]
  - Echo offsets: ping byte 6; 0x2201 f1/f2 and 0x2202 f5/f6 byte 4; 0x2202 f2 bytes 4–6; B5 sub-address byte 4. [OBS dec_all.c FUN_140ccee80, FUN_140cd04e0, FUN_140cd0df0, FUN_140cd0ff0]
- **Unmatched reports** arriving during a transaction (events, other software's replies) are queued (max 64) for `watch`.
- **Errors.** [OBS §9.5]
  - HID++ 2.0: `rsp[2]==FF`, code in `rsp[5]`.
  - HID++ 1.0: `rsp[2]==8F`, code in `rsp[5]`; code names [PUB].
  - [HW] the receiver answers a 2.0 ping with `10 FF 8F 00 13 01 00`. This is how we detect a receiver. [OBS §9.6 NotHpp20]
  - [HW] empty pairing slots answer `10 FF 8F 83 B5 09 00`.
- **Timeout and retries.**
  - 2000 ms per attempt.
  - **2 retries** on timeout or Busy (0x08), pausing 100 + (10 << attempt) ms. G HUB uses 3000 ms, 9 retries and `rand()%500 + (10<<attempt)` ms [OBS §9.6]; dropping the random part is [INF].
  - Resending is safe here because every request is a read or a write of an absolute value.
  - Motivation [HW]: the first request after the SL2 had idled (asleep) timed out at 1000 ms.
  - Receiver slot pings in `devices` use a 600 ms timeout per attempt.
- **Duplicated reply.** Seen once on the SL2 right after wake. The second copy no longer matches a pending request and is ignored. [HW]

### Receiver (HID++ 1.0, device index 0xFF)
- **Reg 0x02 read.** `-> 10 FF 81 02 00 00 00`, `<- 10 FF 81 02 00 01 00`. `p[1]` = 1 connected device. [OBS §9.8 bytes; PUB meaning; HW]
- **Reg 0x00 read** (notification flags; read only, never written). [HW]
  - `-> 10 FF 81 00 00 00 00`, `<- … 00 01 00`.
  - [PUB Solaar] 0x000100 = wireless notifications enabled. Someone (the firmware default or G HUB) already set it, so we do not need to write it.
  - The bits G HUB itself writes are [UNK] (§9.8).
- **Pairing info.** `-> 10 FF 83 B5 20 00 00`, `<- 11 FF 83 B5 20 08 08 40 A9 11 00 02 B1 01 00 00 7F 24 00 00`. [OBS §9.8 request; PUB layout; HW]
  - `p[3..5]` = wireless PID 0x40A9 (big-endian); `p[7]&0x0F` = 2 = mouse.
- **Online check.** Root ping to device index `slot+1`. [HW]

### Events decoded by `watch`
HID++ 2.0 events have swid 0 and fn 0 on a known feature index [OBS §9.5].
- **0x41 connection** [OBS §9.8]: `rsp[1]` = device index, `rsp[3]` = protocol, `rsp[4]` bits 0–3 = type, **bit 6 = link not established**, `rsp[5..6]` = wireless PID little-endian. **Not yet seen on hardware** (TEST REQUEST 5).
- **0x40 with `rsp[3]==0x02`**: unpaired or disconnected [OBS §9.8]. Not yet seen on hardware.
- **0x1004 battery event**: same layout as f1 [OBS §10.5]. Not yet seen on hardware.
- **0x2202 DPI event** `[sensor, X, Y, lod]`. [HW] `11 FF 09 00 00 06 40 06 40 02` (cable); `11 01 09 00 00 03 20 03 20 02` (receiver), seen by `watch` while another process wrote DPI.
- **0x8061 rate event** `[connType, bit]`. [HW] `11 FF 0C 00 00 02` (cable); `11 01 0C 00 01 03` (receiver).

### Identity (HID++ 2.0)
- **Root f1 ping.** `10 idx 00 1s 00 00 5A` → `[major, minor, echo]`. [OBS §9.6] [HW: SL2 4.2, headset 4.2]
- **Root f0 GetFeature** `[idHi, idLo]` → `[index, flags, version]`. [OBS §9.7] [HW]
- **0x0001 FeatureSet.** f0 → count (excludes Root); f1 `[i]` → `[idHi, idLo, flags, version]`. [OBS §9.7] [HW]
  - The SL2 has 32 features with the same indices on receiver and cable: 0x1004 at 6, 0x2202 at 9, 0x8061 at 12, 0x8100 at 13.
- **0x0005 DeviceName.** f0 → length; f1 `[offset]` → chars; f2 → type (3 mouse, 8 headset [PUB]). [OBS §9.7] [HW]
- **0x0003 FirmwareInfo.** [OBS §9.7 + PUB layout] [HW]
  - f0 → `[count, unitId(4), transport(2), modelId(6), …]`. SL2 model IDs are 40A9 (wireless) and C09B (wired).
  - f1 `[e]` → `[type&0x0F, prefix(3 ASCII), number, revision, build BE16, active, transportPid BE16, …]`. SL2: bootloader BL1 71.05 B0029, main MPM 32.05 B0029.
  - **Agent cross-check:** the headset decodes to `U112.01.B0203`; the agent reports `U112` / `112.1.203`. ✅

### Battery
- **0x1004 f1** → `[soc %, level bits (b0 crit, b1 low, b2 good, b3 full), charging, external]`. [OBS §10.5 order, PUB bytes] [HW SL2]
  - Readings: 13 % (receiver, unplugged), then 28 % and 38 % charging on the cable, then 49 % discharging on the receiver.

### DPI
- **0x2201** (layout from G HUB; no device here has it). [OBS §9.9; FUN_140ccee80 / 140ccf0e0 / 140ccf280 / 140ccf450]
  - f1 `[sensor]` list; f2 `[sensor]` → `[sensor, cur, def]`.
  - f3 `[sensor, dpi BE16]`, plus `dpiLevel` if version ≥ 2 (meaning [UNK], we send 0). UNVERIFIED.
- **0x2202 v0** on the SL2 (index 9).
  - **f2 ranges `[sensor, dir, page]`.** [OBS FUN_140cd04e0] [HW]
    - Each reply echoes 3 bytes, then carries 13 bytes of a BE16 stream. **Values straddle pages**, so pages are concatenated.
    - `0xE0xx` = step xx (low byte); `0x0000` ends the list.
    - SL2: 100–200/1, 200–500/2, 500–1000/5, 1000–2000/10, 2000–5000/20, 5000–10000/50, 10000–20000/100, 20000–32000/125, 32000–44000/200.
    - Raw pages:
      ```
      11 01 09 23 00 00 00 00 64 E0 01 00 C8 E0 02 01 F4 E0 05 03
      11 01 09 23 00 00 01 E8 E0 0A 07 D0 E0 14 13 88 E0 32 27 10
      11 01 09 23 00 00 02 E0 64 4E 20 E0 7D 7D 00 E0 C8 AB E0 00
      11 01 09 23 00 00 03 00 00 …
      ```
  - **f5 get `[sensor]`** → **`[sensor, X, defaultX, Y, defaultY (BE16), lod]`**. [HW; byte positions OBS FUN_140cd0df0; field order corrected, see §3]
  - **f6 set `[sensor, xH, xL, yH, yL, lod]`**. We send X=Y=value and keep LOD. [OBS FUN_140cd0ff0] [HW cable + receiver]
    - Onboard mode: refused with **NotAllowed**.

### Report rate
- **0x8061 v0** on the SL2 (index 12). [OBS dec_all.c FUN_140cf3af0 / 140cf3c80 / 140cf3dd0 / 140cf3f60] [HW]
  - **`connType`: 0 = wired, 1 = wireless.** [HW]
    - f0 `[0]` → `00 0F`; f0 `[1]` → `00 7F`.
    - Events carry 00 over the cable and 01 over the receiver.
    - We pick connType 1 for devices behind a receiver and 0 for direct devices [INF; a direct Bluetooth device would need another rule].
  - **f0** GetDeviceCapabilities `[connType]` → reply[1] = rate bitmap for that connection type. Unused by the code.
  - **f1** GetActualReportRateList `[]` → reply[1] = bitmap for the **active** connection. Cable `0F` (125–1000 Hz); receiver `7F` (125–8000 Hz).
  - **f2** GetReportRate `[connType]` → reply[0] = bit number **of that connection type's rate**. [HW]
    - Over the receiver with wireless set to 500 Hz, f2 `[0]` → `03` (the wired setting) while f2 `[1]` → `02`.
    - So f2 must be asked for the active type. Before this fix, a read-back over the receiver falsely reported a mismatch.
  - **f3** SetReportRate `[bit]` sets the active connection's rate (in host mode). In onboard mode it is refused with **InvalidArgument**.
  - **Bit → Hz** = 125 << bit [PUB Solaar]. The real polling rate has not been measured (TEST REQUEST 4).

### Onboard profiles
- **0x8100** (SL2 index 13). [HW cable + receiver]
  - f2 get mode → `p[0]`: 1 = onboard [OBS §9.9], 2 = host [PUB].
  - f1 set `[1|2]`: G HUB sends `onboard ? 1 : 2` [OBS FUN_140d0e970].
  - Returning to onboard mode makes the device apply its profile DPI and rate again; it emits DPI and rate events.

## 3. Corrections to protocol.md
- **§9.9, 0x2202 f5 field order.** The reply is `[sensor, X, defaultX, Y, defaultY, lod]`. [HW]
- **§9.9, 0x2202 f2.** The range stream continues across pages at byte granularity (13 bytes per page). [HW + OBS]
- **§9.9, 0x2201/0x2202 step.** G HUB takes the step from the low byte of `0xE0xx` only. [OBS]
- **§9.9, 0x2202 f6 / 0x8061 f3.** Refused in onboard mode with NotAllowed and InvalidArgument respectively; both work in host mode. [HW]
- **§9.9, 0x8061.**
  - f0 `[connType]` returns the rate bitmap for that connection type (0 wired, 1 wireless).
  - f1 takes no parameter and returns the active connection's bitmap.
  - **f2's result is per connType**, not global. [HW]
- **§10.7, PRO X Wireless headset (0aba).** It has **no** 0x1000/0x1001/0x1004. Battery voltage must come from **0x1F20 ADCMeasurement** (layout [UNK]). [HW]
- **§9.10 vs. the headset.** 0aba speaks plain HID++ 2.0 (protocol 4.2) on `FF43/0202`, not Centurion framing. [HW]
- **New device** missing from G HUB 2023.1's plaintext DB: receiver **c54d** + PRO X SUPERLIGHT 2, wireless PID **0x40A9**, wired PID **0xC09B**. [HW]

## 4. Dependencies
| Crate | Why | Transitive |
|---|---|---|
| `windows-sys 0.61` (Foundation, HumanInterfaceDevice, DeviceAndDriverInstallation, Storage_FileSystem, System_IO, System_Threading, Security) | Raw Win32 bindings for SetupDi/HidD/HidP/CreateFileW/overlapped I/O/CM notifications. std has none of these. | +1 (`windows-link`) |

**Rejected `hidapi`:** it builds C via `cc`, hides share mode and overlapped timeout control, and has no hot-plug API.

No `clap`, `serde`, logging crates or async runtime. Dev-only research tool: `tools/agent.ps1` (PowerShell, GET-only agent client, not part of the crate).

## 5. Tests run
| Date | Device | Connection | Test | Result |
|---|---|---|---|---|
| 2026-10-15 | LIGHTSPEED receiver c54d | USB | reg 0x02, B5 pairing 0..5, ping | ✅ 1 connected; slot 0 = 0x40A9 mouse; slots 1–5 error 0x09 |
| 2026-10-15 | SL2 (0x40A9) | LIGHTSPEED | ping, name, type, firmware, feature set | ✅ |
| 2026-10-15 | SL2 | LIGHTSPEED | battery 0x1004 | ✅ read 13 % |
| 2026-10-15 | SL2 | LIGHTSPEED | DPI set 0x2202 f6 → 1600 (onboard mode) | ⛔ NotAllowed; unchanged |
| 2026-10-15 | SL2 (c09b) | USB cable | mode host → DPI 1600 → read-back → 800 → onboard | ✅ |
| 2026-10-15 | SL2 (c09b) | USB cable | human run of TEST REQUEST 2: host → 400 → 800 → onboard | ✅ read-backs matched; **cursor noticeably slower at 400, normal at 800 (human-confirmed)** |
| 2026-10-15 | SL2 (c09b) | USB cable | battery while charging | ✅ 28 % then 38 %, charging, wired |
| 2026-10-15 | SL2 (c09b) | USB cable | report rate: onboard set (refused), host → 500 → 1000 → onboard | ✅ |
| 2026-10-15 | SL2 | LIGHTSPEED | `watch` while another process ran host → DPI 1200 → 800 → onboard | ✅ DPI write read back over the receiver; `watch` printed both DPI events and the rate event on return to onboard |
| 2026-10-15 | SL2 | LIGHTSPEED | report rate with connType fix: host → 500 → 1000 → onboard | ✅ read-back 500 then 1000; wireless bitmap 125–8000 Hz; final state onboard, 1000 Hz |
| 2026-10-15 | SL2 | LIGHTSPEED | `watch` idle 30 s | ✅ 0 ms CPU, 1 thread, 5.6 MB working set |
| 2026-10-15 | headset 0aba | USB (own dongle) | ping, name, type, firmware, features | ✅ firmware matches agent |
| — | — | — | `cargo test`: 20 offline byte-exact tests (fixtures from §9 and the [HW] traces above) | ✅ |

All tests ran with G HUB running. G HUB does not manage the SL2.

## 6. Pending test requests

**TEST REQUEST 1: SL2 battery.** Readings so far: 13 → 28 → 38 % charging, then 49 % on the receiver.
- What to observe: are these plausible, compared with the mouse's own indicator or another tool?

**TEST REQUEST 3: physical DPI check over the receiver.** The read-back is already verified [HW].
- Commands, with `c54d:1`: `mode … host`, `dpi … 400`, `dpi … 800`, `mode … onboard`.
- What to observe: the cursor is slower at 400.

**TEST REQUEST 4: real polling rate (optional).**
- Commands: host mode, then `rate <dev> 125` and measure with a polling-rate checker, then `rate <dev> 1000` and measure, then onboard.
- Expected: about 125 Hz, then about 1000 Hz. This would turn the [PUB] bit → Hz mapping into [HW].

**TEST REQUEST 5: hot-plug and wireless connect/disconnect.** G HUB can stay open.
- Start `lghub-lite watch` in a terminal and leave it running, then:
  1. Turn the mouse off with its switch, wait 5 s, and turn it on again.
  2. Plug the USB cable into the mouse, wait, then unplug it.
  3. Unplug the receiver, wait, then plug it back in.
  4. Leave the mouse idle for a few minutes, then move it.
- Expected output:
  - step 1: `slot 1: link lost …`, then `slot 1: connected …` and `+ c54d:1 PRO X 2 …`;
  - steps 2–3: `- … removed` and `+ …` lines;
  - battery events whenever the level changes.
- Please send the output. The 0x41/0x40 layouts are only [OBS] so far.
- Restore: nothing is written.

## 7. Open questions
- Does host mode (0x8100 = 2) persist across a power cycle or a cable unplug? [UNK]
- **0x8061:** the bit → Hz mapping is only [PUB] (TEST REQUEST 4). How should connType be chosen for direct Bluetooth devices? [UNK]
- **Sleep:** what does a receiver answer for a paired device that is off: timeout or an error code? [UNK] This determines how long `devices` takes with the mouse off (up to 3 attempts × 600 ms per slot).
- **0x1F20 ADCMeasurement** reply layout for headset battery [UNK]. G HUB maps mV through the depot XML curve (§10.6).
- **B5 pairing reply:** bytes p[1], p[2], p[5..] not decoded.
- **Unknown SL2 feature IDs:** 0x2251, 0x80E0, 0x1602, plus the engineering IDs 0x1803/0x1817/0x1830/0x1875/0x1861/0x1E02/0x1E22/0x1EB0/0x18B1.
- **`watch` limit:** it waits on at most 63 collections (the `WaitForMultipleObjects` limit). That is far beyond any realistic setup.
- **Agent `GET /routes`** (§14 #1, 2026-10-15): 457 route entries, 305 unique paths.
  - The 37 not in `agent_routes.txt` are `%s` templates instantiated with `dev00000000`.

## 8. Metrics (2026-10-15, release: opt-level "s", LTO, codegen-units 1, panic=abort, strip)
| Metric | Value |
|---|---|
| `lghub-lite.exe` size | **202,240 B** with `watch` (191,488 B before; opt-level 3 measured earlier at 201,728 B vs 184,832 B, so we keep "s") |
| Crates in tree | 3 (`lghub-lite`, `windows-sys`, `windows-link`) |
| `devices` wall time | median 105 ms, min 101 ms (10 runs; SL2 wired + headset, includes HID++ round trips) |
| `collections` wall time (enumeration only) | median 49 ms |
| `watch` idle (SL2 on receiver + headset; 30 s after start-up) | **0 ms CPU** (below the 15.6 ms timer resolution), **1 thread**, working set 5.6 MB, private 0.9 MB |
