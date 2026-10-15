//! Thin Windows HID layer: enumerate collections, open, overlapped read/write with timeouts.

use std::ffi::c_void;
use std::io;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
};
use windows_sys::Win32::Devices::HumanInterfaceDevice::{
    HIDD_ATTRIBUTES, HIDP_CAPS, HIDP_STATUS_SUCCESS, HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetHidGuid,
    HidD_GetPreparsedData, HidD_GetProductString, HidP_GetCaps,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, WaitForMultipleObjects, WaitForSingleObject};
use windows_sys::core::GUID;

/// One HID top-level collection (Windows exposes each as its own device path).
#[derive(Debug, Clone)]
pub struct Info {
    pub path: String,
    pub vid: u16,
    pub pid: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub in_len: u16,
    pub out_len: u16,
    pub product: String,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Owned Win32 handle, closed on drop.
struct OwnedHandle(HANDLE);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: handle came from CreateFileW/CreateEventW and is closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

fn open_raw(path: &str, access: u32) -> io::Result<OwnedHandle> {
    let w = wide(path);
    // Share read+write: G HUB and Windows itself hold these collections open too.
    // SAFETY: w is NUL-terminated and outlives the call.
    let h = unsafe {
        CreateFileW(
            w.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_OVERLAPPED,
            null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(OwnedHandle(h))
}

/// Query attributes and caps through a zero-access handle (works even for collections
/// the OS opens exclusively, e.g. mouse/keyboard).
fn query(path: &str) -> io::Result<Info> {
    let h = open_raw(path, 0)?;
    let mut attr: HIDD_ATTRIBUTES = unsafe { std::mem::zeroed() };
    attr.Size = std::mem::size_of::<HIDD_ATTRIBUTES>() as u32;
    // SAFETY: valid handle and out-pointer.
    if !unsafe { HidD_GetAttributes(h.0, &mut attr) } {
        return Err(io::Error::last_os_error());
    }
    let mut pp = 0isize;
    // SAFETY: valid handle; pp freed below.
    if !unsafe { HidD_GetPreparsedData(h.0, &mut pp) } {
        return Err(io::Error::last_os_error());
    }
    let mut caps: HIDP_CAPS = unsafe { std::mem::zeroed() };
    // SAFETY: pp is valid preparsed data.
    let st = unsafe { HidP_GetCaps(pp, &mut caps) };
    unsafe { HidD_FreePreparsedData(pp) };
    if st != HIDP_STATUS_SUCCESS {
        return Err(io::Error::other(format!("HidP_GetCaps failed: {st:#x}")));
    }
    let mut buf = [0u16; 128];
    // SAFETY: buffer length given in bytes. Failure just leaves the product name empty.
    let product = if unsafe { HidD_GetProductString(h.0, buf.as_mut_ptr() as *mut c_void, 256) } {
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..n])
    } else {
        String::new()
    };
    Ok(Info {
        path: path.to_string(),
        vid: attr.VendorID,
        pid: attr.ProductID,
        usage_page: caps.UsagePage,
        usage: caps.Usage,
        in_len: caps.InputReportByteLength,
        out_len: caps.OutputReportByteLength,
        product,
    })
}

/// Enumerate present HID collections whose VID is `vid`. Paths not containing the VID are
/// skipped before opening, so unrelated devices are never touched.
pub fn enumerate(vid: u16) -> io::Result<Vec<Info>> {
    let mut guid: GUID = unsafe { std::mem::zeroed() };
    // SAFETY: out-pointer is valid.
    unsafe { HidD_GetHidGuid(&mut guid) };
    // SAFETY: guid is valid; no enumerator/hwnd.
    let set = unsafe { SetupDiGetClassDevsW(&guid, null(), null_mut(), DIGCF_PRESENT | DIGCF_DEVICEINTERFACE) };
    if set == -1 {
        return Err(io::Error::last_os_error());
    }
    let needle = format!("vid_{vid:04x}");
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        let mut ifd: SP_DEVICE_INTERFACE_DATA = unsafe { std::mem::zeroed() };
        ifd.cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
        // SAFETY: set is a valid device info list.
        if unsafe { SetupDiEnumDeviceInterfaces(set, null(), &guid, i, &mut ifd) } == 0 {
            break; // ERROR_NO_MORE_ITEMS (or a failure, which ends enumeration the same way)
        }
        i += 1;
        let Some(path) = interface_path(set, &ifd) else {
            continue;
        };
        if !path.to_ascii_lowercase().contains(&needle) {
            continue;
        }
        match query(&path) {
            Ok(info) if info.vid == vid => out.push(info),
            Ok(_) => {}
            Err(e) => crate::verbose(format_args!("skip {path}: {e}")),
        }
    }
    // SAFETY: set came from SetupDiGetClassDevsW.
    unsafe { SetupDiDestroyDeviceInfoList(set) };
    Ok(out)
}

fn interface_path(set: isize, ifd: &SP_DEVICE_INTERFACE_DATA) -> Option<String> {
    let mut need = 0u32;
    // SAFETY: size query with null buffer.
    unsafe { SetupDiGetDeviceInterfaceDetailW(set, ifd, null_mut(), 0, &mut need, null_mut()) };
    if (need as usize) < std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() {
        return None;
    }
    // u32-aligned buffer large enough for the detail struct plus the path.
    let mut buf = vec![0u32; (need as usize).div_ceil(4)];
    let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
    // SAFETY: buffer is `need` bytes, aligned; cbSize is the fixed struct size per the API contract.
    unsafe {
        (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        if SetupDiGetDeviceInterfaceDetailW(set, ifd, detail, need, null_mut(), null_mut()) == 0 {
            return None;
        }
        let p = std::ptr::addr_of!((*detail).DevicePath) as *const u16;
        let max = (need as usize - 4) / 2;
        let s = std::slice::from_raw_parts(p, max);
        let n = s.iter().position(|&c| c == 0).unwrap_or(max);
        Some(String::from_utf16_lossy(&s[..n]))
    }
}

fn new_event() -> io::Result<OwnedHandle> {
    // Manual-reset, initially unsignalled.
    // SAFETY: no attributes, no name.
    let e = unsafe { CreateEventW(null(), 1, 0, null()) };
    if e.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(OwnedHandle(e))
}

/// An opened HID collection with one persistent overlapped read in flight.
pub struct Handle {
    // Field order matters for Drop: the pending read is cancelled in Drop::drop before
    // these are released.
    ov: Box<OVERLAPPED>,
    buf: Vec<u8>,
    pending: bool,
    ev: OwnedHandle,
    h: OwnedHandle,
    pub out_len: usize,
}

impl Handle {
    pub fn open(info: &Info) -> io::Result<Handle> {
        let h = open_raw(&info.path, GENERIC_READ | GENERIC_WRITE)?;
        let ev = new_event()?;
        let mut ov: Box<OVERLAPPED> = Box::new(unsafe { std::mem::zeroed() });
        ov.hEvent = ev.0;
        Ok(Handle {
            ov,
            buf: vec![0; info.in_len.max(1) as usize],
            pending: false,
            ev,
            h,
            out_len: info.out_len as usize,
        })
    }

    /// Write one output report (caller supplies report ID in byte 0 and exact length).
    pub fn write(&self, report: &[u8], timeout_ms: u32) -> io::Result<()> {
        let ev = new_event()?;
        let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
        ov.hEvent = ev.0;
        let mut n = 0u32;
        // SAFETY: report and ov live until the I/O completes or is cancelled below.
        let ok = unsafe { WriteFile(self.h.0, report.as_ptr(), report.len() as u32, &mut n, &mut ov) };
        if ok == 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(e);
            }
        }
        // SAFETY: ev is valid.
        let w = unsafe { WaitForSingleObject(ev.0, timeout_ms) };
        if w == WAIT_TIMEOUT {
            // SAFETY: cancel, then wait for completion so `ov`/`report` are no longer referenced.
            unsafe {
                CancelIoEx(self.h.0, &ov);
                GetOverlappedResult(self.h.0, &ov, &mut n, 1);
            }
            return Err(io::Error::new(io::ErrorKind::TimedOut, "HID write timed out"));
        }
        // SAFETY: I/O completed.
        if unsafe { GetOverlappedResult(self.h.0, &ov, &mut n, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Ensure a read is in flight. Returns Some(report) if it completed synchronously.
    fn arm(&mut self) -> io::Result<Option<Vec<u8>>> {
        if self.pending {
            return Ok(None);
        }
        let ev = self.ov.hEvent;
        *self.ov = unsafe { std::mem::zeroed() };
        self.ov.hEvent = ev;
        let mut n = 0u32;
        // SAFETY: buf and ov are heap-allocated and outlive the I/O (cancelled in Drop).
        let ok = unsafe {
            ReadFile(
                self.h.0,
                self.buf.as_mut_ptr(),
                self.buf.len() as u32,
                &mut n,
                &mut *self.ov,
            )
        };
        if ok != 0 {
            // ReadFile on an overlapped handle may still complete immediately.
            return Ok(Some(self.buf[..n as usize].to_vec()));
        }
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
            return Err(e);
        }
        self.pending = true;
        Ok(None)
    }

    fn finish(&mut self) -> io::Result<Vec<u8>> {
        let mut n = 0u32;
        self.pending = false;
        // SAFETY: the event is signalled, so the I/O is complete.
        if unsafe { GetOverlappedResult(self.h.0, &*self.ov, &mut n, 0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(self.buf[..n as usize].to_vec())
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if self.pending {
            let mut n = 0u32;
            // SAFETY: cancel and wait so the kernel no longer writes into buf/ov.
            unsafe {
                CancelIoEx(self.h.0, &*self.ov);
                GetOverlappedResult(self.h.0, &*self.ov, &mut n, 1);
            }
        }
    }
}

/// Wait up to `timeout_ms` for an input report on any of `hs`.
/// Returns Ok(None) on timeout, else (index into hs, report bytes).
pub fn read_any(hs: &mut [&mut Handle], timeout_ms: u32) -> io::Result<Option<(usize, Vec<u8>)>> {
    for (i, h) in hs.iter_mut().enumerate() {
        if let Some(r) = h.arm()? {
            return Ok(Some((i, r)));
        }
    }
    let evs: Vec<HANDLE> = hs.iter().map(|h| h.ev.0).collect();
    // SAFETY: all events are valid for the duration of the wait.
    let w = unsafe { WaitForMultipleObjects(evs.len() as u32, evs.as_ptr(), 0, timeout_ms) };
    if w == WAIT_TIMEOUT {
        return Ok(None);
    }
    let i = w.wrapping_sub(WAIT_OBJECT_0) as usize;
    if i >= hs.len() {
        return Err(io::Error::last_os_error());
    }
    hs[i].finish().map(|r| Some((i, r)))
}
