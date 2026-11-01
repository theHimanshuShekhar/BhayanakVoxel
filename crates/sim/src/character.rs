//! The character controller: a capsule swept against the Voxel occupancy grid (decision in #16).
//!
//! The same code runs on the client, to predict, and on the server, which is the authority; it
//! must give them the same bits, so it uses only the operations that IEEE-754 pins to one result
//! (`+ - * /`, `sqrt`, `floor`, `ceil`, `min`, `max`, comparisons) and no transcendentals or
//! fused multiply-add. A test reads this file for the ones that are not.
//!
//! # The capsule
//!
//! A segment from one radius above the feet to one radius below the top of the head, along the
//! Planet's radial up at the feet, thickened by the radius. It is tested against the grid
//! exactly: a Voxel is solid or not, and the capsule touches it if the segment comes closer to
//! the Voxel's box than the radius ([`segment_to_box`]).
//!
//! # A step
//!
//! Time is cut into steps of at most 1/240 s ([`SUBSTEPS_PER_SECOND`]), in which the capsule
//! moves at most 0.3 Voxels along the ground and 2 when falling at terminal speed: less than its
//! radius (3), so it cannot pass through anything a Voxel thick between one test and the next.
//! In each step:
//! 1. If the capsule is inside the ground, it is lifted out, slowly, and nothing else happens.
//! 2. Crouching is taken (always), or let go of (only if there is room to stand).
//! 3. The velocity across the ground is the keys' (on the ground), or pulled towards them (in
//!    the air); a jump sets the velocity along up.
//! 4. It moves across the ground. If that is blocked and it is on the ground, it tries the same
//!    move raised by the step height, and if that is clear comes down onto what is there (an
//!    auto-step). Otherwise it slides: pushed out of each contact along the contact's normal,
//!    which keeps the part of the move that is along the wall.
//! 5. On the ground, it comes down to the ground if that is within a step below (walking down
//!    stairs and over the Planet's curve), else it is in the air.
//! 6. Gravity moves it along up, and a ceiling or the ground stops it where it touches. The
//!    place where it touches is found by bisection, so it always ends clear of the Voxels.
//!
//! Standing still, nothing moves it sideways: the fall is straight down along up, so it does
//! not slide down a slope of the Voxel staircase it stands on.

use brick_store::VoxelCoord;
use net::{BodyState, Leash, MoveInput};

use crate::Movement;

/// How long a tick is: the server's step, and the client's.
pub const TICK_DT: f64 = 1.0 / 60.0;

/// Steps per second at least: see the module documentation.
const SUBSTEPS_PER_SECOND: f64 = 240.0;
/// The longest frame taken in one call; more is dropped.
const MAX_DT: f64 = 1.0;
/// A gap this small, in Voxels, between the feet and the ground counts as standing on it.
const SKIN: f64 = 0.02;
/// Rounds of bisection that find where the capsule touches something: the distance searched,
/// divided by 2^14.
const BISECTIONS: usize = 14;
/// How far into a Voxel the capsule may be without counting as touching it, in Voxels (1 mm).
const TOLERANCE: f64 = 0.01;
/// How far into a Voxel the capsule must be at the start of a step to count as stuck, in Voxels
/// (5 mm). More than [`TOLERANCE`], because the radial up turns as the capsule walks and tips
/// a capsule that rests on the ground into it by a hair.
const STUCK: f64 = 0.05;
/// How fast a character that is inside the ground is lifted out of it, in Voxels per step.
const UNSTICK: f64 = 0.25;
/// Rounds of pushing out of contacts before giving up and staying where it was.
const PUSH_ROUNDS: usize = 4;

/// What a Voxel is, as far as a character is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell {
    Air,
    Solid,
    /// Not known (not generated or not streamed in yet). Treated as solid, so that a character
    /// stops at the edge of what exists rather than falling through what does not.
    Unloaded,
}

/// The seam between the controller and wherever the Voxels are held: the ground that a client
/// and the server hold ([`crate::Terrain`]) provides it, and the controller reads nothing else of
/// the Planet. It must answer from what is held, and never generate anything.
pub trait Occupancy {
    fn cell(&self, voxel: VoxelCoord) -> Cell;
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f64; 3], k: f64) -> [f64; 3] {
    [a[0] * k, a[1] * k, a[2] * k]
}

fn finite(v: [f64; 3]) -> bool {
    v.iter().all(|c| c.is_finite())
}

fn unit_or_y(v: [f64; 3]) -> [f64; 3] {
    let length = dot(v, v).sqrt();
    if length < 1e-6 {
        [0.0, 1.0, 0.0]
    } else {
        scale(v, 1.0 / length)
    }
}

fn nearest_in_box(p: [f64; 3], lo: [f64; 3], hi: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|k| p[k].max(lo[k]).min(hi[k]))
}

/// The squared distance from the segment `a`..`b` to the box `lo`..`hi`, and the point of the
/// segment that is closest.
///
/// The squared distance from a point to a box is a sum over the axes of a square that is zero
/// inside the box's slab and grows linearly outside it, so along the segment it is convex and
/// piecewise quadratic, with pieces where the segment crosses a slab's faces. The minimum is
/// found piece by piece, in closed form: no iteration, and only `+ - * /` and comparisons.
fn segment_to_box(a: [f64; 3], b: [f64; 3], lo: [f64; 3], hi: [f64; 3]) -> (f64, [f64; 3]) {
    let d = sub(b, a);
    // The ends and where the segment crosses a face of the box: at most eight numbers.
    let mut ts = [0.0f64; 8];
    let mut n = 1;
    for k in 0..3 {
        if d[k] != 0.0 {
            for edge in [lo[k], hi[k]] {
                let t = (edge - a[k]) / d[k];
                if t > 0.0 && t < 1.0 {
                    ts[n] = t;
                    n += 1;
                }
            }
        }
    }
    ts[n] = 1.0;
    n += 1;
    for i in 1..n {
        let mut j = i;
        while j > 0 && ts[j - 1] > ts[j] {
            ts.swap(j - 1, j);
            j -= 1;
        }
    }
    let mut best = (f64::INFINITY, a);
    for w in 0..n - 1 {
        let (t0, t1) = (ts[w], ts[w + 1]);
        let middle = add(a, scale(d, (t0 + t1) * 0.5));
        // On this piece the distance is the length of `c + s t`, per axis.
        let (mut cs, mut ss) = (0.0, 0.0);
        for k in 0..3 {
            let (c, s) = if middle[k] < lo[k] {
                (lo[k] - a[k], -d[k])
            } else if middle[k] > hi[k] {
                (a[k] - hi[k], d[k])
            } else {
                (0.0, 0.0)
            };
            cs += c * s;
            ss += s * s;
        }
        let t = if ss > 0.0 {
            (-cs / ss).max(t0).min(t1)
        } else {
            t0
        };
        let p = add(a, scale(d, t));
        let gap = sub(p, nearest_in_box(p, lo, hi));
        let dist = dot(gap, gap);
        if dist < best.0 {
            best = (dist, p);
        }
    }
    best
}

/// What the capsule is pressing into: how deep, and which way out.
struct Contact {
    depth: f64,
    normal: [f64; 3],
}

/// The capsule and what it can touch: reads the world, with space beyond the leash solid.
struct Capsule<'a, W> {
    world: &'a W,
    leash: Option<Leash>,
    radius: f64,
}

impl<W: Occupancy> Capsule<'_, W> {
    fn solid(&self, voxel: [i32; 3]) -> bool {
        if let Some(leash) = &self.leash {
            let centre = voxel.map(|c| f64::from(c) + 0.5);
            let from_centre = sub(centre, leash.centre);
            if dot(from_centre, from_centre) > leash.radius * leash.radius {
                return true;
            }
        }
        self.world.cell(VoxelCoord(voxel)) != Cell::Air
    }

    /// The deepest contact of the capsule standing on `feet` with Voxels, `height` tall along
    /// `up`; or, with `any`, whichever is found first. `None` if it is clear.
    fn contact(
        &self,
        feet: [f64; 3],
        up: [f64; 3],
        height: f64,
        reach: f64,
        any: bool,
    ) -> Option<Contact> {
        let r = self.radius;
        let a = add(feet, scale(up, r));
        let b = add(feet, scale(up, height - r));
        let lo = [0, 1, 2].map(|k| (a[k].min(b[k]) - r).floor() as i32);
        let hi = [0, 1, 2].map(|k| (a[k].max(b[k]) + r).floor() as i32);
        let mut deepest: Option<Contact> = None;
        for x in lo[0]..=hi[0] {
            for y in lo[1]..=hi[1] {
                for z in lo[2]..=hi[2] {
                    let voxel = [x, y, z];
                    if !self.solid(voxel) {
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
                        // The axis is inside the Voxel: out of it is any way; up is as good.
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

    /// Whether the capsule is pressing into a Voxel by more than [`TOLERANCE`].
    fn overlaps(&self, feet: [f64; 3], up: [f64; 3], height: f64) -> bool {
        self.contact(feet, up, height, self.radius - TOLERANCE, true)
            .is_some()
    }

    /// The farthest the capsule gets going from `free`, where it is clear, towards `blocked`,
    /// where it is not: found by bisection, so it ends clear and within a 2^14th of the way of
    /// touching. It stops where it is a full radius from every Voxel, which is [`TOLERANCE`]
    /// short of where it would count as overlapping, so that the radial up turning as it moves
    /// along the Planet cannot tip a resting capsule into the ground.
    fn first_contact(
        &self,
        free: [f64; 3],
        blocked: [f64; 3],
        up: [f64; 3],
        height: f64,
    ) -> [f64; 3] {
        let way = sub(blocked, free);
        let (mut near, mut far) = (0.0, 1.0);
        for _ in 0..BISECTIONS {
            let middle = (near + far) * 0.5;
            let at = add(free, scale(way, middle));
            if self.contact(at, up, height, self.radius, true).is_some() {
                far = middle;
            } else {
                near = middle;
            }
        }
        add(free, scale(way, near))
    }

    /// Moves the capsule from `from`, where it is clear, by the step `step` along the ground, as
    /// far as it goes: onto a step of up to `climb` Voxels if `grounded`, else sliding along
    /// what is in the way, else not at all.
    fn walk(
        &self,
        from: [f64; 3],
        step: [f64; 3],
        up: [f64; 3],
        height: f64,
        climb: f64,
        grounded: bool,
    ) -> [f64; 3] {
        let to = add(from, step);
        if !self.overlaps(to, up, height) {
            return to;
        }
        if grounded {
            // Lifted by the most it may climb, is it clear a radius further on too? Then it is
            // above a step it can stand on, not only above a corner that its rounded base can
            // graze, and it comes down onto it.
            let lifted = add(to, scale(up, climb));
            let length = dot(step, step).sqrt();
            let beyond = add(lifted, scale(step, self.radius / length));
            if !self.overlaps(lifted, up, height) && !self.overlaps(beyond, up, height) {
                return self.first_contact(lifted, to, up, height);
            }
        }
        // Slide: out of each contact along its normal, until clear.
        let mut p = to;
        for _ in 0..PUSH_ROUNDS {
            match self.contact(p, up, height, self.radius, false) {
                None => break,
                Some(c) => p = add(p, scale(c.normal, c.depth)),
            }
        }
        // Pushing out never sends it further than it was going, or through to the other side.
        let went = sub(p, from);
        let most = dot(step, step).sqrt() * 1.1 + 2.0 * TOLERANCE;
        if finite(p) && !self.overlaps(p, up, height) && dot(went, went) <= most * most {
            p
        } else {
            from
        }
    }
}

/// The direction the keys ask for on the ground at `up`: a unit vector, or zero.
fn wish_direction(input: &MoveInput, up: [f64; 3]) -> [f64; 3] {
    let forward = f64::from(input.forward.clamp(-1, 1));
    let right = f64::from(input.right.clamp(-1, 1));
    if (forward == 0.0 && right == 0.0) || !finite(input.heading) {
        return [0.0; 3];
    }
    let flat = sub(input.heading, scale(up, dot(input.heading, up)));
    let length = dot(flat, flat).sqrt();
    if length.is_nan() || length <= 1e-9 {
        return [0.0; 3];
    }
    let ahead = scale(flat, 1.0 / length);
    let across = cross(ahead, up);
    let wish = add(scale(ahead, forward), scale(across, right));
    let wish = scale(wish, 1.0 / (forward * forward + right * right).sqrt());
    if finite(wish) { wish } else { [0.0; 3] }
}

/// `from` moved towards `target` by at most `most`.
fn approach(from: [f64; 3], target: [f64; 3], most: f64) -> [f64; 3] {
    let gap = sub(target, from);
    let length = dot(gap, gap).sqrt();
    if length <= most {
        target
    } else {
        add(from, scale(gap, most / length))
    }
}

#[derive(Clone, Debug)]
pub struct Character {
    movement: Movement,
    state: BodyState,
    leash: Option<Leash>,
}

impl Character {
    /// A character standing (falling, if there is nothing under it) at `position`.
    pub fn spawn(movement: Movement, position: [f64; 3], leash: Option<Leash>) -> Self {
        Self {
            movement,
            state: BodyState {
                position,
                velocity: [0.0; 3],
                grounded: false,
                crouching: false,
            },
            leash,
        }
    }

    pub fn state(&self) -> &BodyState {
        &self.state
    }

    pub fn leash(&self) -> Option<Leash> {
        self.leash
    }

    /// Changes what bounds the character, from its next tick on.
    pub fn set_leash(&mut self, leash: Option<Leash>) {
        self.leash = leash;
    }

    /// Replaces the state, as the server's word replaces a client's prediction.
    pub fn set_state(&mut self, state: BodyState) {
        self.state = state;
    }

    /// The Planet's radial up at the character.
    pub fn up(&self) -> [f64; 3] {
        unit_or_y(self.state.position)
    }

    /// Where the eyes are: the camera of a first-person view.
    pub fn eye(&self) -> [f64; 3] {
        let height = if self.state.crouching {
            self.movement.crouch_eye_height
        } else {
            self.movement.eye_height
        };
        add(self.state.position, scale(self.up(), height))
    }

    /// Advances one tick.
    pub fn tick(&mut self, world: &impl Occupancy, input: &MoveInput) {
        self.step(world, input, TICK_DT);
    }

    /// Advances `dt` seconds with `input` held throughout, in as many small steps as it takes
    /// to keep the capsule from jumping over anything.
    pub fn step(&mut self, world: &impl Occupancy, input: &MoveInput, dt: f64) {
        if dt.is_nan() || dt <= 0.0 || !finite(self.state.position) || !finite(self.state.velocity)
        {
            return;
        }
        let dt = dt.min(MAX_DT);
        let steps = (dt * SUBSTEPS_PER_SECOND - 1e-6).ceil().max(1.0);
        let each = dt / steps;
        let capsule = Capsule {
            world,
            leash: self.leash,
            radius: self.movement.radius,
        };
        for _ in 0..steps as usize {
            self.substep(&capsule, input, each);
        }
    }

    fn substep(&mut self, capsule: &Capsule<'_, impl Occupancy>, input: &MoveInput, dt: f64) {
        let m = self.movement;
        let up = self.up();
        let mut s = self.state;

        let height = if s.crouching {
            m.crouch_height
        } else {
            m.height
        };
        if capsule
            .contact(s.position, up, height, m.radius - STUCK, true)
            .is_some()
        {
            // Inside the ground (set down there, or it changed): lifted out, slowly.
            s.position = add(s.position, scale(up, UNSTICK));
            s.velocity = [0.0; 3];
            s.grounded = false;
            self.state = s;
            return;
        }
        // Crouching is always possible; standing again needs the room.
        if input.crouch {
            s.crouching = true;
        } else if s.crouching && !capsule.overlaps(s.position, up, m.height) {
            s.crouching = false;
        }
        let height = if s.crouching {
            m.crouch_height
        } else {
            m.height
        };

        // Velocity along up and across it. On the ground the keys set the latter outright; in
        // the air they only pull it.
        let along = dot(s.velocity, up);
        let mut across = sub(s.velocity, scale(up, along));
        // On the ground there is none along up, however the up turned since the last step.
        let mut climb = if s.grounded { 0.0 } else { along };
        let speed = if s.crouching {
            m.crouch_speed
        } else if input.run {
            m.run_speed
        } else {
            m.walk_speed
        };
        let wanted = scale(wish_direction(input, up), speed);
        across = if s.grounded {
            wanted
        } else {
            approach(across, wanted, m.air_acceleration * dt)
        };
        if s.grounded && input.jump {
            climb = m.jump_speed;
            s.grounded = false;
        }

        // Across the ground first: onto a step, along a wall, or off an edge.
        let was_grounded = s.grounded;
        let from = s.position;
        let intended = scale(across, dt);
        let mut position = capsule.walk(from, intended, up, height, m.step_height, was_grounded);
        if !was_grounded {
            // What a wall stopped in the air is not carried on into it.
            let went = sub(position, from);
            if dot(went, went) < dot(intended, intended) * 0.99 {
                across = scale(sub(went, scale(up, dot(went, up))), 1.0 / dt);
            }
        }

        // On the ground, down to it if it is within a step below, else off the edge.
        if was_grounded && climb <= 0.0 {
            let touching = capsule.overlaps(sub(position, scale(up, SKIN)), up, height);
            if !touching {
                let low = sub(position, scale(up, m.step_height));
                if capsule.overlaps(low, up, height) {
                    position = capsule.first_contact(position, low, up, height);
                } else {
                    s.grounded = false;
                }
            }
        }

        // Gravity, and the ground or ceiling it meets.
        if s.grounded && climb <= 0.0 {
            climb = 0.0;
        } else {
            let next = (climb - m.gravity * dt).max(-m.terminal_speed);
            let to = add(position, scale(up, (climb + next) * 0.5 * dt));
            if capsule.overlaps(to, up, height) {
                s.grounded = climb + next < 0.0;
                position = capsule.first_contact(position, to, up, height);
                climb = 0.0;
            } else {
                position = to;
                s.grounded = false;
                climb = next;
            }
        }

        s.position = position;
        s.velocity = add(across, scale(up, climb));
        self.state = s;
    }
}

#[cfg(test)]
mod tests;
