use super::*;
use crate::hash::crc32_str;

fn field(name: &str, ty: i16, category: i16, size: i32, count: u32) -> Field {
    Field {
        name: name.into(),
        hash: crc32_str(name),
        ty,
        category,
        size,
        count,
        stored_offset: 0,
    }
}

fn ty(name: &str, fields: Vec<Field>) -> Type {
    Type {
        name: name.into(),
        hash: crc32_str(name),
        unk_hash: 0xDEAD_BEEF,
        fields,
    }
}

fn list(name: &str, type_index: usize, indexed: bool, rows: Vec<Row>) -> List {
    List {
        name: name.into(),
        hash: crc32_str(name),
        type_index,
        unk1: 2,
        indexed,
        key_field: 0,
        sort_index: None,
        rows,
    }
}

fn one(v: Value) -> Vec<Value> {
    vec![v]
}

fn sample() -> Rdbn {
    use field_type::*;
    let cond = ty(
        "COND",
        vec![
            field("kind", SBYTE, 1, 1, 1),
            field("text", STRING, 3, 4, 1),
        ],
    );
    let main = ty(
        "MAIN",
        vec![
            field("id", HASH, 3, 4, 1),
            field("flag", BOOL, 1, 1, 1),
            field("red", BYTE, 1, 1, 4), // array: stored offsets count it as 1
            field("slice", DATA_TUPLE, 3, 4, 1),
            field("name", STRING, 3, 4, 1),
            field("cond", 0, 2, 8, 1),
            field("pos", POSITION, 3, 16, 1),
            field("xy", POSITION_2D, 3, 8, 1),
            field("act", ACT_TYPE, 1, 2, 1),
        ],
    );
    let orphan = ty("ORPHAN", vec![field("v", FLOAT, 1, 4, 1)]);
    let row = |id: u32, name: Option<&str>, t: &str| -> Row {
        vec![
            one(Value::Hash(id)),
            one(Value::Bool(id.is_multiple_of(2))),
            vec![
                Value::Byte(1),
                Value::Byte(2),
                Value::Byte(3),
                Value::Byte(4),
            ],
            one(Value::Tuple([id as i16, 2])),
            one(Value::String(name.map(Into::into))),
            one(Value::Struct(vec![
                one(Value::SByte(-1)),
                one(Value::String(Some(t.into()))),
            ])),
            one(Value::Vec4([1.0, 2.0, 3.0, 4.0])),
            one(Value::Vec2([0.5, -0.5])),
            one(Value::Short(-3)),
        ]
    };
    Rdbn {
        version: VERSION,
        types: vec![cond, main, orphan],
        lists: vec![
            list(
                "m_List",
                1,
                true,
                vec![
                    row(0x9000_0000, Some("b"), "t1"),
                    row(5, None, "b"),
                    row(3, Some("id"), "t1"),
                ],
            ),
            list("m_Empty", 2, false, vec![]),
        ],
        trailing_strings: Vec::new(),
    }
}

#[test]
fn layout_quirks() {
    let doc = sample();
    let lay = layout(&doc.types[1].fields).unwrap();
    // id@0, flag@4, red[4]@5..9, slice(align 2)@10, name@16, cond@20, pos(align 16)@32, xy@48, act@56
    assert_eq!(lay.real_offsets, vec![0, 4, 5, 10, 16, 20, 32, 48, 56]);
    // stored offsets treat red[4] as red[1]: slice@6, name@12, cond@16, pos@32, xy@48, act@56
    assert_eq!(lay.stored_offsets, vec![0, 4, 5, 6, 12, 16, 32, 48, 56]);
    assert_eq!((lay.size, lay.align), (64, 16));
    assert_eq!(layout(&doc.types[0].fields).unwrap().size, 8);
}

#[test]
fn round_trip() {
    let doc = sample();
    let b = doc.to_bytes().unwrap();
    assert_eq!(&b[..4], b"RDBN");
    assert_eq!(u16::from_le_bytes([b[4], b[5]]), 0x50);
    assert_eq!(
        u32::from_le_bytes(b[12..16].try_into().unwrap()) as usize,
        b.len() - 0x50
    );
    let back = Rdbn::parse(&b).unwrap();
    assert_eq!(back.types[1].fields[3].stored_offset, 6);
    assert_eq!(back.types[0].unk_hash, 0xDEAD_BEEF);
    let l = &back.lists[0];
    assert!(l.indexed);
    // unsigned order: 3 (row2), 5 (row1), 0x90000000 (row0)
    assert_eq!(l.sort_index, Some(vec![2, 1, 0]));
    assert_eq!(l.key_field, 0);
    assert_eq!(l.rows, doc.lists[0].rows);
    assert_eq!(back.lists[1].rows.len(), 0);
    assert!(back.trailing_strings.is_empty());
    assert_eq!(back.to_bytes().unwrap(), b);
    // string pool: exact dedup, game order (types, fields, then per list name + values);
    // "id" is both a field name and a value and is stored once, "b" once
    let pool_start = 0x50 + i32::from_le_bytes(b[0x38..0x3C].try_into().unwrap()) as usize;
    let pool = &b[pool_start..];
    assert!(pool.starts_with(b"COND\0kind\0text\0MAIN\0id\0"));
    assert!(pool.ends_with(b"m_List\0b\0t1\0m_Empty\0"));
}

#[test]
fn trailing_pool_bytes_kept() {
    // soccer_common_text.cfg.bin: no types, no lists, unreferenced strings (rdbn.md §9)
    let doc = Rdbn {
        trailing_strings: "未使用\0文字列\0".as_bytes().to_vec(),
        ..Rdbn::default()
    };
    let b = doc.to_bytes().unwrap();
    let back = Rdbn::parse(&b).unwrap();
    assert_eq!(back.trailing_strings, doc.trailing_strings);
    assert_eq!(back.to_bytes().unwrap(), b);
}

#[test]
fn key_field_detection() {
    let mut doc = sample();
    // key on a later field: ids ascend with the row order, `act` descends
    doc.lists[0].key_field = 8;
    for (i, r) in doc.lists[0].rows.iter_mut().enumerate() {
        r[0] = one(Value::Hash(i as u32 + 1));
        r[1] = one(Value::Bool(true));
        r[3] = one(Value::Tuple([i as i16, 2]));
        r[8] = one(Value::Short(10 - i as i16));
    }
    let b = doc.to_bytes().unwrap();
    let back = Rdbn::parse(&b).unwrap();
    assert_eq!(back.lists[0].sort_index, Some(vec![2, 1, 0]));
    assert_eq!(back.lists[0].key_field, 8);
    assert_eq!(back.to_bytes().unwrap(), b);
}

#[test]
fn type_mismatch_is_error() {
    let mut doc = sample();
    doc.lists[0].rows[0][0] = one(Value::Short(1));
    assert!(matches!(doc.to_bytes(), Err(Error::TypeMismatch(_))));
    let mut doc = sample();
    doc.lists[0].rows[0][2].pop();
    assert!(matches!(doc.to_bytes(), Err(Error::TypeMismatch(_))));
}

#[test]
fn views_and_json() {
    let doc = sample();
    let json = serde_json::to_string(&doc).unwrap();
    let back: Rdbn = serde_json::from_str(&json).unwrap();
    assert_eq!(back.to_bytes().unwrap(), doc.to_bytes().unwrap());

    let t = doc.table(0).unwrap();
    let v = serde_json::to_value(&t).unwrap();
    assert_eq!(v["name"], "m_List");
    assert_eq!(v["typeName"], "MAIN");
    assert_eq!(v["columns"][5]["type"], "Struct");
    assert_eq!(v["columns"][5]["structType"], "COND");
    let r0 = &v["rows"][0];
    assert_eq!(r0["id"], 0x9000_0000u32);
    assert_eq!(r0["red"], serde_json::json!([1, 2, 3, 4]));
    assert_eq!(r0["slice"], serde_json::json!([0, 2]));
    assert_eq!(r0["cond"], serde_json::json!({"kind": -1, "text": "t1"}));
    assert_eq!(v["rows"][1]["name"], serde_json::Value::Null);
    assert_eq!(doc.tables().len(), 2);
}

#[test]
fn malformed_never_panics() {
    let b = sample().to_bytes().unwrap();
    for n in 0..b.len() {
        let _ = Rdbn::parse(&b[..n]);
        let mut t = b.clone();
        t[n] ^= 0xFF;
        let _ = Rdbn::parse(&t);
        t[n] = 0x80;
        let _ = Rdbn::parse(&t);
    }
    assert!(matches!(Rdbn::parse(b"RDBN"), Err(Error::NotRdbn)));
}
