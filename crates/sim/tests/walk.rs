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
        self.serve();
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

/// A scripted walk along the great circle round the test Planet's +z axis, running, jumping and
/// crouching at times, and drifting a little to either side. It goes by the number of the input,
/// so that it is the same however long the character waited for its ground.
fn script(pair: &Pair, _tick: u64) -> MoveInput {
    let i = pair.sent;
    let [x, y, _] = pair.client_state().position;
    MoveInput {
        forward: 1,
        right: [0, 0, 1, 0, -1][(i / 70 % 5) as usize],
        heading: [-y, x, 0.0],
        jump: i.is_multiple_of(83),
        run: i % 200 > 120,
        crouch: i % 310 > 270,
    }
}

const TICKS: u64 = 600;
const GOLDEN: u64 = 0xcdf2_c703_d4ca_2ca8;

/// The scripted walk, with the server's replies `lag` ticks late: where the server ends, and its
/// leash.
fn scripted_walk(lag: usize) -> (BodyState, Leash) {
    let mut pair = Pair::new(test_planet, None, lag);
    pair.spawn([1530.0, 0.0, 0.0]);
    // The character waits for its ground and the server's word, and then walks.
    pair.run(TICKS, 5 * TICKS, script);
    assert_eq!(pair.sent, TICKS);
    pair.assert_agree();
    let leash = pair.server.character(0).unwrap().leash();
    (pair.server_state(), leash.expect("leashed"))
}

#[test]
fn golden_the_client_and_the_server_agree_on_every_bit_of_a_walk_past_the_spawn() {
    let (state, leash) = scripted_walk(3);
    // Pinned, so that CI (Linux and Windows, debug and release) fails if any machine's
    // arithmetic differs: if the controller changes on purpose, replace it with the `actual`.
    let fingerprint = fingerprint(&state, leash);
    assert_eq!(fingerprint, GOLDEN, "actual {fingerprint:#018x}");

    // It really walked: down onto the ground and then 50 m or so along it, far past the 8 m of
    // ground that the server makes first (and the 40 m ring that the game's client generates),
    // and the leash went with it.
    let went = distance(state.position, [1530.0, 0.0, 0.0]);
    assert!(went > 450.0, "went {went} Voxels");
    let height = distance(state.position, [0.0; 3]);
    assert!(
        (1480.0..1540.0).contains(&height),
        "{height} from the centre"
    );
    assert!(distance(state.position, leash.centre) < leash.radius);
}

#[test]
fn what_the_server_does_with_the_inputs_does_not_depend_on_how_late_the_client_hears_of_it() {
    let (state, leash) = scripted_walk(0);
    assert_eq!(fingerprint(&state, leash), GOLDEN);
    let (state, leash) = scripted_walk(11);
    assert_eq!(fingerprint(&state, leash), GOLDEN);
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
