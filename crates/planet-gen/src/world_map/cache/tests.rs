use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

const SEED: u64 = 0xB4A7_A9AC;
const SIZE: u32 = 64;

/// A folder of the test's own, removed when it is dropped.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Dir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "bhayanak-map-cache-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        Dir(dir)
    }

    fn file(&self) -> PathBuf {
        path_in(&self.0, SEED)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn fresh() -> WorldMap {
    WorldMap::with_threads(SEED, SIZE, TEST_RADIUS, 4).0
}

fn same(a: &WorldMap, b: &WorldMap) {
    assert_eq!((a.seed, a.size, a.radius), (b.seed, b.size, b.radius));
    assert_eq!(a.texels, b.texels, "texel for texel");
    assert_eq!(a.content_hash(), b.content_hash());
}

#[test]
fn a_cached_map_is_bit_identical_to_a_fresh_build() {
    let dir = Dir::new();
    let map = fresh();
    store(&dir.0, &map).unwrap();
    let back = load(&dir.0, SEED, SIZE, TEST_RADIUS).expect("a hit");
    same(&map, &back);
    // Whatever the threads made the file, it is the same file.
    let first = fs::read(dir.file()).unwrap();
    let again = WorldMap::with_threads(SEED, SIZE, TEST_RADIUS, 1).0;
    store(&dir.0, &again).unwrap();
    assert_eq!(first, fs::read(dir.file()).unwrap());
}

#[test]
fn the_second_launch_reads_the_map_and_the_first_builds_and_writes_it() {
    let dir = Dir::new();
    let first = load_or_build(Some(&dir.0), SEED, SIZE, TEST_RADIUS, 4);
    assert_eq!(first.outcome, Outcome::Built(Miss::Missing));
    assert!(first.stages.is_some());
    assert!(matches!(first.wrote, Some(Ok(_))));
    let second = load_or_build(Some(&dir.0), SEED, SIZE, TEST_RADIUS, 4);
    assert_eq!(second.outcome, Outcome::Hit);
    assert!(second.stages.is_none(), "a hit builds nothing");
    same(&first.map, &second.map);
}

#[test]
fn a_missing_cache_and_a_cache_that_is_off_build_the_map() {
    let dir = Dir::new();
    assert_eq!(
        load(&dir.0, SEED, SIZE, TEST_RADIUS).err(),
        Some(Miss::Missing)
    );
    let off = load_or_build(None, SEED, SIZE, TEST_RADIUS, 2);
    assert_eq!(off.outcome, Outcome::Built(Miss::Off));
    assert!(off.wrote.is_none());
    same(&off.map, &fresh());
}

/// Makes the cache, applies `damage` to its file, and says what a launch then does.
fn damaged(damage: impl FnOnce(&mut Vec<u8>)) -> (Outcome, Dir, WorldMap) {
    let dir = Dir::new();
    let map = fresh();
    store(&dir.0, &map).unwrap();
    let mut bytes = fs::read(dir.file()).unwrap();
    damage(&mut bytes);
    fs::write(dir.file(), bytes).unwrap();
    let launch = load_or_build(Some(&dir.0), SEED, SIZE, TEST_RADIUS, 4);
    same(&launch.map, &map);
    (launch.outcome, dir, map)
}

#[test]
fn a_corrupt_file_is_rebuilt_and_written_over() {
    let flipped = |at: fn(&Vec<u8>) -> usize| {
        move |bytes: &mut Vec<u8>| {
            let at = at(bytes);
            bytes[at] ^= 0x10;
        }
    };
    // A texel's byte, the checksum, a byte of the header's magic, a file that is cut short, and one
    // with something after its end.
    type Damage = Box<dyn FnOnce(&mut Vec<u8>)>;
    let cases: [(&str, Damage); 5] = [
        ("a texel", Box::new(flipped(|b| HEADER + b.len() / 3))),
        ("the checksum", Box::new(flipped(|b| b.len() - 3))),
        ("the magic", Box::new(flipped(|_| 2))),
        ("cut short", Box::new(|b| b.truncate(b.len() / 2))),
        ("too long", Box::new(|b| b.extend_from_slice(&[1, 2, 3]))),
    ];
    for (what, damage) in cases {
        let (outcome, dir, map) = damaged(damage);
        assert!(
            matches!(outcome, Outcome::Built(Miss::Corrupt(_))),
            "{what}: {outcome:?}"
        );
        // It was written over: the next launch is a hit, and the same map.
        let next =
            load(&dir.0, SEED, SIZE, TEST_RADIUS).unwrap_or_else(|m| panic!("{what}: {m:?}"));
        same(&next, &map);
    }
}

#[test]
fn a_texel_that_cannot_be_is_corrupt_even_with_the_checksum_of_what_it_is() {
    // A biome that does not exist, with the checksum made to match: the file is whole and wrong.
    let dir = Dir::new();
    store(&dir.0, &fresh()).unwrap();
    let mut bytes = fs::read(dir.file()).unwrap();
    bytes[HEADER + 4] = 9;
    let end = bytes.len() - 8;
    let sum = checksum(&bytes[HEADER..end]);
    bytes[end..].copy_from_slice(&sum.to_le_bytes());
    fs::write(dir.file(), bytes).unwrap();
    assert!(matches!(
        load(&dir.0, SEED, SIZE, TEST_RADIUS),
        Err(Miss::Corrupt(_))
    ));
}

#[test]
fn a_file_of_another_key_is_rebuilt_and_written_over() {
    let dir = Dir::new();
    let map = fresh();
    store(&dir.0, &map).unwrap();
    // The same file asked for another size, another radius and (a file of another seed put where
    // this seed's is) another seed.
    assert!(matches!(
        load(&dir.0, SEED, SIZE * 2, TEST_RADIUS),
        Err(Miss::OtherKey(_))
    ));
    assert!(matches!(
        load(&dir.0, SEED, SIZE, TEST_RADIUS / 2),
        Err(Miss::OtherKey(_))
    ));
    let other = WorldMap::with_threads(SEED + 1, SIZE, TEST_RADIUS, 4).0;
    store(&dir.0, &other).unwrap();
    fs::rename(path_in(&dir.0, SEED + 1), dir.file()).unwrap();
    let launch = load_or_build(Some(&dir.0), SEED, SIZE, TEST_RADIUS, 4);
    assert!(matches!(launch.outcome, Outcome::Built(Miss::OtherKey(_))));
    same(&launch.map, &map);
    assert_eq!(
        load(&dir.0, SEED, SIZE, TEST_RADIUS).unwrap().texels,
        map.texels
    );
}

#[test]
fn a_file_of_another_generator_version_or_build_is_not_used() {
    for (offset, what) in [(8, "version"), (12, "build")] {
        let (outcome, _, _) = damaged(|bytes| bytes[offset] ^= 1);
        assert!(
            matches!(outcome, Outcome::Built(Miss::OtherKey(_))),
            "{what}: {outcome:?}"
        );
    }
}

#[test]
fn the_build_hash_is_stable_and_the_checksum_does_not_depend_on_the_threads() {
    assert_eq!(params_hash(), params_hash());
    let bytes: Vec<u8> = (0..(3 * CHECKSUM_PIECE + 77))
        .map(|n| (n * 31 % 251) as u8)
        .collect();
    assert_eq!(checksum(&bytes), checksum(&bytes));
    let mut other = bytes.clone();
    other[CHECKSUM_PIECE + 5] ^= 1;
    assert_ne!(checksum(&bytes), checksum(&other));
}

#[test]
fn the_folder_is_the_environments_then_the_systems_and_the_cache_can_be_turned_off() {
    let get = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| std::ffi::OsString::from(v))
        }
    };
    assert_eq!(
        folder(get(&[(ENV_DIR, "/x/y"), ("HOME", "/h")])),
        Some(PathBuf::from("/x/y"))
    );
    assert_eq!(folder(get(&[(ENV_OFF, "1"), (ENV_DIR, "/x")])), None);
    if cfg!(windows) {
        assert_eq!(
            folder(get(&[("LOCALAPPDATA", "C:\\L")])),
            Some(PathBuf::from("C:\\L").join("bhayanakvoxel"))
        );
    } else {
        assert_eq!(
            folder(get(&[("XDG_CACHE_HOME", "/c"), ("HOME", "/h")])),
            Some(PathBuf::from("/c/bhayanakvoxel"))
        );
        assert_eq!(
            folder(get(&[("HOME", "/h")])),
            Some(PathBuf::from("/h/.cache/bhayanakvoxel"))
        );
    }
    assert_eq!(folder(get(&[])), None);
}
