//! Coverage statistics of a built index: `cargo run --example stats -- <index dir>`.
use std::collections::BTreeMap;
fn main() {
    let dir = std::env::args().nth(1).expect("index dir");
    let t0 = std::time::Instant::now();
    let idx = vr_index::Index::load(&dir).unwrap();
    println!("load {:?}", t0.elapsed());
    let mut st: BTreeMap<&str, (usize, usize, usize, usize)> = BTreeMap::new(); // total, no name(own), hex id, thumb
    for e in idx.entities() {
        let s = st.entry(e.category.as_str()).or_default();
        s.0 += 1;
        if idx.names(e).iter().all(|n| n.is_empty()) { s.1 += 1; }
        if e.id.starts_with("0x") { s.2 += 1; }
        if e.thumb.is_some() { s.3 += 1; }
    }
    println!("{:<12} {:>6} {:>8} {:>6} {:>6}", "category", "total", "no-name", "hexid", "thumb");
    for (k, v) in st { println!("{k:<12} {:>6} {:>8} {:>6} {:>6}", v.0, v.1, v.2, v.3); }
    for q in ["tornado fuego", "mark evans", "mrak evnas", "raimon", "canon", "bg10000"] {
        let t = std::time::Instant::now();
        let h = idx.search(&vr_index::Query::new(q).lang(2).limit(20));
        println!("search {q:?}: {} hits in {:?}", h.len(), t.elapsed());
    }
    for f in idx.text_files() { print!("{f}({}) ", idx.text_table(f).map_or(0, |t| t.len())); }
    println!();
    for w in &idx.meta().warnings { println!("warn: {w}"); }
}
