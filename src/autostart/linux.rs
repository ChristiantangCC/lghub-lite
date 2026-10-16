//! Login autostart via an XDG autostart entry, `~/.config/autostart/lghub-lite.desktop`
//! (honoured by GNOME, KDE, XFCE, Cinnamon, MATE, LXQt). Only touched when the user runs
//! `lghub-lite autostart on|off` or ticks Tray in the app.
//! [INF] Written without Linux hardware; covered by unit tests and CI only.

use std::io;
use std::path::PathBuf;

fn entry_path() -> io::Result<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or_else(|| io::Error::other("neither XDG_CONFIG_HOME nor HOME is set"))?;
    Ok(base.join("autostart").join("lghub-lite.desktop"))
}

/// The .desktop file for `command` (a command line with double-quoted paths).
fn entry(command: &str) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=lghub-lite\nComment=Logitech HID++ battery and settings (lghub-lite)\nExec={command}\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n"
    )
}

pub fn enable(command: &str) -> io::Result<()> {
    let p = entry_path()?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::config::write_atomic(&p, &entry(command)).map_err(io::Error::other)
}

pub fn disable() -> io::Result<()> {
    match std::fs::remove_file(entry_path()?) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The registered command line, if any.
pub fn status() -> io::Result<Option<String>> {
    match std::fs::read_to_string(entry_path()?) {
        Ok(text) => Ok(text.lines().find_map(|l| l.strip_prefix("Exec=")).map(str::to_string)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Split a command line with double-quoted parts into its arguments.
fn split(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut any) = (false, false);
    for c in command.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            ' ' if !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                any = false;
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Start `command` now, detached from this process.
pub fn start(command: &str) -> io::Result<()> {
    let args = split(command);
    let (prog, rest) = args.split_first().ok_or_else(|| io::Error::other("empty command"))?;
    std::process::Command::new(prog)
        .args(rest)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Stop every running process of this user named `exe_name` (SIGTERM). Returns how many.
pub fn stop_running(exe_name: &str) -> io::Result<u32> {
    let me = std::process::id();
    // SAFETY: plain getter.
    let uid = unsafe { libc::getuid() };
    let mut n = 0;
    for e in std::fs::read_dir("/proc")?.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if pid == me {
            continue;
        }
        // comm is truncated to 15 characters, so compare against the exe's file name
        let exe = std::fs::read_link(e.path().join("exe")).ok();
        let name = exe.as_ref().and_then(|p| p.file_name()).and_then(|n| n.to_str());
        if name != Some(exe_name) {
            continue;
        }
        let owner = std::fs::metadata(e.path())
            .map(|m| std::os::unix::fs::MetadataExt::uid(&m))
            .ok();
        if owner != Some(uid) {
            continue;
        }
        // SAFETY: signalling a process of our own user.
        if unsafe { libc::kill(pid as i32, libc::SIGTERM) } == 0 {
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines_and_entries() {
        assert_eq!(
            split(r#""/opt/lghub lite/lghub-lite-bg" --config "/home/a b/p.txt""#),
            ["/opt/lghub lite/lghub-lite-bg", "--config", "/home/a b/p.txt"]
        );
        assert_eq!(split("a  b"), ["a", "b"]);
        let e = entry(r#""/x/lghub-lite-gui" --tray"#);
        assert!(e.contains("\nExec=\"/x/lghub-lite-gui\" --tray\n"));
    }
}
