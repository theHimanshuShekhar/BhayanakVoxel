//! FastScape-like stream-power erosion on the cube-sphere graph.
//! dh/dt = U - K A^0.5 S  (n = 1, m = 1/2), solved implicitly in receiver order.
//! Pits and basins are handled by a priority flood (Barnes epsilon) every step; the lakes
//! it finds are reported, and the drainage is routed through them to the sea.

use super::grid::*;
use super::noise::hash3;
use std::cmp::Reverse;
use std::collections::BinaryHeap;

pub struct Eroded {
    pub h: Vec<f32>,
    pub area: Vec<f32>, // drainage area, km^2 (land only)
    pub lake: Vec<bool>,
    pub rec: Vec<u32>,
}

struct Flow {
    order: Vec<u32>, // land nodes, receivers before donors
    rec: Vec<u32>,
    fill: Vec<f32>,
    area: Vec<f32>,
}

fn flow(g: &Grid, h: &[f32], ocean: &[bool]) -> Flow {
    let n = g.total();
    let mut fill = h.to_vec();
    let mut closed = ocean.to_vec();
    let mut heap: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::new();
    let key = |f: f32| ((f64::from(f) + 6000.0) * 1000.0) as u64;
    for k in 0..n {
        if ocean[k] && g.nbr[k].iter().any(|&m| !ocean[m as usize]) {
            heap.push(Reverse((key(fill[k]), k as u32)));
        }
    }
    let mut order: Vec<u32> = Vec::with_capacity(n);
    while let Some(Reverse((_, k))) = heap.pop() {
        let k = k as usize;
        if !ocean[k] {
            order.push(k as u32);
        }
        for &m in &g.nbr[k] {
            let m = m as usize;
            if closed[m] {
                continue;
            }
            closed[m] = true;
            fill[m] = fill[m].max(fill[k] + 0.02);
            heap.push(Reverse((key(fill[m]), m as u32)));
        }
    }
    let mut rec = vec![u32::MAX; n];
    for &k in &order {
        let k = k as usize;
        let mut best = (u32::MAX, 0.0f32);
        for (m, &nb) in g.nbr[k].iter().enumerate() {
            let nb = nb as usize;
            if fill[nb] < fill[k] {
                // Jittered lengths break up the grid's preferred directions.
                let jit = 1.0
                    + 0.5
                        * (f32::from((hash3(k as i32, m as i32, 3, 77) & 0xFFFF) as u16) / 65535.0
                            - 0.5);
                let s = (fill[k] - fill[nb]) / (g.nbr_d[k][m] * jit);
                if s > best.1 {
                    best = (nb as u32, s);
                }
            }
        }
        rec[k] = best.0;
    }
    let mut area = vec![0.0f32; n];
    for &k in &order {
        area[k as usize] = g.area[k as usize];
    }
    for &k in order.iter().rev() {
        let r = rec[k as usize];
        if r != u32::MAX && !ocean[r as usize] {
            area[r as usize] += area[k as usize];
        }
    }
    Flow {
        order,
        rec,
        fill,
        area,
    }
}

pub struct Params {
    pub steps: usize,
    pub k: f32,    // erodibility per step (per sqrt(km^2) per km)
    pub u: f32,    // uplift per step at uplift = 1, metres
    pub smax: f32, // slope limit, m per km
}

pub fn erode(g: &Grid, h0: &[f32], uplift: &[f32], p: &Params) -> Eroded {
    let n = g.total();
    let ocean: Vec<bool> = h0.iter().map(|&h| h <= 0.0).collect();
    let mut h = h0.to_vec();
    for _ in 0..p.steps {
        let f = flow(g, &h, &ocean);
        for &k in &f.order {
            let k = k as usize;
            let r = f.rec[k];
            if r == u32::MAX {
                continue;
            }
            let r = r as usize;
            let dist = {
                let mut d = 1.0;
                for (m, &nb) in g.nbr[k].iter().enumerate() {
                    if nb as usize == r {
                        d = g.nbr_d[k][m];
                    }
                }
                d
            };
            let hr = if ocean[r] { h[r].max(0.0) } else { h[r] };
            let fac = p.k * f.area[k].sqrt() / dist;
            let mut nh = (h[k] + p.u * uplift[k] + fac * hr) / (1.0 + fac);
            nh = nh.min(hr + p.smax * dist);
            h[k] = nh.max(1.0);
        }
    }
    // Final routing on the eroded surface, for the rivers.
    let f = flow(g, &h, &ocean);
    let lake: Vec<bool> = (0..n)
        .map(|k| !ocean[k] && f.fill[k] > h[k] + 2.0)
        .collect();
    Eroded {
        h,
        area: f.area,
        lake,
        rec: f.rec,
    }
}
