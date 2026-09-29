use super::*;

fn doc_with(entries: &[(&str, Vec<Value>)]) -> T2b {
    let mut d = T2b::default();
    for (n, v) in entries {
        let e = d.new_entry(n, v.clone()).unwrap();
        d.entries.push(e);
    }
    d
}

fn s(v: &str) -> Value {
    Value::String(Some(v.to_owned()))
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

#[test]
fn chat_text_layout() {
    // Shape of common/text/en/chat_text.cfg.bin (cfgbin.md §1.8)
    let doc = doc_with(&[
        ("TEXT_INFO_BEGIN", vec![Value::Int(2)]),
        (
            "TEXT_INFO",
            vec![Value::Int(0x55FB_BA47), Value::Int(0), s("Hello!")],
        ),
        ("TEXT_INFO", vec![Value::Int(1), Value::Int(0), s("Yay!")]),
        ("TEXT_INFO", vec![Value::Int(2), Value::Int(0), s("Yay!")]),
        ("TEXT_INFO_END", vec![]),
    ]);
    let b = doc.to_bytes().unwrap();
    assert_eq!(b.len() % 16, 0);
    assert_eq!(u32_at(&b, 0), 5);
    // record 0: crc, n=1, types=0x01, FF FF, value
    assert_eq!(u32_at(&b, 0x10), 0x0EFB_9738);
    assert_eq!(&b[0x14..0x18], &[1, 1, 0xFF, 0xFF]);
    // record 1: n=3 types [int,int,str] = 0b00_01_01 = 0x05
    assert_eq!(&b[0x1C + 4..0x1C + 8], &[3, 5, 0xFF, 0xFF]);
    // END: crc, 00, FF FF FF
    let s_off = u32_at(&b, 4) as usize;
    assert_eq!(s_off % 16, 0);
    // exact dedup: "Hello!\0Yay!\0" -> 12 bytes, 2 strings
    assert_eq!(u32_at(&b, 8), 12);
    assert_eq!(u32_at(&b, 12), 2);
    assert_eq!(&b[s_off..s_off + 12], b"Hello!\0Yay!\0");
    // key section: 3 distinct names
    let k = s_off + 16;
    assert_eq!(u32_at(&b, k + 4), 3);
    assert_eq!(u32_at(&b, k + 8), 0x30);
    assert_eq!(u32_at(&b, k + 12), 40); // "TEXT_INFO_BEGIN\0TEXT_INFO\0TEXT_INFO_END\0"
    assert_eq!(
        &b[b.len() - 16..],
        &[
            1, 0x74, 0x32, 0x62, 0xFE, 1, 1, 0, 1, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF
        ]
    );

    let back = T2b::parse(&b).unwrap();
    assert_eq!(back.entries, doc.entries);
    assert_eq!(back.string_dedup, StringDedup::Exact);
    assert_eq!(back.hash_kind, HashKind::Crc32);
    assert_eq!(back.to_bytes().unwrap(), b);
}

#[test]
fn dedup_none_detected() {
    let mut doc = doc_with(&[(
        "EV",
        vec![s("a"), s("a"), Value::String(None), Value::Float(-0.5)],
    )]);
    doc.string_dedup = StringDedup::None;
    let b = doc.to_bytes().unwrap();
    assert_eq!(u32_at(&b, 12), 2);
    let back = T2b::parse(&b).unwrap();
    assert_eq!(back.string_dedup, StringDedup::None);
    assert_eq!(back.to_bytes().unwrap(), b);
    // with no duplicates the style is ambiguous and reads back as Exact (same bytes)
    let doc = doc_with(&[("EV", vec![s("a"), s("b")])]);
    assert_eq!(
        T2b::parse(&doc.to_bytes().unwrap()).unwrap().string_dedup,
        StringDedup::Exact
    );
}

#[test]
fn shift_jis_file() {
    let mut doc = T2b::default();
    doc.footer.encoding = 0;
    let e = doc
        .new_entry("名前", vec![s("円堂守"), Value::Int(-1)])
        .unwrap();
    // name hash is over the Shift-JIS bytes
    assert_eq!(
        e.hash,
        hash::crc32(&TextEncoding::ShiftJis.encode("名前").unwrap())
    );
    doc.entries.push(e);
    let b = doc.to_bytes().unwrap();
    let back = T2b::parse(&b).unwrap();
    assert_eq!(back.text_encoding(), TextEncoding::ShiftJis);
    assert_eq!(back.entries, doc.entries);
    assert_eq!(back.to_bytes().unwrap(), b);
    // unencodable text is an error, not a panic
    doc.entries[0].values[0] = s("😀");
    assert!(matches!(doc.to_bytes(), Err(Error::Unencodable { .. })));
}

#[test]
fn many_values_and_empty() {
    let vals: Vec<Value> = (0..255)
        .map(|i| {
            if i % 3 == 0 {
                Value::Float(i as f32)
            } else {
                Value::Int(i)
            }
        })
        .collect();
    let doc = doc_with(&[("BIG", vals), ("NONE", vec![])]);
    let b = doc.to_bytes().unwrap();
    assert_eq!(T2b::parse(&b).unwrap().entries, doc.entries);
    let empty = T2b::default().to_bytes().unwrap();
    assert!(T2b::parse(&empty).unwrap().entries.is_empty());
    let too_many = doc_with(&[("X", vec![Value::Int(0); 256])]);
    assert!(too_many.to_bytes().is_err());
}

#[test]
fn unresolved_names_kept() {
    let mut doc = doc_with(&[("A", vec![Value::Int(1)])]);
    doc.entries.push(Entry {
        name: None,
        hash: 0x1234_5678,
        values: vec![],
    });
    let b = doc.to_bytes().unwrap();
    let back = T2b::parse(&b).unwrap();
    assert_eq!(back.entries[1].name, None);
    assert_eq!(back.entries[1].hash, 0x1234_5678);
    assert_eq!(back.entries[1].display_name(), "#12345678");
    assert_eq!(back.to_bytes().unwrap(), b);
}

fn table_doc() -> T2b {
    doc_with(&[
        ("TEAM_LIST_BEG", vec![Value::Int(3), Value::Int(1)]),
        ("TEAM", vec![Value::Int(-5), s("b")]),
        ("TEAM_REF_MEMBER", vec![Value::Int(0), Value::Int(2)]),
        ("TEAM_SKILL_LIST_BEG", vec![Value::Int(1)]),
        ("TEAM_SKILL", vec![Value::Int(9)]),
        ("TEAM", vec![Value::Int(7), s("a")]),
        ("TEAM_REF_MEMBER", vec![Value::Int(2), Value::Int(1)]),
        ("TEAM", vec![Value::Int(7), s("c")]),
        (SORT_INDEX_NAME, vec![Value::Int(1)]),
        (SORT_INDEX_NAME, vec![Value::Int(2)]),
        (SORT_INDEX_NAME, vec![Value::Int(0)]),
        ("TEXT_BEGIN", vec![Value::Int(1)]),
        ("TEXT", vec![s("x")]),
        ("TEXT_END", vec![]),
    ])
}

#[test]
fn counted_tree() {
    let doc = table_doc();
    let tree = doc.tree();
    assert_eq!(tree.len(), 2);
    let list = &tree[0];
    assert_eq!(list.entry, 0);
    assert_eq!(list.children.len(), 3);
    assert_eq!(list.children[0].entry, 1);
    // row 0 owns its REF and its nested counted sub-list
    assert_eq!(list.children[0].children.len(), 2);
    assert_eq!(list.children[0].children[1].children[0].entry, 4);
    assert_eq!(list.children[1].children[0].entry, 6);
    assert_eq!(list.sort_index, Some(vec![8, 9, 10]));
    assert_eq!(tree[1].entry, 11);
    assert_eq!(tree[1].end, Some(13));
    assert_eq!(tree[1].children.len(), 1);

    // every entry covered exactly once
    fn walk(n: &Node, seen: &mut Vec<usize>) {
        seen.push(n.entry);
        n.children.iter().for_each(|c| walk(c, seen));
        seen.extend(n.end);
        seen.extend(n.sort_index.iter().flatten());
    }
    for mode in [TreeMode::Counted, TreeMode::Editor] {
        let mut seen = Vec::new();
        build_tree(&doc.entries, mode)
            .iter()
            .for_each(|n| walk(n, &mut seen));
        seen.sort();
        assert_eq!(seen, (0..doc.entries.len()).collect::<Vec<_>>(), "{mode:?}");
    }
    // editor mode: the counted list has no END and swallows everything after it
    assert_eq!(build_tree(&doc.entries, TreeMode::Editor).len(), 1);

    let json = serde_json::to_string(&tree).unwrap();
    assert!(json.contains("\"sortIndex\":[8,9,10]"));
    let back: Vec<Node> = serde_json::from_str(&json).unwrap();
    assert_eq!(back, tree);
}

#[test]
fn sort_index_rebuild() {
    let mut doc = table_doc();
    let orig = doc.to_bytes().unwrap();
    // unsigned order: 7 (row1), 7 (row2), -5 = 0xFFFFFFFB (row0) -> [1, 2, 0], already correct
    assert_eq!(doc.rebuild_sort_indexes().unwrap(), 1);
    assert_eq!(doc.to_bytes().unwrap(), orig);

    // append a row with the smallest id, bump the count: a 4th __SORT_INDEX is inserted
    let row = doc.new_entry("TEAM", vec![Value::Int(1), s("d")]).unwrap();
    doc.entries.insert(8, row);
    doc.entries[0].values[0] = Value::Int(4);
    doc.rebuild_sort_indexes().unwrap();
    let idx: Vec<i32> = doc
        .entries
        .iter()
        .filter(|e| e.name.as_deref() == Some(SORT_INDEX_NAME))
        .map(|e| e.values[0].as_int().unwrap())
        .collect();
    assert_eq!(idx, vec![3, 1, 2, 0]);
    assert_eq!(doc.entries[13].name.as_deref(), Some("TEXT_BEGIN"));
}

#[test]
fn json_round_trip() {
    let doc = table_doc();
    let json = serde_json::to_string(&doc).unwrap();
    assert!(json.contains(r#"{"type":"int","value":3}"#));
    assert!(json.contains(r#"{"type":"string","value":"b"}"#));
    let back: T2b = serde_json::from_str(&json).unwrap();
    assert_eq!(back.to_bytes().unwrap(), doc.to_bytes().unwrap());
}

#[test]
fn malformed_never_panics() {
    let b = table_doc().to_bytes().unwrap();
    for n in 0..b.len() {
        let _ = T2b::parse(&b[..n]);
        let mut t = b[n..].to_vec();
        if t.len() >= 16 {
            let _ = T2b::parse(&t);
        }
        t = b.clone();
        t[n] ^= 0xFF;
        let _ = T2b::parse(&t);
        t[n] = 0x7F;
        let _ = T2b::parse(&t);
    }
    assert!(matches!(T2b::parse(b"short"), Err(Error::NotT2b)));
}

#[test]
fn sort_key_on_later_field() {
    // like trophy_config TROPHY_INFO: the index orders rows by value 1, not value 0
    let mut doc = doc_with(&[
        ("INFO_LIST_BEG", vec![Value::Int(3), Value::Int(1)]),
        ("INFO", vec![Value::Int(1), Value::Int(30)]),
        ("INFO", vec![Value::Int(2), Value::Int(10)]),
        ("INFO", vec![Value::Int(3), Value::Int(20)]),
        (SORT_INDEX_NAME, vec![Value::Int(1)]),
        (SORT_INDEX_NAME, vec![Value::Int(2)]),
        (SORT_INDEX_NAME, vec![Value::Int(0)]),
        ("INFO_LIST_END", vec![]),
    ]);
    let lists = doc.sorted_lists();
    assert_eq!(
        lists,
        vec![SortedList {
            begin: 0,
            name: "INFO_LIST_BEG".into(),
            rows: 3,
            key_field: 1
        }]
    );
    let orig = doc.entries.clone();
    doc.rebuild_sort_indexes().unwrap();
    assert_eq!(doc.entries, orig);

    // edit the key of row 0 so the stale index no longer matches, rebuild with the saved keys
    doc.entries[1].values[1] = Value::Int(5);
    doc.rebuild_sort_indexes_with(&lists).unwrap();
    let idx: Vec<i32> = doc.entries[4..7]
        .iter()
        .map(|e| e.values[0].as_int().unwrap())
        .collect();
    assert_eq!(idx, vec![0, 1, 2]);
    assert_eq!(doc.entries[7].name.as_deref(), Some("INFO_LIST_END"));
}
