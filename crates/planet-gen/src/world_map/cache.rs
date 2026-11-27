//! The world map on disk (#264).
//!
//! The map is a pure function of the seed, [`GENERATOR_VERSION`] and the build's parameters, and
//! its build is the longest wait of a launch (about 11 s at the real size on the 8-thread dev box,
//! 24 s before the build was parallel; about 50 s on the 4090 PC with the rivers prototype's
//! landform model), so the client keeps the built map in the operating system's cache folder
//! (`%LOCALAPPDATA%` on Windows, `$XDG_CACHE_HOME` or `~/.cache` elsewhere) and the second launch
//! reads it back instead.
//!
//! * **The key** is the seed, [`GENERATOR_VERSION`], the map's size and radius, and [`build_key`]:
//!   the hash of a tiny map (16 texels along a face) made by the build itself at the radius, with
//!   [`params_hash`] (its main constants) mixed in. The tiny map is made with the noise's salts, the
//!   erosion, the normalisation, the biomes and the drainage as they are, so a change to **any
//!   input of the build** that reaches a texel is another key even if [`GENERATOR_VERSION`] was not
//!   bumped for it, and nothing stale is loaded. (It is a few milliseconds a launch, and a probe
//!   of 16 texels cannot show a change that only reaches texels it has not got, such as a rule for
//!   a feature smaller than 1/16 of a face: bump the version for that, as for any change.) A file
//!   with any other key is not used.
//! * **The file** is a header (magic, the key), the texels (8 bytes each, little-endian) and a
//!   checksum of them, which is made in fixed pieces of [`CHECKSUM_PIECE`] bytes combined in
//!   order, so it is the same however many threads made it. A file that is missing, is short or
//!   long, has another key, a texel that cannot be, or a wrong checksum is **a miss**, and the map
//!   is built and written over it ([`load_or_build`]). Deleting the folder is always safe.
//! * **A cached map is the map the build makes**, texel for texel: the test
//!   `a_cached_map_is_bit_identical_to_a_fresh_build` holds it to that.
//! * **A cached map is never trusted from the network.** The file is local, written by this
//!   program and checked as above, and nothing here is sent or received: a server builds its own
//!   map with [`WorldMap::generate`], and does not read this folder.
//!
//! Writing is to a temporary file in the same folder and a rename, so a launch that is killed
//! in the middle leaves the old file or none, not half of one; a write that fails (a read-only
//! disk, no room) is said and costs nothing, as the map is made. A temporary file that a killed
//! launch left (201 MB) is deleted by the next write once it is [`STALE_TEMPORARY`] old, and the
//! folder keeps the [`MOST_MAPS`] maps used last (a hit counts as a use) and deletes the rest, so it
//! cannot grow with every seed that was ever played.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use machine::{par_chunks, par_pieces};

use super::*;
use crate::GENERATOR_VERSION;

const MAGIC: [u8; 8] = *b"BVXWMAP1";
const HEADER: usize = 8 + 4 + 8 + 8 + 4 + 4;
const TEXEL_BYTES: usize = 8;
/// Bytes read and decoded at a time.
const CHUNK: usize = 8 << 20;
/// Bytes of a piece of the checksum.
pub const CHECKSUM_PIECE: usize = 1 << 20;
/// The maps (a file each, about 201 MB at the real size) that the folder keeps: the ones used last.
pub const MOST_MAPS: usize = 3;
/// How old a temporary file of a write must be to be one that a launch that was killed left.
pub const STALE_TEMPORARY: Duration = Duration::from_secs(5 * 60);
/// The seed and the size of the tiny map that is in the key ([`build_key`]).
const PROBE: (u64, u32) = (0x5EED_CAC4_E000, 16);

/// The environment variable that gives the folder to use instead of the operating system's.
pub const ENV_DIR: &str = "BHAYANAK_CACHE_DIR";
/// The environment variable that turns the cache off when it is set to `1`: the map is built
/// every launch and nothing is read or written.
pub const ENV_OFF: &str = "BHAYANAK_NO_MAP_CACHE";

/// Why a map was not read from the cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Miss {
    /// There is no file (a first launch, or the folder was deleted).
    Missing,
    /// The file is of another seed, generator version, size, radius or build.
    OtherKey(String),
    /// The file is short or long, has a texel that cannot be, or its checksum is wrong.
    Corrupt(String),
    /// The cache is off or there is no folder to keep it in.
    Off,
}

/// How [`load_or_build`] got the map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Read from the cache.
    Hit,
    /// Built, because of the [`Miss`], and written over it if it could be.
    Built(Miss),
}

/// What [`load_or_build`] made, and how long it took.
pub struct Loaded {
    pub map: WorldMap,
    pub outcome: Outcome,
    /// The time to read the map, or to build it.
    pub took: Duration,
    /// The stages of the build, if it was built.
    pub stages: Option<BuildTimes>,
    /// The time to write the file, and what went wrong if it did, if the map was built.
    pub wrote: Option<Result<Duration, String>>,
}

/// The folder the cache is kept in: [`ENV_DIR`], else the operating system's (`%LOCALAPPDATA%`,
/// `$XDG_CACHE_HOME`, `$HOME/.cache`) and `bhayanakvoxel` in it; `None` if the cache is off
/// ([`ENV_OFF`]) or there is no such folder.
pub fn default_dir() -> Option<PathBuf> {
    folder(|name| std::env::var_os(name))
}

fn folder(get: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    if get(ENV_OFF).is_some_and(|v| v == "1") {
        return None;
    }
    let non_empty = |name: &str| get(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(dir) = non_empty(ENV_DIR) {
        return Some(dir);
    }
    let base = if cfg!(windows) {
        non_empty("LOCALAPPDATA")
    } else {
        non_empty("XDG_CACHE_HOME").or_else(|| non_empty("HOME").map(|h| h.join(".cache")))
    };
    base.map(|b| b.join("bhayanakvoxel"))
}

/// The file of the map of `seed` in `dir`: one file for each seed, overwritten when its key
/// changes.
pub fn path_in(dir: &Path, seed: u64) -> PathBuf {
    dir.join(format!("world-map-{seed:016x}.bin"))
}

/// A hash of the main numbers the build is made from (the noise's cells, the continents' shape,
/// the share of the ocean, the relief). Part of [`build_key`]; the rest of the build is in the tiny
/// map that it hashes.
pub fn params_hash() -> u64 {
    let numbers: [u64; 14] = [
        u64::from(ELEVATION_CELL),
        u64::from(ELEVATION_OCTAVES),
        u64::from(CLIMATE_CELL),
        u64::from(PATCH_CELL),
        u64::from(CONTINENT_CELL),
        CONTINENT_REACH.to_bits(),
        CONTINENT_JITTER.to_bits(),
        CONTINENT_SHARE.to_bits(),
        OCEAN_SHARE.to_bits(),
        PEAK_UNITS as u64,
        TRENCH_UNITS as u64,
        ELEVATION_UNITS_PER_METRE as u64,
        VOXELS_PER_METRE.to_bits(),
        i64::from(SEA_LEVEL) as u64,
    ];
    numbers.into_iter().fold(0xcbf2_9ce4_8422_2325u64, mix)
}

/// The part of the key that is the build: [`params_hash`] and the content hash of a tiny map of
/// [`PROBE`]'s seed and size that the build makes at `radius` on one thread. Remembered for the
/// last radius it was asked for (a launch asks twice, to read and to write).
pub fn build_key(radius: i32) -> u64 {
    static LAST: std::sync::Mutex<Option<(i32, u64)>> = std::sync::Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((r, key)) = *last
        && r == radius
    {
        return key;
    }
    let key = build_key_of(
        |radius| WorldMap::with_threads(PROBE.0, PROBE.1, radius, 1).0,
        radius,
    );
    *last = Some((radius, key));
    key
}

/// [`build_key`] of the tiny map that `build` makes at `radius` (the tests give a build that is
/// another, to show that the key follows it).
fn build_key_of(build: impl Fn(i32) -> WorldMap, radius: i32) -> u64 {
    mix(params_hash(), build(radius).content_hash())
}

fn mix(hash: u64, word: u64) -> u64 {
    (hash ^ word)
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        .rotate_left(29)
}

/// The checksum of `bytes`: a hash of each piece of [`CHECKSUM_PIECE`] bytes, on all the threads,
/// combined in the order of the pieces.
pub fn checksum(bytes: &[u8]) -> u64 {
    checksum_on(machine::threads(), bytes)
}

/// [`checksum`] on `threads` threads (the tests make it on 1, 4 and more than the machine has).
pub fn checksum_on(threads: usize, bytes: &[u8]) -> u64 {
    piece_hashes(threads, bytes)
        .into_iter()
        .fold(0x6a09_e667_f3bc_c908, mix)
}

/// The hash of each piece of [`CHECKSUM_PIECE`] bytes of `bytes`, in order, on `threads` threads.
fn piece_hashes(threads: usize, bytes: &[u8]) -> Vec<u64> {
    par_pieces(bytes.len().div_ceil(CHECKSUM_PIECE), threads, |p| {
        piece_hash(&bytes[p * CHECKSUM_PIECE..((p + 1) * CHECKSUM_PIECE).min(bytes.len())])
    })
}

fn piece_hash(piece: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64 ^ piece.len() as u64;
    let (words, rest) = piece.as_chunks::<8>();
    for word in words {
        hash = mix(hash, u64::from_le_bytes(*word));
    }
    for &b in rest {
        hash = mix(hash, u64::from(b));
    }
    hash
}

fn header(seed: u64, size: u32, radius: i32) -> [u8; HEADER] {
    header_of(seed, size, radius, build_key(radius))
}

fn header_of(seed: u64, size: u32, radius: i32, build: u64) -> [u8; HEADER] {
    let mut h = [0u8; HEADER];
    h[..8].copy_from_slice(&MAGIC);
    h[8..12].copy_from_slice(&GENERATOR_VERSION.to_le_bytes());
    h[12..20].copy_from_slice(&build.to_le_bytes());
    h[20..28].copy_from_slice(&seed.to_le_bytes());
    h[28..32].copy_from_slice(&size.to_le_bytes());
    h[32..36].copy_from_slice(&radius.to_le_bytes());
    h
}

fn encode(texel: &Texel) -> [u8; TEXEL_BYTES] {
    let [e0, e1] = texel.elevation.to_le_bytes();
    let [r0, r1] = texel.erosion.to_le_bytes();
    [
        e0,
        e1,
        r0,
        r1,
        texel.biome as u8,
        texel.regions.0,
        texel.rough,
        texel.drainage,
    ]
}

fn decode(bytes: &[u8]) -> Option<Texel> {
    let biome = match bytes[4] {
        0 => Biome::Temperate,
        1 => Biome::Desert,
        2 => Biome::Snow,
        3 => Biome::Volcanic,
        _ => return None,
    };
    if bytes[5] >= 16 {
        return None;
    }
    Some(Texel {
        elevation: i16::from_le_bytes([bytes[0], bytes[1]]),
        erosion: i16::from_le_bytes([bytes[2], bytes[3]]),
        biome,
        regions: Regions(bytes[5]),
        rough: bytes[6],
        drainage: bytes[7],
    })
}

/// The map of `seed`, `size` and `radius` from `dir`, if the file there is for them and is whole.
pub fn load(dir: &Path, seed: u64, size: u32, radius: i32) -> Result<WorldMap, Miss> {
    load_on(machine::threads(), dir, seed, size, radius)
}

/// [`load`] on `threads` threads.
pub fn load_on(
    threads: usize,
    dir: &Path,
    seed: u64,
    size: u32,
    radius: i32,
) -> Result<WorldMap, Miss> {
    let path = path_in(dir, seed);
    let map = read(threads, &path, seed, size, radius)?;
    // A hit is a use: the folder keeps the maps used last ([`MOST_MAPS`]). It costs nothing if the
    // file cannot be touched.
    let _ = File::options()
        .write(true)
        .open(&path)
        .and_then(|f| f.set_modified(std::time::SystemTime::now()));
    Ok(map)
}

fn read(threads: usize, path: &Path, seed: u64, size: u32, radius: i32) -> Result<WorldMap, Miss> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(Miss::Missing),
        Err(e) => return Err(Miss::Corrupt(format!("cannot be opened: {e}"))),
    };
    let corrupt = |what: &str| Miss::Corrupt(what.to_string());
    let mut head = [0u8; HEADER];
    file.read_exact(&mut head)
        .map_err(|_| corrupt("shorter than its header"))?;
    if head[..8] != MAGIC {
        return Err(corrupt("not a world map file"));
    }
    let want = header(seed, size, radius);
    if head != want {
        let field = |range: std::ops::Range<usize>| &head[range];
        let other = if field(8..12) != &want[8..12] {
            "another generator version"
        } else if field(12..20) != &want[12..20] {
            "another build"
        } else if field(20..28) != &want[20..28] {
            "another seed"
        } else if field(28..32) != &want[28..32] {
            "another size"
        } else {
            "another radius"
        };
        return Err(Miss::OtherKey(other.to_string()));
    }
    let count = 6 * (size as usize).pow(2);
    let expected = (count * TEXEL_BYTES) as u64;
    let length = file
        .metadata()
        .map_err(|e| Miss::Corrupt(format!("has no length: {e}")))?
        .len();
    if length != HEADER as u64 + expected + 8 {
        return Err(corrupt("is not as long as its map"));
    }
    let blank = Texel {
        elevation: 0,
        erosion: 0,
        biome: Biome::Temperate,
        regions: Regions::default(),
        rough: 0,
        drainage: 0,
    };
    let mut texels = vec![blank; count];
    // The file is read a chunk at a time, and each chunk is checked in pieces and decoded on all
    // the threads, so the memory it takes is the map and one chunk.
    let mut buffer = vec![0u8; CHUNK];
    let mut pieces: Vec<u64> = Vec::with_capacity((expected as usize).div_ceil(CHECKSUM_PIECE));
    let per_chunk = CHUNK / TEXEL_BYTES;
    let mut bad = false;
    for out in texels.chunks_mut(per_chunk) {
        let bytes = &mut buffer[..out.len() * TEXEL_BYTES];
        file.read_exact(bytes)
            .map_err(|_| corrupt("ends before its map does"))?;
        let bytes: &[u8] = bytes;
        pieces.extend(piece_hashes(threads, bytes));
        let failed = std::sync::atomic::AtomicBool::new(false);
        par_chunks(out, 1 << 16, threads, |from, part| {
            for (k, slot) in part.iter_mut().enumerate() {
                let at = (from + k) * TEXEL_BYTES;
                match decode(&bytes[at..at + TEXEL_BYTES]) {
                    Some(texel) => *slot = texel,
                    None => failed.store(true, std::sync::atomic::Ordering::Relaxed),
                }
            }
        });
        bad |= failed.load(std::sync::atomic::Ordering::Relaxed);
    }
    let mut tail = [0u8; 8];
    file.read_exact(&mut tail)
        .map_err(|_| corrupt("has no checksum"))?;
    let sum = pieces.into_iter().fold(0x6a09_e667_f3bc_c908, mix);
    if sum != u64::from_le_bytes(tail) {
        return Err(corrupt("its checksum is wrong"));
    }
    if bad {
        return Err(corrupt("has a texel that cannot be"));
    }
    Ok(WorldMap {
        seed,
        size,
        radius,
        relief: Relief::DEFAULT,
        stretch: (1.0, 1.0),
        texels,
    })
}

/// Writes `map` to `dir` (made if it is not there), over what was there: to a temporary file in
/// the folder, and then a rename.
pub fn store(dir: &Path, map: &WorldMap) -> io::Result<()> {
    store_on(machine::threads(), dir, map)
}

/// [`store`] on `threads` threads: the file is the same whatever they are. Once it is written,
/// the folder is tidied ([`tidy`]).
pub fn store_on(threads: usize, dir: &Path, map: &WorldMap) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let path = path_in(dir, map.seed);
    let temporary = path.with_extension(format!("tmp{}", std::process::id()));
    let written = (|| {
        let mut out = BufWriter::with_capacity(1 << 20, File::create(&temporary)?);
        out.write_all(&header(map.seed, map.size, map.radius))?;
        let mut pieces = Vec::new();
        let mut buffer = vec![0u8; CHUNK];
        for texels in map.texels.chunks(CHUNK / TEXEL_BYTES) {
            let bytes = &mut buffer[..texels.len() * TEXEL_BYTES];
            par_chunks(bytes, 1 << 19, threads, |from, part| {
                for (k, slot) in part.as_chunks_mut::<TEXEL_BYTES>().0.iter_mut().enumerate() {
                    *slot = encode(&texels[from / TEXEL_BYTES + k]);
                }
            });
            let bytes: &[u8] = bytes;
            pieces.extend(piece_hashes(threads, bytes));
            out.write_all(bytes)?;
        }
        let sum = pieces.into_iter().fold(0x6a09_e667_f3bc_c908, mix);
        out.write_all(&sum.to_le_bytes())?;
        out.flush()?;
        fs::rename(&temporary, &path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    } else {
        tidy(dir);
    }
    written
}

/// Deletes what the folder should not keep, best effort (a file that cannot be deleted is left):
/// the temporary files of writes that were killed ([`STALE_TEMPORARY`] old; a younger one is
/// another launch writing now), and all but the [`MOST_MAPS`] maps that were used last.
pub fn tidy(dir: &Path) {
    let now = std::time::SystemTime::now();
    let mut maps = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|n| n.starts_with("world-map-")) else {
            continue;
        };
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if name.ends_with(".bin") {
            maps.push((modified, entry.path()));
        } else if name.contains(".tmp")
            && now
                .duration_since(modified)
                .is_ok_and(|age| age >= STALE_TEMPORARY)
        {
            let _ = fs::remove_file(entry.path());
        }
    }
    // The most recent first; the ties (a file system with coarse times) in name order.
    maps.sort_by(|a, b| b.cmp(a));
    for (_, path) in maps.into_iter().skip(MOST_MAPS) {
        let _ = fs::remove_file(path);
    }
}

/// The map of `seed`, `size` and `radius`: from `dir` if the file there is its own and whole, else
/// built on `threads` threads and written over it. `dir` is `None` if there is no cache (see
/// [`default_dir`]). The map is the same either way.
pub fn load_or_build(
    dir: Option<&Path>,
    seed: u64,
    size: u32,
    radius: i32,
    threads: usize,
) -> Loaded {
    let started = Instant::now();
    let miss = match dir {
        None => Miss::Off,
        Some(dir) => match load_on(threads, dir, seed, size, radius) {
            Ok(map) => {
                return Loaded {
                    map,
                    outcome: Outcome::Hit,
                    took: started.elapsed(),
                    stages: None,
                    wrote: None,
                };
            }
            Err(miss) => miss,
        },
    };
    let (map, stages) = WorldMap::with_threads(seed, size, radius, threads);
    let took = started.elapsed();
    let wrote = dir.map(|dir| {
        let started = Instant::now();
        store_on(threads, dir, &map)
            .map(|()| started.elapsed())
            .map_err(|e| e.to_string())
    });
    Loaded {
        map,
        outcome: Outcome::Built(miss),
        took,
        stages: Some(stages),
        wrote,
    }
}

#[cfg(test)]
mod tests;
