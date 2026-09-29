//! Module `lua_patch`: static checks against the v7.1.2 dump `nie.exe` (read-only; skipped when absent), and the
//! fingerprint matching run in real Lua 5.2 VMs (mlua).
//! Path of the dump: env `EVT_NIE_EXE`, else the default dump location.

use mlua::Lua;
use std::path::PathBuf;
use vr_loader::debug::x64len;
use vr_loader::lua_patch::sigs;
use vr_loader::lua_patch::{
    build_chunk, mismatch_reason, patch_files_roots_in, select, Fingerprints, MatchMode, Probe, Selection, Via,
};
use vr_loader::pe::parse_headers;
use vr_loader::scan::Pattern;

const DEFAULT_DUMP: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/v7.1.2/nie.exe");

fn load() -> Option<Vec<u8>> {
    let p = std::env::var("EVT_NIE_EXE").unwrap_or_else(|_| DEFAULT_DUMP.to_string());
    std::fs::read(&p).ok()
}

#[test]
fn signatures_are_unique_and_hooks_relocatable() {
    let Some(file) = load() else {
        eprintln!("nie.exe dump not found: skipped");
        return;
    };
    let h = parse_headers(&file).expect("PE32+");
    assert!(vr_loader::gate::header_matches(h.time_date_stamp, h.size_of_image), "dump is not v7.1.2");
    let t = h.section(".text").unwrap();
    let text = &file[t.raw_off as usize..(t.raw_off + t.raw_size.min(t.vsize)) as usize];
    for s in sigs::ALL {
        let p = Pattern::parse(s.pattern).unwrap();
        let hits = p.find(text, 3);
        assert_eq!(hits.len(), 1, "{}: {} hits", s.name, hits.len());
        assert_eq!(t.rva + hits[0] as u32 - s.offset, s.rva, "{}", s.name);
    }
    for (s, steal) in sigs::HOOKS {
        let p = Pattern::parse(s.pattern).unwrap();
        assert!(*steal >= 14 && p.fixed_prefix(*steal).is_some(), "{}: steal", s.name);
        let off = (s.rva - t.rva) as usize;
        // the decoder agrees: exactly `steal` bytes of whole, position-independent instructions
        assert_eq!(x64len::relocatable_prefix(&text[off..off + 64], 14), Ok(*steal), "{}", s.name);
    }
    // ScriptObject_LoadChunk calls the three Lua functions the module uses: luaL_loadbufferx (0x4D6B6E),
    // lua_pcallk (0x4D6B8F) and lua_settop (0x4D6BA4); L is read from [holder+0x50] right before each call
    let base = (sigs::LP_LOAD_CHUNK.rva - t.rva) as usize;
    for (call, target) in [(0x4D6B6Eu32, sigs::LP_LOADBUFFERX.rva), (0x4D6B8F, sigs::LP_PCALLK.rva), (0x4D6BA4, sigs::LP_SETTOP.rva)] {
        let o = (call - t.rva) as usize;
        assert!(o > base && o < base + 0xC0);
        assert_eq!(text[o], 0xE8);
        let d = i32::from_le_bytes(text[o + 1..o + 5].try_into().unwrap());
        assert_eq!((call as i64 + 5 + d as i64) as u32, target);
    }
    let l_read = (0x4D6B5Au32 - t.rva) as usize; // mov rcx,[rsi+50h]
    assert_eq!(&text[l_read..l_read + 4], &[0x48, 0x8B, 0x4E, sigs::HOLDER_L as u8]);
    // success path: mov al,1 at 0x4D6BD0 (the detour runs the patches only when the original returned 1)
    let ok = (0x4D6BD0u32 - t.rva) as usize;
    assert_eq!(&text[ok..ok + 2], &[0xB0, 0x01]);
}

// ---------------------------------------------------------------- fingerprint matching in real Lua 5.2 VMs (mlua)
// The loader's decision (`select`) with the probe chunks compiled and run by a real Lua 5.2, then the selected
// patch files run the way hooks.rs runs them (build_chunk prelude, one chunk per file).

fn vm(retail: &str) -> Lua {
    let lua = Lua::new();
    lua.load(retail).exec().unwrap();
    lua
}

/// The loader's probe: compile + run a `return <expr> or nil` chunk, non-nil = yes.
fn probe(lua: &Lua, src: &str) -> Probe {
    match lua.load(src).set_name("=evt_fingerprint").eval::<mlua::Value>() {
        Ok(mlua::Value::Nil) => Probe::No,
        Ok(_) => Probe::Yes,
        Err(e) => Probe::Error(e.to_string()),
    }
}

fn run(
    lua: &Lua,
    roots: &[(String, PathBuf)],
    name: Option<&str>,
    fps: &Fingerprints,
    mode: MatchMode,
    prefilter: bool,
) -> (Vec<String>, Selection) {
    let mut eval = |src: &str| probe(lua, src);
    let sel = select(fps, name, mode, prefilter, &mut eval);
    let mut ran = Vec::new();
    for g in &sel.groups {
        for f in patch_files_roots_in(roots, &g.dirs) {
            let chunk = build_chunk(&g.script, &f.name, &std::fs::read(&f.path).unwrap());
            lua.load(std::str::from_utf8(&chunk).unwrap()).set_name(format!("=patch:{}", f.name)).exec().unwrap();
            ran.push(f.name);
        }
    }
    (ran, sel)
}

fn ran_global(lua: &Lua) -> String {
    lua.globals().get::<_, Option<String>>("RAN").unwrap().unwrap_or_default()
}

struct Fixture {
    root: PathBuf,
    roots: Vec<(String, PathBuf)>,
    fps: Fingerprints,
}

fn fixture(tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("evt_lua_patch_fp_{}_{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let legacy = root.join("lua_patches");
    let put = |rel: &str, text: &str| {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    put(
        "lua_patches/_fingerprints.json",
        concat!(
            "{\n",
            " \"_markers\": [\"OnInit\", \"Step\"],\n",
            " \"menu_a\": {\"script\": \"menu_a_1.00.00\", \"require\": [\"AlphaOnly\"], \"forbid\": [], \"ambiguous\": []},\n",
            " \"menu_b\": {\"script\": \"menu_b_2.00.00\", \"require\": [\"BetaOnly\"], \"forbid\": [\"AlphaOnly\"], \"ambiguous\": []},\n",
            " \"twin_x\": {\"script\": \"twin_x_1.0.0\", \"require\": [\"TwinFn\"], \"forbid\": [], \"ambiguous\": [\"twin_y\"]},\n",
            " \"twin_y\": {\"script\": \"twin_y_1.0.0\", \"require\": [\"TwinFn\"], \"forbid\": [], \"ambiguous\": [\"twin_x\"]}\n",
            "}\n"
        ),
    );
    put("lua_patches/_all/00_all.lua", "RAN = (RAN or \"\") .. \"all:\" .. EVT_PATCH.script .. \";\"\n");
    put(
        "lua_patches/menu_a/10_a.lua",
        "assert(type(AlphaOnly) == \"function\", \"wrong VM\")\nRAN = RAN .. \"a:\" .. EVT_PATCH.file .. \";\"\n",
    );
    put("lua_patches/menu_b_2.00.00/10_b.lua", "assert(type(BetaOnly) == \"function\", \"wrong VM\")\nRAN = RAN .. \"b;\"\n");
    put("lua_patches/twin_x/01.lua", "RAN = RAN .. \"x;\"\n");
    put("lua_patches/twin_y/01.lua", "RAN = RAN .. \"y;\"\n");
    put("lua_patches/plain_menu/01.lua", "RAN = RAN .. \"plain;\"\n");
    // a mod root with its own fingerprint file
    put("mods/mod_x/lua/_fingerprints.json", "{\"mod_menu\": {\"script\": \"mod_menu_3.0.0\", \"require\": [\"ModOnly\"]}}\n");
    put("mods/mod_x/lua/mod_menu/01.lua", "RAN = RAN .. \"mod;\"\n");
    let mut fps = Fingerprints::load(&legacy.join("_fingerprints.json")).unwrap().unwrap();
    let notes = fps.merge(Fingerprints::load(&root.join("mods/mod_x/lua/_fingerprints.json")).unwrap().unwrap(), "mod_x");
    assert!(notes.is_empty());
    let roots = vec![(String::new(), legacy), ("mod_x".to_string(), root.join("mods/mod_x/lua"))];
    Fixture { root, roots, fps }
}

const RETAIL_A: &str = "function OnInit() end\nfunction AlphaOnly() end\nlocal hidden = 1\n";
const RETAIL_B: &str = "function OnInit() end\nfunction BetaOnly() end\n";
const RETAIL_TWIN: &str = "function Step() end\nfunction TwinFn() end\n";
const RETAIL_PLAIN: &str = "function OnInit() end\nfunction Whatever() end\n";
const RETAIL_NON_MENU: &str = "function Foo() end\n";
const RETAIL_MOD: &str = "function OnInit() end\nfunction ModOnly() end\n";

#[test]
fn fingerprint_picks_the_vm_own_folder_whatever_the_opened_path_says() {
    let fx = fixture("own");
    // menu A's chunk ran, but the thread last opened menu B (two menus requested at once): A's patches run, B's not
    let a = vm(RETAIL_A);
    let (ran, sel) = run(&a, &fx.roots, Some("menu_b_2.00.00"), &fx.fps, MatchMode::Fingerprint, true);
    assert_eq!(ran, vec!["_all/00_all.lua", "menu_a/10_a.lua"]);
    assert_eq!(ran_global(&a), "all:menu_a_1.00.00;a:menu_a/10_a.lua;");
    assert_eq!(sel.groups[1].via, Via::Fingerprint);
    let i = sel.mismatch.expect("the opened path's stem did not match");
    assert_eq!(fx.fps.entries[i].0, "menu_b");
    assert_eq!(mismatch_reason(&fx.fps.entries[i].1, &mut |s: &str| probe(&a, s)), "missing BetaOnly; forbidden AlphaOnly");
    // menu B with no opened path at all (resource cached): still found
    let b = vm(RETAIL_B);
    let (ran, sel) = run(&b, &fx.roots, None, &fx.fps, MatchMode::Fingerprint, true);
    assert_eq!(ran, vec!["_all/00_all.lua", "menu_b_2.00.00/10_b.lua"]);
    assert_eq!(ran_global(&b), "all:menu_b_2.00.00;b;");
    assert!(sel.mismatch.is_none());
    assert_eq!(sel.probes, 1 + 5, "pre-filter + one probe per stem");
    // the mod's fingerprint file: its stem runs from the mod root
    let m = vm(RETAIL_MOD);
    let (ran, _) = run(&m, &fx.roots, Some("menu_a_1.00.00"), &fx.fps, MatchMode::Fingerprint, true);
    assert_eq!(ran, vec!["_all/00_all.lua", "mods/mod_x/mod_menu/01.lua"]);
    assert_eq!(ran_global(&m), "all:mod_menu_3.0.0;mod;");
    let _ = std::fs::remove_dir_all(&fx.root);
}

#[test]
fn fingerprint_twins_prefilter_name_fallback_and_name_mode() {
    let fx = fixture("twins");
    // identical twins: both fingerprints match, the opened path breaks the tie, else the first
    let t = vm(RETAIL_TWIN);
    let (ran, sel) = run(&t, &fx.roots, Some("twin_y_1.0.0"), &fx.fps, MatchMode::Fingerprint, true);
    assert_eq!(ran, vec!["_all/00_all.lua", "twin_y/01.lua"]);
    assert!(sel.notes.iter().any(|(_, n)| n.contains("ambiguous twins twin_x, twin_y all match")), "{:?}", sel.notes);
    let t = vm(RETAIL_TWIN);
    let (ran, _) = run(&t, &fx.roots, None, &fx.fps, MatchMode::Fingerprint, true);
    assert_eq!(ran, vec!["_all/00_all.lua", "twin_x/01.lua"]);
    assert_eq!(ran_global(&t), "all:twin_x_1.0.0;x;");
    // a stem without a fingerprint entry keeps the name path
    let p = vm(RETAIL_PLAIN);
    let (ran, sel) = run(&p, &fx.roots, Some("plain_menu_1.0.0"), &fx.fps, MatchMode::Fingerprint, true);
    assert_eq!(ran, vec!["_all/00_all.lua", "plain_menu/01.lua"]);
    assert_eq!(sel.groups[1].via, Via::Name);
    assert_eq!(ran_global(&p), "all:plain_menu_1.0.0;plain;");
    // a VM without any marker global: only `_all`, no per-stem probe
    let n = vm(RETAIL_NON_MENU);
    let (ran, sel) = run(&n, &fx.roots, None, &fx.fps, MatchMode::Fingerprint, true);
    assert_eq!(ran, vec!["_all/00_all.lua"]);
    assert_eq!((sel.probes, sel.groups[0].script.as_str()), (1, "?"));
    let n = vm(RETAIL_NON_MENU);
    let (_, sel) = run(&n, &fx.roots, None, &fx.fps, MatchMode::Fingerprint, false);
    assert_eq!(sel.probes, 5, "pre-filter off: every stem probed");
    // name mode: the behaviour before the fingerprints (the wrong folder runs in the wrong VM)
    let a = vm(RETAIL_A);
    let mut eval = |src: &str| probe(&a, src);
    let sel = select(&fx.fps, Some("menu_b_2.00.00"), MatchMode::Name, true, &mut eval);
    assert_eq!(sel.probes, 0);
    let dirs: Vec<String> = sel.groups.iter().flat_map(|g| g.dirs.clone()).collect();
    assert_eq!(dirs, vec!["_all", "menu_b_2.00.00", "menu_b"]);
    // a broken base library: the probes report an error, nothing panics, `_all` still runs
    let a = vm(RETAIL_A);
    a.globals().set("rawget", mlua::Nil).unwrap();
    let (ran, sel) = run(&a, &fx.roots, None, &fx.fps, MatchMode::Fingerprint, true);
    assert_eq!(ran, vec!["_all/00_all.lua"]);
    assert!(
        sel.notes.iter().any(|(l, n)| *l == vr_loader::log::Level::Warn && n.contains("failed")),
        "{:?}",
        sel.notes
    );
    let _ = std::fs::remove_dir_all(&fx.root);
}
