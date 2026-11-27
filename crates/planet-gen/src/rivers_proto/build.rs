//! PROTOTYPE (#206): the map-build pass of both methods. See the module above.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use super::*;
use crate::world_map::{proto_index, proto_resolve, proto_split};

/// The epsilon of the fill, metres, that gives a filled flat a slope towards its outlet.
const EPS: f32 = 0.002;
/// A basin is a lake from this depth, metres, and this many texels.
const LAKE_DEPTH: f32 = 1.0;
const LAKE_TEXELS: usize = 6;

const DIST: [f32; 8] = [1.0, 1.0, 1.0, 1.0, 1.414_213_5, 1.414_213_5, 1.414_213_5, 1.414_213_5];

fn neighbour(size: u32, idx: usize, k: usize) -> usize {
    let n = size as usize;
    let (face, rest) = (idx / (n * n), idx % (n * n));
    let (i, j) = ((rest % n) as i32, (rest / n) as i32);
    let (ni, nj) = (i + OFFS[k].0, j + OFFS[k].1);
    if ni >= 0 && nj >= 0 && (ni as usize) < n && (nj as usize) < n {
        return face * n * n + nj as usize * n + ni as usize;
    }
    let (f, a, b) = proto_resolve(size, face, ni, nj);
    proto_index(size, f, a as u32, b as u32)
}

fn key(f: f32) -> u32 {
    ((f64::from(f) + 6000.0) * 100_000.0) as u32
}

/// The flow code of an area in texels: 12 steps to each doubling.
pub(super) fn flow_code(area: f64) -> u8 {
    if area < 1.0 {
        return 0;
    }
    const STEP: [f64; 12] = [
        1.0, 1.059_463, 1.122_462, 1.189_207, 1.259_921, 1.334_840, 1.414_214, 1.498_307,
        1.587_401, 1.681_793, 1.781_797, 1.887_749,
    ];
    let bits = area.to_bits();
    let whole = ((bits >> 52) & 0x7ff) as i32 - 1023;
    let mantissa = f64::from_bits((bits & 0x000f_ffff_ffff_ffff) | 0x3ff0_0000_0000_0000);
    let mut k = 0;
    while k < 11 && mantissa >= STEP[k + 1] {
        k += 1;
    }
    (whole * FLOW_STEPS as i32 + k as i32).clamp(0, 255) as u8
}

fn hash16(idx: usize, k: usize) -> f32 {
    let mut z = (idx as u64).wrapping_mul(8).wrapping_add(k as u64).wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    (z & 0xffff) as f32 / 65_535.0
}

/// Builds the rivers of `mode` for the map of `size` texels a face whose heights in metres
/// (negative under the sea) are `metres`.
pub fn build(mode: RiverMode, metres: &[f32], size: u32, threads: usize) -> RiverData {
    let n = size as usize;
    let count = 6 * n * n;
    assert_eq!(metres.len(), count);
    let ocean: Vec<bool> = metres.iter().map(|&h| h < 0.0).collect();
    let mut fe = metres.to_vec();
    let mut level = metres.to_vec();
    let mut closed = ocean.clone();
    let mut heap: BinaryHeap<Reverse<(u32, u32)>> = BinaryHeap::new();
    for k in 0..count {
        if ocean[k] && (0..8).any(|m| !ocean[neighbour(size, k, m)]) {
            heap.push(Reverse((key(fe[k]), k as u32)));
        }
    }
    let mut order: Vec<u32> = Vec::with_capacity(count / 2);
    while let Some(Reverse((_, k))) = heap.pop() {
        let k = k as usize;
        if !ocean[k] {
            order.push(k as u32);
        }
        for m in 0..8 {
            let nb = neighbour(size, k, m);
            if closed[nb] {
                continue;
            }
            closed[nb] = true;
            fe[nb] = fe[nb].max(fe[k] + EPS);
            level[nb] = level[nb].max(level[k]);
            heap.push(Reverse((key(fe[nb]), nb as u32)));
        }
    }
    drop(heap);
    drop(closed);

    // Receivers, and the area through each texel.
    let mut recv = vec![NO_RECEIVER; count];
    let mut dir = if mode == RiverMode::Mfd { vec![[0i8; 2]; count] } else { Vec::new() };
    let mut area_d8 = vec![1u32; count];
    let mut area_mfd = vec![1f32; count];
    for &k in order.iter().rev() {
        let k = k as usize;
        // The steepest, with jittered lengths, for the receiver; plain slopes to the 4th for MFD.
        let mut best = (usize::MAX, 0.0f32);
        let mut best_plain = (usize::MAX, 0.0f32);
        let mut total = 0.0f32;
        let mut weights = [0.0f32; 8];
        let mut nbs = [0usize; 8];
        for m in 0..8 {
            let nb = neighbour(size, k, m);
            nbs[m] = nb;
            if fe[nb] < fe[k] {
                let rise = fe[k] - fe[nb];
                let jit = 1.0 + 0.5 * (hash16(k, m) - 0.5);
                let s = rise / (DIST[m] * jit);
                if s > best.1 {
                    best = (m, s);
                }
                let plain = rise / DIST[m];
                if plain > best_plain.1 {
                    best_plain = (m, plain);
                }
                let w = plain * plain;
                weights[m] = w * w;
                total += weights[m];
            }
        }
        match mode {
            RiverMode::D8 => {
                if best.0 != usize::MAX {
                    recv[k] = best.0 as u8;
                    let r = nbs[best.0];
                    if !ocean[r] {
                        area_d8[r] += area_d8[k];
                    }
                }
            }
            RiverMode::Mfd => {
                if best_plain.0 != usize::MAX {
                    recv[k] = best_plain.0 as u8;
                    let (mut vx, mut vy) = (0.0f32, 0.0f32);
                    for m in 0..8 {
                        if weights[m] > 0.0 {
                            let share = weights[m] / total;
                            vx += share * OFFS[m].0 as f32;
                            vy += share * OFFS[m].1 as f32;
                            let r = nbs[m];
                            if !ocean[r] {
                                area_mfd[r] += area_mfd[k] * share;
                            }
                        }
                    }
                    let len = (vx * vx + vy * vy).sqrt();
                    if len > 1e-3 {
                        dir[k] = [(vx / len * 127.0).round() as i8, (vy / len * 127.0).round() as i8];
                    }
                }
            }
            RiverMode::Off => {}
        }
    }

    let mut flow = vec![0u8; count];
    for k in 0..count {
        if ocean[k] {
            continue;
        }
        flow[k] = flow_code(match mode {
            RiverMode::Mfd => f64::from(area_mfd[k]),
            _ => f64::from(area_d8[k]),
        });
    }
    drop(area_d8);
    drop(area_mfd);
    let thr = flow_code(RIVER_MIN_AREA);

    // Lakes: basins deeper than LAKE_DEPTH, in groups of at least LAKE_TEXELS.
    let mut is_lake = vec![false; count];
    let mut seen = vec![false; count];
    let (mut lake_count, mut lake_texels, mut biggest_lake) = (0usize, 0usize, 0usize);
    for s in 0..count {
        if seen[s] || ocean[s] || level[s] - metres[s] < LAKE_DEPTH {
            continue;
        }
        let mut group = vec![s];
        seen[s] = true;
        let mut at = 0;
        while at < group.len() {
            let k = group[at];
            at += 1;
            for m in 0..8 {
                let nb = neighbour(size, k, m);
                if !seen[nb] && !ocean[nb] && level[nb] - metres[nb] >= LAKE_DEPTH {
                    seen[nb] = true;
                    group.push(nb);
                }
            }
        }
        // A lake is a blob, not a chain of pits down a valley floor: at least 5 of its texels have
        // all four neighbours in it too.
        let inside = |k: usize| (0..4).all(|m| group.binary_search(&neighbour(size, k, m)).is_ok());
        let mut sorted = group.clone();
        sorted.sort_unstable();
        let interior = {
            let group = &sorted;
            group
                .iter()
                .filter(|&&k| (0..4).all(|m| group.binary_search(&neighbour(size, k, m)).is_ok()))
                .count()
        };
        let _ = inside;
        if group.len() >= LAKE_TEXELS && interior >= 5 {
            lake_count += 1;
            lake_texels += group.len();
            biggest_lake = biggest_lake.max(group.len());
            for &k in &group {
                is_lake[k] = true;
            }
        }
    }
    drop(seen);

    // Water surfaces: a river's is a little under its texel's ground and never above an
    // upstream node's; a lake's is its spill height.
    let mut water = vec![NO_WATER; count];
    let mut water_m = vec![f32::INFINITY; count];
    for k in 0..count {
        if ocean[k] {
            continue;
        }
        if is_lake[k] {
            water_m[k] = level[k];
            recv[k] |= LAKE;
        } else if flow[k] >= thr {
            let hw = half_width(flow[k]);
            water_m[k] = metres[k] - drop_of(hw) as f32;
        }
    }
    let mut has_donor = vec![false; count];
    for &k in order.iter().rev() {
        let k = k as usize;
        if water_m[k] == f32::INFINITY || recv[k] & NO_RECEIVER != 0 {
            continue;
        }
        let r = neighbour(size, k, (recv[k] & 7) as usize);
        if water_m[r] == f32::INFINITY || ocean[r] {
            continue;
        }
        if !is_lake[r] {
            water_m[r] = water_m[r].min(water_m[k]);
            has_donor[r] = true;
        }
    }
    let (mut river_texels, mut heads, mut max_flow) = (0usize, 0usize, 0u8);
    for k in 0..count {
        if water_m[k] == f32::INFINITY {
            continue;
        }
        let w = if is_lake[k] { water_m[k] } else { water_m[k].max(0.0) };
        water[k] = (w / WATER_UNIT as f32).round().clamp(-32_000.0, 32_000.0) as i16;
        if !is_lake[k] {
            river_texels += 1;
            max_flow = max_flow.max(flow[k]);
            if !has_donor[k] {
                recv[k] |= HEAD;
                heads += 1;
            }
        }
    }
    eprintln!(
        "rivers {mode:?}: {river_texels} river texels ({heads} heads, threshold {} texels), flow code up to {max_flow}; \
         {lake_count} lakes, {lake_texels} lake texels, the biggest {biggest_lake}",
        RIVER_MIN_AREA as u32
    );
    let _ = (threads, proto_split);
    RiverData {
        mode,
        recv,
        flow,
        dir,
        water,
        thr,
    }
}
