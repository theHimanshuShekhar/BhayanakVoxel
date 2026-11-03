//! The ghost Brush: the Voxels that the next click would change, shown at the cursor. They are
//! exactly those of [`sim::plan`], which is also what the click's Edit is applied from, so what the
//! ghost shows is what the Edit does.
//!
//! The ghost is drawn by the overlay's egui pass, as the faces of the Voxels' outline, projected
//! with the camera and tinted by the mode; it needs no pass of the renderer's own. It is seen
//! through the terrain, which is what a preview of a dig wants.

use std::collections::BTreeSet;

use brick_store::VoxelCoord;
use egui::{Color32, Mesh, Pos2};
use net::{EditMode, EditOp};
use render::Camera;
use sim::Change;

/// One face of a Voxel that has no changed Voxel against it: part of the ghost's outline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Face {
    /// The corners, in Voxels, going round the face.
    pub corners: [[i32; 3]; 4],
    /// Which way the face looks: a unit step along one axis.
    pub normal: [i32; 3],
}

/// The Edit that a click would make and what it would change.
#[derive(Clone, Debug)]
pub struct Ghost {
    pub op: EditOp,
    /// What the Edit changes. Drawing needs only the outline; the tests read the list.
    #[cfg_attr(not(test), allow(dead_code))]
    pub changes: Vec<Change>,
    faces: Vec<Face>,
}

impl Ghost {
    /// The ghost of `op`, which changes `changes` (see [`sim::plan`]).
    pub fn new(op: EditOp, changes: Vec<Change>) -> Self {
        let faces = outline(&changes);
        Self { op, changes, faces }
    }

    /// The Voxels the Edit changes.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn voxels(&self) -> impl Iterator<Item = VoxelCoord> + '_ {
        self.changes.iter().map(|c| c.voxel)
    }

    /// The outline of the Voxels.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn faces(&self) -> &[Face] {
        &self.faces
    }

    /// Paints the faces that look at `camera`, on a screen `size` points across, under the panels.
    pub fn paint(&self, ctx: &egui::Context, camera: &Camera) {
        let rect = ctx.content_rect();
        let mesh = self.mesh(camera, rect.size());
        if !mesh.is_empty() {
            let mut mesh = mesh;
            mesh.translate(rect.min.to_vec2());
            ctx.layer_painter(egui::LayerId::background())
                .add(egui::Shape::mesh(mesh));
        }
    }

    /// The faces that look at the camera as triangles on a screen `size` points across, from its
    /// top left corner.
    pub fn mesh(&self, camera: &Camera, size: egui::Vec2) -> Mesh {
        let colour = tint(self.op.mode);
        let aspect = f64::from(size.x) / f64::from(size.y);
        let mut mesh = Mesh::default();
        for face in &self.faces {
            if !looks_at(face, camera.position) {
                continue;
            }
            let screen: Option<Vec<Pos2>> = face
                .corners
                .iter()
                .map(|&c| project(camera, aspect, c.map(f64::from)))
                .map(|p| p.map(|[x, y]| Pos2::new(x * size.x, y * size.y)))
                .collect();
            let Some(corners) = screen else { continue };
            let first = mesh.vertices.len() as u32;
            for corner in corners {
                mesh.colored_vertex(corner, colour);
            }
            mesh.add_triangle(first, first + 1, first + 2);
            mesh.add_triangle(first, first + 2, first + 3);
        }
        mesh
    }
}

/// How the ghost is tinted: red for what is taken away, green for what is added, and blue and
/// amber for the two that do both.
fn tint(mode: EditMode) -> Color32 {
    let (r, g, b) = match mode {
        EditMode::Dig => (230, 50, 50),
        EditMode::Place => (60, 210, 90),
        EditMode::Smooth => (70, 130, 240),
        EditMode::Flatten => (240, 170, 40),
    };
    Color32::from_rgba_unmultiplied(r, g, b, 70)
}

/// The faces of the changed Voxels that no changed Voxel is against.
fn outline(changes: &[Change]) -> Vec<Face> {
    let set: BTreeSet<[i32; 3]> = changes.iter().map(|c| c.voxel.0).collect();
    let mut faces = Vec::new();
    for &voxel in &set {
        for axis in 0..3 {
            for sign in [-1, 1] {
                let mut normal = [0; 3];
                normal[axis] = sign;
                let beside: [i32; 3] = std::array::from_fn(|a| voxel[a] + normal[a]);
                if set.contains(&beside) {
                    continue;
                }
                // The plane of the face, and the two axes across it.
                let plane = voxel[axis] + i32::from(sign > 0);
                let (u, w) = ((axis + 1) % 3, (axis + 2) % 3);
                let corner = |du: i32, dw: i32| {
                    let mut c = voxel;
                    c[axis] = plane;
                    c[u] += du;
                    c[w] += dw;
                    c
                };
                faces.push(Face {
                    corners: [corner(0, 0), corner(1, 0), corner(1, 1), corner(0, 1)],
                    normal,
                });
            }
        }
    }
    faces
}

/// Whether `face` is on the camera's side of its own plane.
fn looks_at(face: &Face, eye: [f64; 3]) -> bool {
    let axis = (0..3).find(|&a| face.normal[a] != 0).expect("a unit step");
    let plane = f64::from(face.corners[0][axis]);
    (eye[axis] - plane) * f64::from(face.normal[axis]) > 0.0
}

/// Nearer than this, in Voxels, a point is not drawn.
const NEAR: f64 = 0.5;

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
    use brick_store::Material;

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
        Ghost::new(EditOp::dig_sphere(VoxelCoord([0; 3]), 1), changes)
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
    }

    #[test]
    fn every_face_lies_on_the_voxel_it_belongs_to_and_looks_away_from_it() {
        let ghost = ghost(vec![change([0, 0, 0])]);
        for face in ghost.faces() {
            let axis = (0..3).find(|&a| face.normal[a] != 0).unwrap();
            let plane = if face.normal[axis] > 0 { 1 } else { 0 };
            assert!(face.corners.iter().all(|c| c[axis] == plane), "{face:?}");
            // The corners are the unit square, each next to the one before.
            for i in 0..4 {
                let (a, b) = (face.corners[i], face.corners[(i + 1) % 4]);
                let apart: i32 = (0..3).map(|k| (a[k] - b[k]).abs()).sum();
                assert_eq!(apart, 1, "{face:?}");
            }
        }
    }

    #[test]
    fn a_ghost_lists_the_voxels_it_was_given_and_nothing_else() {
        let changes = cube(2);
        let ghost = ghost(changes.clone());
        assert_eq!(
            ghost.voxels().collect::<Vec<_>>(),
            changes.iter().map(|c| c.voxel).collect::<Vec<_>>()
        );
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
    fn the_ghost_is_tinted_by_the_mode() {
        let tints: BTreeSet<_> = [
            EditMode::Dig,
            EditMode::Place,
            EditMode::Smooth,
            EditMode::Flatten,
        ]
        .map(|m| tint(m).to_array())
        .into_iter()
        .collect();
        assert_eq!(tints.len(), 4);
    }
}
