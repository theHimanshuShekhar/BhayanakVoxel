//! Serial droplet erosion (port of Sebastian Lague's CPU Erosion.cs, radius 3 brush, lifetime 30),
//! on one 2048^2 face of the real map, one droplet per land texel, spawn points from an integer
//! hash. Timing only. Throwaway.
use std::time::Instant;
use planet_gen::world_map::WorldMap;

fn hash(mut x: u64) -> u64 {
    x ^= x >> 33; x = x.wrapping_mul(0xff51afd7ed558ccd); x ^= x >> 33;
    x = x.wrapping_mul(0xc4ceb9fe1a85ec53); x ^ (x >> 33)
}

fn main() {
    let size: usize = std::env::args().nth(1).map_or(2048, |s| s.parse().unwrap());
    let map = WorldMap::generate(0xB4A7_A9AC, size as u32);
    let face = 4;
    let mut h: Vec<f32> = (0..size * size)
        .map(|k| f32::from(map.texel(face, (k % size) as u32, (k / size) as u32).elevation) * 0.1 / 245.0)
        .collect();
    let land: Vec<usize> = (0..size * size).filter(|&k| h[k] >= 0.0).collect();
    // Brush of radius 3: offsets and weights.
    let r = 3i32;
    let mut brush: Vec<(i32, i32, f32)> = vec![];
    for y in -r..=r { for x in -r..=r {
        let d = ((x * x + y * y) as f32).sqrt();
        if d < r as f32 { brush.push((x, y, 1.0 - d / r as f32)); }
    }}
    let wsum: f32 = brush.iter().map(|b| b.2).sum();
    let (inertia, cap_f, min_cap, erode_s, dep_s, evap, grav) = (0.05f32, 4.0, 0.01, 0.3, 0.3, 0.01, 4.0);
    let t = Instant::now();
    let mut steps = 0u64;
    for (n, _) in land.iter().enumerate() {
        let k = land[(hash(n as u64) % land.len() as u64) as usize];
        let (mut px, mut py) = ((k % size) as f32 + 0.5, (k / size) as f32 + 0.5);
        let (mut dx, mut dy, mut speed, mut water, mut sed) = (0.0f32, 0.0f32, 1.0f32, 1.0f32, 0.0f32);
        for _ in 0..30 {
            let (ix, iy) = (px as i32, py as i32);
            if ix < r + 1 || iy < r + 1 || ix >= size as i32 - r - 2 || iy >= size as i32 - r - 2 { break; }
            let (ox, oy) = (px - ix as f32, py - iy as f32);
            let i0 = iy as usize * size + ix as usize;
            let (nw, ne, sw, se) = (h[i0], h[i0 + 1], h[i0 + size], h[i0 + size + 1]);
            let gx = (ne - nw) * (1.0 - oy) + (se - sw) * oy;
            let gy = (sw - nw) * (1.0 - ox) + (se - ne) * ox;
            let height = nw * (1.0 - ox) * (1.0 - oy) + ne * ox * (1.0 - oy) + sw * (1.0 - ox) * oy + se * ox * oy;
            dx = dx * inertia - gx * (1.0 - inertia);
            dy = dy * inertia - gy * (1.0 - inertia);
            let len = (dx * dx + dy * dy).sqrt().max(0.01);
            dx /= len; dy /= len;
            px += dx; py += dy;
            steps += 1;
            let (jx, jy) = (px as i32, py as i32);
            if jx < 1 || jy < 1 || jx >= size as i32 - 2 || jy >= size as i32 - 2 { break; }
            let (qx, qy) = (px - jx as f32, py - jy as f32);
            let j0 = jy as usize * size + jx as usize;
            let nh = h[j0] * (1.0 - qx) * (1.0 - qy) + h[j0 + 1] * qx * (1.0 - qy) + h[j0 + size] * (1.0 - qx) * qy + h[j0 + size + 1] * qx * qy;
            let dh = nh - height;
            let cap = (-dh * speed * water * cap_f).max(min_cap);
            if sed > cap || dh > 0.0 {
                let amt = if dh > 0.0 { dh.min(sed) } else { (sed - cap) * dep_s };
                sed -= amt;
                h[i0] += amt * (1.0 - ox) * (1.0 - oy); h[i0 + 1] += amt * ox * (1.0 - oy);
                h[i0 + size] += amt * (1.0 - ox) * oy; h[i0 + size + 1] += amt * ox * oy;
            } else {
                let amt = ((cap - sed) * erode_s).min(-dh);
                for &(bx, by, w) in &brush {
                    let b = (i0 as i64 + by as i64 * size as i64 + bx as i64) as usize;
                    let e = amt * w / wsum;
                    let d = if h[b] < e { h[b] } else { e };
                    h[b] -= d; sed += d;
                }
            }
            speed = (speed * speed + dh * grav).max(0.0).sqrt();
            water *= 1.0 - evap;
        }
    }
    let s = t.elapsed().as_secs_f64();
    println!("droplets {} on one {size}^2 face: {s:.1} s, {steps} droplet steps, {:.1} ns/step; x6 faces = {:.0} s per pass", land.len(), s * 1e9 / steps as f64, s * 6.0);
}
