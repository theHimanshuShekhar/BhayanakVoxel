//! The Brush the player has picked: what the mouse applies, and the keys that change it.

use brick_store::{Material, VoxelCoord};
use materials::MaterialTable;
use net::{Alignment, Brush, EditMode, EditOp, Shape};
use sim::{MAX_BRUSH_RADIUS, MAX_SMOOTH_RADIUS};
use winit::keyboard::KeyCode;

/// The radius the client starts with: 1 m at 10 cm Voxels.
pub(crate) const DEFAULT_RADIUS: u16 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tool {
    pub mode: EditMode,
    pub shape: Shape,
    pub radius: u16,
    /// What Place and Flatten fill with.
    pub material: Material,
    pub alignment: Alignment,
}

impl Default for Tool {
    fn default() -> Self {
        Self {
            mode: EditMode::Dig,
            shape: Shape::Sphere,
            radius: DEFAULT_RADIUS,
            material: Material(MaterialTable::built_in().defs()[0].id),
            alignment: Alignment::Local,
        }
    }
}

impl Tool {
    /// The Edit that this tool makes at `centre`, with the Brush turned `rotation` steps of 45
    /// degrees about up (see [`rotation_for`]).
    pub fn op(&self, centre: VoxelCoord, rotation: u8) -> EditOp {
        let fills = matches!(self.mode, EditMode::Place | EditMode::Flatten);
        EditOp {
            mode: self.mode,
            brush: Brush {
                shape: self.shape,
                radius: self.radius,
            },
            centre,
            // The other modes ignore it, and an op is the same whatever was picked last.
            material: if fills { self.material } else { Material(0) },
            alignment: self.alignment,
            rotation,
        }
    }

    /// Changes the tool for `key`, if it is one of the tool's. `1` to `4` pick the mode, `B` the
    /// next shape, `[` and `]` a smaller or bigger Brush (a Voxel at the least, and the 2 m radius
    /// that the server allows at the most), `-` and `=` the previous or next Material, and `G`
    /// switches between a Brush on the local up and one on the grid. Returns whether it was.
    pub fn key(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Digit1 => self.mode = EditMode::Dig,
            KeyCode::Digit2 => self.mode = EditMode::Place,
            KeyCode::Digit3 => self.mode = EditMode::Smooth,
            KeyCode::Digit4 => self.mode = EditMode::Flatten,
            KeyCode::KeyB => {
                self.shape = match self.shape {
                    Shape::Sphere => Shape::Cube,
                    Shape::Cube => Shape::Cylinder,
                    Shape::Cylinder => Shape::Sphere,
                }
            }
            KeyCode::BracketLeft => self.radius = self.radius.saturating_sub(1),
            KeyCode::BracketRight => self.radius += 1,
            KeyCode::Minus => self.step_material(false),
            KeyCode::Equal => self.step_material(true),
            KeyCode::KeyG => {
                self.alignment = match self.alignment {
                    Alignment::Local => Alignment::World,
                    Alignment::World => Alignment::Local,
                }
            }
            _ => return false,
        }
        // A Smooth or a Flatten is a smaller Brush than a Dig or a Place may be.
        self.radius = self.radius.min(self.largest());
        true
    }

    /// The biggest radius that the server allows this tool's mode.
    fn largest(&self) -> u16 {
        match self.mode {
            EditMode::Smooth | EditMode::Flatten => MAX_SMOOTH_RADIUS,
            EditMode::Dig | EditMode::Place => MAX_BRUSH_RADIUS,
        }
    }

    /// The next Material of the table, or the previous, wrapping round.
    fn step_material(&mut self, forward: bool) {
        let defs = MaterialTable::built_in().defs();
        let at = defs
            .iter()
            .position(|d| d.id == self.material.0)
            .unwrap_or(0);
        let next = if forward {
            (at + 1) % defs.len()
        } else {
            (at + defs.len() - 1) % defs.len()
        };
        self.material = Material(defs[next].id);
    }

    /// What the tool is, for the window title.
    pub fn describe(&self) -> String {
        let mode = match self.mode {
            EditMode::Dig => "dig",
            EditMode::Place => "place",
            EditMode::Smooth => "smooth",
            EditMode::Flatten => "flatten",
        };
        let shape = match self.shape {
            Shape::Sphere => "sphere",
            Shape::Cube => "cube",
            Shape::Cylinder => "cylinder",
        };
        let fill = match (self.mode, MaterialTable::built_in().get(self.material)) {
            (EditMode::Place | EditMode::Flatten, Some(def)) => format!(", {}", def.name),
            _ => String::new(),
        };
        let grid = match self.alignment {
            Alignment::Local => "local up",
            Alignment::World => "grid",
        };
        format!(
            "{mode} {shape} {} cm{fill}, {grid}",
            (u32::from(self.radius) * 2 + 1) * 10
        )
    }
}

/// The length of the part of the view's direction across up, below which the view is looking along
/// up: the sine of about 11 degrees.
const LOOKING_ALONG_UP: f64 = 0.2;

/// How many steps of 45 degrees, 0 to 7, to turn a Brush at `centre` about its up axis so that its
/// sides face `heading` as nearly as they can: the direction in the Planet frame that the camera
/// looks, measured round from the Brush's first horizontal axis ([`sim::axes`]) towards its
/// second. A Brush on the grid is not turned.
pub fn rotation_for(centre: VoxelCoord, alignment: Alignment, heading: [f64; 3]) -> u8 {
    if alignment == Alignment::World {
        return 0;
    }
    let [h0, h1, _] = sim::axes(centre, alignment);
    let along = |axis: [i64; 3]| {
        let axis = axis.map(|c| c as f64);
        let length = axis.iter().map(|c| c * c).sum::<f64>().sqrt();
        (0..3).map(|a| heading[a] * axis[a] / length).sum::<f64>()
    };
    let (x, y) = (along(h0), along(h1));
    // Looking along up (within about 11 degrees of it) says nothing of which way the sides
    // should face, and the least tilt of the view would turn the Brush: it is not turned.
    let length = heading.iter().map(|c| c * c).sum::<f64>().sqrt();
    if x.hypot(y) < LOOKING_ALONG_UP * length {
        return 0;
    }
    let eighths = (y.atan2(x) / std::f64::consts::FRAC_PI_4).round();
    // A heading that is NaN turns it by nothing.
    (eighths as i32).rem_euclid(8) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tool_starts_as_the_1_m_dig_sphere() {
        let tool = Tool::default();
        assert_eq!(
            tool.op(VoxelCoord([1, 2, 3]), 0),
            EditOp::dig_sphere(VoxelCoord([1, 2, 3]), 10)
        );
    }

    #[test]
    fn the_number_keys_pick_the_mode_and_b_cycles_the_shape() {
        let mut tool = Tool::default();
        for (key, mode) in [
            (KeyCode::Digit2, EditMode::Place),
            (KeyCode::Digit3, EditMode::Smooth),
            (KeyCode::Digit4, EditMode::Flatten),
            (KeyCode::Digit1, EditMode::Dig),
        ] {
            assert!(tool.key(key));
            assert_eq!(tool.mode, mode);
        }
        let shapes: Vec<_> = (0..4)
            .map(|_| {
                tool.key(KeyCode::KeyB);
                tool.shape
            })
            .collect();
        assert_eq!(
            shapes,
            [Shape::Cube, Shape::Cylinder, Shape::Sphere, Shape::Cube]
        );
        assert!(!tool.key(KeyCode::KeyW), "W is the character's");
    }

    #[test]
    fn the_brackets_size_the_brush_from_a_voxel_to_2_m() {
        let mut tool = Tool::default();
        for _ in 0..30 {
            tool.key(KeyCode::BracketLeft);
        }
        assert_eq!(tool.radius, 0);
        for _ in 0..30 {
            tool.key(KeyCode::BracketRight);
        }
        assert_eq!(tool.radius, MAX_BRUSH_RADIUS);
        assert!(sim::validate(&tool.op(VoxelCoord([0; 3]), 0)).is_ok());
    }

    #[test]
    fn a_smooth_or_a_flatten_is_held_to_radius_10_and_a_dig_or_a_place_to_20() {
        let mut tool = Tool::default();
        for _ in 0..30 {
            tool.key(KeyCode::BracketRight);
        }
        assert_eq!(tool.radius, MAX_BRUSH_RADIUS);
        // Picking Smooth takes it down to what the server allows, and it does not go up again.
        tool.key(KeyCode::Digit3);
        assert_eq!(tool.radius, MAX_SMOOTH_RADIUS);
        tool.key(KeyCode::BracketRight);
        assert_eq!(tool.radius, MAX_SMOOTH_RADIUS);
        tool.key(KeyCode::Digit4);
        assert_eq!(tool.radius, MAX_SMOOTH_RADIUS);
        tool.key(KeyCode::Digit2);
        tool.key(KeyCode::BracketRight);
        assert_eq!(tool.radius, MAX_SMOOTH_RADIUS + 1);
        for mode in [
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
        ] {
            tool.key(mode);
            assert_eq!(sim::validate(&tool.op(VoxelCoord([0; 3]), 0)), Ok(()));
        }
    }

    #[test]
    fn the_material_keys_walk_the_table_and_wrap() {
        let table = MaterialTable::built_in();
        let (first, last) = (table.defs()[0].id, table.defs().last().unwrap().id);
        let mut tool = Tool::default();
        tool.key(KeyCode::Digit2);
        tool.key(KeyCode::Minus);
        assert_eq!(tool.material, Material(last), "wraps back to the last");
        tool.key(KeyCode::Equal);
        assert_eq!(tool.material, Material(first));
        tool.key(KeyCode::Equal);
        assert_eq!(tool.material, Material(table.defs()[1].id));
        // Every Material it offers is one the server accepts.
        let op = tool.op(VoxelCoord([0; 3]), 0);
        assert_eq!(sim::validate(&op), Ok(()));
        assert_eq!(op.material, tool.material);
    }

    #[test]
    fn only_the_modes_that_fill_carry_a_material_and_g_switches_to_the_grid() {
        let mut tool = Tool::default();
        tool.key(KeyCode::Equal);
        assert_eq!(tool.op(VoxelCoord([0; 3]), 0).material, Material(0), "dig");
        tool.key(KeyCode::Digit4);
        assert_eq!(tool.op(VoxelCoord([0; 3]), 0).material, tool.material);
        assert_eq!(tool.op(VoxelCoord([0; 3]), 0).alignment, Alignment::Local);
        tool.key(KeyCode::KeyG);
        assert_eq!(tool.op(VoxelCoord([0; 3]), 0).alignment, Alignment::World);
        tool.key(KeyCode::KeyG);
        assert_eq!(tool.alignment, Alignment::Local);
    }

    #[test]
    fn the_title_says_what_the_tool_does() {
        let mut tool = Tool::default();
        assert_eq!(tool.describe(), "dig sphere 210 cm, local up");
        tool.key(KeyCode::Digit2);
        tool.key(KeyCode::KeyB);
        tool.key(KeyCode::KeyG);
        assert_eq!(tool.describe(), "place cube 210 cm, rock, grid");
    }

    #[test]
    fn the_brush_turns_to_face_the_way_the_camera_looks_in_steps_of_an_eighth() {
        // At (1000, 0, 0) the horizontal axes are +z and -y (see sim::axes), up is +x.
        let centre = VoxelCoord([1000, 0, 0]);
        let turn = |heading: [f64; 3]| rotation_for(centre, Alignment::Local, heading);
        let (z, y) = ([0.0, 0.0, 1.0], [0.0, -1.0, 0.0]);
        let between = |a: [f64; 3], b: [f64; 3], t: f64| -> [f64; 3] {
            std::array::from_fn(|i| a[i] * (1.0 - t) + b[i] * t)
        };
        assert_eq!(turn(z), 0);
        assert_eq!(turn(y), 2, "a quarter turn");
        assert_eq!(turn(between(z, y, 0.5)), 1, "an eighth");
        assert_eq!(turn([0.0, 0.0, -1.0]), 4);
        assert_eq!(turn([0.0, 1.0, 0.0]), 6, "three quarters");
        // Within half a step it rounds to the step: 22 degrees is still 0, 23 is 1.
        let at = |degrees: f64| {
            let r = degrees.to_radians();
            turn([0.3, -r.sin(), r.cos()])
        };
        assert_eq!((at(22.0), at(23.0), at(-22.0), at(-23.0)), (0, 1, 0, 7));
        // Looking along up, or nowhere, or NaN, is no turn; and on the grid there is none either.
        assert_eq!(turn([1.0, 0.0, 0.0]), 0);
        assert_eq!(
            turn([1.0, -0.05, 0.1]),
            0,
            "within 11 degrees of up, whatever the lean"
        );
        assert_eq!(turn([-1.0, -0.19, 0.0]), 0);
        assert_eq!(
            turn([-1.0, -0.21, 0.0]),
            2,
            "past it, which way it leans decides"
        );
        assert_eq!(turn([0.0; 3]), 0);
        assert_eq!(turn([f64::NAN; 3]), 0);
        assert_eq!(rotation_for(centre, Alignment::World, y), 0);
    }

    #[test]
    fn the_turn_it_picks_is_one_the_server_allows() {
        let mut tool = Tool::default();
        tool.key(KeyCode::Digit2);
        let centre = VoxelCoord([123, -456, 789]);
        for step in 0..16 {
            let a = f64::from(step) * 0.4;
            let heading = [0.2, a.sin(), a.cos()];
            let op = tool.op(centre, rotation_for(centre, Alignment::Local, heading));
            assert_eq!(sim::validate(&op), Ok(()), "{op:?}");
        }
    }
}
