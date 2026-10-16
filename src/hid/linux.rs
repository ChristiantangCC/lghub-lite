//! Linux HID backend: hidraw nodes found through sysfs, report layout from the raw report
//! descriptor, `poll(2)` to wait on devices, an eventfd waker and a netlink uevent socket for
//! hot-plug. [INF] Written without Linux hardware; covered by unit tests and CI only.
//!
//! Access: /dev/hidraw* is root-only by default; packaging/linux/70-lghub-lite.rules gives the
//! logged-in user access to Logitech nodes (`TAG+="uaccess"`).

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{FOREVER, Info, Signal, Wake, descriptor};

/// What `wait_any` waits on: a pollable file descriptor.
pub type RawWaitable = RawFd;

/// `HID_ID=0003:0000046D:0000C54D` → (vid, pid); `HID_NAME=...` → product.
fn parse_uevent(text: &str) -> Option<(u16, u16, String)> {
    let mut ids = None;
    let mut name = String::new();
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("HID_ID=") {
            let f: Vec<&str> = v.split(':').collect();
            if let [_bus, vid, pid] = f[..] {
                ids = Some((
                    u32::from_str_radix(vid, 16).ok()? as u16,
                    u32::from_str_radix(pid, 16).ok()? as u16,
                ));
            }
        } else if let Some(v) = line.strip_prefix("HID_NAME=") {
            name = v.to_string();
        }
    }
    ids.map(|(v, p)| (v, p, name))
}

/// A sysfs HID device directory name looks like `0003:046D:C54D.0007`.
fn is_hid_dir_name(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() >= 19
        && b[4] == b':'
        && b[9] == b':'
        && b[14] == b'.'
        && name.chars().filter(|c| c.is_ascii_hexdigit()).count() == b.len() - 3
}

/// The kernel's hid-logitech-dj driver adds a virtual HID device per paired device *under*
/// the receiver's own HID device. The receiver node already reaches those devices by index,
/// so nested nodes are skipped (Solaar does the same). [INF: needs hardware confirmation]
fn is_nested(dev: &Path) -> bool {
    dev.parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .is_some_and(is_hid_dir_name)
}

/// Present hidraw interfaces of vendor `vid`, one `Info` per top-level collection.
pub fn enumerate(vid: u16) -> io::Result<Vec<Info>> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/sys/class/hidraw") else {
        return Ok(out); // no hidraw support (or not mounted): no devices
    };
    let mut nodes: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    nodes.sort();
    for node in nodes {
        let Some(name) = node.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
            continue;
        };
        let Ok(dev) = std::fs::canonicalize(node.join("device")) else {
            continue;
        };
        let Some((v, pid, product)) = std::fs::read_to_string(dev.join("uevent"))
            .ok()
            .and_then(|t| parse_uevent(&t))
        else {
            continue;
        };
        if v != vid {
            continue;
        }
        if is_nested(&dev) {
            crate::verbose(format_args!("skip /dev/{name}: virtual device under a receiver"));
            continue;
        }
        let Ok(desc) = std::fs::read(dev.join("report_descriptor")) else {
            continue;
        };
        for c in descriptor::parse(&desc) {
            out.push(Info {
                path: format!("/dev/{name}"),
                vid: v,
                pid,
                usage_page: c.usage_page,
                usage: c.usage,
                in_len: c.in_len,
                out_len: c.out_len,
                product: product.clone(),
                iface: dev.display().to_string(),
            });
        }
    }
    Ok(out)
}

/// An open hidraw node (all report IDs of one interface).
pub struct Handle {
    f: File,
}

impl Handle {
    pub fn open(info: &Info) -> io::Result<Handle> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&info.path)?;
        Ok(Handle { f })
    }

    /// Write one output report (report ID in byte 0). hidraw writes are synchronous in the
    /// kernel; the timeout only covers a node that is not writable yet.
    pub fn write(&self, report: &[u8], timeout_ms: u32) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
        loop {
            match (&self.f).write(report) {
                Ok(n) if n == report.len() => return Ok(()),
                Ok(_) => return Err(io::Error::other("short HID write")),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() || !poll_one(self.f.as_raw_fd(), libc::POLLOUT, left)? {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "HID write timed out"));
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    /// One report if one is waiting, else None.
    fn read_now(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut buf = [0u8; 4096];
        match self.f.read(&mut buf) {
            Ok(0) => Err(io::Error::from_raw_os_error(libc::ENODEV)),
            Ok(n) => Ok(Some(buf[..n].to_vec())),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => Ok(None),
            Err(e) => Err(e),
        }
    }
}

fn poll_one(fd: RawFd, events: i16, timeout: Duration) -> io::Result<bool> {
    let mut p = libc::pollfd { fd, events, revents: 0 };
    let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
    // SAFETY: one valid pollfd.
    let r = unsafe { libc::poll(&mut p, 1, ms) };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(r > 0)
}

/// Wait up to `timeout_ms` (`FOREVER` allowed) for an input report on any of `hs`, or for one
/// of `extras` to be signalled. One `poll(2)` call; no busy waiting.
pub fn wait_any(hs: &mut [&mut Handle], extras: &[&dyn Signal], timeout_ms: u32) -> Wake {
    let deadline = (timeout_ms != FOREVER).then(|| Instant::now() + Duration::from_millis(timeout_ms as u64));
    loop {
        let mut fds: Vec<libc::pollfd> = hs
            .iter()
            .map(|h| h.f.as_raw_fd())
            .chain(extras.iter().map(|s| s.raw()))
            .map(|fd| libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let ms = match deadline {
            None => -1,
            Some(d) => d
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(i32::MAX as u128) as i32,
        };
        // SAFETY: fds is a valid array of pollfd for the call.
        let r = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Wake::Failed(0, e);
        }
        if r == 0 {
            return Wake::Timeout;
        }
        for (i, h) in hs.iter_mut().enumerate() {
            let re = fds[i].revents;
            if re & libc::POLLIN != 0 {
                match h.read_now() {
                    Ok(Some(rep)) => return Wake::Report(i, rep),
                    Ok(None) => {}
                    Err(e) => return Wake::Failed(i, e),
                }
            }
            if re & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                return Wake::Failed(i, io::Error::from_raw_os_error(libc::ENODEV));
            }
        }
        for (k, s) in extras.iter().enumerate() {
            if fds[hs.len() + k].revents & libc::POLLIN != 0 {
                s.consume(); // like an auto-reset event: one wake-up consumes the signal
                return Wake::Extra(k);
            }
        }
        // Spurious wake-up (e.g. a report that was already read): wait again.
    }
}

/// Wakes a thread blocked in `wait_any` from another thread: an eventfd.
pub struct Waker {
    fd: OwnedFd,
}

impl Waker {
    pub fn new() -> io::Result<Waker> {
        // SAFETY: plain syscall; the fd is owned below.
        let fd = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is a fresh, owned descriptor.
        Ok(Waker {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        })
    }

    pub fn wake(&self) {
        let one: u64 = 1;
        // SAFETY: writes 8 bytes from a valid u64 to our eventfd.
        unsafe { libc::write(self.fd.as_raw_fd(), (&one as *const u64).cast(), 8) };
    }

    pub(super) fn raw_waitable(&self) -> RawWaitable {
        self.fd.as_raw_fd()
    }

    pub(super) fn consume(&self) {
        let mut v: u64 = 0;
        // SAFETY: reads 8 bytes into a valid u64; resets the counter.
        unsafe { libc::read(self.fd.as_raw_fd(), (&mut v as *mut u64).cast(), 8) };
    }
}

/// Device arrival/removal: kernel uevents on a netlink socket. Any uevent wakes the waiter
/// (re-enumerating is cheap); only hidraw ones matter.
pub struct Notifier {
    fd: OwnedFd,
}

impl Notifier {
    pub fn hid() -> io::Result<Notifier> {
        // SAFETY: plain syscalls; the fd is owned below, the address struct is fully set.
        unsafe {
            let fd = libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                libc::NETLINK_KOBJECT_UEVENT,
            );
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let fd = OwnedFd::from_raw_fd(fd);
            let mut addr: libc::sockaddr_nl = std::mem::zeroed();
            addr.nl_family = libc::AF_NETLINK as u16;
            addr.nl_groups = 1; // kernel uevent multicast group
            let r = libc::bind(
                fd.as_raw_fd(),
                (&addr as *const libc::sockaddr_nl).cast(),
                std::mem::size_of::<libc::sockaddr_nl>() as u32,
            );
            if r < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Notifier { fd })
        }
    }

    /// Drain pending uevents; true if one was about hidraw.
    fn drain(&self) -> bool {
        let mut any = false;
        let mut buf = [0u8; 8192];
        loop {
            // SAFETY: valid buffer and fd; non-blocking.
            let n = unsafe { libc::recv(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n <= 0 {
                return any;
            }
            any |= buf[..n as usize].split(|&b| b == 0).any(|f| f == b"SUBSYSTEM=hidraw");
        }
    }

    /// Wait up to `ms` for further notifications, consuming them (lets a burst from one
    /// plug-in settle before re-enumerating).
    pub fn settle(&self, ms: u32) {
        self.drain();
        while poll_one(self.fd.as_raw_fd(), libc::POLLIN, Duration::from_millis(ms as u64)).unwrap_or(false) {
            self.drain();
        }
    }

    pub(super) fn raw_waitable(&self) -> RawWaitable {
        self.fd.as_raw_fd()
    }

    pub(super) fn consume(&self) {
        self.drain();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uevent_and_sysfs_names() {
        let t = "DRIVER=logitech-djreceiver\nHID_ID=0003:0000046D:0000C54D\nHID_NAME=Logitech USB Receiver\nHID_PHYS=usb-0000:00:14.0-2/input2\n";
        assert_eq!(parse_uevent(t), Some((0x046D, 0xC54D, "Logitech USB Receiver".into())));
        assert_eq!(parse_uevent("HID_NAME=x\n"), None);
        assert!(is_hid_dir_name("0003:046D:C54D.0007"));
        assert!(!is_hid_dir_name("1-2:1.2"));
        assert!(is_nested(Path::new(
            "/sys/devices/pci0000:00/usb1/1-2/1-2:1.2/0003:046D:C54D.0007/0003:046D:407F.0008"
        )));
        assert!(!is_nested(Path::new(
            "/sys/devices/pci0000:00/usb1/1-2/1-2:1.2/0003:046D:C54D.0007"
        )));
    }
}
