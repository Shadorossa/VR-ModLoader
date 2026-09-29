use super::*;
use l5_core::t2b::SORT_INDEX_NAME;

const IDX: &str = "a/t\tT_INFO\t0\tdata/common/gamedata/a/t_*.cfg.bin\t0:id,1:a,2:b,3:name\n\
                   a/n\tN_INFO\t0\tdata/common/gamedata/a/t_*.cfg.bin\t0:name,1:v\n\
                   a/nokey\tT_INFO\t\tdata/common/gamedata/a/t_*.cfg.bin\t0:id\n\
                   a/maps\tM_INFO\t0\tdata/common/gamedata/map/{map}/m.cfg.bin\t0:id\n";
const KEY: &str = "data/common/gamedata/a/t_1.00.00.00.cfg.bin";

fn idx() -> HashMap<String, TableInfo> {
    tables::parse_index(IDX)
}

fn s(x: &str) -> Value {
    Value::String(Some(x.into()))
}

/// Counted list T_INFO (id, a, b, name) with a sort index and a child row, plus a text-keyed list N_INFO.
fn base() -> Vec<u8> {
    let mut d = T2b::default();
    let rows: Vec<(&str, Vec<Value>)> = vec![
        ("T_INFO_LIST_BEG", vec![Value::Int(3), Value::Int(1)]),
        ("T_INFO", vec![Value::Int(30), Value::Int(1), Value::Float(1.5), s("thirty")]),
        ("T_INFO_REF_X", vec![Value::Int(0), Value::Int(2)]),
        ("T_INFO", vec![Value::Int(-5), Value::Int(2), Value::Float(2.5), s("minus")]),
        ("T_INFO", vec![Value::Int(10), Value::Int(3), Value::Float(3.5), s("ten")]),
        (SORT_INDEX_NAME, vec![Value::Int(2)]),
        (SORT_INDEX_NAME, vec![Value::Int(0)]),
        (SORT_INDEX_NAME, vec![Value::Int(1)]),
        ("T_INFO_LIST_END", vec![]),
        ("N_INFO_LIST_BEG", vec![Value::Int(1)]),
        ("N_INFO", vec![s("alpha"), Value::Int(7)]),
        ("N_INFO_LIST_END", vec![]),
    ];
    for (n, v) in rows {
        let e = d.new_entry(n, v).unwrap();
        d.entries.push(e);
    }
    d.to_bytes().unwrap()
}

fn set(table: &str, key: &str, col: &str, v: Cell) -> SetOp {
    let column = match col.parse::<u32>() {
        Ok(i) => evt_modfmt::Column::Index(i),
        Err(_) => evt_modfmt::Column::Name(col.into()),
    };
    SetOp { table: table.into(), key: key.into(), column, value: v }
}

fn add(key: &str, from: Option<&str>, values: &[(&str, Cell)]) -> AddOp {
    AddOp { table: "a/t".into(), key: key.into(), from: from.map(str::to_string), values: values.iter().map(|(k, v)| (k.to_string(), v.clone())).collect() }
}

/// Rows of `list` in file order: values.
fn rows(bytes: &[u8], list: &str) -> Vec<Vec<Value>> {
    let t = T2b::parse(bytes).unwrap();
    t.entries.iter().filter(|e| e.name.as_deref() == Some(list)).map(|e| e.values.clone()).collect()
}

fn row(bytes: &[u8], id: i32) -> Vec<Value> {
    rows(bytes, "T_INFO").into_iter().find(|r| r[0] == Value::Int(id)).unwrap()
}

fn warns(m: &Merged) -> Vec<&str> {
    m.notes.iter().filter(|(l, _)| *l == Lvl::Warn).map(|(_, s)| s.as_str()).collect()
}

#[test]
fn two_mods_same_row_different_cells() {
    let (sa, sb) = (set("a/t", "10", "a", Cell::Int(100)), set("a/t", "10", "b", Cell::Int(4)));
    let mods = [ModOps { id: "ma", sets: vec![&sa], adds: vec![] }, ModOps { id: "mb", sets: vec![&sb], adds: vec![] }];
    let m = merge_file(&base(), &idx(), &mods).unwrap();
    assert_eq!(m.applied, 2);
    assert!(m.notes.is_empty(), "{:?}", m.notes);
    // both changes, the int written into a decimal cell keeps the cell a decimal
    assert_eq!(row(&m.bytes, 10), vec![Value::Int(10), Value::Int(100), Value::Float(4.0), s("ten")]);
    // untouched rows and the other list are unchanged
    assert_eq!(row(&m.bytes, 30), rows(&base(), "T_INFO")[0]);
    assert_eq!(rows(&m.bytes, "N_INFO"), rows(&base(), "N_INFO"));
    // no op: byte-identical round trip
    let none = merge_file(&base(), &idx(), &[]).unwrap();
    assert_eq!(none.bytes, base());
}

#[test]
fn same_cell_conflict_later_wins() {
    let sa = set("a/t", "10", "a", Cell::Int(1));
    // same cell spelled differently: column index 1, key in hex
    let sb = set("a/t", "0xA", "1", Cell::Int(2));
    let other = set("a/t", "30", "name", Cell::Str("x".into()));
    let mods = [ModOps { id: "ma", sets: vec![&sa, &other], adds: vec![] }, ModOps { id: "mb", sets: vec![&sb], adds: vec![] }];
    let m = merge_file(&base(), &idx(), &mods).unwrap();
    assert_eq!(row(&m.bytes, 10)[1], Value::Int(2));
    assert_eq!(row(&m.bytes, 30)[3], s("x"));
    let w = warns(&m);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("data conflict: cell a/t[0xA].#1: ma = 1, mb = 2 (winner: mb)"), "{w:?}");
}

#[test]
fn row_keys_numbers_names_text() {
    // signed / unsigned spellings of -5
    let a = set("a/t", &(-5i32 as u32).to_string(), "a", Cell::Int(50));
    // a text-keyed table
    let b = set("a/n", "alpha", "v", Cell::Int(8));
    // a name in a hash cell = its crc32
    let c = set("a/t", "30", "a", Cell::Str("pc_para_x".into()));
    let mods = [ModOps { id: "m", sets: vec![&a, &b, &c], adds: vec![] }];
    let m = merge_file(&base(), &idx(), &mods).unwrap();
    assert!(m.notes.is_empty(), "{:?}", m.notes);
    assert_eq!(row(&m.bytes, -5)[1], Value::Int(50));
    assert_eq!(rows(&m.bytes, "N_INFO")[0][1], Value::Int(8));
    assert_eq!(row(&m.bytes, 30)[1], Value::Int(evt_modfmt::crc32(b"pc_para_x") as i32));
}

#[test]
fn bad_ops_are_skipped_the_rest_applies() {
    let ops = [
        set("a/t", "999", "a", Cell::Int(1)),             // row not found
        set("a/t", "10", "nope", Cell::Int(1)),           // unknown column
        set("a/t", "10", "9", Cell::Int(1)),              // past the end of the row
        set("a/t", "10", "name", Cell::Int(1)),           // type
        set("a/t", "10", "a", Cell::Int(1 << 40)),        // does not fit
        set("a/nokey", "10", "id", Cell::Int(1)),         // table without key column
        set("a/t", "30", "b", Cell::Float(9.25)),         // fine
    ];
    let mods = [ModOps { id: "m", sets: ops.iter().collect(), adds: vec![] }];
    let m = merge_file(&base(), &idx(), &mods).unwrap();
    assert_eq!(m.applied, 1);
    assert_eq!(warns(&m).len(), 6, "{:?}", m.notes);
    assert!(warns(&m).iter().all(|w| w.ends_with("(skipped)")));
    assert_eq!(row(&m.bytes, 30)[2], Value::Float(9.25));
    assert!(merge_file(b"not a cfg.bin", &idx(), &[]).is_err());
}

#[test]
fn add_rows() {
    // ma: clone row 30 (with its child REF row) as id 5; mb: a row from scratch keyed by a name (crc32), then a set
    // from ma on the row mb adds (sets run after every add)
    let a1 = add("5", Some("30"), &[("b", Cell::Float(0.5))]);
    let b1 = add("new_row", None, &[("a", Cell::Int(77)), ("name", Cell::Str("nuevo".into()))]);
    let sa = set("a/t", "new_row", "b", Cell::Float(8.0));
    let mods = [ModOps { id: "ma", sets: vec![&sa], adds: vec![&a1] }, ModOps { id: "mb", sets: vec![], adds: vec![&b1] }];
    let m = merge_file(&base(), &idx(), &mods).unwrap();
    assert!(m.notes.is_empty(), "{:?}", m.notes);
    assert_eq!(m.applied, 3);
    let t = T2b::parse(&m.bytes).unwrap();
    let r = rows(&m.bytes, "T_INFO");
    assert_eq!(r.len(), 5);
    assert_eq!(r[3], vec![Value::Int(5), Value::Int(1), Value::Float(0.5), s("thirty")]);
    let h = evt_modfmt::crc32(b"new_row") as i32;
    assert_eq!(r[4], vec![Value::Int(h), Value::Int(77), Value::Float(8.0), s("nuevo")]);
    // the clone kept the child row; count raised; the sort index has 5 entries in unsigned key order
    assert_eq!(t.entries.iter().filter(|e| e.name.as_deref() == Some("T_INFO_REF_X")).count(), 2);
    assert_eq!(t.entries[0].values[0], Value::Int(5));
    let idx_vals: Vec<usize> = t.entries.iter().filter(|e| e.name.as_deref() == Some(SORT_INDEX_NAME)).map(|e| e.values[0].as_int().unwrap() as usize).collect();
    let mut want: Vec<usize> = (0..5).collect();
    want.sort_by_key(|&i| (match r[i][0] { Value::Int(v) => v as u32, _ => 0 }, i));
    assert_eq!(idx_vals, want);
    // the tree still reads 5 rows in the list, and the next list is intact
    let tree = t.tree();
    assert_eq!(tree[0].children.len(), 5);
    assert_eq!(rows(&m.bytes, "N_INFO"), rows(&base(), "N_INFO"));
}

#[test]
fn add_conflicts_and_existing_rows() {
    let a1 = add("7", Some("10"), &[("a", Cell::Int(1))]);
    let b1 = add("7", None, &[("a", Cell::Int(2))]);
    let b2 = add("30", None, &[("a", Cell::Int(3))]); // exists in the game table: replaced (child row dropped)
    let bad = add("8", Some("404"), &[]);
    let mods = [ModOps { id: "ma", sets: vec![], adds: vec![&a1] }, ModOps { id: "mb", sets: vec![], adds: vec![&b1, &b2, &bad] }];
    let m = merge_file(&base(), &idx(), &mods).unwrap();
    let r = rows(&m.bytes, "T_INFO");
    assert_eq!(r.len(), 4);
    assert_eq!(row(&m.bytes, 7), vec![Value::Int(7), Value::Int(2), Value::Float(0.0), Value::String(None)]);
    assert_eq!(row(&m.bytes, 30)[1], Value::Int(3));
    let t = T2b::parse(&m.bytes).unwrap();
    assert_eq!(t.entries.iter().filter(|e| e.name.as_deref() == Some("T_INFO_REF_X")).count(), 0);
    assert_eq!(t.entries[0].values[0], Value::Int(4));
    let w = warns(&m);
    assert!(w.iter().any(|x| x.contains("data conflict: new row a/t[7]: ma, mb (winner: mb)")), "{w:?}");
    assert!(w.iter().any(|x| x.contains("add a/t[30]: the row already exists")), "{w:?}");
    assert!(w.iter().any(|x| x.contains("`from` row 404 not found")), "{w:?}");
}

// ---------------------------------------------------------------- run(): table resolution, overrides, cache

struct Mem {
    files: HashMap<String, PathBuf>,
    reads: usize,
}

impl Source for Mem {
    fn keys(&mut self) -> Result<Vec<String>, String> {
        Ok(self.files.keys().cloned().collect())
    }
    fn read(&mut self, key: &str) -> Result<(Vec<u8>, Vec<PathBuf>), String> {
        self.reads += 1;
        let p = self.files.get(key).ok_or("missing")?;
        Ok((std::fs::read(p).map_err(|e| e.to_string())?, vec![p.clone()]))
    }
}

fn write(p: &Path, t: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, t).unwrap();
}

#[test]
fn run_resolves_caches_and_invalidates() {
    let root = std::env::temp_dir().join(format!("evt_merge_run_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mods = root.join("mods");
    let cache = root.join("evt_loader").join(CACHE_REL);
    let game = root.join("game_base.cfg.bin");
    write(&game, &base());
    // an older version of the same table must not be picked
    let old = root.join("old.cfg.bin");
    write(&old, b"old");
    for (id, prio, delta) in [
        ("ma", 0, "[[set]]\nkey = \"10\"\ncolumn = \"a\"\nvalue = 1\n"),
        ("mb", 1, "[[set]]\nkey = \"10\"\ncolumn = \"a\"\nvalue = 2\n[[set]]\nkey = \"30\"\ncolumn = \"name\"\nvalue = \"b\"\n"),
        ("mc", 2, "[[set]]\ntable = \"a/maps\"\nkey = \"1\"\ncolumn = \"id\"\nvalue = 1\n[[set]]\ntable = \"zz/none\"\nkey = \"1\"\ncolumn = 0\nvalue = 1\n"),
    ] {
        write(&mods.join(id).join("mod.toml"), format!("id=\"{id}\"\nversion=\"1\"\npriority={prio}\n").as_bytes());
        write(&mods.join(id).join("data/a/t.toml"), delta.as_bytes());
    }
    let mut src = Mem {
        files: [
            (KEY.to_string(), game.clone()),
            ("data/common/gamedata/a/t_0.90.00.00.cfg.bin".to_string(), old.clone()),
            ("data/common/gamedata/map/w01/m.cfg.bin".to_string(), old.clone()),
            ("data/common/gamedata/map/w02/m.cfg.bin".to_string(), old.clone()),
        ]
        .into_iter()
        .collect(),
        reads: 0,
    };
    let plan = evt_modfmt::plan_root(&mods);
    let o = run_with(&plan, &mut src, &cache, &idx());
    assert_eq!(src.reads, 1);
    assert_eq!(o.served.len(), 1, "{:?}", o.notes);
    let (key, path, label) = &o.served[0];
    assert_eq!(key, KEY);
    assert_eq!(label, "delta merge [ma, mb]");
    assert!(path.starts_with(&cache));
    let merged = std::fs::read(path).unwrap();
    assert_eq!(row(&merged, 10)[1], Value::Int(2));
    assert_eq!(row(&merged, 30)[3], s("b"));
    let text: Vec<&str> = o.notes.iter().map(|(_, s)| s.as_str()).collect();
    assert!(text.iter().any(|t| t.contains("data conflict: cell a/t[10].a: ma = 1, mb = 2 (winner: mb)")), "{text:?}");
    assert!(text.iter().any(|t| t.contains("data table a/maps: table `a/maps` spans 2 files")), "{text:?}");
    assert!(text.iter().any(|t| t.contains("unknown table `zz/none`")), "{text:?}");
    assert!(text.last().unwrap().contains("cache rebuilt"));

    // same inputs: served from the cache, nothing read, the notes repeated
    let o2 = run_with(&plan, &mut src, &cache, &idx());
    assert_eq!(src.reads, 1);
    assert_eq!(o2.served, o.served);
    assert!(o2.notes.last().unwrap().1.contains("from the cache"), "{:?}", o2.notes);
    assert!(o2.notes.iter().any(|(_, t)| t.contains("winner: mb")));

    // a delta changes -> rebuilt
    std::thread::sleep(std::time::Duration::from_millis(20));
    write(&mods.join("ma/data/a/t.toml"), b"[[set]]\nkey = \"-5\"\ncolumn = \"a\"\nvalue = 9\n");
    let plan = evt_modfmt::plan_root(&mods);
    let o3 = run_with(&plan, &mut src, &cache, &idx());
    assert_eq!(src.reads, 2);
    let merged = std::fs::read(&o3.served[0].1).unwrap();
    assert_eq!(row(&merged, -5)[1], Value::Int(9));
    assert_eq!(row(&merged, 10)[1], Value::Int(2));

    // the game file changes (size / mtime) -> rebuilt
    let mut b = T2b::parse(&base()).unwrap();
    b.entries[3].values[3] = s("minus-changed");
    write(&game, &b.to_bytes().unwrap());
    let o4 = run_with(&plan, &mut src, &cache, &idx());
    assert_eq!(src.reads, 3);
    assert_eq!(row(&std::fs::read(&o4.served[0].1).unwrap(), -5)[3], s("minus-changed"));

    // a whole-file override of the table becomes the base (the game file is not read)
    let mut ov = T2b::parse(&base()).unwrap();
    ov.entries[1].values[3] = s("override");
    write(&mods.join("mb/files").join(KEY), &ov.to_bytes().unwrap());
    let plan = evt_modfmt::plan_root(&mods);
    let o5 = run_with(&plan, &mut src, &cache, &idx());
    assert_eq!(src.reads, 3);
    assert!(o5.notes.iter().any(|(_, t)| t.contains("merged over the override of mb")), "{:?}", o5.notes);
    let merged = std::fs::read(&o5.served[0].1).unwrap();
    assert_eq!(row(&merged, 30)[3], s("b")); // mb's cell over mb's own file
    assert_eq!(row(&merged, -5)[1], Value::Int(9));

    // no delta left -> nothing served, nothing read
    for id in ["ma", "mb", "mc"] {
        std::fs::remove_dir_all(mods.join(id).join("data")).unwrap();
    }
    let plan = evt_modfmt::plan_root(&mods);
    let o6 = run_with(&plan, &mut src, &cache, &idx());
    assert!(o6.served.is_empty() && o6.notes.is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// The real retail chara_param of the v7.1.2 dump (read-only; skipped when the dump is not there), through the
/// game-folder source (its cpk_list names CPKs the dump does not have: the extracted loose file is read).
#[test]
fn retail_chara_param_from_the_dump() {
    let dump = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/v7.1.2"));
    if !dump.join("data").join("cpk_list.cfg.bin").is_file() {
        eprintln!("dump not found: skipped");
        return;
    }
    let root = std::env::temp_dir().join(format!("evt_merge_retail_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let mods = root.join("mods");
    // first retail row: id -1046485492; mod A changes its element, mod B its main position; B also adds a clone
    write(&mods.join("da/mod.toml"), b"id=\"da\"\nversion=\"1\"\npriority=0\n");
    write(&mods.join("da/data/character/chara_param.toml"), b"[[set]]\nkey = \"-1046485492\"\ncolumn = \"element\"\nvalue = 5\n");
    write(&mods.join("db/mod.toml"), b"id=\"db\"\nversion=\"1\"\npriority=1\n");
    write(
        &mods.join("db/data/character/chara_param.toml"),
        b"[[set]]\nkey = \"0xC19FE60C\"\ncolumn = \"main_position\"\nvalue = 1\n[[add]]\nkey = \"evt_test_delta_row\"\nfrom = \"-1046485492\"\nvalues = { rank = 4 }\n",
    );
    let plan = evt_modfmt::plan_root(&mods);
    assert!(!plan.has_errors(), "{:?}", plan.issues);
    let t0 = std::time::Instant::now();
    let mut src = GameSource::new(dump);
    let o = run(&plan, &mut src, &root.join("cache"));
    let cold = t0.elapsed();
    assert_eq!(o.served.len(), 1, "{:?}", o.notes);
    let (key, path, _) = &o.served[0];
    assert_eq!(key, "data/common/gamedata/character/chara_param_1.03.66.00.cfg.bin");
    let base = std::fs::read(dump.join(key)).unwrap();
    let merged = std::fs::read(path).unwrap();
    let b = rows(&base, "CHARA_PARAM_INFO");
    let m = rows(&merged, "CHARA_PARAM_INFO");
    assert_eq!(m.len(), b.len() + 1);
    let first = m.iter().find(|r| r[0] == Value::Int(-1046485492)).unwrap();
    assert_eq!(first[2], Value::Int(5));
    assert_eq!(first[3], Value::Int(1));
    // every other cell of that row and every other base row unchanged
    assert_eq!(first[4..], b[0][4..]);
    assert_eq!(&m[1..b.len()], &b[1..]);
    let new = m.last().unwrap();
    assert_eq!(new[0], Value::Int(evt_modfmt::crc32(b"evt_test_delta_row") as i32));
    assert_eq!(new[9], Value::Int(4));
    // cloned before the sets ran (adds first): the retail values
    assert_eq!(new[1..9], b[0][1..9]);
    let t = T2b::parse(&merged).unwrap();
    let beg = t.entries.iter().find(|e| e.name.as_deref() == Some("CHARA_PARAM_INFO_LIST_BEG")).unwrap();
    assert_eq!(beg.values[0], Value::Int(b.len() as i32 + 1));
    // one row + one __SORT_INDEX entry more
    assert_eq!(t.entries.len(), base_entries(&base) + 2);
    assert_eq!(t.entries.iter().filter(|e| e.name.as_deref() == Some(SORT_INDEX_NAME)).count(), m.len());
    assert!(o.notes.iter().all(|(l, _)| *l == Lvl::Info), "{:?}", o.notes);
    // warm start
    let t1 = std::time::Instant::now();
    let o2 = run(&plan, &mut GameSource::new(dump), &root.join("cache"));
    let warm = t1.elapsed();
    assert!(o2.notes.last().unwrap().1.contains("from the cache"));
    eprintln!("retail chara_param merge: cold {cold:?}, cached {warm:?}; {}", o.notes.last().unwrap().1);
    let _ = std::fs::remove_dir_all(&root);
}

fn base_entries(b: &[u8]) -> usize {
    T2b::parse(b).unwrap().entries.len()
}
