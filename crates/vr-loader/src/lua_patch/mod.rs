//! Module `lua_patch`: user-supplied **plain-text Lua patch files** run inside a script's own VM right after the
//! engine has executed that script's main chunk (docs/game/engine/lua-patches.md). Off by default
//! (`[modules] lua_patch = false`).
//!
//! Folder `<game>/evt_loader/lua_patches/<script>/*.lua`, where `<script>` is the script stem
//! (`team_dock_formation_menu_7.00.09.00`) or the stem without its trailing `_N.NN.NN(.NN)` version
//! (`team_dock_formation_menu`); `_all/*.lua` runs for every script. Within a folder the files run in lexicographic
//! order; folders run `_all`, exact stem, unversioned stem. Before each file the global
//! `EVT_PATCH = { script = "<stem>", file = "<folder>/<file>" }` is set (a prelude on the file's first line, so
//! line numbers in error messages are the file's own).
//!
//! **Which script is this VM?** Two ways, `[lua_patch] match`:
//! * `"fingerprint"` (default): `<root>/_fingerprints.json` (written by `research/scripts/menu_assemble.py build`)
//!   lists, per stem, the global function names that only that script's chunk defines (`require`), names a twin
//!   defines and it does not (`forbid`) and the twins it cannot be told apart from (`ambiguous`). Right after the
//!   chunk ran, one tiny probe chunk per stem is compiled and run in the VM ([`fingerprint_chunk`]: `rawget(_ENV,
//!   ...)` checks, `return true or nil`); a non-nil result is a match and that stem's folders run. A pre-filter probe
//!   ([`prefilter_chunk`], the `_markers` of the file: names at least one of which every fingerprinted script
//!   defines) skips the scan for VMs that define none of them. The opened path (below) is used only for `_all`'s
//!   label, for stems that have no fingerprint, and to break the tie between ambiguous twins ([`select`]).
//! * `"name"`: the behaviour before 2026-09-27: the last `.lua.bin` path opened on this thread (fallback: the newest
//!   of any thread). Wrong or missing when several menus load at once from another thread (lua-patches-audit §2).
//!
//! Run time ([`hooks`], Windows only), the **fs route** of the design:
//! * `fs.CCriFileOperate_Open 0x4E70C0(this, const char* path, ...)` (pre-hook, pass-through): the single open every
//!   loader uses. A path ending in `.lua.bin` / `.lua` is remembered per thread (newest wins).
//! * `lua.ScriptObject_LoadChunk 0x4D6B20(holder, resource)` (post-hook): the engine's loader of a script object's
//!   **own** chunk (menus, triggers, objects; INCLUDE goes through `0x177C740` instead). It checks the resource is
//!   read (the open happened earlier, maybe on another thread), calls `luaL_loadbufferx(L, buf, size, NULL, NULL)`
//!   and `lua_pcallk(L, 0, 0, 0, 0, NULL)`, `L = [holder+0x50]`, and returns 1 on success. When it returns 1 the
//!   detour selects the folders ([`select`]) and runs their files in `L` with the engine's own `luaL_loadbufferx`
//!   (chunk name `=patch:<file>`) + `lua_pcallk`; errors are logged (message at -1, then popped). The pcall depth
//!   problem of hooking `lua_pcallk` itself (INCLUDE's nested pcalls) does not arise, and the two Lua functions stay
//!   free for module `debug` (which hooks them and, when on, adds a traceback to the errors of the patch files too).
//!
//! Extra roots (module `mods`, docs/game/engine/mod-format.md): every enabled mod's `mods/<id>/lua/` folder has the
//! same `<script>/*.lua` layout (+ its own `lua/_fingerprints.json`) and runs after `evt_loader/lua_patches`, mods in
//! load order ([`patch_files_roots`]); their files are named `mods/<id>/<folder>/<file>`.
//! ModLoader phase 2 (docs/app/modloader-roadmap.md): the load order is module `mods`' plan (`LoadPlan::lua_roots`:
//! required mods first, then priority / `load_order.toml`), then the file name (`NNN_*.lua`) inside each folder. The
//! per-chunk line says where the files came from (`ran 5 files [lua_patches 3, clean_hud 2]`, [`sources_summary`]).
//! Several mods patching one script share the VM: a file that reads a global before redefining it (wrapper) chains
//! onto the earlier roots' version; one that redefines it outright replaces it and the later root (higher priority)
//! wins. [`root_conflicts`] warns about the latter and about scripts a mod serves whole (`files/`) while another root
//! patches them (`lua_patch: conflict: ...` at init).
//!
//! Stripped bytecode: a patch reaches only the script's **globals** (functions, tables); locals and upvalues of the
//! original chunk are not visible.

pub mod sigs;
#[cfg(all(windows, target_arch = "x86_64"))]
pub mod hooks;

use crate::log::Level;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Folder under `evt_loader` holding one sub-folder per script.
pub const FOLDER: &str = "lua_patches";
/// Sub-folder whose files run for every script.
pub const ALL_DIR: &str = "_all";
/// Chunk-name prefix of the patch files (`=patch:<file>`; error messages say `patch:<file>:<line>:`).
pub const CHUNK_PREFIX: &str = "=patch:";
/// Fingerprint file at the root of every patch root (`lua_patches/_fingerprints.json`,
/// `mods/<id>/lua/_fingerprints.json`). Root entries starting with `_` are never script folders.
pub const FINGERPRINTS_FILE: &str = "_fingerprints.json";
/// Chunk name of the probe chunks (a named chunk: module `debug` never takes it as the VM's script name).
pub const PROBE_CHUNK: &str = "=evt_fingerprint";
/// Pre-filter markers when no fingerprint file carries `_markers` (the five most common menu callbacks: 543 of the
/// 552 retail menu chunks define at least one).
pub const DEFAULT_MARKERS: &[&str] = &["OnOpenLayer", "OnSetupLayer", "OnInit", "Step", "OnFunction"];

/// `[lua_patch]` of config.toml.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct LuaPatchCfg {
    /// `"fingerprint"`: a script's folder is chosen by the globals its chunk defined (`_fingerprints.json`); the
    /// opened path serves `_all`, the stems without a fingerprint and the tie between ambiguous twins.
    /// `"name"`: the opened path alone (behaviour before 2026-09-27).
    #[serde(rename = "match")]
    pub match_: String,
    /// Fingerprint mode: skip the per-stem probes when the VM defines none of the marker globals (one cheap probe).
    pub prefilter: bool,
}

impl Default for LuaPatchCfg {
    fn default() -> Self {
        LuaPatchCfg { match_: "fingerprint".into(), prefilter: true }
    }
}

impl LuaPatchCfg {
    pub fn mode(&self) -> MatchMode {
        if self.match_.trim().eq_ignore_ascii_case("name") {
            MatchMode::Name
        } else {
            MatchMode::Fingerprint
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchMode {
    Fingerprint,
    Name,
}

/// Script stem of an engine path: `common/script/lua/menu/x_7.00.09.00.lua.bin` -> `x_7.00.09.00`. None when the
/// file is not `.lua.bin` / `.lua`.
pub fn script_stem(path: &str) -> Option<String> {
    let name = path.rsplit(['/', '\\']).next()?;
    let lower = name.to_ascii_lowercase();
    let stem = if let Some(n) = lower.strip_suffix(".lua.bin") {
        &name[..n.len()]
    } else if let Some(n) = lower.strip_suffix(".lua") {
        &name[..n.len()]
    } else {
        return None;
    };
    (!stem.is_empty()).then(|| stem.to_string())
}

/// The stem without its trailing version `_N.NN.NN` or `_N.NN.NN.NN` (digits only); None when there is none.
pub fn strip_version(stem: &str) -> Option<String> {
    let us = stem.rfind('_')?;
    let ver = &stem[us + 1..];
    let parts: Vec<&str> = ver.split('.').collect();
    if !(parts.len() == 3 || parts.len() == 4) || us == 0 {
        return None;
    }
    if parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) {
        Some(stem[..us].to_string())
    } else {
        None
    }
}

/// The stem without its version when it has one, else itself (the key form of `_fingerprints.json`).
pub fn unversioned(stem: &str) -> String {
    strip_version(stem).unwrap_or_else(|| stem.to_string())
}

/// Folders (relative to [`FOLDER`]) whose files run for `stem`, in execution order.
pub fn patch_dirs(stem: &str) -> Vec<String> {
    let mut v = vec![ALL_DIR.to_string(), stem.to_string()];
    if let Some(s) = strip_version(stem) {
        if s != stem {
            v.push(s);
        }
    }
    v
}

/// One patch file to run.
#[derive(Debug, Clone, PartialEq)]
pub struct PatchFile {
    /// `<folder>/<file>` (the `EVT_PATCH.file` value and the chunk name).
    pub name: String,
    pub path: PathBuf,
}

/// Every `*.lua` regular file of the given folders under `root` (= `<data_dir>/lua_patches`), folders in the given
/// order, files of a folder in lexicographic (byte) order of their names.
pub fn patch_files_in(root: &Path, dirs: &[String]) -> Vec<PatchFile> {
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(root.join(dir)) else { continue };
        let mut names: Vec<(String, PathBuf)> = rd
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.to_ascii_lowercase().ends_with(".lua").then(|| (n, e.path()))
            })
            .collect();
        names.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        out.extend(names.into_iter().map(|(n, p)| PatchFile { name: format!("{dir}/{n}"), path: p }));
    }
    out
}

/// Every `*.lua` regular file for `stem` under `root`: folders in [`patch_dirs`] order.
pub fn patch_files(root: &Path, stem: &str) -> Vec<PatchFile> {
    patch_files_in(root, &patch_dirs(stem))
}

/// Patch files of the given folders in every root, roots in the given order. Label `""` = the legacy `lua_patches`
/// folder (names unchanged); any other label is a mod id (names `mods/<id>/<folder>/<file>`).
pub fn patch_files_roots_in(roots: &[(String, PathBuf)], dirs: &[String]) -> Vec<PatchFile> {
    let mut out = Vec::new();
    for (label, root) in roots {
        out.extend(patch_files_in(root, dirs).into_iter().map(|mut f| {
            if !label.is_empty() {
                f.name = format!("mods/{label}/{}", f.name);
            }
            f
        }));
    }
    out
}

/// Patch files of every root for `stem` (folders of [`patch_dirs`]).
pub fn patch_files_roots(roots: &[(String, PathBuf)], stem: &str) -> Vec<PatchFile> {
    patch_files_roots_in(roots, &patch_dirs(stem))
}

/// Where a patch file comes from, for the logs: the mod id of `mods/<id>/...` names, else [`FOLDER`] (the global
/// `evt_loader/lua_patches`).
pub fn patch_source(name: &str) -> &str {
    name.strip_prefix("mods/").and_then(|r| r.split('/').next()).filter(|s| !s.is_empty()).unwrap_or(FOLDER)
}

/// Files per source in run order (`lua_patches 3, clean_hud 2`): the per-chunk line says which mod each patch came
/// from.
pub fn sources_summary<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let mut v: Vec<(&str, usize)> = Vec::new();
    for n in names {
        let s = patch_source(n);
        match v.iter_mut().find(|(k, _)| *k == s) {
            Some((_, c)) => *c += 1,
            None => v.push((s, 1)),
        }
    }
    v.iter().map(|(k, c)| format!("{k} {c}")).collect::<Vec<_>>().join(", ")
}

// ---------------------------------------------------------------- conflicts between roots (global folder, mods)

fn word_at(text: &str, at: usize, w: &str) -> bool {
    let b = text.as_bytes();
    let before = at == 0 || !(b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_' || b[at - 1] == b'.' || b[at - 1] == b':');
    let end = at + w.len();
    let after = end >= b.len() || !(b[end].is_ascii_alphanumeric() || b[end] == b'_');
    before && after
}

/// Global functions a patch file (re)defines at its top level (`function Name(` / `Name = function` at column 0;
/// plain identifiers only, not `a.b` / `local function`), each with `true` when the file reads the name **before**
/// its first definition (a wrapper: `local prev = Name` ... `function Name(...) prev(...) end`, which chains onto
/// whatever ran earlier). A heuristic for warnings only: a comment naming the function earlier also counts as a read.
pub fn defined_globals(text: &str) -> Vec<(String, bool)> {
    let mut defs: Vec<(String, usize)> = Vec::new(); // name, byte offset of the first definition line
    let mut off = 0usize;
    for line in text.split_inclusive('\n') {
        let name = if let Some(r) = line.strip_prefix("function ") {
            r.split('(').next().map(str::trim)
        } else {
            line.split_once('=').and_then(|(l, r)| {
                let r = r.trim_start();
                (r.starts_with("function") && !r[8..].starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_'))
                    .then(|| l.trim_end())
                    .filter(|l| !l.is_empty() && !line.starts_with(char::is_whitespace))
            })
        };
        if let Some(n) = name.filter(|n| is_ident(n) && *n != "local") {
            if !defs.iter().any(|(d, _)| d == n) {
                defs.push((n.to_string(), off));
            }
        }
        off += line.len();
    }
    defs.into_iter()
        .map(|(n, first)| {
            let head = &text[..first];
            let reads = head.match_indices(n.as_str()).any(|(i, _)| word_at(head, i, &n));
            (n, reads)
        })
        .collect()
}

fn root_label(label: &str) -> String {
    if label.is_empty() {
        FOLDER.to_string()
    } else {
        format!("mod {label}")
    }
}

/// Conflicts between patch roots (`roots` in run order: the global folder, then the mods in load order), as warning
/// lines. `full_scripts` = `(script stem, mod id)` of every script a mod serves as a **whole file** (`files/`).
///
/// * Two roots defining the same global in the same script folder (versioned and unversioned folders together): a
///   later file that reads the name first (wrapper) chains onto the earlier one and is fine; one that does not
///   replaces it: warned, the later root (higher priority) wins because it runs last.
/// * A script one mod replaces whole while another root patches it: the patches run on that mod's file, not on the
///   retail chunk they were built for.
///
/// Whole file against whole file (two mods' `files/`) is reported by module `mods` (`conflict: file ...`, winner =
/// the mod loaded later).
pub fn root_conflicts(roots: &[(String, PathBuf)], full_scripts: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    // folder (unversioned, lowercase) -> [(root label, defined globals of every file of that root, in run order)]
    let mut groups: Vec<(String, Vec<(String, Vec<(String, bool)>)>)> = Vec::new();
    for (label, root) in roots {
        let Ok(rd) = std::fs::read_dir(root) else { continue };
        let mut dirs: Vec<String> = rd
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n == ALL_DIR || !n.starts_with('_'))
            .collect();
        // run order inside a root: versioned folder before the unversioned one (patch_dirs)
        dirs.sort_by_key(|d| (unversioned(d), unversioned(d) == *d, d.clone()));
        for d in dirs {
            let key = unversioned(&d).to_ascii_lowercase();
            let mut defs = Vec::new();
            for f in patch_files_in(root, std::slice::from_ref(&d)) {
                if let Ok(t) = std::fs::read(&f.path) {
                    defs.extend(defined_globals(&String::from_utf8_lossy(&t)));
                }
            }
            let gi = match groups.iter().position(|(k, _)| *k == key) {
                Some(i) => i,
                None => {
                    groups.push((key.clone(), Vec::new()));
                    groups.len() - 1
                }
            };
            let g = &mut groups[gi].1;
            match g.iter_mut().find(|(l, _)| l == label) {
                Some((_, v)) => v.extend(defs),
                None => g.push((label.clone(), defs)),
            }
        }
    }
    for (key, per_root) in &groups {
        // global -> labels that defined it so far (a wrapper keeps the list, a replacement resets it)
        let mut owners: Vec<(String, Vec<String>)> = Vec::new();
        for (label, defs) in per_root {
            for (name, wraps) in defs {
                match owners.iter_mut().find(|(n, _)| n == name) {
                    None => owners.push((name.clone(), vec![label.clone()])),
                    Some((_, who)) => {
                        let others: Vec<String> = who.iter().filter(|w| *w != label).map(|w| root_label(w)).collect();
                        if !*wraps && !others.is_empty() {
                            out.push(format!(
                                "{key}: {} replaces {name} (defined by {}) without chaining onto it: {} runs later (higher priority) and wins",
                                root_label(label),
                                others.join(", "),
                                root_label(label)
                            ));
                            who.clear();
                        }
                        if !who.contains(label) {
                            who.push(label.clone());
                        }
                    }
                }
            }
        }
    }
    for (stem, owner) in full_scripts {
        let key = unversioned(stem).to_ascii_lowercase();
        let Some((_, per_root)) = groups.iter().find(|(k, _)| *k == key) else { continue };
        let patchers: Vec<String> = per_root.iter().filter(|(l, _)| l != owner).map(|(l, _)| root_label(l)).collect();
        if !patchers.is_empty() {
            out.push(format!(
                "{stem}: mod {owner} replaces the whole script (files/) and {} patch it: the patches run on {owner}'s file, not on the retail chunk they were built for",
                patchers.join(", ")
            ));
        }
    }
    out
}

/// Script stems among module `mods`' whole-file overrides (`data/common/script/lua/.../<stem>.lua.bin` keys), with
/// the winning mod.
pub fn full_scripts<'a>(overrides: impl IntoIterator<Item = (&'a String, &'a String)>) -> Vec<(String, String)> {
    overrides
        .into_iter()
        .filter(|(k, _)| k.starts_with("data/common/script/lua/"))
        .filter_map(|(k, m)| script_stem(k).map(|s| (s, m.clone())))
        .collect()
}

/// `s` as a double-quoted Lua string literal (non-printable / non-ASCII bytes as `\ddd`).
pub fn lua_quote(s: &str) -> String {
    let mut q = String::with_capacity(s.len() + 2);
    q.push('"');
    for b in s.bytes() {
        match b {
            b'"' => q.push_str("\\\""),
            b'\\' => q.push_str("\\\\"),
            0x20..=0x7E => q.push(b as char),
            _ => q.push_str(&format!("\\{b:03}")),
        }
    }
    q.push('"');
    q
}

/// The chunk actually loaded: `EVT_PATCH={script=...,file=...};` followed by the file text on the **same** line (line
/// numbers are preserved). A UTF-8 BOM is dropped (`luaL_loadbufferx` does not skip it).
pub fn build_chunk(stem: &str, file: &str, text: &[u8]) -> Vec<u8> {
    let text = text.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(text);
    let mut v = format!("EVT_PATCH={{script={},file={}}};", lua_quote(stem), lua_quote(file)).into_bytes();
    v.extend_from_slice(text);
    v
}

// ---------------------------------------------------------------- fingerprints

/// One entry of `_fingerprints.json` (docs/game/engine/lua-patches.md "Detección por huella").
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Fingerprint {
    /// The retail script this stem patches, with its version (`EVT_PATCH.script` of the files that run).
    pub script: String,
    /// Global **functions** the chunk defines and no other script does (all must be present).
    pub require: Vec<String>,
    /// Globals a twin defines and this script does not (none may be present).
    pub forbid: Vec<String>,
    /// Stems (unversioned) whose VM holds the same globals: the opened path breaks the tie.
    pub ambiguous: Vec<String>,
}

/// The merged fingerprint files of every root.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fingerprints {
    /// `(stem, fingerprint)` in file order (legacy root first, then the mods).
    pub entries: Vec<(String, Fingerprint)>,
    /// Pre-filter markers (`_markers`): every fingerprinted script defines at least one.
    pub markers: Vec<String>,
}

/// A Lua identifier (`[A-Za-z_][A-Za-z0-9_]*`): the only names allowed inside a probe chunk.
pub fn is_ident(s: &str) -> bool {
    let mut it = s.bytes();
    match it.next() {
        Some(b) if b.is_ascii_alphabetic() || b == b'_' => {}
        _ => return false,
    }
    it.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl Fingerprints {
    /// Parse the JSON text. Keys starting with `_` are metadata (`_markers` = the pre-filter names); every other key
    /// is a stem. Names are checked to be identifiers (they are spliced into Lua source).
    pub fn parse(text: &str) -> Result<Fingerprints, String> {
        let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(text).map_err(|e| format!("not a JSON object: {e}"))?;
        let mut out = Fingerprints::default();
        for (k, v) in map {
            if k == "_markers" {
                let m: Vec<String> = serde_json::from_value(v).map_err(|e| format!("_markers: {e}"))?;
                for n in &m {
                    if !is_ident(n) {
                        return Err(format!("_markers: `{n}` is not an identifier"));
                    }
                }
                out.markers = m;
                continue;
            }
            if k.starts_with('_') {
                continue;
            }
            let mut fp: Fingerprint = serde_json::from_value(v).map_err(|e| format!("{k}: {e}"))?;
            if fp.script.is_empty() {
                fp.script = k.clone();
            }
            if fp.require.is_empty() && fp.forbid.is_empty() {
                return Err(format!("{k}: empty fingerprint (no require / forbid names)"));
            }
            for n in fp.require.iter().chain(&fp.forbid) {
                if !is_ident(n) {
                    return Err(format!("{k}: `{n}` is not an identifier"));
                }
            }
            out.entries.push((k, fp));
        }
        Ok(out)
    }

    /// Read `path`; `Ok(None)` when the file does not exist.
    pub fn load(path: &Path) -> Result<Option<Fingerprints>, String> {
        match std::fs::read_to_string(path) {
            Ok(t) => Fingerprints::parse(&t).map(Some).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Add the entries of another file (a mod's). A stem already present keeps the first fingerprint; a different
    /// one is reported. Markers are united.
    pub fn merge(&mut self, other: Fingerprints, label: &str) -> Vec<String> {
        let mut notes = Vec::new();
        for (k, fp) in other.entries {
            match self.entries.iter().find(|(s, _)| *s == k) {
                Some((_, have)) if *have == fp => {}
                Some(_) => notes.push(format!("{k}: the fingerprint of {label} differs from the one already loaded: keeping the first")),
                None => self.entries.push((k, fp)),
            }
        }
        for m in other.markers {
            if !self.markers.contains(&m) {
                self.markers.push(m);
            }
        }
        notes
    }

    pub fn get(&self, stem: &str) -> Option<&Fingerprint> {
        self.entries.iter().find(|(s, _)| s == stem).map(|(_, fp)| fp)
    }

    /// Index of the entry a script name (versioned or not) belongs to: key or `script` equal to the name or to its
    /// unversioned form.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        let short = unversioned(name);
        self.entries.iter().position(|(k, fp)| *k == name || *k == short || fp.script == name || unversioned(&fp.script) == short)
    }
}

fn rawget(name: &str) -> String {
    format!("rawget(_ENV,{})", lua_quote(name))
}

/// The probe chunk of one fingerprint: returns `true` when every `require` name is a global function and no
/// `forbid` name is defined, else `nil` (never an error: module `debug` logs every failed pcall).
pub fn fingerprint_chunk(fp: &Fingerprint) -> String {
    let mut conds: Vec<String> = fp.require.iter().map(|n| format!("type({})==\"function\"", rawget(n))).collect();
    conds.extend(fp.forbid.iter().map(|n| format!("{}==nil", rawget(n))));
    if conds.is_empty() {
        return "return nil".into();
    }
    format!("return ({}) or nil", conds.join(" and "))
}

/// The pre-filter probe: `true` when at least one marker global is defined, else `nil`.
pub fn prefilter_chunk(markers: &[String]) -> String {
    if markers.is_empty() {
        return "return true".into();
    }
    let conds: Vec<String> = markers.iter().map(|n| format!("{}~=nil", rawget(n))).collect();
    format!("return ({}) or nil", conds.join(" or "))
}

/// Probe of one name: `true` when it is a global function (`function = true`) / a defined global (`false`).
pub fn name_chunk(name: &str, function: bool) -> String {
    if function {
        format!("return (type({})==\"function\") or nil", rawget(name))
    } else {
        format!("return ({}~=nil) or nil", rawget(name))
    }
}

/// Result of running a probe chunk in the VM.
#[derive(Debug, Clone, PartialEq)]
pub enum Probe {
    /// The chunk returned a non-nil value.
    Yes,
    /// The chunk returned nil.
    No,
    /// Compile or run error (message).
    Error(String),
}

/// Why `fp` did not match (for the debug line when the opened path disagrees with the probes).
pub fn mismatch_reason(fp: &Fingerprint, eval: &mut dyn FnMut(&str) -> Probe) -> String {
    let missing: Vec<&str> =
        fp.require.iter().filter(|n| eval(&name_chunk(n, true)) != Probe::Yes).map(|s| s.as_str()).collect();
    let present: Vec<&str> =
        fp.forbid.iter().filter(|n| eval(&name_chunk(n, false)) == Probe::Yes).map(|s| s.as_str()).collect();
    let mut parts = Vec::new();
    if !missing.is_empty() {
        parts.push(format!("missing {}", missing.join(", ")));
    }
    if !present.is_empty() {
        parts.push(format!("forbidden {}", present.join(", ")));
    }
    if parts.is_empty() {
        "no difference found".into()
    } else {
        parts.join("; ")
    }
}

/// How a group of folders was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Via {
    /// `_all` (every script).
    All,
    /// A fingerprint matched the VM.
    Fingerprint,
    /// The opened path (name mode, or a stem without a fingerprint).
    Name,
}

/// Folders to run, with the `EVT_PATCH.script` value their files get.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    pub script: String,
    pub dirs: Vec<String>,
    pub via: Via,
}

/// What [`select`] decided for one chunk.
#[derive(Debug, Default, PartialEq)]
pub struct Selection {
    pub groups: Vec<Group>,
    /// Log lines (without the `lua_patch: ` prefix), with their level.
    pub notes: Vec<(Level, String)>,
    /// The fingerprint entry the opened path names when the VM did not match it (the caller logs the reason with
    /// [`mismatch_reason`] at debug level).
    pub mismatch: Option<usize>,
    /// Probe chunks evaluated (cost accounting for the tests).
    pub probes: usize,
}

fn lists(fp: &Fingerprint, key: &str, script: &str) -> bool {
    let short = unversioned(script);
    fp.ambiguous.iter().any(|t| t == key || *t == short)
}

/// Decide which folders run for a chunk that just ran in a VM. `name` = the script stem the opened path named (None
/// when nothing was opened); `eval` runs a probe chunk in that VM. Folders never repeat across the groups.
///
/// Fingerprint mode: `_all` always runs (label = the matched script, else the opened name, else `?`); every matching
/// fingerprint runs its `script` folders (versioned + unversioned); of matching twins that list each other as
/// `ambiguous` only one runs (the one the opened path names, else the first); a match whose `ambiguous` twin is the
/// one the opened path names is skipped (the path is trusted between identical VMs); a name with no fingerprint
/// entry runs by name as before; a name with an entry that did not match runs nothing by name (`mismatch`).
/// Name mode: exactly the pre-fingerprint behaviour (`_all` + the name's folders, nothing without a name).
pub fn select(
    fps: &Fingerprints,
    name: Option<&str>,
    mode: MatchMode,
    prefilter: bool,
    eval: &mut dyn FnMut(&str) -> Probe,
) -> Selection {
    let mut sel = Selection::default();
    let name_short = name.map(unversioned);
    let mut used: Vec<String> = vec![ALL_DIR.to_string()];
    let push = |sel: &mut Selection, used: &mut Vec<String>, script: &str, via: Via| {
        let dirs: Vec<String> = patch_dirs(script).into_iter().filter(|d| !used.contains(d)).collect();
        used.extend(dirs.iter().cloned());
        sel.groups.push(Group { script: script.to_string(), dirs, via });
    };
    if mode == MatchMode::Name {
        match name {
            Some(n) => push(&mut sel, &mut used, n, Via::Name),
            None => sel.notes.push((Level::Debug, "chunk ran but no .lua.bin was opened before it: no patches".into())),
        }
        if let Some(g) = sel.groups.first_mut() {
            g.dirs.insert(0, ALL_DIR.to_string());
            g.via = Via::All;
        }
        return sel;
    }
    // ---- fingerprint probes
    let mut matched: Vec<usize> = Vec::new();
    if !fps.entries.is_empty() {
        let mut scan = true;
        if prefilter && !fps.markers.is_empty() {
            sel.probes += 1;
            match eval(&prefilter_chunk(&fps.markers)) {
                Probe::Yes => {}
                Probe::No => {
                    scan = false;
                    sel.notes.push((Level::Debug, "no menu marker global defined: fingerprint scan skipped".into()));
                }
                Probe::Error(e) => sel.notes.push((Level::Warn, format!("fingerprint pre-filter failed: {e}"))),
            }
        }
        if scan {
            for (i, (k, fp)) in fps.entries.iter().enumerate() {
                sel.probes += 1;
                match eval(&fingerprint_chunk(fp)) {
                    Probe::Yes => matched.push(i),
                    Probe::No => {}
                    Probe::Error(e) => sel.notes.push((Level::Warn, format!("{k}: fingerprint probe failed: {e}"))),
                }
            }
        }
    }
    // ---- ambiguous twins
    let mut keep: Vec<usize> = Vec::new();
    let mut done: Vec<usize> = Vec::new();
    for &i in &matched {
        if done.contains(&i) {
            continue;
        }
        let (k, fp) = &fps.entries[i];
        let twins: Vec<usize> = matched
            .iter()
            .copied()
            .filter(|&j| j != i && !done.contains(&j))
            .filter(|&j| {
                let (kj, fj) = &fps.entries[j];
                lists(fp, kj, &fj.script) || lists(fj, k, &fp.script)
            })
            .collect();
        done.push(i);
        if twins.is_empty() {
            if let Some(ns) = &name_short {
                if *ns != *k && *ns != unversioned(&fp.script) && fp.ambiguous.contains(ns) {
                    sel.notes.push((Level::Info, format!("{k}: fingerprint matches but the opened path names its twin {ns}: skipped")));
                    continue;
                }
            }
            keep.push(i);
            continue;
        }
        let mut group = vec![i];
        group.extend(twins.iter().copied());
        done.extend(twins.iter().copied());
        let by_name = group.iter().copied().find(|&j| {
            let (kj, fj) = &fps.entries[j];
            name_short.as_ref().is_some_and(|ns| ns == kj || *ns == unversioned(&fj.script))
        });
        let pick = by_name.unwrap_or(group[0]);
        let names: Vec<&str> = group.iter().map(|&j| fps.entries[j].0.as_str()).collect();
        sel.notes.push((
            Level::Warn,
            format!(
                "ambiguous twins {} all match this VM: running {} only ({})",
                names.join(", "),
                fps.entries[pick].0,
                if by_name.is_some() { "named by the opened path" } else { "the first" }
            ),
        ));
        keep.push(pick);
    }
    for &i in &keep {
        let (k, fp) = &fps.entries[i];
        sel.notes.push((Level::Info, format!("{k}: matched by fingerprint")));
        push(&mut sel, &mut used, &fp.script, Via::Fingerprint);
    }
    // ---- the opened path
    match name {
        None => {
            if keep.is_empty() {
                sel.notes.push((Level::Debug, "chunk ran but no .lua.bin was opened before it and no fingerprint matched: only _all".into()));
            }
        }
        Some(n) => match fps.index_of(n) {
            None => {
                sel.notes.push((Level::Debug, format!("{n}: no fingerprint: matched by the opened path")));
                push(&mut sel, &mut used, n, Via::Name);
            }
            Some(i) if !keep.contains(&i) && !matched.contains(&i) => sel.mismatch = Some(i),
            Some(_) => {}
        },
    }
    // ---- `_all` first
    let label = sel.groups.first().map(|g| g.script.clone()).or_else(|| name.map(String::from)).unwrap_or_else(|| "?".into());
    sel.groups.insert(0, Group { script: label, dirs: vec![ALL_DIR.to_string()], via: Via::All });
    sel
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stems_and_versions() {
        assert_eq!(
            script_stem("common/script/lua/menu/team_dock_formation_menu_7.00.09.00.lua.bin").as_deref(),
            Some("team_dock_formation_menu_7.00.09.00")
        );
        assert_eq!(script_stem(r"data\common\script\lua\menu\Title_Menu.LUA.BIN").as_deref(), Some("Title_Menu"));
        assert_eq!(script_stem("x/y/abc.lua").as_deref(), Some("abc"));
        assert_eq!(script_stem("x/y/abc.g4tx"), None);
        assert_eq!(script_stem(".lua.bin"), None);
        assert_eq!(strip_version("team_dock_formation_menu_7.00.09.00").as_deref(), Some("team_dock_formation_menu"));
        assert_eq!(strip_version("menu_1.2.3").as_deref(), Some("menu"));
        assert_eq!(strip_version("menu_1.2"), None);
        assert_eq!(strip_version("menu_1.2.x"), None);
        assert_eq!(strip_version("title_menu"), None);
        assert_eq!(strip_version("_1.2.3"), None);
        assert_eq!(unversioned("soccer_menu_0.03.81"), "soccer_menu");
        assert_eq!(unversioned("soccer_menu"), "soccer_menu");
        assert_eq!(
            patch_dirs("team_dock_formation_menu_7.00.09.00"),
            vec!["_all".to_string(), "team_dock_formation_menu_7.00.09.00".into(), "team_dock_formation_menu".into()]
        );
        assert_eq!(patch_dirs("title_menu"), vec!["_all".to_string(), "title_menu".into()]);
    }

    #[test]
    fn chunk_keeps_line_numbers_and_quotes() {
        assert_eq!(lua_quote(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(lua_quote("é\n"), "\"\\195\\169\\010\"");
        let c = build_chunk("menu_1.0.0", "menu/10_x.lua", b"\xEF\xBB\xBF-- first\nlocal a = 1\n");
        let s = String::from_utf8(c).unwrap();
        assert_eq!(s, "EVT_PATCH={script=\"menu_1.0.0\",file=\"menu/10_x.lua\"};-- first\nlocal a = 1\n");
        assert_eq!(s.lines().count(), 2);
    }

    #[test]
    fn files_in_order() {
        let root = std::env::temp_dir().join(format!("evt_lua_patch_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let stem = "team_dock_formation_menu_7.00.09.00";
        std::fs::create_dir_all(root.join(stem)).unwrap();
        std::fs::create_dir_all(root.join("team_dock_formation_menu")).unwrap();
        std::fs::create_dir_all(root.join("_all")).unwrap();
        std::fs::write(root.join(stem).join("20_trash_button.lua"), "").unwrap();
        std::fs::write(root.join(stem).join("10_cards.LUA"), "").unwrap();
        std::fs::write(root.join(stem).join("notes.txt"), "").unwrap();
        std::fs::write(root.join("team_dock_formation_menu").join("00_base.lua"), "").unwrap();
        std::fs::write(root.join("_all").join("print.lua"), "").unwrap();
        let names: Vec<String> = patch_files(&root, stem).into_iter().map(|f| f.name).collect();
        assert_eq!(
            names,
            vec![
                "_all/print.lua",
                "team_dock_formation_menu_7.00.09.00/10_cards.LUA",
                "team_dock_formation_menu_7.00.09.00/20_trash_button.lua",
                "team_dock_formation_menu/00_base.lua",
            ]
        );
        assert!(patch_files(&root, "other_menu").iter().all(|f| f.name.starts_with("_all/")));
        assert!(patch_files(&root.join("missing"), stem).is_empty());
        // folders picked by the caller (fingerprint groups): no `_all`
        let names: Vec<String> = patch_files_in(&root, &["team_dock_formation_menu".to_string()]).into_iter().map(|f| f.name).collect();
        assert_eq!(names, vec!["team_dock_formation_menu/00_base.lua"]);
        // extra roots (module mods): legacy folder first, then mods in the given order
        let m = root.join("mod_a_lua");
        std::fs::create_dir_all(m.join("team_dock_formation_menu")).unwrap();
        std::fs::write(m.join("team_dock_formation_menu").join("01_a.lua"), "").unwrap();
        let roots = vec![("".to_string(), root.clone()), ("mod_a".to_string(), m.clone()), ("mod_b".to_string(), root.join("none"))];
        let names: Vec<String> = patch_files_roots(&roots, stem).into_iter().map(|f| f.name).collect();
        assert_eq!(names.len(), 5);
        assert_eq!(names[4], "mods/mod_a/team_dock_formation_menu/01_a.lua");
        assert_eq!(names[0], "_all/print.lua");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fingerprint_file_and_probe_chunks() {
        let text = r#"{
 "_markers": ["OnInit", "Step"],
 "_note": "ignored",
 "menu_a": {"script": "menu_a_1.00.00", "require": ["AlphaOnly"], "forbid": [], "ambiguous": []},
 "menu_b": {"script": "menu_b_2.00.00.01", "require": ["Beta1", "Beta2"], "forbid": ["Gamma"], "ambiguous": ["menu_c"]}
}"#;
        let f = Fingerprints::parse(text).unwrap();
        assert_eq!(f.markers, vec!["OnInit", "Step"]);
        assert_eq!(f.entries.len(), 2);
        assert_eq!(f.get("menu_b").unwrap().ambiguous, vec!["menu_c"]);
        assert_eq!(f.index_of("menu_b_2.00.00.01"), Some(1));
        assert_eq!(f.index_of("menu_b"), Some(1));
        assert_eq!(f.index_of("menu_a_9.9.9"), Some(0));
        assert_eq!(f.index_of("menu_c"), None);
        assert_eq!(fingerprint_chunk(f.get("menu_a").unwrap()), "return (type(rawget(_ENV,\"AlphaOnly\"))==\"function\") or nil");
        assert_eq!(
            fingerprint_chunk(f.get("menu_b").unwrap()),
            "return (type(rawget(_ENV,\"Beta1\"))==\"function\" and type(rawget(_ENV,\"Beta2\"))==\"function\" and rawget(_ENV,\"Gamma\")==nil) or nil"
        );
        assert_eq!(prefilter_chunk(&f.markers), "return (rawget(_ENV,\"OnInit\")~=nil or rawget(_ENV,\"Step\")~=nil) or nil");
        assert_eq!(name_chunk("X", false), "return (rawget(_ENV,\"X\")~=nil) or nil");
        // a missing `script` falls back to the key; bad names are refused
        let f = Fingerprints::parse(r#"{"m": {"require": ["A"]}}"#).unwrap();
        assert_eq!(f.get("m").unwrap().script, "m");
        assert!(Fingerprints::parse(r#"{"m": {"require": ["A B"]}}"#).unwrap_err().contains("not an identifier"));
        assert!(Fingerprints::parse(r#"{"m": {"require": [], "forbid": []}}"#).unwrap_err().contains("empty"));
        assert!(Fingerprints::parse(r#"{"_markers": ["1x"]}"#).is_err());
        assert!(Fingerprints::parse("[1]").is_err());
        // merge: first wins, markers united
        let mut a = Fingerprints::parse(r#"{"_markers": ["OnInit"], "m": {"require": ["A"]}}"#).unwrap();
        let b = Fingerprints::parse(r#"{"_markers": ["Step"], "m": {"require": ["B"]}, "n": {"require": ["N"]}}"#).unwrap();
        let notes = a.merge(b, "mod_x");
        assert_eq!(notes.len(), 1);
        assert_eq!(a.entries.len(), 2);
        assert_eq!(a.get("m").unwrap().require, vec!["A"]);
        assert_eq!(a.markers, vec!["OnInit", "Step"]);
    }

    /// A fake VM defining `globals` (all functions): evaluates the three probe shapes of this module
    /// (`return nil` / `return true` / `return (<terms> and|or ...) or nil`) from their source text. The real Lua
    /// evaluation is covered by `tests/lua_patch.rs` (mlua).
    fn fake_vm(globals: &'static [&'static str]) -> impl FnMut(&str) -> Probe {
        move |src: &str| {
            let Some(body) = src.strip_prefix("return ") else { return Probe::Error(format!("bad probe: {src}")) };
            match body {
                "nil" => return Probe::No,
                "true" => return Probe::Yes,
                _ => {}
            }
            let inner = body.strip_prefix('(').and_then(|s| s.strip_suffix(") or nil")).expect(src);
            let any = inner.contains(" or ");
            let term = |t: &str| {
                let name = t.split("_ENV,\"").nth(1).and_then(|s| s.split('"').next()).expect(t);
                let present = globals.contains(&name);
                if t.ends_with("==nil") {
                    !present
                } else {
                    present
                }
            };
            let r = if any { inner.split(" or ").any(term) } else { inner.split(" and ").all(term) };
            if r {
                Probe::Yes
            } else {
                Probe::No
            }
        }
    }

    fn fps() -> Fingerprints {
        Fingerprints::parse(
            r#"{
 "_markers": ["OnInit"],
 "menu_a": {"script": "menu_a_1.00.00", "require": ["AlphaOnly"], "forbid": [], "ambiguous": []},
 "menu_b": {"script": "menu_b_2.00.00", "require": ["BetaOnly"], "forbid": ["AlphaOnly"], "ambiguous": []},
 "twin_x": {"script": "twin_x_1.0.0", "require": ["TwinFn"], "forbid": [], "ambiguous": ["twin_y"]},
 "twin_y": {"script": "twin_y_1.0.0", "require": ["TwinFn"], "forbid": [], "ambiguous": ["twin_x"]},
 "solo": {"script": "solo_1.0.0", "require": ["SoloFn"], "forbid": [], "ambiguous": ["solo_replay"]}
}"#,
        )
        .unwrap()
    }

    fn dirs(sel: &Selection) -> Vec<String> {
        sel.groups.iter().flat_map(|g| g.dirs.clone()).collect()
    }

    #[test]
    fn select_by_fingerprint_ignores_a_wrong_name() {
        let f = fps();
        let mut vm = fake_vm(&["OnInit", "AlphaOnly"]);
        let sel = select(&f, Some("menu_b_2.00.00"), MatchMode::Fingerprint, true, &mut vm);
        assert_eq!(dirs(&sel), vec!["_all", "menu_a_1.00.00", "menu_a"]);
        assert_eq!(sel.groups[0].script, "menu_a_1.00.00");
        assert_eq!(sel.groups[1].via, Via::Fingerprint);
        assert_eq!(sel.mismatch, Some(1), "{sel:?}");
        assert_eq!(sel.probes, 1 + 5);
        assert!(sel.notes.iter().any(|(l, n)| *l == Level::Info && n == "menu_a: matched by fingerprint"), "{:?}", sel.notes);
        let reason = mismatch_reason(f.get("menu_b").unwrap(), &mut vm);
        assert_eq!(reason, "missing BetaOnly; forbidden AlphaOnly");
        // no name at all: still found
        let sel = select(&f, None, MatchMode::Fingerprint, true, &mut fake_vm(&["OnInit", "BetaOnly"]));
        assert_eq!(dirs(&sel), vec!["_all", "menu_b_2.00.00", "menu_b"]);
        assert!(sel.mismatch.is_none());
    }

    #[test]
    fn select_prefilter_name_only_and_name_mode() {
        let f = fps();
        // no marker: only `_all`, no per-stem probe
        let sel = select(&f, None, MatchMode::Fingerprint, true, &mut fake_vm(&["Whatever"]));
        assert_eq!(dirs(&sel), vec!["_all"]);
        assert_eq!(sel.groups[0].script, "?");
        assert_eq!(sel.probes, 1);
        // pre-filter off: every stem probed
        let sel = select(&f, None, MatchMode::Fingerprint, false, &mut fake_vm(&["Whatever"]));
        assert_eq!(sel.probes, 5);
        // a name without a fingerprint entry runs by name (after the fingerprint matches)
        let sel = select(&f, Some("plain_menu_1.0.0"), MatchMode::Fingerprint, true, &mut fake_vm(&["OnInit", "Whatever"]));
        assert_eq!(dirs(&sel), vec!["_all", "plain_menu_1.0.0", "plain_menu"]);
        assert_eq!(sel.groups[0].script, "plain_menu_1.0.0");
        assert_eq!(sel.groups[1].via, Via::Name);
        // both: fingerprint match + a different unfingerprinted name, no folder twice
        let sel = select(&f, Some("plain_menu"), MatchMode::Fingerprint, true, &mut fake_vm(&["OnInit", "AlphaOnly"]));
        assert_eq!(dirs(&sel), vec!["_all", "menu_a_1.00.00", "menu_a", "plain_menu"]);
        // name mode = the old behaviour, no probe at all
        let sel = select(&f, Some("menu_b_2.00.00"), MatchMode::Name, true, &mut fake_vm(&["OnInit", "AlphaOnly"]));
        assert_eq!(dirs(&sel), vec!["_all", "menu_b_2.00.00", "menu_b"]);
        assert_eq!(sel.probes, 0);
        let sel = select(&f, None, MatchMode::Name, true, &mut fake_vm(&["OnInit", "AlphaOnly"]));
        assert!(sel.groups.is_empty());
        // no fingerprints loaded at all: name path only
        let sel = select(&Fingerprints::default(), Some("m_1.0.0"), MatchMode::Fingerprint, true, &mut fake_vm(&["OnInit"]));
        assert_eq!(dirs(&sel), vec!["_all", "m_1.0.0", "m"]);
        assert_eq!(sel.probes, 0);
    }

    #[test]
    fn select_ambiguous_twins() {
        let f = fps();
        // both twins match: the opened path breaks the tie
        let sel = select(&f, Some("twin_y_1.0.0"), MatchMode::Fingerprint, true, &mut fake_vm(&["OnInit", "TwinFn"]));
        assert_eq!(dirs(&sel), vec!["_all", "twin_y_1.0.0", "twin_y"]);
        assert!(sel.notes.iter().any(|(l, n)| *l == Level::Warn && n.contains("running twin_y only (named by the opened path)")), "{:?}", sel.notes);
        // no usable name: the first entry
        let sel = select(&f, Some("other"), MatchMode::Fingerprint, true, &mut fake_vm(&["OnInit", "TwinFn"]));
        assert_eq!(dirs(&sel), vec!["_all", "twin_x_1.0.0", "twin_x", "other"]);
        let sel = select(&f, None, MatchMode::Fingerprint, true, &mut fake_vm(&["OnInit", "TwinFn"]));
        assert_eq!(dirs(&sel), vec!["_all", "twin_x_1.0.0", "twin_x"]);
        // a twin without its own entry: the path names it -> the fingerprinted stem is skipped and the twin runs by
        // name (no fingerprint entry); any other path -> the fingerprinted stem runs
        let sel = select(&f, Some("solo_replay_1.0.0"), MatchMode::Fingerprint, true, &mut fake_vm(&["OnInit", "SoloFn"]));
        assert_eq!(dirs(&sel), vec!["_all", "solo_replay_1.0.0", "solo_replay"]);
        assert_eq!(sel.groups[1].via, Via::Name);
        assert!(sel.notes.iter().any(|(_, n)| n.contains("names its twin solo_replay: skipped")), "{:?}", sel.notes);
        let sel = select(&f, Some("solo_1.0.0"), MatchMode::Fingerprint, true, &mut fake_vm(&["OnInit", "SoloFn"]));
        assert_eq!(dirs(&sel), vec!["_all", "solo_1.0.0", "solo"]);
        let sel = select(&f, None, MatchMode::Fingerprint, true, &mut fake_vm(&["OnInit", "SoloFn"]));
        assert_eq!(dirs(&sel), vec!["_all", "solo_1.0.0", "solo"]);
    }

    #[test]
    fn sources_of_patch_files() {
        assert_eq!(patch_source("title_menu/10_a.lua"), "lua_patches");
        assert_eq!(patch_source("mods/clean_hud/title_menu/10_a.lua"), "clean_hud");
        assert_eq!(patch_source("mods//x.lua"), "lua_patches");
        let names = ["_all/a.lua", "m/10_b.lua", "mods/clean_hud/_all/c.lua", "mods/clean_hud/m/01.lua", "mods/test/m/01.lua"];
        assert_eq!(sources_summary(names), "lua_patches 2, clean_hud 2, test 1");
        assert_eq!(sources_summary([]), "");
    }

    #[test]
    fn defined_globals_and_wrappers() {
        let t = "-- header\nlocal prev = OnInit\nfunction OnInit(...)\n  prev(...)\nend\nfunction Step()\n  local function inner() end\nend\n\
                 Helper = function(x) return x end\nlocal Loc = function() end\nfunction M.field() end\n  function Indented() end\n\
                 function Step() end\nfunction Rec() Rec() end\nfunctional = 1\nX = functionName\n";
        assert_eq!(
            defined_globals(t),
            vec![("OnInit".to_string(), true), ("Step".into(), false), ("Helper".into(), false), ("Rec".into(), false)]
        );
        // a field access of the same name is not a read of the global
        assert_eq!(defined_globals("local p = t.Step\nfunction Step() end\n"), vec![("Step".to_string(), false)]);
        assert!(defined_globals("local p=Step;function Step() end\n").is_empty());
    }

    fn scratch(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("evt_lua_patch_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn put(p: &Path, text: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn conflicts_between_roots() {
        let root = scratch("conflicts");
        let (g, a, b) = (root.join("lua_patches"), root.join("mods/a/lua"), root.join("mods/b/lua"));
        put(&g.join("menu_x/100_01_base.lua"), "function OnInit() end\nfunction Step() end\nfunction Solo() end\n");
        put(&g.join("_fingerprints.json"), "{}");
        put(&g.join("_files/data/x.lua.bin"), "");
        // a wraps OnInit (chains), b replaces Step (the versioned folder counts as the same script) and wraps OnInit
        put(&a.join("menu_x/010_a.lua"), "local p = OnInit\nfunction OnInit() p() end\n");
        put(&b.join("menu_x_1.00.00/010_b.lua"), "function Step() end\nlocal q = OnInit\nfunction OnInit() q() end\n");
        put(&b.join("other_menu/010_b.lua"), "function Solo() end\n");
        let roots = vec![(String::new(), g.clone()), ("a".to_string(), a.clone()), ("b".to_string(), b.clone())];
        let lines = root_conflicts(&roots, &[]);
        assert_eq!(
            lines,
            vec!["menu_x: mod b replaces Step (defined by lua_patches) without chaining onto it: mod b runs later (higher priority) and wins"],
            "{lines:?}"
        );
        // a whole-file script of mod c patched by the others; b's own whole file of other_menu is not a conflict
        let (k1, k2, k3, c, bb) = (
            "data/common/script/lua/menu/menu_x_1.00.00.lua.bin".to_string(),
            "data/common/gamedata/x.cfg.bin".to_string(),
            "data/common/script/lua/menu/other_menu.lua.bin".to_string(),
            "c".to_string(),
            "b".to_string(),
        );
        let full = full_scripts([(&k1, &c), (&k2, &c), (&k3, &bb)]);
        assert_eq!(full, vec![("menu_x_1.00.00".to_string(), "c".to_string()), ("other_menu".to_string(), "b".to_string())]);
        let lines = root_conflicts(&roots, &full);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(
            lines[1],
            "menu_x_1.00.00: mod c replaces the whole script (files/) and lua_patches, mod a, mod b patch it: the patches run on c's file, not on the retail chunk they were built for"
        );
        // a replacement in a second folder of the same script by the same mod
        put(&a.join("menu_x_1.00.00/020_a.lua"), "function OnInit() end\n");
        let lines = root_conflicts(&roots[..2], &[]);
        assert_eq!(lines, vec!["menu_x: mod a replaces OnInit (defined by lua_patches) without chaining onto it: mod a runs later (higher priority) and wins"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Roots come from module `mods`' plan: required mods before the mods requiring them, then priority; inside a
    /// folder the file number orders the files.
    #[test]
    fn mod_roots_follow_the_load_order() {
        let root = scratch("order");
        let mods = root.join("mods");
        for (id, prio, req) in [("base", 10, ""), ("addon", 0, "requires = [\"base\"]"), ("zeta", 0, "")] {
            put(&mods.join(id).join("mod.toml"), &format!("id = \"{id}\"\nversion = \"1\"\npriority = {prio}\n{req}\n"));
            put(&mods.join(id).join("lua/title_menu/020_second.lua"), "");
            put(&mods.join(id).join("lua/title_menu/010_first.lua"), "");
        }
        let plan = crate::mods::plan_root(&mods);
        let lr = plan.lua_roots();
        let ids: Vec<&str> = lr.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids.len(), 3, "{ids:?} {:?}", plan.skipped);
        let pos = |x: &str| ids.iter().position(|i| *i == x).unwrap();
        assert!(pos("base") < pos("addon"), "a required mod loads first: {ids:?}");
        assert!(pos("zeta") < pos("base"), "then priority: {ids:?}");
        let legacy = root.join("lua_patches");
        put(&legacy.join("title_menu/100_01_x.lua"), "");
        let mut roots = vec![(String::new(), legacy)];
        roots.extend(lr.clone());
        let names: Vec<String> = patch_files_roots(&roots, "title_menu_2.0.0").into_iter().map(|f| f.name).collect();
        let mut want = vec!["title_menu/100_01_x.lua".to_string()];
        for id in &ids {
            want.push(format!("mods/{id}/title_menu/010_first.lua"));
            want.push(format!("mods/{id}/title_menu/020_second.lua"));
        }
        assert_eq!(names, want);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Two mods wrapping the same global chain in the script's VM (run in the loader's order); a later replacement
    /// wins.
    #[test]
    fn wrappers_of_several_mods_chain_in_a_real_vm() {
        let root = scratch("chain");
        let (g, a, b) = (root.join("lua_patches"), root.join("mods/a/lua"), root.join("mods/b/lua"));
        put(&g.join("menu_x/100_01_base.lua"), "local r = OnInit\nfunction OnInit() r(); LOG = LOG .. 'global;' end\n");
        put(&a.join("menu_x/010_a.lua"), "local r = OnInit\nfunction OnInit() r(); LOG = LOG .. 'a;' end\nfunction Step() return 'a' end\n");
        put(&b.join("menu_x/010_b.lua"), "local r = OnInit\nfunction OnInit() r(); LOG = LOG .. EVT_PATCH.file .. ';' end\nfunction Step() return 'b' end\n");
        let roots = vec![(String::new(), g), ("a".to_string(), a), ("b".to_string(), b)];
        let lua = mlua::Lua::new();
        lua.load("LOG = ''\nfunction OnInit() LOG = LOG .. 'retail;' end\nfunction Step() return 'retail' end").exec().unwrap();
        let files = patch_files_roots(&roots, "menu_x_1.0.0");
        assert_eq!(sources_summary(files.iter().map(|f| f.name.as_str())), "lua_patches 1, a 1, b 1");
        for f in &files {
            let chunk = build_chunk("menu_x_1.0.0", &f.name, &std::fs::read(&f.path).unwrap());
            lua.load(std::str::from_utf8(&chunk).unwrap()).set_name(format!("{CHUNK_PREFIX}{}", f.name)).exec().unwrap();
        }
        lua.load("OnInit()").exec().unwrap();
        let log: String = lua.globals().get::<_, String>("LOG").unwrap();
        assert_eq!(log, "retail;global;a;mods/b/menu_x/010_b.lua;");
        let step: String = lua.load("return Step()").eval().unwrap();
        assert_eq!(step, "b");
        // Step: a replaces nothing earlier (the retail chunk is not a root), b replaces a's
        assert_eq!(root_conflicts(&roots, &[]), vec!["menu_x: mod b replaces Step (defined by mod a) without chaining onto it: mod b runs later (higher priority) and wins"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
