//! The server simulation: owns the Planet's bricks, validates and applies the Edits that
//! clients ask for, and tells every client about each one that it accepted.
//!
//! [`Server`] is the logic, driven by messages and with no clock, so tests call it directly.
//! [`spawn`] runs it on its own thread at a fixed tick rate behind a set of [`ServerEnd`]s: the
//! in-process single-player server today, and the dedicated server once renet carries the
//! messages instead of the loopback.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use brick_store::{BrickStore, VOXELS_PER_METRE, VoxelCoord};
use net::{
    ClientMessage, EditMode, EditOp, Leash, MoveInput, RejectReason, ServerEnd, ServerMessage,
};

use crate::{
    BrickCache, Character, EditBudget, FixedTimestep, Footing, Ground, Movement, Reads,
    apply_plan_from, dig_ticks, plan_bricks_from, target_hardness, validate, within_reach, work,
};

/// The server simulates at this rate (issue 11).
pub const TICK_RATE_HZ: u32 = 60;
/// After a stall, run at most this many ticks to catch up and drop the rest.
const MAX_TICKS_PER_ADVANCE: u32 = 5;

/// What the server decided about one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Applied {
        global_seq: u64,
        op: EditOp,
        /// The `client_seq` of the request, for the client that made it.
        client_seq: u64,
    },
    Rejected {
        client_seq: u64,
        reason: RejectReason,
    },
}

/// How far inside a leash the character must be for the server to hold it to that leash, in
/// Voxels: its height (1.8 m) and some to spare (2.4 m), so that a leash never takes the ground
/// from under or over it.
const CLEARANCE: f64 = 2.4 * VOXELS_PER_METRE;

/// Inputs held for a character that has no leash yet, at the most (ten seconds of them): see
/// [`ClientMessage::Spawn`]. It is also how far past the next input due a `seq` may be.
const MAX_QUEUED: usize = 600;

/// How many inputs a client may have applied at once, after a rest: how far a client that was held
/// up (or a tick that was late) can catch up in a tick.
const INPUT_BURST: u32 = 4;

/// What one input costs of a client's credit, and what a tick gives back, in eighths of an input.
/// A tick refills nine eighths, more than the one input a tick that an honest client uses, so that
/// the burst is built up again (in about half a second) after a catch-up, and the next stall does
/// not drop inputs. This deliberately lets a modified client move up to 12.5% faster than the
/// tick rate, over a second at most [`TICK_RATE_HZ`] * 9 / 8 inputs. A later check of the speed
/// that the character may move at can close that.
const INPUT_COST: u32 = 8;
const INPUT_REFILL: u32 = 9;

/// How many ticks early a Dig may be, against the time that it should wait ([`dig_ticks`]): the
/// jitter of the network and of the server's own ticks. It is not a gain: the time of the last
/// Dig moves on by the full wait, so being early now makes the next one later.
const DIG_EARLY_TICKS: u64 = 3;

/// What the server keeps of each client. It outlasts a spawn, so that spawning again is no way
/// round a limit.
struct Player {
    /// What the client may still ask in Edits.
    budget: EditBudget,
    /// What the client may have applied of its inputs now, in eighths of one (see
    /// [`INPUT_COST`]).
    input_credit: u32,
    /// The tick of the client's last Dig, or of its arrival before it has dug: the wait for the
    /// next Dig is counted from it (see [`DIG_EARLY_TICKS`]). So the first Dig pays for its own
    /// target as every other does, and cannot be made at once.
    last_dig: u64,
}

impl Player {
    fn new(arrived: u64) -> Self {
        Self {
            budget: EditBudget::full(),
            input_credit: INPUT_BURST * INPUT_COST,
            last_dig: arrived,
        }
    }
}

/// The most work, in units ([`crate::work`]), that the Edits of one tick may ask, all clients
/// together: what the server will spend on Edits in a tick, so that a burst of them from many
/// clients cannot make a tick late. A unit is about 10 ns (a Smooth's Voxels count six), so this is
/// a few milliseconds. It is twice [`crate::WORK_BURST`], what one maximum Brush asks, so that a
/// maximum Brush fits beside as much ordinary work as another one. An Edit that does not fit is
/// turned down with [`RejectReason::RateLimited`], and its client may ask again; which clients
/// are served first goes round, a tick at a time, so that it is not always the same ones.
pub const EDIT_WORK_PER_TICK: u64 = 2 * crate::WORK_BURST;

/// One client's character, how many of its inputs the server has applied, and the ground it
/// walks on.
struct Avatar {
    character: Character,
    /// The client's number for the spawn that made this character.
    spawn: u64,
    applied: u64,
    /// How many ticks the character has run: the inputs applied, which is fewer than `applied`
    /// when gaps skipped some.
    ticks_run: u64,
    /// Inputs that arrived and are not applied yet, each with its number, in order.
    queued: VecDeque<(u64, MoveInput)>,
    ground: Ground,
    /// Leashes decided since the client was last told, each with the input it first holds for.
    fresh: Vec<(u64, Leash)>,
}

impl Avatar {
    /// Applies one tick of `input`, and keeps the ground.
    fn apply(&mut self, store: &BrickStore, input: &MoveInput) {
        self.character
            .tick(&Footing::new(store, &self.ground), input);
        self.applied += 1;
        self.ticks_run += 1;
        self.keep_ground();
    }

    /// Fills the ground around the character, and moves the leash to the ground that is ready if
    /// the character is well inside it and it is a change: another centre, or a bigger ball. The
    /// character is held to the leash from the next input it applies.
    ///
    /// This runs once per input applied, so what the leash is when the character has applied `n`
    /// inputs does not depend on the clock, nor on how the inputs were spread over ticks.
    fn keep_ground(&mut self) {
        let position = self.character.state().position;
        self.ground.follow(position);
        self.ground.fill();
        let Some(ready) = self.ground.ready() else {
            return;
        };
        let away = (0..3)
            .map(|a| (position[a] - ready.centre[a]) * (position[a] - ready.centre[a]))
            .sum::<f64>()
            .sqrt();
        let change = self
            .character
            .leash()
            .is_none_or(|now| now.centre != ready.centre || now.radius < ready.radius);
        if change && away + CLEARANCE <= ready.radius {
            self.character.set_leash(Some(ready));
            self.fresh.push((self.applied, ready));
        }
    }
}

/// The authoritative Planet, the order of the Edits made to it, and the characters on it.
pub struct Server {
    store: BrickStore,
    /// Global sequence number of the latest Edit; the first Edit is 1.
    global_seq: u64,
    movement: Movement,
    /// New ground generated per input applied, for a character's ground.
    ground_rate: Option<usize>,
    /// Where the first spawn of each client is put.
    spawn_point: [f64; 3],
    /// The clients (by index) that may spawn anywhere.
    debug: BTreeSet<usize>,
    /// Whether a Dig is held to the time its target takes (see [`DIG_EARLY_TICKS`]).
    dig_timing: bool,
    /// The character of each client, by its index in the list given to [`Server::tick`].
    avatars: Vec<Option<Avatar>>,
    /// What the server keeps of each client, by the same index.
    players: Vec<Player>,
    /// Units of work that the Edits made this tick asked, against [`EDIT_WORK_PER_TICK`].
    edit_work: u64,
    /// The bricks that Edits read where the store holds none, so that they are made once.
    cache: BrickCache,
    /// Ticks so far, which decide whose requests are handled first.
    tick_count: u64,
}

impl Server {
    /// A server of the Planet in `store`, which puts the first spawn of every client at
    /// `spawn_point`, in Voxels, and not where the client asks (see [`ClientMessage::Spawn`]).
    pub fn new(store: BrickStore, spawn_point: [f64; 3]) -> Self {
        Self {
            store,
            global_seq: 0,
            movement: Movement::default(),
            ground_rate: None,
            spawn_point,
            debug: BTreeSet::new(),
            dig_timing: true,
            avatars: Vec::new(),
            players: Vec::new(),
            edit_work: 0,
            cache: BrickCache::new(),
            tick_count: 0,
        }
    }

    /// Generates at most `rate` bricks of ground per input applied, instead of the usual.
    pub fn with_ground_rate(mut self, rate: usize) -> Self {
        self.ground_rate = Some(rate);
        self
    }

    /// Gives client `index` (its place in the list given to [`Server::tick`]) debug rights: its
    /// spawns are put where it asks, which is what lets a noclip camera set the character down
    /// anywhere. The single-player server gives them to its one client.
    pub fn with_debug_client(mut self, index: usize) -> Self {
        self.debug.insert(index);
        self
    }

    /// Stops holding Digs to the time that their target takes: for tests of the other rules,
    /// which dig faster than a player could.
    pub fn without_dig_timing(mut self) -> Self {
        self.dig_timing = false;
        self
    }

    pub fn store(&self) -> &BrickStore {
        &self.store
    }

    /// The character of client `index` (its place in the list given to [`Server::tick`]), if it
    /// has spawned.
    pub fn character(&self, index: usize) -> Option<&Character> {
        self.avatars.get(index)?.as_ref().map(|a| &a.character)
    }

    /// How many ticks of input client `index`'s character has run since it spawned: the inputs
    /// applied, which is fewer than the ones it has dealt with when a gap skipped some.
    pub fn ticks_run(&self, index: usize) -> Option<u64> {
        self.avatars.get(index)?.as_ref().map(|a| a.ticks_run)
    }

    /// The ground that client `index`'s character walks on, if it has spawned.
    pub fn ground(&self, index: usize) -> Option<&Ground> {
        self.avatars.get(index)?.as_ref().map(|a| &a.ground)
    }

    /// Units of work that the Edits made in the latest tick asked, which is at most
    /// [`EDIT_WORK_PER_TICK`].
    pub fn edit_work(&self) -> u64 {
        self.edit_work
    }

    /// Checks the request against client `asker`'s limits and the rules of an Edit and, if it
    /// passes, applies it and gives it the next global sequence number. A rejected request changes
    /// nothing and takes no number. The checks are, in this order: the rate (every request
    /// counts, however it ends), the reach from the character's eyes (no character, no reach), the
    /// Brush itself ([`validate`]), and the work that it asks ([`work`], known from the op alone):
    /// against what the player has left and what the server has left of this tick. Only then is
    /// the Planet read, for a Dig, to find the Material it is aimed at, and the Dig is held to the
    /// time that Material takes after the player's last (see [`DIG_EARLY_TICKS`]); that read is the
    /// one Voxel's, and makes its brick if there is none. Then the work is taken, the Edit is
    /// worked out ([`plan`]) at a cost that those have bounded, and the Voxels it changes are held
    /// to what the player may change, and if they are over it nothing is applied.
    fn handle_edit(&mut self, asker: usize, client_seq: u64, op: EditOp) -> Outcome {
        let reject = |reason| Outcome::Rejected { client_seq, reason };
        self.make_room(asker + 1);
        if !self.players[asker].budget.take_op() {
            return reject(RejectReason::RateLimited);
        }
        let in_reach = self
            .character(asker)
            .is_some_and(|character| within_reach(character.eye(), op.centre));
        if !in_reach {
            return reject(RejectReason::OutOfReach);
        }
        if let Err(reason) = validate(&op) {
            return reject(reason);
        }
        let asked = work(&op);
        if !self.players[asker].budget.has_work(asked)
            || self.edit_work + asked > EDIT_WORK_PER_TICK
        {
            return reject(RejectReason::RateLimited);
        }
        // A Dig waits for what the Material it is aimed at takes to dig, from the last one. It is
        // counted from here once the Dig is made, below.
        let wait = (self.dig_timing && op.mode == EditMode::Dig)
            .then(|| dig_ticks(self.hardness_at(asker, op.centre)));
        if let Some(wait) = wait
            && self.tick_count + DIG_EARLY_TICKS < self.players[asker].last_dig + wait
        {
            return reject(RejectReason::TooSoon);
        }
        self.players[asker].budget.take_work(asked);
        self.edit_work += asked;
        // The Edit reads the Planet from the store, then from the asker's Ground, which holds what
        // the generator makes round the character, and then from the cache.
        let ground = self
            .avatars
            .get(asker)
            .and_then(Option::as_ref)
            .map(|avatar| &avatar.ground);
        let plan = plan_bricks_from(&mut reads_over(&self.store, ground, &mut self.cache), &op);
        if !self.players[asker].budget.take_volume(plan.len() as u64) {
            return reject(RejectReason::VolumeLimited);
        }
        apply_plan_from(&mut self.store, &plan, ground, Some(&mut self.cache));
        if let Some(wait) = wait {
            let player = &mut self.players[asker];
            player.last_dig = self.tick_count.max(player.last_dig + wait);
        }
        self.global_seq += 1;
        Outcome::Applied {
            global_seq: self.global_seq,
            op,
            client_seq,
        }
    }

    /// Makes room for `clients` clients in what the server keeps for each.
    fn make_room(&mut self, clients: usize) {
        let arrived = self.tick_count;
        if self.players.len() < clients {
            self.players.resize_with(clients, || Player::new(arrived));
        }
    }

    /// The hardness of the Voxel at `centre`, as client `asker`'s Edit would read it: that of the
    /// Voxel that a Dig is aimed at ([`target_hardness`]).
    fn hardness_at(&mut self, asker: usize, centre: VoxelCoord) -> f32 {
        let ground = self
            .avatars
            .get(asker)
            .and_then(Option::as_ref)
            .map(|avatar| &avatar.ground);
        target_hardness(reads_over(&self.store, ground, &mut self.cache).get(centre))
    }

    /// Where client `index`'s character is put by a spawn that asks for `position`: see
    /// [`ClientMessage::Spawn`].
    fn spawn_position(&self, index: usize, position: [f64; 3]) -> [f64; 3] {
        if self.debug.contains(&index) {
            return position;
        }
        let Some(Some(avatar)) = self.avatars.get(index) else {
            return self.spawn_point;
        };
        let Some(leash) = avatar.character.leash() else {
            return avatar.character.state().position;
        };
        let away2: f64 = (0..3)
            .map(|a| (position[a] - leash.centre[a]) * (position[a] - leash.centre[a]))
            .sum();
        // A NaN is not inside.
        if away2 <= leash.radius * leash.radius {
            position
        } else {
            leash.centre
        }
    }

    /// (Re)places client `index`'s character, standing where [`Server::spawn_position`] says
    /// with no inputs applied. It has no leash until the ground around it is ready, and applies
    /// no input until then.
    fn spawn_character(&mut self, index: usize, spawn: u64, asked: [f64; 3]) {
        let position = self.spawn_position(index, asked);
        if self.avatars.len() <= index {
            self.avatars.resize_with(index + 1, || None);
        }
        let mut ground = Ground::new(self.store.source());
        if let Some(rate) = self.ground_rate {
            ground = ground.with_rate(rate);
        }
        ground.reset(position);
        self.avatars[index] = Some(Avatar {
            character: Character::spawn(self.movement, position, None),
            spawn,
            applied: 0,
            ticks_run: 0,
            queued: VecDeque::new(),
            ground,
            fresh: Vec::new(),
        });
    }

    /// Takes in input number `seq` for client `index`'s character, if it has one, and applies what
    /// it may of the inputs held (see [`Server::apply_queued`]). An input before the one that is
    /// due is a repeat or late and is ignored, as is one far past it (see
    /// [`ClientMessage::Input`]); one past it is taken, and what is between is skipped when it is
    /// applied. Returns whether any input was applied.
    fn apply_input(&mut self, index: usize, seq: u64, input: &MoveInput) -> bool {
        let Some(Some(avatar)) = self.avatars.get_mut(index) else {
            return false;
        };
        let due = avatar
            .queued
            .back()
            .map_or(avatar.applied, |&(last, _)| last + 1);
        if seq < due || seq - due >= MAX_QUEUED as u64 {
            return false;
        }
        // With a leash the inputs are applied as they come, so only a burst's worth are held; the
        // rest were sent faster than they may be applied, and are dropped.
        let held = if avatar.character.leash().is_some() {
            INPUT_BURST as usize
        } else {
            MAX_QUEUED
        };
        if avatar.queued.len() < held {
            avatar.queued.push_back((seq, *input));
        }
        self.apply_queued(index)
    }

    /// Applies the inputs that client `index`'s character has held, in order, while it has a
    /// leash and the client has credit for them. An input that is past the next one skips what is
    /// between. Returns whether any was applied.
    fn apply_queued(&mut self, index: usize) -> bool {
        let Some(Some(avatar)) = self.avatars.get_mut(index) else {
            return false;
        };
        let credit = &mut self.players[index].input_credit;
        let mut moved = false;
        while avatar.character.leash().is_some() && *credit >= INPUT_COST {
            let Some((seq, input)) = avatar.queued.pop_front() else {
                break;
            };
            *credit -= INPUT_COST;
            avatar.applied = seq;
            avatar.apply(&self.store, &input);
            moved = true;
        }
        moved
    }

    /// One tick: tops up each client's Edit allowance, then handles every request that has
    /// arrived, each client's in the order it sent them, and the clients one after another from
    /// the one that is `tick number mod the number of clients` along (so that the tick's share of
    /// work, which is first come first served, is not always the first clients'). Each
    /// accepted Edit goes to every client, with the ack only for the one that asked; a reject goes
    /// to the one that asked. A client whose character moved is told where it is, once, after its
    /// requests, and before that of any new leash. A character with no leash yet is waiting for
    /// its ground, which is filled each tick.
    pub fn tick(&mut self, clients: &[ServerEnd]) {
        self.make_room(clients.len());
        for player in &mut self.players {
            player.budget.refill();
            player.input_credit =
                (player.input_credit + INPUT_REFILL).min(INPUT_BURST * INPUT_COST);
        }
        self.edit_work = 0;
        let count = clients.len();
        let first = (self.tick_count % count.max(1) as u64) as usize;
        self.tick_count += 1;
        for step in 0..count {
            let asker = (first + step) % count;
            let client = &clients[asker];
            let mut moved = false;
            while let Ok(message) = client.try_recv() {
                match message {
                    ClientMessage::Edit { client_seq, op } => {
                        match self.handle_edit(asker, client_seq, op) {
                            Outcome::Applied {
                                global_seq,
                                op,
                                client_seq,
                            } => {
                                for (index, client) in clients.iter().enumerate() {
                                    // A client that has gone away must not stop the others hearing.
                                    let _ = client.send(ServerMessage::Applied {
                                        global_seq,
                                        op,
                                        ack: (index == asker).then_some(client_seq),
                                    });
                                }
                            }
                            Outcome::Rejected { client_seq, reason } => {
                                let _ = client.send(ServerMessage::Rejected { client_seq, reason });
                            }
                        }
                    }
                    ClientMessage::Spawn { spawn, position } => {
                        self.spawn_character(asker, spawn, position);
                        moved = true;
                    }
                    ClientMessage::Input { seq, input } => {
                        moved |= self.apply_input(asker, seq, &input);
                    }
                }
            }
            let Some(Some(avatar)) = self.avatars.get_mut(asker) else {
                continue;
            };
            if avatar.character.leash().is_none() {
                avatar.keep_ground();
            }
            // The inputs that came before the leash, or faster than they may be applied, go on.
            moved |= self.apply_queued(asker);
            let Some(Some(avatar)) = self.avatars.get_mut(asker) else {
                continue;
            };
            for (from, leash) in avatar.fresh.drain(..) {
                let _ = client.send(ServerMessage::Leash {
                    spawn: avatar.spawn,
                    from,
                    leash,
                });
            }
            if moved {
                let _ = client.send(ServerMessage::Body {
                    spawn: avatar.spawn,
                    applied: avatar.applied,
                    state: *avatar.character.state(),
                });
            }
        }
    }
}

/// Where an Edit by a client with this `ground` reads the Planet from.
fn reads_over<'a>(
    store: &'a BrickStore,
    ground: Option<&'a Ground>,
    cache: &'a mut BrickCache,
) -> Reads<'a> {
    let reads = Reads::new(store).with_cache(cache);
    match ground {
        Some(ground) => reads.with_ground(ground),
        None => reads,
    }
}

/// A [`Server`] running on its own thread.
pub struct ServerHandle {
    stop: Arc<AtomicBool>,
    ticks: Arc<AtomicU64>,
    thread: Option<JoinHandle<Server>>,
}

impl ServerHandle {
    /// Ticks run so far.
    pub fn ticks(&self) -> u64 {
        self.ticks.load(Ordering::Relaxed)
    }

    /// Stops the thread and returns the server as it ended.
    pub fn stop(mut self) -> Server {
        self.stop.store(true, Ordering::SeqCst);
        let thread = self.thread.take().expect("only stop takes the thread");
        thread.join().expect("the server thread panicked")
    }
}

/// Dropping the handle tells the thread to stop but does not wait for it, which could be a
/// while if the Planet is still being built; [`ServerHandle::stop`] waits.
impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Starts the server on a new thread. `make_server` builds it, and its Planet, on that thread, so
/// the caller need not wait for it: requests wait in their channels until it is ready.
pub fn spawn(
    make_server: impl FnOnce() -> Server + Send + 'static,
    clients: Vec<ServerEnd>,
) -> ServerHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let ticks = Arc::new(AtomicU64::new(0));
    let thread = {
        let (stop, ticks) = (Arc::clone(&stop), Arc::clone(&ticks));
        std::thread::spawn(move || {
            let mut server = make_server();
            let mut clock =
                FixedTimestep::new(Duration::from_secs(1) / TICK_RATE_HZ, MAX_TICKS_PER_ADVANCE);
            let mut last = Instant::now();
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(clock.until_next_tick());
                let now = Instant::now();
                for _ in 0..clock.advance(now - last) {
                    server.tick(&clients);
                    ticks.fetch_add(1, Ordering::Relaxed);
                }
                last = now;
            }
            server
        })
    };
    ServerHandle {
        stop,
        ticks,
        thread: Some(thread),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brick_store::{Brick, BrickCoord, Material, VoxelCoord};
    use materials::MaterialTable;
    use net::{ClientEnd, TryRecvError, loopback};

    const STONE: Material = Material(7);

    fn dig(client_seq: u64, centre: [i32; 3], radius: u16) -> ClientMessage {
        ClientMessage::Edit {
            client_seq,
            op: EditOp::dig_sphere(VoxelCoord(centre), radius),
        }
    }

    fn stone_store() -> BrickStore {
        let mut store = BrickStore::new();
        for x in -2..2 {
            store.insert(BrickCoord([x, 0, 0]), Brick::solid(STONE));
        }
        store
    }

    fn received(client: &ClientEnd) -> Vec<ServerMessage> {
        std::iter::from_fn(|| client.try_recv().ok()).collect()
    }

    /// What the server said about Edits, and not about the character.
    fn edits_heard(client: &ClientEnd) -> Vec<ServerMessage> {
        let mut heard = received(client);
        heard.retain(|m| !matches!(m, ServerMessage::Body { .. } | ServerMessage::Leash { .. }));
        heard
    }

    /// The request that puts a character at the origin, among the stone the tests edit: Edits need
    /// a character to be in reach of.
    fn stand() -> ClientMessage {
        ClientMessage::Spawn {
            spawn: 1,
            position: [0.0; 3],
        }
    }

    #[test]
    fn a_request_is_applied_numbered_and_acked() {
        let (client, end) = loopback();
        let mut server = Server::new(stone_store(), [0.0; 3]).without_dig_timing();
        client.send(stand()).unwrap();
        client.send(dig(0, [3, 3, 3], 2)).unwrap();
        server.tick(&[end]);
        let op = EditOp::dig_sphere(VoxelCoord([3, 3, 3]), 2);
        assert_eq!(
            edits_heard(&client),
            [ServerMessage::Applied {
                global_seq: 1,
                op,
                ack: Some(0)
            }]
        );
        assert_eq!(server.store().get(VoxelCoord([3, 3, 3])), None);
        assert_eq!(server.store().get(VoxelCoord([6, 3, 3])), Some(STONE));
    }

    #[test]
    fn global_sequence_numbers_follow_the_order_the_server_handled_requests() {
        let (client, end) = loopback();
        let mut server = Server::new(stone_store(), [0.0; 3]).without_dig_timing();
        client.send(stand()).unwrap();
        // Places, in the air: Digs would be held to the time they take.
        for seq in 0..4 {
            let op = place([seq as i32 * 4, 20, 0], net::Shape::Sphere, 1);
            client
                .send(ClientMessage::Edit {
                    client_seq: seq,
                    op,
                })
                .unwrap();
        }
        server.tick(&[end]);
        let seqs: Vec<_> = edits_heard(&client)
            .into_iter()
            .map(|m| match m {
                ServerMessage::Applied {
                    global_seq, ack, ..
                } => (global_seq, ack),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            seqs,
            [(1, Some(0)), (2, Some(1)), (3, Some(2)), (4, Some(3))]
        );
    }

    #[test]
    fn a_rejected_request_changes_nothing_and_takes_no_number() {
        let (client, end) = loopback();
        let mut server = Server::new(stone_store(), [0.0; 3]).without_dig_timing();
        let before = server.store().content_hash();
        client.send(stand()).unwrap();
        client.send(dig(0, [0, 0, 0], 200)).unwrap();
        client.send(dig(1, [0, 0, 0], 1)).unwrap();
        server.tick(&[end]);
        let replies = edits_heard(&client);
        assert_eq!(
            replies[0],
            ServerMessage::Rejected {
                client_seq: 0,
                reason: RejectReason::BrushTooLarge
            }
        );
        assert!(matches!(
            replies[1],
            ServerMessage::Applied {
                global_seq: 1,
                ack: Some(1),
                ..
            }
        ));
        assert_ne!(server.store().content_hash(), before);
    }

    #[test]
    fn every_client_hears_every_edit_but_only_the_asker_is_acked() {
        let ((a, a_end), (b, b_end)) = (loopback(), loopback());
        let mut server = Server::new(stone_store(), [0.0; 3]).without_dig_timing();
        a.send(stand()).unwrap();
        b.send(stand()).unwrap();
        a.send(dig(5, [0, 0, 0], 1)).unwrap();
        b.send(dig(9, [8, 0, 0], 1)).unwrap();
        server.tick(&[a_end, b_end]);
        let acks = |client: &ClientEnd| -> Vec<_> {
            edits_heard(client)
                .into_iter()
                .map(|m| match m {
                    ServerMessage::Applied {
                        global_seq, ack, ..
                    } => (global_seq, ack),
                    other => panic!("{other:?}"),
                })
                .collect()
        };
        assert_eq!(acks(&a), [(1, Some(5)), (2, None)]);
        assert_eq!(acks(&b), [(1, None), (2, Some(9))]);
    }

    #[test]
    fn a_client_that_left_does_not_stop_the_others_hearing() {
        let ((a, a_end), (b, b_end)) = (loopback(), loopback());
        let mut server = Server::new(stone_store(), [0.0; 3]).without_dig_timing();
        b.send(stand()).unwrap();
        b.send(dig(0, [0, 0, 0], 1)).unwrap();
        drop(a);
        server.tick(&[a_end, b_end]);
        assert_eq!(edits_heard(&b).len(), 1);
    }

    /// A server on its own thread, and a client that has stood and dug and heard the answer.
    fn a_server_thread_that_has_answered() -> (ServerHandle, ClientEnd) {
        let (client, end) = loopback();
        let handle = spawn(
            || Server::new(stone_store(), [0.0; 3]).without_dig_timing(),
            vec![end],
        );
        client.send(stand()).unwrap();
        client.send(dig(0, [3, 3, 3], 2)).unwrap();
        let started = Instant::now();
        let reply = loop {
            match client.try_recv() {
                Ok(ServerMessage::Body { .. }) => {}
                Ok(reply) => break reply,
                // Only a guard against a hang: how long the answer takes is not what is checked.
                Err(TryRecvError::Empty) if started.elapsed() < Duration::from_secs(60) => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(err) => panic!("no reply: {err:?}"),
            }
        };
        assert!(matches!(
            reply,
            ServerMessage::Applied {
                global_seq: 1,
                ack: Some(0),
                ..
            }
        ));
        (handle, client)
    }

    /// The thread keeps ticking, and answers: counted, not timed (#193). Its pace is the
    /// `FixedTimestep`'s, tested with a clock of the test's own above, and the rate on the
    /// machine's clock is in the next test.
    #[test]
    fn on_its_own_thread_it_ticks_on_and_answers() {
        let (handle, _client) = a_server_thread_that_has_answered();
        let first = handle.ticks();
        // A hang guard of a minute, not a rate: a busy machine only makes this take longer.
        let started = Instant::now();
        while handle.ticks() < first + 5 {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "the thread stopped ticking"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let server = handle.stop();
        assert_eq!(server.store().get(VoxelCoord([3, 3, 3])), None);
    }

    /// The thread's rate on the machine's clock, which a loaded machine misses: it does not gate
    /// (#193). The `real-thread-tests` job runs it (`--ignored`).
    #[test]
    #[ignore = "a wall-clock limit: run by the real-thread-tests job (#193)"]
    fn on_its_own_thread_it_ticks_at_about_60_hz_and_answers() {
        let (handle, _client) = a_server_thread_that_has_answered();
        let first = handle.ticks();
        std::thread::sleep(Duration::from_millis(500));
        // 30 ticks expected; the bounds leave room for a busy machine.
        let ran = handle.ticks() - first;
        assert!((15..=45).contains(&ran), "{ran} ticks in half a second");
        handle.stop();
    }

    /// The kinds of Edit, one request each, as the server answers them in one tick.
    fn answers(
        server: &mut Server,
        end: &ServerEnd,
        client: &ClientEnd,
        asks: &[EditOp],
    ) -> Vec<Option<RejectReason>> {
        for (seq, &op) in asks.iter().enumerate() {
            client
                .send(ClientMessage::Edit {
                    client_seq: seq as u64,
                    op,
                })
                .unwrap();
        }
        server.tick(std::slice::from_ref(end));
        edits_heard(client)
            .into_iter()
            .map(|m| match m {
                ServerMessage::Applied { .. } => None,
                ServerMessage::Rejected { reason, .. } => Some(reason),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    /// A server with a character standing at the origin of a store of stone.
    fn standing() -> (Server, ServerEnd, ClientEnd) {
        let (client, end) = loopback();
        let mut server = Server::new(stone_store(), [0.0; 3]).without_dig_timing();
        client.send(stand()).unwrap();
        server.tick(std::slice::from_ref(&end));
        received(&client);
        (server, end, client)
    }

    fn place(centre: [i32; 3], shape: net::Shape, radius: u16) -> EditOp {
        EditOp {
            mode: net::EditMode::Place,
            brush: net::Brush { shape, radius },
            centre: VoxelCoord(centre),
            material: Material(7),
            alignment: net::Alignment::World,
            rotation: 0,
        }
    }

    #[test]
    fn every_mode_and_shape_is_applied_by_the_server() {
        use net::{EditMode, Shape};
        let (mut server, end, client) = standing();
        for shape in [Shape::Sphere, Shape::Cube, Shape::Cylinder] {
            for mode in [
                EditMode::Dig,
                EditMode::Place,
                EditMode::Smooth,
                EditMode::Flatten,
            ] {
                let op = EditOp {
                    mode,
                    ..place([2, 3, 4], shape, 3)
                };
                let before = server.store().content_hash();
                assert_eq!(answers(&mut server, &end, &client, &[op]), [None]);
                // The first of each shape's digs and places changes something, and the smoothing
                // of what is left may find nothing to round off: only the others are checked.
                if matches!(mode, EditMode::Dig) {
                    assert_ne!(server.store().content_hash(), before, "{op:?}");
                }
                // Let the allowance of Edits fill up again between requests.
                for _ in 0..TICK_RATE_HZ {
                    server.tick(std::slice::from_ref(&end));
                }
            }
        }
    }

    #[test]
    fn an_edit_from_a_client_with_no_character_is_out_of_reach() {
        let (client, end) = loopback();
        let mut server = Server::new(stone_store(), [0.0; 3]).without_dig_timing();
        let before = server.store().content_hash();
        let answers = answers(
            &mut server,
            &end,
            &client,
            &[EditOp::dig_sphere(VoxelCoord([3, 3, 3]), 2)],
        );
        assert_eq!(answers, [Some(RejectReason::OutOfReach)]);
        assert_eq!(server.store().content_hash(), before);
    }

    #[test]
    fn an_edit_is_in_reach_up_to_8_m_from_the_eyes_and_not_beyond() {
        let reach = crate::EDIT_REACH;
        let (mut server, end, client) = standing();
        let eye = server.character(0).unwrap().eye();
        // Along y, which is the way up at the origin's own, odd spot, and so is the eyes' way.
        let at = |voxels: f64| {
            let y = (eye[1] + voxels).floor() as i32;
            // Places, so that they are not held to the time a Dig takes.
            place([0, y, 0], net::Shape::Sphere, 1)
        };
        assert_eq!(
            answers(
                &mut server,
                &end,
                &client,
                &[at(reach - 1.0), at(1.0 - reach)]
            ),
            [None, None]
        );
        let far = answers(
            &mut server,
            &end,
            &client,
            &[at(reach + 1.0), at(-reach - 1.0)],
        );
        assert_eq!(far, [Some(RejectReason::OutOfReach); 2]);
    }

    #[test]
    fn a_brush_bigger_than_2_m_and_a_material_that_is_not_defined_are_turned_down() {
        let (mut server, end, client) = standing();
        let big = place([0, 0, 0], net::Shape::Cube, crate::MAX_BRUSH_RADIUS + 1);
        let unknown = EditOp {
            material: Material(60_000),
            ..place([0, 0, 0], net::Shape::Sphere, 2)
        };
        let nothing = EditOp {
            material: Material(0),
            ..place([0, 0, 0], net::Shape::Sphere, 2)
        };
        let flatten = EditOp {
            mode: net::EditMode::Flatten,
            ..unknown
        };
        let dig_ignores_it = EditOp {
            mode: net::EditMode::Dig,
            ..unknown
        };
        assert_eq!(
            answers(
                &mut server,
                &end,
                &client,
                &[big, unknown, nothing, flatten, dig_ignores_it]
            ),
            [
                Some(RejectReason::BrushTooLarge),
                Some(RejectReason::UnknownMaterial),
                Some(RejectReason::UnknownMaterial),
                Some(RejectReason::UnknownMaterial),
                None,
            ]
        );
    }

    #[test]
    fn a_player_may_make_20_edits_a_second() {
        let (mut server, end, client) = standing();
        // Places, in the air: Digs would be held to the time they take.
        let ops = vec![place([0, 20, 0], net::Shape::Sphere, 0); 25];
        let told = answers(&mut server, &end, &client, &ops);
        assert_eq!(told.iter().filter(|r| r.is_none()).count(), 20);
        assert!(
            told[20..]
                .iter()
                .all(|r| *r == Some(RejectReason::RateLimited))
        );

        // The allowance comes back at 20 a second: one Edit every third tick of the 60.
        for _ in 0..2 {
            server.tick(std::slice::from_ref(&end));
        }
        let soon = answers(&mut server, &end, &client, &ops[..2]);
        assert_eq!(soon, [None, Some(RejectReason::RateLimited)]);
        for _ in 0..TICK_RATE_HZ {
            server.tick(std::slice::from_ref(&end));
        }
        let later = answers(&mut server, &end, &client, &ops);
        assert_eq!(later.iter().filter(|r| r.is_none()).count(), 20);
    }

    #[test]
    fn a_player_may_change_8_cubic_metres_a_second() {
        // A mass of stone 80 Voxels (4 m) across, with a character standing in the middle of it.
        let mut store = BrickStore::new();
        for x in -5..5 {
            for y in -5..5 {
                for z in -5..5 {
                    store.insert(BrickCoord([x, y, z]), Brick::solid(STONE));
                }
            }
        }
        let (client, end) = loopback();
        let mut server = Server::new(store, [0.0; 3]).without_dig_timing();
        client.send(stand()).unwrap();
        server.tick(std::slice::from_ref(&end));
        received(&client);

        // Nine cubes of 41 Voxels a side, 68 921 Voxels each, side by side, placed in the air above
        // the stone (Digs would be held to the time they take).
        let cube = |i: i32| {
            place(
                [(i % 3 - 1) * 42, 120, (i / 3 - 1) * 42],
                net::Shape::Cube,
                20,
            )
        };
        let nine: Vec<_> = (0..9).map(cube).collect();
        // The burst is 576 000 Voxels, which is eight of them. The one that did not fit changed
        // nothing and cost nothing.
        let told = answers(&mut server, &end, &client, &nine);
        assert_eq!(told[..8], [None; 8]);
        assert_eq!(told[8..], [Some(RejectReason::VolumeLimited)]);

        // A second brings back 64 000 Voxels, which is room for one more, and not for two.
        for _ in 0..TICK_RATE_HZ {
            server.tick(std::slice::from_ref(&end));
        }
        let told = answers(
            &mut server,
            &end,
            &client,
            &[nine[8], place([0, 120, -84], net::Shape::Cube, 20)],
        );
        assert_eq!(told, [None, Some(RejectReason::VolumeLimited)]);
    }

    /// A mass of stone 80 Voxels (4 m) across, with room to place in beyond it.
    fn mass() -> BrickStore {
        let mut store = BrickStore::new();
        for x in -5..5 {
            for y in -5..5 {
                for z in -5..5 {
                    store.insert(BrickCoord([x, y, z]), Brick::solid(STONE));
                }
            }
        }
        store
    }

    fn smooth(
        shape: net::Shape,
        radius: u16,
        centre: [i32; 3],
        alignment: net::Alignment,
    ) -> EditOp {
        EditOp {
            mode: net::EditMode::Smooth,
            alignment,
            ..place(centre, shape, radius)
        }
    }

    #[test]
    fn twenty_smooths_that_change_nothing_in_one_tick_are_limited_by_the_work_they_ask() {
        let (client, end) = loopback();
        let mut server = Server::new(mass(), [0.0; 3]).without_dig_timing();
        client.send(stand()).unwrap();
        server.tick(std::slice::from_ref(&end));
        received(&client);

        // A cube of 41 Voxels a side on the grid, in the middle of solid stone, where a Smooth
        // changes nothing. It asks 68 921 Voxels' worth, six units each: 413 526. The player's 2.88
        // million are six of those, and no more, though none of them changes a Voxel.
        let smooth = smooth(net::Shape::Cube, 20, [0, 0, 0], net::Alignment::World);
        assert_eq!(crate::work(&smooth), 413_526);
        let told = answers(&mut server, &end, &client, &[smooth; 20]);
        assert_eq!(told[..6], [None; 6]);
        assert_eq!(told[6..], [Some(RejectReason::RateLimited); 14]);
        assert_eq!(server.edit_work(), 6 * 413_526);
        assert!(server.edit_work() <= EDIT_WORK_PER_TICK);

        // And the work comes back at 4.8 million a second: 80 000 a tick, so one Smooth in
        // six ticks.
        for _ in 0..4 {
            server.tick(std::slice::from_ref(&end));
        }
        assert_eq!(
            answers(&mut server, &end, &client, &[smooth; 2]),
            [None, Some(RejectReason::RateLimited)]
        );
    }

    #[test]
    fn a_leaning_cube_of_4_m_placed_from_rest_is_accepted_and_one_more_has_to_wait() {
        let (client, end) = loopback();
        let mut server = Server::new(mass(), [0.0; 3]).without_dig_timing();
        client.send(stand()).unwrap();
        server.tick(std::slice::from_ref(&end));
        received(&client);

        // In the air over the stone, on a lean, and turned an eighth: the box that holds it is
        // 2 million Voxels and more, the most that any Brush asks.
        let cube = EditOp {
            alignment: net::Alignment::Local,
            rotation: 1,
            ..place([60, 120, 24], net::Shape::Cube, 40)
        };
        let asked = crate::work(&cube);
        assert!((2_000_000..=crate::WORK_BURST).contains(&asked), "{asked}");
        let changed_before = server.store().content_hash();
        assert_eq!(answers(&mut server, &end, &client, &[cube]), [None]);
        assert_ne!(server.store().content_hash(), changed_before);
        assert_eq!(server.edit_work(), asked);
        // Placing again changes nothing, and the work for it is not there.
        assert_eq!(
            answers(&mut server, &end, &client, &[cube]),
            [Some(RejectReason::RateLimited)]
        );
    }

    #[test]
    fn a_smooth_or_a_flatten_over_1_m_is_too_big_and_a_dig_or_a_place_over_2_m() {
        let (mut server, end, client) = standing();
        let asked = |mode, radius| EditOp {
            mode,
            ..place([0, 0, 0], net::Shape::Sphere, radius)
        };
        let told = answers(
            &mut server,
            &end,
            &client,
            &[
                asked(net::EditMode::Smooth, crate::MAX_SMOOTH_RADIUS + 1),
                asked(net::EditMode::Flatten, crate::MAX_SMOOTH_RADIUS + 1),
                asked(net::EditMode::Smooth, 10),
                asked(net::EditMode::Flatten, 10),
                asked(net::EditMode::Place, 20),
                asked(net::EditMode::Dig, crate::MAX_BRUSH_RADIUS + 1),
            ],
        );
        assert_eq!(
            told,
            [
                Some(RejectReason::BrushTooLarge),
                Some(RejectReason::BrushTooLarge),
                None,
                None,
                None,
                Some(RejectReason::BrushTooLarge)
            ]
        );
    }

    #[test]
    fn what_an_edit_asks_of_the_server_is_charged_whether_or_not_it_changes_anything() {
        let (mut server, end, client) = standing();
        // Placing in air changes nothing in stone, and is charged the same as digging.
        let nothing = place([0, 0, 0], net::Shape::Sphere, 10);
        assert_eq!(answers(&mut server, &end, &client, &[nothing]), [None]);
        assert_eq!(server.edit_work(), crate::examined(&nothing));
        assert_eq!(crate::examined(&nothing), 21 * 21 * 21);
    }

    /// `players` clients, each with a character at the origin, and a server of `mass`.
    fn crowd(players: usize) -> (Server, Vec<ClientEnd>, Vec<ServerEnd>) {
        let mut clients = Vec::new();
        let mut ends = Vec::new();
        for _ in 0..players {
            let (client, end) = loopback();
            client.send(stand()).unwrap();
            clients.push(client);
            ends.push(end);
        }
        let mut server = Server::new(mass(), [0.0; 3]).without_dig_timing();
        server.tick(&ends);
        for client in &clients {
            received(client);
        }
        (server, clients, ends)
    }

    /// The clients of `who` ask for `op` and the server ticks once: what each is told (its own
    /// reject, if it has one).
    fn ask_all(
        server: &mut Server,
        clients: &[ClientEnd],
        ends: &[ServerEnd],
        who: &[usize],
        op: EditOp,
    ) -> Vec<Option<RejectReason>> {
        for &i in who {
            clients[i]
                .send(ClientMessage::Edit {
                    client_seq: server.tick_count,
                    op,
                })
                .unwrap();
        }
        server.tick(ends);
        let told = who
            .iter()
            .map(|&i| {
                edits_heard(&clients[i]).into_iter().find_map(|m| match m {
                    ServerMessage::Rejected { reason, .. } => Some(reason),
                    _ => None,
                })
            })
            .collect();
        for client in clients {
            received(client);
        }
        told
    }

    fn big_sphere() -> EditOp {
        EditOp {
            mode: net::EditMode::Dig,
            ..place([0, 0, 0], net::Shape::Sphere, 40)
        }
    }

    fn leaning_cube() -> EditOp {
        EditOp {
            alignment: net::Alignment::Local,
            rotation: 1,
            ..place([60, 120, 24], net::Shape::Cube, 40)
        }
    }

    #[test]
    fn the_work_of_the_edits_of_a_tick_is_no_more_than_the_servers_share_whoever_asks() {
        // Fourteen players, every one with a full allowance, each asking for the biggest sphere
        // in the one tick: 531 441 each. Ten fit the tick's 5.76 million, and the other four are told
        // to ask again.
        let (mut server, clients, ends) = crowd(14);
        let everyone: Vec<usize> = (0..14).collect();
        let told = ask_all(&mut server, &clients, &ends, &everyone, big_sphere());
        assert_eq!(told.iter().filter(|t| t.is_none()).count(), 10);
        assert!(
            told.iter()
                .all(|t| t.is_none() || *t == Some(RejectReason::RateLimited))
        );
        assert_eq!(server.edit_work(), 10 * 531_441);
        assert!(server.edit_work() <= EDIT_WORK_PER_TICK);
    }

    #[test]
    fn the_clients_that_are_served_first_go_round_so_that_none_is_left_out() {
        // The four that are told to ask again are served the tick after.
        let (mut server, clients, ends) = crowd(14);
        let everyone: Vec<usize> = (0..14).collect();
        let told = ask_all(&mut server, &clients, &ends, &everyone, big_sphere());
        let refused: Vec<usize> = told
            .iter()
            .enumerate()
            .filter(|(_, t)| t.is_some())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(refused.len(), 4);
        let told = ask_all(&mut server, &clients, &ends, &refused, big_sphere());
        assert_eq!(told, [None; 4]);

        // Now all of them ask in every tick for a share that is far too small for them all (a
        // tilted cube of 4 m, over 2 million each, of which one fits), each with a full allowance
        // again after a rest of a second. Whoever is served first goes round: over 14 ticks each
        // of them is served, and none is turned down by the tick every time.
        let (mut server, clients, ends) = crowd(14);
        assert!(crate::work(&leaning_cube()) > 2_000_000);
        let mut served = [0; 14];
        let mut refused = [0; 14];
        for _ in 0..14 {
            for _ in 0..60 {
                server.tick(&ends);
            }
            for client in &clients {
                received(client);
            }
            let told = ask_all(&mut server, &clients, &ends, &everyone, leaning_cube());
            for (i, told) in told.into_iter().enumerate() {
                match told {
                    None => served[i] += 1,
                    Some(RejectReason::RateLimited) => refused[i] += 1,
                    Some(other) => panic!("{other:?}"),
                }
            }
        }
        assert!(served.iter().all(|&n| n >= 1), "served {served:?}");
        assert!(refused.iter().all(|&n| n < 14), "refused {refused:?}");
    }

    #[test]
    fn a_maximum_brush_is_served_beside_the_ordinary_work_of_the_others_whoever_is_last() {
        // Nineteen players each make an ordinary Edit (68 921 Voxels' work), and one a cube of 4 m
        // on a lean (over 2 million): 3.3 million and more, within the tick's 5.76 million, wherever in the
        // order the cube is.
        let (mut server, clients, ends) = crowd(20);
        let ordinary = EditOp {
            mode: net::EditMode::Dig,
            ..place([0, 0, 0], net::Shape::Cube, 20)
        };
        for (i, client) in clients.iter().enumerate() {
            let op = if i == 19 { leaning_cube() } else { ordinary };
            client
                .send(ClientMessage::Edit { client_seq: 0, op })
                .unwrap();
        }
        server.tick(&ends);
        for (i, client) in clients.iter().enumerate() {
            let refused = edits_heard(client)
                .into_iter()
                .any(|m| matches!(m, ServerMessage::Rejected { .. }));
            assert!(!refused, "client {i} was turned down");
        }
        assert!(server.edit_work() > 19 * 68_921 + 2_000_000);
        assert!(server.edit_work() <= EDIT_WORK_PER_TICK);
    }

    #[test]
    fn a_rotation_that_is_not_one_of_the_8_is_turned_down() {
        let (mut server, end, client) = standing();
        let turned = |rotation| EditOp {
            rotation,
            ..place([0, 0, 0], net::Shape::Cube, 2)
        };
        assert_eq!(
            answers(
                &mut server,
                &end,
                &client,
                &[turned(7), turned(8), turned(255)]
            ),
            [
                None,
                Some(RejectReason::BadRotation),
                Some(RejectReason::BadRotation)
            ]
        );
    }

    /// Stone below y = 0: a flat Planet, as a source.
    #[derive(Debug)]
    struct Flat;

    impl brick_store::BrickSource for Flat {
        fn brick(&self, _level: usize, coord: BrickCoord) -> Brick {
            if coord.0[1] < 0 {
                Brick::solid(STONE)
            } else {
                Brick::AIR
            }
        }
    }

    fn flat_store() -> BrickStore {
        let mut store = BrickStore::new();
        store.set_source(Flat);
        store
    }

    fn forward() -> MoveInput {
        MoveInput {
            forward: 1,
            heading: [1.0, 0.0, 0.0],
            ..MoveInput::default()
        }
    }

    #[test]
    fn a_character_applies_no_input_until_its_ground_is_ready_and_is_then_told_its_leash() {
        let (client, end) = loopback();
        let mut server = Server::new(flat_store(), [0.0; 3]).with_debug_client(0);
        let input = |seq| ClientMessage::Input {
            seq,
            input: forward(),
        };
        client
            .send(ClientMessage::Spawn {
                spawn: 1,
                position: [0.0, 3.0, 0.0],
            })
            .unwrap();
        server.tick(std::slice::from_ref(&end));
        // The Spawn is answered, and nothing is applied before the leash.
        let told = received(&client);
        assert!(
            matches!(told[..], [ServerMessage::Body { applied: 0, .. }]),
            "{told:?}"
        );

        // Each tick fills some more, until there is room to stand in: then the leash is told,
        // from the first input.
        let mut leash = None;
        for _ in 0..100 {
            server.tick(std::slice::from_ref(&end));
            let told = received(&client);
            if let [
                ServerMessage::Leash {
                    spawn: 1,
                    from: 0,
                    leash: got,
                },
            ] = told[..]
            {
                leash = Some(got);
                break;
            }
            assert!(told.is_empty(), "{told:?}");
        }
        let leash = leash.expect("the ground became ready");
        assert_eq!(server.character(0).unwrap().leash(), Some(leash));
        assert!(
            (48.0..=f64::from(crate::ground::REACH)).contains(&leash.radius),
            "{leash:?}"
        );

        // The ball goes on filling, so the input may be answered with a wider leash first.
        client.send(input(0)).unwrap();
        server.tick(std::slice::from_ref(&end));
        let told = received(&client);
        assert!(
            matches!(
                told[..],
                [ServerMessage::Body { applied: 1, .. }]
                    | [
                        ServerMessage::Leash { from: 1, .. },
                        ServerMessage::Body { applied: 1, .. }
                    ]
            ),
            "{told:?}"
        );
    }

    #[test]
    fn a_new_leash_is_told_ahead_of_the_body_it_was_decided_in_and_from_the_input_after_it() {
        let (client, end) = loopback();
        let mut server = Server::new(flat_store(), [0.0; 3]).with_debug_client(0);
        client
            .send(ClientMessage::Spawn {
                spawn: 1,
                position: [0.0, 3.0, 0.0],
            })
            .unwrap();
        let (mut seq, mut leashes) = (0, Vec::new());
        for _ in 0..200 {
            if server.character(0).and_then(Character::leash).is_some() {
                client
                    .send(ClientMessage::Input {
                        seq,
                        input: forward(),
                    })
                    .unwrap();
                seq += 1;
            }
            server.tick(std::slice::from_ref(&end));
            let told = received(&client);
            // A Leash is never behind the Body of its tick.
            if let Some(body) = told
                .iter()
                .position(|m| matches!(m, ServerMessage::Body { .. }))
            {
                assert!(
                    told[body..]
                        .iter()
                        .all(|m| !matches!(m, ServerMessage::Leash { .. })),
                    "{told:?}"
                );
            }
            for message in told {
                if let ServerMessage::Leash { from, leash, .. } = message {
                    assert!(from <= seq, "{from} of {seq}");
                    leashes.push((from, leash));
                }
            }
        }
        // The ground grew as it was walked on: a leash for each step of its radius, from the
        // inputs applied then, never from the past.
        assert!(leashes.len() > 5, "{leashes:?}");
        assert!(leashes.windows(2).all(|w| w[0].0 <= w[1].0));
        assert!(leashes.last().unwrap().1.radius > leashes[0].1.radius);
    }

    #[test]
    fn inputs_that_arrive_before_the_leash_are_held_and_applied_when_it_comes() {
        let (client, end) = loopback();
        let mut server = Server::new(flat_store(), [0.0; 3]).with_debug_client(0);
        client
            .send(ClientMessage::Spawn {
                spawn: 7,
                position: [0.0, 3.0, 0.0],
            })
            .unwrap();
        for seq in 0..5 {
            client
                .send(ClientMessage::Input {
                    seq,
                    input: forward(),
                })
                .unwrap();
        }
        let mut told = Vec::new();
        for _ in 0..100 {
            server.tick(std::slice::from_ref(&end));
            told.extend(received(&client));
            if server.character(0).and_then(Character::leash).is_some() {
                break;
            }
        }
        // The five were applied, in order, as the leash came (the burst of four at once, and the
        // fifth with the next tick's credit), and the client is told so.
        server.tick(std::slice::from_ref(&end));
        told.extend(received(&client));
        let last = told.last().expect("told");
        assert!(
            matches!(
                last,
                ServerMessage::Body {
                    spawn: 7,
                    applied: 5,
                    ..
                }
            ),
            "{last:?}"
        );
        assert!(told.iter().any(|m| matches!(
            m,
            ServerMessage::Leash {
                spawn: 7,
                from: 0,
                ..
            }
        )));
        // The next input is number 5, with no gap.
        client
            .send(ClientMessage::Input {
                seq: 5,
                input: forward(),
            })
            .unwrap();
        server.tick(std::slice::from_ref(&end));
        let told = received(&client);
        assert!(matches!(told[..], [ServerMessage::Body { applied: 6, .. }]));
    }

    /// A server on flat ground with a client (debug rights) whose character has its leash, and
    /// the messages so far taken.
    fn walking() -> (Server, ServerEnd, ClientEnd) {
        let (client, end) = loopback();
        let mut server = Server::new(flat_store(), [0.0; 3]).with_debug_client(0);
        client
            .send(ClientMessage::Spawn {
                spawn: 1,
                position: [0.0, 3.0, 0.0],
            })
            .unwrap();
        for _ in 0..200 {
            server.tick(std::slice::from_ref(&end));
            if server.character(0).and_then(Character::leash).is_some() {
                break;
            }
        }
        assert!(server.character(0).and_then(Character::leash).is_some());
        received(&client);
        (server, end, client)
    }

    fn input(seq: u64) -> ClientMessage {
        ClientMessage::Input {
            seq,
            input: forward(),
        }
    }

    /// The `applied` of the last Body the server sent.
    fn applied_told(client: &ClientEnd) -> Option<u64> {
        received(client).into_iter().rev().find_map(|m| match m {
            ServerMessage::Body { applied, .. } => Some(applied),
            _ => None,
        })
    }

    #[test]
    fn a_client_that_floods_inputs_gets_about_one_applied_per_tick() {
        let (mut server, end, client) = walking();
        let mut seq = 0;
        let mut flood = |server: &mut Server, count: u64| {
            for _ in 0..count {
                client.send(input(seq)).unwrap();
                seq += 1;
            }
            server.tick(std::slice::from_ref(&end));
        };
        // A few hundred at once: no more than the burst is applied.
        flood(&mut server, 300);
        assert!(server.ticks_run(0).unwrap() <= u64::from(INPUT_BURST));
        // And then five every tick for a second: nine eighths of one a tick, and the burst.
        let before = server.ticks_run(0).unwrap();
        for _ in 0..TICK_RATE_HZ {
            flood(&mut server, 5);
        }
        let run = server.ticks_run(0).unwrap() - before;
        assert!(
            run <= u64::from(TICK_RATE_HZ * INPUT_REFILL / INPUT_COST + INPUT_BURST),
            "{run}"
        );
        assert!(run >= u64::from(TICK_RATE_HZ) - 1, "not starved: {run}");
        // What it is holding is bounded too.
        let held = server.avatars[0].as_ref().unwrap().queued.len();
        assert!(held <= INPUT_BURST as usize, "{held}");
    }

    #[test]
    fn a_client_that_sends_an_input_a_tick_has_every_one_applied() {
        let (mut server, end, client) = walking();
        for seq in 0..120 {
            client.send(input(seq)).unwrap();
            server.tick(std::slice::from_ref(&end));
            assert_eq!(server.ticks_run(0), Some(seq + 1));
        }
        assert_eq!(applied_told(&client), Some(120));
    }

    #[test]
    fn a_gap_in_the_inputs_is_skipped_and_does_not_freeze_the_character() {
        let (mut server, end, client) = walking();
        for seq in [0, 1, 5, 6] {
            client.send(input(seq)).unwrap();
        }
        server.tick(std::slice::from_ref(&end));
        // The four that came were run, and the three missing never were.
        assert_eq!(server.ticks_run(0), Some(4));
        assert_eq!(applied_told(&client), Some(7));
        // The next one due is 7.
        client.send(input(7)).unwrap();
        server.tick(std::slice::from_ref(&end));
        assert_eq!(applied_told(&client), Some(8));
    }

    #[test]
    fn an_input_that_came_late_or_twice_is_ignored_and_one_far_ahead_costs_nothing() {
        let (mut server, end, client) = walking();
        for seq in [0, 1, 2] {
            client.send(input(seq)).unwrap();
        }
        server.tick(std::slice::from_ref(&end));
        assert_eq!(applied_told(&client), Some(3));
        // A repeat, and 3 after 4 (the channel is ordered, so this is a client's own doing).
        for seq in [2, 4, 3] {
            client.send(input(seq)).unwrap();
        }
        server.tick(std::slice::from_ref(&end));
        assert_eq!(server.ticks_run(0), Some(4), "0, 1, 2 and 4");
        assert_eq!(applied_told(&client), Some(5));
        // A number that would overflow the count, and one far past the one due, are ignored.
        for seq in [u64::MAX, 5 + MAX_QUEUED as u64, 5] {
            client.send(input(seq)).unwrap();
        }
        server.tick(std::slice::from_ref(&end));
        assert_eq!(applied_told(&client), Some(6));
    }

    /// Where the last Body that the server sent puts the character.
    fn placed(client: &ClientEnd) -> [f64; 3] {
        received(client)
            .into_iter()
            .rev()
            .find_map(|m| match m {
                ServerMessage::Body { state, .. } => Some(state.position),
                _ => None,
            })
            .expect("a Body")
    }

    fn respawn(client: &ClientEnd, spawn: u64, position: [f64; 3]) {
        client
            .send(ClientMessage::Spawn { spawn, position })
            .unwrap();
    }

    #[test]
    fn the_first_spawn_is_where_the_server_says_not_where_the_client_asks() {
        let (client, end) = loopback();
        let mut server = Server::new(flat_store(), [0.0, 3.0, 0.0]);
        respawn(&client, 1, [900.0, 3.0, 900.0]);
        server.tick(std::slice::from_ref(&end));
        assert_eq!(placed(&client), [0.0, 3.0, 0.0]);
    }

    #[test]
    fn debug_rights_are_a_clients_own() {
        let (first, first_end) = loopback();
        let (second, second_end) = loopback();
        let mut server = Server::new(flat_store(), [0.0, 3.0, 0.0]).with_debug_client(1);
        respawn(&first, 1, [500.0, 3.0, 0.0]);
        respawn(&second, 1, [500.0, 3.0, 0.0]);
        server.tick(&[first_end, second_end]);
        assert_eq!(placed(&first), [0.0, 3.0, 0.0]);
        assert_eq!(placed(&second), [500.0, 3.0, 0.0]);
    }

    /// A client without debug rights, standing with its leash.
    fn leashed() -> (Server, ServerEnd, ClientEnd, Leash) {
        let (client, end) = loopback();
        let mut server = Server::new(flat_store(), [0.0, 3.0, 0.0]);
        respawn(&client, 1, [0.0, 3.0, 0.0]);
        for _ in 0..200 {
            server.tick(std::slice::from_ref(&end));
            if server.character(0).and_then(Character::leash).is_some() {
                break;
            }
        }
        let leash = server
            .character(0)
            .and_then(Character::leash)
            .expect("a leash");
        received(&client);
        (server, end, client, leash)
    }

    #[test]
    fn a_respawn_inside_the_leash_is_accepted() {
        let (mut server, end, client, leash) = leashed();
        let inside = [leash.centre[0] + leash.radius * 0.5, 3.0, leash.centre[2]];
        respawn(&client, 2, inside);
        server.tick(std::slice::from_ref(&end));
        assert_eq!(placed(&client), inside);
    }

    #[test]
    fn a_respawn_outside_the_leash_or_at_nowhere_goes_to_the_centre_of_the_leash() {
        let (mut server, end, client, leash) = leashed();
        for (spawn, asked) in [
            (2, [leash.centre[0] + leash.radius * 2.0, 3.0, 0.0]),
            (3, [f64::NAN; 3]),
            (4, [f64::INFINITY, 0.0, 0.0]),
        ] {
            respawn(&client, spawn, asked);
            server.tick(std::slice::from_ref(&end));
            assert_eq!(placed(&client), leash.centre, "{asked:?}");
            // The new character has to get its leash before the next respawn is judged by one.
            for _ in 0..200 {
                server.tick(std::slice::from_ref(&end));
                if server.character(0).and_then(Character::leash).is_some() {
                    break;
                }
            }
            received(&client);
        }
    }

    #[test]
    fn a_respawn_before_the_leash_stays_where_the_character_is() {
        let (client, end) = loopback();
        let mut server = Server::new(flat_store(), [0.0, 3.0, 0.0]);
        respawn(&client, 1, [0.0, 3.0, 0.0]);
        server.tick(std::slice::from_ref(&end));
        assert!(server.character(0).and_then(Character::leash).is_none());
        received(&client);
        respawn(&client, 2, [300.0, 3.0, 0.0]);
        server.tick(std::slice::from_ref(&end));
        assert_eq!(placed(&client), [0.0, 3.0, 0.0]);
    }

    #[test]
    fn a_debug_client_lands_anywhere() {
        let (mut server, end, client) = walking();
        respawn(&client, 2, [900.0, 3.0, -900.0]);
        server.tick(std::slice::from_ref(&end));
        assert_eq!(placed(&client), [900.0, 3.0, -900.0]);
    }

    #[test]
    fn edit_reach_holds_after_a_respawn_that_the_server_turned_back() {
        let (mut server, end, client, leash) = leashed();
        // Asks to stand a kilometre away, next to something to dig, and digs it at once.
        let far = leash.centre[0] + 1000.0;
        respawn(&client, 2, [far, 3.0, 0.0]);
        client.send(dig(0, [far as i32, -2, 0], 1)).unwrap();
        server.tick(std::slice::from_ref(&end));
        let told = edits_heard(&client);
        assert!(
            matches!(
                told[..],
                [ServerMessage::Rejected {
                    reason: RejectReason::OutOfReach,
                    ..
                }]
            ),
            "{told:?}"
        );
    }

    /// A server that holds Digs to the time they take, with a character standing at the origin of
    /// a store of stone, and the time it takes to dig the hardest Material gone by since it came.
    fn timed() -> (Server, ServerEnd, ClientEnd) {
        let (client, end) = loopback();
        let mut server = Server::new(stone_store(), [0.0; 3]);
        client.send(stand()).unwrap();
        for _ in 0..dig_ticks(target_hardness(None)) {
            server.tick(std::slice::from_ref(&end));
        }
        received(&client);
        (server, end, client)
    }

    /// The hardness of `STONE`, in ticks of digging.
    fn stone_ticks() -> u64 {
        dig_ticks(MaterialTable::built_in().get(STONE).unwrap().hardness)
    }

    #[test]
    fn a_dig_before_the_time_it_takes_is_turned_down_and_after_it_is_made() {
        let (mut server, end, client) = timed();
        let ticks = stone_ticks();
        assert!(ticks > DIG_EARLY_TICKS + 1, "stone takes time: {ticks}");
        let at = |i: i32| EditOp::dig_sphere(VoxelCoord([3 + 2 * i, 3, 3]), 0);
        assert_eq!(answers(&mut server, &end, &client, &[at(0)]), [None]);
        // The next tick, and in the same one: too soon, and nothing is changed.
        let hash = server.store().content_hash();
        assert_eq!(
            answers(&mut server, &end, &client, &[at(1), at(2)]),
            [Some(RejectReason::TooSoon); 2]
        );
        assert_eq!(server.store().content_hash(), hash);
        // Once the time has gone (less the jitter allowed) it is made, and then waits again.
        for _ in 0..ticks - DIG_EARLY_TICKS - 2 {
            server.tick(std::slice::from_ref(&end));
        }
        assert_eq!(answers(&mut server, &end, &client, &[at(3)]), [None]);
        assert_eq!(
            answers(&mut server, &end, &client, &[at(4)]),
            [Some(RejectReason::TooSoon)]
        );
    }

    #[test]
    fn the_first_dig_after_arriving_pays_for_its_own_time_too() {
        let (client, end) = loopback();
        let mut server = Server::new(stone_store(), [0.0; 3]);
        client.send(stand()).unwrap();
        // In the tick it arrived in: too soon.
        client.send(dig(0, [3, 3, 3], 0)).unwrap();
        server.tick(std::slice::from_ref(&end));
        assert!(matches!(
            edits_heard(&client)[..],
            [ServerMessage::Rejected {
                reason: RejectReason::TooSoon,
                ..
            }]
        ));
        for _ in 0..stone_ticks() {
            server.tick(std::slice::from_ref(&end));
        }
        let at = EditOp::dig_sphere(VoxelCoord([3, 3, 3]), 0);
        assert_eq!(answers(&mut server, &end, &client, &[at]), [None]);
    }

    #[test]
    fn digging_faster_than_the_hardness_allows_gains_nothing_over_time() {
        let (mut server, end, client) = timed();
        let ticks = stone_ticks();
        // A Voxel of stone that has not been dug each time.
        let at = |i: u64| EditOp::dig_sphere(VoxelCoord([i as i32 - 12, 3, 3]), 0);
        // Trying every tick for ten seconds gets no more than the time allows, plus the jitter.
        let mut made = 0;
        for _ in 0..10 * TICK_RATE_HZ {
            if answers(&mut server, &end, &client, &[at(made)]) == [None] {
                made += 1;
            }
        }
        let expected = 10 * u64::from(TICK_RATE_HZ) / ticks;
        assert!(
            (expected - 1..=expected + 2).contains(&made),
            "{made} of {expected}"
        );
    }

    /// Ticks that a Dig of `to` has to wait after one of `from`, found by trying every tick, in a
    /// store of the two Materials side by side that a character at the origin stands in.
    fn wait_after(from: &str, to: &str) -> u64 {
        let table = MaterialTable::built_in();
        let mut store = BrickStore::new();
        for (x, name) in [from, to].into_iter().enumerate() {
            let material = Material(table.by_name(name).unwrap().id);
            store.insert(BrickCoord([x as i32, 0, 0]), Brick::solid(material));
        }
        let (client, end) = loopback();
        let mut server = Server::new(store, [0.0; 3]);
        client.send(stand()).unwrap();
        for _ in 0..dig_ticks(target_hardness(None)) {
            server.tick(std::slice::from_ref(&end));
        }
        received(&client);
        let first = EditOp::dig_sphere(VoxelCoord([3, 3, 3]), 0);
        let second = EditOp::dig_sphere(VoxelCoord([11, 3, 3]), 0);
        assert_eq!(answers(&mut server, &end, &client, &[first]), [None]);
        (1..1000)
            .find(|_| answers(&mut server, &end, &client, &[second]) == [None])
            .expect("it is made in the end")
    }

    #[test]
    fn the_wait_is_for_the_material_dug_next_not_the_one_dug_last() {
        let table = MaterialTable::built_in();
        let ticks = |name: &str| dig_ticks(table.by_name(name).unwrap().hardness);
        for (from, to) in [
            ("sand", "rock"),
            ("rock", "sand"),
            ("obsidian", "sand"),
            ("sand", "obsidian"),
        ] {
            // The tick that makes it is the wait of the Material dug next, less the jitter
            // allowed, from the last Dig.
            assert_eq!(
                wait_after(from, to),
                ticks(to) - DIG_EARLY_TICKS,
                "{from} then {to}"
            );
        }
    }

    #[test]
    fn a_dig_aimed_at_air_waits_as_long_as_the_hardest_material() {
        let (mut server, end, client) = timed();
        // The air above the stone, which a big Brush reaches down into the stone from.
        let big = EditOp::dig_sphere(VoxelCoord([0, 9, 3]), 6);
        assert_eq!(answers(&mut server, &end, &client, &[big]), [None]);
        // It was made, and the next waits as long as the hardest Material does.
        assert_eq!(
            answers(&mut server, &end, &client, &[big]),
            [Some(RejectReason::TooSoon)]
        );
        for _ in 0..dig_ticks(target_hardness(None)) - DIG_EARLY_TICKS - 2 {
            server.tick(std::slice::from_ref(&end));
        }
        assert_eq!(answers(&mut server, &end, &client, &[big]), [None]);
    }

    #[test]
    fn the_wait_between_digs_outlasts_a_respawn_and_places_are_not_held() {
        let (mut server, end, client) = timed();
        let at = |i: i32| EditOp::dig_sphere(VoxelCoord([3 + 2 * i, 3, 3]), 0);
        assert_eq!(answers(&mut server, &end, &client, &[at(0)]), [None]);
        client.send(stand()).unwrap();
        assert_eq!(
            answers(&mut server, &end, &client, &[at(1)]),
            [Some(RejectReason::TooSoon)]
        );
        // Places do not wait for it.
        let put = |i: i32| place([i * 3, 20, 0], net::Shape::Sphere, 0);
        assert_eq!(
            answers(&mut server, &end, &client, &[put(0), put(1), put(2)]),
            [None; 3]
        );
    }

    #[test]
    fn the_credit_for_inputs_rebuilds_after_a_stall_and_a_second_stall_drops_no_more_than_the_first()
     {
        // A stall: the client sends `lump` inputs in one tick, after which it sends one a tick.
        // Returns how many of the lump were dropped (the inputs the server did not deal with by
        // the end), and the ticks after which the credit was full again.
        let stall = |server: &mut Server, end: &ServerEnd, client: &ClientEnd, seq: &mut u64| {
            let (first, run_before) = (*seq, server.ticks_run(0).unwrap());
            for _ in 0..12 {
                client.send(input(*seq)).unwrap();
                *seq += 1;
            }
            server.tick(std::slice::from_ref(end));
            // One a tick from then on, until the credit is back.
            let mut ticks = 0;
            // (An honest client spends an input's worth of the credit each tick, which the next tick
            // refills, so the credit is full at the start of a tick when it is that much short of
            // full at the end of one.)
            while server.players[0].input_credit + INPUT_COST < INPUT_BURST * INPUT_COST {
                client.send(input(*seq)).unwrap();
                *seq += 1;
                server.tick(std::slice::from_ref(end));
                ticks += 1;
                assert!(ticks < 100, "the credit never came back");
            }
            // With the credit full nothing is held, so what was not run was dropped.
            let run = server.ticks_run(0).unwrap() - run_before;
            (*seq - first - run, ticks)
        };
        let (mut server, end, client) = walking();
        let mut seq = 0;
        let (dropped, ticks) = stall(&mut server, &end, &client, &mut seq);
        // The burst comes back in about half a second after the inputs that were held have been
        // applied (a tick refills one eighth more than the input it applies), which is under a
        // second in all.
        assert!(ticks <= u64::from(TICK_RATE_HZ), "{ticks}");
        let (dropped_again, _) = stall(&mut server, &end, &client, &mut seq);
        assert!(
            dropped_again <= dropped,
            "{dropped_again} against {dropped}"
        );
    }
}
