//! Wire protocol of the debug server: one request per line, one JSON response per line.
//!
//! A request is either JSON (`{"id": 1, "cmd": "read", "args": ["rva:0x21F62A0", "16"]}`, named fields allowed:
//! `{"cmd": "lua", "code": "return 1", "vm": "title"}`) or a plain text line (`read rva:0x21F62A0 16`,
//! `lua return 1+1`). Responses: `{"id": .., "ok": true, "data": ..}` / `{"id": .., "ok": false, "error": ".."}`;
//! streams (`log follow`) send `{"event": "log", "line": ".."}` lines until the client sends a line or disconnects.

use serde_json::{json, Map, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: Value,
    pub cmd: String,
    pub args: Vec<String>,
    pub named: Map<String, Value>,
    /// Text form: everything after the command word (the Lua code of `lua ...`).
    pub rest: String,
}

impl Request {
    /// Named field, else positional argument `pos`.
    pub fn get(&self, name: &str, pos: usize) -> Option<String> {
        if let Some(v) = self.named.get(name) {
            return Some(match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            });
        }
        self.args.get(pos).cloned()
    }
    pub fn get_u64(&self, name: &str, pos: usize) -> Result<Option<u64>, String> {
        match self.get(name, pos) {
            None => Ok(None),
            Some(s) => parse_num(&s).map(Some),
        }
    }
    pub fn flag(&self, name: &str) -> bool {
        self.named.get(name).is_some_and(|v| v.as_bool().unwrap_or(true)) || self.args.iter().any(|a| a == name)
    }
}

/// Decimal, or hexadecimal with `0x`.
pub fn parse_num(s: &str) -> Result<u64, String> {
    let t = s.trim();
    let r = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(h, 16)
    } else {
        t.parse::<u64>()
    };
    r.map_err(|_| format!("bad number {s:?}"))
}

pub fn parse_line(line: &str) -> Result<Request, String> {
    let line = line.trim_matches(|c: char| c == '\r' || c == '\n' || c.is_whitespace());
    if line.is_empty() {
        return Err("empty request".into());
    }
    if line.starts_with('{') {
        let v: Value = serde_json::from_str(line).map_err(|e| format!("bad JSON: {e}"))?;
        let Value::Object(mut m) = v else { return Err("JSON request must be an object".into()) };
        let id = m.remove("id").unwrap_or(Value::Null);
        let cmd = m.remove("cmd").and_then(|c| c.as_str().map(str::to_string)).ok_or("missing \"cmd\"")?;
        let args = match m.remove("args") {
            Some(Value::Array(a)) => a
                .into_iter()
                .map(|x| match x {
                    Value::String(s) => s,
                    other => other.to_string(),
                })
                .collect(),
            Some(Value::String(s)) => split_words(&s)?,
            Some(_) => return Err("\"args\" must be an array".into()),
            None => vec![],
        };
        let rest = args.join(" ");
        return Ok(Request { id, cmd: cmd.to_ascii_lowercase(), args, named: m, rest });
    }
    let (cmd, rest) = match line.find(char::is_whitespace) {
        Some(i) => (&line[..i], line[i..].trim_start()),
        None => (line, ""),
    };
    let cmd = cmd.to_ascii_lowercase();
    // `lua` keeps its code verbatim (leading key=value options are parsed by the handler)
    let args = if cmd == "lua" { vec![] } else { split_words(rest)? };
    Ok(Request { id: Value::Null, cmd, args, named: Map::new(), rest: rest.to_string() })
}

/// Whitespace-separated words; `"..."` is a JSON string literal (escapes allowed).
pub fn split_words(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if b[i] == b'"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'"' {
                if b[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            if j >= b.len() {
                return Err("unterminated string".into());
            }
            let lit: String = serde_json::from_str(&s[i..=j]).map_err(|e| format!("bad string: {e}"))?;
            out.push(lit);
            i = j + 1;
        } else {
            let start = i;
            while i < b.len() && !b[i].is_ascii_whitespace() {
                i += 1;
            }
            out.push(s[start..i].to_string());
        }
    }
    Ok(out)
}

/// Options in front of the Lua code of a text `lua` line: `vm=<L or label> timeout=<ms>`; a quoted code is a JSON
/// string literal. Returns (options, code).
pub fn split_lua_options(rest: &str) -> Result<(Vec<(String, String)>, String), String> {
    let mut opts = Vec::new();
    let mut r = rest.trim_start();
    loop {
        let word_end = r.find(char::is_whitespace).unwrap_or(r.len());
        let word = &r[..word_end];
        match word.split_once('=') {
            Some((k, v)) if matches!(k, "vm" | "timeout") && !v.is_empty() => {
                opts.push((k.to_string(), v.to_string()));
                r = r[word_end..].trim_start();
            }
            _ => break,
        }
    }
    let code = if r.starts_with('"') && r.trim_end().ends_with('"') {
        serde_json::from_str::<String>(r.trim_end()).map_err(|e| format!("bad quoted code: {e}"))?
    } else {
        r.to_string()
    };
    Ok((opts, code))
}

pub fn ok(id: &Value, data: Value) -> String {
    json!({"id": id, "ok": true, "data": data}).to_string()
}

pub fn err(id: &Value, msg: &str) -> String {
    json!({"id": id, "ok": false, "error": msg}).to_string()
}

pub fn event(kind: &str, line: &str) -> String {
    json!({"event": kind, "line": line}).to_string()
}

/// Is `bind` an acceptable listen address (loopback only)?
pub fn loopback_addr(bind: &str) -> Option<std::net::IpAddr> {
    let a: std::net::IpAddr = bind.trim().parse().ok()?;
    a.is_loopback().then_some(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_lines() {
        let r = parse_line("read rva:0x21F62A0 16\r\n").unwrap();
        assert_eq!(r.cmd, "read");
        assert_eq!(r.args, vec!["rva:0x21F62A0", "16"]);
        assert_eq!(r.get("addr", 0).as_deref(), Some("rva:0x21F62A0"));
        assert_eq!(r.get_u64("len", 1), Ok(Some(16)));
        let r = parse_line("LUA return funcLuaCommand(1, \"a b\")").unwrap();
        assert_eq!(r.cmd, "lua");
        assert_eq!(r.rest, "return funcLuaCommand(1, \"a b\")");
        let r = parse_line("write 0x1000 \"48 8B\"").unwrap();
        assert_eq!(r.args, vec!["0x1000", "48 8B"]);
        assert!(parse_line("   ").is_err());
    }

    #[test]
    fn json_lines() {
        let r = parse_line(r#"{"id": 7, "cmd": "lua", "code": "return 1", "vm": "title"}"#).unwrap();
        assert_eq!(r.id, json!(7));
        assert_eq!(r.get("code", 0).as_deref(), Some("return 1"));
        assert_eq!(r.get("vm", 9).as_deref(), Some("title"));
        let r = parse_line(r#"{"cmd": "read", "args": ["rva:0x10", 16]}"#).unwrap();
        assert_eq!(r.args, vec!["rva:0x10", "16"]);
        assert!(r.flag("x") == false);
        assert!(parse_line(r#"{"args": []}"#).is_err());
        assert!(parse_line("{nope").is_err());
    }

    #[test]
    fn lua_options() {
        let (o, c) = split_lua_options("vm=title timeout=2000 return 1").unwrap();
        assert_eq!(o, vec![("vm".into(), "title".into()), ("timeout".into(), "2000".into())]);
        assert_eq!(c, "return 1");
        let (o, c) = split_lua_options(r#""return \"x\"""#).unwrap();
        assert!(o.is_empty());
        assert_eq!(c, "return \"x\"");
        let (o, c) = split_lua_options("x=1").unwrap();
        assert!(o.is_empty());
        assert_eq!(c, "x=1");
    }

    #[test]
    fn numbers_and_bind() {
        assert_eq!(parse_num("0x10"), Ok(16));
        assert_eq!(parse_num("10"), Ok(10));
        assert!(parse_num("x").is_err());
        assert!(loopback_addr("127.0.0.1").is_some());
        assert!(loopback_addr("::1").is_some());
        assert!(loopback_addr("0.0.0.0").is_none());
        assert!(loopback_addr("192.168.1.2").is_none());
    }
}
