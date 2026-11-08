//! The controller on small synthetic Planets: a sphere of Voxels with boxes on it, so that each
//! test sets up exactly the step, wall or ceiling it is about, in the frame of the spot on the
//! sphere it is run at (the poles, a cube-face edge and a cube corner among them, where the
//! capsule's radial up is farthest from the Voxel grid's axes).

use planet_gen::planet::tangents;

use super::*;

/// The size of `metres`, in Voxels. The scenes below are set up in metres, as the movement is.
fn v(metres: f64) -> f64 {
    metres * VOXELS_PER_METRE
}

/// Radius of the test Planet, in Voxels: 200 m.
const RADIUS: f64 = 200.0 * VOXELS_PER_METRE;

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
    let mut c = Character::spawn(Movement::default(), site.point(s, t, v(0.6)), None);
    for _ in 0..120 {
        c.tick(site, &site.input());
    }
    assert!(c.state().grounded, "did not land from 60 cm up");
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
            drift.sqrt() < v(0.005),
            "{direction:?}: drifted {}",
            drift.sqrt()
        );
        assert!(c.state().grounded && speed(&c) < v(0.05), "{direction:?}");
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
            let wanted = v(metres);
            assert!(
                (wanted * 0.95..=wanted * 1.01).contains(&went),
                "{direction:?} {name}: {went} Voxels in a second, wanted {wanted}"
            );
            assert!(c.state().grounded, "{direction:?} {name}");
        }
    }
}

#[test]
fn it_walks_up_onto_a_40_cm_step() {
    for direction in SITES {
        let site = Site::at(direction).with((v(10.0), 1e6), EVERYWHERE, (0.0, v(0.4)));
        let mut c = standing(&site, 0.0, 0.0);
        run(&mut c, &site, &site.walk(), 360);
        let [s, t, h] = feet(&c, &site);
        assert!(s > v(20.0), "{direction:?}: only got to s = {s}");
        assert!(t.abs() < v(0.1), "{direction:?}: strayed to t = {t}");
        assert!(
            (v(0.3)..=v(0.5)).contains(&h) && c.state().grounded,
            "{direction:?}: standing {h} up"
        );
    }
}

#[test]
fn it_stops_at_a_60_cm_wall() {
    for direction in SITES {
        let site = Site::at(direction).with((v(10.0), v(13.0)), EVERYWHERE, (0.0, v(0.6)));
        let mut c = standing(&site, 0.0, 0.0);
        let mut furthest = f64::MIN;
        for _ in 0..480 {
            c.tick(&site, &site.walk());
            furthest = furthest.max(feet(&c, &site)[0]);
        }
        let [s, _, h] = feet(&c, &site);
        // Its axis stops a radius (30 cm) short of the wall.
        assert!(
            (v(9.5)..=v(9.73)).contains(&furthest) && (s - furthest).abs() < v(0.05),
            "{direction:?}: stopped at s = {s} (furthest {furthest}), wall at 10 m"
        );
        assert!(h < v(0.2), "{direction:?}: climbed to {h}");
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
            (v(1.1)..=v(1.3)).contains(&apex),
            "{direction:?}: jumped {apex} Voxels, wanted {}",
            v(1.2)
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
    assert!(apex < v(1.3), "{apex}");
}

/// A low ceiling, 1.5 m up, over `s` from 10 m to 30 m: a tunnel with no walls.
fn tunnel(direction: [f64; 3]) -> Site {
    Site::at(direction).with((v(10.0), v(30.0)), EVERYWHERE, (v(1.5), v(4.0)))
}

#[test]
fn standing_it_cannot_go_under_a_1_5_metre_ceiling_and_crouching_it_can() {
    for direction in SITES {
        let site = tunnel(direction);

        let mut c = standing(&site, 0.0, 0.0);
        run(&mut c, &site, &site.walk(), 300);
        let [s, ..] = feet(&c, &site);
        assert!(s < v(9.75), "{direction:?}: walked under it, to s = {s}");

        let mut c = standing(&site, 0.0, 0.0);
        let crouch = MoveInput {
            crouch: true,
            ..site.walk()
        };
        // 8 s at 2 m/s is 16 m.
        run(&mut c, &site, &crouch, 480);
        let [s, ..] = feet(&c, &site);
        assert!(
            s > v(15.0) && c.state().crouching,
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
        assert!(
            s > v(32.0) && !c.state().crouching,
            "{direction:?}: s = {s}"
        );
    }
}

#[test]
fn a_one_voxel_wall_is_never_gone_through_at_run_speed_whatever_the_frame_time() {
    for direction in SITES {
        // The wall is one Voxel thick, whatever a Voxel measures, at 100 Voxels.
        let site = Site::at(direction).with((100.0, 101.0), EVERYWHERE, (0.0, v(4.0)));
        let run_at = MoveInput {
            run: true,
            ..site.walk()
        };
        // The capsule's axis stops a radius short of the wall.
        let stops = 100.0 - Movement::default().radius;
        for dt in [1.0 / 60.0, 0.1, 0.25] {
            let mut c = standing(&site, 0.0, 0.0);
            let mut furthest = f64::MIN;
            // 6 s: a run covers 42 m, many times the way to the wall.
            for _ in 0..(6.0 / dt) as usize {
                c.step(&site, &run_at, dt);
                furthest = furthest.max(feet(&c, &site)[0]);
            }
            assert!(
                furthest < stops + v(0.05) && furthest > stops - 3.0,
                "{direction:?} at {dt} s a frame: got to s = {furthest}, wall at 100"
            );
        }
    }
}

#[test]
fn a_thin_floor_stops_a_fall_at_terminal_speed() {
    for direction in SITES {
        // A one-Voxel-thick platform 30 m up, over wide ground that is not solid below it.
        let (top, thick) = (v(30.0), 1.0);
        let site = Site::at(direction).with(EVERYWHERE, EVERYWHERE, (top, top + thick));
        for dt in [1.0 / 60.0, 0.25] {
            let mut c = Character::spawn(Movement::default(), site.point(0.0, 0.0, v(33.0)), None);
            for _ in 0..(3.0 / dt) as usize {
                c.step(&site, &site.input(), dt);
            }
            let [_, _, h] = feet(&c, &site);
            assert!(
                (top + 0.5..=top + 4.0).contains(&h) && c.state().grounded,
                "{direction:?} at {dt} s a frame: at h = {h}"
            );
        }
    }
}

#[test]
fn it_slides_along_a_wall_it_walks_at_slantwise() {
    for direction in SITES {
        let site = Site::at(direction).with((v(10.0), v(13.0)), EVERYWHERE, (0.0, v(3.0)));
        let mut c = standing(&site, v(9.0), 0.0);
        let slant = MoveInput {
            right: 1,
            ..site.walk()
        };
        run(&mut c, &site, &slant, 120);
        let [s, t, _] = feet(&c, &site);
        assert!(s < v(9.75), "{direction:?}: through the wall, s = {s}");
        // Two seconds along the wall at the walk's 4.5 m/s, less the part of it into the wall.
        assert!(t > v(4.0), "{direction:?}: slid only to t = {t}");
    }
}

#[test]
fn unloaded_space_is_solid_so_it_stops_at_the_edge_instead_of_falling() {
    for direction in SITES {
        let mut site = Site::at(direction);
        site.unloaded_from = Some(v(10.0));
        let mut c = standing(&site, 0.0, 0.0);
        run(&mut c, &site, &site.walk(), 360);
        let [s, _, h] = feet(&c, &site);
        assert!(
            (v(9.5)..v(9.75)).contains(&s) && h.abs() < 1.5 && c.state().grounded,
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
        let mut c = Character::spawn(Movement::default(), site.point(0.0, 0.0, v(-1.2)), None);
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
            radius: v(5.0),
        };
        let mut c = Character::spawn(
            Movement::default(),
            site.point(0.0, 0.0, v(0.6)),
            Some(leash),
        );
        run(&mut c, &site, &site.walk(), 360);
        let [s, t, h] = feet(&c, &site);
        let from_centre = (s * s + t * t + h * h).sqrt();
        // The wall is a ball, so the head, 1.8 m up, meets it before the feet do: the capsule
        // (radius 30 cm) stops where its top sphere, 1.5 m up, is 30 cm inside the ball. The feet
        // are then sqrt(4.7 * 4.7 - 1.5 * 1.5) = 4.45 m from the centre, less the controller's
        // tolerance.
        assert!(
            (v(4.3)..=v(4.46)).contains(&from_centre),
            "{direction:?}: stopped {from_centre} from the centre, leash 5 m"
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
        assert!((rise(&c) - v(1.6)).abs() < 1e-6, "{direction:?}");
        let crouch = MoveInput {
            crouch: true,
            ..site.input()
        };
        run(&mut c, &site, &crouch, 2);
        assert!((rise(&c) - v(1.0)).abs() < 1e-6, "{direction:?}");
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

/// Bricks held by coordinate, as the ground is: what is not held is unloaded.
struct BrickWorld(std::collections::BTreeMap<BrickCoord, brick_store::Brick>);

impl Occupancy for BrickWorld {
    fn cell(&self, voxel: VoxelCoord) -> Cell {
        match self.0.get(&voxel.brick()) {
            None => Cell::Unloaded,
            Some(brick) if brick.get(voxel.local()).is_some() => Cell::Solid,
            Some(_) => Cell::Air,
        }
    }

    fn content(&self, brick: BrickCoord) -> Content<'_> {
        match self.0.get(&brick).map(brick_store::Brick::view) {
            None | Some(brick_store::BrickView::Solid(_)) => Content::Solid,
            Some(brick_store::BrickView::Air) => Content::Air,
            Some(brick_store::BrickView::Mixed { occupancy, .. }) => Content::Mixed(occupancy),
        }
    }
}

/// The deepest contact as the controller found it before it asked for bricks (5b94335): every
/// Voxel of the capsule's bounds, one `cell` at a time, in the same order. Frozen here as the
/// reference that the brick-wise `contact` must equal to the bit.
#[allow(clippy::too_many_arguments)]
fn contact_per_voxel(
    world: &impl Occupancy,
    leash: Option<Leash>,
    r: f64,
    feet: [f64; 3],
    up: [f64; 3],
    height: f64,
    reach: f64,
    any: bool,
) -> Option<Contact> {
    let solid = |voxel: [i32; 3]| {
        if let Some(leash) = &leash {
            let centre = voxel.map(|c| f64::from(c) + 0.5);
            let from_centre = sub(centre, leash.centre);
            if dot(from_centre, from_centre) > leash.radius * leash.radius {
                return true;
            }
        }
        world.cell(VoxelCoord(voxel)) != Cell::Air
    };
    let a = add(feet, scale(up, r));
    let b = add(feet, scale(up, height - r));
    let lo = [0, 1, 2].map(|k| (a[k].min(b[k]) - r).floor() as i32);
    let hi = [0, 1, 2].map(|k| (a[k].max(b[k]) + r).floor() as i32);
    let mut deepest: Option<Contact> = None;
    for x in lo[0]..=hi[0] {
        for y in lo[1]..=hi[1] {
            for z in lo[2]..=hi[2] {
                let voxel = [x, y, z];
                if !solid(voxel) {
                    continue;
                }
                let min = voxel.map(f64::from);
                let max = min.map(|c| c + 1.0);
                let (dist, p) = segment_to_box(a, b, min, max);
                if dist >= reach * reach {
                    continue;
                }
                if any {
                    return Some(Contact {
                        depth: 0.0,
                        normal: up,
                    });
                }
                let contact = if dist > 0.0 {
                    let length = dist.sqrt();
                    Contact {
                        depth: reach - length,
                        normal: scale(sub(p, nearest_in_box(p, min, max)), 1.0 / length),
                    }
                } else {
                    Contact {
                        depth: r + 1.0,
                        normal: up,
                    }
                };
                if deepest.as_ref().is_none_or(|d| contact.depth > d.depth) {
                    deepest = Some(contact);
                }
            }
        }
    }
    deepest
}

/// A small deterministic generator (xorshift), so that the run is the same each time.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// In `[0, 1)`.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn between(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }
}

#[test]
fn the_contact_found_brick_by_brick_is_the_one_found_voxel_by_voxel_to_the_bit() {
    use brick_store::{Brick, Material};

    let mut random = Random(0x9E37_79B9_7F4A_7C15);
    let stone = Material(7);
    let r = Movement::default().radius;
    let mut compared = [0u32; 3];
    for _ in 0..24 {
        // A world of 4 x 4 x 4 bricks: air, solid, mixed at various densities, and some not held.
        let mut bricks = std::collections::BTreeMap::new();
        for x in -2..2 {
            for y in -2..2 {
                for z in -2..2 {
                    let brick = match random.next() % 16 {
                        0 => continue,
                        1..=8 => Brick::AIR,
                        9 => Brick::solid(stone),
                        kind => {
                            let density = [0.02, 0.1, 0.5][kind as usize % 3];
                            let mut brick = Brick::AIR;
                            for i in 0..512u32 {
                                if random.unit() < density {
                                    let local = [(i % 8) as u8, (i / 8 % 8) as u8, (i / 64) as u8];
                                    brick.set(local, Some(stone));
                                }
                            }
                            brick
                        }
                    };
                    bricks.insert(BrickCoord([x, y, z]), brick);
                }
            }
        }
        let world = BrickWorld(bricks);
        for _ in 0..400 {
            let up = [0, 1, 2].map(|_| random.between(-1.0, 1.0));
            let length = dot(up, up).sqrt();
            let up = up.map(|c| c / length);
            let mut feet = [0, 1, 2].map(|_| random.between(-8.0, 8.0));
            let leash = match random.next() % 4 {
                0 | 1 => None,
                // A wall that grazes the capsule: a sphere about a point some way off, whose
                // surface is within a few Voxels of the feet.
                2 => {
                    let away = [0, 1, 2].map(|_| random.between(-1.0, 1.0));
                    let away = away.map(|c| c / dot(away, away).sqrt() * random.between(8.0, 50.0));
                    Some(Leash {
                        centre: add(feet, away),
                        radius: (dot(away, away).sqrt() + random.between(-14.0, 14.0)).max(1.0),
                    })
                }
                // A wall that a Voxel's centre is on, or a hair either side of, with the capsule
                // beside that Voxel: the radius is the square root of a whole number of Voxels'
                // squares, less or more by nothing, 1/100 or 1/2.
                _ => {
                    // The corner of a brick that is farthest from the centre, which is what a
                    // brick is judged by.
                    let far = [0, 1, 2].map(|_| random.next() % 2 == 1);
                    let voxel = far.map(|far| {
                        let brick = (random.next() % 4) as f64 - 2.0;
                        brick * 8.0 + if far { 7.5 } else { 0.5 }
                    });
                    feet = voxel.map(|c| c + random.between(-4.0, 4.0));
                    let mut centre = voxel;
                    for k in 0..3 {
                        let by = (random.next() % 18) as f64 + 2.0;
                        centre[k] += if far[k] { -by } else { by };
                    }
                    let gap = sub(voxel, centre);
                    let offset = [0.0, 0.01, -0.01, 0.5, -0.5][random.next() as usize % 5];
                    Some(Leash {
                        centre,
                        radius: (dot(gap, gap) + offset).max(1.0).sqrt(),
                    })
                }
            };
            let height = [v(1.8), v(1.2)][random.next() as usize % 2];
            let reach = match random.next() % 4 {
                0 => r,
                1 => r - TOLERANCE,
                2 => r - STUCK,
                _ => random.between(0.5, r + 1.0),
            };
            for any in [false, true] {
                let capsule = Capsule {
                    world: &world,
                    leash,
                    radius: r,
                    reached: RefCell::new(Vec::new()),
                };
                let new = capsule.contact(feet, up, height, reach, any);
                let old = contact_per_voxel(&world, leash, r, feet, up, height, reach, any);
                match (&new, &old) {
                    (None, None) => compared[0] += 1,
                    (Some(new), Some(old)) => {
                        compared[1] += 1;
                        assert_eq!(new.depth.to_bits(), old.depth.to_bits());
                        for k in 0..3 {
                            assert_eq!(new.normal[k].to_bits(), old.normal[k].to_bits());
                        }
                    }
                    _ => panic!(
                        "brick by brick {:?}, voxel by voxel {:?} at {feet:?}, up {up:?}, \
                         height {height}, reach {reach}, any {any}, leash {leash:?}",
                        new.as_ref().map(|c| (c.depth, c.normal)),
                        old.as_ref().map(|c| (c.depth, c.normal)),
                    ),
                }
                compared[2] += u32::from(leash.is_some());
            }
        }
    }
    // Both kinds of answer, and the leash, were exercised.
    assert!(
        compared[0] >= 10 && compared[1] > 1000 && compared[2] > 1000,
        "{compared:?}"
    );
}
