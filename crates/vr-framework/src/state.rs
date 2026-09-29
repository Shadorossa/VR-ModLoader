//! Reading the ModLoader's state through `game_state` (`Host::state`). Events are published as a numbered ring:
//! `<prefix>_seq` (the number of the last event) + `<prefix>.<n>` (event `n`, the last `ring` ones kept). A reader
//! remembers the last number it handled and asks [`ring_pending`] which ones are new.

/// A reader of a numbered event ring (`<prefix>_seq` + `<prefix>.<n>`) stands at `seen`: events `range` are new;
/// `lost` = some left the ring (size `ring`) before they were read.
#[allow(clippy::reversed_empty_ranges)] // `1..=0` is the empty result
pub fn ring_pending(seen: u64, seq: u64, ring: u64) -> (std::ops::RangeInclusive<u64>, bool) {
    if seq <= seen {
        return (1..=0, false);
    }
    let oldest = seq.saturating_sub(ring - 1).max(1);
    let first = (seen + 1).max(oldest);
    (first..=seq, first > seen + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::reversed_empty_ranges)]
    fn ring_reader() {
        assert_eq!(ring_pending(0, 0, 64), (1..=0, false));
        assert_eq!(ring_pending(3, 5, 64), (4..=5, false));
        assert_eq!(ring_pending(0, 100, 64), (37..=100, true));
        assert_eq!(ring_pending(36, 100, 64), (37..=100, false));
        assert!(ring_pending(5, 5, 64).0.is_empty());
    }
}
