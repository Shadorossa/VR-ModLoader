//! `evt_loader\config.toml` `[modules]` switches, written the way the app writes them
//! (`app/src-tauri/src/loader.rs`: `apply_project_modules` / `set_module_in`; the app crate is not a library, so the
//! line rules are repeated here: same output for the same input).

/// `name = true|false` lines of `evt_loader_modules.toml` (`#` comments), in file order.
pub fn parse_module_list(text: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let l = line.split('#').next().unwrap_or("").trim();
        let Some((k, v)) = l.split_once('=') else { continue };
        let name = k.trim();
        if name.is_empty() || name.starts_with('[') {
            continue;
        }
        out.push((name.to_string(), v.trim() == "true"));
    }
    out
}

/// `config.toml` text with `[modules] name = enabled` (added at the end of `[modules]` when missing).
/// CRLF line ends and runs of blank lines collapsed, as the app does.
pub fn set_module(text: &str, name: &str, enabled: bool) -> Result<String, String> {
    let plain = !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !plain {
        return Err(format!("nombre de módulo no válido: «{name}»"));
    }
    let mut section = String::new();
    let mut found = false;
    let mut out: Vec<String> = Vec::new();
    for line in text.replace('\r', "").lines() {
        if line.trim().is_empty() && out.last().is_some_and(|l| l.trim().is_empty()) {
            continue;
        }
        let t = line.trim();
        if t.starts_with('[') {
            section = t.split('#').next().unwrap_or("").trim().trim_matches(|c| c == '[' || c == ']').to_string();
        }
        let key = t.split('=').next().unwrap_or("").trim();
        if section == "modules" && key == name && t.contains('=') {
            let comment = line.find('#').map(|i| format!("  {}", line[i..].trim())).unwrap_or_default();
            out.push(format!("{name} = {enabled}{comment}"));
            found = true;
        } else {
            out.push(line.to_string());
        }
    }
    if !found {
        let start = out.iter().position(|l| l.trim().starts_with("[modules]")).ok_or("config.toml no tiene [modules]")?;
        let mut at = start + 1;
        while at < out.len() && !out[at].trim().starts_with('[') {
            at += 1;
        }
        while at > start + 1 && out[at - 1].trim().is_empty() {
            at -= 1;
        }
        out.insert(at, format!("{name} = {enabled}"));
    }
    Ok(out.join("\r\n") + "\r\n")
}

/// Current `[modules]` switches of a config text.
pub fn modules_of(text: &str) -> Vec<(String, bool)> {
    let mut section = String::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.split('#').next().unwrap_or("").trim();
        if t.starts_with('[') {
            section = t.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            continue;
        }
        if section != "modules" {
            continue;
        }
        if let Some((k, v)) = t.split_once('=') {
            out.push((k.trim().to_string(), v.trim() == "true"));
        }
    }
    out
}

/// Apply every switch (only lines that differ are rewritten). Returns the new text and the names changed.
pub fn apply_modules(text: &str, switches: &[(String, bool)]) -> Result<(String, Vec<String>), String> {
    let mut cur = text.to_string();
    let mut changed = Vec::new();
    for (name, on) in switches {
        let now = modules_of(&cur);
        if now.iter().any(|(n, v)| n == name && v == on) {
            continue;
        }
        cur = set_module(&cur, name, *on)?;
        changed.push(format!("{name} = {on}"));
    }
    Ok((cur, changed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_and_adds() {
        let cfg = "[loader]\nlog_level = \"info\"\n\n[modules]\nlua_bridge = true  # bridge\nstats = false\n\n[stats]\nhero_mult = 1.0\n";
        let (t, ch) = apply_modules(cfg, &[("stats".into(), true), ("lua_bridge".into(), true), ("rogue".into(), true)]).unwrap();
        assert_eq!(ch, vec!["stats = true", "rogue = true"]);
        assert!(t.contains("stats = true\r\n"));
        assert!(t.contains("lua_bridge = true  # bridge"));
        let m = modules_of(&t);
        assert_eq!(m, vec![("lua_bridge".into(), true), ("stats".into(), true), ("rogue".into(), true)]);
        assert!(t.find("rogue = true").unwrap() < t.find("[stats]").unwrap());
    }

    #[test]
    fn list_parse() {
        let l = parse_module_list("# c\nlua_bridge = true # x\nstats = false\n\n");
        assert_eq!(l, vec![("lua_bridge".into(), true), ("stats".into(), false)]);
    }
}
