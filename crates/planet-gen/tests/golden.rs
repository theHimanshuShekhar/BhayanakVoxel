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
    assert_eq!(GENERATOR_VERSION, 3);
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

const WORLD_MAP_HASH: u64 = 0xcd2f00519adae333;

#[test]
fn golden_world_map_hash() {
    let actual = WorldMap::generate(SEED, WORLD_MAP_SIZE).content_hash();
    assert_eq!(actual, WORLD_MAP_HASH, "actual {actual:#018x}");
}

/// Bricks of the real Planet on the coarse world map above, with a sample of every layer of it.
#[rustfmt::skip]
const PLANET_GOLDEN: &[([i32; 3], Kind, u64)] = &[
    // The core, its edge, deep rock, and the interior's outer edge.
    ([0, 0, 0], Solid, 0x02c5150a75e71525),
    ([4000, 0, 0], Mixed, 0x9c31dfda9ca31e8a),
    ([4001, 0, 0], Solid, 0xd97e511494bfdb25),
    ([0, 18690, 0], Solid, 0xd97e511494bfdb25),
    // Temperate land, from the sky down: air, the ground, the strata and the boundaries between
    // them, and deep rock.
    ([11962, -11838, 11345], Air, 0x6ab05ef9aa8b9b25),
    ([11960, -11837, 11343], Mixed, 0x4971180b20088cda),
    ([11960, -11836, 11343], Mixed, 0xc0730cf0330ecb13),
    ([11959, -11835, 11342], Solid, 0xb3c594f58463b525),
    ([11954, -11831, 11338], Solid, 0x6fa64afc62031525),
    ([11945, -11822, 11329], Solid, 0xe0e4f56507851525),
    ([11931, -11807, 11315], Solid, 0x4d3463ca4f10d325),
    ([11925, -11802, 11310], Mixed, 0x9f2381fb981eb919),
    ([11916, -11793, 11301], Solid, 0xd97e511494bfdb25),
    // The other biomes: sand over sandstone, snow over ice over gravel over granite, basalt
    // and obsidian.
    ([12929, -10969, 10716], Mixed, 0x64bc921269bfa366),
    ([12926, -10966, 10713], Solid, 0x0dc0fd820e036725),
    ([13730, -11919, -9818], Mixed, 0x3ab28e1b23bcd22c),
    ([13728, -11917, -9817], Solid, 0xfeb0233604966725),
    ([13723, -11913, -9813], Solid, 0x4a6835539d1e5b25),
    ([13697, -11890, -9794], Solid, 0x40312cd046acb525),
    ([11872, -11750, 11749], Mixed, 0xe261774ab58c3c6d),
    ([11871, -11748, 11747], Solid, 0xfc11e2d7244c1525),
    ([11866, -11744, 11743], Solid, 0x3725155588c5a725),
    // The ocean: water, the sea's surface, the sea floor and its layers, and a coast.
    ([13825, -13682, 2324], Solid, 0xb812ab782d86a725),
    ([14115, -13969, 2373], Mixed, 0x35adbfe6fd3bd695),
    ([13823, -13680, 2324], Mixed, 0x033c20973f0f1c37),
    ([13821, -13678, 2324], Solid, 0x6fa64afc62031525),
    ([13788, -13645, 2318], Solid, 0x4d3463ca4f10d325),
    ([14194, -14048, 688], Mixed, 0xc8ea6e05bb0f9600),
    // A polar sea, from the water below to the air above: the water, the ice and the water under
    // it, and the ice and the air over it.
    ([-12158, 15764, 1865], Solid, 0xb812ab782d86a725),
    ([-12160, 15767, 1866], Mixed, 0x2d7892c58a49c155),
    ([-12160, 15768, 1866], Mixed, 0x0335af5226415465),
    ([-12161, 15768, 1866], Mixed, 0x9e3e2b2fcc1fa1f1),
    ([-12161, 15769, 1866], Air, 0x6ab05ef9aa8b9b25),
    // Caves (#46), at a few of the mouths on this map and under it: the sky above a mouth, the
    // brick that the mouth is in, bricks that are mostly cave, one wholly cave under the ground
    // (air, where the strata would be), and the strata below, which the caves did not change.
    ([402, -4416, -20663], Air, 0x6ab05ef9aa8b9b25),
    ([401, -4414, -20657], Mixed, 0x3aeedc798aea8363),
    ([401, -4414, -20654], Mixed, 0x10b6fce6722643f3),
    ([401, -4413, -20649], Solid, 0x6fa64afc62031525),
    ([-1824, -1990, -20740], Air, 0x6ab05ef9aa8b9b25),
    ([-1824, -1989, -20738], Mixed, 0x2dcefb50a15b5f19),
    ([-2238, -1561, -20820], Mixed, 0x6279f76739769dc1),
    ([-2237, -1560, -20813], Solid, 0x0dc0fd820e036725),
    // Chambers, which are wide enough to be seen from further away: the brick at the end of a
    // tube, and at the end of another, both wholly cave.
    ([-5433, -1881, -19691], Air, 0x6ab05ef9aa8b9b25),
    ([-5482, -818, -19760], Air, 0x6ab05ef9aa8b9b25),
];

/// The hash of the tubes of the caves that the walk makes on this map: the table itself, so that
/// a change to the walk, or to the floats under it, shows here before it shows in the bricks.
const CAVES_HASH: u64 = 0x83c77f1a027f7924;

#[test]
fn golden_caves_hash() {
    let actual = planet().caves_hash();
    assert_eq!(actual, CAVES_HASH, "actual {actual:#018x}");
}

/// Voxels around the ground that the region golden covers, and the ground: the temperate land
/// above, at the corner of its brick (which is within a brick of the ground).
const REGION_REACH: i32 = 40;
const REGION_GROUND: [f64; 3] = [95680.0, -94696.0, 90744.0];

/// The hash of the store that `region_around` makes there.
const REGION_HASH: u64 = 0x84df86a009cab10e;

fn planet() -> &'static Planet {
    static PLANET: OnceLock<Planet> = OnceLock::new();
    PLANET.get_or_init(|| Planet::new(WorldMap::generate(SEED, WORLD_MAP_SIZE)))
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
/// bricks over the ground changed with it, from level 3; those of levels 1 and 2 with ground
/// in them, the sea, the sky and the interior did not.
#[rustfmt::skip]
const COARSE_GOLDEN: &[(usize, [i32; 3], Kind, u64)] = &[
    // Temperate land, from 20 cm Voxels up to orbit: a brick is 1.6 km across at level 11, 13 km at
    // level 14, and at level 19 a Voxel is 52 km, which is wider than the Planet, so it is air.
    (1, [11960, -11837, 11343], Mixed, 0x8565e8f966d40f49),
    (3, [11960, -11837, 11343], Mixed, 0x3788f7b911e25b6d),
    (5, [11960, -11837, 11343], Mixed, 0x1e2eb33cedbfe41c),
    (8, [11960, -11837, 11343], Mixed, 0x59fdfa374196f730),
    (11, [11960, -11837, 11343], Mixed, 0x0cb55abb80f2d189),
    (14, [11960, -11837, 11343], Mixed, 0x36301fd372a2ec18),
    (19, [11960, -11837, 11343], Air, 0x6ab05ef9aa8b9b25),
    // The other biomes, the sky above the land, the ocean, a coast and a polar sea.
    (2, [12929, -10969, 10716], Mixed, 0x07787a6cc86d9a29),
    (6, [12929, -10969, 10716], Mixed, 0xb1ff2acf10b58d64),
    (2, [13730, -11919, -9818], Mixed, 0x41ef64706b0316db),
    (6, [13730, -11919, -9818], Mixed, 0x1b18ebde98244c62),
    (3, [11872, -11750, 11749], Mixed, 0x3d13405f56f7e5fd),
    (2, [11962, -11838, 11345], Mixed, 0x6a00d8d9d876da05),
    (4, [14115, -13969, 2373], Mixed, 0xce83b6cef716c779),
    (9, [14115, -13969, 2373], Mixed, 0xa89a1db55bc431f5),
    (4, [14194, -14048, 688], Mixed, 0x5886e977298e2ca8),
    (1, [-12160, 15767, 1866], Mixed, 0x9e55f936177ff0d9),
    (3, [-12160, 15767, 1866], Solid, 0xb812ab782d86a725),
    (7, [-12160, 15767, 1866], Mixed, 0x702836b6574c4521),
    // Under the caves: a Voxel is a cave at the levels where a tube holds most of it, and not
    // from level 7, so these differ from what the ground makes at levels 1 to 3 and not at 7.
    (1, [-5433, -1881, -19691], Air, 0x6ab05ef9aa8b9b25),
    (2, [-5433, -1881, -19691], Mixed, 0xc71222b94a24693f),
    (3, [-5433, -1881, -19691], Mixed, 0x122d41b723f43201),
    (2, [-5482, -818, -19760], Mixed, 0xe7fe83c79afea461),
    (2, [401, -4414, -20657], Mixed, 0x354d26b721ad07d3),
    (7, [-5433, -1881, -19691], Solid, 0xd97e511494bfdb25),
    // The core's edge, the interior's, and the interior far from either.
    (1, [2309, 2309, 2309], Mixed, 0x237d2d4cb454ecf6),
    (5, [2309, 2309, 2309], Mixed, 0xe2c5cca2e703b85f),
    (10, [2309, 2309, 2309], Mixed, 0xee14a7ee217ce91c),
    (6, [10790, 10790, 10790], Solid, 0xd97e511494bfdb25),
    (4, [9000, 9000, 9000], Solid, 0xd97e511494bfdb25),
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
