//! PROTOTYPE (#148): the shading A/B. The renderer shades with the face's normal; the art
//! direction (#135, item 8) wants natural Materials shaded with smooth normals computed from the
//! neighbouring occupancy. The GPU casts the rays (`Offscreen::trace_rays`) and this shades what
//! they hit on the CPU, once with the face normal and once with the smooth one, lit the same way:
//! the sun (with shadows) and a sky term, so the two differ in the normal alone.

use brick_store::{BrickStore, VoxelCoord};
use materials::{Category, MaterialTable};
use render::reference::Ray;
use render::{Camera, Image, Offscreen, Sun};

/// Voxels, as Voxel offsets from the surface Voxel in front of the hit, that the smoothing looks
/// at: a cube of 5 (10 cm across).
const WINDOW: i32 = 2;
/// Hits further than this (30 m) keep their face normals: past it the bricks are not the held
/// level 0 ring, and a Voxel is under a pixel anyway.
const SMOOTH_UP_TO: f32 = 600.0;

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn unit(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt().max(1e-12);
    a.map(|c| c / l)
}

/// Whether `at` is solid, without generating anything: a held brick says, and one that is not
/// held is solid only if the Planet proves it solid all the way down (buried), else air.
fn solid(store: &BrickStore, at: VoxelCoord) -> bool {
    let brick = at.brick();
    match store.brick(brick) {
        Some(b) => b.get(at.local()).is_some(),
        None => store.source_solid_below(0, brick),
    }
}

/// The surface normal at the hit Voxel `voxel`, found by the face `face`: the direction away from
/// the solid Voxels round the one in front of the face, each weighted by how near it is. On a
/// stair-step slope that is the slope's normal; on a flat face, the face's.
pub fn smooth_normal(store: &BrickStore, voxel: [i32; 3], face: [i32; 3]) -> [f32; 3] {
    let front: [i32; 3] = std::array::from_fn(|a| voxel[a] + face[a]);
    let mut sum = [0.0f32; 3];
    for dx in -WINDOW..=WINDOW {
        for dy in -WINDOW..=WINDOW {
            for dz in -WINDOW..=WINDOW {
                let at = VoxelCoord([front[0] + dx, front[1] + dy, front[2] + dz]);
                if solid(store, at) {
                    let d = [dx as f32, dy as f32, dz as f32];
                    let w = (-dot(d, d) / (2.0 * 1.4 * 1.4)).exp();
                    for a in 0..3 {
                        sum[a] += d[a] * w;
                    }
                }
            }
        }
    }
    let face = face.map(|c| c as f32);
    if dot(sum, sum) < 1e-6 {
        return face;
    }
    // Away from the solid, kept to the face's side of the surface.
    let away = unit(sum.map(|c| -c));
    unit(std::array::from_fn(|a| away[a] + 0.25 * face[a]))
}

fn rays(camera: &Camera, (w, h): (u32, u32)) -> Vec<Ray> {
    let u = camera.uniform();
    let tan_half = u.params[0];
    let aspect = w as f32 / h as f32;
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let ndc = [
                (x as f32 + 0.5) / w as f32 * 2.0 - 1.0,
                (y as f32 + 0.5) / h as f32 * 2.0 - 1.0,
            ];
            let dir = unit(std::array::from_fn(|a| {
                u.forward[a] + u.right[a] * ndc[0] * aspect * tan_half - u.up[a] * ndc[1] * tan_half
            }));
            out.push(Ray {
                cell: [u.cell[0], u.cell[1], u.cell[2]],
                origin: [u.origin[0], u.origin[1], u.origin[2]],
                dir,
            });
        }
    }
    out
}

/// The two images of `camera`, `size` pixels: shaded with face normals, and with smooth ones.
/// `up` is the local up, for the sky.
pub fn ab(
    gpu: &Offscreen,
    store: &BrickStore,
    camera: &Camera,
    size: (u32, u32),
    sun: &Sun,
    up: [f32; 3],
) -> (Image, Image) {
    let table = MaterialTable::built_in();
    let primary = rays(camera, size);
    let hits = gpu.trace_rays(&primary);
    let sun_dir = sun.direction();
    // The shadow rays: from the empty Voxel in front of each hit that faces the sun.
    let mut shadow_for = Vec::new();
    let mut shadow_rays = Vec::new();
    for (i, hit) in hits.iter().enumerate() {
        let Some(hit) = hit else { continue };
        let n = hit.normal.map(|c| c as f32);
        if dot(n, sun_dir) > 0.0 {
            shadow_for.push(i);
            shadow_rays.push(Ray {
                cell: std::array::from_fn(|a| hit.voxel[a] + hit.normal[a]),
                origin: std::array::from_fn(|a| {
                    if hit.normal[a] != 0 {
                        0.5 - n[a] * (0.5 - 1e-3)
                    } else {
                        0.5
                    }
                }),
                dir: sun_dir,
            });
        }
    }
    let blocked = gpu.trace_rays(&shadow_rays);
    let mut lit = vec![true; hits.len()];
    for (k, i) in shadow_for.iter().enumerate() {
        lit[*i] = blocked[k].is_none();
    }
    let shade = |smooth: bool| -> Image {
        let mut rgba = Vec::with_capacity(hits.len() * 4);
        for (i, hit) in hits.iter().enumerate() {
            let colour: [f32; 3] = match hit {
                None => {
                    let t = dot(primary[i].dir, up).clamp(0.0, 1.0);
                    std::array::from_fn(|a| {
                        [0.75, 0.82, 0.92][a] * (1.0 - t) + [0.25, 0.45, 0.85][a] * t
                    })
                }
                Some(hit) => {
                    let def = table.get(brick_store::Material(hit.material));
                    let albedo = def.map_or([1.0, 0.0, 1.0], |d| d.colour);
                    let natural = def.is_some_and(|d| d.category != Category::Building);
                    let n = if smooth && natural && hit.t < SMOOTH_UP_TO {
                        smooth_normal(store, hit.voxel, hit.normal)
                    } else {
                        hit.normal.map(|c| c as f32)
                    };
                    let sun_light = if lit[i] {
                        dot(n, sun_dir).max(0.0)
                    } else {
                        0.0
                    };
                    let sky_light = 0.30 * (0.55 + 0.45 * dot(n, up));
                    let emit =
                        def.map_or([0.0; 3], |d| d.emissive.map(|c| c * d.emissive_intensity));
                    std::array::from_fn(|a| {
                        albedo[a] * (1.6 * sun_light * [1.0, 0.94, 0.85][a] + sky_light) + emit[a]
                    })
                }
            };
            for c in colour {
                rgba.push((c.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0 + 0.5) as u8);
            }
            rgba.push(255);
        }
        Image {
            width: size.0,
            height: size.1,
            rgba,
        }
    };
    (shade(false), shade(true))
}

#[cfg(test)]
mod tests {
    use brick_store::Material;

    use super::*;

    /// A store with solid below `y = 0` along `slope`: `y < slope * x`.
    fn ground(slope: f32) -> BrickStore {
        let mut store = BrickStore::new();
        for x in -12..12 {
            for y in -12..12 {
                for z in -12..12 {
                    if (y as f32) < slope * x as f32 {
                        store.set(VoxelCoord([x, y, z]), Some(Material(1)));
                    }
                }
            }
        }
        store
    }

    #[test]
    fn a_flat_floor_is_smooth_the_same_as_its_face() {
        let store = ground(0.0);
        let n = smooth_normal(&store, [0, -1, 0], [0, 1, 0]);
        assert!(n[1] > 0.99, "{n:?}");
    }

    #[test]
    fn a_stair_step_slope_has_a_tilted_normal_where_the_face_is_up() {
        let store = ground(0.5);
        let n = smooth_normal(&store, [0, -1, 0], [0, 1, 0]);
        // Rises along +x, so the surface faces -x as well as up.
        assert!(n[0] < -0.2 && n[1] > 0.5, "{n:?}");
    }

    #[test]
    fn air_all_round_keeps_the_face_normal() {
        let store = BrickStore::new();
        assert_eq!(smooth_normal(&store, [0, 0, 0], [0, 1, 0]), [0.0, 1.0, 0.0]);
    }
}
