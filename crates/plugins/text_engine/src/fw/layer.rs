//! Cross-mod merge of keyed values (generic): every mod *claims* a value for a key with a precedence **tier** (how
//! explicit the claim is, e.g. «written for this language» > «written for all languages» > «fallback of the mod's
//! default language»); the winner is the highest `(tier, load_index)`: at equal tier the mod that loads later wins,
//! like every other ModLoader conflict. Two explicit claims (tier >= [`Layer::warn_tier`]) of different mods with
//! different values are logged as a conflict.

use std::collections::BTreeMap;

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
}
