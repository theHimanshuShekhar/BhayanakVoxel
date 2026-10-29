//! Golden hashes of the test Planet after a fixed run of Edits. CI runs this on Linux and
//! Windows, debug and release, and every runner checks against the values below, so all runners
//! must agree with each other (ADR 0003).
//!
//! If `apply` or the generator changes on purpose, replace the hashes with the `actual` column
//! printed by a failing run.

use brick_store::{BrickCoord, BrickStore};
use net::EditOp;
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

fn run(mut store: BrickStore, script: Script) -> Vec<u64> {
    script
        .iter()
        .map(|&(centre, radius)| {
            let op = EditOp::dig_sphere(brick_store::VoxelCoord(centre), radius);
            sim::apply(&mut store, &op);
            store.content_hash()
        })
        .collect()
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
    check("surface", &run(store, SURFACE), &SURFACE_GOLDEN);
}

#[test]
fn golden_edits_in_the_core() {
    check("core", &run(BrickStore::new(), CORE), &CORE_GOLDEN);
}
