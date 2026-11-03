//! The Brush the player has picked: what the mouse applies, and the keys that change it.

use brick_store::{Material, VoxelCoord};
use materials::MaterialTable;
use net::{Alignment, Brush, EditMode, EditOp, Shape};
use sim::MAX_BRUSH_RADIUS;
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
    /// The Edit that this tool makes at `centre`.
    pub fn op(&self, centre: VoxelCoord) -> EditOp {
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
            KeyCode::BracketRight => self.radius = (self.radius + 1).min(MAX_BRUSH_RADIUS),
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
        true
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tool_starts_as_the_1_m_dig_sphere() {
        let tool = Tool::default();
        assert_eq!(
            tool.op(VoxelCoord([1, 2, 3])),
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
        assert!(sim::validate(&tool.op(VoxelCoord([0; 3]))).is_ok());
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
        let op = tool.op(VoxelCoord([0; 3]));
        assert_eq!(sim::validate(&op), Ok(()));
        assert_eq!(op.material, tool.material);
    }

    #[test]
    fn only_the_modes_that_fill_carry_a_material_and_g_switches_to_the_grid() {
        let mut tool = Tool::default();
        tool.key(KeyCode::Equal);
        assert_eq!(tool.op(VoxelCoord([0; 3])).material, Material(0), "dig");
        tool.key(KeyCode::Digit4);
        assert_eq!(tool.op(VoxelCoord([0; 3])).material, tool.material);
        assert_eq!(tool.op(VoxelCoord([0; 3])).alignment, Alignment::Local);
        tool.key(KeyCode::KeyG);
        assert_eq!(tool.op(VoxelCoord([0; 3])).alignment, Alignment::World);
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
}
