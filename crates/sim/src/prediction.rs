//! A client's copy of the Planet, which shows its own Edits at once and then agrees with the
//! server's.
//!
//! An Edit is applied locally when the player asks for it, and kept as *pending* with what it
//! overwrote. The server's reply settles it: an ack means the prediction was right and it stops
//! being pending; a reject takes it back. Pending Edits are a stack, so taking one back means
//! reverting the newer ones first and applying them again afterwards. An Edit by another
//! client arrives ahead of the pending ones in the server's order, so it is applied under them
//! in the same way.

use std::collections::{BTreeSet, VecDeque};

use brick_store::{BrickCoord, BrickStore};
use net::{ClientMessage, EditOp, ServerMessage};

use crate::{Undo, apply};

struct Pending {
    client_seq: u64,
    op: EditOp,
    undo: Undo,
}

pub struct Predictor {
    store: BrickStore,
    next_client_seq: u64,
    pending: VecDeque<Pending>,
    desyncs: u64,
}

impl Predictor {
    /// `store` is the client's Planet, generated from the same seed as the server's.
    pub fn new(store: BrickStore) -> Self {
        Self {
            store,
            next_client_seq: 0,
            pending: VecDeque::new(),
            desyncs: 0,
        }
    }

    pub fn store(&self) -> &BrickStore {
        &self.store
    }

    /// Edits that the server has not answered yet.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Times the server's word did not fit what this client had asked for.
    pub fn desyncs(&self) -> u64 {
        self.desyncs
    }

    /// Applies `op` to the local Planet right away. Returns the request to send to the server
    /// and the bricks that changed, in ascending order.
    pub fn edit(&mut self, op: EditOp) -> (ClientMessage, Vec<BrickCoord>) {
        let client_seq = self.next_client_seq;
        self.next_client_seq += 1;
        let changed = self.predict(client_seq, op);
        (ClientMessage::Edit { client_seq, op }, changed)
    }

    /// Takes in the server's word. Returns the bricks that changed, in ascending order.
    pub fn receive(&mut self, message: ServerMessage) -> Vec<BrickCoord> {
        match message {
            ServerMessage::Applied {
                op,
                ack: Some(client_seq),
                ..
            } => {
                // The server answers in the order it was asked, so this is the oldest, and it
                // is the Edit that was predicted.
                let expected = self
                    .pending
                    .front()
                    .is_some_and(|p| p.client_seq == client_seq && p.op == op);
                if expected {
                    self.pending.pop_front();
                    return Vec::new();
                }
                // The server and this client disagree about what has been asked for. Trust the
                // server, and say so rather than drift unnoticed.
                debug_assert!(
                    false,
                    "ack {client_seq} does not match the oldest pending Edit"
                );
                self.desyncs += 1;
                eprintln!("desync: ack {client_seq} ({op:?}) is not the oldest pending Edit");
                self.apply_authoritative(op, Some(client_seq))
            }
            ServerMessage::Applied { op, ack: None, .. } => self.apply_authoritative(op, None),
            ServerMessage::Rejected { client_seq, .. } => {
                let Some(index) = self.pending.iter().position(|p| p.client_seq == client_seq)
                else {
                    return Vec::new();
                };
                let mut touched = BTreeSet::new();
                let mut again = self.revert_from(index, &mut touched).into_iter();
                again.next();
                for (client_seq, op) in again {
                    touched.extend(self.predict(client_seq, op));
                }
                touched.into_iter().collect()
            }
        }
    }

    /// Applies an Edit that the server made, under the pending ones. `settles` is the pending
    /// Edit it answers, if any, which is not applied again.
    fn apply_authoritative(&mut self, op: EditOp, settles: Option<u64>) -> Vec<BrickCoord> {
        let mut touched = BTreeSet::new();
        let again = self.revert_from(0, &mut touched);
        touched.extend(apply(&mut self.store, &op).bricks());
        for (client_seq, op) in again {
            if Some(client_seq) != settles {
                touched.extend(self.predict(client_seq, op));
            }
        }
        touched.into_iter().collect()
    }

    /// Applies `op` as a new pending Edit and returns the bricks it changed.
    fn predict(&mut self, client_seq: u64, op: EditOp) -> Vec<BrickCoord> {
        let undo = apply(&mut self.store, &op);
        let changed = undo.bricks().collect();
        self.pending.push_back(Pending {
            client_seq,
            op,
            undo,
        });
        changed
    }

    /// Reverts the pending Edits from `index` on, newest first, and returns them, oldest first,
    /// so that they can be applied again. Adds the bricks reverted to `touched`.
    fn revert_from(
        &mut self,
        index: usize,
        touched: &mut BTreeSet<BrickCoord>,
    ) -> Vec<(u64, EditOp)> {
        let mut reverted = Vec::new();
        while self.pending.len() > index {
            let pending = self.pending.pop_back().expect("longer than index");
            touched.extend(pending.undo.bricks());
            pending.undo.revert(&mut self.store);
            reverted.push((pending.client_seq, pending.op));
        }
        reverted.reverse();
        reverted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brick_store::{Brick, Material, VoxelCoord};
    use net::RejectReason;

    const STONE: Material = Material(7);

    fn stone_store() -> BrickStore {
        let mut store = BrickStore::new();
        for x in -2..2 {
            store.insert(BrickCoord([x, 0, 0]), Brick::solid(STONE));
        }
        store
    }

    fn op(centre: [i32; 3]) -> EditOp {
        EditOp::dig_sphere(VoxelCoord(centre), 2)
    }

    fn applied(op: EditOp, global_seq: u64, ack: Option<u64>) -> ServerMessage {
        ServerMessage::Applied {
            global_seq,
            op,
            ack,
        }
    }

    fn rejected(client_seq: u64) -> ServerMessage {
        ServerMessage::Rejected {
            client_seq,
            reason: RejectReason::BrushTooLarge,
        }
    }

    fn hash_after(ops: &[EditOp]) -> u64 {
        let mut store = stone_store();
        for op in ops {
            apply(&mut store, op);
        }
        store.content_hash()
    }

    #[test]
    fn an_edit_shows_at_once_and_is_numbered_from_zero() {
        let mut client = Predictor::new(stone_store());
        let (first, changed) = client.edit(op([3, 3, 3]));
        assert_eq!(
            first,
            ClientMessage::Edit {
                client_seq: 0,
                op: op([3, 3, 3])
            }
        );
        assert_eq!(changed, [BrickCoord([0, 0, 0])]);
        assert_eq!(client.store().get(VoxelCoord([3, 3, 3])), None);
        let (second, _) = client.edit(op([-5, 3, 3]));
        assert!(matches!(second, ClientMessage::Edit { client_seq: 1, .. }));
        assert_eq!(client.pending(), 2);
    }

    #[test]
    fn an_ack_keeps_the_change_and_settles_it() {
        let mut client = Predictor::new(stone_store());
        client.edit(op([3, 3, 3]));
        let after = client.store().content_hash();
        assert!(
            client
                .receive(applied(op([3, 3, 3]), 1, Some(0)))
                .is_empty()
        );
        assert_eq!(client.pending(), 0);
        assert_eq!(client.store().content_hash(), after);
        assert_eq!(after, hash_after(&[op([3, 3, 3])]));
    }

    #[test]
    fn a_reject_rolls_the_change_back() {
        let mut client = Predictor::new(stone_store());
        let before = client.store().content_hash();
        client.edit(op([3, 3, 3]));
        assert_eq!(client.receive(rejected(0)), [BrickCoord([0, 0, 0])]);
        assert_eq!(client.pending(), 0);
        assert_eq!(client.store().content_hash(), before);
    }

    #[test]
    fn rejecting_an_older_edit_keeps_the_newer_ones_applied_on_top() {
        let mut client = Predictor::new(stone_store());
        // Overlapping Edits in one brick, so that their order matters to the bricks' history.
        client.edit(op([3, 3, 3]));
        client.edit(op([5, 3, 3]));
        client.edit(op([-3, 3, 3]));
        client.receive(rejected(0));
        assert_eq!(client.pending(), 2);
        assert_eq!(
            client.store().content_hash(),
            hash_after(&[op([5, 3, 3]), op([-3, 3, 3])])
        );
        client.receive(applied(op([5, 3, 3]), 1, Some(1)));
        client.receive(applied(op([-3, 3, 3]), 2, Some(2)));
        assert_eq!(client.pending(), 0);
    }

    #[test]
    fn another_clients_edit_goes_under_the_pending_ones() {
        let mut client = Predictor::new(stone_store());
        client.edit(op([3, 3, 3]));
        // The server ordered the other client's Edit first.
        let changed = client.receive(applied(op([5, 3, 3]), 1, None));
        assert_eq!(changed, [BrickCoord([0, 0, 0])]);
        assert_eq!(client.pending(), 1);
        client.receive(applied(op([3, 3, 3]), 2, Some(0)));
        assert_eq!(
            client.store().content_hash(),
            hash_after(&[op([5, 3, 3]), op([3, 3, 3])])
        );
    }

    #[test]
    fn another_clients_edit_with_nothing_pending_changes_just_what_it_dug() {
        let mut client = Predictor::new(stone_store());
        assert_eq!(
            client.receive(applied(op([-9, 3, 3]), 1, None)),
            [BrickCoord([-2, 0, 0]), BrickCoord([-1, 0, 0])]
        );
    }

    /// In a debug build an unexpected ack is a bug and stops the run; in a release build it is
    /// applied as the server's Edit and counted.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "does not match the oldest pending Edit")]
    fn an_ack_that_is_not_the_oldest_pending_edit_is_a_bug_in_debug_builds() {
        let mut client = Predictor::new(stone_store());
        client.edit(op([3, 3, 3]));
        client.edit(op([5, 3, 3]));
        client.receive(applied(op([5, 3, 3]), 1, Some(1)));
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn an_ack_that_is_not_the_oldest_pending_edit_is_applied_as_the_servers_and_counted() {
        let mut client = Predictor::new(stone_store());
        client.edit(op([3, 3, 3]));
        client.edit(op([5, 3, 3]));
        // The server skipped our first Edit: it made the second, and we still predict the first.
        client.receive(applied(op([5, 3, 3]), 1, Some(1)));
        assert_eq!(client.desyncs(), 1);
        assert_eq!(client.pending(), 1);
        assert_eq!(
            client.store().content_hash(),
            hash_after(&[op([5, 3, 3]), op([3, 3, 3])])
        );
        // An ack with nothing pending is the same: the server made that Edit.
        let mut client = Predictor::new(stone_store());
        client.receive(applied(op([5, 3, 3]), 1, Some(0)));
        assert_eq!(client.desyncs(), 1);
        assert_eq!(client.store().content_hash(), hash_after(&[op([5, 3, 3])]));
    }

    #[test]
    fn matching_acks_are_not_desyncs() {
        let mut client = Predictor::new(stone_store());
        client.edit(op([3, 3, 3]));
        client.receive(applied(op([3, 3, 3]), 1, Some(0)));
        assert_eq!(client.desyncs(), 0);
    }

    #[test]
    fn a_reject_for_an_edit_not_pending_is_ignored() {
        let mut client = Predictor::new(stone_store());
        let before = client.store().content_hash();
        assert!(client.receive(rejected(7)).is_empty());
        assert_eq!(client.store().content_hash(), before);
    }
}
