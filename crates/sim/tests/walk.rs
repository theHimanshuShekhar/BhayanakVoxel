//! A client predicts its character, the server runs it for real, and both end with the same bits,
//! talking only through the loopback: past the ground that is generated at the spawn, on the
//! ground that each of them generates around the character as it goes (the leash moves with it).
//!
//! The two are stepped together, one tick of the client and then one of the server, with the
//! server's replies delivered `lag` ticks late: the sim's time is the inputs, so this is the same
//! run as two threads would make, but the same each time.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use brick_store::{Brick, BrickCoord, BrickSource, BrickStore, VoxelCoord};
use net::{BodyState, ClientEnd, EditOp, Leash, MoveInput, ServerEnd, ServerMessage, loopback};
use planet_gen::generate_brick;
use planet_gen::planet::{Planet, PlanetSource, tangents};
use planet_gen::world_map::WorldMap;
use sim::{Ground, Predictor, Server, TICK_RATE_HZ};

const SEED: u64 = 0xB4A7_A9AC;

/// The test Planet (150 m in radius), as bricks, from nothing but the seed: what a store reads
/// where it holds nothing, so that the ground can be generated from it.
#[derive(Debug)]
struct TestPlanet;

impl BrickSource for TestPlanet {
    fn brick(&self, level: usize, coord: BrickCoord) -> Brick {
        if level == 0 {
            generate_brick(SEED, coord)
        } else {
            Brick::AIR
        }
    }
}

fn test_planet() -> BrickStore {
    let mut store = BrickStore::new();
    store.set_source(TestPlanet);
    store
}

/// A Planet with a world map as the game has it, but a coarse one, which is quick to make.
fn planet() -> &'static Planet {
    static PLANET: OnceLock<Planet> = OnceLock::new();
    PLANET.get_or_init(|| Planet::new(WorldMap::generate(SEED, 512)))
}

fn real_planet() -> BrickStore {
    let mut store = BrickStore::new();
    store.set_source(PlanetSource::new(planet().clone()));
    store
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// What it costs to run a tick, over a run.
#[derive(Default)]
struct Costs(Vec<Duration>);

impl Costs {
    fn mean(&self) -> Duration {
        self.0.iter().sum::<Duration>() / self.0.len().max(1) as u32
    }

    fn p99(&self) -> Duration {
        let mut sorted = self.0.clone();
        sorted.sort();
        sorted[(sorted.len() * 99).div_ceil(100).saturating_sub(1)]
    }

    fn worst(&self) -> Duration {
        self.0.iter().copied().max().unwrap_or_default()
    }

    fn summary(&self, what: &str) -> String {
        format!(
            "{what}: {} ticks, mean {:.3?}, p99 {:.3?}, worst {:.3?}",
            self.0.len(),
            self.mean(),
            self.p99(),
            self.worst()
        )
    }
}

/// A client and a server, stepped together.
struct Pair {
    client: Predictor,
    server: Server,
    link: ClientEnd,
    server_end: ServerEnd,
    /// What the server said on each of the last ticks, which the client has yet to hear.
    in_flight: VecDeque<Vec<ServerMessage>>,
    lag: usize,
    /// Inputs the client has sent.
    sent: u64,
    /// Ticks on which the character had spawned and waited: no input sent.
    waited: u64,
    server_costs: Costs,
    client_costs: Costs,
    /// The most bytes the ground of the client and of the server have taken.
    ground_bytes: (usize, usize),
}

impl Pair {
    fn new(store: impl Fn() -> BrickStore, ground_rate: Option<usize>, lag: usize) -> Self {
        Self::with_stores(store(), store(), ground_rate, lag)
    }

    fn with_stores(
        client: BrickStore,
        server: BrickStore,
        ground_rate: Option<usize>,
        lag: usize,
    ) -> Self {
        let (link, server_end) = loopback();
        let (mut client, mut server) = (Predictor::new(client), Server::new(server));
        if let Some(rate) = ground_rate {
            client = client.with_ground_rate(rate);
            server = server.with_ground_rate(rate);
        }
        Self {
            client,
            server,
            link,
            server_end,
            in_flight: VecDeque::new(),
            lag,
            sent: 0,
            waited: 0,
            server_costs: Costs::default(),
            client_costs: Costs::default(),
            ground_bytes: (0, 0),
        }
    }

    fn spawn(&mut self, position: [f64; 3]) {
        self.link.send(self.client.spawn(position)).unwrap();
    }

    /// The client's tick, the server's, and the delivery of what the server said `lag` ticks ago.
    fn tick(&mut self, input: MoveInput) {
        self.client_tick(input);
        self.serve();
    }

    /// The client's tick alone: it sends an input, or waits.
    fn client_tick(&mut self, input: MoveInput) {
        let started = Instant::now();
        let request = self.client.walk(input);
        self.client_costs.0.push(started.elapsed());
        match request {
            Some(request) => {
                self.sent += 1;
                self.link.send(request).unwrap();
            }
            None => self.waited += u64::from(self.sent > 0),
        }
    }

    /// The server's tick, and the delivery of what it said `lag` ticks ago.
    fn serve(&mut self) {
        let started = Instant::now();
        self.server.tick(std::slice::from_ref(&self.server_end));
        self.server_costs.0.push(started.elapsed());
        self.in_flight
            .push_back(std::iter::from_fn(|| self.link.try_recv().ok()).collect());
        while self.in_flight.len() > self.lag {
            for message in self.in_flight.pop_front().into_iter().flatten() {
                self.client.receive(message);
            }
        }
        let server = self.server.ground(0).map_or(0, |g| g.bytes().1);
        self.ground_bytes = (
            self.ground_bytes.0.max(self.client.ground().bytes().1),
            self.ground_bytes.1.max(server),
        );
    }

    /// Runs ticks of `input` until the server has applied `inputs` of them (or `limit` ticks
    /// have gone), then lets the server's last words arrive.
    fn run(&mut self, inputs: u64, limit: u64, mut input: impl FnMut(&Self, u64) -> MoveInput) {
        for i in 0..limit {
            if self.sent >= inputs {
                break;
            }
            let next = input(self, i);
            self.tick(next);
        }
        for _ in 0..=self.lag {
            self.serve();
        }
    }

    /// The client digs `op`, and the server hears of it if `ask`.
    fn dig(&mut self, op: EditOp, ask: bool) {
        let (request, _) = self.client.edit(op);
        if ask {
            self.link.send(request).unwrap();
        }
    }

    fn client_state(&self) -> BodyState {
        *self.client.character().expect("spawned").state()
    }

    fn server_state(&self) -> BodyState {
        *self.server.character(0).expect("spawned").state()
    }

    /// The client's character and the server's are in the same state, to the bit, and the server
    /// never had to say otherwise.
    fn assert_agree(&self) {
        let (client, server) = (self.client_state(), self.server_state());
        assert!(
            server.identical(&client),
            "server {server:?}\nclient {client:?}"
        );
        assert_eq!(
            self.client.body_desyncs(),
            0,
            "the server had to correct it"
        );
    }
}

/// FNV-1a over the bits of the state and the leash.
fn fingerprint(state: &BodyState, leash: Leash) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let words = state
        .position
        .into_iter()
        .chain(state.velocity)
        .chain(leash.centre)
        .chain([leash.radius])
        .map(f64::to_bits)
        .chain([u64::from(state.grounded), u64::from(state.crouching)]);
    for word in words {
        for byte in word.to_le_bytes() {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

/// A scripted walk of legs of 100 inputs, each in another heading across the ground (world
/// directions, which the controller flattens onto the ground at the spawn on the +x axis), running, jumping and
/// crouching at times, and drifting a little to either side. It goes by the number of the input
/// and by nothing the character does, so that it is the same however long the character waited
/// for its ground and whoever has been corrected.
fn script(i: u64) -> MoveInput {
    let heading = [
        [0.0, 1.0, 0.0],
        [0.0, 1.0, 1.0],
        [0.0, 1.0, 0.0],
        [0.0, 1.0, -1.0],
        [0.0, 0.0, 1.0],
        [0.0, 1.0, 0.0],
    ][(i / 100 % 6) as usize];
    MoveInput {
        forward: 1,
        right: [0, 0, 1, 0, -1][(i / 70 % 5) as usize],
        heading,
        jump: i.is_multiple_of(83),
        run: i % 200 > 120,
        crouch: i % 310 > 270,
    }
}

const TICKS: u64 = 600;
const GOLDEN: u64 = 0x456e_48e7_5898_859e;
/// Bricks of ground per input, so slow that the character is held at the edge of its leash.
const SLOW: usize = 8;
const GOLDEN_AT_THE_EDGE: u64 = 0x2abe_ab1a_1140_ac35;

/// Runs the scripted walk for `inputs` inputs, with the server's replies `lag` ticks late and
/// `batch` inputs reaching the server in each of its ticks, and `rate` new bricks of ground per
/// input if not the usual. Returns where the server ends and its leash, and how close to the edge
/// of it the character got, as a fraction of the leash's radius.
fn scripted_walk(
    inputs: u64,
    lag: usize,
    batch: u64,
    rate: Option<usize>,
) -> (BodyState, Leash, f64) {
    let mut pair = Pair::new(test_planet, rate, lag);
    pair.spawn([1530.0, 0.0, 0.0]);
    // The character waits for its ground and the server's word, and then walks.
    let mut edge: f64 = 0.0;
    for _ in 0..50 * inputs {
        if pair.sent >= inputs {
            break;
        }
        for _ in 0..batch {
            if pair.sent < inputs {
                pair.client_tick(script(pair.sent));
            }
        }
        pair.serve();
        if let Some(leash) = pair.server.character(0).and_then(sim::Character::leash) {
            let from_centre = distance(pair.server_state().position, leash.centre);
            edge = edge.max(from_centre / leash.radius);
        }
    }
    assert_eq!(pair.sent, inputs);
    for _ in 0..=pair.lag {
        pair.serve();
    }
    if batch == 1 && rate.is_none() {
        pair.assert_agree();
    } else {
        // Held at the edge, or with the inputs bunched, the client runs ahead of the leashes it has
        // heard of and is corrected (the server never is): it ends in step all the same.
        assert!(pair.server_state().identical(&pair.client_state()));
    }
    let leash = pair.server.character(0).unwrap().leash();
    (pair.server_state(), leash.expect("leashed"), edge)
}

#[test]
fn golden_the_client_and_the_server_agree_on_every_bit_of_a_walk_past_the_spawn() {
    let (state, leash, _) = scripted_walk(TICKS, 3, 1, None);
    // Pinned, so that CI (Linux and Windows, debug and release) fails if any machine's
    // arithmetic differs: if the controller changes on purpose, replace it with the `actual`.
    let fingerprint = fingerprint(&state, leash);
    assert_eq!(fingerprint, GOLDEN, "actual {fingerprint:#018x}");

    // It really walked: down onto the ground and then 50 m or so along it, in several headings,
    // far past the 8 m of ground that the server makes first (and the 40 m ring that the game's
    // client generates), and the leash went with it.
    let went = distance(state.position, [1530.0, 0.0, 0.0]);
    assert!(went > 300.0, "went {went} Voxels");
    let height = distance(state.position, [0.0; 3]);
    assert!(
        (1480.0..1540.0).contains(&height),
        "{height} from the centre"
    );
    assert!(distance(state.position, leash.centre) < leash.radius);
}

#[test]
fn golden_the_client_and_the_server_agree_on_every_bit_of_a_walk_held_at_the_edge_of_the_leash() {
    let (state, leash, edge) = scripted_walk(400, 2, 1, Some(SLOW));
    let fingerprint = fingerprint(&state, leash);
    assert_eq!(
        fingerprint, GOLDEN_AT_THE_EDGE,
        "actual {fingerprint:#018x}"
    );
    // The ground came slower than the character walked, so that it pushed against the wall.
    assert!(edge > 0.9, "got to {edge} of the leash");
}

#[test]
fn what_the_server_does_with_the_inputs_does_not_depend_on_how_late_the_client_hears_of_it() {
    for lag in [0, 11] {
        let (state, leash, _) = scripted_walk(TICKS, lag, 1, None);
        assert_eq!(fingerprint(&state, leash), GOLDEN, "lag {lag}");
    }
}

/// The ground is made once per input the server applies, not once per tick of its own: so with
/// several inputs reaching it in one tick, it holds the character to the same leashes as with
/// one, and the client (which makes its ground once per input too) stays in step.
#[test]
fn the_server_makes_ground_once_per_input_not_once_per_tick() {
    for batch in [3, 8] {
        let (state, leash, _) = scripted_walk(TICKS, 3, batch, None);
        assert_eq!(fingerprint(&state, leash), GOLDEN, "{batch} inputs a tick");
        let (state, leash, _) = scripted_walk(400, 2, batch, Some(SLOW));
        assert_eq!(
            fingerprint(&state, leash),
            GOLDEN_AT_THE_EDGE,
            "{batch} inputs a tick, held at the edge"
        );
    }
}

/// The real Planet's gentle land around its spawn: where the spawn is, the way ahead along the
/// ground, and the axis of the great circle that goes that way.
fn spawn_and_way() -> ([f64; 3], [f64; 3]) {
    let spawn = planet().spawn(100);
    let length = spawn.iter().map(|c| c * c).sum::<f64>().sqrt();
    let up = spawn.map(|c| c / length);
    let (ahead, _) = tangents(up);
    let start = std::array::from_fn(|a| spawn[a] + up[a] * 3.0);
    (start, cross(up, ahead))
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3)
        .map(|k| (a[k] - b[k]) * (a[k] - b[k]))
        .sum::<f64>()
        .sqrt()
}

#[test]
fn a_character_runs_a_kilometre_from_the_spawn_without_stopping_while_the_ground_keeps_up() {
    let (start, axis) = spawn_and_way();
    let mut pair = Pair::new(real_planet, None, 2);
    pair.spawn(start);
    // 7 m/s for 150 s, and a little to spare.
    let inputs = 150 * u64::from(TICK_RATE_HZ);
    pair.run(inputs, 2 * inputs, |pair, _| MoveInput {
        forward: 1,
        heading: cross(axis, pair.client_state().position),
        run: true,
        ..MoveInput::default()
    });
    eprintln!("{}", pair.server_costs.summary("server tick"));
    eprintln!("{}", pair.client_costs.summary("client tick"));
    eprintln!(
        "ground bytes at the most (client, server): {:?}",
        pair.ground_bytes
    );
    let went = distance(pair.server_state().position, start);
    eprintln!(
        "went {:.0} m in {} inputs, waited {} ticks",
        went / 10.0,
        pair.sent,
        pair.waited
    );
    assert!(went >= 10_000.0, "went {} m", went / 10.0);
    assert_eq!(pair.waited, 0, "stopped to wait for the ground");
    pair.assert_agree();

    // Each tick, the server's and the client's, stays within the time it has, with the
    // generation of the ground in it. The worst is reported: one slow tick is a machine's
    // doing as much as the code's, so only the mean and the 99th percentile are held to it.
    let tick = Duration::from_secs(1) / TICK_RATE_HZ;
    for costs in [&pair.server_costs, &pair.client_costs] {
        assert!(costs.mean() < tick / 2, "{}", costs.summary("tick"));
        assert!(costs.p99() < tick, "{}", costs.summary("tick"));
    }
    // The ground of each takes a bounded amount of memory (a few MiB walking; never more than
    // `Ground::MAX_BYTES`).
    assert!(pair.ground_bytes.0.max(pair.ground_bytes.1) < Ground::MAX_BYTES);
}

/// A source that counts the bricks it is asked for.
#[derive(Debug)]
struct Counting(PlanetSource, Arc<AtomicUsize>);

impl BrickSource for Counting {
    fn brick(&self, level: usize, coord: BrickCoord) -> Brick {
        self.1.fetch_add(1, Ordering::SeqCst);
        self.0.brick(level, coord)
    }
}

#[test]
fn nothing_is_generated_for_a_collision_query_only_for_the_ground() {
    let (start, axis) = spawn_and_way();
    let made = Arc::new(AtomicUsize::new(0));
    let store = || {
        let mut store = BrickStore::new();
        store.set_source(Counting(PlanetSource::new(planet().clone()), made.clone()));
        store
    };
    let mut pair = Pair::new(store, None, 0);
    pair.spawn(start);
    // Stands on the ground until all of it is there...
    pair.run(120, 1000, |_, _| MoveInput::default());
    let ground = made.load(Ordering::SeqCst);
    assert!(ground > 0);
    // ...and then walks to and fro within it, and nothing is made at all.
    pair.run(420, 1000, |pair, i| MoveInput {
        forward: if i / 20 % 2 == 0 { 1 } else { -1 },
        heading: cross(axis, pair.client_state().position),
        ..MoveInput::default()
    });
    assert_eq!(made.load(Ordering::SeqCst), ground);
    pair.assert_agree();
}

#[test]
fn with_a_slow_generator_the_character_stops_at_the_edge_of_the_ground_and_never_falls() {
    let (start, axis) = spawn_and_way();
    // Twelve bricks per input: about a fifth of what running needs.
    let mut pair = Pair::new(real_planet, Some(12), 2);
    pair.spawn(start);
    let (mut stopped, mut last) = (0, start);
    pair.run(1500, 5000, |pair, _| {
        let character = pair.server.character(0);
        let position = character.map_or(start, |c| c.state().position);
        // It is never out of its leash, nor in the ground.
        if let Some(leash) = character.and_then(sim::Character::leash) {
            assert!(
                distance(position, leash.centre) <= leash.radius,
                "{position:?}"
            );
            assert!(
                planet()
                    .voxel(VoxelCoord(position.map(|c| c.floor() as i32)))
                    .is_none()
            );
            stopped += i32::from(distance(position, last) < 0.01);
        }
        last = position;
        MoveInput {
            forward: 1,
            heading: cross(axis, pair.client_state().position),
            run: true,
            ..MoveInput::default()
        }
    });
    let went = distance(pair.server_state().position, start) / 10.0;
    eprintln!(
        "went {went:.0} m in 1500 inputs, stopped on {stopped}, {} desyncs",
        pair.client.body_desyncs()
    );
    // 25 s of running is 175 m; it waited for the ground instead, and got some of the way.
    assert!(stopped > 500, "stopped on {stopped} inputs");
    assert!((10.0..60.0).contains(&went), "went {went} m");
    assert_eq!(
        pair.server_state().position.map(f64::to_bits),
        pair.client_state().position.map(f64::to_bits)
    );
}

/// A Voxel of the real Planet's ground 200 m from its spawn, and the way up there.
fn ground_200_m_away() -> (VoxelCoord, [f64; 3]) {
    let (start, axis) = spawn_and_way();
    let length = start.iter().map(|c| c * c).sum::<f64>().sqrt();
    let up = start.map(|c| c / length);
    let ahead = cross(axis, up);
    let point: [f64; 3] = std::array::from_fn(|a| start[a] + ahead[a] * 2000.0);
    let radius = point.iter().map(|c| c * c).sum::<f64>().sqrt();
    let at = |r: f64| VoxelCoord(point.map(|c| (c / radius * r).round() as i32));
    let surface = (0..4000)
        .map(|d| radius + 800.0 - f64::from(d) * 0.5)
        .find(|&r| planet().voxel(at(r)).is_some())
        .expect("ground under the point");
    (at(surface), point.map(|c| c / radius))
}

/// Whether the two stores read the same Voxels in the 5 bricks cube around `centre`.
fn read_alike(client: &BrickStore, server: &BrickStore, centre: VoxelCoord) -> bool {
    let first = centre.brick();
    (-2..=2).all(|dx| {
        (-2..=2).all(|dy| {
            (-2..=2).all(|dz| {
                let brick = BrickCoord([first.0[0] + dx, first.0[1] + dy, first.0[2] + dz]);
                (0..512).all(|i| {
                    let v = brick.voxel([(i & 7) as u8, (i >> 3 & 7) as u8, (i >> 6) as u8]);
                    client.voxel_at(0, v) == server.voxel_at(0, v)
                })
            })
        })
    })
}

#[test]
fn a_dig_200_m_from_the_spawn_is_the_same_on_the_client_and_the_server_and_so_is_taking_it_back() {
    let (centre, _) = ground_200_m_away();
    let mut pair = Pair::new(real_planet, None, 2);
    let pristine = real_planet().content_hash();

    // Made and asked for: the server makes it too, and tells the client, which has no Edit left
    // to settle.
    let dug = EditOp::dig_sphere(centre, 10);
    pair.dig(dug, true);
    assert_eq!(pair.client.pending(), 1);
    for _ in 0..4 {
        pair.serve();
    }
    assert_eq!((pair.client.pending(), pair.client.desyncs()), (0, 0));
    let dug_hash = pair.server.store().content_hash();
    assert_ne!(dug_hash, pristine);
    assert_eq!(pair.client.store().content_hash(), dug_hash);
    assert_eq!(pair.client.store().voxel_at(0, centre), None);
    assert!(planet().voxel(centre).is_some(), "there was ground to dig");
    assert!(read_alike(pair.client.store(), pair.server.store(), centre));

    // Made and then turned down: the client takes it back, and the two agree again.
    let beside = VoxelCoord(centre.0.map(|c| c + 12));
    pair.dig(EditOp::dig_sphere(beside, 10), false);
    assert_ne!(pair.client.store().content_hash(), dug_hash);
    pair.client.receive(ServerMessage::Rejected {
        client_seq: 1,
        reason: net::RejectReason::BrushTooLarge,
    });
    assert_eq!(pair.client.pending(), 0);
    assert_eq!(pair.client.store().content_hash(), dug_hash);
    assert!(read_alike(pair.client.store(), pair.server.store(), centre));
}

#[test]
fn a_dig_taken_back_beyond_the_spawn_in_a_client_that_streams_leaves_it_reading_as_the_server_does()
{
    let (centre, _) = ground_200_m_away();
    // The client streams the Planet in, as the game's does, and the server does not.
    let mut client = real_planet();
    client.build_lod(2);
    client.stream_from(1);
    let mut pair = Pair::with_stores(client, real_planet(), None, 2);
    pair.dig(EditOp::dig_sphere(centre, 10), false);
    assert_eq!(pair.client.store().voxel_at(0, centre), None);
    pair.client.receive(ServerMessage::Rejected {
        client_seq: 0,
        reason: net::RejectReason::BrushTooLarge,
    });
    assert!(pair.client.store().voxel_at(0, centre).is_some());
    assert!(read_alike(pair.client.store(), pair.server.store(), centre));
}

#[test]
fn a_character_falls_into_a_hole_dug_under_it_beyond_the_spawn_and_both_sides_agree() {
    let (centre, up) = ground_200_m_away();
    let feet: [f64; 3] = std::array::from_fn(|a| f64::from(centre.0[a]) + up[a] * 3.0);
    let mut pair = Pair::new(real_planet, None, 2);
    pair.spawn(feet);
    pair.run(120, 1000, |_, _| MoveInput::default());
    let height = |position: [f64; 3]| (0..3).map(|a| position[a] * up[a]).sum::<f64>();
    let before = height(pair.server_state().position);
    assert!((before - height(feet)).abs() < 8.0, "stood on the ground");

    pair.dig(EditOp::dig_sphere(centre, 10), true);
    pair.run(240, 1000, |_, _| MoveInput::default());
    let after = height(pair.server_state().position);
    assert!(after < before - 6.0, "fell {} Voxels", before - after);
    pair.assert_agree();
}

#[test]
fn a_leash_from_before_a_respawn_that_arrives_after_it_is_ignored_and_the_walk_goes_on() {
    let (link, server_end) = loopback();
    let mut client = Predictor::new(test_planet());
    let mut server = Server::new(test_planet());
    // Spawns at A, and the server's first word about it is held back...
    link.send(client.spawn([1530.0, 0.0, 0.0])).unwrap();
    let mut held = Vec::new();
    while !held
        .iter()
        .any(|m| matches!(m, ServerMessage::Leash { .. }))
    {
        // The client makes the ground round A meanwhile.
        client.walk(MoveInput::default());
        server.tick(std::slice::from_ref(&server_end));
        held.extend(std::iter::from_fn(|| link.try_recv().ok()));
    }
    // ...while the client respawns at B, nearby (noclip off and on), and then it arrives.
    link.send(client.spawn([1530.0, 40.0, 0.0])).unwrap();
    for message in held {
        client.receive(message);
    }
    let mut sent = 0;
    for _ in 0..1500 {
        if sent == 300 {
            break;
        }
        let [x, y, _] = client.character().unwrap().state().position;
        let input = MoveInput {
            forward: 1,
            heading: [-y, x, 0.0],
            ..MoveInput::default()
        };
        if let Some(request) = client.walk(input) {
            sent += 1;
            link.send(request).unwrap();
        }
        server.tick(std::slice::from_ref(&server_end));
        while let Ok(message) = link.try_recv() {
            client.receive(message);
        }
    }
    eprintln!(
        "applied {:?} desyncs {}",
        client.character().unwrap().state().position,
        client.body_desyncs()
    );
    assert_eq!(sent, 300);
    server.tick(std::slice::from_ref(&server_end));
    while let Ok(message) = link.try_recv() {
        client.receive(message);
    }
    let (server, client_state) = (
        *server.character(0).unwrap().state(),
        *client.character().unwrap().state(),
    );
    assert!(
        server.identical(&client_state),
        "{server:?}\n{client_state:?}"
    );
    assert!(server.position[1] > 100.0, "the server's character walked");
}
