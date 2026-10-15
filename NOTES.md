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
| `dpi <dev> [v]` | ✅ read via 0x2201/0x2202. ✅ write via 0x2202 with read-back (SL2 wired, host mode). 0x2201 write is unverified (no device) |
| `rate <dev> [hz]` | ✅ 0x8061 read and write with read-back (SL2 wired, host mode). The Hz mapping is [PUB] and unmeasured |
| `mode <dev> [host\|onboard]` | ✅ 0x8100 get/set with read-back (SL2 wired) |
| `raw <dev> <hex…> --allow-raw` | implemented, debug only |
| `--trace` | prints every sent/received report in hex; reports that don't match the pending request are marked `(ignored)` |

**MVP done** (2026-10-15). The full chain (discover → connect → identify → features → read → write → read-back) works on hardware, and the human confirmed the physical effect: the cursor was noticeably slower at 400 DPI and back to normal at 800 (SL2, USB cable).
- Still untested: a DPI write over the LIGHTSPEED receiver in host mode.

## 2. Protocol facts used by the code

### Architecture
Single binary crate, no traits, no async, no threads:
- `src/hid.rs`: SetupDi enumeration, HidP caps, CreateFileW (shared R/W, overlapped). One persistent overlapped read per collection, `WaitForMultipleObjects` timeouts, cancel and drain on drop.
- `src/hidpp.rs`: framing, swid, reply matching, error decoding, Root/ping, feature names.
- `src/device.rs`: interface grouping, receiver slots, identity, battery, DPI, onboard mode.
- `src/main.rs`: argument parsing and output.

### Transport
- **Collection selection.** We accept page `FF00` usage 1 (7 B) or 2 (20 B), and page `FF43` where usage `HHLL` has LL a single bit and `HH&LL≠0`. [OBS §9.2]
  - We also require the length to match a HID++ report, which excludes the headset's 64-byte `FF00/0001` collection. [INF]
  - [HW] receiver c54d `mi_02` and SL2 wired c09b `mi_02` both have `FF00/0001 in=7 out=7` and `FF00/0002 in=20 out=20`.
  - [HW] headset 0aba `mi_03` has `FF43/0202 in=20 out=20`. That is the collection G HUB opens, matching agent `/devices/list`.
- **Grouping.** Collections of one interface are grouped by path with `&colNN` and the last `&NNNN` of the instance removed. [HW]
- **Write.** Each message goes to the first collection whose output length is at least the message length. It is zero-padded and byte 0 is rewritten to that collection's report ID. [OBS §9.3] [HW]
  - The headset only has a 20-byte collection, so short requests go out as `11 …`; it answers correctly.
- **Reads.** We read both collections. [HW]
  - **The SL2 answers short (`10`) requests with long (`11`) reports**, both over the receiver and over the cable.
  - Receiver HID++ 1.0 short register replies come back as `10`.
- **Share mode** `FILE_SHARE_READ|WRITE` works while G HUB runs. [HW]

### Request/response
- **Software ID.** We use swid **0x03** (`--swid` overrides). G HUB uses 0x0A..0x0E [OBS §9.4]. No collision was seen with G HUB running. [HW]
- **Matching.** We compare `rsp[1..3]==cmd[1..3]` plus per-call echo offsets. The report ID is ignored and replies must be at least 5 bytes. [OBS §9.5]
  - Echo offsets: ping byte 6; 0x2201 f1/f2 and 0x2202 f5/f6 byte 4; 0x2202 f2 bytes 4–6; B5 sub-address byte 4. [OBS dec_all.c FUN_140ccee80, FUN_140cd04e0, FUN_140cd0df0, FUN_140cd0ff0]
- **Events** (swid 0) are never taken as replies. [OBS §9.5] [HW]
  - After a DPI set, the SL2 emits `11 FF 09 00 00 06 40 06 40 02`: 0x2202 event `[sensor, X, Y, lod]`.
  - After switching back to onboard, it emits `11 FF 0C 00 00 03 …`: an 0x8061 event at index 12, meaning [UNK].
- **HID++ 2.0 error.** `rsp[2]==FF`, `rsp[3]==cmd[2]`, `rsp[4]==cmd[3]`, code in `rsp[5]`. [OBS §9.5]
  - [HW] `11 01 FF 09 63 05` = NotAllowed.
- **HID++ 1.0 error.** `rsp[2]==8F`, code in `rsp[5]`. [OBS §9.5] Code names are [PUB].
  - [HW] the receiver answers a 2.0 ping with `10 FF 8F 00 13 01 00`. This is how we detect a receiver. [OBS §9.6 NotHpp20]
  - [HW] empty pairing slots answer `10 FF 8F 83 B5 09 00` (0x09 resource error).
- **Duplicated reply.** Seen once on the SL2 right after wake: the same ping reply arrived twice. The second copy no longer matches a pending request and is ignored. [HW]
- **Timeout.** 2000 ms per request; G HUB uses 3000 ms with 9 retries [OBS §9.6].
  - [HW] 1000 ms was too short for the first request after the SL2 had idled (asleep). No retries are implemented yet.

### Receiver (HID++ 1.0, device index 0xFF)
- **Reg 0x02 read.** `-> 10 FF 81 02 00 00 00`, `<- 10 FF 81 02 00 01 00`. `p[1]` = 1 connected device. [OBS §9.8 bytes; PUB meaning; HW]
- **Pairing info.** `-> 10 FF 83 B5 20 00 00`, `<- 11 FF 83 B5 20 08 08 40 A9 11 00 02 B1 01 00 00 7F 24 00 00`. [OBS §9.8 request; PUB layout; HW]
  - `p[3..5]` = wireless PID **0x40A9** (big-endian).
  - `p[7]&0x0F` = 2 = mouse.
  - Other bytes are not decoded.
  - The 0x41 notification carries the PID little-endian (§9.8); the B5 reply is big-endian.
- **Online check.** Root ping to device index `slot+1`. [HW]

### Identity (HID++ 2.0)
- **Root f1 ping.** `10 idx 00 1s 00 00 5A` → `[major, minor, echo]`. [OBS §9.6] [HW: SL2 4.2, headset 4.2]
- **Root f0 GetFeature** `[idHi, idLo]` → `[index, flags, version]`. [OBS §9.7] [HW]
- **0x0001 FeatureSet.** f0 → count (excludes Root); f1 `[i]` → `[idHi, idLo, flags, version]`. [OBS §9.7] [HW]
  - Flags bit 7 obsolete, 6 hidden, 5 engineering [PUB].
  - The SL2 has 32 features with the same indices on receiver and cable: 0x2202 at 9, 0x8061 at 12, 0x8100 at 13, 0x1004 at 6.
- **0x0005 DeviceName.** f0 → length; f1 `[offset]` → up to 16 chars; f2 → type (3 mouse, 8 headset [PUB]). [OBS §9.7] [HW]
  - SL2 = "PRO X 2". Headset = "PRO X Wireless Gaming Headset" (2 chunks).
- **0x0003 FirmwareInfo.** [OBS §9.7 + PUB layout] [HW]
  - f0 → `[count, unitId(4), transport(2), modelId(6), extModel, caps…]`. SL2 returns `02 7A F4 AD 28 00 0C 40 A9 C0 9B 00 00 00 01`, which gives model IDs 40A9 (wireless) and C09B (wired).
  - f1 `[e]` → `[type&0x0F, prefix(3 ASCII), number, revision, build BE16, active, transportPid BE16, …]`. SL2 returns:
    - `01 42 4C 31 71 05 00 29 00 AB 1C…` = bootloader BL1 71.05 B0029;
    - `00 4D 50 4D 32 05 00 29 01 40 A9…` = main MPM 32.05 B0029.
  - **Agent cross-check:** the headset decodes to `U112.01.B0203`; the agent reports `firmwareName U112`, `firmwareVersion 112.1.203`. ✅

### Battery
- **0x1004 f1** → `[soc %, level bits (b0 crit, b1 low, b2 good, b3 full), charging, external]`. [OBS §10.5 order, PUB bytes] [HW SL2]
  - Over the receiver: `<- 11 01 06 13 0D 01 00 00` = 13 %, critical, discharging.
  - Over the cable, minutes later: `<- 11 FF 06 13 1C 02 01 01` = 28 %, low, charging, external wired.
  - The jump from 13 to 28 % on plugging in is unexplained, and the physical value is unconfirmed (TEST REQUEST 1).

### DPI
- **0x2201** (layout from G HUB; no device here has it). [OBS §9.9; FUN_140ccee80 / 140ccf0e0 / 140ccf280 / 140ccf450]
  - f1 `[sensor]` → `[sensor, BE16 stream]`.
  - f2 `[sensor]` → `[sensor, cur BE16, def BE16]`.
  - f3 `[sensor, dpi BE16]`, plus a `dpiLevel` byte if version ≥ 2. dpiLevel meaning [UNK]; we send 0. UNVERIFIED.
- **0x2202 v0** on the SL2 (index 9).
  - **f2 ranges `[sensor, dir, page]`.** [OBS FUN_140cd04e0] [HW]
    - Each reply echoes 3 bytes, then carries 13 bytes of a BE16 stream. **Values straddle pages**, so pages are concatenated.
    - `0xE0xx` = step xx (low byte); `0x0000` ends the list. `dir` 0 = X axis [INF].
    - SL2 pages 0..3:
      ```
      11 01 09 23 00 00 00 00 64 E0 01 00 C8 E0 02 01 F4 E0 05 03
      11 01 09 23 00 00 01 E8 E0 0A 07 D0 E0 14 13 88 E0 32 27 10
      11 01 09 23 00 00 02 E0 64 4E 20 E0 7D 7D 00 E0 C8 AB E0 00
      11 01 09 23 00 00 03 00 00 …
      ```
    - Decoded: 100–200/1, 200–500/2, 500–1000/5, 1000–2000/10, 2000–5000/20, 5000–10000/50, 10000–20000/100, 20000–32000/125, 32000–44000/200.
  - **f5 get `[sensor]`** → **`[sensor, X, defaultX, Y, defaultY (BE16), lod]`**. [HW; byte positions OBS FUN_140cd0df0, field order corrected — see §3]
    - After setting 1600/1600: `<- 11 FF 09 53 00 06 40 03 20 06 40 03 20 02`.
  - **f6 set `[sensor, xH, xL, yH, yL, lod]`**. We send X=Y=value and keep LOD. [OBS FUN_140cd0ff0] [HW]
    - Host mode: `-> 11 FF 09 63 00 06 40 06 40 02`, `<- 11 FF 09 63 00 06 40 06 40 02` (echo), read-back 1600.
    - Onboard mode: `<- 11 01 FF 09 63 05` (**NotAllowed**).
### Report rate
- **0x8061 v0** on the SL2 (index 12). [OBS dec_all.c FUN_140cf3af0 / 140cf3c80 / 140cf3dd0 / 140cf3f60] [HW SL2 wired]
  - f1 GetActualReportRateList `[]` → reply[1] = bitmap of supported rates. `<- 11 FF 0C 13 00 0F` (bits 0–3).
  - f2 GetReportRate `[connType]` → reply[0] = active bit number. `-> 10 FF 0C 23 00`, `<- 11 FF 0C 23 03`.
    - `connType` meaning [UNK]; we send 0, as only 0 has been tried.
  - f3 SetReportRate `[bit number]`.
    - Host mode: `-> 10 FF 0C 33 02`, `<- 11 FF 0C 33 00`; f2 then reads back 2.
    - Onboard mode: `<- 11 FF FF 0C 33 02` (**InvalidArgument**, not NotAllowed).
  - Event (fn 0, swid 0): `11 FF 0C 00 00 <bit>`, so the new rate bit is in byte 5. Seen after a set (`02`) and after returning to onboard (`03`).
  - f0 GetDeviceCapabilities `[connType]` echoes connType and returns a byte at reply[1]. It returned `0F` too; its meaning [UNK], unused.
  - **Bit → Hz** = 125 << bit (8 ms … 125 µs) [PUB Solaar]. Over the cable the SL2 offers 125/250/500/1000 Hz and defaults to 1000. The real polling rate has not been measured.

- **0x8100 OnboardProfiles** (SL2 index 13). [HW]
  - f2 get mode → `p[0]`: 1 = onboard [OBS §9.9], 2 = host [PUB].
  - f1 set mode `[1|2]`: G HUB sends `onboard ? 1 : 2` [OBS FUN_140d0e970].
  - [HW] `-> 10 FF 0D 13 02 00 00`, `<- 11 FF 0D 13 00 …`, mode then reads `02`. `-> 10 FF 0D 13 01` restores `01`.
  - Returning to onboard mode makes the device apply its profile DPI again (event `11 FF 09 00 00 03 20 03 20 02`).

## 3. Corrections to protocol.md
- **§9.9, 0x2202 f5 field order.** The reply is `[sensor, X, defaultX, Y, defaultY, lod]`, not X, Y, defX, defY. [HW SL2: X=Y=1600 written, reply `00 06 40 03 20 06 40 03 20 02`]
- **§9.9, 0x2202 f2.** The range stream continues across pages at byte granularity (13 bytes per page). [HW + OBS FUN_140cd04e0 carries the parity across pages]
- **§9.9, 0x2201/0x2202 step.** G HUB takes the step from the low byte of `0xE0xx` only. [OBS]
- **§9.9, 0x2202 f6.** Refused with NotAllowed while 0x8100 reports onboard mode; works in host mode. [HW]
- **§9.9, 0x8061.** f3 is refused with **InvalidArgument** (not NotAllowed) while in onboard mode; works in host mode. f1 takes no parameter; f2's reply is the bit number at byte 4. [HW]
- **§10.7, PRO X Wireless headset (0aba).** It has **no** 0x1000/0x1001/0x1004. Its features are 0000, 0001, 0003, 0005, 8310, 8300 and **1F20 ADCMeasurement**, so battery voltage must come from 0x1F20 (layout [UNK]). [HW]
- **§9.10 vs. the headset.** 0aba speaks plain HID++ 2.0 (protocol 4.2) on `FF43/0202`, not Centurion framing. [HW]
- **New device** missing from G HUB 2023.1's plaintext DB: receiver **c54d** + PRO X SUPERLIGHT 2, wireless PID **0x40A9**, wired PID **0xC09B**. The agent does not list it. [HW]

## 4. Dependencies
| Crate | Why | Transitive |
|---|---|---|
| `windows-sys 0.61` (Foundation, HumanInterfaceDevice, DeviceAndDriverInstallation, Storage_FileSystem, System_IO, System_Threading, Security) | Raw Win32 bindings for SetupDi/HidD/HidP/CreateFileW/overlapped I/O. std has no HID or overlapped I/O. | +1 (`windows-link`) |

**Rejected `hidapi`:** it builds C via `cc`, hides share mode and overlapped timeout control, and needs its own per-collection handling for Windows anyway.

No `clap`, `serde`, logging crates or async runtime. Dev-only research tool: `tools/agent.ps1` (PowerShell, GET-only agent client, not part of the crate).

## 5. Tests run
| Date | Device | Connection | Test | Result |
|---|---|---|---|---|
| 2026-10-15 | LIGHTSPEED receiver c54d | USB | reg 0x02, B5 pairing 0..5, ping | ✅ 1 connected; slot 0 = 0x40A9 mouse; slots 1–5 error 0x09 |
| 2026-10-15 | SL2 (0x40A9) | LIGHTSPEED | ping, name, type, firmware, feature set | ✅ |
| 2026-10-15 | SL2 | LIGHTSPEED | battery 0x1004 | ✅ read 13 % (physical check pending) |
| 2026-10-15 | SL2 | LIGHTSPEED | DPI read 0x2202 | ✅ 800, LOD 2 |
| 2026-10-15 | SL2 | LIGHTSPEED | DPI set 0x2202 f6 → 1600 (onboard mode) | ⛔ NotAllowed; unchanged |
| 2026-10-15 | SL2 (c09b) | USB cable | battery | ✅ 28 %, charging, wired |
| 2026-10-15 | SL2 (c09b) | USB cable | mode host → DPI 1600 → read-back → DPI 800 → mode onboard | ✅ every step read back as written; final state = onboard, 800 |
| 2026-10-15 | SL2 (c09b) | USB cable | human run of TEST REQUEST 2 (release build): host → 400 → 800 → onboard | ✅ all read-backs matched; **cursor noticeably slower at 400, normal at 800 (human-confirmed)**; f5 read at 400 shows X=Y=400, default 800 (confirms the §3 field-order fix) |
| 2026-10-15 | SL2 (c09b) | USB cable | battery while charging | ✅ 38 %, good, charging, wired (rising from 28 % while charging; not yet compared against another source) |
| 2026-10-15 | SL2 (c09b) | USB cable | report rate: read; set 500 in onboard mode; host → 500 → read-back → 1000 → onboard | ✅ read 1000 Hz (bitmap 0x0F); onboard set refused (InvalidArgument), unchanged; host set read back 500, restored 1000; final state onboard, 1000 Hz, 800 DPI |
| 2026-10-15 | headset 0aba | USB (own dongle) | ping, name, type, firmware, features | ✅ firmware matches agent |
| — | — | — | `cargo test`: 19 offline byte-exact tests (fixtures from §9 and the [HW] traces above) | ✅ |

All tests ran with G HUB running. G HUB does not manage the SL2.

## 6. Pending test requests

**TEST REQUEST 1: SL2 battery.**
- Command: `lghub-lite battery c09b` (cable) or `lghub-lite battery c54d:1` (receiver). G HUB can stay open.
- Expected output: `<N>% level=… <charging state> external=…`.
- What to observe: does N match what the mouse/another tool shows? Are the 13 % (receiver, unplugged) and 28 % (cable, a few minutes later) readings plausible?
- Restore: nothing changes.

TEST REQUEST 2 (physical DPI check over the cable) passed on 2026-10-15; see §5.

**TEST REQUEST 3: DPI write over the LIGHTSPEED receiver.** Run it when the mouse is unplugged and on the receiver.
- Commands: the same four steps as TEST REQUEST 2, with `c54d:1` in place of `c09b`:
  1. `lghub-lite mode c54d:1 host`
  2. `lghub-lite dpi c54d:1 400`
  3. `lghub-lite dpi c54d:1 800`
  4. `lghub-lite mode c54d:1 onboard`
- Expected output: the same read-backs as over the cable.
- What to observe: the same cursor slow-down at 400.
- Restore: step 4. Whether host mode survives a power cycle is [UNK]; please report what you see.

**TEST REQUEST 4: real polling rate (optional).** Use any mouse polling-rate checker (a web page or tool that reports Hz while you move the mouse quickly).
- Commands, with the mouse on the cable:
  1. `lghub-lite mode c09b host`
  2. `lghub-lite rate c09b 125`
  3. Measure.
  4. `lghub-lite rate c09b 1000`
  5. Measure.
  6. `lghub-lite mode c09b onboard`
- Expected: about 125 Hz at step 3 and about 1000 Hz at step 5.
- This would turn the [PUB] bit → Hz mapping into [HW].
- Restore: step 6.

## 7. Open questions
- Does host mode (0x8100 = 2) persist across a power cycle or a cable unplug? [UNK]
- **0x8061:** the bit → Hz mapping is only [PUB]. Confirming it needs a polling-rate measurement (TEST REQUEST 4). The meaning of `connType` and of f0 [UNK]. Over the receiver the bitmap may include 2000+ Hz bits (untested).
- **0x1F20 ADCMeasurement** reply layout for headset battery [UNK]. G HUB maps mV through the depot XML curve (§10.6).
- **B5 pairing reply:** bytes p[1], p[2], p[5..] not decoded.
- **Unknown SL2 feature IDs** with no name in the G HUB table: 0x2251, 0x80E0, 0x1602, plus the engineering IDs 0x1803/0x1817/0x1830/0x1875/0x1861/0x1E02/0x1E22/0x1EB0/0x18B1.
- **Retry policy:** G HUB retries 9× with backoff (§9.6). We have none yet; the 2 s timeout has been enough so far.
- **Agent `GET /routes`** (§14 #1, 2026-10-15): 457 route entries, 305 unique paths.
  - The 37 paths not in `agent_routes.txt` are its `%s` templates instantiated with `dev00000000`.
  - Mouse DPI routes appear only as BROADCAST (`/mouse/dpi`) because no mouse is attached to G HUB.

## 8. Metrics (2026-10-15, release: opt-level "s", LTO, codegen-units 1, panic=abort, strip)
| Metric | Value |
|---|---|
| `lghub-lite.exe` size | **191,488 B** with `rate` (186,880 B before it; opt-level 3 measured earlier at 201,728 B vs 184,832 B, so we keep "s") |
| Crates in tree | 3 (`lghub-lite`, `windows-sys`, `windows-link`) |
| `devices` wall time | median 105 ms, min 101 ms (10 runs, `Measure-Command`; SL2 wired + headset, includes HID++ round trips) |
| `collections` wall time (enumeration only) | median 49 ms |
| Long-running modes | none yet |
