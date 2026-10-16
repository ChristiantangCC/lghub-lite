//! Per-user login autostart via the standard `HKCU\...\CurrentVersion\Run` registry value.
//! Only touched when the user runs `lghub-lite autostart on|off`.

use std::io;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DETACHED_PROCESS, OpenProcess, PROCESS_INFORMATION, PROCESS_TERMINATE, STARTUPINFOW,
    TerminateProcess,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "lghub-lite";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn check(err: u32) -> io::Result<()> {
    if err == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(err as i32))
    }
}

/// Register `command` (a quoted exe path plus arguments) to run at login.
pub fn enable(command: &str) -> io::Result<()> {
    let (key, name, data) = (wide(RUN_KEY), wide(VALUE), wide(command));
    // SAFETY: NUL-terminated strings; size in bytes including the terminator.
    check(unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            REG_SZ,
            data.as_ptr().cast(),
            (data.len() * 2) as u32,
        )
    })
}

/// Remove the login entry; succeeds if it was not there.
pub fn disable() -> io::Result<()> {
    let (key, name) = (wide(RUN_KEY), wide(VALUE));
    // SAFETY: NUL-terminated strings.
    match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) } {
        ERROR_FILE_NOT_FOUND => Ok(()),
        e => check(e),
    }
}

/// Start `command` (quoted exe + args) detached. Direct CreateProcessW instead of
/// std::process::Command, which added ~130 KB to the release exe for this one call.
pub fn start(command: &str) -> io::Result<()> {
    let mut cmd = wide(command); // CreateProcessW may modify the command-line buffer
    let si = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..unsafe { std::mem::zeroed() }
    };
    let mut pi: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: valid, NUL-terminated mutable command line and zeroed structs.
    let ok = unsafe {
        CreateProcessW(
            std::ptr::null(),
            cmd.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            DETACHED_PROCESS,
            std::ptr::null(),
            std::ptr::null(),
            &si,
            &mut pi,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: handles returned by CreateProcessW; we do not wait on the child.
    unsafe {
        CloseHandle(pi.hThread);
        CloseHandle(pi.hProcess);
    }
    Ok(())
}

/// End every running process named `exe_name` that this user may terminate. Returns how many.
pub fn stop_running(exe_name: &str) -> io::Result<u32> {
    // SAFETY: snapshot handle is closed below.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut e: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut n = 0;
    // SAFETY: e.dwSize is set; snapshot is valid.
    let mut more = unsafe { Process32FirstW(snap, &mut e) } != 0;
    while more {
        let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
        if String::from_utf16_lossy(&e.szExeFile[..len]).eq_ignore_ascii_case(exe_name) {
            // SAFETY: plain process handle, closed right after use.
            let h = unsafe { OpenProcess(PROCESS_TERMINATE, 0, e.th32ProcessID) };
            if !h.is_null() {
                if unsafe { TerminateProcess(h, 0) } != 0 {
                    n += 1;
                }
                unsafe { CloseHandle(h) };
            }
        }
        // SAFETY: as above.
        more = unsafe { Process32NextW(snap, &mut e) } != 0;
    }
    unsafe { CloseHandle(snap) };
    Ok(n)
}

/// The registered command, if any.
pub fn status() -> io::Result<Option<String>> {
    let (key, name) = (wide(RUN_KEY), wide(VALUE));
    let mut buf = [0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: buffer and its byte length are valid; RRF_RT_REG_SZ guarantees termination.
    let err = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buf.as_mut_ptr().cast(),
            &mut len,
        )
    };
    if err == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    check(err)?;
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Ok(Some(String::from_utf16_lossy(&buf[..n])))
}
