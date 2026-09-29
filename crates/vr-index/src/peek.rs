//! Debug helper: print rows of a cfg.bin read from the game (used while writing extractors).
use l5_core::CfgBin;

pub fn peek(bytes: &[u8], lists: &[String], n: usize) -> String {
    let mut out = String::new();
    match CfgBin::parse(bytes) {
        Ok(CfgBin::T2b(t)) => {
            let mut cnt: std::collections::BTreeMap<String, usize> = Default::default();
            for e in &t.entries {
                let name = e.display_name().into_owned();
                let c = cnt.entry(name.clone()).or_default();
                *c += 1;
                if (lists.is_empty() || lists.iter().any(|l| *l == name)) && *c <= n {
                    let vals: Vec<String> = e.values.iter().map(|v| match v {
                        l5_core::t2b::Value::Int(i) => format!("{i}"),
                        l5_core::t2b::Value::Float(f) => format!("{f}f"),
                        l5_core::t2b::Value::String(s) => format!("{s:?}"),
                    }).collect();
                    out.push_str(&format!("{name} [{}]\n", vals.join(", ")));
                }
            }
            out.push_str(&format!("counts: {cnt:?}\n"));
        }
        Ok(CfgBin::Rdbn(r)) => {
            for l in &r.lists {
                if !lists.is_empty() && !lists.iter().any(|x| *x == l.name) { 
                    out.push_str(&format!("list {} ({} rows)\n", l.name, l.rows.len()));
                    continue; }
                let ty = &r.types[l.type_index];
                let f: Vec<String> = ty.fields.iter().map(|f| format!("{}:{}x{}", f.name, f.type_name(), f.count)).collect();
                out.push_str(&format!("list {} ({} rows) fields {:?}\n", l.name, l.rows.len(), f));
                for row in l.rows.iter().take(n) {
                    out.push_str(&format!("  {:?}\n", row));
                }
            }
        }
        Err(e) => out.push_str(&format!("parse error {e}\n")),
    }
    out
}
