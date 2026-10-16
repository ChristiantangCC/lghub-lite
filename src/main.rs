use std::path::PathBuf;
use std::time::Duration;

use lghub_lite::device::parse_selector;
use lghub_lite::hidpp::{self, DIRECT, Error, Hidpp};
use lghub_lite::{LOGITECH_VID, audio, autostart, battery, config, device, hid, platform, profiles, verbose, watch};

const USAGE: &str = "\
lghub-lite: control Logitech HID++ mice and headsets without G HUB.
Not affiliated with or endorsed by Logitech.

usage: lghub-lite [--trace] [--verbose] [--swid N] [--config FILE] <command>

  devices                  list HID++ devices, including those behind receivers
  watch                    print plug/unplug, receiver connect/disconnect and setting events;
                           applies presets whenever a device connects
  autostart [on|off]       run the windowless watcher (lghub-lite-bg.exe) at login, or stop it
  presets                  show the preset file
  preset <dev> [dpi=N] [rate=HZ] | clear
                           save (or remove) the preset for this device's model
  apply <dev>              apply the saved preset now (switches the device to host mode)
  profiles <dev>           onboard profile memory: info, active profile, directories, CRCs
  dump <dev> <file>        read every onboard memory sector into a text backup file
  info <dev>               identity, firmware and supported features
  battery <dev>            battery state (headsets: percentage from the voltage and a curve)
  curve <dev> [build]      headset battery curve in use and the voltage recording; `build`
                           makes or refines your measured curve from a recorded discharge
  eq <dev>                 show the headset hardware equalizer (read-only)
  dpi <dev> [value]        read DPI, or set it and read it back
  rate <dev> [hz]          read report rate, or set it and read it back
  mode <dev> [host|onboard] read or set onboard-profile mode (host mode allows DPI/rate writes)
  raw <dev> <hex...>       send one raw report, print replies (needs --allow-raw)
  collections              list every Logitech HID collection (debug)

<dev> is PID[:INDEX] as printed by `devices`, e.g. c54d:1 (receiver slot 1) or 0aba.
Presets live in %APPDATA%\\lghub-lite\\presets.txt unless --config is given.";

struct Opts {
    trace: bool,
    swid: u8,
    allow_raw: bool,
    config: Option<PathBuf>,
}

impl Opts {
    fn config_path(&self) -> Result<PathBuf, Error> {
        self.config
            .clone()
            .or_else(config::default_path)
            .ok_or_else(|| Error::Other("APPDATA is not set; pass --config FILE".into()))
    }

    /// Folder holding the preset file; battery curves and recordings live here too.
    fn curve_dir(&self) -> Result<PathBuf, Error> {
        let p = self.config_path()?;
        Ok(p.parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")))
    }
}

fn main() {
    let mut opts = Opts {
        trace: false,
        swid: hidpp::DEFAULT_SWID,
        allow_raw: false,
        config: None,
    };
    let mut args = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--verbose" | "-v" => lghub_lite::set_verbose(true),
            "--trace" => opts.trace = true,
            "--allow-raw" => opts.allow_raw = true,
            "--swid" => match it
                .next()
                .and_then(|s| s.parse::<u8>().ok())
                .filter(|s| (1..=15).contains(s))
            {
                Some(s) => opts.swid = s,
                None => die("--swid needs a value 1..15"),
            },
            "--config" => match it.next() {
                Some(p) => opts.config = Some(PathBuf::from(p)),
                None => die("--config needs a file path"),
            },
            _ => args.push(a),
        }
    }
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let r = match a.as_slice() {
        ["devices"] => devices(&opts),
        ["watch"] => {
            // a watcher gets its own software ID unless --swid chose one (see WATCH_SWID)
            let swid = if opts.swid == hidpp::DEFAULT_SWID {
                hidpp::WATCH_SWID
            } else {
                opts.swid
            };
            opts.config_path().and_then(|c| watch::run(swid, opts.trace, &c))
        }
        ["autostart"] => autostart_cmd(&opts, None),
        ["autostart", "on"] => autostart_cmd(&opts, Some(true)),
        ["autostart", "off"] => autostart_cmd(&opts, Some(false)),
        ["presets"] => presets(&opts),
        ["preset", d, rest @ ..] => preset(&opts, d, rest),
        ["profiles", d] => with_dev(&opts, d, show_profiles),
        ["dump", d, file] => with_dev(&opts, d, |h, dev| dump(h, dev, std::path::Path::new(file))),
        ["apply", d] => {
            let path = opts.config_path().unwrap_or_else(|e| die(&e.to_string()));
            with_dev(&opts, d, |h, dev| apply(h, dev, &path))
        }
        ["collections"] => collections(),
        ["info", d] => with_dev(&opts, d, info),
        ["battery", d] => {
            let dir = opts.curve_dir().unwrap_or_else(|e| die(&e.to_string()));
            with_dev(&opts, d, |h, dev| battery(h, dev, &dir))
        }
        ["curve", d, rest @ ..] if rest.len() <= 1 && rest.iter().all(|r| *r == "build") => {
            let dir = opts.curve_dir().unwrap_or_else(|e| die(&e.to_string()));
            with_dev(&opts, d, |h, dev| curve(h, dev, &dir, !rest.is_empty()))
        }
        ["eq", d] => with_dev(&opts, d, eq),
        ["dpi", d] => with_dev(&opts, d, |h, dev| dpi(h, dev, None)),
        ["dpi", d, v] => {
            let v: u16 = v.parse().unwrap_or_else(|_| die("DPI value must be a number"));
            with_dev(&opts, d, |h, dev| dpi(h, dev, Some(v)))
        }
        ["rate", d] => with_dev(&opts, d, |h, dev| rate(h, dev, None)),
        ["rate", d, v] => {
            let v: u32 = v.parse().unwrap_or_else(|_| die("report rate must be a number in Hz"));
            with_dev(&opts, d, |h, dev| rate(h, dev, Some(v)))
        }
        ["mode", d] => with_dev(&opts, d, |h, dev| mode(h, dev, None)),
        ["mode", d, "host"] => with_dev(&opts, d, |h, dev| mode(h, dev, Some(device::MODE_HOST))),
        ["mode", d, "onboard"] => with_dev(&opts, d, |h, dev| mode(h, dev, Some(device::MODE_ONBOARD))),
        ["raw", d, bytes @ ..] if !bytes.is_empty() => {
            if !opts.allow_raw {
                die("raw sends arbitrary bytes to hardware; pass --allow-raw to confirm");
            }
            raw(&opts, d, bytes)
        }
        _ => die(USAGE),
    };
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2)
}

fn with_dev(o: &Opts, sel: &str, f: impl FnOnce(&mut Hidpp, u8) -> Result<(), Error>) -> Result<(), Error> {
    let (pid, dev) = parse_selector(sel).ok_or_else(|| Error::Other(format!("bad device selector `{sel}`")))?;
    let mut h = device::open(pid, o.swid, o.trace)?;
    f(&mut h, dev)
}

fn presets(o: &Opts) -> Result<(), Error> {
    let path = o.config_path()?;
    let list = config::load(&path).map_err(Error::Other)?;
    println!("file       {}", path.display());
    if list.is_empty() {
        println!("(no presets)");
    }
    for p in &list {
        println!("{}", config::line(p));
    }
    Ok(())
}

/// `preset <dev> [dpi=N] [rate=HZ]` or `preset <dev> clear`: keyed by the device's model ID,
/// so the same preset applies over the cable and the receiver.
fn preset(o: &Opts, sel: &str, rest: &[&str]) -> Result<(), Error> {
    let path = o.config_path()?;
    let mut list = config::load(&path).map_err(Error::Other)?;
    let mut new = None;
    let mut changed = false;
    with_dev(o, sel, |h, dev| {
        let model = device::model_id(h, dev)?;
        if rest.is_empty() {
            match list.iter().find(|p| p.model == model) {
                Some(p) => println!("preset     {}", config::line(p)),
                None => println!("no preset for model {model:04x}"),
            }
            return Ok(());
        }
        changed = true;
        if rest == ["clear"] {
            list.retain(|p| p.model != model);
            println!("cleared preset for model {model:04x}");
            return Ok(());
        }
        let mut p = config::parse(&format!("{model:04x} {}", rest.join(" ")))
            .map_err(|e| Error::Other(e.replace("line 1: ", "")))?
            .remove(0);
        if let Some(v) = p.dpi {
            let caps = device::dpi(h, dev)?.caps;
            if !caps.allows(v) {
                return Err(Error::Other(format!("{v} DPI is not allowed by the device ({caps})")));
            }
        }
        // Keep fields not given now from the existing preset.
        if let Some(old) = list.iter().find(|x| x.model == model) {
            p.dpi = p.dpi.or(old.dpi);
            p.rate = p.rate.or(old.rate);
        }
        list.retain(|x| x.model != model);
        list.push(p.clone());
        new = Some(p);
        Ok(())
    })?;
    if changed {
        config::save(&path, &list).map_err(Error::Other)?;
    }
    if let Some(p) = new {
        println!("saved      {}", config::line(&p));
        println!("file       {}", path.display());
    }
    Ok(())
}

fn apply(h: &mut Hidpp, dev: u8, path: &std::path::Path) -> Result<(), Error> {
    let model = device::model_id(h, dev)?;
    let list = config::load(path).map_err(Error::Other)?;
    let p = list
        .iter()
        .find(|p| p.model == model)
        .ok_or_else(|| Error::Other(format!("no preset for model {model:04x} in {}", path.display())))?;
    println!("applied    {}", config::apply(h, dev, p)?);
    Ok(())
}

fn collections() -> Result<(), Error> {
    let list = hid::enumerate(LOGITECH_VID).map_err(|e| Error::Io(e.to_string()))?;
    for i in list {
        println!(
            "{:04x}:{:04x} page={:04x} usage={:04x} in={:>2} out={:>2} hidpp={} {:?}\n    {}",
            i.vid,
            i.pid,
            i.usage_page,
            i.usage,
            i.in_len,
            i.out_len,
            device::is_hidpp(&i),
            i.product,
            i.path
        );
    }
    Ok(())
}

fn devices(o: &Opts) -> Result<(), Error> {
    let ifs = device::interfaces()?;
    if ifs.is_empty() {
        println!("no Logitech HID++ interfaces found");
    }
    for f in &ifs {
        let mut h = match Hidpp::open(&f.cols, o.swid, o.trace) {
            Ok(h) => h,
            Err(e) => {
                println!("{:04x}      {}  [cannot open: {e}]", f.pid, f.product);
                continue;
            }
        };
        match h.ping(DIRECT, 0x5A) {
            Ok((maj, min)) => {
                let name = device::name(&mut h, DIRECT).unwrap_or_else(|_| f.product.clone());
                println!("{:04x}      {name}  (HID++ {maj}.{min}, direct)", f.pid);
            }
            Err(Error::Hpp10(_)) => {
                // HID++ 1.0 answer to a 2.0 ping: this is a receiver. [OBS Â§9.6]
                let n = device::connected_count(&mut h)
                    .map(|n| n.to_string())
                    .unwrap_or_else(|e| format!("? ({e})"));
                println!("{:04x}      {}  (receiver, {n} connected)", f.pid, f.product);
                receiver_slots(&mut h, f.pid);
            }
            Err(e) => println!("{:04x}      {}  [no HID++ 2.0 answer: {e}]", f.pid, f.product),
        }
    }
    Ok(())
}

fn receiver_slots(h: &mut Hidpp, pid: u16) {
    for slot in 0..6u8 {
        let p = match device::pairing(h, slot) {
            Ok(p) => p,
            Err(e) => {
                verbose(format_args!("slot {slot}: {e}"));
                continue;
            }
        };
        let idx = slot + 1;
        let sel = format!("{pid:04x}:{idx}");
        h.timeout = Duration::from_millis(600);
        let state = match h.ping(idx, 0x5A) {
            Ok((maj, min)) => {
                let name = device::name(h, idx).unwrap_or_default();
                format!("{name}  (HID++ {maj}.{min}, online)")
            }
            // [HW] the receiver answers 0x8F code 0x08 at once for a paired device whose link is down.
            Err(Error::Hpp10(0x08)) => "(offline: paired, not connected)".to_string(),
            Err(e) => format!("(offline: {e})"),
        };
        h.timeout = Duration::from_millis(2000);
        println!(
            "  {sel:<8} wpid={:04x} kind={}  {state}",
            p.wpid,
            device::kind_name(p.kind)
        );
        verbose(format_args!("    pairing raw: {}", hidpp::hex(&p.raw)));
    }
}

fn info(h: &mut Hidpp, dev: u8) -> Result<(), Error> {
    let (maj, min) = h.ping(dev, 0x5A)?;
    println!("protocol   HID++ {maj}.{min}");
    match device::name(h, dev) {
        Ok(n) => println!("name       {n}"),
        Err(e) => println!("name       ({e})"),
    }
    if let Ok(t) = device::device_type(h, dev) {
        println!("type       {} ({t})", device::device_type_name(t));
    }
    match device::firmware(h, dev) {
        Ok(fw) => {
            for f in fw {
                println!(
                    "firmware   {:<10} {}{:02X}.{:02X}.B{:04X}",
                    device::firmware_kind_name(f.kind),
                    f.prefix,
                    f.number,
                    f.revision,
                    f.build
                );
            }
        }
        Err(e) => println!("firmware   ({e})"),
    }
    println!("features");
    for f in device::features(h, dev)? {
        let mut fl = String::new();
        for (bit, s) in [(7, " obsolete"), (6, " hidden"), (5, " engineering")] {
            if f.flags & (1 << bit) != 0 {
                fl.push_str(s);
            }
        }
        println!(
            "  [{:>2}] {:04X} v{} {}{fl}",
            f.index,
            f.id,
            f.version,
            hidpp::feature_name(f.id)
        );
    }
    Ok(())
}

fn dpi(h: &mut Hidpp, dev: u8, set: Option<u16>) -> Result<(), Error> {
    let d = device::dpi(h, dev)?;
    let y =
        d.y.filter(|&y| y != d.current)
            .map(|y| format!(" y={y}"))
            .unwrap_or_default();
    println!(
        "dpi        {}{y} (default {}) [{:04X} v{}]",
        d.current, d.default, d.feature, d.version
    );
    println!("allowed    {}", d.caps);
    if d.feature == 0x2202 {
        println!("lod        {}", d.lod);
    }
    let mode = device::onboard_mode(h, dev).ok().flatten();
    if let Some(m) = mode {
        println!("profiles   mode {m} ({})", device::mode_name(m));
    }
    let Some(v) = set else { return Ok(()) };
    println!("old={}  (restore with: lghub-lite dpi <dev> {})", d.current, d.current);
    let after = device::set_dpi_verified(h, dev, v).map_err(cli_hint)?;
    println!("new={v} readback={after}");
    Ok(())
}

/// In the CLI, name the command that switches modes.
fn cli_hint(e: Error) -> Error {
    match e {
        Error::Other(s) if s.contains("switch it to host mode first") => {
            Error::Other(s.replace("switch it to host mode first", "run `mode <dev> host` first"))
        }
        e => e,
    }
}

fn rate(h: &mut Hidpp, dev: u8, set: Option<u32>) -> Result<(), Error> {
    let r = device::report_rate(h, dev)?;
    let list: Vec<String> = (0..8u8)
        .filter(|b| r.supported & (1 << b) != 0)
        .map(|b| device::rate_hz(b).to_string())
        .collect();
    println!(
        "rate       {} Hz  [8061, bit {}]",
        device::rate_hz(r.current),
        r.current
    );
    println!("allowed    {} Hz", list.join(", "));
    let mode = device::onboard_mode(h, dev).ok().flatten();
    if let Some(m) = mode {
        println!("profiles   mode {m} ({})", device::mode_name(m));
    }
    let Some(hz) = set else { return Ok(()) };
    let old = device::rate_hz(r.current);
    println!("old={old}  (restore with: lghub-lite rate <dev> {old})");
    let after = device::set_rate_verified(h, dev, hz).map_err(cli_hint)?;
    println!("new={hz} readback={after}");
    Ok(())
}

fn sector_label(sector: u16) -> String {
    match sector {
        profiles::USER_DIRECTORY => "user directory".into(),
        profiles::ROM_BASE => "factory directory".into(),
        s if s > profiles::ROM_BASE => format!("factory profile {}", s - profiles::ROM_BASE),
        s => format!("user sector {s}"),
    }
}

fn state_text(s: &profiles::SectorState) -> String {
    match s {
        profiles::SectorState::Erased => "erased".into(),
        profiles::SectorState::Rom => "factory (ROM), no CRC".into(),
        profiles::SectorState::CrcOk => "CRC ok".into(),
        profiles::SectorState::CrcBad(c, st) => format!("CRC MISMATCH (computed {c:04X}, stored {st:04X})"),
    }
}

/// Read-only overview of onboard profile memory.
fn show_profiles(h: &mut Hidpp, dev: u8) -> Result<(), Error> {
    let (fi, _, i) = profiles::info(h, dev)?;
    println!(
        "info       {} user profiles, {} factory, {} buttons, {} sectors x {} B (profile format {}, macro format {}, memory model {}{}{})",
        i.profile_count,
        i.factory_count,
        i.button_count,
        i.sector_count,
        i.sector_size,
        i.profile_format,
        i.macro_format,
        i.mem_model,
        if i.layout & 3 == 2 { ", G-shift" } else { "" },
        if i.layout & 0xC == 8 { ", DPI-shift" } else { "" },
    );
    if let Some(m) = device::onboard_mode(h, dev)? {
        println!("mode       {m} ({})", device::mode_name(m));
    }
    let active = profiles::active_profile(h, dev, fi)?;
    println!("active     {active:04x} ({})", sector_label(active));
    for dir in [profiles::USER_DIRECTORY, profiles::ROM_BASE] {
        let data = profiles::read_sector(h, dev, fi, dir, i.sector_size)?;
        let entries = profiles::parse_directory(&data);
        println!(
            "{:<10} {} ({})",
            if dir == 0 { "user" } else { "factory" },
            sector_label(dir),
            state_text(&profiles::sector_state(dir, &data))
        );
        if entries.is_empty() {
            println!("             no profiles");
        }
        for (sec, enabled) in entries {
            let state = match profiles::read_sector(h, dev, fi, sec, i.sector_size) {
                Ok(d) => state_text(&profiles::sector_state(sec, &d)),
                Err(e) => format!("unreadable: {e}"),
            };
            println!(
                "             {sec:04x} {} {}  {state}",
                sector_label(sec),
                if enabled { "enabled" } else { "disabled" }
            );
        }
    }
    Ok(())
}

/// Read every user sector and every factory sector into a text backup. Read-only.
fn dump(h: &mut Hidpp, dev: u8, file: &std::path::Path) -> Result<(), Error> {
    let model = device::model_id(h, dev)?;
    let (fi, raw, i) = profiles::info(h, dev)?;
    let user = 0..i.sector_count as u16;
    let rom = profiles::ROM_BASE..=profiles::ROM_BASE + i.factory_count as u16;
    let mut sectors = Vec::new();
    for sec in user.chain(rom) {
        let data = profiles::read_sector(h, dev, fi, sec, i.sector_size)?;
        let state = profiles::sector_state(sec, &data);
        // Cross-check our CRC with the device's for flash sectors that hold data. [HW f10]
        let dev_crc = if sec < profiles::ROM_BASE && state != profiles::SectorState::Erased {
            match profiles::device_crc(h, dev, fi, sec) {
                Ok(c) if c == profiles::crc_ccitt(&data[..data.len() - 2]) => ", device CRC agrees".to_string(),
                Ok(c) => format!(", device CRC {c:04X} DIFFERS"),
                Err(e) => format!(", device CRC: {e}"),
            }
        } else {
            String::new()
        };
        println!(
            "sector {sec:04x}  {:<18} {}{dev_crc}",
            sector_label(sec),
            state_text(&state)
        );
        sectors.push((sec, data));
    }
    config::write_atomic(file, &profiles::format_dump(model, &raw, &sectors)).map_err(Error::Other)?;
    println!("wrote      {} sectors to {}", sectors.len(), file.display());
    Ok(())
}

fn battery(h: &mut Hidpp, dev: u8, curve_dir: &std::path::Path) -> Result<(), Error> {
    let line = audio::battery_text(h, dev, curve_dir)?;
    println!("{line}");
    if line.starts_with('≈') {
        println!("hint       this is a generic estimate; see `lghub-lite curve <dev>` for a measured one");
    }
    Ok(())
}

/// `curve <dev> [build]`: which battery curve is used, the recording, and building from it.
fn curve(h: &mut Hidpp, dev: u8, dir: &std::path::Path, build: bool) -> Result<(), Error> {
    audio::adc(h, dev)?; // only voltage-reporting devices have curves
    let model = device::model_id(h, dev)?;
    if build {
        match battery::update_measured(dir, model).map_err(Error::Other)? {
            Some(_) => println!("built      {}", dir.join(format!("curve-{model:04x}.txt")).display()),
            None => println!("unchanged  the measured curve already includes the latest recorded discharge"),
        }
    }
    let c = battery::find_curve(dir, model);
    let runtime = c
        .runtime_tenths
        .map(|t| format!(", full runtime ~{}.{} h", t / 10, t % 10))
        .unwrap_or_default();
    println!("curve      {}{runtime}", c.label());
    println!("recording  {}", battery::recording_status(dir, model));
    println!("folder     {}", dir.display());
    if c.is_approx() {
        println!(
            "hint       keep `watch`, the background watcher or the app running while the headset goes\n           from full (>= {} mV) to nearly empty (<= {} mV); the curve is built when it charges again",
            battery::FULL_MV,
            battery::EMPTY_MV
        );
    }
    Ok(())
}

/// `autostart [on|off]`: the per-user login entry for lghub-lite-bg.exe (status when no arg).
fn autostart_cmd(o: &Opts, set: Option<bool>) -> Result<(), Error> {
    let bg = std::env::current_exe()
        .map_err(|e| Error::Io(e.to_string()))?
        .with_file_name(platform::exe("lghub-lite-bg"));
    match set {
        None => {}
        Some(true) => {
            if !bg.exists() {
                return Err(Error::Other(format!("{} not found next to lghub-lite", bg.display())));
            }
            let mut cmd = format!("\"{}\"", bg.display());
            if let Some(c) = &o.config {
                cmd.push_str(&format!(" --config \"{}\"", c.display()));
            }
            autostart::enable(&cmd).map_err(|e| Error::Io(e.to_string()))?;
            // Start it now too; a second copy exits on its own (single-instance mutex).
            autostart::start(&cmd).map_err(|e| Error::Io(e.to_string()))?;
            println!("autostart  on (started now; log: {})", log_hint(o));
        }
        Some(false) => {
            autostart::disable().map_err(|e| Error::Io(e.to_string()))?;
            let n = autostart::stop_running(&platform::exe("lghub-lite-bg")).map_err(|e| Error::Io(e.to_string()))?;
            println!("autostart  off (stopped {n} running watcher(s))");
        }
    }
    match autostart::status().map_err(|e| Error::Io(e.to_string()))? {
        Some(cmd) => println!("login      {cmd}"),
        None => println!("login      not registered"),
    }
    Ok(())
}

fn log_hint(o: &Opts) -> String {
    o.curve_dir()
        .map(|d| d.join("watch.log").display().to_string())
        .unwrap_or_else(|_| "watch.log next to the preset file".into())
}

/// Read-only view of the headset hardware EQ (writing is not supported; see audio.rs).
fn eq(h: &mut Hidpp, dev: u8) -> Result<(), Error> {
    let e = audio::eq(h, dev)?;
    let cells: Vec<String> = e
        .freqs
        .iter()
        .zip(&e.gains)
        .map(|(f, g)| {
            if *f >= 1000 {
                format!("{}k:{g:+}", f / 1000)
            } else {
                format!("{f}:{g:+}")
            }
        })
        .collect();
    println!(
        "eq         {} bands, +/-{} dB (hardware EQ, read-only)",
        e.info.bands, e.info.db_range
    );
    println!("gains      {}", cells.join("  "));
    Ok(())
}

fn mode(h: &mut Hidpp, dev: u8, set: Option<u8>) -> Result<(), Error> {
    let cur = device::onboard_mode(h, dev)?.ok_or(Error::NoFeature(0x8100))?;
    println!("mode       {cur} ({})", device::mode_name(cur));
    let Some(m) = set else { return Ok(()) };
    let after = device::set_mode_verified(h, dev, m)?;
    println!("new={m} readback={after} ({})", device::mode_name(after));
    Ok(())
}

fn raw(o: &Opts, sel: &str, bytes: &[&str]) -> Result<(), Error> {
    let (pid, _) = parse_selector(sel).ok_or_else(|| Error::Other(format!("bad device selector `{sel}`")))?;
    let msg: Vec<u8> = bytes
        .iter()
        .map(|b| u8::from_str_radix(b, 16))
        .collect::<Result<_, _>>()
        .map_err(|_| Error::Other("raw bytes must be hex".into()))?;
    if msg.len() < 4 {
        return Err(Error::Other(
            "need at least 4 bytes (report id, index, feature/sub-id, function/register)".into(),
        ));
    }
    let mut h = device::open(pid, o.swid, true)?;
    match h.transact(&msg, &[]) {
        Ok(_) => Ok(()),
        Err(Error::Timeout) => {
            println!("no matching reply");
            Ok(())
        }
        Err(e) => Err(e),
    }
}
