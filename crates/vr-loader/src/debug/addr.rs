//! Address expressions of the debug server (`read`, `write`, `watch`).
//!
//! ```text
//! expr := term (('+' | '-') term)*
//! term := 'rva:' hex | hex | symbol | '[' expr ']'      ([x] = the 8-byte pointer stored at x)
//! hex  := 0x1234 | 1234                                 (numbers are always hexadecimal)
//! ```
//! Examples: `rva:0x21F62A0`, `0x7FF6A0000000`, `[g_gameRoot]+6A58`, `[[g_gameRoot]+6A58]+2C29`.
//! Symbols: `base` (image base) and the globals the loader resolves (`g_gameRoot`, `g_soccerActors`, ...).

pub struct Env<'a> {
    pub base: usize,
    /// Pointer read (None = unreadable).
    pub deref: &'a dyn Fn(usize) -> Option<usize>,
    /// Symbol lookup (case-insensitive name).
    pub sym: &'a dyn Fn(&str) -> Option<usize>,
}

pub fn parse(expr: &str, env: &Env) -> Result<usize, String> {
    let s: Vec<char> = expr.chars().filter(|c| !c.is_whitespace()).collect();
    if s.is_empty() {
        return Err("empty address".into());
    }
    let mut p = Parser { s: &s, i: 0, env };
    let v = p.sum()?;
    if p.i != s.len() {
        return Err(format!("unexpected '{}' in address {expr:?}", s[p.i]));
    }
    Ok(v)
}

struct Parser<'a, 'b> {
    s: &'a [char],
    i: usize,
    env: &'a Env<'b>,
}

impl Parser<'_, '_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }
    fn sum(&mut self) -> Result<usize, String> {
        let mut v = self.term()?;
        while let Some(c) = self.peek() {
            match c {
                '+' => {
                    self.i += 1;
                    v = v.wrapping_add(self.term()?);
                }
                '-' => {
                    self.i += 1;
                    v = v.wrapping_sub(self.term()?);
                }
                _ => break,
            }
        }
        Ok(v)
    }
    fn term(&mut self) -> Result<usize, String> {
        match self.peek() {
            Some('[') => {
                self.i += 1;
                let a = self.sum()?;
                if self.peek() != Some(']') {
                    return Err("missing ']'".into());
                }
                self.i += 1;
                (self.env.deref)(a).ok_or_else(|| format!("cannot read pointer at 0x{a:X}"))
            }
            Some(_) => {
                let start = self.i;
                while let Some(c) = self.peek() {
                    if c.is_ascii_alphanumeric() || c == '_' || c == ':' {
                        self.i += 1;
                    } else {
                        break;
                    }
                }
                let tok: String = self.s[start..self.i].iter().collect();
                if tok.is_empty() {
                    return Err(format!("expected a number at position {start}"));
                }
                let lower = tok.to_ascii_lowercase();
                if let Some(r) = lower.strip_prefix("rva:") {
                    return Ok(self.env.base.wrapping_add(hex(r)?));
                }
                if lower == "base" {
                    return Ok(self.env.base);
                }
                if let Ok(v) = hex(&lower) {
                    return Ok(v);
                }
                (self.env.sym)(&lower).ok_or_else(|| format!("unknown symbol or number {tok:?}"))
            }
            None => Err("unexpected end of address".into()),
        }
    }
}

fn hex(s: &str) -> Result<usize, String> {
    let t = s.strip_prefix("0x").unwrap_or(s);
    if t.is_empty() || !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("bad hex number {s:?}"));
    }
    usize::from_str_radix(t, 16).map_err(|e| e.to_string())
}

/// Parse `"48 8B 05"`, `"488B05"` or `"48,8b,05"` into bytes.
pub fn parse_hex_bytes(s: &str) -> Result<Vec<u8>, String> {
    let clean: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let rest: String = s.chars().filter(|c| !c.is_ascii_hexdigit() && !c.is_whitespace() && *c != ',').collect();
    if !rest.is_empty() {
        return Err(format!("not hex bytes: {s:?}"));
    }
    if clean.is_empty() || clean.len() % 2 != 0 {
        return Err("hex bytes need an even number of digits".into());
    }
    (0..clean.len()).step_by(2).map(|i| u8::from_str_radix(&clean[i..i + 2], 16).map_err(|e| e.to_string())).collect()
}

pub fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_eval(e: &str) -> Result<usize, String> {
        let mem = |a: usize| match a {
            0x1_021F_62A0 => Some(0x5000usize),
            0xBA58 => Some(0x9000),
            _ => None,
        };
        let sym = |n: &str| (n == "g_gameroot").then_some(0x1_021F_62A0usize);
        parse(e, &Env { base: 0x1_0000_0000, deref: &mem, sym: &sym })
    }

    #[test]
    fn expressions() {
        assert_eq!(env_eval("rva:0x21F62A0"), Ok(0x1_021F_62A0));
        assert_eq!(env_eval("rva:21F62A0 + 10"), Ok(0x1_021F_62B0));
        assert_eq!(env_eval("0x7FF6A0000000"), Ok(0x7FF6_A000_0000));
        assert_eq!(env_eval("[g_gameRoot]+6A58"), Ok(0xBA58));
        assert_eq!(env_eval("[[g_gameRoot]+6A58]+2C29"), Ok(0x9000 + 0x2C29));
        assert_eq!(env_eval("base+10-8"), Ok(0x1_0000_0008));
        assert!(env_eval("[0x1234]").unwrap_err().contains("cannot read"));
        assert!(env_eval("nope").is_err());
        assert!(env_eval("[rva:1").is_err());
    }

    #[test]
    fn hex_bytes() {
        assert_eq!(parse_hex_bytes("48 8B 05"), Ok(vec![0x48, 0x8B, 5]));
        assert_eq!(parse_hex_bytes("488b05"), Ok(vec![0x48, 0x8B, 5]));
        assert!(parse_hex_bytes("48 8").is_err());
        assert!(parse_hex_bytes("zz").is_err());
        assert_eq!(to_hex(&[1, 0xAB]), "01 AB");
    }
}
