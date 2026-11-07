//! The Brush the player has picked: what the mouse applies, and the hotbar of tools it comes from.

use brick_store::{Material, VOXELS_PER_METRE, VoxelCoord};
use materials::MaterialTable;
use net::{Alignment, Brush, EditMode, EditOp, Shape};
use sim::{MAX_BRUSH_RADIUS, MAX_SMOOTH_RADIUS};

/// The tools of the hotbar, which the number keys `1` to `9` pick: a mode and a shape each.
pub const HOTBAR: [(EditMode, Shape); 9] = [
    (EditMode::Dig, Shape::Sphere),
    (EditMode::Dig, Shape::Cube),
    (EditMode::Dig, Shape::Cylinder),
    (EditMode::Place, Shape::Sphere),
    (EditMode::Place, Shape::Cube),
    (EditMode::Place, Shape::Cylinder),
    (EditMode::Smooth, Shape::Sphere),
    (EditMode::Flatten, Shape::Cube),
    (EditMode::Flatten, Shape::Cylinder),
];

/// The radius the client starts with: 1 m, in Voxels.
pub(crate) const DEFAULT_RADIUS: u16 = VOXELS_PER_METRE as u16;
const _: () = assert!(DEFAULT_RADIUS <= MAX_BRUSH_RADIUS);

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

    /// Picks the tool in hotbar slot `slot` (0 to 8; one past the last is ignored): its mode and
    /// its shape. The size is kept, down to what the server allows the mode.
    pub fn select(&mut self, slot: usize) {
        if let Some(&(mode, shape)) = HOTBAR.get(slot) {
            self.mode = mode;
            self.shape = shape;
            self.radius = self.radius.min(self.largest());
        }
    }

    /// The hotbar slot the tool is in.
    pub fn slot(&self) -> usize {
        HOTBAR
            .iter()
            .position(|&(mode, shape)| mode == self.mode && shape == self.shape)
            .unwrap_or(0)
    }

    /// Moves along the hotbar by `steps` slots, wrapping round.
    pub fn step_slot(&mut self, steps: i32) {
        let n = HOTBAR.len() as i32;
        self.select((self.slot() as i32 + steps).rem_euclid(n) as usize);
    }

    /// Makes the Brush `steps` radii bigger, or smaller if negative: a Voxel at the least, and at
    /// the most what the server allows the mode.
    pub fn resize(&mut self, steps: i32) {
        let radius = (i32::from(self.radius) + steps).clamp(0, i32::from(self.largest()));
        self.radius = radius as u16;
    }

    /// Switches between a Brush on the local up and one on the grid.
    pub fn toggle_alignment(&mut self) {
        self.alignment = match self.alignment {
            Alignment::Local => Alignment::World,
            Alignment::World => Alignment::Local,
        }
    }

    /// The biggest radius that the server allows this tool's mode.
    fn largest(&self) -> u16 {
        match self.mode {
            EditMode::Smooth | EditMode::Flatten => MAX_SMOOTH_RADIUS,
            EditMode::Dig | EditMode::Place => MAX_BRUSH_RADIUS,
        }
    }

    /// The next Material of the table, or the previous, wrapping round.
    pub fn step_material(&mut self, forward: bool) {
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
        let (mode, shape) = (mode_name(self.mode), shape_name(self.shape));
        let fill = match (self.mode, MaterialTable::built_in().get(self.material)) {
            (EditMode::Place | EditMode::Flatten, Some(def)) => format!(", {}", def.name),
            _ => String::new(),
        };
        let grid = match self.alignment {
            Alignment::Local => "local up",
            Alignment::World => "grid",
        };
        format!(
            "{mode} {shape} {:.0} cm{fill}, {grid}",
            (f64::from(self.radius) * 2.0 + 1.0) * 100.0 / VOXELS_PER_METRE
        )
    }
}

/// The label of each hotbar slot: its number, mode and shape on a line each.
pub fn slot_labels() -> Vec<String> {
    HOTBAR
        .iter()
        .enumerate()
        .map(|(i, &(mode, shape))| format!("{}\n{}\n{}", i + 1, mode_name(mode), shape_name(shape)))
        .collect()
}

/// What a mode is called.
pub fn mode_name(mode: EditMode) -> &'static str {
    match mode {
        EditMode::Dig => "dig",
        EditMode::Place => "place",
        EditMode::Smooth => "smooth",
        EditMode::Flatten => "flatten",
    }
}

/// What a shape is called.
pub fn shape_name(shape: Shape) -> &'static str {
    match shape {
        Shape::Sphere => "sphere",
        Shape::Cube => "cube",
        Shape::Cylinder => "cylinder",
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
            EditOp::dig_sphere(VoxelCoord([1, 2, 3]), DEFAULT_RADIUS)
        );
    }

    #[test]
    fn every_hotbar_slot_is_a_tool_of_its_own_and_picks_its_mode_and_shape() {
        let mut tool = Tool::default();
        assert_eq!(tool.slot(), 0, "the tool starts in the first slot");
        for (slot, &(mode, shape)) in HOTBAR.iter().enumerate() {
            tool.select(slot);
            assert_eq!((tool.mode, tool.shape, tool.slot()), (mode, shape, slot));
        }
        let distinct: std::collections::BTreeSet<_> = HOTBAR
            .iter()
            .map(|&(m, s)| (mode_name(m), shape_name(s)))
            .collect();
        assert_eq!(distinct.len(), HOTBAR.len());
        tool.select(9);
        assert_eq!(tool.slot(), 8, "a slot past the last is ignored");
    }

    #[test]
    fn the_hotbar_wraps_round_in_both_directions() {
        let mut tool = Tool::default();
        tool.step_slot(-1);
        assert_eq!(tool.slot(), 8);
        tool.step_slot(1);
        assert_eq!(tool.slot(), 0);
        tool.step_slot(10);
        assert_eq!(tool.slot(), 1);
    }

    #[test]
    fn resizing_goes_from_a_voxel_to_2_m() {
        let mut tool = Tool::default();
        tool.resize(-100);
        assert_eq!(tool.radius, 0);
        tool.resize(100);
        assert_eq!(tool.radius, MAX_BRUSH_RADIUS);
        assert!(sim::validate(&tool.op(VoxelCoord([0; 3]), 0)).is_ok());
    }

    #[test]
    fn a_smooth_or_a_flatten_is_held_to_1_m_and_a_dig_or_a_place_to_2_m() {
        let mut tool = Tool::default();
        tool.resize(100);
        assert_eq!(tool.radius, MAX_BRUSH_RADIUS);
        // Picking Smooth takes it down to what the server allows, and it does not go up again.
        tool.select(6);
        assert_eq!(tool.mode, EditMode::Smooth);
        assert_eq!(tool.radius, MAX_SMOOTH_RADIUS);
        tool.resize(1);
        assert_eq!(tool.radius, MAX_SMOOTH_RADIUS);
        tool.select(7);
        assert_eq!(tool.radius, MAX_SMOOTH_RADIUS);
        tool.select(3);
        tool.resize(1);
        assert_eq!(tool.radius, MAX_SMOOTH_RADIUS + 1);
        for slot in 0..HOTBAR.len() {
            tool.select(slot);
            assert_eq!(sim::validate(&tool.op(VoxelCoord([0; 3]), 0)), Ok(()));
        }
    }

    #[test]
    fn stepping_the_material_walks_the_table_and_wraps() {
        let table = MaterialTable::built_in();
        let (first, last) = (table.defs()[0].id, table.defs().last().unwrap().id);
        let mut tool = Tool::default();
        tool.select(3);
        tool.step_material(false);
        assert_eq!(tool.material, Material(last), "wraps back to the last");
        tool.step_material(true);
        assert_eq!(tool.material, Material(first));
        tool.step_material(true);
        assert_eq!(tool.material, Material(table.defs()[1].id));
        // Every Material it offers is one the server accepts.
        let op = tool.op(VoxelCoord([0; 3]), 0);
        assert_eq!(sim::validate(&op), Ok(()));
        assert_eq!(op.material, tool.material);
    }

    #[test]
    fn only_the_modes_that_fill_carry_a_material_and_the_alignment_toggles() {
        let mut tool = Tool::default();
        tool.step_material(true);
        assert_eq!(tool.op(VoxelCoord([0; 3]), 0).material, Material(0), "dig");
        tool.select(7);
        assert_eq!(tool.op(VoxelCoord([0; 3]), 0).material, tool.material);
        assert_eq!(tool.op(VoxelCoord([0; 3]), 0).alignment, Alignment::Local);
        tool.toggle_alignment();
        assert_eq!(tool.op(VoxelCoord([0; 3]), 0).alignment, Alignment::World);
        tool.toggle_alignment();
        assert_eq!(tool.alignment, Alignment::Local);
    }

    #[test]
    fn the_title_says_what_the_tool_does() {
        let mut tool = Tool::default();
        assert_eq!(tool.describe(), "dig sphere 205 cm, local up");
        tool.select(4);
        tool.toggle_alignment();
        assert_eq!(tool.describe(), "place cube 205 cm, rock, grid");
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
        tool.select(3);
        let centre = VoxelCoord([123, -456, 789]);
        for step in 0..16 {
            let a = f64::from(step) * 0.4;
            let heading = [0.2, a.sin(), a.cos()];
            let op = tool.op(centre, rotation_for(centre, Alignment::Local, heading));
            assert_eq!(sim::validate(&op), Ok(()), "{op:?}");
        }
    }
}
