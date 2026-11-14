//! PROTOTYPE (#148): hand-placed stamps for what M1b's generators do not make yet: crystals
//! (#135's six to eight hues, emissive) and simple vegetation. Written into the level 0 ring
//! round the shot, in air only, before the scene is built. Nothing here is the design of M1b's
//! stamps: it is as much as it takes for a frame to look like a place.

use brick_store::{BrickStore, Material, VoxelCoord};
use materials::MaterialTable;

use super::{Hue, Place, Stamp};

fn id(name: &str) -> Material {
    Material(
        MaterialTable::built_in()
            .by_name(name)
            .unwrap_or_else(|| panic!("no Material {name}"))
            .id,
    )
}

/// The crystal Material of a hue (the prototype's own entries in `materials.toml`).
pub fn crystal(hue: Hue) -> Material {
    id(match hue {
        Hue::Blue => "crystal blue",
        Hue::Cyan => "crystal cyan",
        Hue::Green => "crystal green",
        Hue::Magenta => "crystal magenta",
        Hue::Red => "crystal red",
        Hue::Amber => "crystal amber",
        Hue::White => "crystal white",
        Hue::Violet => "crystal violet",
    })
}

/// A small deterministic random stream, so that a stamp is the same every run.
struct Rng(u64);

impl Rng {
    fn new(at: [i32; 3]) -> Self {
        let mut h = 0x9E37_79B9_7F4A_7C15u64;
        for c in at {
            h = (h ^ c as u32 as u64).wrapping_mul(0x1000_0000_01B3);
        }
        Self(h | 1)
    }

    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }

    fn range(&mut self, low: f32, high: f32) -> f32 {
        low + (high - low) * self.next()
    }
}

/// A frame at a place: `up` is the way the surface faces, `a` and `b` lie along it.
struct Frame {
    at: [i32; 3],
    up: [i32; 3],
    a: [i32; 3],
    b: [i32; 3],
}

impl Frame {
    fn new(place: &Place) -> Self {
        let up = place.face.map(i32::from);
        // Any two axes across `up`.
        let axis = up.iter().position(|c| *c != 0).unwrap_or(1);
        let mut a = [0; 3];
        a[(axis + 1) % 3] = 1;
        let mut b = [0; 3];
        b[(axis + 2) % 3] = 1;
        Self {
            at: place.at,
            up,
            a,
            b,
        }
    }

    /// The Voxel `h` along the surface's way, and `i`, `j` across it.
    fn at(&self, h: i32, i: i32, j: i32) -> [i32; 3] {
        std::array::from_fn(|k| self.at[k] + self.up[k] * h + self.a[k] * i + self.b[k] * j)
    }
}

/// The Voxels written, so that a stamp can be counted.
pub struct Writer<'a> {
    store: &'a mut BrickStore,
    pub written: usize,
}

impl Writer<'_> {
    /// Sets `at` to `material` if it is air, and nothing else, so that a stamp never replaces
    /// ground. A brick that is not held (all air, as the ring does not hold those) is made
    /// first, as the Planet has it.
    fn put(&mut self, at: [i32; 3], material: Material) {
        let coord = VoxelCoord(at);
        let brick = coord.brick();
        if self.store.brick(brick).is_none() {
            let made = self.store.brick_at(0, brick).into_owned();
            self.store.insert(brick, made);
        }
        if self.store.get(coord).is_none() {
            self.store.set(coord, Some(material));
            self.written += 1;
        }
    }

    /// A disc of `radius` Voxels across the surface, `h` up.
    fn disc(&mut self, f: &Frame, h: i32, (ci, cj): (i32, i32), radius: f32, material: Material) {
        let r = radius.ceil() as i32;
        for i in -r..=r {
            for j in -r..=r {
                if ((i * i + j * j) as f32).sqrt() <= radius + 0.25 {
                    self.put(f.at(h, ci + i, cj + j), material);
                }
            }
        }
    }
}

/// Writes the stamp of `place` into `store`; how many Voxels it set.
pub fn stamp(store: &mut BrickStore, place: &Place, cave_bed: Option<Hue>) -> usize {
    let f = Frame::new(place);
    let mut rng = Rng::new(place.at);
    let mut w = Writer { store, written: 0 };
    match place.stamp {
        Stamp::Crystal(hue) => cluster(&mut w, &f, &mut rng, crystal(hue)),
        Stamp::Tree => tree(&mut w, &f, &mut rng),
        Stamp::Fern => fern(&mut w, &f, &mut rng),
        Stamp::Flowers => flowers(&mut w, &f, &mut rng),
        Stamp::Moss => {
            let moss = cave_bed.map_or_else(|| id("moss"), crystal);
            w.disc(&f, 0, (0, 0), rng.range(2.0, 3.5), moss);
        }
    }
    w.written
}

/// A cluster of faceted spires: one tall, a few leaning, a few small.
fn cluster(w: &mut Writer, f: &Frame, rng: &mut Rng, material: Material) {
    let spires = 4 + (rng.next() * 3.0) as usize;
    for s in 0..spires {
        let (height, radius, lean) = if s == 0 {
            (rng.range(16.0, 30.0), 2.6, (0.0, 0.0))
        } else {
            (
                rng.range(6.0, 16.0),
                rng.range(1.2, 2.2),
                (rng.range(-0.5, 0.5), rng.range(-0.5, 0.5)),
            )
        };
        let base = if s == 0 {
            (0, 0)
        } else {
            (rng.range(-4.0, 4.0) as i32, rng.range(-4.0, 4.0) as i32)
        };
        for h in 0..height as i32 {
            let taper = 1.0 - h as f32 / height;
            let centre = (
                base.0 + (lean.0 * h as f32) as i32,
                base.1 + (lean.1 * h as f32) as i32,
            );
            w.disc(f, h, centre, (radius * taper.powf(0.7)).max(0.4), material);
        }
    }
}

/// A trunk with a canopy of leaves that has holes, so that light comes through.
fn tree(w: &mut Writer, f: &Frame, rng: &mut Rng) {
    let (bark, leaves) = (id("bark"), id("leaves"));
    let height = rng.range(36.0, 60.0) as i32;
    for h in 0..height {
        let r = if h < 6 { 1.8 } else { 1.0 };
        w.disc(f, h, (0, 0), r, bark);
    }
    let radius = rng.range(11.0, 16.0);
    let crown = height as f32 - radius * 0.3;
    for i in -(radius as i32)..=radius as i32 {
        for j in -(radius as i32)..=radius as i32 {
            for h in -(radius as i32)..=radius as i32 {
                let d = ((i * i + j * j + h * h) as f32).sqrt();
                // A shell, with holes.
                if d <= radius && d >= radius * 0.55 && rng.next() > 0.3 {
                    w.put(f.at(crown as i32 + h, i, j), leaves);
                }
            }
        }
    }
}

/// Fronds that rise from a point and droop.
fn fern(w: &mut Writer, f: &Frame, rng: &mut Rng) {
    let leaves = id("leaves");
    let fronds = 6 + (rng.next() * 3.0) as usize;
    for k in 0..fronds {
        let angle = (k as f32 + rng.next() * 0.5) / fronds as f32 * std::f32::consts::TAU;
        let (ca, sa) = (angle.cos(), angle.sin());
        let length = rng.range(8.0, 13.0);
        let mut t = 0.0;
        while t < length {
            let out = t;
            let up = (t * 0.9 - t * t * 0.075).max(-1.0);
            let (i, j, h) = ((out * ca) as i32, (out * sa) as i32, up as i32);
            w.put(f.at(h, i, j), leaves);
            // A leaf is two Voxels wide across the frond.
            w.put(
                f.at(h, i + sa.signum() as i32, j - ca.signum() as i32),
                leaves,
            );
            t += 0.5;
        }
    }
}

/// A carpet: green with a scatter of flowers.
fn flowers(w: &mut Writer, f: &Frame, rng: &mut Rng) {
    let (flower, green) = (id("flowers"), id("moss"));
    w.disc(f, 0, (0, 0), 5.0, green);
    for _ in 0..30 {
        let (i, j) = (rng.range(-5.0, 5.0) as i32, rng.range(-5.0, 5.0) as i32);
        w.put(f.at(1, i, j), flower);
        if rng.next() > 0.5 {
            w.put(f.at(1, i + 1, j), flower);
        }
    }
}
