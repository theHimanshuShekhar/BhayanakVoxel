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
    assert_eq!(GENERATOR_VERSION, 6);
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

const WORLD_MAP_HASH: u64 = 0xede22622e083246e;

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
    ([561945, -556104, -123944], Air, 0x6ab05ef9aa8b9b25),
    ([561928, -556087, -123940], Air, 0x6ab05ef9aa8b9b25),
    ([561927, -556086, -123939], Mixed, 0x3820a68bdb2d0b98),
    ([561924, -556083, -123939], Solid, 0xb3c594f58463b525),
    ([561917, -556076, -123937], Solid, 0x6fa64afc62031525),
    ([561901, -556060, -123934], Solid, 0xe0e4f56507851525),
    ([561866, -556026, -123926], Solid, 0x4d3463ca4f10d325),
    ([561831, -555991, -123918], Solid, 0xd97e511494bfdb25),
    // The other biomes: sand over sandstone, snow over ice over gravel over granite, basalt
    // and obsidian.
    ([465127, -460292, 460291], Air, 0x6ab05ef9aa8b9b25),
    ([465124, -460290, 460289], Mixed, 0xcede855b7676189f),
    ([465124, -460289, 460288], Mixed, 0x2296a661a21c0c5b),
    ([465121, -460287, 460286], Solid, 0x0dc0fd820e036725),
    ([465102, -460268, 460267], Solid, 0xe0e4f56507851525),
    ([-42509, 597474, 530484], Mixed, 0xd49fabaa6128e4cd),
    ([-42509, 597473, 530483], Solid, 0xd53a9d8b6953b525),
    ([-42508, 597470, 530480], Solid, 0xfeb0233604966725),
    ([-42508, 597462, 530474], Solid, 0x4a6835539d1e5b25),
    ([750916, 88868, -261321], Mixed, 0x24026bfc631ccee1),
    ([750915, 88868, -261321], Mixed, 0x1c319a37e57ab965),
    ([750911, 88868, -261319], Solid, 0xfc11e2d7244c1525),
    ([750902, 88867, -261316], Solid, 0x3725155588c5a725),
    // The ocean: water, the sea's surface and the sea floor, and a coast.
    ([477786, -472820, 433781], Air, 0x6ab05ef9aa8b9b25),
    ([477785, -472818, 433780], Mixed, 0x953d0eba638c3059),
    ([477784, -472818, 433779], Mixed, 0x186070a4213aa719),
    ([477781, -472815, 433777], Solid, 0xb812ab782d86a725),
    ([477654, -472689, 433661], Solid, 0xb812ab782d86a725),
    ([477651, -472686, 433658], Mixed, 0xf4ffb8168d9ef27d),
    ([477642, -472678, 433651], Mixed, 0x8e1e5cee9c627b59),
    ([579413, -549531, -47919], Mixed, 0xca14562806125509),
    ([579409, -549527, -47919], Solid, 0xb812ab782d86a725),
    ([579405, -549523, -47919], Mixed, 0x09007cb03f42ae25),
    ([579404, -549522, -47919], Mixed, 0x4ef8adcb3acea6a5),
    ([579377, -549497, -47917], Solid, 0xe0e4f56507851525),
    // A polar sea, from the water below to the air above: the water, the ice over it, and the
    // air over that.
    ([-76546, 720066, 340124], Air, 0x6ab05ef9aa8b9b25),
    ([-76543, 720044, 340113], Mixed, 0x457e0c3801f2d705),
    ([-76543, 720043, 340113], Mixed, 0xd22c45472bc84845),
    ([-76543, 720039, 340111], Solid, 0xb812ab782d86a725),
    ([-76465, 719308, 339766], Mixed, 0x6521bc940d41b845),
    ([-76465, 719307, 339765], Mixed, 0x88a829fac1cd2dbd),
    // Caves (#46), at a few of the mouths on this map and under it: the sky above a mouth, bricks
    // that are mostly cave, one wholly cave under the ground (air, where the strata would be), and
    // the strata below, which the caves did not change.
    ([-525047, -40094, -602822], Air, 0x6ab05ef9aa8b9b25),
    ([-525027, -40093, -602799], Mixed, 0xf072cfcfab6b67c2),
    ([-525024, -40092, -602795], Mixed, 0x02bb810749dea28f),
    ([-525017, -40092, -602788], Solid, 0x6fa64afc62031525),
    ([-525006, -40091, -602775], Solid, 0xe0e4f56507851525),
    ([-525520, -36417, -602551], Mixed, 0x2e6ffbc64f5aeac4),
    ([-525516, -36417, -602547], Mixed, 0x3cefdea05c39c52f),
    ([-525510, -36416, -602540], Solid, 0x6fa64afc62031525),
    // Chambers, which are wide enough to be seen from further away: bricks wholly cave.
    ([-525030, -40093, -602803], Air, 0x6ab05ef9aa8b9b25),
    ([-525029, -40093, -602801], Air, 0x6ab05ef9aa8b9b25),
    ([-525616, -36904, -602447], Air, 0x6ab05ef9aa8b9b25),
];

/// Boxes of Voxels from the centre, a few caves' cubes each, that [`CAVES_GOLDEN`] hashes the
/// tubes of: around the mouths and chambers of the bricks above.
const CAVES_BOXES: [([f64; 3], [f64; 3]); 2] = [
    (
        [-4_205_000.0, -325_000.0, -4_825_000.0],
        [-4_195_000.0, -315_000.0, -4_819_000.0],
    ),
    (
        [-4_212_000.0, -307_000.0, -4_820_000.0],
        [-4_202_000.0, -297_000.0, -4_814_000.0],
    ),
];

/// The hash of the tubes of the caves that the walk makes in each of the boxes above: the tubes
/// themselves, so that a change to the walk, or to the floats under it, shows here before it
/// shows in the bricks.
const CAVES_GOLDEN: [u64; 2] = [0xe4f7f97ece129d52, 0x9142e30d27d0ba6d];

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
const REGION_GROUND: [f64; 3] = [4495423.0, -4448687.0, -991514.0];

/// The hash of the store that `region_around` makes there.
const REGION_HASH: u64 = 0x0f28c8881c179c12;

fn planet() -> &'static Planet {
    static PLANET: OnceLock<Planet> = OnceLock::new();
    PLANET.get_or_init(|| Planet::new(WorldMap::generate(SEED, WORLD_MAP_SIZE)))
}

/// The Spawn of this seed, to the bit (`f64::to_bits`): the same on every machine.
const SPAWN_BITS: [u64; 3] = [
    4703913670304689464,
    4705621307742087906,
    13929261612330084643,
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
    (2, [561927, -556086, -123939], Mixed, 0x0f378879a006f5c6),
    (4, [561927, -556086, -123939], Mixed, 0x5a28a0091bad5de5),
    (6, [561927, -556086, -123939], Mixed, 0xf2aef99fe28e91fd),
    (9, [561927, -556086, -123939], Mixed, 0xfe60650dc66833de),
    (12, [561927, -556086, -123939], Mixed, 0x7259d47049be4e17),
    (15, [561927, -556086, -123939], Mixed, 0xe8a7b3a620a80f1f),
    (20, [561927, -556086, -123939], Mixed, 0x9774d318bb5f099b),
    (24, [561927, -556086, -123939], Air, 0x6ab05ef9aa8b9b25),
    // The other biomes, the sky above the land, the ocean, a coast and a polar sea.
    (3, [-42509, 597474, 530484], Mixed, 0xea41b4d1fd30541c),
    (7, [-42509, 597474, 530484], Mixed, 0xd2f302f9e001b9e5),
    (3, [750916, 88868, -261321], Mixed, 0xe829656474495aa0),
    (7, [750916, 88868, -261321], Mixed, 0x304345c720bc18ef),
    (4, [465124, -460290, 460289], Mixed, 0x1e7f5d704794c8a7),
    (3, [561928, -556087, -123940], Mixed, 0x071785966fafcb01),
    (5, [477785, -472818, 433780], Mixed, 0x8e2fc5bdf13d90ed),
    (10, [477785, -472818, 433780], Mixed, 0xa22e1b1d935144e3),
    (5, [579413, -549531, -47919], Mixed, 0x09dff683a48d9f53),
    (2, [-76543, 720044, 340113], Mixed, 0xd2842f1c8ce88c59),
    (4, [-76543, 720044, 340113], Mixed, 0x73f6800a14c645a1),
    (8, [-76543, 720044, 340113], Mixed, 0xb28b4226ba8e5c69),
    // Under the caves: a Voxel is a cave at the levels where a tube holds most of it, and not
    // from level 8, so these differ from what the ground makes at levels 1 to 4 and not at 8.
    (2, [-525027, -40093, -602799], Mixed, 0x3ac6e2afd67a39a2),
    (3, [-525027, -40093, -602799], Mixed, 0x29557f09a5031cf3),
    (4, [-525027, -40093, -602799], Mixed, 0x12bdeae2412203f6),
    (3, [-525024, -40092, -602795], Mixed, 0x2d04929c47734155),
    (3, [-525520, -36417, -602551], Mixed, 0x1b8d337d68897977),
    (8, [-525027, -40093, -602799], Mixed, 0x5b5c8f7e71601a08),
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
