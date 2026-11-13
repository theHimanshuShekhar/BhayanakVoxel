//! Golden hashes of generated bricks. CI runs this on Linux and Windows, debug and release,
//! and every runner checks against the values below, so all runners must agree with each other.
//!
//! If generation changes on purpose, bump `GENERATOR_VERSION`, then replace the hashes with the
//! `actual` column printed by a failing run.

use brick_store::{Brick, BrickCoord};
use planet_gen::planet::Planet;
use planet_gen::world_map::WorldMap;
use planet_gen::{GENERATOR_VERSION, generate_brick};
use std::sync::OnceLock;

const SEED: u64 = 0x5EED_B41A_2024;

#[derive(Debug, PartialEq)]
enum Kind {
    Air,
    Solid,
    Mixed,
}

use Kind::{Air, Mixed, Solid};

fn kind(brick: &Brick) -> Kind {
    match (brick.is_air(), brick.is_uniform()) {
        (true, _) => Air,
        (false, true) => Solid,
        (false, false) => Mixed,
    }
}

/// (brick coordinate, expected kind, FNV-1a hash of the brick's canonical content)
#[rustfmt::skip]
const GOLDEN: &[([i32; 3], Kind, u64)] = &[
    // Interior: the core, across zero, and deep inside.
    ([0, 0, 0], Solid, 0x479c06dceb2be325),
    ([-1, -1, -1], Solid, 0x479c06dceb2be325),
    ([100, -100, 50], Solid, 0x479c06dceb2be325),
    // Exterior.
    ([400, 0, 0], Air, 0x6ab05ef9aa8b9b25),
    ([-300, 300, -300], Air, 0x6ab05ef9aa8b9b25),
    ([i32::MIN / 8, i32::MAX / 8, 0], Air, 0x6ab05ef9aa8b9b25),
    // Surface, on the axes and on a diagonal, on both sides of zero.
    ([186, 0, 0], Mixed, 0x7ae8dbdf6821ca28),
    ([-187, 3, -2], Mixed, 0x89b5d0f3e4dc2cee),
    ([-188, 3, -2], Mixed, 0xf1e1191c27781aa5),
    ([0, 1, -187], Mixed, 0xfec71a197f20be1f),
    ([108, 108, 108], Mixed, 0x8d9615f180f082dd),
    ([-108, 108, -108], Mixed, 0xb3c2f24c2e422de9),
    ([107, -107, 109], Mixed, 0xb7b8e540d6611761),
    // Edge cases: bricks inside the surface band that are sampled but turn out uniform.
    ([187, 0, 0], Air, 0x6ab05ef9aa8b9b25),
    ([0, 187, 0], Solid, 0x479c06dceb2be325),
];

#[test]
fn golden_generator_version() {
    // Changing this means the hashes below were regenerated on purpose.
    assert_eq!(GENERATOR_VERSION, 5);
}

#[test]
fn golden_brick_hashes() {
    let mut report = String::new();
    let mut failed = false;
    for (coord, want_kind, want_hash) in GOLDEN {
        let brick = generate_brick(SEED, BrickCoord(*coord));
        let (got_kind, got_hash) = (kind(&brick), brick.content_hash());
        failed |= got_kind != *want_kind || got_hash != *want_hash;
        report +=
            &format!("{coord:?} {got_kind:?} actual {got_hash:#018x} want {want_hash:#018x}\n");
    }
    assert!(!failed, "golden mismatch:\n{report}");
}

/// Texels along a face of the world map the golden hash covers: coarse, to keep CI fast. The
/// map's algorithm is the same at every size.
const WORLD_MAP_SIZE: u32 = 128;

const WORLD_MAP_HASH: u64 = 0xa8b3355aa886d5b2;

#[test]
fn golden_world_map_hash() {
    let actual = WorldMap::generate(SEED, WORLD_MAP_SIZE).content_hash();
    assert_eq!(actual, WORLD_MAP_HASH, "actual {actual:#018x}");
}

/// Bricks of the real Planet (320 km, `WORLD_RADIUS`) on the coarse world map above, with a sample
/// of every layer of it.
#[rustfmt::skip]
const PLANET_GOLDEN: &[([i32; 3], Kind, u64)] = &[
    // The core, its edge, deep rock, and the interior's outer edge.
    ([0, 0, 0], Solid, 0x02c5150a75e71525),
    ([160000, 0, 0], Mixed, 0x9c31dfda9ca31e8a),
    ([160002, 0, 0], Solid, 0xd97e511494bfdb25),
    ([0, 300000, 0], Solid, 0xd97e511494bfdb25),
    ([797329, 0, 0], Solid, 0xd97e511494bfdb25),
    // Temperate land, from the sky down: air, the ground, the strata and the boundaries between
    // them, and deep rock.
    ([533743, -517162, 296419], Air, 0x6ab05ef9aa8b9b25),
    ([533735, -517154, 296414], Air, 0x6ab05ef9aa8b9b25),
    ([533733, -517152, 296413], Mixed, 0x7ea1273154058e0c),
    ([533730, -517150, 296412], Solid, 0xb3c594f58463b525),
    ([533722, -517141, 296407], Solid, 0x6fa64afc62031525),
    ([533708, -517129, 296399], Solid, 0xe0e4f56507851525),
    ([533675, -517096, 296381], Solid, 0x4d3463ca4f10d325),
    ([533652, -517074, 296368], Solid, 0xd97e511494bfdb25),
    // The other biomes: sand over sandstone, snow over ice over gravel over granite, basalt
    // and obsidian.
    ([504902, -458400, -418462], Solid, 0x8be0c40c241ab525),
    ([504895, -458394, -418456], Solid, 0x0dc0fd820e036725),
    ([-15725, 592256, 537708], Mixed, 0x21af38d1d5f1499a),
    ([-15725, 592253, 537705], Solid, 0xfeb0233604966725),
    ([-15725, 592238, 537692], Mixed, 0x53af882b1adee525),
    ([-15724, 592208, 537665], Solid, 0x4d3463ca4f10d325),
    ([579250, -525901, 167828], Mixed, 0xa30d7e286015d7b1),
    ([579247, -525898, 167827], Solid, 0xfc11e2d7244c1525),
    ([579232, -525884, 167823], Solid, 0x3725155588c5a725),
    // The ocean: water, the sea's surface and the sea floor, and a coast.
    ([463954, -459131, 459131], Solid, 0xe0e4f56507851525),
    ([463976, -459153, 459152], Mixed, 0x6226f4947a643083),
    ([463978, -459155, 459154], Solid, 0xb812ab782d86a725),
    ([465098, -460263, 460262], Mixed, 0x3303cd46092608ed),
    ([465099, -460264, 460263], Air, 0x6ab05ef9aa8b9b25),
    ([522061, -453161, 402602], Mixed, 0xf87202c3998cdda4),
    ([522059, -453159, 402601], Solid, 0x6fa64afc62031525),
    // A polar sea, from the water below to the air above: the water, the ice and the water under
    // it, and the ice and the air over it.
    ([-49868, 602968, 523389], Solid, 0xb812ab782d86a725),
    ([-49868, 602970, 523391], Solid, 0xfeb0233604966725),
    ([-49868, 602971, 523391], Mixed, 0xa600681c28adecc5),
    ([-49868, 602971, 523392], Mixed, 0x3afd700576df75c5),
    ([-49868, 602972, 523392], Air, 0x6ab05ef9aa8b9b25),
    // Caves (#46), at a few of the mouths on this map and under it: the sky above a mouth, the
    // brick that the mouth is in, bricks that are mostly cave, one wholly cave under the ground
    // (air, where the strata would be), and the strata below, which the caves did not change.
    ([509730, -475221, 393241], Air, 0x6ab05ef9aa8b9b25),
    ([509714, -475223, 393218], Mixed, 0x9383d22ddbdefea6),
    ([509715, -475223, 393218], Mixed, 0x53b61b2c5558fb39),
    ([509703, -475221, 393218], Mixed, 0x26f0d08316654baf),
    ([509703, -475219, 393218], Mixed, 0x59fe2c63e00e70f1),
    ([510182, -474707, 393150], Solid, 0xb3c594f58463b525),
    ([510179, -474704, 393147], Solid, 0x6fa64afc62031525),
    // Chambers, which are wide enough to be seen from further away: bricks wholly cave.
    ([509705, -475204, 393222], Air, 0x6ab05ef9aa8b9b25),
    ([650925, -319280, 338531], Air, 0x6ab05ef9aa8b9b25),
    ([650936, -319278, 338519], Air, 0x6ab05ef9aa8b9b25),
];

/// Boxes of Voxels from the centre, a few caves' cubes each, that [`CAVES_GOLDEN`] hashes the
/// tubes of: around the mouths and chambers of the bricks above.
const CAVES_BOXES: [([f64; 3], [f64; 3]); 2] = [
    (
        [4_075_000.0, -3_805_000.0, 3_143_000.0],
        [4_085_000.0, -3_795_000.0, 3_149_000.0],
    ),
    (
        [5_200_000.0, -2_560_000.0, 2_705_000.0],
        [5_210_000.0, -2_550_000.0, 2_711_000.0],
    ),
];

/// The hash of the tubes of the caves that the walk makes in each of the boxes above: the tubes
/// themselves, so that a change to the walk, or to the floats under it, shows here before it
/// shows in the bricks.
const CAVES_GOLDEN: [u64; 2] = [0x18bf7a329aead62f, 0xb5e4e9da53fa1dbe];

#[test]
fn golden_caves_hash() {
    let actual: Vec<u64> = CAVES_BOXES
        .iter()
        .map(|(low, high)| planet().caves_hash(*low, *high))
        .collect();
    let hex: Vec<String> = actual.iter().map(|h| format!("{h:#018x}")).collect();
    assert_eq!(actual, CAVES_GOLDEN, "actual {hex:?}");
}

/// Voxels around the ground that the region golden covers, and the ground: the temperate land
/// above, at the corner of its brick (which is within a brick of the ground).
const REGION_REACH: i32 = 80;
const REGION_GROUND: [f64; 3] = [4269870.0, -4137219.0, 2371310.0];

/// The hash of the store that `region_around` makes there.
const REGION_HASH: u64 = 0x9a1bca7e0602d6bc;

fn planet() -> &'static Planet {
    static PLANET: OnceLock<Planet> = OnceLock::new();
    PLANET.get_or_init(|| Planet::new(WorldMap::generate(SEED, WORLD_MAP_SIZE)))
}

/// The Spawn of this seed, to the bit (`f64::to_bits`): the same on every machine.
const SPAWN_BITS: [u64; 3] = [
    4706891993883463654,
    4706316363597622642,
    4688651980890690691,
];

#[test]
fn golden_spawn() {
    let spawn = planet().spawn(100);
    let actual = spawn.map(f64::to_bits);
    assert_eq!(actual, SPAWN_BITS, "actual {actual:?}, {spawn:?}");
}

#[test]
fn golden_planet_brick_hashes() {
    let mut report = String::new();
    let mut failed = false;
    for (coord, want_kind, want_hash) in PLANET_GOLDEN {
        let brick = planet().brick(BrickCoord(*coord));
        let (got_kind, got_hash) = (kind(&brick), brick.content_hash());
        failed |= got_kind != *want_kind || got_hash != *want_hash;
        report +=
            &format!("{coord:?} {got_kind:?} actual {got_hash:#018x} want {want_hash:#018x}\n");
    }
    assert!(!failed, "golden mismatch:\n{report}");
}

#[test]
fn golden_region_hash() {
    let region = planet().region_around(REGION_GROUND, REGION_REACH);
    let actual = region.content_hash();
    assert_eq!(
        actual,
        REGION_HASH,
        "actual {actual:#018x}, {} bricks",
        region.len()
    );
}

/// Coarse bricks of the real Planet on the same world map, generated directly at their level
/// (`Planet::coarse_brick`): (mip level, a brick of level 0 under it, expected kind, hash). The
/// level 0 bricks are those of the ground above, and the coarse brick is the one over each.
/// The Voxels with air beside them take the Material at the ground's surface (#79), so the
/// bricks over the ground changed with it, from level 4; those of levels 1 to 3 with ground
/// in them, the sea, the sky and the interior did not.
#[rustfmt::skip]
const COARSE_GOLDEN: &[(usize, [i32; 3], Kind, u64)] = &[
    // Temperate land, from 5 cm Voxels up to orbit: a brick is 1.6 km across at level 12, 13 km at
    // level 15, 410 km at level 20, and at level 24 a Voxel is 840 km, which is wider than the
    // Planet, so it is air.
    (2, [533733, -517152, 296413], Mixed, 0x472aa43cf21f35ab),
    (4, [533733, -517152, 296413], Mixed, 0xe9a3f3c992c66b1d),
    (6, [533733, -517152, 296413], Mixed, 0xb242c9011ace0e96),
    (9, [533733, -517152, 296413], Mixed, 0x1cf1cb2cde93b4d9),
    (12, [533733, -517152, 296413], Mixed, 0x29edbd2e9b8213b1),
    (15, [533733, -517152, 296413], Mixed, 0x1a98d219a6282471),
    (20, [533733, -517152, 296413], Mixed, 0xccdcc342ee803ddd),
    (24, [533733, -517152, 296413], Air, 0x6ab05ef9aa8b9b25),
    // The other biomes, the sky above the land, the ocean, a coast and a polar sea.
    (3, [-15725, 592256, 537708], Mixed, 0x4e715bf6ee9cf093),
    (7, [-15725, 592256, 537708], Mixed, 0xb591457469283fdb),
    (3, [579250, -525901, 167828], Mixed, 0x4cec4e0a0a2dc35c),
    (7, [579250, -525901, 167828], Mixed, 0xe05d879e7e113f39),
    (4, [504902, -458400, -418462], Mixed, 0x6e94aa2edac3e693),
    (3, [533735, -517154, 296414], Mixed, 0x231ce71f6114cdb7),
    (5, [463976, -459153, 459152], Mixed, 0xf4dad3ac5e4cae24),
    (10, [463976, -459153, 459152], Mixed, 0xba9198728649c91b),
    (5, [522061, -453161, 402602], Mixed, 0x396741cda01f1773),
    (2, [-49837, 602601, 523071], Mixed, 0xf51317df10f72146),
    (4, [-49837, 602601, 523071], Mixed, 0x6f7bb5f13e964ffe),
    (8, [-49837, 602601, 523071], Mixed, 0x7f590850aa0e3014),
    // Under the caves: a Voxel is a cave at the levels where a tube holds most of it, and not
    // from level 8, so these differ from what the ground makes at levels 1 to 4 and not at 8.
    (2, [509705, -475204, 393222], Mixed, 0x021d36e137dc65ee),
    (3, [509705, -475204, 393222], Mixed, 0x08cc4b694e40f223),
    (4, [509705, -475204, 393222], Mixed, 0x00fe3da5b839d270),
    (3, [509717, -475209, 393231], Mixed, 0xc08dc1d99875a171),
    (3, [650925, -319280, 338531], Mixed, 0xa86d5e5a0c20e966),
    (8, [509705, -475204, 393222], Mixed, 0xacbe0533239052bc),
    // The core's edge, the interior's, and the interior far from either.
    (2, [92376, 92376, 92376], Solid, 0xd97e511494bfdb25),
    (6, [92376, 92376, 92376], Mixed, 0x99eba5730a5ffae1),
    (11, [92376, 92376, 92376], Mixed, 0xd7e1d9ad7b8617ef),
    (6, [460334, 460334, 460334], Solid, 0xd97e511494bfdb25),
    (7, [300000, 300000, 300000], Solid, 0xd97e511494bfdb25),
    (5, [200000, 200000, 200000], Solid, 0xd97e511494bfdb25),
];

#[test]
fn golden_coarse_brick_hashes() {
    let mut report = String::new();
    let mut failed = false;
    for (level, under, want_kind, want_hash) in COARSE_GOLDEN {
        let coord = BrickCoord(under.map(|c| c >> level));
        let brick = planet().coarse_brick(*level, coord);
        let (got_kind, got_hash) = (kind(&brick), brick.content_hash());
        failed |= got_kind != *want_kind || got_hash != *want_hash;
        report += &format!(
            "({level}, {under:?}) {got_kind:?} actual {got_hash:#018x} want {want_hash:#018x}\n"
        );
    }
    assert!(!failed, "golden mismatch:\n{report}");
}
