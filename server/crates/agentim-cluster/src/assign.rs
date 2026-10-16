//! Which node should hold a shard (docs/11 §3): rendezvous hashing over
//! the live nodes. Only a suggestion; whether a node may write is decided
//! by the lease alone.

/// 64-bit FNV-1a with a final mix, stable across versions and platforms.
fn score(node: &str, db: &str, shard: u32) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in node
        .as_bytes()
        .iter()
        .chain(b"\0")
        .chain(db.as_bytes())
        .chain(b"\0")
        .chain(&shard.to_be_bytes())
    {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // splitmix64 finaliser: FNV alone spreads short keys poorly.
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d0_49bb_1331_11eb);
    h ^ (h >> 31)
}

/// The preferred node of a shard among `nodes` (node ids); ties go to the
/// smaller id.
pub fn preferred<'a>(nodes: &'a [String], db: &str, shard: u32) -> Option<&'a str> {
    nodes
        .iter()
        .map(|n| (score(n, db, shard), std::cmp::Reverse(n.as_str())))
        .max()
        .map(|(_, std::cmp::Reverse(n))| n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("node-{i}")).collect()
    }

    #[test]
    fn spread_is_even_and_stable() {
        let nodes = names(3);
        let mut c = [0u32; 3];
        for s in 0..258 {
            let p = preferred(&nodes, "main", s).unwrap();
            assert_eq!(
                preferred(&nodes, "main", s).unwrap(),
                p,
                "same input, same output"
            );
            c[nodes.iter().position(|n| n == p).unwrap()] += 1;
        }
        assert!(c.iter().all(|x| (60..=112).contains(x)), "{c:?}");
        // Databases are spread independently.
        let differ = (0..258)
            .filter(|s| preferred(&nodes, "a", *s) != preferred(&nodes, "b", *s))
            .count();
        assert!(differ > 100);
    }

    #[test]
    fn adding_a_node_moves_only_its_share() {
        let before = names(4);
        let after = names(5);
        let moved: Vec<u32> = (0..1024)
            .filter(|s| preferred(&before, "main", *s) != preferred(&after, "main", *s))
            .collect();
        // Every moved shard goes to the new node; about a fifth move.
        for s in &moved {
            assert_eq!(preferred(&after, "main", *s), Some("node-4"));
        }
        assert!((140..=280).contains(&moved.len()), "{}", moved.len());
        // Removing a node moves only the shards it had.
        let fewer: Vec<String> = before.iter().filter(|n| *n != "node-1").cloned().collect();
        for s in 0..1024 {
            let was = preferred(&before, "main", s).unwrap();
            if was != "node-1" {
                assert_eq!(preferred(&fewer, "main", s), Some(was));
            }
        }
    }

    #[test]
    fn edges() {
        assert_eq!(preferred(&[], "main", 0), None);
        let one = vec!["solo".to_string()];
        assert!((0..300).all(|s| preferred(&one, "main", s) == Some("solo")));
        // Ids sharing a prefix are different nodes.
        let pre = vec!["n".to_string(), "n1".to_string(), "n11".to_string()];
        let mut seen = std::collections::HashSet::new();
        for s in 0..300 {
            seen.insert(preferred(&pre, "main", s).unwrap());
        }
        assert_eq!(seen.len(), 3);
    }
}
