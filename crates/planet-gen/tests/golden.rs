//! Golden hashes of generated bricks. CI runs this on Linux and Windows, debug and release,
//! and every runner checks against the values below, so all runners must agree with each other.
//!
//! If generation changes on purpose, bump `GENERATOR_VERSION`, then replace the hashes with the
//! `actual` column printed by a failing run.

use brick_store::{Brick, BrickCoord};
use planet_gen::world_map::WorldMap;
use planet_gen::{GENERATOR_VERSION, generate_brick};

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
    assert_eq!(GENERATOR_VERSION, 1);
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
