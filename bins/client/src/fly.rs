//! Free-fly camera controls: which keys are held, and how they move the camera.

use render::Camera;

/// Radians of turn per pixel of mouse movement.
const LOOK_SENSITIVITY: f64 = 0.0025;
/// Voxels per second (10 cm Voxels, so 6 m/s).
const SPEED: f64 = 60.0;
/// Multiplier while the speed modifier is held.
const FAST: f64 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Forward,
    Back,
    Left,
    Right,
    Up,
    Down,
    Fast,
}

#[derive(Debug, Default)]
pub struct Fly {
    held: [bool; 7],
}

impl Fly {
    pub fn set(&mut self, action: Action, pressed: bool) {
        self.held[action as usize] = pressed;
    }

    fn axis(&self, positive: Action, negative: Action) -> f64 {
        f64::from(i8::from(self.held[positive as usize]) - i8::from(self.held[negative as usize]))
    }

    /// Turns the camera by a mouse movement of `dx`, `dy` pixels: right and down are positive.
    pub fn look(&self, camera: &mut Camera, dx: f64, dy: f64) {
        camera.turn(-dx * LOOK_SENSITIVITY, -dy * LOOK_SENSITIVITY);
    }

    /// Moves the camera for `dt` seconds of the held keys: forward and back along where it
    /// looks, left and right across it, up and down along the world's y axis.
    pub fn update(&self, camera: &mut Camera, dt: f64) {
        let (forward, right) = (camera.forward(), camera.right());
        let wish = [0, 1, 2].map(|a| {
            forward[a] * self.axis(Action::Forward, Action::Back)
                + right[a] * self.axis(Action::Right, Action::Left)
                + [0.0, 1.0, 0.0][a] * self.axis(Action::Up, Action::Down)
        });
        let length = wish.iter().map(|c| c * c).sum::<f64>().sqrt();
        if length == 0.0 {
            return;
        }
        let speed = SPEED
            * if self.held[Action::Fast as usize] {
                FAST
            } else {
                1.0
            };
        // Normalised, so that holding two keys is no faster than one.
        for (position, wish) in camera.position.iter_mut().zip(wish) {
            *position += wish / length * speed * dt;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera() -> Camera {
        // Looking along -z with y up, from the origin.
        Camera::looking_at([0.0; 3], [0.0, 0.0, -1.0])
    }

    fn held(actions: &[Action]) -> Fly {
        let mut fly = Fly::default();
        for &a in actions {
            fly.set(a, true);
        }
        fly
    }

    fn moved(actions: &[Action], dt: f64) -> [f64; 3] {
        let mut camera = camera();
        held(actions).update(&mut camera, dt);
        camera.position.map(|c| (c * 1e9).round() / 1e9)
    }

    #[test]
    fn no_keys_do_not_move_the_camera() {
        assert_eq!(moved(&[], 1.0), [0.0; 3]);
    }

    #[test]
    fn wasd_move_along_and_across_the_view() {
        assert_eq!(moved(&[Action::Forward], 0.5), [0.0, 0.0, -30.0]);
        assert_eq!(moved(&[Action::Back], 0.5), [0.0, 0.0, 30.0]);
        assert_eq!(moved(&[Action::Right], 0.5), [30.0, 0.0, 0.0]);
        assert_eq!(moved(&[Action::Left], 0.5), [-30.0, 0.0, 0.0]);
    }

    #[test]
    fn up_and_down_move_along_the_world_y_axis_even_when_looking_down() {
        let mut camera = camera();
        camera.turn(0.0, -1.0);
        held(&[Action::Up]).update(&mut camera, 1.0);
        assert_eq!(camera.position, [0.0, 60.0, 0.0]);
    }

    #[test]
    fn forward_follows_the_pitch() {
        let mut camera = camera();
        camera.turn(0.0, std::f64::consts::FRAC_PI_4);
        held(&[Action::Forward]).update(&mut camera, 1.0);
        let [x, y, z] = camera.position;
        assert!(
            x.abs() < 1e-9
                && (y - 60.0 * 0.5f64.sqrt()).abs() < 1e-9
                && (z + 60.0 * 0.5f64.sqrt()).abs() < 1e-9
        );
    }

    #[test]
    fn opposite_keys_cancel() {
        assert_eq!(moved(&[Action::Forward, Action::Back], 1.0), [0.0; 3]);
    }

    #[test]
    fn two_keys_are_no_faster_than_one() {
        let [x, _, z] = moved(&[Action::Forward, Action::Right], 1.0);
        assert!(((x * x + z * z).sqrt() - 60.0).abs() < 1e-6);
    }

    #[test]
    fn the_speed_modifier_multiplies_the_speed() {
        assert_eq!(
            moved(&[Action::Forward, Action::Fast], 1.0),
            [0.0, 0.0, -480.0]
        );
    }

    #[test]
    fn moving_the_mouse_right_turns_the_view_right_and_down_looks_down() {
        let mut camera = camera();
        Fly::default().look(&mut camera, 100.0, 0.0);
        assert!(camera.forward()[0] > 0.0);
        let mut camera = self::camera();
        Fly::default().look(&mut camera, 0.0, 100.0);
        assert!(camera.forward()[1] < 0.0);
    }
}
