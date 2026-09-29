//! CRC-32 name hashes and the sorted ("argsort") index tables that accompany them.
//!
//! Level-5 binds almost everything by the standard CRC-32 (zlib/IEEE) of an ASCII name.
//! Next to every hash table the engine keeps an index table `ids` such that
//! `hashes[ids[0]] <= hashes[ids[1]] <= ...`, so it can binary-search a hash. Any writer that
//! changes names must regenerate that table.

/// Standard CRC-32 (zlib / IEEE, polynomial 0xEDB88320) of raw bytes.
pub fn crc32(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

/// CRC-32 of a name, as stored in G4TX/G4PK/G4MD hash tables.
pub fn name_hash(name: &str) -> u32 {
    crc32(name.as_bytes())
}

/// Stable argsort of `hashes`: `ids[k]` is the entry index of the k-th smallest hash.
/// Equal hashes keep their entry order.
pub fn sorted_index(hashes: &[u32]) -> Vec<usize> {
    let mut ids: Vec<usize> = (0..hashes.len()).collect();
    ids.sort_by_key(|&i| hashes[i]);
    ids
}

/// True if `ids` is a permutation of `0..hashes.len()` that orders `hashes` ascending.
///
/// Retail files sort equal hashes (duplicate names) in an arbitrary order, so this accepts
/// any tie order.
pub fn is_sorted_index(hashes: &[u32], ids: &[usize]) -> bool {
    if ids.len() != hashes.len() {
        return false;
    }
    let mut seen = vec![false; ids.len()];
    for &i in ids {
        match seen.get_mut(i) {
            Some(s) if !*s => *s = true,
            _ => return false,
        }
    }
    ids.windows(2).all(|w| hashes[w[0]] <= hashes[w[1]])
}

/// Return `previous` if it is still a valid sorted index for `hashes` (keeps the original
/// tie order byte-for-byte), otherwise compute a fresh [`sorted_index`].
pub fn sorted_index_preserving(hashes: &[u32], previous: Option<&[usize]>) -> Vec<usize> {
    match previous {
        Some(p) if is_sorted_index(hashes, p) => p.to_vec(),
        _ => sorted_index(hashes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_hashes() {
        // docs/formats/level5-archives.md
        assert_eq!(name_hash("soccer00_01.g4sk"), 0xD108_7524);
        // docs/OVERVIEW.md: crc32("INVALID") = -992181094
        assert_eq!(name_hash("INVALID") as i32, -992_181_094);
    }

    #[test]
    fn argsort() {
        let h = [30, 10, 20, 10];
        let ids = sorted_index(&h);
        assert_eq!(ids, vec![1, 3, 2, 0]);
        assert!(is_sorted_index(&h, &ids));
        assert!(is_sorted_index(&h, &[3, 1, 2, 0])); // other tie order is valid
        assert!(!is_sorted_index(&h, &[0, 1, 2, 3]));
        assert!(!is_sorted_index(&h, &[1, 1, 2, 0]));
        assert!(!is_sorted_index(&h, &[1, 3, 2, 9]));
        assert_eq!(
            sorted_index_preserving(&h, Some(&[3, 1, 2, 0])),
            vec![3, 1, 2, 0]
        );
        assert_eq!(sorted_index_preserving(&h, Some(&[0, 1, 2, 3])), ids);
    }
}
