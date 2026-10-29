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
use net::{ClientMessage, EditOp, RejectReason, ServerEnd, ServerMessage};

use crate::{FixedTimestep, apply, validate};

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

/// The authoritative Planet and the order of the Edits made to it.
pub struct Server {
    store: BrickStore,
    /// Global sequence number of the latest Edit; the first Edit is 1.
    global_seq: u64,
}

impl Server {
    pub fn new(store: BrickStore) -> Self {
        Self {
            store,
            global_seq: 0,
        }
    }

    pub fn store(&self) -> &BrickStore {
        &self.store
    }

    /// Validates the request and, if it passes, applies it and gives it the next global
    /// sequence number. A rejected request changes nothing and takes no number.
    pub fn handle(&mut self, message: ClientMessage) -> Outcome {
        let ClientMessage::Edit { client_seq, op } = message;
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

    /// One tick: handles every request that has arrived, clients in order and each client's
    /// requests in the order it sent them. Each accepted Edit goes to every client, with the
    /// ack only for the one that asked; a reject goes to the one that asked.
    pub fn tick(&mut self, clients: &[ServerEnd]) {
        for (asker, client) in clients.iter().enumerate() {
            while let Ok(message) = client.try_recv() {
                match self.handle(message) {
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
}
