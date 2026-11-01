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
use net::{BodyState, ClientMessage, EditOp, Leash, MoveInput, ServerMessage};

use crate::{Character, Ground, Movement, Terrain, Undo, apply};

struct Pending {
    client_seq: u64,
    op: EditOp,
    undo: Undo,
}

/// An input that the server has not said it applied, with where it predicted the character to be
/// after it.
struct PendingInput {
    seq: u64,
    input: MoveInput,
    after: BodyState,
}

/// The leash that holds for input `seq`: the latest that the server said holds from then or
/// before.
fn leash_at(leashes: &VecDeque<(u64, Leash)>, seq: u64) -> Option<Leash> {
    leashes
        .iter()
        .rev()
        .find(|&&(from, _)| from <= seq)
        .map(|&(_, leash)| leash)
}

pub struct Predictor {
    store: BrickStore,
    next_client_seq: u64,
    pending: VecDeque<Pending>,
    desyncs: u64,
    character: Option<Character>,
    next_input_seq: u64,
    pending_inputs: VecDeque<PendingInput>,
    body_desyncs: u64,
    /// The ground around the character, generated here from the store's source.
    ground: Ground,
    /// The server's leashes that still matter, each with the input it first holds for.
    leashes: VecDeque<(u64, Leash)>,
    /// The leash last found to be covered by the ground, so that it is looked for once.
    covered: Option<Leash>,
}

impl Predictor {
    /// `store` is the client's Planet, generated from the same seed as the server's.
    pub fn new(store: BrickStore) -> Self {
        let ground = Ground::new(store.source());
        Self {
            store,
            next_client_seq: 0,
            pending: VecDeque::new(),
            desyncs: 0,
            character: None,
            next_input_seq: 0,
            pending_inputs: VecDeque::new(),
            body_desyncs: 0,
            ground,
            leashes: VecDeque::new(),
            covered: None,
        }
    }

    /// Generates at most `rate` bricks of ground per tick, instead of the usual.
    pub fn with_ground_rate(mut self, rate: usize) -> Self {
        self.ground = Ground::new(self.store.source()).with_rate(rate);
        self
    }

    /// The ground around the character.
    pub fn ground(&self) -> &Ground {
        &self.ground
    }

    /// The character this client predicts, once it has spawned.
    pub fn character(&self) -> Option<&Character> {
        self.character.as_ref()
    }

    /// Times the server's word on the character differed from the prediction.
    pub fn body_desyncs(&self) -> u64 {
        self.body_desyncs
    }

    /// Puts the character on the Planet at `position`, standing there. Returns the request to
    /// send to the server. Spawning again replaces the character. It does not move until the
    /// server has said how far it may go.
    pub fn spawn(&mut self, position: [f64; 3]) -> ClientMessage {
        self.character = Some(Character::spawn(Movement::default(), position, None));
        self.next_input_seq = 0;
        self.pending_inputs.clear();
        self.leashes.clear();
        self.covered = None;
        self.ground.reset(position);
        ClientMessage::Spawn { position }
    }

    /// Runs one tick of the character on the local Planet, which shows it at once. Returns the
    /// request to send to the server, or `None` if the character has not spawned, or waits: for
    /// the server's word on how far it may go, or for the ground that word reaches (this client
    /// makes it, as the server does). A tick that waits is not an input, and the server does not
    /// hear of it. The ground around the character is filled either way.
    pub fn walk(&mut self, input: MoveInput) -> Option<ClientMessage> {
        let character = self.character.as_mut()?;
        let seq = self.next_input_seq;
        let leash = leash_at(&self.leashes, seq).filter(|leash| {
            if self.covered != Some(*leash) {
                if !self.ground.covers(leash) {
                    // Not made yet: make it, even if the character is somewhere else.
                    self.ground.centre_on(leash);
                    return false;
                }
                self.covered = Some(*leash);
            }
            true
        });
        let request = leash.map(|leash| {
            character.set_leash(Some(leash));
            character.tick(&Terrain::new(&self.store, &self.ground), &input);
            self.next_input_seq += 1;
            self.pending_inputs.push_back(PendingInput {
                seq,
                input,
                after: *character.state(),
            });
            ClientMessage::Input { seq, input }
        });
        self.ground.follow(character.state().position);
        self.ground.fill();
        request
    }

    /// Takes in where the server has the character after `applied` inputs. If that is not where
    /// this client had it, takes the server's and runs the inputs the server has not seen again
    /// from there.
    fn receive_body(&mut self, applied: u64, state: BodyState) {
        let Some(character) = self.character.as_mut() else {
            return;
        };
        let mut predicted = None;
        while self.pending_inputs.front().is_some_and(|p| p.seq < applied) {
            predicted = self.pending_inputs.pop_front().map(|p| p.after);
        }
        // The leashes from before `applied` are of no use to inputs that are run again.
        while self
            .leashes
            .get(1)
            .is_some_and(|&(from, _)| from <= applied)
        {
            self.leashes.pop_front();
        }
        // With no input applied yet (just after a spawn) the prediction is the spawn.
        let same = predicted.map_or(applied == 0, |p| p.identical(&state));
        if same {
            return;
        }
        self.body_desyncs += 1;
        character.set_state(state);
        for pending in &mut self.pending_inputs {
            character.set_leash(leash_at(&self.leashes, pending.seq));
            character.tick(&Terrain::new(&self.store, &self.ground), &pending.input);
            pending.after = *character.state();
        }
    }

    pub fn store(&self) -> &BrickStore {
        &self.store
    }

    /// The Planet's bricks, for a loader to hold more of them: bricks the generator makes, never
    /// an Edit (those come through [`Predictor::edit`] and the server's word, which keep the
    /// history to take them back). It must not replace a brick that is held.
    pub fn store_for_loading(&mut self) -> &mut BrickStore {
        &mut self.store
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
            ServerMessage::Body { applied, state } => {
                self.receive_body(applied, state);
                Vec::new()
            }
            ServerMessage::Leash { from, leash } => {
                self.leashes.push_back((from, leash));
                Vec::new()
            }
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

    const START: [f64; 3] = [0.0, 3.0, 0.0];

    fn ball(radius: f64) -> Leash {
        Leash {
            centre: [4.0, 4.0, 4.0],
            radius,
        }
    }

    #[test]
    fn the_character_waits_for_the_servers_leash_and_then_for_the_ground_it_reaches() {
        let mut client = Predictor::new(flat_store());
        assert!(client.walk(forward()).is_none(), "not spawned");
        client.spawn(START);
        assert!(client.walk(forward()).is_none(), "no leash yet");
        client.receive(ServerMessage::Leash {
            from: 0,
            leash: ball(80.0),
        });
        // The ground is made by the client, a little at a time, and then it goes: with the first
        // input, whatever number of ticks it waited.
        let mut waited = 0;
        let first = loop {
            assert_eq!(client.character().unwrap().state().position, START);
            match client.walk(forward()) {
                Some(request) => break request,
                None => waited += 1,
            }
            assert!(waited < 200, "never went");
        };
        assert!((10..100).contains(&waited), "waited {waited} ticks");
        assert!(matches!(first, ClientMessage::Input { seq: 0, .. }));
        assert!(client.ground().covers(&ball(80.0)));
    }

    /// The client has run ahead of a leash that the server then says holds from input 20: when
    /// the server corrects it, the inputs run again are held by it from that one, and not before
    /// or after.
    #[test]
    fn a_leash_holds_from_the_input_the_server_said_when_inputs_are_run_again() {
        // What the client should end at, if the inputs from `applied` on are held to `late` from
        // input `from`.
        let rerun = |client: &Predictor, inputs: &[MoveInput], from: usize| {
            let applied = 10;
            let mut character = Character::spawn(Movement::default(), START, Some(ball(80.0)));
            let terrain = Terrain::new(client.store(), client.ground());
            for input in &inputs[..applied] {
                character.tick(&terrain, input);
            }
            let mut told = *character.state();
            told.position[0] += 1.0;
            character.set_state(told);
            for (seq, input) in inputs.iter().enumerate().skip(applied) {
                character.set_leash(Some(ball(if seq < from { 80.0 } else { 14.0 })));
                character.tick(&terrain, input);
            }
            (told, *character.state())
        };

        let mut client = Predictor::new(flat_store());
        client.spawn(START);
        client.receive(ServerMessage::Leash {
            from: 0,
            leash: ball(80.0),
        });
        let mut inputs = Vec::new();
        while inputs.len() < 60 {
            if client.walk(forward()).is_some() {
                inputs.push(forward());
            }
        }
        let (told, want) = rerun(&client, &inputs, 20);
        // The same, if the leash held a tick sooner or later, would end up elsewhere.
        assert!(!rerun(&client, &inputs, 19).1.identical(&want));
        assert!(!rerun(&client, &inputs, 21).1.identical(&want));

        client.receive(ServerMessage::Leash {
            from: 20,
            leash: ball(14.0),
        });
        client.receive(ServerMessage::Body {
            applied: 10,
            state: told,
        });
        assert_eq!(client.body_desyncs(), 1);
        let now = client.character().unwrap().state();
        assert!(now.identical(&want), "{now:?}\n{want:?}");
    }
}
