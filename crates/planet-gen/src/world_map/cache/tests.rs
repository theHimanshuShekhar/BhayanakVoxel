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
fn the_checksum_is_the_same_on_1_4_and_more_threads_than_the_machine_has_and_follows_the_bytes() {
    let bytes: Vec<u8> = (0..(5 * CHECKSUM_PIECE + 77))
        .map(|n| (n * 31 % 251) as u8)
        .collect();
    let alone = checksum_on(1, &bytes);
    for threads in [2, 4, machine::threads(), 3 * machine::threads()] {
        assert_eq!(checksum_on(threads, &bytes), alone, "{threads} threads");
    }
    assert_eq!(checksum(&bytes), alone);
    // A byte in any piece, and the order of the pieces, change it.
    for at in [0, CHECKSUM_PIECE + 5, 5 * CHECKSUM_PIECE + 76] {
        let mut other = bytes.clone();
        other[at] ^= 1;
        assert_ne!(checksum_on(4, &other), alone, "byte {at}");
    }
    let mut swapped = bytes.clone();
    let (a, b) = swapped.split_at_mut(CHECKSUM_PIECE);
    a.swap_with_slice(&mut b[..CHECKSUM_PIECE]);
    assert_ne!(checksum_on(4, &swapped), alone, "pieces in another order");
}

#[test]
fn the_file_is_the_same_whatever_the_threads_that_wrote_and_read_it() {
    let dir = Dir::new();
    let map = fresh();
    let mut files = Vec::new();
    for threads in [1, 4, 3 * machine::threads()] {
        store_on(threads, &dir.0, &map).unwrap();
        files.push(fs::read(dir.file()).unwrap());
        same(
            &map,
            &load_on(threads, &dir.0, SEED, SIZE, TEST_RADIUS).unwrap(),
        );
    }
    assert!(files.windows(2).all(|w| w[0] == w[1]));
}

#[test]
fn the_key_follows_the_build_and_not_only_the_version() {
    let real = |radius| WorldMap::with_threads(PROBE.0, PROBE.1, radius, 1).0;
    let key = build_key_of(real, TEST_RADIUS);
    assert_eq!(
        key,
        build_key(TEST_RADIUS),
        "the same build, the same key, however often asked"
    );
    assert_eq!(key, build_key_of(real, TEST_RADIUS));
    // A build whose rules differ in any way that reaches a texel (a biome's threshold, the
    // erosion's step, the drainage's coding) makes another tiny map and so another key, with no
    // change to the version: here each field of a texel in turn.
    let changed = |change: fn(&mut Texel)| {
        build_key_of(
            |radius| {
                let mut map = real(radius);
                let middle = map.texels.len() / 2;
                change(&mut map.texels[middle]);
                map
            },
            TEST_RADIUS,
        )
    };
    for (what, other) in [
        ("elevation", changed(|t| t.elevation ^= 1)),
        ("erosion", changed(|t| t.erosion ^= 1)),
        (
            "biome",
            changed(|t| {
                t.biome = if t.biome == Biome::Snow {
                    Biome::Desert
                } else {
                    Biome::Snow
                }
            }),
        ),
        (
            "regions",
            changed(|t| t.regions = Regions((t.regions.0 + 1) % 16)),
        ),
        ("roughness", changed(|t| t.rough ^= 1)),
        ("drainage", changed(|t| t.drainage ^= 1)),
    ] {
        assert_ne!(other, key, "{what}");
    }
    // The radius is part of what is built: another radius is another key.
    assert_ne!(build_key(TEST_RADIUS), build_key(WORLD_RADIUS));
    // And the key is in the file: a file written with another build's key is not used.
    let dir = Dir::new();
    let map = fresh();
    store(&dir.0, &map).unwrap();
    let mut bytes = fs::read(dir.file()).unwrap();
    bytes[12..20].copy_from_slice(&changed(|t| t.biome = Biome::Volcanic).to_le_bytes());
    fs::write(dir.file(), bytes).unwrap();
    assert!(matches!(
        load(&dir.0, SEED, SIZE, TEST_RADIUS),
        Err(Miss::OtherKey(_))
    ));
}

fn set_age(path: &Path, age: Duration) {
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - age)
        .unwrap();
}

#[test]
fn a_write_deletes_the_temporary_files_of_launches_that_were_killed_and_keeps_a_live_one() {
    let dir = Dir::new();
    fs::create_dir_all(&dir.0).unwrap();
    let stale = dir.0.join("world-map-00000000000000aa.tmp4242");
    let live = dir.0.join("world-map-00000000000000bb.tmp4243");
    let other = dir.0.join("notes.tmp1");
    for file in [&stale, &live, &other] {
        fs::write(file, b"half a map").unwrap();
    }
    set_age(&stale, STALE_TEMPORARY + Duration::from_secs(60));
    set_age(&other, STALE_TEMPORARY + Duration::from_secs(60));
    store(&dir.0, &fresh()).unwrap();
    assert!(
        !stale.exists(),
        "a temporary file that is old is a killed launch's"
    );
    assert!(live.exists(), "a young one is a write in another process");
    assert!(other.exists(), "what is not the cache's is not touched");
    assert!(dir.file().exists());
}

#[test]
fn the_folder_keeps_the_maps_used_last_and_a_hit_is_a_use() {
    let dir = Dir::new();
    let of = |seed: u64| WorldMap::with_threads(seed, SIZE, TEST_RADIUS, 4).0;
    let kept = |last: u64| -> Vec<u64> {
        (1..=last)
            .filter(|&s| path_in(&dir.0, s).exists())
            .collect()
    };
    // Five seeds' maps, the first the oldest, each a day older than the next.
    for seed in 1..=5 {
        store(&dir.0, &of(seed)).unwrap();
        set_age(
            &path_in(&dir.0, seed),
            Duration::from_secs((6 - seed) * 86_400),
        );
    }
    // The write of the sixth tidies: the three used last stay, the new one is one of them.
    store(&dir.0, &of(6)).unwrap();
    assert_eq!(kept(6), [4, 5, 6], "{MOST_MAPS} maps");
    // Reading the oldest of them counts as using it: the next write keeps it, and drops another.
    assert!(load(&dir.0, 4, SIZE, TEST_RADIUS).is_ok());
    store(&dir.0, &of(7)).unwrap();
    let left = kept(7);
    assert_eq!(left.len(), MOST_MAPS);
    assert!(left.contains(&4) && left.contains(&7), "{left:?}");
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
