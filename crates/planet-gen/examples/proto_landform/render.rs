//! Renderers: orbit globe, perspective ray-march from altitude, orthographic crop,
//! equirectangular map layers, legends.

use crate::grid::*;
use crate::img::*;
use crate::noise::*;

pub struct Scene<'a> {
    pub g: &'a Grid,
    pub h: &'a [f32],
    pub temp: &'a [f32],
    pub area: &'a [f32],
    pub lake: &'a [bool],
    pub rivers: bool,
}

const LAND: &[(f32, [f32; 3])] = &[
    (0.0, [66.0, 118.0, 58.0]),
    (150.0, [92.0, 140.0, 68.0]),
    (500.0, [148.0, 160.0, 88.0]),
    (1000.0, [186.0, 166.0, 108.0]),
    (1800.0, [166.0, 126.0, 90.0]),
    (2800.0, [140.0, 122.0, 108.0]),
    (4000.0, [168.0, 164.0, 164.0]),
    (5500.0, [220.0, 220.0, 226.0]),
    (8000.0, [255.0, 255.0, 255.0]),
];
const SEA: &[(f32, [f32; 3])] = &[
    (0.0, [78.0, 168.0, 196.0]),
    (200.0, [44.0, 118.0, 176.0]),
    (1000.0, [28.0, 76.0, 146.0]),
    (2500.0, [16.0, 44.0, 104.0]),
    (4200.0, [8.0, 20.0, 60.0]),
];

pub fn hypso(h: f32) -> [f32; 3] {
    if h > 0.0 {
        ramp(LAND, h)
    } else {
        ramp(SEA, -h)
    }
}

impl Scene<'_> {
    fn height(&self, d: V3) -> f64 {
        f64::from(self.g.sample(self.h, d))
    }
    /// Slope (dimensionless, uphill positive) along east and north.
    fn grad(&self, d: V3, delta: f64) -> (f64, f64) {
        let (e, n) = Grid::frame(d);
        let s = delta / R;
        let he = self.height(norm(add(d, mul(e, s))));
        let hw = self.height(norm(sub(d, mul(e, s))));
        let hn = self.height(norm(add(d, mul(n, s))));
        let hs = self.height(norm(sub(d, mul(n, s))));
        ((he - hw) / (2.0 * delta * 1000.0), (hn - hs) / (2.0 * delta * 1000.0))
    }
    /// Surface colour, before lighting.
    fn albedo(&self, d: V3, h: f64) -> [f32; 3] {
        let k = self.g.nearest(d);
        let t = self.g.sample(self.temp, d);
        if h > 0.0 {
            let mut c = hypso(h as f32);
            let snow = smoothstep(-2.0, -9.0, f64::from(t)) as f32;
            c = lerp3(c, [240.0, 245.0, 250.0], snow * 0.92);
            if self.rivers {
                if self.lake[k] {
                    c = [70.0, 150.0, 235.0];
                } else if self.area[k] > 1200.0 {
                    let s = ((self.area[k] / 1200.0).ln() / (60000f32 / 1200.0).ln()).clamp(0.0, 1.0);
                    c = lerp3(c, lerp3([60.0, 140.0, 255.0], [30.0, 80.0, 230.0], s), 0.65 + 0.3 * s);
                }
            }
            c
        } else {
            let mut c = hypso(h as f32);
            let ice = smoothstep(-8.0, -18.0, f64::from(t)) as f32;
            c = lerp3(c, [215.0, 228.0, 240.0], ice * 0.85);
            c
        }
    }
    /// Lit colour at a direction on the sphere with the global light `l`.
    fn lit(&self, d: V3, l: V3, exag: f64) -> (f64, [f32; 3]) {
        let h = self.height(d);
        let (gx, gy) = self.grad(d, 0.9);
        let (e, nn) = Grid::frame(d);
        let ex = if h > 0.0 { exag } else { exag * 0.35 };
        let nrm = norm(sub(d, add(mul(e, ex * gx), mul(nn, ex * gy))));
        let diff = dot(nrm, l).max(0.0);
        let sph = dot(d, l).max(0.0);
        // Hill shading is the perturbed-normal term; the sphere term darkens the night side.
        let shade = 0.10 + 0.95 * (0.65 * diff + 0.35 * diff.min(sph * 1.4 + 0.15));
        let a = self.albedo(d, h);
        (h, [a[0] * shade as f32, a[1] * shade as f32, a[2] * shade as f32])
    }
}

fn sky(up: f64) -> [f32; 3] {
    lerp3([190.0, 212.0, 238.0], [62.0, 110.0, 195.0], (up * 2.2).clamp(0.0, 1.0) as f32)
}

/// Globe seen from distance `dist_r` (in planet radii), looking at the centre from `view_dir`.
pub fn orbit(sc: &Scene, view_dir: V3, dist_r: f64, size: usize, exag: f64) -> Img {
    let fwd = mul(view_dir, -1.0);
    let upv = if view_dir[1].abs() > 0.95 { [0.0, 0.0, 1.0] } else { [0.0, 1.0, 0.0] };
    let right = norm(cross(fwd, upv));
    let up = cross(right, fwd);
    let cam = mul(view_dir, dist_r * R);
    let half = (1.0 / dist_r).asin() * 1.12;
    let tanf = half.tan();
    // Sun: well off to the left and above, slightly toward the viewer.
    let l = norm(add(add(mul(right, -0.85), mul(up, 0.35)), mul(view_dir, 0.45)));
    let px: Vec<Rgb> = par_map(size * size, |i| {
        let (x, y) = (i % size, i / size);
        let sx = (2.0 * (x as f64 + 0.5) / size as f64 - 1.0) * tanf;
        let sy = (1.0 - 2.0 * (y as f64 + 0.5) / size as f64) * tanf;
        let rd = norm(add(fwd, add(mul(right, sx), mul(up, sy))));
        let b = dot(cam, rd);
        let c2 = dot(cam, cam) - R * R;
        let disc = b * b - c2;
        if disc > 0.0 {
            let t = -b - disc.sqrt();
            let pos = add(cam, mul(rd, t));
            let d = norm(pos);
            let (_, c) = sc.lit(d, l, exag);
            let rim = (1.0 + dot(d, rd)).max(0.0).powi(3) as f32 * 0.5 * (dot(d, l).max(0.0) as f32 * 0.8 + 0.2);
            to_rgb(lerp3(c, [130.0, 180.0, 255.0], rim))
        } else {
            // Atmosphere glow near the limb.
            let closest = add(cam, mul(rd, -b));
            let dist = len(closest);
            let glow = (1.0 - (dist - R) / (0.07 * R)).clamp(0.0, 1.0);
            let lit = (dot(norm(closest), l) * 0.8 + 0.35).clamp(0.0, 1.0);
            let a = (glow * glow * lit) as f32;
            to_rgb(lerp3([3.0, 5.0, 12.0], [120.0, 170.0, 255.0], a))
        }
    });
    Img::from_px(size, size, px)
}

/// True orthographic crop: `half_km` is half the window width, looking straight down at `centre`.
pub fn ortho(sc: &Scene, centre: V3, half_km: f64, size: usize, exag: f64) -> Img {
    let (e, n) = Grid::frame(centre);
    let l = norm(add(add(mul(e, -0.85), mul(n, 0.35)), mul(centre, 0.45)));
    let px: Vec<Rgb> = par_map(size * size, |i| {
        let (x, y) = (i % size, i / size);
        let sx = (2.0 * (x as f64 + 0.5) / size as f64 - 1.0) * half_km;
        let sy = (1.0 - 2.0 * (y as f64 + 0.5) / size as f64) * half_km;
        let r2 = sx * sx + sy * sy;
        if r2 >= R * R * 0.995 {
            return [10, 14, 24];
        }
        let z = (R * R - r2).sqrt();
        let pos = add(add(mul(centre, z), mul(e, sx)), mul(n, sy));
        let d = norm(pos);
        let (_, c) = sc.lit(d, l, exag);
        // Fade toward the limb, where the view is edge-on.
        let edge = smoothstep(0.985, 0.8, dot(d, centre)) as f32;
        to_rgb(lerp3(c, [10.0, 14.0, 24.0], edge * 0.0))
    });
    Img::from_px(size, size, px)
}

pub struct Cam {
    pub pos: V3,
    pub target: V3,
}

/// Camera `dist_km` from the target along the ground, `alt_km` above sea level, looking toward
/// `azimuth` (radians, 0 = north, pi/2 = east).
pub fn camera(target_dir: V3, azimuth: f64, dist_km: f64, alt_km: f64) -> Cam {
    let (e, n) = Grid::frame(target_dir);
    let look = add(mul(n, azimuth.cos()), mul(e, azimuth.sin()));
    let th = dist_km / R;
    let cdir = add(mul(target_dir, th.cos()), mul(look, -th.sin()));
    Cam { pos: mul(cdir, R + alt_km), target: mul(target_dir, R) }
}

pub fn persp(sc: &Scene, cam: &Cam, w: usize, h: usize, fov_deg: f64, exag: f64) -> Img {
    let fwd = norm(sub(cam.target, cam.pos));
    let up0 = norm(cam.pos);
    let right = norm(cross(fwd, up0));
    let up = cross(right, fwd);
    let tanf = (fov_deg.to_radians() * 0.5).tan();
    let tdir = norm(cam.target);
    let l = norm(add(add(mul(right, -0.8), mul(up0, 0.5)), mul(fwd, 0.25)));
    let _ = tdir;
    let ex = exag / 1000.0;
    let px: Vec<Rgb> = par_map(w * h, |i| {
        let (x, y) = (i % w, i / w);
        let sx = (2.0 * (x as f64 + 0.5) / w as f64 - 1.0) * tanf * (w as f64 / h as f64);
        let sy = (1.0 - 2.0 * (y as f64 + 0.5) / h as f64) * tanf;
        let rd = norm(add(fwd, add(mul(right, sx), mul(up, sy))));
        let skyc = sky(dot(rd, up0));
        let mut t = 0.5;
        let mut prev = 0.0;
        let tmax = 520.0;
        let mut hit = false;
        while t < tmax {
            let pos = add(cam.pos, mul(rd, t));
            let r = len(pos);
            let hh = sc.height(mul(pos, 1.0 / r)).max(0.0);
            let tr = R + hh * ex;
            if r < tr {
                hit = true;
                break;
            }
            if r > R + 12.0 * exag + 2.0 && dot(pos, rd) > 0.0 {
                break;
            }
            prev = t;
            t += (0.5 * (r - tr)).clamp(0.25, 5.0);
        }
        if !hit {
            return to_rgb(skyc);
        }
        let (mut lo, mut hi) = (prev, t);
        for _ in 0..7 {
            let mid = 0.5 * (lo + hi);
            let pos = add(cam.pos, mul(rd, mid));
            let r = len(pos);
            let hh = sc.height(mul(pos, 1.0 / r)).max(0.0);
            if r < R + hh * ex {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        let pos = add(cam.pos, mul(rd, hi));
        let d = norm(pos);
        let hraw = sc.height(d);
        let fog = 1.0 - (-hi / 240.0).exp();
        let haze = [172.0, 196.0, 228.0];
        let c = if hraw <= 0.0 {
            // Water: depth colour, sky reflection and a sun glint.
            let a = sc.albedo(d, hraw);
            let nrm = d;
            let cosv = (-dot(rd, nrm)).clamp(0.0, 1.0);
            let fres = (1.0 - cosv).powi(4) as f32 * 0.8 + 0.04;
            let refl = reflect(rd, nrm);
            let spec = dot(refl, l).max(0.0).powi(60) as f32 * 0.7;
            let lit = (0.35 + 0.65 * dot(nrm, l).max(0.0)) as f32;
            let base = [a[0] * lit, a[1] * lit, a[2] * lit];
            let mixed = lerp3(base, sky(0.3), fres);
            [mixed[0] + spec * 255.0, mixed[1] + spec * 255.0, mixed[2] + spec * 240.0]
        } else {
            // Shade with the exaggerated slope so the light matches what the eye sees.
            let (gx, gy) = sc.grad(d, 0.8);
            let (e, nn) = Grid::frame(d);
            let nrm = norm(sub(d, add(mul(e, exag * gx), mul(nn, exag * gy))));
            let shade = (0.16 + 0.95 * dot(nrm, l).max(0.0)) as f32;
            let a = sc.albedo(d, hraw);
            [a[0] * shade, a[1] * shade, a[2] * shade]
        };
        to_rgb(lerp3(c, haze, fog as f32 * 0.85))
    });
    Img::from_px(w, h, px)
}

fn reflect(i: V3, n: V3) -> V3 {
    sub(i, mul(n, 2.0 * dot(i, n)))
}

// ------------------------------------------------------------------ equirectangular maps

pub const MAP_W: usize = 2048;
pub const MAP_H: usize = 1024;

pub fn equirect(g: &Grid, w: usize, h: usize, f: impl Fn(usize, V3) -> [f32; 3] + Sync) -> Img {
    let px: Vec<Rgb> = par_map(w * h, |i| {
        let (x, y) = (i % w, i / w);
        let lon = ((x as f64 + 0.5) / w as f64 * 2.0 - 1.0) * std::f64::consts::PI;
        let lat = (0.5 - (y as f64 + 0.5) / h as f64) * std::f64::consts::PI;
        let d = [lat.cos() * lon.cos(), lat.sin(), lat.cos() * lon.sin()];
        to_rgb(f(g.nearest(d), d))
    });
    Img::from_px(w, h, px)
}

/// Local hill shade for map views: sun from the north-west.
pub fn map_shade(sc: &Scene, d: V3, exag: f64) -> f32 {
    let (gx, gy) = sc.grad(d, 1.0);
    let ex = if sc.height(d) > 0.0 { exag } else { exag * 0.3 };
    let nv = norm([-ex * gx, -ex * gy, 1.0]);
    let l = norm([-0.6, 0.6, 0.55]);
    (0.35 + 0.8 * (nv[0] * l[0] + nv[1] * l[1] + nv[2] * l[2]).max(0.0)) as f32
}

pub fn legend_entries(img: &Img, title: &str, entries: &[(String, Rgb)], cols: usize) -> Img {
    let sc = 2;
    let rowh = 16;
    let rows = entries.len().div_ceil(cols);
    let band = 30 + rows * rowh + 8;
    let mut out = Img::new(img.w, img.h + band, [18, 20, 26]);
    out.blit(img, 0, 0);
    out.text(10, img.h + 8, title, 2, [235, 235, 235]);
    let colw = img.w / cols;
    for (i, (name, c)) in entries.iter().enumerate() {
        let (cx, cy) = (10 + (i / rows) * colw, img.h + 30 + (i % rows) * rowh);
        out.rect(cx, cy, 12, 12, *c);
        out.text(cx + 18, cy + 1, name, sc, [215, 215, 215]);
    }
    out
}

pub fn legend_bar(img: &Img, title: &str, stops: &[(f32, [f32; 3])], lo: f32, hi: f32, unit: &str) -> Img {
    let band = 52;
    let mut out = Img::new(img.w, img.h + band, [18, 20, 26]);
    out.blit(img, 0, 0);
    out.text(10, img.h + 6, title, 2, [235, 235, 235]);
    let (bx, by, bw) = (img.w / 2, img.h + 8, img.w / 2 - 20);
    for x in 0..bw {
        let v = lo + (hi - lo) * x as f32 / (bw - 1) as f32;
        out.rect(bx + x, by, 1, 18, to_rgb(ramp(stops, v)));
    }
    out.text(bx, by + 24, &format!("{lo:.0}{unit}"), 2, [215, 215, 215]);
    let hi_s = format!("{hi:.0}{unit}");
    out.text(bx + bw - Img::text_width(&hi_s, 2), by + 24, &hi_s, 2, [215, 215, 215]);
    out
}

pub fn label(img: &Img, text: &str) -> Img {
    let mut out = img.clone();
    out.rect(0, 0, img.w, 26, [18, 20, 26]);
    out.text(8, 6, text, 2, [240, 240, 240]);
    out
}
