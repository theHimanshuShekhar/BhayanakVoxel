//! The server simulation: owns the Planet's bricks, validates and applies the Edits that
//! clients ask for, and tells every client about each one that it accepted.
//!
//! [`Server`] is the logic, driven by messages and with no clock, so tests call it directly.
//! [`spawn`] runs it on its own thread at a fixed tick rate behind a set of [`ServerEnd`]s: the
//! in-process single-player server today, and the dedicated server once renet carries the
//! messages instead of the loopback.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use brick_store::BrickStore;
use net::{ClientMessage, EditOp, Leash, MoveInput, RejectReason, ServerEnd, ServerMessage};

use crate::{Character, FixedTimestep, Ground, Movement, Terrain, apply, validate};

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

/// One client's character, how many of its inputs the server has applied, and the ground it
/// walks on.
struct Avatar {
    character: Character,
    applied: u64,
    ground: Ground,
    /// Leashes decided since the client was last told, each with the input it first holds for.
    fresh: Vec<(u64, Leash)>,
}

impl Avatar {
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
}

impl Server {
    pub fn new(store: BrickStore) -> Self {
        Self {
            store,
            global_seq: 0,
            movement: Movement::default(),
            ground_rate: None,
            avatars: Vec::new(),
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

    /// Validates the request and, if it passes, applies it and gives it the next global
    /// sequence number. A rejected request changes nothing and takes no number.
    fn handle_edit(&mut self, client_seq: u64, op: EditOp) -> Outcome {
        if let Err(reason) = validate(&op) {
            return Outcome::Rejected { client_seq, reason };
        }
        apply(&mut self.store, &op);
        self.global_seq += 1;
        Outcome::Applied {
            global_seq: self.global_seq,
            op,
            client_seq,
        }
    }

    /// (Re)places client `index`'s character, standing at `position` with no inputs applied. It
    /// has no leash until the ground around it is ready, and applies no input until then.
    fn spawn_character(&mut self, index: usize, position: [f64; 3]) {
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
            applied: 0,
            ground,
            fresh: Vec::new(),
        });
    }

    /// Applies one tick of input to client `index`'s character, if it has one, it has a leash, and
    /// `seq` is the input that is due (the channel is ordered, so any other is a repeat or a gap
    /// and is ignored). Returns whether it was applied.
    fn apply_input(&mut self, index: usize, seq: u64, input: &MoveInput) -> bool {
        let Some(Some(avatar)) = self.avatars.get_mut(index) else {
            return false;
        };
        if seq != avatar.applied || avatar.character.leash().is_none() {
            return false;
        }
        avatar
            .character
            .tick(&Terrain::new(&self.store, &avatar.ground), input);
        avatar.applied += 1;
        avatar.keep_ground();
        true
    }

    /// One tick: handles every request that has arrived, clients in order and each client's
    /// requests in the order it sent them. Each accepted Edit goes to every client, with the
    /// ack only for the one that asked; a reject goes to the one that asked. A client whose
    /// character moved is told where it is, once, after its requests, and before that of any new
    /// leash. A character with no leash yet is waiting for its ground, which is filled each tick.
    pub fn tick(&mut self, clients: &[ServerEnd]) {
        for (asker, client) in clients.iter().enumerate() {
            let mut moved = false;
            while let Ok(message) = client.try_recv() {
                match message {
                    ClientMessage::Edit { client_seq, op } => {
                        match self.handle_edit(client_seq, op) {
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
                    ClientMessage::Spawn { position } => {
                        self.spawn_character(asker, position);
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
            for (from, leash) in avatar.fresh.drain(..) {
                let _ = client.send(ServerMessage::Leash { from, leash });
            }
            if moved {
                let _ = client.send(ServerMessage::Body {
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

    #[test]
    fn a_request_is_applied_numbered_and_acked() {
        let (client, end) = loopback();
        let mut server = Server::new(stone_store());
        client.send(dig(0, [3, 3, 3], 2)).unwrap();
        server.tick(&[end]);
        let op = EditOp::dig_sphere(VoxelCoord([3, 3, 3]), 2);
        assert_eq!(
            received(&client),
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
        for seq in 0..4 {
            client.send(dig(seq, [seq as i32 * 4, 0, 0], 1)).unwrap();
        }
        server.tick(&[end]);
        let seqs: Vec<_> = received(&client)
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
        client.send(dig(0, [0, 0, 0], 200)).unwrap();
        client.send(dig(1, [0, 0, 0], 1)).unwrap();
        server.tick(&[end]);
        let replies = received(&client);
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
        a.send(dig(5, [0, 0, 0], 1)).unwrap();
        b.send(dig(9, [8, 0, 0], 1)).unwrap();
        server.tick(&[a_end, b_end]);
        let acks = |client: &ClientEnd| -> Vec<_> {
            received(client)
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
        b.send(dig(0, [0, 0, 0], 1)).unwrap();
        drop(a);
        server.tick(&[a_end, b_end]);
        assert_eq!(received(&b).len(), 1);
    }

    #[test]
    fn on_its_own_thread_it_ticks_at_about_60_hz_and_answers() {
        let (client, end) = loopback();
        let handle = spawn(stone_store, vec![end]);
        client.send(dig(0, [3, 3, 3], 2)).unwrap();
        let started = Instant::now();
        let reply = loop {
            match client.try_recv() {
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
                position: [0.0, 3.0, 0.0],
            })
            .unwrap();
        client.send(input(0)).unwrap();
        server.tick(std::slice::from_ref(&end));
        // The Spawn is answered. The input waits for the ground and is dropped.
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
                if let ServerMessage::Leash { from, leash } = message {
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
}
