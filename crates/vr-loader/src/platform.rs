//! Small OS helpers (with portable fallbacks so the pure modules can be unit-tested anywhere).

/// Local wall-clock time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LocalTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub millis: u16,
}

#[cfg(windows)]
pub fn local_time() -> LocalTime {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    let mut st = unsafe { std::mem::zeroed() };
    unsafe { GetLocalTime(&mut st) };
    LocalTime {
        year: st.wYear,
        month: st.wMonth as u8,
        day: st.wDay as u8,
        hour: st.wHour as u8,
        minute: st.wMinute as u8,
        second: st.wSecond as u8,
        millis: st.wMilliseconds,
    }
}

#[cfg(not(windows))]
pub fn local_time() -> LocalTime {
    LocalTime::default()
}

pub fn timestamp() -> String {
    let t = local_time();
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}", t.year, t.month, t.day, t.hour, t.minute, t.second, t.millis)
}

/// `yyyymmdd-hhmmss` (file names).
pub fn file_stamp() -> String {
    let t = local_time();
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", t.year, t.month, t.day, t.hour, t.minute, t.second)
}

#[cfg(windows)]
pub fn thread_id() -> u32 {
    unsafe { windows_sys::Win32::System::Threading::GetCurrentThreadId() }
}
#[cfg(not(windows))]
pub fn thread_id() -> u32 {
    0
}

#[cfg(windows)]
pub fn debug_string(s: &str) {
    let w: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { windows_sys::Win32::System::Diagnostics::Debug::OutputDebugStringW(w.as_ptr()) };
}
#[cfg(not(windows))]
pub fn debug_string(_s: &str) {}
