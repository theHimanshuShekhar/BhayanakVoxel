//! Golden hashes of the test Planet after a fixed run of Edits. CI runs this on Linux and
//! Windows, debug and release, and every runner checks against the values below, so all runners
//! must agree with each other (ADR 0003).
//!
//! Each dig is checked twice: the hash of level 0, and the hash of the store's coarse LOD levels
//! (`lod_hash`), so a change to the downsampling rule or to how an Edit propagates up the
//! pyramid fails here on every runner too.
//!
//! The Brushes (every mode and shape, in both alignments, with the integer rasterisation of the
//! tilted ones) have goldens of their own at the end: the same hashes after each Edit.
//!
//! If `apply`, the downsampling or the generator changes on purpose, replace the hashes with the
//! `actual` column printed by a failing run.

use brick_store::{BrickCoord, BrickStore, Material, VoxelCoord};
use net::{Alignment, Brush, EditMode, EditOp, Shape};
use planet_gen::generate_region;

const SEED: u64 = 0x5EED_B41A_2024;

/// (centre, radius) of each dig, in order.
type Script = &'static [([i32; 3], u16)];

/// Digs into the surface: overlapping, repeated, and with the largest radius and the smallest.
const SURFACE: Script = &[
    ([1490, 0, 0], 10),
    ([1495, 5, -3], 10),
    ([1480, -8, 4], 20),
    ([1478, 10, -15], 3),
    ([1490, 0, 0], 10),
    ([1470, 2, 2], 20),
    ([1460, -20, 20], 0),
    ([1493, -20, 9], 15),
];

/// Digs from an empty store, in the Planet's core: across zero, with negative coordinates, and
/// on brick boundaries.
const CORE: Script = &[
    ([0, 0, 0], 10),
    ([-5, -5, -5], 6),
    ([30, -4, 2], 20),
    ([-12, -12, -12], 1),
    ([15, 15, 15], 4),
    ([-8, 8, -48], 9),
];

/// Coarse levels built over each store before its digs.
///
/// The surface store holds every brick of its region, the core's among them, and nothing else, so
/// it has no source and its coarse levels are those of that block. The core store is empty and
/// reads as `planet_gen::CORE`. Its digs are within a few bricks of the centre, and every coarse
/// brick over them up to level 6 (512 Voxels wide) is wholly inside the core's radius of 1462, so
/// those levels are true to the Planet. Level 7 would not be, as its bricks are over the core's
/// edge and there is no crust held to make them from.
const SURFACE_LEVELS: usize = 6;
const CORE_LEVELS: usize = 6;

/// The store's hash after each dig of `SURFACE`, on the generated surface bricks.
#[rustfmt::skip]
const SURFACE_GOLDEN: [u64; 8] = [
    0xda65c09d15d918b4, 0x0ed3f5f5b0c3019b, 0x2e37c631d06dc0d0, 0xa7c5148bcf8a40c6,
    0xa7c5148bcf8a40c6, 0x634576da5d672bff, 0x3940fdf10f501031, 0x13e2e17d09cfd637,
];

/// The store's hash after each dig of `CORE`, from an empty store.
#[rustfmt::skip]
const CORE_GOLDEN: [u64; 6] = [
    0xaaa55d8d46ef2c0d, 0x523b20b39982a046, 0xb47526a25cceeb63, 0x15dfba8001a3b665,
    0x60a9fc894845caf6, 0x69d4a74cda43ccae,
];

/// The store's hash after each dig of `SURFACE`, over its coarse levels.
#[rustfmt::skip]
const SURFACE_LOD_GOLDEN: [u64; 8] = [
    0x726b1bd2e356611e, 0x2f192f18dd65ada3, 0xd49787e2c9e04420, 0x7dd1e5e4db8e4b80,
    0x7dd1e5e4db8e4b80, 0xde2f51cd5c3d20a8, 0xde2f51cd5c3d20a8, 0xcd0ec34bedfaaf34,
];

/// The store's hash after each dig of `CORE`, over its coarse levels.
#[rustfmt::skip]
const CORE_LOD_GOLDEN: [u64; 6] = [
    0x64fa160138feb2f7, 0xe97f97fb8f969fc8, 0x6e1edfcb9027e54c, 0x6e1edfcb9027e54c,
    0x63419f5df7d66b22, 0x1abbb95b46d8d34c,
];

/// The hashes of level 0 and of the coarse levels after each dig of `script`, on `store` with
/// `levels` coarse levels built over it (and over its source, if it has one).
fn run(mut store: BrickStore, levels: usize, script: Script) -> (Vec<u64>, Vec<u64>) {
    store.build_lod(levels);
    script
        .iter()
        .map(|&(centre, radius)| {
            let op = EditOp::dig_sphere(VoxelCoord(centre), radius);
            sim::apply(&mut store, &op);
            (store.content_hash(), store.lod_hash())
        })
        .unzip()
}

fn check(name: &str, got: &[u64], want: &[u64]) {
    let report: String = got
        .iter()
        .zip(want)
        .enumerate()
        .map(|(i, (got, want))| format!("{name} {i} actual {got:#018x} want {want:#018x}\n"))
        .collect();
    assert_eq!(got, want, "golden mismatch:\n{report}");
}

#[test]
fn golden_edits_on_the_surface() {
    let store = generate_region(SEED, BrickCoord([180, -3, -3]), BrickCoord([192, 3, 3]));
    let (level_0, lod) = run(store, SURFACE_LEVELS, SURFACE);
    check("surface", &level_0, &SURFACE_GOLDEN);
    check("surface lod", &lod, &SURFACE_LOD_GOLDEN);
}

#[test]
fn golden_edits_in_the_core() {
    let mut store = BrickStore::new();
    store.set_source(planet_gen::CORE);
    let (level_0, lod) = run(store, CORE_LEVELS, CORE);
    check("core", &level_0, &CORE_GOLDEN);
    check("core lod", &lod, &CORE_LOD_GOLDEN);
}

/// One Edit of the Brush goldens: mode, shape, radius, centre, Material and alignment.
type Brushed = (EditMode, Shape, u16, [i32; 3], u16, Alignment);

use Alignment::{Local, World};
use EditMode::{Dig, Flatten, Place, Smooth};
use Shape::{Cube, Cylinder, Sphere};

/// Edits of the generated surface (near Voxel x = 1500, so up is the +x axis): a crater dug with
/// each shape, built on again with each, rounded off and levelled, with the Brush aligned to the
/// Planet and to the grid.
const SURFACE_BRUSHES: &[Brushed] = &[
    (Dig, Cube, 9, [1490, 0, 0], 0, Local),
    (Dig, Cylinder, 12, [1488, 6, -4], 0, World),
    (Place, Cylinder, 6, [1494, 0, 0], 35, Local),
    (Place, Cube, 5, [1498, -3, 2], 42, World),
    (Place, Sphere, 8, [1492, 4, 4], 7, Local),
    (Smooth, Sphere, 10, [1490, 2, 0], 0, Local),
    (Smooth, Cube, 7, [1496, -2, 3], 0, World),
    (Flatten, Cylinder, 14, [1491, 0, 1], 35, Local),
    (Flatten, Cube, 8, [1489, -5, -5], 7, World),
    (Dig, Sphere, 20, [1478, 0, 0], 0, Local),
];

/// Edits in the Planet's core, where up is every other way than +x: tilted cubes and cylinders
/// dug into the rock, refilled, rounded and levelled against a tangent plane that is not square
/// to the grid.
const CORE_BRUSHES: &[Brushed] = &[
    (Dig, Cube, 8, [20, 20, 3], 0, Local),
    (Dig, Cylinder, 10, [-12, 13, 14], 0, Local),
    (Place, Cylinder, 5, [20, 20, 3], 7, Local),
    (Place, Cube, 6, [-12, 13, 14], 35, Local),
    (Smooth, Sphere, 9, [18, 18, 2], 0, Local),
    (Flatten, Cube, 9, [-10, 12, 12], 42, Local),
    (Flatten, Cylinder, 7, [30, 0, 30], 7, Local),
    (Dig, Cube, 12, [0, -30, 4], 0, World),
    (Smooth, Cylinder, 8, [12, -18, 16], 0, World),
];

/// One Place in free space for each of the 26 directions the Planet's up snaps to, in turn a cube
/// and a cylinder, and then a Flatten and a Smooth across the lot.
fn free_brushes() -> Vec<Brushed> {
    let mut ops = Vec::new();
    for x in -1..=1 {
        for y in -1..=1 {
            for z in -1..=1 {
                if (x, y, z) != (0, 0, 0) {
                    let shape = if ops.len() % 2 == 0 { Cube } else { Cylinder };
                    ops.push((Place, shape, 5, [x * 1000, y * 1000, z * 1000], 35, Local));
                }
            }
        }
    }
    ops.push((Flatten, Cube, 9, [1000, 1000, 0], 7, Local));
    ops.push((Smooth, Sphere, 7, [0, -1000, -1000], 0, Local));
    ops
}

/// The hashes of level 0 and of the coarse levels after each of `ops`, on `store` with `levels`
/// coarse levels built over it.
fn run_brushes(mut store: BrickStore, levels: usize, ops: &[Brushed]) -> (Vec<u64>, Vec<u64>) {
    store.build_lod(levels);
    ops.iter()
        .map(|&(mode, shape, radius, centre, material, alignment)| {
            let op = EditOp {
                mode,
                brush: Brush { shape, radius },
                centre: VoxelCoord(centre),
                material: Material(material),
                alignment,
            };
            assert_eq!(sim::validate(&op), Ok(()), "{op:?}");
            sim::apply(&mut store, &op);
            (store.content_hash(), store.lod_hash())
        })
        .unzip()
}

#[rustfmt::skip]
const SURFACE_BRUSHES_GOLDEN: [u64; 10] = [
    0xde6763a27e20d231, 0x853250e977f1a48d, 0xcba73f3adbc0e270, 0x71205ccdd3e3d363,
    0xcc8eb58c3636fdfc, 0x3c09bc20dee763fc, 0x2676b77b277d7034, 0xa11c3ce418ccca3e,
    0x69f0448a60dd2e2d, 0x8f975e9c0360c75b,
];
#[rustfmt::skip]
const SURFACE_BRUSHES_LOD_GOLDEN: [u64; 10] = [
    0x29374ae5903b4792, 0x3507e9bdb5b987e1, 0x7c071d968574fa00, 0x47f8d433e46c990c,
    0x5e7d523c55447234, 0x6ce0953b1e7b09f4, 0x40808873f689fcb7, 0x812c3e3ed6c987c3,
    0xf6cec76ebae38d9c, 0x4b9798b659f6f443,
];
#[rustfmt::skip]
const CORE_BRUSHES_GOLDEN: [u64; 9] = [
    0xbb04f09c434fa261, 0xa39212fb0eb2adb4, 0x6553d51e63e45890, 0xcbc5b9d22f9f9a61,
    0x42e033bf14dbd0c0, 0xb34a294b68e1e3e3, 0xa54190bc95637457, 0x2ff22a4df9aa15e6,
    0xf3cde748fc53d68e,
];
#[rustfmt::skip]
const CORE_BRUSHES_LOD_GOLDEN: [u64; 9] = [
    0x2d388147b8f48653, 0x39f55945b5a8a1c3, 0x1236ca8e465cc70a, 0xc6c8915f455e9c02,
    0x8a745a96cab6407d, 0xc302160b5822511d, 0xa18aa0a1f8ab278e, 0x595888255985342c,
    0x595888255985342c,
];
#[rustfmt::skip]
const FREE_BRUSHES_GOLDEN: [u64; 28] = [
    0xcae4b8069c7674cf, 0x8998d6610ff961ca, 0xbabae5ecc61a7195, 0x284e936d38f7ee92,
    0xe8e2624bf49c51d6, 0x7ef3fdb3e6f0beb2, 0x3882613b92df6e43, 0x8e1ba4ea5e974a19,
    0x469f94e6d557d44a, 0xefbe9173b66f76c9, 0xd53816e2c7a440b1, 0xc42f6aa8d4ee0219,
    0xd525585bdddce8ed, 0x831662c28f856f00, 0x94a1db8314d4882f, 0x6b26515f2a9b728d,
    0x211dd13404ac624a, 0x10f0705eaf608566, 0xb413bcc2f4698f0e, 0x08330f21330172ee,
    0x01839a35078265ec, 0x21ecc13c8ddb9283, 0x633219b99e442f05, 0x41e7c042ee8c93dd,
    0x405bf64c17494d91, 0xbf31ca4e5e0ae8d8, 0x3d656f346290f173, 0x9292f675274fadc1,
];
#[rustfmt::skip]
const FREE_BRUSHES_LOD_GOLDEN: [u64; 28] = [
    0x26bca923600eb91f, 0xade2c5cf9a057352, 0x939ed26f2a86b640, 0xe951fa08822795a9,
    0xf1b595c82201a8a8, 0x8893aee484dd1ca9, 0xcf68694ec3247590, 0xf4c16d411a262f07,
    0xb24ae3691df551c9, 0x2717d6e6a4ae9f09, 0xaa2622ceca928b9a, 0xde179549b33aeedc,
    0x59b5932846860b07, 0x329f811bbc8ecd0c, 0x6b58f180a5a40a97, 0x1c4e7309fa6e989c,
    0xd6d838b8b4ca599d, 0x3963997be449d1af, 0x35bb4d76003c848a, 0xafc44a0a369324a6,
    0x49585d9a5f805ed4, 0x3e60cb702c1b1796, 0xc4d9845e76625c6d, 0x89ae5fdb8d1c69c0,
    0xb23cbc832ea25efe, 0xd315a6e4e9537dfd, 0x90f3c5f65a9c40b0, 0x3bd6331576fc7aff,
];

#[test]
fn golden_brushes_on_the_surface() {
    let store = generate_region(SEED, BrickCoord([180, -3, -3]), BrickCoord([192, 3, 3]));
    let (level_0, lod) = run_brushes(store, SURFACE_LEVELS, SURFACE_BRUSHES);
    check("surface brushes", &level_0, &SURFACE_BRUSHES_GOLDEN);
    check("surface brushes lod", &lod, &SURFACE_BRUSHES_LOD_GOLDEN);
}

#[test]
fn golden_brushes_in_the_core() {
    let mut store = BrickStore::new();
    store.set_source(planet_gen::CORE);
    let (level_0, lod) = run_brushes(store, CORE_LEVELS, CORE_BRUSHES);
    check("core brushes", &level_0, &CORE_BRUSHES_GOLDEN);
    check("core brushes lod", &lod, &CORE_BRUSHES_LOD_GOLDEN);
}

#[test]
fn golden_brushes_in_every_direction_of_up_in_free_space() {
    let (level_0, lod) = run_brushes(BrickStore::new(), 3, &free_brushes());
    check("free brushes", &level_0, &FREE_BRUSHES_GOLDEN);
    check("free brushes lod", &lod, &FREE_BRUSHES_LOD_GOLDEN);
}
