//! PROTOTYPE (#206), throwaway: dev-box images of the rivers and lakes of one method.
//!
//! `cargo run --release -p planet-gen --example proto_rivers_maps -- d8|mfd OUT_DIR [SIZE]`
//!
//! Set `PROTO_MAP_CACHE=DIR` to keep the 90 s of the landform model between runs. Writes, per
//! method: the whole Planet (equirectangular), the biggest river's basin at the map's texels,
//! and top-down views through the generator (what a brick makes): the river's mouth, a mid-size
//! river, the biggest lake, and a source.

use std::path::Path;
use std::time::Instant;

use brick_store::{VOXELS_PER_METRE, VoxelCoord};
use planet_gen::planet::Planet;
use planet_gen::rivers_proto::{self, HEAD, NO_RECEIVER, NO_WATER, RiverData, RiverMode};
use planet_gen::world_map::{WORLD_RADIUS, WorldMap};

type Rgb = [u8; 3];

fn write_png(path: &Path, w: u32, h: u32, rgb: &[u8]) {
    let file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut enc = png::Encoder::new(file, w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(rgb).unwrap();
    println!("wrote {}", path.display());
}

fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    std::array::from_fn(|k| (f64::from(a[k]) + (f64::from(b[k]) - f64::from(a[k])) * t) as u8)
}

fn land_colour(m: f64) -> Rgb {
    if m < 0.0 {
        mix([70, 130, 210], [8, 16, 70], -m / 4000.0)
    } else if m < 1500.0 {
        mix([70, 140, 70], [175, 155, 95], m / 1500.0)
    } else if m < 4500.0 {
        mix([175, 155, 95], [130, 130, 130], (m - 1500.0) / 3000.0)
    } else {
        mix([130, 130, 130], [255, 255, 255], (m - 4500.0) / 3000.0)
    }
}

fn dir_of(lat: f64, lon: f64) -> [f64; 3] {
    let (la, lo) = (lat.to_radians(), lon.to_radians());
    [la.cos() * lo.cos(), la.sin(), la.cos() * lo.sin()]
}

fn norm(a: [f64; 3]) -> [f64; 3] {
    let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    a.map(|c| c / l)
}

fn frame(d: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let r = if d[1].abs() > 0.99 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
    let e = norm([r[1] * d[2] - r[2] * d[1], r[2] * d[0] - r[0] * d[2], r[0] * d[1] - r[1] * d[0]]);
    let n = [d[1] * e[2] - d[2] * e[1], d[2] * e[0] - d[0] * e[2], d[0] * e[1] - d[1] * e[0]];
    (e, n)
}

fn par_rows(h: usize, f: impl Fn(usize) -> Vec<u8> + Sync) -> Vec<u8> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    let mut rows: Vec<Vec<u8>> = vec![Vec::new(); h];
    let next = std::sync::atomic::AtomicUsize::new(0);
    let slots: Vec<std::sync::Mutex<&mut Vec<u8>>> = rows.iter_mut().map(std::sync::Mutex::new).collect();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let y = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if y >= h {
                    break;
                }
                let row = f(y);
                **slots[y].lock().unwrap() = row;
            });
        }
    });
    drop(slots);
    rows.concat()
}

fn material_colour(id: u16) -> Rgb {
    match id {
        27 => [30, 90, 200],
        6 => [225, 205, 140],
        5 => [150, 150, 150],
        31 => [95, 65, 40],
        3 => [80, 150, 60],
        2 | 4 => [120, 95, 60],
        14 => [240, 240, 250],
        15 => [190, 220, 240],
        7 | 29 => [200, 150, 100],
        _ => [115, 115, 120],
    }
}

/// A top-down view through the generator: `extent` metres across at `px` pixels, hill-shaded,
/// with the top Material's colour and the water in blue (deeper is darker).
fn topdown(planet: &Planet, centre: [f64; 3], extent: f64, px: usize) -> Vec<u8> {
    let (east, north) = frame(centre);
    let radius = f64::from(WORLD_RADIUS);
    let v = VOXELS_PER_METRE;
    let step = extent / px as f64;
    let at = |x: f64, y: f64| -> [f64; 3] {
        let p: [f64; 3] = std::array::from_fn(|k| centre[k] * radius + east[k] * x * v + north[k] * y * v);
        let l = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
        p.map(|c| c / l * radius)
    };
    let height = |x: f64, y: f64| {
        let p = at(x, y);
        (planet.ground_height(p), planet.surface_height(p))
    };
    par_rows(px, |row| {
        let mut out = Vec::with_capacity(px * 3);
        let y = (px as f64 / 2.0 - row as f64 - 0.5) * step;
        for col in 0..px {
            let x = (col as f64 - px as f64 / 2.0 + 0.5) * step;
            let (g, s) = height(x, y);
            let (_, sl) = height(x - step, y);
            let (_, sr) = height(x + step, y);
            let (_, su) = height(x, y + step);
            let (_, sd) = height(x, y - step);
            let (gx, gy) = ((sr - sl) / (2.0 * step * v), (su - sd) / (2.0 * step * v));
            let shade = (0.8 - 1.4 * (gx - gy) * 0.7).clamp(0.35, 1.25);
            // The top Material: a voxel a little under the surface.
            let p = at(x, y);
            let r = radius + s - 0.6;
            let q = p.map(|c| c / radius * r);
            let m = planet.voxel(VoxelCoord(q.map(|c| c.round() as i32)));
            let mut c = material_colour(m.map_or(0, |m| m.0 as u16));
            if s > g + 0.5 {
                let depth = (s - g) / v;
                c = mix([70, 140, 220], [10, 40, 120], depth / 4.0);
            }
            out.extend(c.map(|c| (f64::from(c) * shade).clamp(0.0, 255.0) as u8));
        }
        out
    })
}

struct Found {
    mouth: (usize, u32, u32),
    river: (usize, u32, u32),
    source: (usize, u32, u32),
    lake: Option<(usize, u32, u32, usize)>,
}

fn find(map: &WorldMap, rd: &RiverData) -> Found {
    let n = map.size();
    let total = 6 * (n as usize).pow(2);
    let split = |idx: usize| {
        let (f, i, j) = rivers_proto::texel_index(map, idx);
        (f, i, j)
    };
    // The biggest river reaching the sea.
    let mut mouth: Option<(u8, usize)> = None;
    let mut river: Option<(u32, usize)> = None;
    let mut source: Option<(u32, usize)> = None;
    for idx in 0..total {
        if !rd.is_river(idx) || rd.recv[idx] & NO_RECEIVER != 0 {
            continue;
        }
        let (f, i, j) = split(idx);
        let t = map.texel(f, i, j);
        // Its receiver is under the sea?
        let k = (rd.recv[idx] & 7) as usize;
        let offs = [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (-1, 1), (1, -1), (-1, -1)][k];
        let (ni, nj) = (i as i32 + offs.0, j as i32 + offs.1);
        let inside = ni >= 0 && nj >= 0 && ni < n as i32 && nj < n as i32;
        let to_sea = inside && map.texel(f, ni as u32, nj as u32).is_ocean();
        if to_sea && mouth.is_none_or(|m| rd.flow[idx] > m.0) && i > 220 && j > 220 && i < n - 220 && j < n - 220 {
            mouth = Some((rd.flow[idx], idx));
        }
        // A mid-size river: about 3000 texels draining, inland, low.
        let want = 12 * 11 + 6; // 2^(11.5) ~ 2900
        let e = f64::from(t.elevation) / 4.0;
        if (i32::from(rd.flow[idx]) - want).abs() <= 1 && (20.0..300.0).contains(&e) {
            let h = (idx as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 40;
            if river.is_none_or(|r| h as u32 > r.0) && i > 100 && j > 100 && i < n - 100 && j < n - 100 {
                river = Some((h as u32, idx));
            }
        }
        if rd.recv[idx] & HEAD != 0 && e > 100.0 && e < 800.0 && rd.flow[idx] >= rd.thr && rd.flow[idx] < rd.thr + 3 {
            let h = (idx as u64).wrapping_mul(0xbf58_476d_1ce4_e5b9) >> 40;
            if source.is_none_or(|r| h as u32 > r.0) && i > 100 && j > 100 && i < n - 100 && j < n - 100 {
                source = Some((h as u32, idx));
            }
        }
    }
    // The biggest lake: flood the lake texels.
    let mut seen = vec![false; total];
    let mut lake: Option<(usize, u32, u32, usize)> = None;
    for s in 0..total {
        if seen[s] || !rd.is_lake(s) {
            continue;
        }
        let mut group = vec![s];
        seen[s] = true;
        let mut at = 0;
        let (mut si, mut sj) = (0u64, 0u64);
        let mut face = 0;
        while at < group.len() {
            let (f, i, j) = split(group[at]);
            at += 1;
            face = f;
            si += u64::from(i);
            sj += u64::from(j);
            for (di, dj) in [(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
                let (ni, nj) = (i as i32 + di, j as i32 + dj);
                if ni < 0 || nj < 0 || ni >= n as i32 || nj >= n as i32 {
                    continue;
                }
                let nidx = (f * n as usize + nj as usize) * n as usize + ni as usize;
                if !seen[nidx] && rd.is_lake(nidx) {
                    seen[nidx] = true;
                    group.push(nidx);
                }
            }
        }
        let len = group.len();
        if lake.is_none_or(|l| len > l.3) {
            let (ci, cj) = ((si / len as u64) as i64, (sj / len as u64) as i64);
            let near = group
                .iter()
                .map(|&g| split(g))
                .min_by_key(|&(_, i, j)| (i as i64 - ci).pow(2) + (j as i64 - cj).pow(2))
                .unwrap();
            lake = Some((face, near.1, near.2, len));
        }
    }
    let mouth = split(mouth.expect("no river reaches the sea").1);
    let river = split(river.map_or(split_back(mouth, n), |r| r.1));
    let source = split(source.map_or(split_back(river, n), |r| r.1));
    Found { mouth, river, source, lake }
}

fn split_back((f, i, j): (usize, u32, u32), n: u32) -> usize {
    (f * n as usize + j as usize) * n as usize + i as usize
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mode = RiverMode::parse(&args.next().expect("d8|mfd")).expect("d8|mfd");
    let out = std::path::PathBuf::from(args.next().expect("OUT_DIR"));
    let size: u32 = args.next().map_or(2048, |s| s.parse().unwrap());
    std::fs::create_dir_all(&out).unwrap();
    let name = format!("{mode:?}").to_lowercase();
    rivers_proto::set_default_mode(mode);
    let t = Instant::now();
    let planet = Planet::new(WorldMap::generate(0xB4A7_A9AC, size));
    println!("world map ({name}) in {:.1} s", t.elapsed().as_secs_f64());
    let map = planet.map();
    let rd = map.rivers().expect("rivers");
    println!("storage added: {} B per texel", rd.bytes_per_texel());
    let found = find(map, rd);
    let n = map.size();

    // The whole Planet, equirectangular.
    let (w, h) = (2048usize, 1024usize);
    let mut img = par_rows(h, |py| {
        let lat = 90.0 - 180.0 * (py as f64 + 0.5) / h as f64;
        (0..w)
            .flat_map(|px| {
                let lon = 360.0 * (px as f64 + 0.5) / w as f64 - 180.0;
                land_colour(map.elevation_at(dir_of(lat, lon)) / VOXELS_PER_METRE)
            })
            .collect()
    });
    let total = 6 * (n as usize).pow(2);
    let mut paint = |idx: usize, c: Rgb| {
        let (f, i, j) = rivers_proto::texel_index(map, idx);
        let d = map.texel_direction(f, i, j);
        let (lat, lon) = (d[1].asin().to_degrees(), d[2].atan2(d[0]).to_degrees());
        let px = (((lon + 180.0) / 360.0 * w as f64) as usize).min(w - 1);
        let py = (((90.0 - lat) / 180.0 * h as f64) as usize).min(h - 1);
        img[(py * w + px) * 3..(py * w + px) * 3 + 3].copy_from_slice(&c);
    };
    for idx in 0..total {
        if rd.is_lake(idx) {
            paint(idx, [0, 255, 255]);
        } else if rd.is_river(idx) && rd.flow[idx] >= rd.thr + 18 {
            let t = f64::from(rd.flow[idx] - rd.thr) / 80.0;
            paint(idx, mix([60, 120, 255], [10, 10, 200], t));
        }
    }
    write_png(&out.join(format!("{name}-overview.png")), w as u32, h as u32, &img);

    // The biggest river's basin at the map's texels, 2x.
    let (f, mi, mj) = found.mouth;
    let half = 200i32;
    let (i0, j0) = (
        (mi as i32 - half).clamp(0, n as i32 - 2 * half),
        (mj as i32 - half).clamp(0, n as i32 - 2 * half),
    );
    let side = (2 * half) as usize;
    let mut crop = vec![0u8; side * 2 * side * 2 * 3];
    for dj in 0..side {
        for di in 0..side {
            let (i, j) = (i0 as u32 + di as u32, j0 as u32 + dj as u32);
            let t = map.texel(f, i, j);
            let e = f64::from(t.elevation) / 4.0;
            let ex = |a: i32, b: i32| {
                let (ii, jj) = ((i as i32 + a).clamp(0, n as i32 - 1) as u32, (j as i32 + b).clamp(0, n as i32 - 1) as u32);
                f64::from(map.texel(f, ii, jj).elevation) / 4.0
            };
            let shade = (0.85 + (ex(-1, 0) - ex(1, 0) + ex(0, 1) - ex(0, -1)) / 160.0).clamp(0.5, 1.2);
            let mut c = land_colour(e).map(|c| (f64::from(c) * shade).min(255.0) as u8);
            let idx = (f * n as usize + j as usize) * n as usize + i as usize;
            if rd.is_lake(idx) {
                c = [0, 255, 255];
            } else if rd.water[idx] != NO_WATER && rd.flow[idx] >= rd.thr {
                let t = f64::from(rd.flow[idx] - rd.thr) / 100.0;
                c = mix([90, 150, 255], [20, 20, 160], t);
            }
            for (a, b) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let o = ((dj * 2 + b) * side * 2 + di * 2 + a) * 3;
                crop[o..o + 3].copy_from_slice(&c);
            }
        }
    }
    write_png(&out.join(format!("{name}-basin-texels.png")), (side * 2) as u32, (side * 2) as u32, &crop);

    let dir_of_texel = |(f, i, j): (usize, u32, u32)| map.texel_direction(f, i, j);
    let jobs: Vec<(String, [f64; 3], f64, usize)> = {
        let mut v = vec![
            ("mouth-8km".to_string(), dir_of_texel(found.mouth), 8000.0, 1000),
            ("river-2km".to_string(), dir_of_texel(found.river), 2000.0, 1000),
            ("source-1km".to_string(), dir_of_texel(found.source), 1000.0, 1000),
        ];
        if let Some((f, i, j, len)) = found.lake {
            println!("biggest lake: {len} texels");
            v.push(("lake-10km".to_string(), dir_of_texel((f, i, j)), 10000.0, 1000));
        }
        v
    };
    for (label, centre, extent, px) in jobs {
        let t = Instant::now();
        let img = topdown(&planet, centre, extent, px);
        write_png(&out.join(format!("{name}-{label}.png")), px as u32, px as u32, &img);
        println!("  {label} in {:.1} s", t.elapsed().as_secs_f64());
    }
}
