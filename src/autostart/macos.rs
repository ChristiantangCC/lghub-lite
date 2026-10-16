//! Login autostart via a per-user LaunchAgent,
//! `~/Library/LaunchAgents/io.github.lghub-lite.plist`, which launchd starts at the next
//! login (RunAtLoad). Only touched when the user runs `lghub-lite autostart on|off` or ticks
//! Tray in the app.

use std::io;
use std::path::PathBuf;

use super::unix::split;

pub use super::unix::start;

const LABEL: &str = "io.github.lghub-lite";

fn agent_path() -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is not set"))?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{LABEL}.plist")))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

/// The LaunchAgent for `command` (a command line with double-quoted paths).
fn agent(command: &str) -> String {
    let args: String = split(command)
        .iter()
        .map(|a| format!("\t\t<string>{}</string>\n", xml_escape(a)))
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\t<string>{LABEL}</string>\n\
         \t<key>ProgramArguments</key>\n\t<array>\n{args}\t</array>\n\
         \t<key>RunAtLoad</key>\n\t<true/>\n\
         \t<key>ProcessType</key>\n\t<string>Interactive</string>\n\
         </dict>\n\
         </plist>\n"
    )
}

/// The command line in `agent` text, double-quoting arguments with spaces (and the program).
fn command_of(plist: &str) -> Option<String> {
    let array = plist.split_once("<key>ProgramArguments</key>")?.1;
    let array = &array[..array.find("</array>")?];
    let args: Vec<String> = array
        .split("<string>")
        .skip(1)
        .filter_map(|s| s.split_once("</string>").map(|x| xml_unescape(x.0)))
        .collect();
    let quoted: Vec<String> = args
        .iter()
        .enumerate()
        .map(|(i, a)| {
            if i == 0 || a.contains(' ') {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect();
    (!quoted.is_empty()).then(|| quoted.join(" "))
}

pub fn enable(command: &str) -> io::Result<()> {
    let p = agent_path()?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    crate::config::write_atomic(&p, &agent(command)).map_err(io::Error::other)
}

pub fn disable() -> io::Result<()> {
    match std::fs::remove_file(agent_path()?) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The registered command line, if any.
pub fn status() -> io::Result<Option<String>> {
    match std::fs::read_to_string(agent_path()?) {
        Ok(text) => Ok(command_of(&text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Stop every running process of this user named `exe_name` (SIGTERM). Returns how many.
pub fn stop_running(exe_name: &str) -> io::Result<u32> {
    let me = std::process::id() as i32;
    // SAFETY: a null buffer asks for the number of processes.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut pids = vec![0i32; count as usize + 32]; // room for processes started meanwhile
    // SAFETY: the buffer holds pids.len() pids; the size is given in bytes.
    let n = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr().cast(),
            (pids.len() * std::mem::size_of::<i32>()) as i32,
        )
    };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut stopped = 0;
    for &pid in &pids[..n as usize] {
        if pid <= 0 || pid == me {
            continue;
        }
        let mut name = [0u8; 64];
        // SAFETY: valid buffer of the given size.
        let len = unsafe { libc::proc_name(pid, name.as_mut_ptr().cast(), name.len() as u32) };
        if len <= 0 || &name[..len as usize] != exe_name.as_bytes() {
            continue;
        }
        // SAFETY: plain signal; the kernel refuses (EPERM) processes of other users.
        if unsafe { libc::kill(pid, libc::SIGTERM) } == 0 {
            stopped += 1;
        }
    }
    Ok(stopped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_round_trips_the_command() {
        let cmd = r#""/Applications/lghub lite/lghub-lite-gui" --tray"#;
        let a = agent(cmd);
        assert!(a.contains("<string>/Applications/lghub lite/lghub-lite-gui</string>"));
        assert!(a.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert_eq!(command_of(&a).as_deref(), Some(cmd));
        let cmd = r#""/x/lghub-lite-bg" --config "/a & b/p.txt""#;
        assert_eq!(command_of(&agent(cmd)).as_deref(), Some(cmd));
    }
}
