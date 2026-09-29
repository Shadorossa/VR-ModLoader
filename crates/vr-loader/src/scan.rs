//! Byte-pattern scanner (`"48 8B ?? ..."`, `??` = wildcard) with a uniqueness guard.

#[derive(Debug, Clone)]
pub struct Pattern {
    bytes: Vec<Option<u8>>,
    anchor: usize,
}

impl Pattern {
    pub fn parse(s: &str) -> Result<Pattern, String> {
        let mut bytes = Vec::new();
        for tok in s.split_whitespace() {
            if tok == "??" || tok == "?" {
                bytes.push(None);
            } else {
                bytes.push(Some(u8::from_str_radix(tok, 16).map_err(|_| format!("bad token {tok:?}"))?));
            }
        }
        let anchor = bytes.iter().position(|b| b.is_some()).ok_or("pattern has no fixed byte")?;
        Ok(Pattern { bytes, anchor })
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Fixed bytes of `[0, n)` (None if any is a wildcard).
    pub fn fixed_prefix(&self, n: usize) -> Option<Vec<u8>> {
        self.bytes.get(..n)?.iter().copied().collect()
    }

    fn matches_at(&self, hay: &[u8], i: usize) -> bool {
        self.bytes.iter().enumerate().all(|(k, b)| b.map_or(true, |v| hay[i + k] == v))
    }

    /// Up to `limit` match offsets.
    pub fn find(&self, hay: &[u8], limit: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let n = self.bytes.len();
        if hay.len() < n {
            return out;
        }
        let a = self.anchor;
        let av = self.bytes[a].unwrap();
        let last = hay.len() - n;
        let mut i = 0;
        while i <= last {
            match hay[i + a..=last + a].iter().position(|&b| b == av) {
                None => break,
                Some(p) => {
                    i += p;
                    if self.matches_at(hay, i) {
                        out.push(i);
                        if out.len() >= limit {
                            break;
                        }
                    }
                    i += 1;
                }
            }
        }
        out
    }

    /// The single match, or an error describing 0 / multiple hits.
    pub fn find_unique(&self, hay: &[u8]) -> Result<usize, String> {
        let hits = self.find(hay, 2);
        match hits.len() {
            1 => Ok(hits[0]),
            0 => Err("no match".into()),
            _ => Err(format!("not unique (hits at +0x{:X}, +0x{:X})", hits[0], hits[1])),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scan_basic() {
        let hay = [0x90, 0x48, 0x8B, 0x05, 1, 2, 3, 4, 0xC3, 0x48, 0x8B, 0x05, 9, 9, 9, 9, 0xCC];
        let p = Pattern::parse("48 8B 05 ?? ?? ?? ?? C3").unwrap();
        assert_eq!(p.find(&hay, 10), vec![1]);
        assert_eq!(p.find_unique(&hay), Ok(1));
        let q = Pattern::parse("48 8B 05").unwrap();
        assert!(q.find_unique(&hay).is_err());
        let w = Pattern::parse("?? 8B 05").unwrap();
        assert_eq!(w.find(&hay, 10), vec![1, 9]);
        assert_eq!(p.fixed_prefix(3), Some(vec![0x48, 0x8B, 0x05]));
        assert_eq!(p.fixed_prefix(4), None);
    }
}
