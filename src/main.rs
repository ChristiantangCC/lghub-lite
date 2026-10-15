mod audio;
mod config;
mod device;
mod hid;
mod hidpp;
mod profiles;
mod watch;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use device::parse_selector;
use hidpp::{DIRECT, Error, Hidpp};

static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn verbose(args: std::fmt::Arguments) {
    if VERBOSE.load(Ordering::Relaxed) {
        eprintln!("{args}");
    }
}

pub const LOGITECH_VID: u16 = 0x046D;

const USAGE: &str = "\
usage: lghub-lite [--trace] [--verbose] [--swid N] [--config FILE] <command>

  devices                  list HID++ devices, including those behind receivers
  watch                    print plug/unplug, receiver connect/disconnect and setting events;
                           applies presets whenever a device connects
  presets                  show the preset file
  preset <dev> [dpi=N] [rate=HZ] | clear
                           save (or remove) the preset for this device's model
  apply <dev>              apply the saved preset now (switches the device to host mode)
  profiles <dev>           onboard profile memory: info, active profile, directories, CRCs
  dump <dev> <file>        read every onboard memory sector into a text backup file
  info <dev>               identity, firmware and supported features
  battery <dev>            battery state (headsets: voltage; percentage needs a power-model
                           XML copied from your own G HUB install into the preset folder)
  sidetone <dev> [0-100]   read or set headset sidetone (hearing your own voice)
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

    /// Folder holding the preset file; battery curve XMLs are looked up here too.
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
            "--verbose" | "-v" => VERBOSE.store(true, Ordering::Relaxed),
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
        ["watch"] => opts.config_path().and_then(|c| watch::run(opts.swid, opts.trace, &c)),
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
        ["sidetone", d] => with_dev(&opts, d, |h, dev| sidetone(h, dev, None)),
        ["sidetone", d, v] => {
            let v: u8 = v
                .parse()
                .unwrap_or_else(|_| die("sidetone level must be a number 0-100"));
            with_dev(&opts, d, |h, dev| sidetone(h, dev, Some(v)))
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
    onboard_hint(device::set_dpi(h, dev, &d, v), mode)?;
    let after = device::dpi(h, dev)?;
    println!("new={v} readback={}", after.current);
    if after.current != v {
        return Err(Error::Other("read-back differs from the value written".into()));
    }
    Ok(())
}

/// Explain a write refusal when the device is in onboard-profile mode. Values are validated
/// against the device's own capability list before sending, so the refusal comes from the mode.
/// [HW] SL2 in onboard mode: 0x2202 f6 -> NotAllowed (5), 0x8061 f3 -> InvalidArgument (2).
fn onboard_hint(r: Result<(), Error>, mode: Option<u8>) -> Result<(), Error> {
    match r {
        Err(Error::Hpp20(c @ (2 | 5))) if mode == Some(device::MODE_ONBOARD) => Err(Error::Other(format!(
            "device refused ({}): it is in onboard-profile mode; run `mode <dev> host` first",
            hidpp::hpp20_error_name(c)
        ))),
        r => r,
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
    onboard_hint(device::set_report_rate(h, dev, &r, hz), mode)?;
    let after = device::rate_hz(device::report_rate(h, dev)?.current);
    println!("new={hz} readback={after}");
    if after != hz {
        return Err(Error::Other("read-back differs from the value written".into()));
    }
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
    if line.contains("no battery curve") {
        println!(
            "hint       for a percentage, copy your G HUB power model (e.g. C:\\ProgramData\\LGHUB\\depots\\<n>\\pro_headset\\pro_x_wireless_headset_battery.xml) into {}",
            curve_dir.display()
        );
    }
    Ok(())
}

fn sidetone(h: &mut Hidpp, dev: u8, set: Option<u8>) -> Result<(), Error> {
    let cur = audio::sidetone(h, dev)?;
    println!("sidetone   {cur}");
    let Some(v) = set else { return Ok(()) };
    println!("old={cur}  (restore with: lghub-lite sidetone <dev> {cur})");
    audio::set_sidetone(h, dev, v)?;
    let after = audio::sidetone(h, dev)?;
    println!("new={v} readback={after}");
    if after != v {
        return Err(Error::Other("read-back differs from the value written".into()));
    }
    Ok(())
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
    device::set_onboard_mode(h, dev, m)?;
    let after = device::onboard_mode(h, dev)?.unwrap_or(0);
    println!("new={m} readback={after} ({})", device::mode_name(after));
    if after != m {
        return Err(Error::Other("read-back differs from the mode written".into()));
    }
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
