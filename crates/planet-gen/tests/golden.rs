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
    assert_eq!(GENERATOR_VERSION, 2);
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
];

/// Voxels around the ground that the region golden covers, and the ground: the temperate land
/// above, at the corner of its brick (which is within a brick of the ground).
const REGION_REACH: i32 = 40;
const REGION_GROUND: [f64; 3] = [95680.0, -94696.0, 90744.0];

/// The hash of the store that `region_around` makes there.
const REGION_HASH: u64 = 0xa401d7c4a0e008ac;

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
