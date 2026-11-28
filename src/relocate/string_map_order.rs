//! The iteration order of an `llvm::StringMap`, which decides the order in which clangd loads the main
//! files of a compilation database: `JSONCompilationDatabase::getAllFiles` iterates its `IndexByFile`
//! map, and clangd's broadcast and loader keep that order.
//!
//! `llvm/lib/Support/StringMap.cpp`: open addressing over a power-of-two table that starts with 16
//! buckets, keyed by the low 32 bits of `xxh3_64bits`; after an insertion that leaves it more than 3/4
//! full it doubles, reinserting the entries in bucket order. Iteration visits the buckets in order.
//! LLVM 18 to 22 probe quadratically, LLVM 23 linearly (see [`StringMapProbing`]).

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use xxhash_rust::xxh3::xxh3_64;

const INITIAL_BUCKETS: usize = 16;
const GROWTH_FACTOR: usize = 2;
/// The table grows when `items * MAX_LOAD_DENOMINATOR > buckets * MAX_LOAD_NUMERATOR`.
const MAX_LOAD_NUMERATOR: usize = 3;
const MAX_LOAD_DENOMINATOR: usize = 4;

/// How `StringMapImpl::LookupBucketFor` and `RehashTable` step from an occupied bucket to the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StringMapProbing {
    /// LLVM 22 and earlier: steps of 1, 2, 3 and so on.
    Quadratic,
    /// LLVM 23 and later: steps of 1.
    Linear,
}

impl StringMapProbing {
    fn next_step(self, step: usize) -> usize {
        match self {
            StringMapProbing::Quadratic => step + 1,
            StringMapProbing::Linear => 1,
        }
    }
}

struct Entry {
    hash: u32,
    key: PathBuf,
}

/// `keys` in the order a `StringMap` probing by `probing` and filled with them in sequence iterates them;
/// a repeated key keeps its first place.
pub fn string_map_order(keys: Vec<PathBuf>, probing: StringMapProbing) -> Vec<PathBuf> {
    let mut buckets: Vec<Option<Entry>> = empty_table(INITIAL_BUCKETS);
    let mut items = 0;
    for key in keys {
        let hash = string_map_hash(&key);
        let bucket = find_bucket(&buckets, hash, &key, probing);
        if buckets[bucket].is_some() {
            continue;
        }
        buckets[bucket] = Some(Entry { hash, key });
        items += 1;
        let is_overloaded = items * MAX_LOAD_DENOMINATOR > buckets.len() * MAX_LOAD_NUMERATOR;
        if is_overloaded {
            buckets = rehash(buckets, probing);
        }
    }
    buckets.into_iter().flatten().map(|entry| entry.key).collect()
}

/// `StringMapImpl::hash`: `xxh3_64bits` truncated to `uint32_t`.
fn string_map_hash(key: &Path) -> u32 {
    xxh3_64(key.as_os_str().as_bytes()) as u32
}

fn empty_table(size: usize) -> Vec<Option<Entry>> {
    (0..size).map(|_| None).collect()
}

/// `LookupBucketFor`: the bucket holding `key`, or the first empty one on its probe sequence.
fn find_bucket(buckets: &[Option<Entry>], hash: u32, key: &Path, probing: StringMapProbing) -> usize {
    let mask = buckets.len() - 1;
    let mut bucket = hash as usize & mask;
    let mut step = 1;
    while let Some(entry) = &buckets[bucket] {
        let is_match = entry.hash == hash && entry.key == key;
        if is_match {
            return bucket;
        }
        bucket = (bucket + step) & mask;
        step = probing.next_step(step);
    }
    bucket
}

/// `RehashTable`: entries move to a table twice the size in their old bucket order.
fn rehash(buckets: Vec<Option<Entry>>, probing: StringMapProbing) -> Vec<Option<Entry>> {
    let mut grown = empty_table(buckets.len() * GROWTH_FACTOR);
    let mask = grown.len() - 1;
    for entry in buckets.into_iter().flatten() {
        let mut bucket = entry.hash as usize & mask;
        let mut step = 1;
        while grown[bucket].is_some() {
            bucket = (bucket + step) & mask;
            step = probing.next_step(step);
        }
        grown[bucket] = Some(entry);
    }
    grown
}

/// Expected order from real LLVM: `llvm-stringmap-order/run.py` in the shared script library builds
/// `llvm/lib/Support/StringMap.cpp`, `xxhash.cpp` and `Path.cpp` of LLVM 19 (19.1.2, 7ba7d8e2) and
/// LLVM 23 (ea7d852a) into a driver that fills a `StringMap` the way `JSONCompilationDatabase` does.
#[cfg(test)]
pub(crate) mod golden {
    use std::path::PathBuf;

    use super::StringMapProbing;

    /// The driver's compilation database directory; its entries are `../src/tu<i>.cpp`,
    /// `/work/proj/src/./tu<i>.cpp` and `/work/proj/src/tu<i>.cpp` in turn for `i` in `0..ENTRY_COUNT`,
    /// then `/work/proj/src/tu0.cpp` once more.
    pub const CDB_DIRECTORY: &str = "/work/proj/build";
    pub const ENTRY_COUNT: usize = 50;
    /// The `i` of each [`key`] in LLVM 19's iteration order.
    pub const LLVM_19_ORDER: [usize; ENTRY_COUNT] = [
        20, 42, 26, 19, 33, 17, 29, 12, 7, 46, 16, 4, 49, 37, 8, 0, 34, 18, 21, 2, 31, 1, 32, 48, 22, 5, 39, 15, 9, 38, 43, 44, 27, 40, 28,
        35, 25, 14, 36, 3, 24, 6, 11, 47, 23, 45, 10, 13, 30, 41,
    ];
    /// The `i` of each [`key`] in LLVM 23's iteration order.
    pub const LLVM_23_ORDER: [usize; ENTRY_COUNT] = [
        20, 42, 26, 19, 33, 17, 29, 12, 7, 46, 16, 4, 49, 37, 8, 0, 34, 18, 2, 21, 31, 1, 32, 48, 22, 5, 39, 15, 9, 38, 43, 44, 27, 40, 28,
        35, 25, 36, 14, 3, 24, 6, 11, 47, 23, 45, 10, 13, 30, 41,
    ];

    /// The `StringMap` key of entry `index`.
    pub fn key(index: usize) -> PathBuf {
        PathBuf::from(format!("/work/proj/src/tu{index}.cpp"))
    }

    /// The `(directory, file)` of the driver's entries.
    pub fn entries() -> Vec<(String, String)> {
        let file = |index: usize| match index % 3 {
            0 => format!("../src/tu{index}.cpp"),
            1 => format!("/work/proj/src/./tu{index}.cpp"),
            _ => format!("/work/proj/src/tu{index}.cpp"),
        };
        let files = (0..ENTRY_COUNT).map(file).chain(["/work/proj/src/tu0.cpp".to_string()]);
        files.map(|file| (CDB_DIRECTORY.to_string(), file)).collect()
    }

    /// The iteration order of a `StringMap` probing by `probing`.
    pub fn expected_order(probing: StringMapProbing) -> Vec<PathBuf> {
        let order = match probing {
            StringMapProbing::Quadratic => LLVM_19_ORDER,
            StringMapProbing::Linear => LLVM_23_ORDER,
        };
        order.iter().map(|&index| key(index)).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    const LINEAR: StringMapProbing = StringMapProbing::Linear;
    const QUADRATIC: StringMapProbing = StringMapProbing::Quadratic;

    fn candidates() -> impl Iterator<Item = PathBuf> {
        (0..).map(|index| PathBuf::from(format!("/p/src/tu{index}.cpp")))
    }

    fn bucket_of(key: &Path, size: usize) -> usize {
        string_map_hash(key) as usize & (size - 1)
    }

    /// `count` keys that each want a different bucket of a 16-bucket table, so none probes.
    fn keys_in_distinct_buckets(count: usize) -> Vec<PathBuf> {
        let mut taken: HashSet<usize> = HashSet::new();
        candidates().filter(|key| taken.insert(bucket_of(key, INITIAL_BUCKETS))).take(count).collect()
    }

    fn sorted_by_bucket(keys: &[PathBuf], size: usize) -> Vec<PathBuf> {
        let mut sorted = keys.to_vec();
        sorted.sort_by_key(|key| bucket_of(key, size));
        sorted
    }

    #[test]
    fn iterates_in_bucket_order_and_keeps_first_of_repeated_keys() {
        let keys = keys_in_distinct_buckets(5);

        let order = string_map_order([keys.clone(), keys.clone()].concat(), LINEAR);

        assert_eq!(order, sorted_by_bucket(&keys, INITIAL_BUCKETS));
    }

    #[test]
    fn doubles_table_once_more_than_three_quarters_full() {
        // 12 of 16 buckets is exactly 3/4; the 13th key doubles the table.
        let keys = keys_in_distinct_buckets(13);

        assert_eq!(string_map_order(keys[..12].to_vec(), LINEAR), sorted_by_bucket(&keys[..12], INITIAL_BUCKETS));
        assert_eq!(string_map_order(keys.clone(), LINEAR), sorted_by_bucket(&keys, INITIAL_BUCKETS * GROWTH_FACTOR));
    }

    #[test]
    fn probes_colliding_key_into_next_bucket() {
        let first = candidates().next().unwrap();
        let colliding = candidates().skip(1).find(|key| bucket_of(key, INITIAL_BUCKETS) == bucket_of(&first, INITIAL_BUCKETS)).unwrap();

        assert_eq!(string_map_order(vec![first.clone(), colliding.clone()], LINEAR), vec![first.clone(), colliding.clone()]);
        assert_eq!(string_map_order(vec![colliding.clone(), first.clone()], LINEAR), vec![colliding, first]);
    }

    /// The first candidate whose 16-bucket home is `bucket`.
    fn key_in_bucket(bucket: usize, excluded: &[&PathBuf]) -> PathBuf {
        candidates().find(|key| bucket_of(key, INITIAL_BUCKETS) == bucket && !excluded.contains(&key)).unwrap()
    }

    #[test]
    fn places_third_colliding_key_two_or_six_buckets_from_home() {
        // `a`, `b` and `c` share home `x`; `d` sits at `x + 3`. Linear probing puts `c` at `x + 2`,
        // quadratic probing at `x + 1 + 2 = x + 3`, which `d` holds, then at `x + 6`.
        let a = candidates().next().unwrap();
        let home = bucket_of(&a, INITIAL_BUCKETS);
        let b = key_in_bucket(home, &[&a]);
        let c = key_in_bucket(home, &[&a, &b]);
        let d = key_in_bucket((home + 3) % INITIAL_BUCKETS, &[]);
        let keys = vec![d.clone(), a.clone(), b.clone(), c.clone()];
        let bucket_order = |placed: [(&PathBuf, usize); 4]| {
            let mut sorted = placed.to_vec();
            sorted.sort_by_key(|&(_, offset)| (home + offset) % INITIAL_BUCKETS);
            sorted.into_iter().map(|(key, _)| key.clone()).collect::<Vec<_>>()
        };

        assert_eq!(string_map_order(keys.clone(), LINEAR), bucket_order([(&a, 0), (&b, 1), (&c, 2), (&d, 3)]));
        assert_eq!(string_map_order(keys, QUADRATIC), bucket_order([(&a, 0), (&b, 1), (&d, 3), (&c, 6)]));
    }

    #[test]
    fn matches_order_of_real_llvm_string_map() {
        let keys: Vec<PathBuf> = (0..golden::ENTRY_COUNT).chain([0]).map(golden::key).collect();

        assert_eq!(string_map_order(keys.clone(), QUADRATIC), golden::expected_order(QUADRATIC));
        assert_eq!(string_map_order(keys, LINEAR), golden::expected_order(LINEAR));
    }

    #[test]
    fn keeps_every_key_of_a_large_map() {
        let keys: Vec<PathBuf> = candidates().take(5000).collect();

        for probing in [QUADRATIC, LINEAR] {
            let order = string_map_order(keys.clone(), probing);

            assert_eq!(order.iter().collect::<HashSet<_>>(), keys.iter().collect::<HashSet<_>>(), "{probing:?}");
            assert_eq!(order.len(), keys.len(), "{probing:?}");
        }
    }
}
