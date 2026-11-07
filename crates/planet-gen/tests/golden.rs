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
    assert_eq!(GENERATOR_VERSION, 4);
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
    ([8000, 0, 0], Mixed, 0x9c31dfda9ca31e8a),
    ([8002, 0, 0], Solid, 0xd97e511494bfdb25),
    ([0, 37380, 0], Solid, 0xd97e511494bfdb25),
    // Temperate land, from the sky down: air, the ground, the strata and the boundaries between
    // them, and deep rock.
    ([23924, -23676, 22690], Air, 0x6ab05ef9aa8b9b25),
    ([23920, -23674, 22686], Mixed, 0xa6bd8a214eafa0e9),
    ([23920, -23672, 22686], Mixed, 0xc94a1d256b3090f3),
    ([23918, -23670, 22684], Solid, 0xb3c594f58463b525),
    ([23908, -23662, 22676], Solid, 0x6fa64afc62031525),
    ([23890, -23644, 22658], Solid, 0xe0e4f56507851525),
    ([23862, -23614, 22630], Solid, 0x4d3463ca4f10d325),
    ([23850, -23604, 22620], Mixed, 0xcbc917f5ecbd4ccd),
    ([23832, -23586, 22602], Solid, 0xd97e511494bfdb25),
    // The other biomes: sand over sandstone, snow over ice over gravel over granite, basalt
    // and obsidian.
    ([25858, -21938, 21432], Mixed, 0x2ddb1d3b984ae27b),
    ([25852, -21932, 21426], Solid, 0x0dc0fd820e036725),
    ([27460, -23837, -19636], Mixed, 0x2fe666e7950d0492),
    ([27456, -23834, -19634], Solid, 0xfeb0233604966725),
    ([27446, -23826, -19626], Solid, 0x4a6835539d1e5b25),
    ([27394, -23780, -19588], Solid, 0x40312cd046acb525),
    ([23744, -23500, 23498], Mixed, 0xf9e0c112940ee2dd),
    ([23742, -23496, 23494], Solid, 0xfc11e2d7244c1525),
    ([23732, -23488, 23486], Solid, 0x3725155588c5a725),
    // The ocean: water, the sea's surface, the sea floor and its layers, and a coast.
    ([27650, -27364, 4648], Solid, 0xb812ab782d86a725),
    ([28230, -27938, 4746], Mixed, 0x548df38cfb57afc9),
    ([27646, -27360, 4648], Mixed, 0x73a80fb2cb7dddc3),
    ([27642, -27356, 4648], Solid, 0x6fa64afc62031525),
    ([27576, -27290, 4636], Solid, 0x4d3463ca4f10d325),
    ([28388, -28096, 1376], Mixed, 0xfa0ff2af7a930aa3),
    // A polar sea, from the water below to the air above: the water, the ice and the water under
    // it, and the ice and the air over it.
    ([-24316, 31528, 3730], Solid, 0xb812ab782d86a725),
    ([-24320, 31535, 3732], Mixed, 0x8b1693591b57bf59),
    ([-24320, 31536, 3732], Mixed, 0xdcabd3ed56e92a39),
    ([-24322, 31536, 3732], Mixed, 0x2262629570ceb499),
    ([-24322, 31538, 3732], Air, 0x6ab05ef9aa8b9b25),
    // Caves (#46), at a few of the mouths on this map and under it: the sky above a mouth, the
    // brick that the mouth is in, bricks that are mostly cave, one wholly cave under the ground
    // (air, where the strata would be), and the strata below, which the caves did not change.
    ([804, -8832, -41326], Air, 0x6ab05ef9aa8b9b25),
    ([802, -8828, -41313], Mixed, 0x2ad7abaf510facd3),
    ([802, -8828, -41308], Mixed, 0x6a23f321f5e34641),
    ([802, -8826, -41298], Solid, 0x6fa64afc62031525),
    ([-3648, -3980, -41480], Air, 0x6ab05ef9aa8b9b25),
    ([-3648, -3978, -41476], Mixed, 0x1d55487e6a069a0d),
    ([-4476, -3122, -41640], Mixed, 0x49a2a5120efcce75),
    ([-4474, -3120, -41626], Solid, 0x0dc0fd820e036725),
    // Chambers, which are wide enough to be seen from further away: the brick at the end of a
    // tube, and at the end of another, both wholly cave.
    ([-10866, -3762, -39382], Air, 0x6ab05ef9aa8b9b25),
    ([-10964, -1636, -39520], Air, 0x6ab05ef9aa8b9b25),
];

/// The hash of the tubes of the caves that the walk makes on this map: the table itself, so that
/// a change to the walk, or to the floats under it, shows here before it shows in the bricks.
const CAVES_HASH: u64 = 0xd9e77605f137ec18;

#[test]
fn golden_caves_hash() {
    let actual = planet().caves_hash();
    assert_eq!(actual, CAVES_HASH, "actual {actual:#018x}");
}

/// Voxels around the ground that the region golden covers, and the ground: the temperate land
/// above, at the corner of its brick (which is within a brick of the ground).
const REGION_REACH: i32 = 80;
const REGION_GROUND: [f64; 3] = [191360.0, -189392.0, 181488.0];

/// The hash of the store that `region_around` makes there.
const REGION_HASH: u64 = 0x49cda98d13a643b4;

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
/// bricks over the ground changed with it, from level 4; those of levels 1 to 3 with ground
/// in them, the sea, the sky and the interior did not.
#[rustfmt::skip]
const COARSE_GOLDEN: &[(usize, [i32; 3], Kind, u64)] = &[
    // Temperate land, from 10 cm Voxels up to orbit: a brick is 1.6 km across at level 12, 13 km at
    // level 15, and at level 20 a Voxel is 52 km, which is wider than the Planet, so it is air.
    (2, [23920, -23674, 22686], Mixed, 0xfa9cbec5e56075d4),
    (4, [23920, -23674, 22686], Mixed, 0x3788f7b911e25b6d),
    (6, [23920, -23674, 22686], Mixed, 0xf6c851598a6c1446),
    (9, [23920, -23674, 22686], Mixed, 0x59fdfa374196f730),
    (12, [23920, -23674, 22686], Mixed, 0x0cb55abb80f2d189),
    (15, [23920, -23674, 22686], Mixed, 0x36301fd372a2ec18),
    (20, [23920, -23674, 22686], Air, 0x6ab05ef9aa8b9b25),
    // The other biomes, the sky above the land, the ocean, a coast and a polar sea.
    (3, [25858, -21938, 21432], Mixed, 0x07787a6cc86d9a29),
    (7, [25858, -21938, 21432], Mixed, 0xb1ff2acf10b58d64),
    (3, [27460, -23838, -19636], Mixed, 0x6cbb319bc51480fb),
    (7, [27460, -23838, -19636], Mixed, 0x1b18ebde98244c62),
    (4, [23744, -23500, 23498], Mixed, 0x24310e2636cd844b),
    (3, [23924, -23676, 22690], Mixed, 0xd2c3afe20c4d1595),
    (5, [28230, -27938, 4746], Mixed, 0xce83b6cef716c779),
    (10, [28230, -27938, 4746], Mixed, 0xa89a1db55bc431f5),
    (5, [28388, -28096, 1376], Mixed, 0xee30ab707837aefe),
    (2, [-24320, 31534, 3732], Mixed, 0x00fd7221bbbca7c5),
    (4, [-24320, 31534, 3732], Solid, 0xb812ab782d86a725),
    (8, [-24320, 31534, 3732], Mixed, 0x702836b6574c4521),
    // Under the caves: a Voxel is a cave at the levels where a tube holds most of it, and not
    // from level 8, so these differ from what the ground makes at levels 1 to 4 and not at 8.
    (2, [-10866, -3762, -39382], Air, 0x6ab05ef9aa8b9b25),
    (3, [-10866, -3762, -39382], Mixed, 0xc71222b94a24693f),
    (4, [-10866, -3762, -39382], Mixed, 0x8eb86d9caeca02b7),
    (3, [-10964, -1636, -39520], Mixed, 0xe7fe83c79afea461),
    (3, [802, -8828, -41314], Mixed, 0xda23bcebf1a92ad1),
    (8, [-10866, -3762, -39382], Solid, 0xd97e511494bfdb25),
    // The core's edge, the interior's, and the interior far from either.
    (2, [4618, 4618, 4618], Mixed, 0x471f08b899b435a9),
    (6, [4618, 4618, 4618], Mixed, 0xe2c5cca2e703b85f),
    (11, [4618, 4618, 4618], Mixed, 0xee14a7ee217ce91c),
    (7, [21580, 21580, 21580], Solid, 0xd97e511494bfdb25),
    (5, [18000, 18000, 18000], Solid, 0xd97e511494bfdb25),
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
