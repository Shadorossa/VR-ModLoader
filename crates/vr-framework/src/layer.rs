//! Cross-mod merge of keyed values (generic): every mod *claims* a value for a key with a precedence **tier** (how
//! explicit the claim is, e.g. «written for this language» > «written for all languages» > «fallback of the mod's
//! default language»); the winner is the highest `(tier, load_index)`: at equal tier the mod that loads later wins,
//! like every other ModLoader conflict. Two explicit claims (tier >= [`Layer::warn_tier`]) of different mods with
//! different values are logged as a conflict.
//!
//! [`merge_by_key`] is the plain form (one tier): items in load order, the last value of a key wins, every key set by
//! more than one mod is reported ([`MergeConflict`]).

use std::collections::BTreeMap;

/// A merge note of [`merge_by_key`]: `key` was set by `losers` (in load order) and by `winner` (loads last).
#[derive(Debug, Clone, PartialEq)]
pub struct MergeConflict {
    pub key: String,
    pub winner: String,
    pub losers: Vec<String>,
}

/// Merge `(mod id, key, value)` items given in load order: the last value of each key wins.
pub fn merge_by_key<V: Clone>(items: &[(String, String, V)]) -> (BTreeMap<String, (String, V)>, Vec<MergeConflict>) {
    let mut out: BTreeMap<String, (String, V)> = BTreeMap::new();
    let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (m, k, v) in items {
        seen.entry(k.clone()).or_default().push(m.clone());
        out.insert(k.clone(), (m.clone(), v.clone()));
    }
    let conflicts = seen
        .into_iter()
        .filter(|(_, ms)| ms.len() > 1)
        .map(|(k, mut ms)| {
            let winner = ms.pop().unwrap_or_default();
            MergeConflict { key: k, winner, losers: ms }
        })
        .collect();
    (out, conflicts)
}

/// One claim on a key.
#[derive(Debug, Clone, PartialEq)]
pub struct Claim<V> {
    pub value: V,
    pub tier: u8,
    pub load_index: u32,
    /// Mod id.
    pub owner: String,
    /// How the mod wrote it (key as written, file…), for messages.
    pub origin: String,
}

/// A conflict between two explicit claims (for the log).
#[derive(Debug, Clone, PartialEq)]
pub struct Conflict<K, V> {
    pub key: K,
    pub loser: Claim<V>,
    pub winner: Claim<V>,
}

/// Keyed claims; `get` = the winner.
#[derive(Debug, Clone)]
pub struct Layer<K: Ord, V> {
    map: BTreeMap<K, Claim<V>>,
    /// Claims at this tier or above are "explicit": a lost explicit claim of another mod is a conflict.
    pub warn_tier: u8,
    pub conflicts: Vec<Conflict<K, V>>,
}

impl<K: Ord + Clone, V: Clone + PartialEq> Layer<K, V> {
    pub fn new(warn_tier: u8) -> Self {
        Layer { map: BTreeMap::new(), warn_tier, conflicts: Vec::new() }
    }

    /// Add a claim; returns true when it is (now) the winner.
    pub fn claim(&mut self, key: K, c: Claim<V>) -> bool {
        match self.map.get_mut(&key) {
            None => {
                self.map.insert(key, c);
                true
            }
            Some(old) => {
                let wins = (c.tier, c.load_index) >= (old.tier, old.load_index);
                if old.owner != c.owner && old.value != c.value && old.tier >= self.warn_tier && c.tier >= self.warn_tier {
                    let (w, l) = if wins { (c.clone(), old.clone()) } else { (old.clone(), c.clone()) };
                    self.conflicts.push(Conflict { key: key.clone(), loser: l, winner: w });
                }
                if wins {
                    *old = c;
                }
                wins
            }
        }
    }

    pub fn get(&self, key: &K) -> Option<&Claim<V>> {
        self.map.get(key)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&K, &Claim<V>)> {
        self.map.iter()
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    pub fn into_map(self) -> BTreeMap<K, Claim<V>> {
        self.map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(v: &str, tier: u8, li: u32, owner: &str) -> Claim<String> {
        Claim { value: v.into(), tier, load_index: li, owner: owner.into(), origin: String::new() }
    }

    #[test]
    fn later_wins_at_equal_tier_and_explicit_beats_fallback() {
        let mut l: Layer<&str, String> = Layer::new(2);
        assert!(l.claim("k", c("a", 3, 1, "a")));
        assert!(l.claim("k", c("b", 3, 2, "b")));
        assert_eq!(l.get(&"k").unwrap().value, "b");
        assert_eq!(l.conflicts.len(), 1);
        assert_eq!((l.conflicts[0].winner.owner.as_str(), l.conflicts[0].loser.owner.as_str()), ("b", "a"));
        // a fallback of a mod that loads even later does not beat an explicit claim, and is no conflict
        assert!(!l.claim("k", c("fallback", 1, 9, "z")));
        assert_eq!(l.get(&"k").unwrap().value, "b");
        assert_eq!(l.conflicts.len(), 1);
        // an earlier explicit claim over a fallback: wins silently
        let mut l: Layer<&str, String> = Layer::new(2);
        l.claim("k", c("fb", 1, 5, "x"));
        assert!(l.claim("k", c("fr", 3, 0, "y")));
        assert!(l.conflicts.is_empty());
        // same value: no conflict
        let mut l: Layer<&str, String> = Layer::new(2);
        l.claim("k", c("same", 3, 0, "x"));
        l.claim("k", c("same", 3, 1, "y"));
        assert!(l.conflicts.is_empty());
        // an earlier-loading explicit claim loses to the later one, even when it arrives second
        let mut l: Layer<&str, String> = Layer::new(2);
        l.claim("k", c("late", 3, 7, "late"));
        assert!(!l.claim("k", c("early", 3, 1, "early")));
        assert_eq!(l.get(&"k").unwrap().value, "late");
        assert_eq!(l.conflicts[0].winner.owner, "late");
    }

    #[test]
    fn merge_rule_last_wins_with_conflicts() {
        let items = vec![
            ("a".to_string(), "waza_stream/ev60_1".to_string(), 1),
            ("b".to_string(), "waza_stream/ev60_2".to_string(), 2),
            ("c".to_string(), "waza_stream/ev60_1".to_string(), 3),
        ];
        let (m, c) = merge_by_key(&items);
        assert_eq!(m["waza_stream/ev60_1"], ("c".to_string(), 3));
        assert_eq!(m["waza_stream/ev60_2"], ("b".to_string(), 2));
        assert_eq!(c, vec![MergeConflict { key: "waza_stream/ev60_1".into(), winner: "c".into(), losers: vec!["a".into()] }]);
    }
}
