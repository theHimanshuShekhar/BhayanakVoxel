//! What the keys and the wheel do to the tool, the Material and the palette, in each state of the
//! mouse: captured for looking around, or released.
//!
//! The controls, which the overlay's help and the README list too:
//! - `1` to `9` pick the hotbar slot, and the wheel steps along it;
//! - `Alt` and the wheel make the Brush bigger or smaller, as `[` and `]` do;
//! - `-` and `=` step the Material, and `G` switches between local up and the grid;
//! - `Tab` opens the Material palette, which releases the mouse, and closes it, which captures
//!   the mouse again if it was captured. While it is open every key goes to its search box.

use brick_store::Material;
use winit::keyboard::KeyCode;

use crate::tool::{HOTBAR, Tool};

/// Something the player did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    /// A key was pressed (or repeated; see [`repeats`]).
    Key(KeyCode),
    /// The wheel turned by `lines` (up is positive), with `alt` held or not.
    Wheel { lines: f32, alt: bool },
}

/// What the window is to do with the mouse as a result of an input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grab {
    Release,
    Capture,
}

/// Whether holding `key` repeats its action: the ones that size and pick.
pub fn repeats(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::BracketLeft | KeyCode::BracketRight | KeyCode::Minus | KeyCode::Equal
    )
}

/// The slot of the hotbar that the number key `key` picks.
fn slot_of(key: KeyCode) -> Option<usize> {
    let digits = [
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
    ];
    digits
        .iter()
        .position(|&d| d == key)
        .filter(|&i| i < HOTBAR.len())
}

#[derive(Debug, Default)]
pub struct Editor {
    /// The tool and Material picked. They are kept for the session, palette open or not.
    pub tool: Tool,
    /// The palette is open, and every key is for its search box.
    pub palette_open: bool,
    /// What is typed in the palette's search box.
    pub query: String,
    /// The mouse was captured when the palette was opened, so closing it captures it again.
    restore_capture: bool,
    /// Wheel movement not yet enough for a step, as touchpads send it in small pieces.
    wheel: f32,
}

impl Editor {
    /// Handles `input` while the mouse is `captured` (or not). Returns what to do with the mouse.
    pub fn input(&mut self, input: Input, captured: bool) -> Option<Grab> {
        match input {
            Input::Key(KeyCode::Tab) => self.toggle_palette(captured),
            Input::Key(KeyCode::Escape) if self.palette_open => self.toggle_palette(captured),
            // Typing in the search box is not for the game.
            _ if self.palette_open => None,
            Input::Key(key) => {
                self.key(key);
                None
            }
            // Released, the wheel is over a panel and is for that.
            Input::Wheel { lines, alt } if captured => {
                self.wheel(lines, alt);
                None
            }
            Input::Wheel { .. } => None,
        }
    }

    /// Opens the palette, or closes it. The search starts empty each time it is opened.
    fn toggle_palette(&mut self, captured: bool) -> Option<Grab> {
        self.wheel = 0.0;
        self.palette_open = !self.palette_open;
        if self.palette_open {
            self.query.clear();
            self.restore_capture = captured;
            captured.then_some(Grab::Release)
        } else {
            std::mem::take(&mut self.restore_capture).then_some(Grab::Capture)
        }
    }

    fn key(&mut self, key: KeyCode) {
        if let Some(slot) = slot_of(key) {
            self.tool.select(slot);
            return;
        }
        match key {
            KeyCode::BracketLeft => self.tool.resize(-1),
            KeyCode::BracketRight => self.tool.resize(1),
            KeyCode::Minus => self.tool.step_material(false),
            KeyCode::Equal => self.tool.step_material(true),
            KeyCode::KeyG => self.tool.toggle_alignment(),
            _ => {}
        }
    }

    fn wheel(&mut self, lines: f32, alt: bool) {
        self.wheel += lines;
        let steps = self.wheel.trunc();
        self.wheel -= steps;
        let steps = steps as i32;
        if alt {
            self.tool.resize(steps);
        } else {
            // Up goes to the slot before.
            self.tool.step_slot(-steps);
        }
    }

    /// Picks a Material, from the palette.
    pub fn pick_material(&mut self, id: u16) {
        self.tool.material = Material(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use materials::MaterialTable;
    use net::{EditMode, Shape};

    const TAB: Input = Input::Key(KeyCode::Tab);

    fn wheel(lines: f32) -> Input {
        Input::Wheel { lines, alt: false }
    }

    fn alt_wheel(lines: f32) -> Input {
        Input::Wheel { lines, alt: true }
    }

    #[test]
    fn number_keys_pick_the_tool_with_the_mouse_captured_or_released() {
        for captured in [true, false] {
            let mut editor = Editor::default();
            for (key, slot) in [
                (KeyCode::Digit4, 3),
                (KeyCode::Digit1, 0),
                (KeyCode::Digit9, 8),
            ] {
                assert_eq!(editor.input(Input::Key(key), captured), None);
                assert_eq!(editor.tool.slot(), slot, "captured: {captured}");
            }
            assert_eq!(
                (editor.tool.mode, editor.tool.shape),
                (EditMode::Flatten, Shape::Cylinder)
            );
        }
    }

    #[test]
    fn the_wheel_steps_along_the_hotbar_with_the_mouse_captured() {
        let mut editor = Editor::default();
        editor.input(wheel(-1.0), true);
        assert_eq!(editor.tool.slot(), 1, "down is the next slot");
        editor.input(wheel(-2.0), true);
        assert_eq!(editor.tool.slot(), 3);
        editor.input(wheel(1.0), true);
        assert_eq!(editor.tool.slot(), 2, "up is the one before");
        editor.input(wheel(3.0), true);
        assert_eq!(editor.tool.slot(), 8, "it wraps round");
    }

    #[test]
    fn the_wheel_with_alt_sizes_the_brush_and_leaves_the_slot() {
        let mut editor = Editor::default();
        let radius = editor.tool.radius;
        editor.input(alt_wheel(1.0), true);
        editor.input(alt_wheel(2.0), true);
        assert_eq!(editor.tool.radius, radius + 3);
        editor.input(alt_wheel(-5.0), true);
        assert_eq!(editor.tool.radius, radius - 2);
        assert_eq!(editor.tool.slot(), 0);
        editor.input(alt_wheel(-100.0), true);
        assert_eq!(editor.tool.radius, 0, "a Voxel at the least");
        editor.input(alt_wheel(100.0), true);
        assert_eq!(editor.tool.radius, sim::MAX_BRUSH_RADIUS);
    }

    #[test]
    fn small_pieces_of_wheel_add_up_to_a_step() {
        let mut editor = Editor::default();
        for _ in 0..3 {
            editor.input(wheel(-0.4), true);
        }
        assert_eq!(editor.tool.slot(), 1, "1.2 of a line is a step");
        for _ in 0..2 {
            editor.input(wheel(-0.3), true);
        }
        assert_eq!(editor.tool.slot(), 1, "0.8 so far, with the 0.2 over kept");
        editor.input(wheel(-0.4), true);
        assert_eq!(editor.tool.slot(), 2);
    }

    #[test]
    fn the_wheel_released_is_left_to_the_panel_under_it() {
        let mut editor = Editor::default();
        editor.input(wheel(-1.0), false);
        editor.input(alt_wheel(1.0), false);
        assert_eq!(editor.tool, Tool::default());
    }

    #[test]
    fn the_brackets_and_the_material_keys_still_work_in_both_states() {
        for captured in [true, false] {
            let mut editor = Editor::default();
            let radius = editor.tool.radius;
            editor.input(Input::Key(KeyCode::BracketRight), captured);
            assert_eq!(editor.tool.radius, radius + 1);
            editor.input(Input::Key(KeyCode::BracketLeft), captured);
            assert_eq!(editor.tool.radius, radius);
            let first = editor.tool.material;
            editor.input(Input::Key(KeyCode::Equal), captured);
            assert_ne!(editor.tool.material, first);
            editor.input(Input::Key(KeyCode::Minus), captured);
            assert_eq!(editor.tool.material, first);
            editor.input(Input::Key(KeyCode::KeyG), captured);
            assert_eq!(editor.tool.alignment, net::Alignment::World);
        }
    }

    #[test]
    fn keys_that_are_not_the_editors_are_not_taken() {
        let mut editor = Editor::default();
        for key in [KeyCode::KeyW, KeyCode::Space, KeyCode::KeyN, KeyCode::KeyB] {
            assert_eq!(editor.input(Input::Key(key), true), None);
        }
        assert_eq!(editor.tool, Tool::default());
        assert!(!editor.palette_open);
    }

    #[test]
    fn tab_opens_the_palette_and_releases_the_mouse_and_closing_captures_it_again() {
        let mut editor = Editor::default();
        assert_eq!(editor.input(TAB, true), Some(Grab::Release));
        assert!(editor.palette_open);
        // The mouse is now released.
        assert_eq!(editor.input(TAB, false), Some(Grab::Capture));
        assert!(!editor.palette_open);
    }

    #[test]
    fn closing_the_palette_leaves_the_mouse_free_if_it_was_free_when_it_opened() {
        let mut editor = Editor::default();
        assert_eq!(editor.input(TAB, false), None);
        assert!(editor.palette_open);
        assert_eq!(editor.input(TAB, false), None);
        // And the memory of one opening does not leak into the next.
        assert_eq!(editor.input(TAB, true), Some(Grab::Release));
        assert_eq!(
            editor.input(Input::Key(KeyCode::Escape), false),
            Some(Grab::Capture)
        );
        assert_eq!(editor.input(TAB, false), None);
        assert_eq!(editor.input(TAB, false), None);
    }

    #[test]
    fn escape_closes_the_palette_and_only_then() {
        let mut editor = Editor::default();
        // Closed, Escape is the window's (it frees the mouse).
        assert_eq!(editor.input(Input::Key(KeyCode::Escape), true), None);
        assert!(!editor.palette_open);
        editor.input(TAB, true);
        assert_eq!(
            editor.input(Input::Key(KeyCode::Escape), false),
            Some(Grab::Capture)
        );
        assert!(!editor.palette_open);
    }

    #[test]
    fn typing_in_the_search_box_changes_neither_the_tool_nor_the_size_nor_the_material() {
        let mut editor = Editor::default();
        editor.input(TAB, true);
        let before = editor.tool;
        for key in [
            KeyCode::Digit1,
            KeyCode::Digit5,
            KeyCode::Digit9,
            KeyCode::BracketLeft,
            KeyCode::BracketRight,
            KeyCode::Minus,
            KeyCode::Equal,
            KeyCode::KeyG,
            KeyCode::KeyB,
        ] {
            assert_eq!(editor.input(Input::Key(key), false), None);
        }
        editor.input(wheel(-3.0), false);
        editor.input(alt_wheel(3.0), false);
        // Even if the mouse were still captured, the box has the keys and the wheel.
        editor.input(Input::Key(KeyCode::Digit3), true);
        editor.input(wheel(-3.0), true);
        assert_eq!(editor.tool, before);
        assert!(editor.palette_open);
    }

    #[test]
    fn the_keys_work_again_once_the_palette_is_closed() {
        let mut editor = Editor::default();
        editor.input(TAB, true);
        editor.input(Input::Key(KeyCode::Digit5), false);
        editor.input(TAB, false);
        editor.input(Input::Key(KeyCode::Digit5), true);
        assert_eq!(editor.tool.slot(), 4);
    }

    #[test]
    fn the_picked_material_and_tool_persist_across_openings_of_the_palette() {
        let obsidian = MaterialTable::built_in().by_name("obsidian").unwrap().id;
        let mut editor = Editor::default();
        editor.input(Input::Key(KeyCode::Digit6), true);
        editor.input(TAB, true);
        editor.pick_material(obsidian);
        editor.input(TAB, false);
        editor.input(TAB, true);
        editor.input(TAB, false);
        assert_eq!(editor.tool.material, Material(obsidian));
        assert_eq!(editor.tool.slot(), 5);
    }

    #[test]
    fn the_search_starts_empty_each_time_the_palette_opens() {
        let mut editor = Editor::default();
        editor.input(TAB, true);
        editor.query = "OBSID".into();
        editor.input(TAB, false);
        editor.input(TAB, true);
        assert_eq!(editor.query, "");
    }

    #[test]
    fn only_the_sizing_and_picking_keys_repeat() {
        assert!(repeats(KeyCode::BracketLeft) && repeats(KeyCode::Equal));
        assert!(!repeats(KeyCode::Tab) && !repeats(KeyCode::Digit1) && !repeats(KeyCode::KeyG));
    }
}
