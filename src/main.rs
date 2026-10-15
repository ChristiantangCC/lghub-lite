mod device;
mod hid;
mod hidpp;
mod watch;

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
usage: lghub-lite [--trace] [--verbose] [--swid N] <command>

  devices                  list HID++ devices, including those behind receivers
  watch                    print plug/unplug, receiver connect/disconnect and setting events
  info <dev>               identity, firmware and supported features
  battery <dev>            battery state
  dpi <dev> [value]        read DPI, or set it and read it back
  rate <dev> [hz]          read report rate, or set it and read it back
  mode <dev> [host|onboard] read or set onboard-profile mode (host mode allows DPI/rate writes)
  raw <dev> <hex...>       send one raw report, print replies (needs --allow-raw)
  collections              list every Logitech HID collection (debug)

<dev> is PID[:INDEX] as printed by `devices`, e.g. c54d:1 (receiver slot 1) or 0aba.";

struct Opts {
    trace: bool,
    swid: u8,
    allow_raw: bool,
}

fn main() {
    let mut opts = Opts {
        trace: false,
        swid: hidpp::DEFAULT_SWID,
        allow_raw: false,
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
            _ => args.push(a),
        }
    }
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let r = match a.as_slice() {
        ["devices"] => devices(&opts),
        ["watch"] => watch::run(opts.swid, opts.trace),
        ["collections"] => collections(),
        ["info", d] => with_dev(&opts, d, info),
        ["battery", d] => with_dev(&opts, d, |h, dev| {
            println!("{}", device::battery(h, dev)?);
            Ok(())
        }),
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
                // HID++ 1.0 answer to a 2.0 ping: this is a receiver. [OBS §9.6]
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
