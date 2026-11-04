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

/// One Edit of the Brush goldens: mode, shape, radius, centre, Material, alignment and rotation.
type Brushed = (EditMode, Shape, u16, [i32; 3], u16, Alignment, u8);

use Alignment::{Local, World};
use EditMode::{Dig, Flatten, Place, Smooth};
use Shape::{Cube, Cylinder, Sphere};

/// Edits of the generated surface (near Voxel x = 1500, so up is the +x axis): a crater dug with
/// each shape, built on again with each, rounded off and levelled, with the Brush aligned to the
/// Planet and to the grid.
const SURFACE_BRUSHES: &[Brushed] = &[
    (Dig, Cube, 9, [1490, 0, 0], 0, Local, 1),
    (Dig, Cylinder, 12, [1488, 6, -4], 0, World, 0),
    (Place, Cylinder, 6, [1494, 0, 0], 35, Local, 0),
    (Place, Cube, 5, [1498, -3, 2], 42, World, 1),
    (Place, Sphere, 8, [1492, 4, 4], 7, Local, 0),
    (Smooth, Sphere, 10, [1490, 2, 0], 0, Local, 0),
    (Smooth, Cube, 7, [1496, -2, 3], 0, World, 0),
    (Flatten, Cylinder, 10, [1491, 0, 1], 35, Local, 0),
    (Flatten, Cube, 8, [1489, -5, -5], 7, World, 3),
    (Dig, Sphere, 20, [1478, 0, 0], 0, Local, 0),
];

/// Edits in the Planet's core, where up is every other way than +x: tilted cubes and cylinders
/// dug into the rock, refilled, rounded and levelled against a tangent plane that is not square
/// to the grid.
const CORE_BRUSHES: &[Brushed] = &[
    (Dig, Cube, 8, [20, 20, 3], 0, Local, 1),
    (Dig, Cylinder, 10, [-12, 13, 14], 0, Local, 0),
    (Place, Cylinder, 5, [20, 20, 3], 7, Local, 0),
    (Place, Cube, 6, [-12, 13, 14], 35, Local, 1),
    (Smooth, Sphere, 9, [18, 18, 2], 0, Local, 0),
    (Flatten, Cube, 9, [-10, 12, 12], 42, Local, 5),
    (Flatten, Cylinder, 7, [30, 0, 30], 7, Local, 0),
    (Dig, Cube, 12, [0, -30, 4], 0, World, 0),
    (Smooth, Cylinder, 8, [12, -18, 16], 0, World, 0),
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
                    ops.push((
                        Place,
                        shape,
                        5,
                        [x * 1000, y * 1000, z * 1000],
                        35,
                        Local,
                        0,
                    ));
                }
            }
        }
    }
    ops.push((Flatten, Cube, 9, [1000, 1000, 0], 7, Local, 0));
    ops.push((Smooth, Sphere, 7, [0, -1000, -1000], 0, Local, 0));
    // Then the two that turn: a cube and the flatten of one, an eighth of a turn about up.
    ops.push((Place, Cube, 6, [1000, 1000, 1000], 42, Local, 1));
    ops.push((Flatten, Cube, 7, [-1000, 0, 1000], 35, Local, 3));
    ops
}

/// The hashes of level 0 and of the coarse levels after each of `ops`, on `store` with `levels`
/// coarse levels built over it.
fn run_brushes(mut store: BrickStore, levels: usize, ops: &[Brushed]) -> (Vec<u64>, Vec<u64>) {
    store.build_lod(levels);
    ops.iter()
        .map(
            |&(mode, shape, radius, centre, material, alignment, rotation)| {
                let op = EditOp {
                    mode,
                    brush: Brush { shape, radius },
                    centre: VoxelCoord(centre),
                    material: Material(material),
                    alignment,
                    rotation,
                };
                assert_eq!(sim::validate(&op), Ok(()), "{op:?}");
                sim::apply(&mut store, &op);
                (store.content_hash(), store.lod_hash())
            },
        )
        .unzip()
}

#[rustfmt::skip]
const SURFACE_BRUSHES_GOLDEN: [u64; 10] = [
    0xf57145a090bcbe6a, 0x19b320594a6481fc, 0xda8412aa525cfdd3, 0xccaf7e076ae9ee53,
    0x7d5fe8a2bcca6fa5, 0x97fd42774e3efe8a, 0xa1ab407510330ec3, 0x2357ba32e84a4f37,
    0x6dab7b729b4a03fc, 0xdc1b4fc7e33c3aca,
];
#[rustfmt::skip]
const SURFACE_BRUSHES_LOD_GOLDEN: [u64; 10] = [
    0xea2421f445a8c61d, 0xf08377e7e8611814, 0x0167f0b4a5621694, 0x262a0dd8dcfcd23b,
    0x13a29576157f15ee, 0x7965b7e990e2652c, 0x63dafb67542fbee4, 0xc91e103dbfc63e27,
    0xf496b073ed1f47f3, 0xa8ac88a169e26429,
];
#[rustfmt::skip]
const CORE_BRUSHES_GOLDEN: [u64; 9] = [
    0xc2a66fd87dd0859f, 0x7a5d5e01b1f251e4, 0x7861fbf64c0f8770, 0xffe87ec4c48a9770,
    0x48b10dec0d44a498, 0x0663f2edc6c38407, 0x95b151d88699bb47, 0x2815975753e923b2,
    0xbcb7aeb7dc4be622,
];
#[rustfmt::skip]
const CORE_BRUSHES_LOD_GOLDEN: [u64; 9] = [
    0xbdaae0575e5ed912, 0xf48811bf04009ea6, 0x64a9b5cddf0e5000, 0xb4b1e477f21f816b,
    0x7dfad1594635912d, 0x97ee1c72258ff88a, 0x6000dc41d0d8804c, 0x8b5a57b10e74d381,
    0x8b5a57b10e74d381,
];
#[rustfmt::skip]
const FREE_BRUSHES_GOLDEN: [u64; 30] = [
    0x0ce9e43ae0d9b69f, 0x2f027dc7a50e7972, 0x5ae8da0b716ccdee, 0x23b899c449ac43ed,
    0x18c494243a79b28d, 0xeb1d3e4f73f4c599, 0xb6f5b1aa55dc3efd, 0x4a16d723fd3c4553,
    0x941bf5b43d3f9f20, 0x925ecc0cce9f5257, 0x6b5a62ef1908e80f, 0xe3bd667886c42683,
    0xbedd46071fc7647f, 0x640a73d5a23c1b8e, 0x32962d5a4ce156c5, 0xa114983bd974917f,
    0xcd56c4bce7ed60b4, 0xadcc59e713fadba4, 0x827f05520a973c48, 0xb64f48f5eca24660,
    0x5d48852464532476, 0x47740e9e25259335, 0xb238a91eeb01c8bf, 0x535cdaab7262d467,
    0x80a137367cc6e247, 0x6532ed46e2c15fa6, 0xe9be9a6e0c202af9, 0xe8e919be25065ab7,
    0xed3ebf314294a8f7, 0x9f3b82f5ea39eba4,
];
#[rustfmt::skip]
const FREE_BRUSHES_LOD_GOLDEN: [u64; 30] = [
    0x7dde92f03b3703fd, 0xcf1b285a3caceca6, 0xff90c5a799f7e21f, 0x40164b3853184104,
    0xc6b803034df5647a, 0x87a6b136fbdf1a8f, 0x7fbdee8e8be0d7d7, 0x7d96ca26e6b0f65b,
    0x3ca9f8f813ff0970, 0xf88e0bda37173086, 0x606ffdd0b391c57d, 0xbe808d8293228947,
    0x2c48099e126efa95, 0x5f133e4a2076c288, 0xa1a017aab1837ce5, 0x07687277d5d21619,
    0xac3345dcd499cf0c, 0xfaec55e4b0327ed3, 0x9363fba238a4cdae, 0xc7dfbd105f32bcfc,
    0xb7230a86bfdc1dfa, 0xd7cbe3f1743e0bdc, 0x9bc5d52b52e29551, 0x4d54da4ae9f67600,
    0x2647d6eb95ed6124, 0x6f78fae4ace78b01, 0xb46f3b3d839beea9, 0xcd131c430d8f9fd6,
    0xb31a3638f82e540b, 0xd75d49b0c02fae09,
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
