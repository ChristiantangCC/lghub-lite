//! Command lines shared by the Linux and macOS autostart entries: split one into arguments,
//! start it detached.

use std::io;
use std::os::unix::process::CommandExt;

/// Split a command line with double-quoted parts into its arguments.
pub(super) fn split(command: &str) -> Vec<String> {
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
        .process_group(0) // not tied to the terminal it was started from
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines() {
        assert_eq!(
            split(r#""/opt/lghub lite/lghub-lite-bg" --config "/home/a b/p.txt""#),
            ["/opt/lghub lite/lghub-lite-bg", "--config", "/home/a b/p.txt"]
        );
        assert_eq!(split("a  b"), ["a", "b"]);
    }
}
