//! Native **plugins** of mods (docs/app/modloader-plugins.md): `mods\<id>\<name>.dll` named by `plugin = "<name>.dll"`
//! in the mod's `mod.toml`, loaded by the init thread in load order with the C API table of `evt-plugin-sdk`
//! (`abi::EvtApi`, v1). Nothing here knows any particular mod.
//!
//! Pure part (unit-tested): which mods have a plugin ([`specs`]), the merged configuration of a mod
//! ([`mod_config`]), API version acceptance ([`check_api_version`]) and the built-in module a loaded plugin replaces
//! ([`replacing`]). Run time (Windows): [`host`].

use evt_modfmt::{parse_provide, LoadPlan};
use std::path::PathBuf;

pub use evt_plugin_sdk::abi::EVT_PLUGIN_API_VERSION;

/// One plugin to load.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginSpec {
    pub mod_id: String,
    pub version: String,
    pub dir: PathBuf,
    /// DLL file name (in `dir`).
    pub dll: String,
    /// Position of the mod in the load order.
    pub load_index: u32,
    /// Names from `provides` (versions dropped).
    pub provides: Vec<String>,
}

impl PluginSpec {
    pub fn path(&self) -> PathBuf {
        self.dir.join(&self.dll)
    }
    pub fn label(&self) -> String {
        format!("{}@{} ({})", self.mod_id, self.version, self.dll)
    }
}

/// The active mods of `plan` that ship a plugin, in load order.
pub fn specs(plan: &LoadPlan) -> Vec<PluginSpec> {
    plan.mods
        .iter()
        .enumerate()
        .filter(|(_, m)| !m.manifest.plugin.is_empty())
        .map(|(i, m)| PluginSpec {
            mod_id: m.manifest.id.clone(),
            version: m.manifest.version.clone(),
            dir: m.dir.clone(),
            dll: m.manifest.plugin.clone(),
            load_index: i as u32,
            provides: m.manifest.provides.iter().filter_map(|p| parse_provide(p).ok().map(|x| x.0)).collect(),
        })
        .collect()
}

/// A plugin built for API `v` runs on this loader (append-only table: every older version is served).
pub fn check_api_version(v: u32) -> Result<(), String> {
    if v == 0 {
        Err("reports API version 0".into())
    } else if v > EVT_PLUGIN_API_VERSION {
        Err(format!("needs plugin API v{v}, this ModLoader has v{EVT_PLUGIN_API_VERSION} (update the ModLoader)"))
    } else {
        Ok(())
    }
}

/// The loaded plugin (mod id) that replaces built-in loader module `module`: the first one whose `provides` names it.
/// `loaded` = the plugins whose init succeeded.
pub fn replacing<'a>(module: &str, loaded: &'a [PluginSpec]) -> Option<&'a PluginSpec> {
    loaded.iter().find(|p| p.provides.iter().any(|n| n == module))
}

fn deep_merge(base: &mut toml::Table, over: &toml::Table) {
    for (k, v) in over {
        match (base.get_mut(k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => deep_merge(b, o),
            _ => {
                base.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Configuration of mod `id` as TOML text: `mod_file` (`<mod>\config.toml`), overridden by the `[<id>]` table of the
/// loader's `config.toml` (`loader_cfg`; legacy section of a built-in module that became a plugin, same keys), then
/// by its `[mods.<id>]` table (the user's own settings, survive mod updates). Unreadable parts are skipped and
/// reported in the second value.
pub fn mod_config(id: &str, mod_file: Option<&str>, loader_cfg: Option<&str>) -> (String, Vec<String>) {
    let mut out = toml::Table::new();
    let mut problems = Vec::new();
    if let Some(t) = mod_file {
        match t.parse::<toml::Table>() {
            Ok(t) => out = t,
            Err(e) => problems.push(format!("config.toml of the mod not valid TOML ({}): ignored", e.message())),
        }
    }
    if let Some(l) = loader_cfg {
        if let Ok(t) = l.parse::<toml::Table>() {
            if let Some(toml::Value::Table(legacy)) = t.get(id) {
                deep_merge(&mut out, legacy);
            }
            if let Some(toml::Value::Table(user)) = t.get("mods").and_then(|m| m.get(id)) {
                deep_merge(&mut out, user);
            }
        }
    }
    (if out.is_empty() { String::new() } else { toml::to_string(&out).unwrap_or_default() }, problems)
}

/// Early-phase trigger: the exe entry point (`AddressOfEntryPoint`, MSVC `mainCRTStartup`, nie.exe v7.1.2 RVA
/// 0x9D5048). Its shape: `sub rsp,28h; call __security_init_cookie; add rsp,28h; jmp __scrt_common_main_seh`. The
/// first [`ENTRY_PATCH_LEN`] bytes are replaced by a one-shot absolute jump and restored before the original runs.
/// (An import is not reliable: the Windows loader already initialises the /GS cookie of nie.exe — its load config
/// names it — so the CRT's `__security_init_cookie` skips `GetSystemTimeAsFileTime`; in-game log 29/09 16:16.)
pub const ENTRY_SHAPE: &str = "48 83 EC 28 E8 ?? ?? ?? ?? 48 83 C4 28 E9 ?? ?? ?? ??";
pub const ENTRY_PATCH_LEN: usize = 14;

/// `AddressOfEntryPoint` of a PE image (headers at the start of `image`).
pub fn entry_point_rva(image: &[u8]) -> Option<u32> {
    let pe = u32::from_le_bytes(image.get(0x3C..0x40)?.try_into().ok()?) as usize;
    if image.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    Some(u32::from_le_bytes(image.get(pe + 24 + 16..pe + 24 + 20)?.try_into().ok()?))
}

/// The bytes at the entry point have the expected MSVC shape (safe to patch and restore).
pub fn entry_shape_ok(code: &[u8]) -> bool {
    crate::scan::Pattern::parse(ENTRY_SHAPE).is_ok_and(|p| code.len() >= p.len() && p.find(&code[..p.len()], 1).first() == Some(&0))
}

#[cfg(all(windows, target_arch = "x86_64"))]
pub mod host;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_specs_follow_the_load_order() {
        let root = std::env::temp_dir().join(format!("evt_loader_plugins_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mk = |id: &str, extra: &str| {
            std::fs::create_dir_all(root.join(id)).unwrap();
            std::fs::write(root.join(id).join("mod.toml"), format!("id=\"{id}\"\nname=\"{id}\"\nversion=\"1.0\"\n{extra}")).unwrap();
        };
        mk("user", "plugin = \"user.dll\"\nrequires = [\"core\"]\n");
        mk("core", "plugin = \"core.dll\"\nprovides = [\"quit_fix\", \"core_api=2\"]\npriority = 3\n");
        mk("data_only", "");
        let plan = evt_modfmt::plan_root_for(&root, Some(crate::MODLOADER_VERSION));
        let s = specs(&plan);
        assert_eq!(s.iter().map(|p| p.mod_id.as_str()).collect::<Vec<_>>(), vec!["core", "user"]);
        assert_eq!(s[0].provides, vec!["quit_fix", "core_api"]);
        assert!(s[0].load_index < s[1].load_index);
        assert!(s[1].path().ends_with("user/user.dll"));
        assert_eq!(replacing("quit_fix", &s).map(|p| p.mod_id.as_str()), Some("core"));
        assert!(replacing("stamina", &s).is_none());
        assert!(replacing("quit_fix", &s[1..]).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Static check against the v7.1.2 dump (skipped when absent): entry point RVA and shape of the early trigger.
    #[test]
    fn entry_point_trigger_matches_the_dump() {
        let p = std::env::var("EVT_NIE_EXE")
            .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/v7.1.2/nie.exe").into());
        let Ok(b) = std::fs::read(&p) else {
            eprintln!("nie.exe dump not found: skipped");
            return;
        };
        let rva = entry_point_rva(&b).unwrap();
        assert_eq!(rva, 0x9D5048);
        let h = crate::pe::parse_headers(&b).unwrap();
        let s = h.sections.iter().find(|s| s.rva <= rva && rva < s.rva + s.vsize).unwrap();
        let off = (rva - s.rva + s.raw_off) as usize;
        assert!(entry_shape_ok(&b[off..off + 18]), "{:02X?}", &b[off..off + 18]);
        assert!(!entry_shape_ok(&[0x48, 0x83, 0xEC, 0x28, 0x90]));
    }

    #[test]
    fn api_versions() {
        assert!(check_api_version(1).is_ok());
        assert!(check_api_version(0).is_err());
        assert!(check_api_version(EVT_PLUGIN_API_VERSION + 1).unwrap_err().contains("update the ModLoader"));
    }

    #[test]
    fn config_layers() {
        let modf = "grace_seconds = 5\nretail_quit = true\n[extra]\na = 1\nb = 2\n";
        let loader = "[modules]\nmods = true\n[quit_fix]\ngrace_seconds = 8\n[mods.quit_fix]\nretail_quit = false\n[mods.quit_fix.extra]\nb = 3\n[mods.other]\nx = 1\n";
        let (t, p) = mod_config("quit_fix", Some(modf), Some(loader));
        assert!(p.is_empty());
        let v: toml::Table = t.parse().unwrap();
        assert_eq!(v["grace_seconds"].as_integer(), Some(8), "legacy [quit_fix] over the mod file");
        assert_eq!(v["retail_quit"].as_bool(), Some(false), "[mods.quit_fix] over everything");
        assert_eq!((v["extra"]["a"].as_integer(), v["extra"]["b"].as_integer()), (Some(1), Some(3)), "tables merge key by key");
        assert!(v.get("x").is_none());
        // no file anywhere: empty; a broken mod file is reported and skipped
        assert_eq!(mod_config("x", None, None).0, "");
        let (t, p) = mod_config("x", Some("= broken"), Some("[mods.x]\nk = 1\n"));
        assert_eq!((t.trim(), p.len()), ("k = 1", 1));
    }
}
