//! The server simulation: owns the Planet's bricks, validates and applies the Edits that
//! clients ask for, and tells every client about each one that it accepted.
//!
//! [`Server`] is the logic, driven by messages and with no clock, so tests call it directly.
//! [`spawn`] runs it on its own thread at a fixed tick rate behind a set of [`ServerEnd`]s: the
//! in-process single-player server today, and the dedicated server once renet carries the
//! messages instead of the loopback.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use brick_store::BrickStore;
use net::{ClientMessage, EditOp, Leash, MoveInput, RejectReason, ServerEnd, ServerMessage};

use crate::{
    BrickCache, Character, EditBudget, FixedTimestep, Footing, Ground, Movement, Reads,
    apply_changes_from, plan_from, validate, within_reach, work,
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
/// Voxels: its height (1.8 m) and some to spare, so that a leash never takes the ground from under
/// or over it.
const CLEARANCE: f64 = 24.0;

/// Inputs held for a character that has no leash yet, at the most (ten seconds of them): see
/// [`ClientMessage::Spawn`].
const MAX_QUEUED: usize = 600;

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
    /// Inputs that arrived before the first leash, in order: the numbers `applied` on.
    queued: VecDeque<MoveInput>,
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
    /// The character of each client, by its index in the list given to [`Server::tick`].
    avatars: Vec<Option<Avatar>>,
    /// What each client may still ask in Edits, by the same index. It outlasts a spawn, so that
    /// spawning again is no way round a limit.
    budgets: Vec<EditBudget>,
    /// Units of work that the Edits made this tick asked, against [`EDIT_WORK_PER_TICK`].
    edit_work: u64,
    /// The bricks that Edits read where the store holds none, so that they are made once.
    cache: BrickCache,
    /// Ticks so far, which decide whose requests are handled first.
    tick_count: u64,
}

impl Server {
    pub fn new(store: BrickStore) -> Self {
        Self {
            store,
            global_seq: 0,
            movement: Movement::default(),
            ground_rate: None,
            avatars: Vec::new(),
            budgets: Vec::new(),
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

    pub fn store(&self) -> &BrickStore {
        &self.store
    }

    /// The character of client `index` (its place in the list given to [`Server::tick`]), if it
    /// has spawned.
    pub fn character(&self, index: usize) -> Option<&Character> {
        self.avatars.get(index)?.as_ref().map(|a| &a.character)
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
    /// Brush itself ([`validate`]), the work that it asks ([`work`], known from the op alone, so
    /// that the Planet is not read for an Edit that is not afforded): against what the player has
    /// left and what the server has left of this tick. Then the Edit is worked out ([`plan`]) at a
    /// cost that those have bounded, and the Voxels it changes are held to what the player may
    /// change, and if they are over it nothing is applied.
    fn handle_edit(&mut self, asker: usize, client_seq: u64, op: EditOp) -> Outcome {
        let reject = |reason| Outcome::Rejected { client_seq, reason };
        if self.budgets.len() <= asker {
            self.budgets.resize_with(asker + 1, EditBudget::full);
        }
        if !self.budgets[asker].take_op() {
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
        if !self.budgets[asker].has_work(asked) || self.edit_work + asked > EDIT_WORK_PER_TICK {
            return reject(RejectReason::RateLimited);
        }
        self.budgets[asker].take_work(asked);
        self.edit_work += asked;
        // The Edit reads the Planet from the store, then from the asker's Ground, which holds what
        // the generator makes round the character, and then from the cache.
        let ground = self
            .avatars
            .get(asker)
            .and_then(Option::as_ref)
            .map(|avatar| &avatar.ground);
        let mut reads = Reads::new(&self.store).with_cache(&mut self.cache);
        if let Some(ground) = ground {
            reads = reads.with_ground(ground);
        }
        let changes = plan_from(&mut reads, &op);
        if !self.budgets[asker].take_volume(changes.len() as u64) {
            return reject(RejectReason::VolumeLimited);
        }
        apply_changes_from(&mut self.store, &changes, ground, Some(&mut self.cache));
        self.global_seq += 1;
        Outcome::Applied {
            global_seq: self.global_seq,
            op,
            client_seq,
        }
    }

    /// (Re)places client `index`'s character, standing at `position` with no inputs applied. It
    /// has no leash until the ground around it is ready, and applies no input until then.
    fn spawn_character(&mut self, index: usize, spawn: u64, position: [f64; 3]) {
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
            queued: VecDeque::new(),
            ground,
            fresh: Vec::new(),
        });
    }

    /// Applies one tick of input to client `index`'s character, if it has one, it has a leash, and
    /// `seq` is the input that is due (the channel is ordered, so any other is a repeat or a gap
    /// and is ignored). Before the leash the input is held instead, to be applied when it comes.
    /// Returns whether it was applied.
    fn apply_input(&mut self, index: usize, seq: u64, input: &MoveInput) -> bool {
        let Some(Some(avatar)) = self.avatars.get_mut(index) else {
            return false;
        };
        if seq != avatar.applied + avatar.queued.len() as u64 {
            return false;
        }
        if avatar.character.leash().is_none() {
            if avatar.queued.len() < MAX_QUEUED {
                avatar.queued.push_back(*input);
            }
            return false;
        }
        avatar.apply(&self.store, input);
        true
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
        if self.budgets.len() < clients.len() {
            self.budgets.resize_with(clients.len(), EditBudget::full);
        }
        self.budgets.iter_mut().for_each(EditBudget::refill);
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
            // The inputs that came before the leash are applied as it comes.
            while avatar.character.leash().is_some()
                && let Some(input) = avatar.queued.pop_front()
            {
                avatar.apply(&self.store, &input);
                moved = true;
            }
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

/// Starts the server on a new thread. `make_store` builds the Planet on that thread, so the
/// caller need not wait for it: requests wait in their channels until it is ready.
pub fn spawn(
    make_store: impl FnOnce() -> BrickStore + Send + 'static,
    clients: Vec<ServerEnd>,
) -> ServerHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let ticks = Arc::new(AtomicU64::new(0));
    let thread = {
        let (stop, ticks) = (Arc::clone(&stop), Arc::clone(&ticks));
        std::thread::spawn(move || {
            let mut server = Server::new(make_store());
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
        let mut server = Server::new(stone_store());
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
        let mut server = Server::new(stone_store());
        client.send(stand()).unwrap();
        for seq in 0..4 {
            client.send(dig(seq, [seq as i32 * 4, 0, 0], 1)).unwrap();
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
        let mut server = Server::new(stone_store());
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
        let mut server = Server::new(stone_store());
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
        let mut server = Server::new(stone_store());
        b.send(stand()).unwrap();
        b.send(dig(0, [0, 0, 0], 1)).unwrap();
        drop(a);
        server.tick(&[a_end, b_end]);
        assert_eq!(edits_heard(&b).len(), 1);
    }

    #[test]
    fn on_its_own_thread_it_ticks_at_about_60_hz_and_answers() {
        let (client, end) = loopback();
        let handle = spawn(stone_store, vec![end]);
        client.send(stand()).unwrap();
        client.send(dig(0, [3, 3, 3], 2)).unwrap();
        let started = Instant::now();
        let reply = loop {
            match client.try_recv() {
                Ok(ServerMessage::Body { .. }) => {}
                Ok(reply) => break reply,
                Err(TryRecvError::Empty) if started.elapsed() < Duration::from_secs(5) => {
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
        let first = handle.ticks();
        std::thread::sleep(Duration::from_millis(500));
        // 30 ticks expected; the bounds leave room for a loaded CI machine.
        let ran = handle.ticks() - first;
        assert!((15..=45).contains(&ran), "{ran} ticks in half a second");
        let server = handle.stop();
        assert_eq!(server.store().get(VoxelCoord([3, 3, 3])), None);
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
        let mut server = Server::new(stone_store());
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
        let mut server = Server::new(stone_store());
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
        let (mut server, end, client) = standing();
        let eye = server.character(0).unwrap().eye();
        // Along y, which is the way up at the origin's own, odd spot, and so is the eyes' way.
        let at = |voxels: f64| {
            let y = (eye[1] + voxels).floor() as i32;
            EditOp::dig_sphere(VoxelCoord([0, y, 0]), 1)
        };
        assert_eq!(
            answers(&mut server, &end, &client, &[at(79.0), at(-79.0)]),
            [None, None]
        );
        let far = answers(&mut server, &end, &client, &[at(81.0), at(-81.0)]);
        assert_eq!(far, [Some(RejectReason::OutOfReach); 2]);
    }

    #[test]
    fn a_brush_bigger_than_2_m_and_a_material_that_is_not_defined_are_turned_down() {
        let (mut server, end, client) = standing();
        let big = place([0, 0, 0], net::Shape::Cube, 21);
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
        let ops = vec![EditOp::dig_sphere(VoxelCoord([0, 0, 0]), 0); 25];
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
    fn a_player_may_change_8_m3_of_voxels_a_second() {
        // A mass of stone 80 Voxels (8 m) across, with a character standing in the middle of it.
        let mut store = BrickStore::new();
        for x in -5..5 {
            for y in -5..5 {
                for z in -5..5 {
                    store.insert(BrickCoord([x, y, z]), Brick::solid(STONE));
                }
            }
        }
        let (client, end) = loopback();
        let mut server = Server::new(store);
        client.send(stand()).unwrap();
        server.tick(std::slice::from_ref(&end));
        received(&client);

        // Nine cubes of 21 Voxels a side, 9 261 Voxels each, side by side.
        let dig = |i: i32| EditOp {
            mode: net::EditMode::Dig,
            ..place(
                [(i % 3 - 1) * 22, 0, (i / 3 - 1) * 22],
                net::Shape::Cube,
                10,
            )
        };
        let nine: Vec<_> = (0..9).map(dig).collect();
        // The burst is 72 000 Voxels, which is seven of them. The two that did not fit changed
        // nothing and cost nothing.
        let told = answers(&mut server, &end, &client, &nine);
        assert_eq!(told[..7], [None; 7]);
        assert_eq!(told[7..], [Some(RejectReason::VolumeLimited); 2]);

        // A second brings back 8 000 Voxels, which is room for one more, and not for two.
        for _ in 0..TICK_RATE_HZ {
            server.tick(std::slice::from_ref(&end));
        }
        let told = answers(&mut server, &end, &client, &nine[7..]);
        assert_eq!(told, [None, Some(RejectReason::VolumeLimited)]);
    }

    /// A mass of stone 80 Voxels (8 m) across, with room to place in beyond it.
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
        let mut server = Server::new(mass());
        client.send(stand()).unwrap();
        server.tick(std::slice::from_ref(&end));
        received(&client);

        // A cube of 21 Voxels a side on the grid, in the middle of solid stone, where a Smooth
        // changes nothing. It asks 9 261 Voxels' worth, six units each: 55 566. The player's 360 000
        // are six of those, and no more, though none of them changes a Voxel.
        let smooth = smooth(net::Shape::Cube, 10, [0, 0, 0], net::Alignment::World);
        assert_eq!(crate::work(&smooth), 55_566);
        let told = answers(&mut server, &end, &client, &[smooth; 20]);
        assert_eq!(told[..6], [None; 6]);
        assert_eq!(told[6..], [Some(RejectReason::RateLimited); 14]);
        assert_eq!(server.edit_work(), 6 * 55_566);
        assert!(server.edit_work() <= EDIT_WORK_PER_TICK);

        // And the work comes back at 600 000 a second: ten thousand a tick, so one Smooth in
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
        let mut server = Server::new(mass());
        client.send(stand()).unwrap();
        server.tick(std::slice::from_ref(&end));
        received(&client);

        // In the air over the stone, on a lean, and turned an eighth: the box that holds it is
        // 300 000 Voxels and more, the most that any Brush asks.
        let cube = EditOp {
            alignment: net::Alignment::Local,
            rotation: 1,
            ..place([30, 60, 12], net::Shape::Cube, 20)
        };
        let asked = crate::work(&cube);
        assert!((250_000..=crate::WORK_BURST).contains(&asked), "{asked}");
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
    fn a_smooth_or_a_flatten_over_radius_10_is_too_big_and_a_dig_or_a_place_may_be_20() {
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
                asked(net::EditMode::Smooth, 11),
                asked(net::EditMode::Flatten, 11),
                asked(net::EditMode::Smooth, 10),
                asked(net::EditMode::Flatten, 10),
                asked(net::EditMode::Place, 20),
                asked(net::EditMode::Dig, 21),
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
        let mut server = Server::new(mass());
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
            ..place([0, 0, 0], net::Shape::Sphere, 20)
        }
    }

    fn leaning_cube() -> EditOp {
        EditOp {
            alignment: net::Alignment::Local,
            rotation: 1,
            ..place([30, 60, 12], net::Shape::Cube, 20)
        }
    }

    #[test]
    fn the_work_of_the_edits_of_a_tick_is_no_more_than_the_servers_share_whoever_asks() {
        // Fourteen players, every one with a full allowance, each asking for the biggest sphere
        // in the one tick: 68 921 each. Ten fit the tick's 720 000, and the other four are told
        // to ask again.
        let (mut server, clients, ends) = crowd(14);
        let everyone: Vec<usize> = (0..14).collect();
        let told = ask_all(&mut server, &clients, &ends, &everyone, big_sphere());
        assert_eq!(told.iter().filter(|t| t.is_none()).count(), 10);
        assert!(
            told.iter()
                .all(|t| t.is_none() || *t == Some(RejectReason::RateLimited))
        );
        assert_eq!(server.edit_work(), 10 * 68_921);
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
        // tilted cube of 4 m, over 300 000 each, of which two fit), each with a full allowance
        // again after a rest of a second. Whoever is served first goes round: over 14 ticks each
        // of them is served, and none is turned down by the tick every time.
        let (mut server, clients, ends) = crowd(14);
        assert!(crate::work(&leaning_cube()) > 250_000);
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
        // Nineteen players each make an ordinary Edit (9 261 Voxels' work), and one a cube of 4 m
        // on a lean (over 300 000): 176 000 and more, within the tick's 720 000, wherever in the
        // order the cube is.
        let (mut server, clients, ends) = crowd(20);
        let ordinary = EditOp {
            mode: net::EditMode::Dig,
            ..place([0, 0, 0], net::Shape::Cube, 10)
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
        assert!(server.edit_work() > 19 * 9_261 + 250_000);
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
        let mut server = Server::new(flat_store());
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
        assert!((32.0..=80.0).contains(&leash.radius), "{leash:?}");

        client.send(input(0)).unwrap();
        server.tick(std::slice::from_ref(&end));
        let told = received(&client);
        assert!(
            matches!(told[..], [ServerMessage::Body { applied: 1, .. }]),
            "{told:?}"
        );
    }

    #[test]
    fn a_new_leash_is_told_ahead_of_the_body_it_was_decided_in_and_from_the_input_after_it() {
        let (client, end) = loopback();
        let mut server = Server::new(flat_store());
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
        let mut server = Server::new(flat_store());
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
        // The five were applied, in order, as the leash came, and the client is told so.
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
}
