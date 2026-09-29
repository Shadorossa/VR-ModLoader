//! 1-click install: `vrmodloader:` links.
//!
//! A mod site links to `vrmodloader:<https download url>`; Windows starts `VR-ModLoader.exe "<link>"` (registered
//! per user under `HKCU\Software\Classes\vrmodloader`, only after the player presses «Register» in Settings), the
//! manager asks for confirmation, downloads the archive over HTTPS and installs it like a dropped `.zip`.
//!
//! Accepted forms (see `docs/app/modloader-manager.md` for the GameBanana setup):
//! * `vrmodloader:https://gamebanana.com/mmdl/1234567,Mod,654321` — GameBanana's 1-click format
//!   (`<scheme>:<download url>,<item type>,<item id>`);
//! * `vrmodloader:https://example.org/files/my_mod.zip` or `vrmodloader://https://…` — any HTTPS link;
//! * `vrmodloader://install?url=<percent-encoded https url>`.
//!
//! Nexus Mods' own `nxm://` links need a Nexus API key and are not handled; Nexus pages can use the plain form.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const SCHEME: &str = "vrmodloader";
/// Largest download accepted.
pub const MAX_DOWNLOAD: u64 = 8 << 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The HTTPS URL of the archive.
    pub url: String,
    /// GameBanana item type and id when the link has them (`Mod`, `654321`).
    pub item: Option<(String, String)>,
}

impl Link {
    pub fn host(&self) -> &str {
        let rest = self.url.strip_prefix("https://").unwrap_or(&self.url);
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let hostport = &rest[..end];
        let host = hostport.rsplit('@').next().unwrap_or(hostport);
        host.split(':').next().unwrap_or(host)
    }
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse a `vrmodloader:` link (as Windows passes it: `%1`).
pub fn parse_link(link: &str) -> Result<Link, String> {
    let link = link.trim().trim_matches('"');
    let (scheme, rest) = link.split_once(':').ok_or("not a vrmodloader: link")?;
    if !scheme.eq_ignore_ascii_case(SCHEME) {
        return Err(format!("not a {SCHEME}: link"));
    }
    let mut rest = rest.trim_start_matches('/').to_string();
    let lower = rest.to_ascii_lowercase();
    if let Some(q) = lower.strip_prefix("install?").map(|_| &rest["install?".len()..]) {
        let url = q.split('&').find_map(|kv| kv.strip_prefix("url=")).ok_or("install link without url=")?;
        rest = pct_decode(url);
    } else if lower.starts_with("https%3a") {
        rest = pct_decode(&rest);
    } else if lower.starts_with("https:/") && !lower.starts_with("https://") {
        // some browsers collapse the double slash after a custom scheme
        rest = format!("https://{}", &rest["https:/".len()..]);
    }
    let mut item = None;
    // GameBanana: <url>,<ItemType>,<ItemId>[,<more>]
    let parts: Vec<&str> = rest.split(',').collect();
    if parts.len() >= 3 && parts[1].chars().all(|c| c.is_ascii_alphabetic()) && !parts[1].is_empty() && parts[2].chars().all(|c| c.is_ascii_digit()) {
        item = Some((parts[1].to_string(), parts[2].to_string()));
        rest = parts[0].to_string();
    }
    let url = rest.trim().to_string();
    if !url.to_ascii_lowercase().starts_with("https://") {
        return Err("only https:// downloads are accepted".into());
    }
    if url.len() > 2048 || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("malformed URL".into());
    }
    let l = Link { url, item };
    if l.host().is_empty() || !l.host().contains('.') {
        return Err("malformed URL (no host)".into());
    }
    Ok(l)
}

// ---------------------------------------------------------------- registration (HKCU)

/// Default registry path of the scheme, below HKEY_CURRENT_USER.
pub const CLASSES: &str = r"Software\Classes";

/// `(subkey below HKCU, value name ("" = default), data)` of the registration for `exe`.
pub fn registry_entries(classes: &str, exe: &Path) -> Vec<(String, String, String)> {
    let base = format!(r"{classes}\{SCHEME}");
    let exe = exe.display().to_string();
    vec![
        (base.clone(), String::new(), format!("URL:{} link", crate::APP_NAME)),
        (base.clone(), "URL Protocol".into(), String::new()),
        (format!(r"{base}\DefaultIcon"), String::new(), format!("\"{exe}\",0")),
        (format!(r"{base}\shell\open\command"), String::new(), format!("\"{exe}\" \"%1\"")),
    ]
}

/// Register the scheme for the current user (`classes` = [`CLASSES`]; tests use a scratch key).
pub fn register_at(classes: &str, exe: &Path) -> Result<(), String> {
    use winreg::enums::HKEY_CURRENT_USER;
    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    for (key, name, data) in registry_entries(classes, exe) {
        let (k, _) = hkcu.create_subkey(&key).map_err(|e| format!("HKCU\\{key}: {e}"))?;
        k.set_value(&name, &data).map_err(|e| format!("HKCU\\{key}: {e}"))?;
    }
    Ok(())
}

pub fn unregister_at(classes: &str) -> Result<(), String> {
    use winreg::enums::HKEY_CURRENT_USER;
    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    let key = format!(r"{classes}\{SCHEME}");
    match hkcu.delete_subkey_all(&key) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("HKCU\\{key}: {e}")),
    }
}

/// The command registered for the scheme (None = not registered).
pub fn registered_command_at(classes: &str) -> Option<String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    let k = hkcu.open_subkey_with_flags(format!(r"{classes}\{SCHEME}\shell\open\command"), KEY_READ).ok()?;
    k.get_value::<String, _>("").ok()
}

/// The registered command runs `exe`.
pub fn command_is(cmd: &str, exe: &Path) -> bool {
    let want = exe.display().to_string().to_lowercase();
    let c = cmd.trim().to_lowercase();
    c.strip_prefix('"').and_then(|r| r.split('"').next()).is_some_and(|p| p == want)
}

// ---------------------------------------------------------------- download

/// A safe file name for a download: `Content-Disposition` file name, else the last URL segment; an archive
/// extension is kept, anything else becomes `.zip` when the data is a zip (checked after the download).
pub fn file_name_for(url: &str, disposition: Option<&str>) -> String {
    let from_disp = disposition.and_then(|d| {
        d.split(';').map(str::trim).find_map(|p| p.strip_prefix("filename=")).map(|v| v.trim_matches('"').to_string())
    });
    let from_url = || {
        let path = url.split(['?', '#']).next().unwrap_or(url);
        path.rsplit('/').next().map(pct_decode).unwrap_or_default()
    };
    let raw = from_disp.filter(|s| !s.is_empty()).unwrap_or_else(from_url);
    let clean: String = raw.chars().map(|c| if c.is_alphanumeric() || " ._-()[]".contains(c) { c } else { '_' }).collect();
    let clean = clean.trim_matches(['.', ' ']).to_string();
    if clean.is_empty() {
        "download".into()
    } else {
        clean.chars().take(120).collect()
    }
}

/// Download `url` (HTTPS only, redirects too) into `dir`. `progress(done, total)` is called as data arrives.
pub fn download(url: &str, dir: &Path, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<PathBuf, String> {
    let config = ureq::Agent::config_builder()
        .https_only(true)
        .timeout_connect(Some(std::time::Duration::from_secs(30)))
        .user_agent(format!("{}/{}", crate::APP_NAME, crate::APP_VERSION))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut resp = agent.get(url).call().map_err(|e| format!("{url}: {e}"))?;
    let total = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    if total.is_some_and(|t| t > MAX_DOWNLOAD) {
        return Err(format!("{url}: file too big"));
    }
    let disp = resp.headers().get("content-disposition").and_then(|v| v.to_str().ok()).map(str::to_string);
    let mut name = file_name_for(url, disp.as_deref());
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let part = dir.join(format!("{name}.part"));
    let mut out = std::fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let mut reader = resp.body_mut().with_config().limit(MAX_DOWNLOAD).reader();
    let mut buf = vec![0u8; 256 << 10];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("{url}: {e}"))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| format!("{}: {e}", part.display()))?;
        done += n as u64;
        progress(done, total);
    }
    drop(out);
    let lower = name.to_ascii_lowercase();
    if ![".zip", ".7z", ".rar"].iter().any(|x| lower.ends_with(x)) {
        let mut head = [0u8; 4];
        let is_zip = std::fs::File::open(&part).and_then(|mut f| f.read_exact(&mut head)).is_ok() && &head == b"PK\x03\x04";
        name.push_str(if is_zip { ".zip" } else { ".7z" });
    }
    let fin = dir.join(&name);
    let _ = std::fs::remove_file(&fin);
    std::fs::rename(&part, &fin).map_err(|e| format!("{}: {e}", fin.display()))?;
    Ok(fin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links() {
        let l = parse_link("vrmodloader:https://gamebanana.com/mmdl/1234567,Mod,654321").unwrap();
        assert_eq!(l.url, "https://gamebanana.com/mmdl/1234567");
        assert_eq!(l.item, Some(("Mod".into(), "654321".into())));
        assert_eq!(l.host(), "gamebanana.com");
        let l = parse_link("\"vrmodloader://https://example.org/f/my_mod.zip\"").unwrap();
        assert_eq!(l.url, "https://example.org/f/my_mod.zip");
        assert_eq!(l.item, None);
        let l = parse_link("VRMODLOADER:https:/example.org/a.zip").unwrap();
        assert_eq!(l.url, "https://example.org/a.zip");
        let l = parse_link("vrmodloader://install?url=https%3A%2F%2Fexample.org%2Fa%5Fb.zip&x=1").unwrap();
        assert_eq!(l.url, "https://example.org/a_b.zip");
        // a decoded space is not a valid URL
        assert!(parse_link("vrmodloader://install?url=https%3A%2F%2Fexample.org%2Fa%20b.zip").is_err());
        let l = parse_link("vrmodloader:https%3A%2F%2Fexample.org%2Fx.zip").unwrap();
        assert_eq!(l.url, "https://example.org/x.zip");
        for bad in [
            "vrmodloader:http://example.org/x.zip",
            "vrmodloader:file:///C:/x.zip",
            "other:https://example.org/x.zip",
            "vrmodloader:https://",
            "vrmodloader:https://exa mple.org/x",
            "vrmodloader:javascript:alert(1)",
            "https://example.org",
        ] {
            assert!(parse_link(bad).is_err(), "{bad}");
        }
        assert_eq!(parse_link("vrmodloader:https://user@evil.example.org:443/x").unwrap().host(), "evil.example.org");
    }

    #[test]
    fn registry_layout() {
        let exe = Path::new(r"C:\Tools\VR-ModLoader\VR-ModLoader.exe");
        let e = registry_entries(CLASSES, exe);
        assert_eq!(e[0], (r"Software\Classes\vrmodloader".into(), String::new(), "URL:VR-ModLoader link".into()));
        assert_eq!(e[1].1, "URL Protocol");
        assert_eq!(e[3], (r"Software\Classes\vrmodloader\shell\open\command".into(), String::new(), r#""C:\Tools\VR-ModLoader\VR-ModLoader.exe" "%1""#.into()));
        assert!(command_is(&e[3].2, exe));
        assert!(!command_is(r#""C:\Other\x.exe" "%1""#, exe));
    }

    /// Writes a scratch key `HKCU\Software\VRModLoaderTest-<pid>` (never `Software\Classes`) and removes it.
    /// Ignored by default: run with `cargo test -p vr-modloader-app -- --ignored` to check the registry code.
    #[test]
    #[ignore]
    fn registry_round_trip_scratch_key() {
        let scratch = format!(r"Software\VRModLoaderTest-{}", std::process::id());
        let classes = format!(r"{scratch}\Classes");
        let exe = Path::new(r"C:\Tools\VR-ModLoader.exe");
        register_at(&classes, exe).unwrap();
        assert!(command_is(&registered_command_at(&classes).unwrap(), exe));
        unregister_at(&classes).unwrap();
        assert_eq!(registered_command_at(&classes), None);
        let hkcu = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER);
        hkcu.delete_subkey_all(&scratch).unwrap();
    }

    #[test]
    fn download_names() {
        assert_eq!(file_name_for("https://x.org/a/My%20Mod.zip?t=1", None), "My Mod.zip");
        assert_eq!(file_name_for("https://gamebanana.com/mmdl/123", Some("attachment; filename=\"cool_mod.7z\"")), "cool_mod.7z");
        assert_eq!(file_name_for("https://x.org/", None), "download");
        assert_eq!(file_name_for("https://x.org/..%2F..%2Fevil.zip", None), "_.._evil.zip");
    }
}
