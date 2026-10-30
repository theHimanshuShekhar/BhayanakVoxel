//! Generates a world map and dumps its layers as PNGs for the dev to eyeball, then prints how
//! long it took and what it covers.
//!
//! `cargo run --release -p planet-gen --example world_map -- [SIZE] [OUT_DIR] [SEED]`
//!
//! SIZE is the texels along a face (default 2048, the real Planet); OUT_DIR defaults to
//! `world-map`. Each layer is drawn as the unfolded cube, with +y (north) on top, in the
//! cross of faces below, at no more than 512 pixels a face. Neighbouring faces meet without a
//! seam, which is how to see that the map has none.
//!
//! ```text
//!        +y
//!   -x   +z   +x   -z
//!        -y
//! ```

use std::path::Path;
use std::time::Instant;

use planet_gen::world_map::{Biome, Regions, SEA_LEVEL, Texel, WorldMap};

const FACE_PIXELS: u32 = 512;

type Rgb = [u8; 3];

/// The cube point of pixel `(s, t)` of a face in the cross, each in `[-1, 1]`, `t` upwards.
fn cube_point(cell: (u32, u32), s: f64, t: f64) -> [f64; 3] {
    match cell {
        (1, 0) => [s, 1.0, -t],
        (0, 1) => [-1.0, t, s],
        (1, 1) => [s, t, 1.0],
        (2, 1) => [1.0, t, -s],
        (3, 1) => [-s, t, -1.0],
        _ => [s, -1.0, t],
    }
}

fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    std::array::from_fn(|k| (f64::from(a[k]) + (f64::from(b[k]) - f64::from(a[k])) * t) as u8)
}

fn elevation_colour(texel: Texel) -> Rgb {
    let e = f64::from(texel.elevation);
    if texel.is_ocean() {
        mix([70, 130, 210], [8, 16, 70], -e / 10_000.0)
    } else if e < 4_000.0 {
        mix([60, 140, 60], [170, 150, 90], e / 4_000.0)
    } else if e < 12_000.0 {
        mix([170, 150, 90], [130, 130, 130], (e - 4_000.0) / 8_000.0)
    } else {
        mix([130, 130, 130], [255, 255, 255], (e - 12_000.0) / 8_000.0)
    }
}

/// Red where erosion took height away, blue where it laid it down.
fn erosion_colour(texel: Texel) -> Rgb {
    let e = f64::from(texel.erosion);
    let strength = e.abs() / 500.0;
    mix(
        [235, 235, 235],
        if e > 0.0 {
            [200, 20, 20]
        } else {
            [20, 40, 200]
        },
        strength,
    )
}

fn biome_colour(texel: Texel) -> Rgb {
    let land = match texel.biome {
        Biome::Temperate => [70, 150, 70],
        Biome::Desert => [225, 190, 100],
        Biome::Snow => [240, 245, 250],
        Biome::Volcanic => [170, 40, 20],
    };
    if texel.is_ocean() {
        mix(land, [30, 70, 140], 0.6)
    } else {
        land
    }
}

/// Each region adds its own colour to a dark land or ocean.
fn region_colour(texel: Texel) -> Rgb {
    let colours: [(Regions, Rgb); 4] = [
        (Regions::CRYSTAL_CAVES, [255, 0, 255]),
        (Regions::FLOATING_ISLANDS, [0, 220, 255]),
        (Regions::ARCHES_AND_SPIRES, [255, 160, 0]),
        (Regions::BIOLUMINESCENT_FLORA, [0, 255, 80]),
    ];
    let mut rgb: Rgb = if texel.is_ocean() {
        [10, 10, 30]
    } else {
        [45, 45, 45]
    };
    for (region, colour) in colours {
        if texel.regions.contains(region) {
            rgb = std::array::from_fn(|k| rgb[k].saturating_add(colour[k] / 2));
        }
    }
    rgb
}

fn dump(map: &WorldMap, path: &Path, colour: fn(Texel) -> Rgb) {
    let face = map.size().min(FACE_PIXELS);
    let (width, height) = (4 * face, 3 * face);
    let mut pixels = vec![0u8; (width * height * 3) as usize];
    for (cx, cy) in [(1, 0), (0, 1), (1, 1), (2, 1), (3, 1), (1, 2)] {
        for py in 0..face {
            for px in 0..face {
                let s = 2.0 * (f64::from(px) + 0.5) / f64::from(face) - 1.0;
                let t = 1.0 - 2.0 * (f64::from(py) + 0.5) / f64::from(face);
                let rgb = colour(map.sample(cube_point((cx, cy), s, t)));
                let at = (((cy * face + py) * width + cx * face + px) * 3) as usize;
                pixels[at..at + 3].copy_from_slice(&rgb);
            }
        }
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&pixels)
        .unwrap();
}

fn main() {
    let mut args = std::env::args().skip(1);
    let size: u32 = args.next().map_or(2048, |s| s.parse().expect("SIZE"));
    let out = args.next().unwrap_or_else(|| "world-map".into());
    let seed: u64 = args
        .next()
        .map_or(0xB4A7_A9AC, |s| s.parse().expect("SEED"));

    let started = Instant::now();
    let map = WorldMap::generate(seed, size);
    println!("{size}^2 x 6 texels in {:.1?}", started.elapsed());

    let count = |pred: &dyn Fn(Texel) -> bool| {
        let mut n = 0u64;
        for face in 0..6 {
            for j in 0..size {
                for i in 0..size {
                    n += u64::from(pred(map.texel(face, i, j)));
                }
            }
        }
        100.0 * n as f64 / (6.0 * f64::from(size) * f64::from(size))
    };
    println!("hash {:#018x}", map.content_hash());
    println!(
        "ocean {:.1}% of texels (SEA_LEVEL {SEA_LEVEL})",
        count(&|t| t.is_ocean())
    );
    for (name, biome) in [
        ("temperate", Biome::Temperate),
        ("desert", Biome::Desert),
        ("snow", Biome::Snow),
        ("volcanic", Biome::Volcanic),
    ] {
        println!("{name} {:.1}% of texels", count(&|t| t.biome == biome));
    }

    std::fs::create_dir_all(&out).unwrap();
    for (name, colour) in [
        ("elevation", elevation_colour as fn(Texel) -> Rgb),
        ("erosion", erosion_colour),
        ("biomes", biome_colour),
        ("regions", region_colour),
    ] {
        let path = Path::new(&out).join(format!("{name}.png"));
        dump(&map, &path, colour);
        println!("wrote {}", path.display());
    }
}
