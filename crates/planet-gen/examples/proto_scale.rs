//! PROTOTYPE (#243, throwaway): dumps a world map's elevation as a lat-long grid and a fine patch
//! around the highest land, for the vertical- and continent-scale renders. Not for main.
//! `PROTO_CONTINENT_CELL=20 PROTO_OCEAN_SHARE=0.6 cargo run --release -p planet-gen --example proto_scale -- OUT`
use planet_gen::world_map::WorldMap;
use std::io::Write;
fn main() {
    let out = std::env::args().nth(1).unwrap_or("proto".into());
    std::fs::create_dir_all(&out).unwrap();
    let map = WorldMap::generate(0xB4A7_A9AC, 2048);
    let (w, h) = (4096usize, 2048usize);
    let mut f = std::io::BufWriter::new(std::fs::File::create(format!("{out}/latlong_f32.bin")).unwrap());
    let mut best = (f64::MIN, [0.0; 3]);
    for y in 0..h {
        let lat = std::f64::consts::PI * (0.5 - (y as f64 + 0.5) / h as f64);
        for x in 0..w {
            let lon = 2.0 * std::f64::consts::PI * ((x as f64 + 0.5) / w as f64) - std::f64::consts::PI;
            let d = [lat.cos() * lon.sin(), lat.sin(), lat.cos() * lon.cos()];
            let e = map.elevation_at(d);
            if e > best.0 && lat.abs() < 1.2 { best = (e, d); }
            f.write_all(&(e as f32).to_le_bytes()).unwrap();
        }
    }
    // A 120 km patch, 100 m spacing, centred on the highest land.
    let c = best.1;
    let up = [0.0, 1.0, 0.0];
    let cross = |a: [f64; 3], b: [f64; 3]| [a[1]*b[2]-a[2]*b[1], a[2]*b[0]-a[0]*b[2], a[0]*b[1]-a[1]*b[0]];
    let norm = |a: [f64; 3]| { let l = (a[0]*a[0]+a[1]*a[1]+a[2]*a[2]).sqrt(); a.map(|v| v / l) };
    let east = norm(cross(up, c)); let north = cross(c, east);
    let n = 1200usize; let step = 100.0 / 320_000.0;
    let mut g = std::io::BufWriter::new(std::fs::File::create(format!("{out}/patch_f32.bin")).unwrap());
    for j in 0..n { for i in 0..n {
        let (u, v) = ((i as f64 - n as f64 / 2.0) * step, (n as f64 / 2.0 - j as f64) * step);
        let d = norm([0, 1, 2].map(|k| c[k] + u * east[k] + v * north[k]));
        g.write_all(&(map.elevation_at(d) as f32).to_le_bytes()).unwrap();
    }}
    println!("max land {:.0} (units of elevation_at) at {:?}", best.0, best.1);
}
