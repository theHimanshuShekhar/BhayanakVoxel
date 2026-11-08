//! The ghost Brush: the Voxels that the next click would change, shown at the cursor. They are
//! exactly those of [`sim::plan_bricks`], which is also what the click's Edit is applied from, so
//! what the ghost shows is what the Edit does.
//!
//! The ghost is drawn by the overlay's egui pass, as the faces of the Voxels' outline, projected
//! with the camera and tinted by the mode; it needs no pass of the renderer's own. It is seen
//! through the terrain, which is what a preview of a dig wants. Only the outline is kept: the
//! Voxels are not. The outline is found brick by brick, on the masks of the changed Voxels, a
//! word of them at a time, so that a Brush of half a million Voxels costs a millisecond or two
//! of the frame thread and not fifteen.

use std::collections::HashMap;

use brick_store::{BRICK_SIZE, BrickCoord, VoxelMask};
use egui::{Color32, Mesh, Pos2};
use materials::MaterialTable;
use net::{EditMode, EditOp};
use render::Camera;
use render::overlay::swatch;
#[cfg(test)]
use sim::Change;
use sim::Plan;

/// One face of a Voxel that has no changed Voxel against it: part of the ghost's outline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Face {
    pub voxel: [i32; 3],
    /// The axis the face is square to.
    pub axis: usize,
    /// Whether it is on the Voxel's far side along that axis, and looks that way.
    pub positive: bool,
}

impl Face {
    /// The coordinate of the face's plane along its axis.
    fn plane(&self) -> i32 {
        self.voxel[self.axis] + i32::from(self.positive)
    }

    /// The four corners, in Voxels, going round the face.
    pub fn corners(&self) -> [[i32; 3]; 4] {
        let (u, w) = ((self.axis + 1) % 3, (self.axis + 2) % 3);
        let corner = |du: i32, dw: i32| {
            let mut c = self.voxel;
            c[self.axis] = self.plane();
            c[u] += du;
            c[w] += dw;
            c
        };
        [corner(0, 0), corner(1, 0), corner(1, 1), corner(0, 1)]
    }
}

/// The Edit that a click would make, its outline, and what it is waiting on.
#[derive(Clone, Debug)]
pub struct Ghost {
    pub op: EditOp,
    /// The Edit is one that the server will turn down (the client counts its allowance): shown grey.
    pub blocked: bool,
    /// A brick that the Edit reads has changed since the outline was made, so it is to be made
    /// again.
    pub dirty: bool,
    /// How many Voxels the Edit changes, which the server holds to the player's allowance.
    pub changed: usize,
    faces: Vec<Face>,
    /// The first and last brick of the Voxels that the Edit reads, a Voxel beyond what it
    /// changes included.
    reads: Option<([i32; 3], [i32; 3])>,
    /// What the Edit changes, for the tests to compare with what the Edit did.
    #[cfg(test)]
    plan: Plan,
}

impl Ghost {
    /// The ghost of `op`, which changes what `plan` says (see [`sim::plan_bricks`]).
    pub fn new(op: EditOp, plan: &Plan) -> Self {
        let reads = sim::examined_box(&op).map(|(first, last)| {
            let brick = |v: [i64; 3]| -> [i32; 3] {
                std::array::from_fn(|a| {
                    let c = v[a].clamp(i64::from(i32::MIN), i64::from(i32::MAX));
                    (c as i32).div_euclid(BRICK_SIZE)
                })
            };
            // The Voxels beside the box, which a Smooth reads.
            (brick(first.map(|c| c - 1)), brick(last.map(|c| c + 1)))
        });
        Self {
            op,
            blocked: false,
            dirty: false,
            changed: plan.len(),
            faces: outline(plan),
            reads,
            #[cfg(test)]
            plan: plan.clone(),
        }
    }

    /// Whether any of the bricks `changed` is one the Edit reads, so that what it would change
    /// may not be what the ghost shows.
    pub fn touches(&self, changed: &[BrickCoord]) -> bool {
        let Some((first, last)) = self.reads else {
            return false;
        };
        changed
            .iter()
            .any(|b| (0..3).all(|a| (first[a]..=last[a]).contains(&b.0[a])))
    }

    /// The Voxels the Edit changes, one by one.
    #[cfg(test)]
    pub fn changes(&self) -> Vec<Change> {
        self.plan.changes().collect()
    }

    /// The outline of the Voxels.
    #[cfg(test)]
    pub fn faces(&self) -> &[Face] {
        &self.faces
    }

    /// Paints the faces that look at `camera`, on the screen, under the panels.
    pub fn paint(&self, ctx: &egui::Context, camera: &Camera) {
        let rect = ctx.content_rect();
        let mut mesh = self.mesh(camera, rect.size());
        if !mesh.is_empty() {
            mesh.translate(rect.min.to_vec2());
            ctx.layer_painter(egui::LayerId::background())
                .add(egui::Shape::mesh(mesh));
        }
    }

    /// The faces that look at the camera as triangles on a screen `size` points across, from its
    /// top left corner.
    pub fn mesh(&self, camera: &Camera, size: egui::Vec2) -> Mesh {
        let colour = tint(&self.op, self.blocked);
        let aspect = f64::from(size.x) / f64::from(size.y);
        let mut mesh = Mesh::default();
        for face in &self.faces {
            if !looks_at(face, camera.position) {
                continue;
            }
            let mut screen = [Pos2::ZERO; 4];
            let all_seen = face.corners().iter().zip(&mut screen).all(|(&c, out)| {
                project(camera, aspect, c.map(f64::from)).is_some_and(|[x, y]| {
                    *out = Pos2::new(x * size.x, y * size.y);
                    true
                })
            });
            if !all_seen {
                continue;
            }
            let first = mesh.vertices.len() as u32;
            for corner in screen {
                mesh.colored_vertex(corner, colour);
            }
            mesh.add_triangle(first, first + 1, first + 2);
            mesh.add_triangle(first, first + 2, first + 3);
        }
        mesh
    }
}

/// How the ghost is tinted: red for what is taken away, the colour of the Material for what is
/// placed, and blue and amber for the two that do both; grey for an Edit that will not be made.
fn tint(op: &EditOp, blocked: bool) -> Color32 {
    let place = MaterialTable::built_in()
        .get(op.material)
        .map_or(Color32::from_rgb(60, 210, 90), |def| swatch(def.colour));
    let (r, g, b) = match (op.mode, blocked) {
        (_, true) => (150, 150, 150),
        (EditMode::Dig, _) => (230, 50, 50),
        (EditMode::Place, _) => (place.r(), place.g(), place.b()),
        (EditMode::Smooth, _) => (70, 130, 240),
        (EditMode::Flatten, _) => (240, 170, 40),
    };
    Color32::from_rgba_unmultiplied(r, g, b, 70)
}

/// The faces of the changed Voxels that no changed Voxel is against. Brick by brick: a brick's
/// changed Voxels are a [`VoxelMask`], and a Voxel's neighbour is a bit of the mask shifted, or of
/// the mask of the brick beside it, so that a word of 64 Voxels is looked at at once, and a brick
/// that is changed whole with changed bricks on all six sides has no faces and costs no more than
/// looking.
fn outline(plan: &Plan) -> Vec<Face> {
    let masks: HashMap<BrickCoord, VoxelMask> = plan
        .bricks()
        .iter()
        .map(|brick| (brick.coord, brick.mask()))
        .collect();
    let mut faces = Vec::new();
    for brick in plan.bricks() {
        let mask = &masks[&brick.coord];
        if *mask == [u64::MAX; 8]
            && NEIGHBOURS.iter().all(|&(axis, positive)| {
                masks
                    .get(&beside(brick.coord, axis, positive))
                    .is_some_and(|m| *m == [u64::MAX; 8])
            })
        {
            continue;
        }
        let next = |axis: usize, positive: bool| -> VoxelMask {
            masks
                .get(&beside(brick.coord, axis, positive))
                .copied()
                .unwrap_or([0; 8])
        };
        let [west, east, below, above, behind, ahead] = [
            next(0, false),
            next(0, true),
            next(1, false),
            next(1, true),
            next(2, false),
            next(2, true),
        ];
        for layer in 0..8 {
            let word = mask[layer];
            // The neighbour of each Voxel in each direction, as a bit at the Voxel's own place:
            // set where that neighbour is changed.
            let present = [
                // -x: the Voxel at x - 1, which for x = 0 is the brick's west one at x = 7.
                (word << 1 & !COLUMN_0) | (west[layer] >> 7 & COLUMN_0),
                (word >> 1 & !COLUMN_7) | (east[layer] << 7 & COLUMN_7),
                word << 8 | below[layer] >> 56,
                word >> 8 | above[layer] << 56,
                if layer > 0 {
                    mask[layer - 1]
                } else {
                    behind[7]
                },
                if layer < 7 { mask[layer + 1] } else { ahead[0] },
            ];
            for (side, present) in present.into_iter().enumerate() {
                let (axis, positive) = (side / 2, side % 2 == 1);
                let mut bits = word & !present;
                while bits != 0 {
                    let bit = bits.trailing_zeros();
                    bits &= bits - 1;
                    let local = [(bit & 7) as u8, (bit >> 3) as u8, layer as u8];
                    faces.push(Face {
                        voxel: brick.coord.voxel(local).0,
                        axis,
                        positive,
                    });
                }
            }
        }
    }
    faces
}

/// The six sides of a brick: the axis, and whether it is the far side.
const NEIGHBOURS: [(usize, bool); 6] = [
    (0, false),
    (0, true),
    (1, false),
    (1, true),
    (2, false),
    (2, true),
];

/// The bits of a layer's word whose Voxel has x of 0, and of 7.
const COLUMN_0: u64 = 0x0101_0101_0101_0101;
const COLUMN_7: u64 = COLUMN_0 << 7;

/// The brick beside `coord` on the `positive` or the near side of `axis`.
fn beside(coord: BrickCoord, axis: usize, positive: bool) -> BrickCoord {
    let mut next = coord.0;
    next[axis] += if positive { 1 } else { -1 };
    BrickCoord(next)
}

/// The outline of `changes` as it was made before #156, a Voxel at a time, kept as it was for the
/// test that holds [`outline`] to it.
#[cfg(test)]
fn outline_by_voxel(changes: &[Change]) -> Vec<Face> {
    let Some(first) = changes.first() else {
        return Vec::new();
    };
    let (mut low, mut high) = (first.voxel.0, first.voxel.0);
    for change in changes {
        for a in 0..3 {
            low[a] = low[a].min(change.voxel.0[a]);
            high[a] = high[a].max(change.voxel.0[a]);
        }
    }
    let size: [usize; 3] = std::array::from_fn(|a| (high[a] - low[a]) as usize + 3);
    let cell = |v: [i32; 3]| -> usize {
        let at: [usize; 3] = std::array::from_fn(|a| (v[a] - low[a] + 1) as usize);
        at[0] + size[0] * (at[1] + size[1] * at[2])
    };
    let mut marked = vec![false; size[0] * size[1] * size[2]];
    for change in changes {
        marked[cell(change.voxel.0)] = true;
    }
    let mut faces = Vec::new();
    for change in changes {
        let voxel = change.voxel.0;
        for axis in 0..3 {
            for positive in [false, true] {
                let mut beside = voxel;
                beside[axis] += if positive { 1 } else { -1 };
                if !marked[cell(beside)] {
                    faces.push(Face {
                        voxel,
                        axis,
                        positive,
                    });
                }
            }
        }
    }
    faces
}

/// Whether `face` is on the camera's side of its own plane.
fn looks_at(face: &Face, eye: [f64; 3]) -> bool {
    let side = if face.positive { 1.0 } else { -1.0 };
    (eye[face.axis] - f64::from(face.plane())) * side > 0.0
}

/// Nearer than this, in Voxels (5 cm), a point is not drawn.
const NEAR: f64 = 0.05 * brick_store::VOXELS_PER_METRE;

/// Where `point`, in Voxels, is on a screen of this `aspect`: from 0 to 1 across and down from the
/// top left. `None` if it is behind the camera or too near it.
fn project(camera: &Camera, aspect: f64, point: [f64; 3]) -> Option<[f32; 2]> {
    let rel: [f64; 3] = std::array::from_fn(|a| point[a] - camera.position[a]);
    let along = |axis: [f64; 3]| (0..3).map(|a| rel[a] * axis[a]).sum::<f64>();
    let depth = along(camera.forward());
    if depth < NEAR {
        return None;
    }
    let half_height = depth * (camera.fov_y / 2.0).tan();
    let x = along(camera.right()) / (half_height * aspect);
    let y = along(camera.up()) / half_height;
    Some([((x + 1.0) / 2.0) as f32, ((1.0 - y) / 2.0) as f32])
}

#[cfg(test)]
mod tests {
    use super::*;
    use brick_store::{Material, VoxelCoord};
    use std::collections::BTreeSet;

    fn change(voxel: [i32; 3]) -> Change {
        Change {
            voxel: VoxelCoord(voxel),
            to: Some(Material(1)),
        }
    }

    fn cube(side: i32) -> Vec<Change> {
        let mut changes = Vec::new();
        for x in 0..side {
            for y in 0..side {
                for z in 0..side {
                    changes.push(change([x, y, z]));
                }
            }
        }
        changes
    }

    fn ghost(changes: Vec<Change>) -> Ghost {
        Ghost::new(
            EditOp::dig_sphere(VoxelCoord([0; 3]), 1),
            &Plan::from_changes(changes),
        )
    }

    #[test]
    fn the_outline_of_a_voxel_is_its_six_faces_and_of_a_cube_the_faces_of_its_surface() {
        assert_eq!(ghost(vec![change([5, 5, 5])]).faces().len(), 6);
        // 6 sides of 3 x 3 faces; the faces between Voxels are not in it.
        assert_eq!(ghost(cube(3)).faces().len(), 54);
        // A gap in the middle adds the 6 faces of its hole.
        let mut hollow = cube(3);
        hollow.retain(|c| c.voxel != VoxelCoord([1, 1, 1]));
        assert_eq!(ghost(hollow).faces().len(), 60);
        // Voxels apart, and with negative coordinates, are each their own.
        let apart = vec![change([-9, 4, 0]), change([20, -3, 7])];
        assert_eq!(ghost(apart).faces().len(), 12);
        assert!(ghost(Vec::new()).faces().is_empty());
    }

    #[test]
    fn every_face_lies_on_the_voxel_it_belongs_to_and_looks_away_from_it() {
        let ghost = ghost(vec![change([0, 0, 0])]);
        let sides: BTreeSet<_> = ghost.faces().iter().map(|f| (f.axis, f.positive)).collect();
        assert_eq!(sides.len(), 6);
        for face in ghost.faces() {
            let plane = if face.positive { 1 } else { 0 };
            assert!(
                face.corners().iter().all(|c| c[face.axis] == plane),
                "{face:?}"
            );
            // The corners are the unit square, each next to the one before.
            let corners = face.corners();
            for i in 0..4 {
                let (a, b) = (corners[i], corners[(i + 1) % 4]);
                let apart: i32 = (0..3).map(|k| (a[k] - b[k]).abs()).sum();
                assert_eq!(apart, 1, "{face:?}");
            }
        }
    }

    #[test]
    fn a_ghost_in_the_tests_lists_the_voxels_it_was_given() {
        let changes = cube(2);
        let ghost = ghost(changes.clone());
        // In the order of an Edit: by brick, and in the brick by layer, row and column.
        let mut ordered = changes;
        ordered.sort_by_key(|c| {
            (
                c.voxel.brick(),
                c.voxel.local()[2],
                c.voxel.local()[1],
                c.voxel.local()[0],
            )
        });
        assert_eq!(ghost.changes(), ordered);
    }

    #[test]
    fn only_the_faces_looking_at_the_camera_are_drawn_and_a_voxel_ahead_is_in_the_middle() {
        let ghost = ghost(vec![change([0, 0, -10])]);
        // Looking down -z from the origin: the voxel is 9 to 10 ahead, and its +z face is the one
        // that looks back.
        let camera = Camera::looking_at([0.5, 0.5, 0.0], [0.5, 0.5, -10.0]);
        let mesh = ghost.mesh(&camera, egui::vec2(800.0, 600.0));
        assert_eq!(mesh.vertices.len(), 4, "only the near face");
        let middle = mesh
            .vertices
            .iter()
            .map(|v| v.pos)
            .fold(Pos2::ZERO, |sum, p| {
                Pos2::new(sum.x + p.x / 4.0, sum.y + p.y / 4.0)
            });
        assert!(
            (middle.x - 400.0).abs() < 1.0 && (middle.y - 300.0).abs() < 1.0,
            "{middle:?}"
        );
        // Behind the camera, nothing.
        let behind = Camera::looking_at([0.5, 0.5, 0.0], [0.5, 0.5, 10.0]);
        assert!(ghost.mesh(&behind, egui::vec2(800.0, 600.0)).is_empty());
    }

    #[test]
    fn a_placing_ghost_takes_the_colour_of_the_material() {
        let place = |id| EditOp {
            mode: EditMode::Place,
            material: Material(id),
            ..EditOp::dig_sphere(VoxelCoord([0; 3]), 1)
        };
        let table = MaterialTable::built_in();
        let (a, b) = (table.defs()[0].id, table.by_name("grassy soil").unwrap().id);
        assert_ne!(tint(&place(a), false), tint(&place(b), false));
        assert_eq!(
            tint(&place(a), false),
            Color32::from_rgba_unmultiplied(
                swatch(table.defs()[0].colour).r(),
                swatch(table.defs()[0].colour).g(),
                swatch(table.defs()[0].colour).b(),
                70
            )
        );
    }

    #[test]
    fn the_ghost_is_tinted_by_the_mode_and_grey_when_it_will_not_be_made() {
        let modes = [
            EditMode::Dig,
            EditMode::Place,
            EditMode::Smooth,
            EditMode::Flatten,
        ];
        let op = |mode| EditOp {
            mode,
            material: Material(1),
            ..EditOp::dig_sphere(VoxelCoord([0; 3]), 1)
        };
        let tints: BTreeSet<_> = modes
            .map(|m| tint(&op(m), false).to_array())
            .into_iter()
            .collect();
        assert_eq!(tints.len(), 4);
        let greys: BTreeSet<_> = modes
            .map(|m| tint(&op(m), true).to_array())
            .into_iter()
            .collect();
        assert_eq!(greys.len(), 1);
        assert!(!tints.contains(greys.iter().next().unwrap()));
    }

    #[test]
    fn a_ghost_is_made_again_only_for_a_change_to_a_brick_it_reads() {
        // A sphere of 10 about the Voxel 1000, 0, 0 reads the bricks that its box and a Voxel more
        // are in: 989 to 1011 along x, so bricks 123 to 126, and -11 to 11 in y and z, bricks -2
        // to 1.
        let ghost = ghost_of(EditOp::dig_sphere(VoxelCoord([1000, 0, 0]), 10));
        assert!(ghost.touches(&[BrickCoord([125, 0, 0])]));
        assert!(ghost.touches(&[BrickCoord([123, -2, 1])]));
        assert!(!ghost.touches(&[BrickCoord([122, 0, 0])]));
        assert!(!ghost.touches(&[BrickCoord([127, 0, 0])]));
        assert!(!ghost.touches(&[BrickCoord([125, 2, 0])]));
        assert!(!ghost.touches(&[]));
        assert!(ghost.touches(&[BrickCoord([0, 0, 0]), BrickCoord([124, 0, -1])]));
        // An op with no box reads nothing.
        let off = ghost_of(EditOp::dig_sphere(VoxelCoord([i32::MAX, 0, 0]), 10));
        assert!(!off.touches(&[BrickCoord([125, 0, 0])]));
    }

    #[test]
    fn the_outline_found_brick_by_brick_is_the_one_found_voxel_by_voxel_face_for_face() {
        use brick_store::{Brick, BrickCoord, BrickStore};
        use net::{Alignment, Brush, Shape};

        // Rock with holes in it, and soil on top, over a few bricks, so that a Brush meets whole
        // bricks, bricks with a surface, and air; and its edges are in the middle of bricks and on
        // their borders.
        let mut store = BrickStore::new();
        for x in -4..4 {
            for y in -4..2 {
                for z in -4..4 {
                    store.insert(BrickCoord([x, y, z]), Brick::solid(Material(3)));
                }
            }
        }
        for i in 0..400i32 {
            let v = [(i * 7) % 61 - 30, (i * 5) % 29 - 14, (i * 11) % 59 - 29];
            store.set(VoxelCoord(v), None);
            store.set(VoxelCoord([v[0], 12 + i % 4, v[2]]), Some(Material(5)));
        }
        let mut checked = 0;
        for mode in [
            EditMode::Dig,
            EditMode::Place,
            EditMode::Smooth,
            EditMode::Flatten,
        ] {
            for shape in [Shape::Sphere, Shape::Cube, Shape::Cylinder] {
                for (centre, radius, alignment, rotation) in [
                    ([0, 0, 0], 6, Alignment::World, 0),
                    ([3, -9, 5], 17, Alignment::World, 0),
                    ([-8, 6, 2], 19, Alignment::Local, 1),
                    ([7, -7, -7], 12, Alignment::Local, 0),
                    ([0, 8, 0], 30, Alignment::World, 0),
                ] {
                    if matches!(mode, EditMode::Smooth | EditMode::Flatten) && radius > 20 {
                        continue;
                    }
                    let op = EditOp {
                        mode,
                        brush: Brush { shape, radius },
                        centre: VoxelCoord(centre),
                        material: Material(35),
                        alignment,
                        rotation,
                    };
                    let plan = sim::plan_bricks(&store, &op);
                    let by_brick = outline(&plan);
                    let changes: Vec<_> = plan.changes().collect();
                    // The same faces, in whatever order, and each once.
                    let faces = |faces: &[Face]| -> BTreeSet<_> {
                        faces
                            .iter()
                            .map(|f| (f.voxel, f.axis, f.positive))
                            .collect()
                    };
                    let old = outline_by_voxel(&changes);
                    assert_eq!(by_brick.len(), old.len(), "{op:?}");
                    assert_eq!(faces(&by_brick), faces(&old), "{op:?}");
                    checked += by_brick.len();
                }
            }
        }
        assert!(checked > 50_000, "{checked}");
    }

    fn ghost_of(op: EditOp) -> Ghost {
        Ghost::new(op, &Plan::default())
    }

    /// Measures what a ghost of the biggest Brush costs on the frame thread: the Edit worked out,
    /// the outline made, and the faces projected. Run it in release, with
    /// `cargo test --release -p client ghost_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement"]
    fn ghost_cost_of_the_biggest_brushes() {
        use brick_store::{Brick, BrickCoord, BrickStore};
        use net::{Alignment, Brush, Shape};
        use std::time::Instant;

        let mut store = BrickStore::new();
        for x in 180..192 {
            for y in -8..8 {
                for z in -8..8 {
                    store.insert(BrickCoord([x, y, z]), Brick::solid(Material(1)));
                }
            }
        }
        let camera = Camera::looking_at([1530.5, 10.5, 10.5], [1500.0, 3.0, 5.0]);
        let mut worst = std::time::Duration::ZERO;
        for centre in [[1500, 0, 0], [1500, 3, 5], [1100, 700, 700]] {
            for mode in [
                EditMode::Dig,
                EditMode::Place,
                EditMode::Smooth,
                EditMode::Flatten,
            ] {
                for shape in [Shape::Sphere, Shape::Cube, Shape::Cylinder] {
                    let op = EditOp {
                        mode,
                        brush: Brush { shape, radius: 20 },
                        centre: VoxelCoord(centre),
                        material: Material(35),
                        alignment: Alignment::Local,
                        rotation: 0,
                    };
                    let started = Instant::now();
                    let changes = sim::plan_bricks(&store, &op);
                    let planned = started.elapsed();
                    let made = Instant::now();
                    let ghost = Ghost::new(op, &changes);
                    let outlined = made.elapsed();
                    let drawn = Instant::now();
                    let mesh = ghost.mesh(&camera, egui::vec2(1920.0, 1080.0));
                    let projected = drawn.elapsed();
                    // On a frame that makes it again, all three; on one that does not, the last.
                    worst = worst.max(planned + outlined + projected);
                    println!(
                        "{centre:?} {mode:?} {shape:?}: plan {planned:?}, outline {outlined:?} ({} faces), \
                         project {projected:?} ({} vertices)",
                        ghost.faces().len(),
                        mesh.vertices.len()
                    );
                }
            }
        }
        println!("worst frame, making it again: {worst:?}");
    }
}
