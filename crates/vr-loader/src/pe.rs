//! PE32+ helpers: header fields, sections, import (IAT) patching. Works on a mapped image (run time) and, for the
//! static tests, on the raw file bytes.

#[derive(Debug, Clone)]
pub struct Section {
    pub name: String,
    pub rva: u32,
    pub vsize: u32,
    pub raw_off: u32,
    pub raw_size: u32,
}

#[derive(Debug, Clone)]
pub struct Headers {
    pub time_date_stamp: u32,
    pub size_of_image: u32,
    pub import_dir_rva: u32,
    pub sections: Vec<Section>,
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

/// Parse the headers from the first bytes of an image or file (needs the first ~0x1000 bytes).
pub fn parse_headers(b: &[u8]) -> Option<Headers> {
    if b.get(0..2)? != b"MZ" {
        return None;
    }
    let nt = u32_at(b, 0x3C)? as usize;
    if b.get(nt..nt + 4)? != b"PE\0\0" {
        return None;
    }
    let fh = nt + 4;
    let nsec = u16_at(b, fh + 2)? as usize;
    let tds = u32_at(b, fh + 4)?;
    let opt_size = u16_at(b, fh + 16)? as usize;
    let opt = fh + 20;
    if u16_at(b, opt)? != 0x20B {
        return None; // not PE32+
    }
    let size_of_image = u32_at(b, opt + 56)?;
    let import_dir_rva = u32_at(b, opt + 112 + 8)?;
    let mut sections = Vec::with_capacity(nsec);
    let mut s = opt + opt_size;
    for _ in 0..nsec {
        let raw_name = b.get(s..s + 8)?;
        let name = String::from_utf8_lossy(raw_name).trim_end_matches('\0').to_string();
        sections.push(Section {
            name,
            vsize: u32_at(b, s + 8)?,
            rva: u32_at(b, s + 12)?,
            raw_size: u32_at(b, s + 16)?,
            raw_off: u32_at(b, s + 20)?,
        });
        s += 40;
    }
    Some(Headers { time_date_stamp: tds, size_of_image, import_dir_rva, sections })
}

impl Headers {
    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|s| s.name == name)
    }
}

#[cfg(windows)]
pub mod live {
    //! Run-time helpers on mapped modules.
    use super::*;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

    pub fn exe_base() -> usize {
        unsafe { GetModuleHandleW(std::ptr::null()) as usize }
    }

    /// Headers of a mapped module.
    pub unsafe fn headers(base: usize) -> Option<Headers> {
        let hdr = std::slice::from_raw_parts(base as *const u8, 0x1000);
        parse_headers(hdr)
    }

    /// The mapped `.text` of a module.
    pub unsafe fn text(base: usize) -> Option<(usize, &'static [u8])> {
        let h = headers(base)?;
        let t = h.section(".text")?;
        Some((base + t.rva as usize, std::slice::from_raw_parts((base + t.rva as usize) as *const u8, t.vsize as usize)))
    }

    unsafe fn cstr_at(p: usize) -> &'static [u8] {
        std::ffi::CStr::from_ptr(p as *const std::ffi::c_char).to_bytes()
    }

    /// Address of the IAT slot of `dll!func` in module `base` (import by name only).
    pub unsafe fn iat_slot(base: usize, dll: &str, func: &str) -> Option<*mut usize> {
        let h = headers(base)?;
        if h.import_dir_rva == 0 {
            return None;
        }
        let mut d = base + h.import_dir_rva as usize;
        loop {
            let oft = *(d as *const u32);
            let name_rva = *((d + 12) as *const u32);
            let ft = *((d + 16) as *const u32);
            if name_rva == 0 && ft == 0 {
                return None;
            }
            let name = cstr_at(base + name_rva as usize);
            if name.eq_ignore_ascii_case(dll.as_bytes()) {
                let names = if oft != 0 { oft } else { ft };
                let mut i = 0usize;
                loop {
                    let thunk = *((base + names as usize + i * 8) as *const u64);
                    if thunk == 0 {
                        return None;
                    }
                    if thunk & (1u64 << 63) == 0 {
                        let fname = cstr_at(base + thunk as usize + 2);
                        if fname == func.as_bytes() {
                            return Some((base + ft as usize + i * 8) as *mut usize);
                        }
                    }
                    i += 1;
                }
            }
            d += 20;
        }
    }
}
