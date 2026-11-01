//! The controller on small synthetic Planets: a sphere of Voxels with boxes on it, so that each
//! test sets up exactly the step, wall or ceiling it is about, in the frame of the spot on the
//! sphere it is run at (the poles, a cube-face edge and a cube corner among them, where the
//! capsule's radial up is farthest from the Voxel grid's axes).

use planet_gen::planet::tangents;

use super::*;

/// Radius of the test Planet, in Voxels: 200 m.
const RADIUS: f64 = 2000.0;

/// Where on the sphere the tests are run: directions from the centre. The first six are the
/// axes (the poles of each); the others are on the edges between cube faces, at a corner, and
/// at no particular place.
const SITES: [[f64; 3]; 11] = [
    [1.0, 0.0, 0.0],
    [-1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, -1.0, 0.0],
    [0.0, 0.0, 1.0],
    [0.0, 0.0, -1.0],
    [1.0, 1.0, 0.0],
    [-1.0, 0.0, 1.0],
    [0.0, 1.0, -1.0],
    [1.0, 1.0, 1.0],
    [-0.3, 0.8, 0.5],
];

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| a[i] * b[i]).sum()
}

fn unit(v: [f64; 3]) -> [f64; 3] {
    let length = dot(v, v).sqrt();
    v.map(|c| c / length)
}

/// A box of Voxels, in a site's frame: `s` along the way ahead, `t` across, `h` Voxels above
/// the sphere.
#[derive(Clone, Copy)]
struct Block {
    s: (f64, f64),
    t: (f64, f64),
    h: (f64, f64),
}

/// A spot on the test Planet, with what is built there. It is the Planet's occupancy: ground
/// below the sphere's surface, the blocks, and optionally space that is not loaded.
struct Site {
    origin: [f64; 3],
    up: [f64; 3],
    ahead: [f64; 3],
    aside: [f64; 3],
    blocks: Vec<Block>,
    /// Voxels at or beyond this `s` are unloaded.
    unloaded_from: Option<f64>,
    /// Voxels below this `h` are unloaded instead of ground.
    unloaded_below: Option<f64>,
}

const EVERYWHERE: (f64, f64) = (-1e6, 1e6);

impl Site {
    fn at(direction: [f64; 3]) -> Self {
        let up = unit(direction);
        let (ahead, aside) = tangents(up);
        Self {
            origin: up.map(|c| c * RADIUS),
            up,
            ahead,
            aside,
            blocks: Vec::new(),
            unloaded_from: None,
            unloaded_below: None,
        }
    }

    fn with(mut self, s: (f64, f64), t: (f64, f64), h: (f64, f64)) -> Self {
        self.blocks.push(Block { s, t, h });
        self
    }

    /// `(s, t, h)` of a point.
    fn frame(&self, p: [f64; 3]) -> [f64; 3] {
        let d = [0, 1, 2].map(|a| p[a] - self.origin[a]);
        [
            dot(d, self.ahead),
            dot(d, self.aside),
            dot(p, p).sqrt() - RADIUS,
        ]
    }

    /// The point `(s, t, h)` of this site.
    fn point(&self, s: f64, t: f64, h: f64) -> [f64; 3] {
        let flat: [f64; 3] =
            std::array::from_fn(|a| self.origin[a] + self.ahead[a] * s + self.aside[a] * t);
        let up = unit(flat);
        // Out along the radial up from the ground at the foot of (s, t).
        up.map(|c| c * (RADIUS + h))
    }

    fn input(&self) -> MoveInput {
        MoveInput {
            heading: self.ahead,
            ..MoveInput::default()
        }
    }

    fn walk(&self) -> MoveInput {
        MoveInput {
            forward: 1,
            ..self.input()
        }
    }
}

impl Occupancy for Site {
    fn cell(&self, voxel: VoxelCoord) -> Cell {
        let centre = voxel.0.map(|c| f64::from(c) + 0.5);
        let [s, t, h] = self.frame(centre);
        if self.unloaded_from.is_some_and(|from| s >= from) {
            return Cell::Unloaded;
        }
        if h <= 0.0 {
            return if self.unloaded_below.is_some_and(|below| h < below) {
                Cell::Unloaded
            } else {
                Cell::Solid
            };
        }
        let inside = |v: f64, (lo, hi): (f64, f64)| lo <= v && v <= hi;
        if self
            .blocks
            .iter()
            .any(|b| inside(s, b.s) && inside(t, b.t) && inside(h, b.h))
        {
            Cell::Solid
        } else {
            Cell::Air
        }
    }
}

/// A character set down just above the ground at `(s, t)` of the site, settled onto it.
fn standing(site: &Site, s: f64, t: f64) -> Character {
    let mut c = Character::spawn(Movement::default(), site.point(s, t, 6.0), None);
    for _ in 0..120 {
        c.tick(site, &site.input());
    }
    assert!(c.state().grounded, "did not land from 6 Voxels up");
    c
}

fn feet(c: &Character, site: &Site) -> [f64; 3] {
    site.frame(c.state().position)
}

fn run(c: &mut Character, site: &Site, input: &MoveInput, ticks: usize) {
    for _ in 0..ticks {
        c.tick(site, input);
    }
}

fn speed(c: &Character) -> f64 {
    dot(c.state().velocity, c.state().velocity).sqrt()
}

#[test]
fn a_character_set_down_stands_on_the_ground_and_stays_there() {
    for direction in SITES {
        let site = Site::at(direction);
        let mut c = standing(&site, 0.0, 0.0);
        let before = c.state().position;
        let [_, _, h] = feet(&c, &site);
        // The capsule rests on Voxel tops of a staircase of a surface, within a Voxel of it.
        assert!(
            h.abs() < 1.5,
            "{direction:?}: feet {h} Voxels above the sphere"
        );
        run(&mut c, &site, &site.input(), 180);
        let after = c.state().position;
        let drift = (0..3)
            .map(|a| (after[a] - before[a]) * (after[a] - before[a]))
            .sum::<f64>();
        assert!(
            drift.sqrt() < 0.05,
            "{direction:?}: drifted {}",
            drift.sqrt()
        );
        assert!(c.state().grounded && speed(&c) < 0.5, "{direction:?}");
    }
}

#[test]
fn walk_run_and_crouch_go_at_their_speeds_in_metres_per_second() {
    for direction in SITES {
        let site = Site::at(direction);
        for (name, run_key, crouch_key, metres) in [
            ("walk", false, false, 4.5),
            ("run", true, false, 7.0),
            ("crouch", false, true, 2.0),
        ] {
            let mut c = standing(&site, 0.0, 0.0);
            let input = MoveInput {
                run: run_key,
                crouch: crouch_key,
                ..site.walk()
            };
            // Settles into the crouch first, then times one second.
            run(&mut c, &site, &input, 3);
            let start = feet(&c, &site);
            run(&mut c, &site, &input, 60);
            let end = feet(&c, &site);
            let went = end[0] - start[0];
            let wanted = metres * 10.0;
            assert!(
                (wanted * 0.95..=wanted * 1.01).contains(&went),
                "{direction:?} {name}: {went} Voxels in a second, wanted {wanted}"
            );
            assert!(c.state().grounded, "{direction:?} {name}");
        }
    }
}

#[test]
fn it_walks_up_onto_a_four_voxel_step() {
    for direction in SITES {
        let site = Site::at(direction).with((100.0, 1e6), EVERYWHERE, (0.0, 4.0));
        let mut c = standing(&site, 0.0, 0.0);
        run(&mut c, &site, &site.walk(), 360);
        let [s, t, h] = feet(&c, &site);
        assert!(s > 200.0, "{direction:?}: only got to s = {s}");
        assert!(t.abs() < 1.0, "{direction:?}: strayed to t = {t}");
        assert!(
            (3.0..=5.0).contains(&h) && c.state().grounded,
            "{direction:?}: standing {h} up"
        );
    }
}

#[test]
fn it_stops_at_a_six_voxel_wall() {
    for direction in SITES {
        let site = Site::at(direction).with((100.0, 130.0), EVERYWHERE, (0.0, 6.0));
        let mut c = standing(&site, 0.0, 0.0);
        let mut furthest = f64::MIN;
        for _ in 0..480 {
            c.tick(&site, &site.walk());
            furthest = furthest.max(feet(&c, &site)[0]);
        }
        let [s, _, h] = feet(&c, &site);
        // Its axis stops a radius (3 Voxels) short of the wall.
        assert!(
            (95.0..=97.3).contains(&furthest) && (s - furthest).abs() < 0.5,
            "{direction:?}: stopped at s = {s} (furthest {furthest}), wall at 100"
        );
        assert!(h < 2.0, "{direction:?}: climbed to {h}");
    }
}

#[test]
fn a_jump_goes_about_1_2_metres_up_and_comes_back_down() {
    for direction in SITES {
        let site = Site::at(direction);
        let mut c = standing(&site, 0.0, 0.0);
        let start = feet(&c, &site)[2];
        let jump = MoveInput {
            jump: true,
            ..site.input()
        };
        c.tick(&site, &jump);
        let mut apex = f64::MIN;
        let mut airborne = 0;
        for _ in 0..120 {
            c.tick(&site, &site.input());
            apex = apex.max(feet(&c, &site)[2] - start);
            airborne += usize::from(!c.state().grounded);
        }
        assert!(
            (11.0..=13.0).contains(&apex),
            "{direction:?}: jumped {apex} Voxels, wanted 12"
        );
        // Up and down again is 2 * sqrt(2 * 1.2 / 9.8) = 0.99 s.
        assert!((55..=65).contains(&airborne), "{direction:?}: {airborne}");
        assert!(c.state().grounded);
    }
}

#[test]
fn a_held_jump_does_not_bounce_it_in_the_air() {
    let site = Site::at(SITES[0]);
    let mut c = standing(&site, 0.0, 0.0);
    let start = feet(&c, &site)[2];
    let jump = MoveInput {
        jump: true,
        ..site.input()
    };
    let mut apex = f64::MIN;
    for _ in 0..30 {
        c.tick(&site, &jump);
        apex = apex.max(feet(&c, &site)[2] - start);
    }
    assert!(apex < 13.0, "{apex}");
}

/// A low ceiling, 1.5 m up, over `s` from 100 to 300: a tunnel with no walls.
fn tunnel(direction: [f64; 3]) -> Site {
    Site::at(direction).with((100.0, 300.0), EVERYWHERE, (15.0, 40.0))
}

#[test]
fn standing_it_cannot_go_under_a_1_5_metre_ceiling_and_crouching_it_can() {
    for direction in SITES {
        let site = tunnel(direction);

        let mut c = standing(&site, 0.0, 0.0);
        run(&mut c, &site, &site.walk(), 300);
        let [s, ..] = feet(&c, &site);
        assert!(s < 97.5, "{direction:?}: walked under it, to s = {s}");

        let mut c = standing(&site, 0.0, 0.0);
        let crouch = MoveInput {
            crouch: true,
            ..site.walk()
        };
        // 8 s at 2 m/s is 160 Voxels.
        run(&mut c, &site, &crouch, 480);
        let [s, ..] = feet(&c, &site);
        assert!(
            s > 150.0 && c.state().crouching,
            "{direction:?}: crouched to s = {s}"
        );

        // Letting go of crouch under it does not stand up into the ceiling.
        run(&mut c, &site, &site.input(), 30);
        assert!(c.state().crouching, "{direction:?}: stood up under it");

        // Out of the other end it can (still at the crouch's pace until it has).
        let on = MoveInput {
            crouch: false,
            ..site.walk()
        };
        run(&mut c, &site, &on, 720);
        let [s, ..] = feet(&c, &site);
        assert!(s > 320.0 && !c.state().crouching, "{direction:?}: s = {s}");
    }
}

#[test]
fn a_one_voxel_wall_is_never_gone_through_at_run_speed_whatever_the_frame_time() {
    for direction in SITES {
        let site = Site::at(direction).with((100.0, 101.0), EVERYWHERE, (0.0, 40.0));
        let run_at = MoveInput {
            run: true,
            ..site.walk()
        };
        for dt in [1.0 / 60.0, 0.1, 0.25] {
            let mut c = standing(&site, 0.0, 0.0);
            let mut furthest = f64::MIN;
            // 6 s: a run covers 420 Voxels, four times the way to the wall.
            for _ in 0..(6.0 / dt) as usize {
                c.step(&site, &run_at, dt);
                furthest = furthest.max(feet(&c, &site)[0]);
            }
            assert!(
                furthest < 97.5 && furthest > 90.0,
                "{direction:?} at {dt} s a frame: got to s = {furthest}, wall at 100"
            );
        }
    }
}

#[test]
fn a_thin_floor_stops_a_fall_at_terminal_speed() {
    for direction in SITES {
        // A one-Voxel-thick platform 30 m up, over wide ground that is not solid below it.
        let site = Site::at(direction).with(EVERYWHERE, EVERYWHERE, (300.0, 301.0));
        for dt in [1.0 / 60.0, 0.25] {
            let mut c = Character::spawn(Movement::default(), site.point(0.0, 0.0, 330.0), None);
            for _ in 0..(3.0 / dt) as usize {
                c.step(&site, &site.input(), dt);
            }
            let [_, _, h] = feet(&c, &site);
            assert!(
                (300.5..=304.0).contains(&h) && c.state().grounded,
                "{direction:?} at {dt} s a frame: at h = {h}"
            );
        }
    }
}

#[test]
fn it_slides_along_a_wall_it_walks_at_slantwise() {
    for direction in SITES {
        let site = Site::at(direction).with((100.0, 130.0), EVERYWHERE, (0.0, 30.0));
        let mut c = standing(&site, 90.0, 0.0);
        let slant = MoveInput {
            right: 1,
            ..site.walk()
        };
        run(&mut c, &site, &slant, 120);
        let [s, t, _] = feet(&c, &site);
        assert!(s < 97.5, "{direction:?}: through the wall, s = {s}");
        // Two seconds along the wall at the walk's 4.5 m/s, less the part of it into the wall.
        assert!(t > 40.0, "{direction:?}: slid only to t = {t}");
    }
}

#[test]
fn unloaded_space_is_solid_so_it_stops_at_the_edge_instead_of_falling() {
    for direction in SITES {
        let mut site = Site::at(direction);
        site.unloaded_from = Some(100.0);
        let mut c = standing(&site, 0.0, 0.0);
        run(&mut c, &site, &site.walk(), 360);
        let [s, _, h] = feet(&c, &site);
        assert!(
            (95.0..97.5).contains(&s) && h.abs() < 1.5 && c.state().grounded,
            "{direction:?}: s = {s}, h = {h}"
        );
    }
}

#[test]
fn it_stands_on_ground_that_is_not_loaded_instead_of_falling_through() {
    for direction in SITES {
        let mut site = Site::at(direction);
        // All of the ground is missing.
        site.unloaded_below = Some(1e9);
        let c = standing(&site, 0.0, 0.0);
        let [_, _, h] = feet(&c, &site);
        assert!(h.abs() < 1.5, "{direction:?}: at {h}");
    }
}

#[test]
fn a_character_set_down_inside_the_ground_is_lifted_out_of_it() {
    for direction in SITES {
        let site = Site::at(direction);
        let mut c = Character::spawn(Movement::default(), site.point(0.0, 0.0, -12.0), None);
        run(&mut c, &site, &site.input(), 120);
        let [_, _, h] = feet(&c, &site);
        assert!(c.state().grounded && h.abs() < 1.5, "{direction:?}: at {h}");
    }
}

#[test]
fn it_is_stopped_at_the_end_of_its_leash() {
    for direction in SITES {
        let site = Site::at(direction);
        let leash = Leash {
            centre: site.origin,
            radius: 50.0,
        };
        let mut c = Character::spawn(Movement::default(), site.point(0.0, 0.0, 6.0), Some(leash));
        run(&mut c, &site, &site.walk(), 360);
        let [s, t, h] = feet(&c, &site);
        let from_centre = (s * s + t * t + h * h).sqrt();
        assert!(
            (42.0..=50.0).contains(&from_centre),
            "{direction:?}: stopped {from_centre} from the centre, leash 50"
        );
    }
}

#[test]
fn input_that_is_not_a_direction_moves_nowhere_and_makes_no_nan() {
    let site = Site::at(SITES[3]);
    for heading in [
        [0.0; 3],
        [f64::NAN, 0.0, 1.0],
        [f64::INFINITY, 0.0, 0.0],
        site.up,
    ] {
        let mut c = standing(&site, 0.0, 0.0);
        let before = c.state().position;
        let input = MoveInput {
            forward: 100,
            right: -100,
            heading,
            ..MoveInput::default()
        };
        run(&mut c, &site, &input, 30);
        let after = c.state().position;
        assert!(after.iter().all(|p| p.is_finite()), "{heading:?}");
        let moved = (0..3)
            .map(|a| (after[a] - before[a]) * (after[a] - before[a]))
            .sum::<f64>();
        assert!(moved.sqrt() < 0.1, "{heading:?}: moved {}", moved.sqrt());
    }
}

#[test]
fn the_eyes_are_above_the_feet_along_the_radial_up_standing_and_lower_crouching() {
    for direction in SITES {
        let site = Site::at(direction);
        let mut c = standing(&site, 0.0, 0.0);
        let rise = |c: &Character| dot(c.eye(), site.up) - dot(c.state().position, site.up);
        assert!((rise(&c) - 16.0).abs() < 1e-6, "{direction:?}");
        let crouch = MoveInput {
            crouch: true,
            ..site.input()
        };
        run(&mut c, &site, &crouch, 2);
        assert!((rise(&c) - 10.0).abs() < 1e-6, "{direction:?}");
    }
}

#[test]
fn the_same_inputs_give_the_same_bits() {
    let site = Site::at(SITES[9]).with((100.0, 130.0), EVERYWHERE, (0.0, 4.0));
    let script = |c: &mut Character| {
        for i in 0..400usize {
            let input = MoveInput {
                forward: 1,
                right: [0, 1, 0, -1][i / 50 % 4],
                jump: i % 97 == 0,
                run: i % 200 > 100,
                crouch: i % 150 > 120,
                ..site.input()
            };
            c.step(&site, &input, if i % 3 == 0 { 0.05 } else { TICK_DT });
        }
    };
    let (mut a, mut b) = (standing(&site, 0.0, 0.0), standing(&site, 0.0, 0.0));
    script(&mut a);
    script(&mut b);
    assert!(a.state().identical(b.state()));
    assert!(!a.state().identical(standing(&site, 0.0, 0.0).state()));
}

#[test]
fn the_sim_path_uses_only_operations_that_are_exact_on_every_machine() {
    // IEEE-754 pins + - * / sqrt, floor, ceil, abs, min and max to one result. Transcendentals
    // and fused multiply-add are left to the platform's maths library or its hardware, so a
    // client and a server on different machines could disagree by a bit.
    const BANNED: [&str; 24] = [
        "sin",
        "cos",
        "tan",
        "asin",
        "acos",
        "atan",
        "atan2",
        "sinh",
        "cosh",
        "tanh",
        "exp",
        "exp2",
        "ln",
        "log",
        "log2",
        "log10",
        "powf",
        "powi",
        "cbrt",
        "hypot",
        "mul_add",
        "to_radians",
        "to_degrees",
        "sin_cos",
    ];
    for (file, source) in [
        ("character.rs", include_str!("../character.rs")),
        ("movement.rs", include_str!("../movement.rs")),
    ] {
        let code = source.split("#[cfg(test)]").next().unwrap();
        for (number, line) in code.lines().enumerate() {
            let line = line.split("//").next().unwrap();
            for banned in BANNED {
                let call = format!(".{banned}(");
                assert!(
                    !line.contains(&call),
                    "{file}:{}: {banned} is not exact across machines",
                    number + 1
                );
            }
        }
    }
}
