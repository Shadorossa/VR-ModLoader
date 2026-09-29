//! Game path patterns: schema placeholders (`{ver}`, `{lang}`…), `*` wildcards and file name versions.

/// Turn schema placeholders (`{ver}`, `{map}`, `{lang}`…) into `*`.
pub fn normalize_pattern(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    let mut depth = 0;
    for ch in pattern.chars() {
        match ch {
            '{' => {
                if depth == 0 {
                    out.push('*');
                }
                depth += 1;
            }
            '}' if depth > 0 => depth -= 1,
            _ if depth > 0 => {}
            _ => out.push(ch),
        }
    }
    if !out.starts_with("data/") {
        out.insert_str(0, if out.starts_with("common/") || out.starts_with("dx11/") { "data/" } else { "" });
    }
    out
}

/// Wildcard match where `*` matches any run of characters except `/`.
pub fn wildcard(pattern: &str, text: &str) -> bool {
    fn go(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') => (0..=t.len()).take_while(|&i| i == 0 || t[i - 1] != b'/').any(|i| go(&p[1..], &t[i..])),
            Some(&c) => t.first().is_some_and(|&x| x.eq_ignore_ascii_case(&c)) && go(&p[1..], &t[1..]),
        }
    }
    go(pattern.as_bytes(), text.as_bytes())
}

/// Version key of a file name like `chara_base_1.03.98.00.cfg.bin` → [1, 3, 98, 0].
pub fn version_key(name: &str) -> Vec<u32> {
    let stem = name.split(".cfg.bin").next().unwrap_or(name);
    let last = stem.rsplit('_').next().unwrap_or("");
    last.split('.').map(|x| x.parse::<u32>().unwrap_or(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(wildcard("data/a/chara_base_*.cfg.bin", "data/a/chara_base_1.03.98.00.cfg.bin"));
        assert!(!wildcard("data/a/*.cfg.bin", "data/a/b/c.cfg.bin"));
        assert!(version_key("x_1.03.98.00.cfg.bin") > version_key("x_1.03.9.00.cfg.bin"));
    }

    #[test]
    fn placeholders() {
        assert_eq!(normalize_pattern("common/gamedata/chara_base_{ver}.cfg.bin"), "data/common/gamedata/chara_base_*.cfg.bin");
    }
}
