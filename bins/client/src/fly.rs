//! Which movement keys are held, and what they do: move the free-fly debug camera (noclip), or
//! become the character's input for a tick.

use net::MoveInput;
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

    /// The held keys as the character's input for one tick, heading where the camera looks.
    /// Space jumps, Ctrl or C crouches, Shift runs.
    pub fn move_input(&self, camera: &Camera) -> MoveInput {
        let held = |action: Action| self.held[action as usize];
        let axis = |positive, negative| held(positive) as i8 - held(negative) as i8;
        MoveInput {
            forward: axis(Action::Forward, Action::Back),
            right: axis(Action::Right, Action::Left),
            heading: camera.forward(),
            jump: held(Action::Up),
            run: held(Action::Fast),
            crouch: held(Action::Down),
        }
    }

    /// Turns the camera by a mouse movement of `dx`, `dy` pixels: right and down are positive.
    pub fn look(&self, camera: &mut Camera, dx: f64, dy: f64) {
        camera.turn(-dx * LOOK_SENSITIVITY, -dy * LOOK_SENSITIVITY);
    }

    /// Moves the camera for `dt` seconds of the held keys: forward and back along where it
    /// looks, left and right across it, up and down along the horizon's up. The horizon is then
    /// levelled to the Planet's centre-out direction at the new position.
    pub fn update(&self, camera: &mut Camera, dt: f64) {
        let (forward, right) = (camera.forward(), camera.right());
        let wish = [0, 1, 2].map(|a| {
            forward[a] * self.axis(Action::Forward, Action::Back)
                + right[a] * self.axis(Action::Right, Action::Left)
                + camera.horizon_up[a] * self.axis(Action::Up, Action::Down)
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
        camera.carry_up(camera.position);
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
    fn held_keys_are_the_characters_input_heading_where_the_camera_looks() {
        let camera = camera();
        let input = held(&[
            Action::Forward,
            Action::Left,
            Action::Up,
            Action::Fast,
            Action::Down,
        ])
        .move_input(&camera);
        assert_eq!((input.forward, input.right), (1, -1));
        assert!(input.jump && input.run && input.crouch);
        assert_eq!(input.heading, camera.forward());
        assert_eq!(
            Fly::default().move_input(&camera),
            MoveInput {
                heading: camera.forward(),
                ..MoveInput::default()
            }
        );
        let back = held(&[Action::Back, Action::Right]).move_input(&camera);
        assert_eq!((back.forward, back.right), (-1, 1));
        let both = held(&[Action::Forward, Action::Back]).move_input(&camera);
        assert_eq!(both.forward, 0);
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
    fn up_and_down_move_along_the_horizons_up_even_when_looking_down() {
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

    fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
        (0..3).map(|i| a[i] * b[i]).sum()
    }

    /// A level camera over the Planet at `position`, looking along `towards`.
    fn over_the_planet(position: [f64; 3], towards: [f64; 3]) -> Camera {
        let mut camera = Camera::looking_at(position, towards);
        camera.carry_up(position);
        camera
    }

    #[test]
    fn up_and_down_move_along_the_radial_up_away_from_the_pole() {
        for (position, radial) in [
            ([3000.0, 0.0, 0.0], [1.0, 0.0, 0.0]),
            ([0.0, 0.0, -3000.0], [0.0, 0.0, -1.0]),
            ([0.0, -3000.0, 0.0], [0.0, -1.0, 0.0]),
        ] {
            let mut camera = over_the_planet(position, [0.0; 3]);
            camera.turn(0.4, 0.3);
            held(&[Action::Up]).update(&mut camera, 1.0);
            let moved = [0, 1, 2].map(|a| camera.position[a] - position[a]);
            assert!(
                (0..3).all(|a| (moved[a] - 60.0 * radial[a]).abs() < 1e-9),
                "up from {position:?} moved {moved:?}"
            );
            held(&[Action::Down]).update(&mut camera, 1.0);
            assert!((0..3).all(|a| (camera.position[a] - position[a]).abs() < 1e-3));
        }
    }

    #[test]
    fn mouse_look_keeps_the_horizon_level_away_from_the_pole() {
        let position = [0.0, 0.0, 3000.0];
        let mut camera = over_the_planet(position, [0.0; 3]);
        Fly::default().look(&mut camera, 300.0, -50.0);
        assert!(dot(camera.right(), [0.0, 0.0, 1.0]).abs() < 1e-12);
        assert!(dot(camera.forward(), [0.0, 0.0, 1.0]) > 0.0);
    }

    #[test]
    fn flying_straight_over_a_pole_carries_the_heading_smoothly() {
        // Level, heading +x, 300 Voxels short of the +y pole.
        let mut camera = over_the_planet([-300.0, 3000.0, 0.0], [0.0, 3000.0, 0.0]);
        let fly = held(&[Action::Forward]);
        let mut forward = camera.forward();
        let mut crossed = false;
        for _ in 0..200 {
            fly.update(&mut camera, 0.1);
            crossed |= camera.position[0] > 0.0;
            let next = camera.forward();
            let step = (0..3).map(|a| (next[a] - forward[a]).powi(2)).sum::<f64>();
            // Each step is 6 Voxels, 0.002 radians of the Planet: nothing like a flip or a spin.
            assert!(
                step.sqrt() < 0.005,
                "forward jumped by {} at {:?}",
                step.sqrt(),
                camera.position
            );
            let radial = camera
                .position
                .map(|c| c / dot(camera.position, camera.position).sqrt());
            assert!(dot(camera.right(), radial).abs() < 1e-9);
            forward = next;
        }
        assert!(crossed, "the path should have passed over the pole");
    }

    #[test]
    fn flying_at_the_centre_gives_no_nan() {
        let mut camera = over_the_planet([0.0, 0.0, 6.0], [0.0; 3]);
        held(&[Action::Forward, Action::Up]).update(&mut camera, 0.0);
        assert!(camera.forward().iter().all(|c| c.is_finite()));
        // Down for 0.1 s is 6 Voxels: onto the centre, and then through it.
        let fly = held(&[Action::Down]);
        for _ in 0..2 {
            fly.update(&mut camera, 0.1);
            for v in [
                camera.position,
                camera.forward(),
                camera.right(),
                camera.up(),
            ] {
                assert!(v.iter().all(|c| c.is_finite()), "{camera:?}");
            }
        }
    }
}
