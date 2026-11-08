//! A client's copy of the Planet, which shows its own Edits at once and then agrees with the
//! server's.
//!
//! An Edit is applied locally when the player asks for it, and kept as *pending* with what it
//! overwrote. The server's reply settles it: an ack means the prediction was right and it stops
//! being pending; a reject takes it back. Pending Edits are a stack, so taking one back means
//! reverting the newer ones first and applying them again afterwards. An Edit by another
//! client arrives ahead of the pending ones in the server's order, so it is applied under them
//! in the same way.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};

use brick_store::{BrickCoord, BrickStore};
use net::{BodyState, ClientMessage, EditOp, Leash, MoveInput, RejectReason, ServerMessage};

use crate::{
    BrickCache, Character, EditBudget, Footing, Ground, Movement, Plan, Reads, Undo,
    apply_plan_from, plan_bricks, plan_bricks_from, validate, work, write_plan_from,
};

/// Writes `plan` to `store`: level 0 and the coarse levels, or level 0 only if `deferred` (see
/// [`Predictor::defer_lod`]).
fn write_edit(
    deferred: bool,
    store: &mut BrickStore,
    plan: &Plan,
    ground: Option<&Ground>,
    cache: Option<&mut BrickCache>,
) -> Undo {
    if deferred {
        write_plan_from(store, plan, ground, cache)
    } else {
        apply_plan_from(store, plan, ground, cache)
    }
}

/// What an Edit that is not yet answered took from the allowance, which it is to be given back if
/// the server turns it down for a reason that did not take it there (see
/// [`Predictor::receive`]).
#[derive(Clone, Copy)]
struct Spent {
    work: u64,
    volume: u64,
}

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

/// The most inputs that a client has sent and the server has not yet dealt with, ten seconds of
/// them. At the bound the character waits, as it does for the server's leash, rather than run on
/// with no word from the server.
pub const MAX_PENDING_INPUTS: usize = 600;

/// The leash that holds for input `seq`: the latest that the server said holds from then or
/// before.
fn leash_at(leashes: &VecDeque<(u64, Leash)>, seq: u64) -> Option<Leash> {
    leashes
        .iter()
        .rev()
        .find(|&&(from, _)| from <= seq)
        .map(|&(_, leash)| leash)
}

/// The client's Planet, which a thread besides the frame's may read and hold more bricks in (see
/// [`Predictor::shared_store`]).
pub type SharedStore = Arc<RwLock<BrickStore>>;

fn read(store: &SharedStore) -> RwLockReadGuard<'_, BrickStore> {
    store.read().expect("the store's lock is not poisoned")
}

pub struct Predictor {
    store: SharedStore,
    next_client_seq: u64,
    pending: VecDeque<Pending>,
    /// What each pending Edit spent of the allowance, by its number.
    spent: BTreeMap<u64, Spent>,
    desyncs: u64,
    character: Option<Character>,
    next_input_seq: u64,
    pending_inputs: VecDeque<PendingInput>,
    body_desyncs: u64,
    /// Times a desync had the ground made whole at once: see [`Predictor::full_fills`].
    full_fills: u64,
    /// Time spent in the ground's fill in [`Predictor::walk`] since [`Predictor::take_fill_time`].
    /// Only written to, for the client's frame breakdown: nothing here reads it.
    fill_time: Duration,
    /// The ground around the character, generated here from the store's source.
    ground: Ground,
    /// The server's leashes that still matter, each with the input it first holds for.
    leashes: VecDeque<(u64, Leash)>,
    /// The leash last found to be covered by the ground, so that it is looked for once.
    covered: Option<Leash>,
    /// How many times the character has been spawned: the number of the current spawn, which the
    /// server's words about the character carry.
    spawns: u64,
    /// What the server will let this client's Edits cost: its own count of the server's, ticked as
    /// the character is, so that an Edit that will be turned down is known beforehand.
    budget: EditBudget,
    /// The bricks that Edits read where the store holds none (and the ground does not), made once.
    cache: BrickCache,
    /// Whether Edits leave the store's coarse levels to the caller: see [`Predictor::defer_lod`].
    lod_deferred: bool,
}

impl Predictor {
    /// `store` is the client's Planet, generated from the same seed as the server's.
    pub fn new(store: BrickStore) -> Self {
        let ground = Ground::new(store.source());
        Self {
            store: Arc::new(RwLock::new(store)),
            next_client_seq: 0,
            pending: VecDeque::new(),
            spent: BTreeMap::new(),
            desyncs: 0,
            character: None,
            next_input_seq: 0,
            pending_inputs: VecDeque::new(),
            body_desyncs: 0,
            full_fills: 0,
            fill_time: Duration::ZERO,
            ground,
            leashes: VecDeque::new(),
            covered: None,
            spawns: 0,
            budget: EditBudget::full(),
            cache: BrickCache::new(),
            lod_deferred: false,
        }
    }

    /// From now on an Edit, or taking one back, changes level 0 of the store and no more: the
    /// coarse levels of its pyramid stay as they were over the bricks that the calls return (the
    /// bricks changed), until the caller remakes them with [`BrickStore::refresh_lod`] over those
    /// bricks, and before anything reads them. Remaking them is most of the time of a large Edit,
    /// and the client leaves it to the scene thread, which reads them.
    pub fn defer_lod(&mut self) {
        self.lod_deferred = true;
    }

    /// Generates at most `rate` bricks of ground per tick, instead of the usual.
    pub fn with_ground_rate(mut self, rate: usize) -> Self {
        self.ground = Ground::new(read(&self.store).source()).with_rate(rate);
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

    /// Times that running inputs again after a desync had to make the rest of the ground at once,
    /// which takes as long as a whole ball of bricks.
    pub fn full_fills(&self) -> u64 {
        self.full_fills
    }

    /// The time that [`Predictor::walk`] has spent making ground since the last call. It is
    /// wall-clock time, for reporting only, and no result of the simulation depends on it.
    pub fn take_fill_time(&mut self) -> Duration {
        std::mem::take(&mut self.fill_time)
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
        self.spawns += 1;
        ClientMessage::Spawn {
            spawn: self.spawns,
            position,
        }
    }

    /// Runs one tick of the character on the local Planet, which shows it at once. Returns the
    /// request to send to the server, or `None` if the character has not spawned, or waits: for
    /// the server's word on how far it may go, or for the ground that word reaches (this client
    /// makes it, as the server does). A tick that waits is not an input, and the server does not
    /// hear of it. The ground around the character is filled either way.
    pub fn walk(&mut self, input: MoveInput) -> Option<ClientMessage> {
        // Every call is a tick, which gives the Edit allowance its credit, as the server's does.
        self.budget.refill();
        let character = self.character.as_mut()?;
        let seq = self.next_input_seq;
        let leash = leash_at(&self.leashes, seq)
            .filter(|_| self.pending_inputs.len() < MAX_PENDING_INPUTS)
            .filter(|leash| {
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
            character.tick(&Footing::new(&read(&self.store), &self.ground), &input);
            self.next_input_seq += 1;
            self.pending_inputs.push_back(PendingInput {
                seq,
                input,
                after: *character.state(),
            });
            ClientMessage::Input { seq, input }
        });
        self.ground.follow(character.state().position);
        let filling = Instant::now();
        self.ground.fill();
        self.fill_time += filling.elapsed();
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
        // With no input applied yet (just after a spawn) the prediction is the spawn, unless the
        // server put the character somewhere else than it asked: then the character is where the
        // server says, and the ground is made there.
        if applied == 0 && predicted.is_none() {
            if self.pending_inputs.is_empty() && !character.state().identical(&state) {
                character.set_state(state);
                self.ground.reset(state.position);
            }
            return;
        }
        if predicted.is_some_and(|p| p.identical(&state)) {
            return;
        }
        self.body_desyncs += 1;
        character.set_state(state);
        for pending in &mut self.pending_inputs {
            let leash = leash_at(&self.leashes, pending.seq);
            if let Some(leash) = leash
                && self.covered != Some(leash)
            {
                // Run again under a leash that the ground here does not reach yet: make the
                // ground (this is rare, and the one time that a tick is spent on it) so that the
                // inputs meet what the server's did.
                if !self.ground.covers(&leash) {
                    self.ground.centre_on(&leash);
                    self.full_fills += 1;
                    while !self.ground.is_complete() {
                        self.ground.fill();
                    }
                }
                self.covered = Some(leash);
            }
            character.set_leash(leash);
            character.tick(
                &Footing::new(&read(&self.store), &self.ground),
                &pending.input,
            );
            pending.after = *character.state();
        }
    }

    /// Reads the Planet's bricks. Do not call this while holding another guard of the same store
    /// on this thread: a thread that waits to write would stop the second read, and so this one.
    pub fn store(&self) -> RwLockReadGuard<'_, BrickStore> {
        read(&self.store)
    }

    fn store_mut(&mut self) -> RwLockWriteGuard<'_, BrickStore> {
        self.store
            .write()
            .expect("the store's lock is not poisoned")
    }

    /// The Planet's bricks, for a thread that selects what is drawn from them, and holds more of
    /// them as a loader generates them: bricks the generator makes, never an Edit (those come
    /// through [`Predictor::edit`] and the server's word, which keep the history to take them
    /// back). It must not replace a brick that is held. Edits write to the store through this
    /// predictor, which waits for the readers it finds.
    pub fn shared_store(&self) -> SharedStore {
        Arc::clone(&self.store)
    }

    /// Edits that the server has not answered yet.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Times the server's word did not fit what this client had asked for.
    pub fn desyncs(&self) -> u64 {
        self.desyncs
    }

    /// Whether the server will not turn `op` down for something that is known here from the op
    /// alone: a Brush that is not allowed (a Smooth or Flatten over 1 m among them), or more
    /// Edits or work than the allowance that this client counts as the server's. The Voxels that
    /// the Edit changes are not known without working it out: see [`Predictor::can_change`]. The
    /// reach is not counted: it depends on where the server has the character, which is a little
    /// behind. An Edit that is not made is not counted either.
    pub fn foresee(&self, op: &EditOp) -> Result<(), RejectReason> {
        validate(op)?;
        if !self.budget.has_op() || !self.budget.has_work(work(op)) {
            return Err(RejectReason::RateLimited);
        }
        Ok(())
    }

    /// The Voxels that `op` changes here, brick by brick (see [`crate::plan_bricks_from`]):
    /// reading the Planet from the store, then the ground around the character, then a cache of
    /// what the store does not hold, so that the bricks are not made again for each Edit or each
    /// look of the ghost. The Edit is applied from this, and the ghost is made from it.
    pub fn plan_bricks(&mut self, op: &EditOp) -> Plan {
        let store = read(&self.store);
        plan_bricks_from(
            &mut Reads::new(&store)
                .with_ground(&self.ground)
                .with_cache(&mut self.cache),
            op,
        )
    }

    /// Whether an Edit that changes `voxels` Voxels is within what this client counts that the
    /// server will let it change.
    pub fn can_change(&self, voxels: usize) -> bool {
        self.budget.has_volume(voxels as u64)
    }

    /// Applies `op` to the local Planet right away, and counts it against the allowance. Returns
    /// the request to send to the server and the bricks that changed, in ascending order. An op
    /// that [`Predictor::foresee`] says is turned down is applied all the same, and taken back
    /// when the server says so: it is for the caller to not make one (see
    /// [`Predictor::edit_checked`]).
    pub fn edit(&mut self, op: EditOp) -> (ClientMessage, Vec<BrickCoord>) {
        let plan = self.plan_bricks(&op);
        let spent = self.spend(&op, plan.len());
        self.edit_planned(op, plan, spent)
    }

    /// [`Predictor::edit`], but not made if the server is known to turn it down for what
    /// [`Predictor::foresee`] and [`Predictor::can_change`] say: that is the reason, and nothing is
    /// applied or counted. The Edit is worked out once.
    pub fn edit_checked(
        &mut self,
        op: EditOp,
    ) -> Result<(ClientMessage, Vec<BrickCoord>), RejectReason> {
        self.foresee(&op)?;
        let plan = self.plan_bricks(&op);
        if !self.can_change(plan.len()) {
            return Err(RejectReason::VolumeLimited);
        }
        let spent = self.spend(&op, plan.len());
        Ok(self.edit_planned(op, plan, spent))
    }

    /// Counts an Edit that changes `changed` Voxels against the allowance, as the server does: an
    /// Edit and its work, then the Voxels if there are as many left.
    /// Returns what was taken besides the Edit.
    fn spend(&mut self, op: &EditOp, changed: usize) -> Spent {
        let mut spent = Spent { work: 0, volume: 0 };
        if validate(op).is_ok() {
            self.budget.take_op();
            let asked = work(op);
            if self.budget.take_work(asked) {
                spent.work = asked;
            }
            if self.budget.take_volume(changed as u64) {
                spent.volume = changed as u64;
            }
        }
        spent
    }

    fn edit_planned(
        &mut self,
        op: EditOp,
        plan: Plan,
        spent: Spent,
    ) -> (ClientMessage, Vec<BrickCoord>) {
        let client_seq = self.next_client_seq;
        self.next_client_seq += 1;
        self.spent.insert(client_seq, spent);
        let changed = self.predict_plan(client_seq, op, &plan);
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
                    self.spent.remove(&client_seq);
                    return Vec::new();
                }
                self.spent.remove(&client_seq);
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
            // What the server said about a character this client has since replaced is not about
            // this one.
            ServerMessage::Body { spawn, .. } | ServerMessage::Leash { spawn, .. }
                if spawn != self.spawns =>
            {
                Vec::new()
            }
            ServerMessage::Body { applied, state, .. } => {
                self.receive_body(applied, state);
                Vec::new()
            }
            ServerMessage::Leash { from, leash, .. } => {
                self.leashes.push_back((from, leash));
                Vec::new()
            }
            ServerMessage::Rejected { client_seq, reason } => {
                // The server takes an Edit's credit, and its work if the Edit is within what it
                // asks of the server, and the Voxels only for an Edit that it makes. What it did
                // not take of what the client counted, the client gives back, so that it does not
                // go short of what the server says it has.
                if let Some(spent) = self.spent.remove(&client_seq) {
                    self.budget.refund_volume(spent.volume);
                    if reason != RejectReason::VolumeLimited {
                        self.budget.refund_work(spent.work);
                    }
                }
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
        let deferred = self.lod_deferred;
        let again = self.revert_from(0, &mut touched);
        let mut store = self.store_mut();
        let plan = plan_bricks(&store, &op);
        touched.extend(write_edit(deferred, &mut store, &plan, None, None).bricks());
        drop(store);
        for (client_seq, op) in again {
            if Some(client_seq) != settles {
                touched.extend(self.predict(client_seq, op));
            }
        }
        touched.into_iter().collect()
    }

    /// Applies `op` as a new pending Edit and returns the bricks it changed.
    fn predict(&mut self, client_seq: u64, op: EditOp) -> Vec<BrickCoord> {
        let plan = self.plan_bricks(&op);
        self.predict_plan(client_seq, op, &plan)
    }

    /// [`Predictor::predict`] for an Edit that is already worked out.
    fn predict_plan(&mut self, client_seq: u64, op: EditOp, plan: &Plan) -> Vec<BrickCoord> {
        let mut store = self
            .store
            .write()
            .expect("the store's lock is not poisoned");
        let undo = write_edit(
            self.lod_deferred,
            &mut store,
            plan,
            Some(&self.ground),
            Some(&mut self.cache),
        );
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
            if self.lod_deferred {
                pending.undo.revert_bricks(&mut self.store_mut());
            } else {
                pending.undo.revert(&mut self.store_mut());
            }
            reverted.push((pending.client_seq, pending.op));
        }
        reverted.reverse();
        reverted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply;
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
    fn the_client_foresees_the_rate_and_volume_limits_as_the_server_counts_them() {
        let mut client = Predictor::new(stone_store());
        let small = EditOp::dig_sphere(VoxelCoord([3, 3, 3]), 1);
        assert_eq!(client.foresee(&small), Ok(()));
        // Not allowed at all.
        let too_big = EditOp::dig_sphere(VoxelCoord([3, 3, 3]), crate::MAX_BRUSH_RADIUS + 1);
        assert_eq!(client.foresee(&too_big), Err(RejectReason::BrushTooLarge));
        // Twenty Edits in a second, and then no more until the ticks bring credit.
        for _ in 0..20 {
            assert_eq!(client.foresee(&small), Ok(()));
            client.edit(small);
        }
        assert_eq!(client.foresee(&small), Err(RejectReason::RateLimited));
        for _ in 0..3 {
            client.walk(MoveInput::default());
        }
        assert_eq!(client.foresee(&small), Ok(()));
        client.edit(small);
        assert_eq!(client.foresee(&small), Err(RejectReason::RateLimited));
    }

    /// Solid stone 96 Voxels across, round the origin: room for the biggest cube.
    fn mass() -> BrickStore {
        let mut store = BrickStore::new();
        for x in -6..6 {
            for y in -6..6 {
                for z in -6..6 {
                    store.insert(BrickCoord([x, y, z]), Brick::solid(STONE));
                }
            }
        }
        store
    }

    fn cube(mode: net::EditMode, radius: u16) -> EditOp {
        EditOp {
            mode,
            brush: net::Brush {
                shape: net::Shape::Cube,
                radius,
            },
            centre: VoxelCoord([0, 0, 0]),
            material: STONE,
            alignment: net::Alignment::World,
            rotation: 0,
        }
    }

    #[test]
    fn the_client_foresees_that_a_second_big_edit_changes_more_voxels_than_it_may() {
        let mut client = Predictor::new(mass());
        // The biggest cube, 81 Voxels a side, dug out of the stone: 531 441 Voxels changed, of 576 000.
        let big = cube(net::EditMode::Dig, crate::MAX_BRUSH_RADIUS);
        assert_eq!(client.foresee(&big), Ok(()));
        let (_, changed) = client.edit_checked(big).unwrap();
        assert!(!changed.is_empty());
        // Another is not made: it is not worked out against the Planet, which stays as it is.
        let hash = client.store().content_hash();
        assert_eq!(
            client.edit_checked(cube(net::EditMode::Place, crate::MAX_BRUSH_RADIUS)),
            Err(RejectReason::VolumeLimited)
        );
        assert_eq!(client.store().content_hash(), hash);
        assert_eq!(client.pending(), 1, "and it is not asked of the server");
        // A small one fits what is left (44 559 Voxels): a cube of 9 changes 729.
        assert!(client.edit_checked(cube(net::EditMode::Place, 4)).is_ok());
        // The ticks bring it back, 64 000 Voxels a second.
        for _ in 0..60 {
            client.walk(MoveInput::default());
        }
        assert!(client.can_change(100_000) && !client.can_change(110_000));
    }

    /// What the allowance is after making `op` and being told `reason`, worked out as the server
    /// does: it took the Edit's credit; the work, if the Edit was within what it asks of the
    /// server; and the Voxels, only if it made the Edit.
    fn server_took(op: &EditOp, reason: Option<RejectReason>, changed: u64) -> EditBudget {
        let mut budget = EditBudget::full();
        budget.take_op();
        let asked_work = !matches!(
            reason,
            Some(
                RejectReason::RateLimited
                    | RejectReason::OutOfReach
                    | RejectReason::BrushTooLarge
                    | RejectReason::OutOfRange
                    | RejectReason::BadRotation
                    | RejectReason::UnknownMaterial
                    | RejectReason::TooSoon
            )
        );
        if asked_work {
            budget.take_work(work(op));
        }
        if reason.is_none() {
            budget.take_volume(changed);
        }
        budget
    }

    #[test]
    fn a_reject_gives_the_client_back_what_the_server_did_not_take() {
        let big = cube(net::EditMode::Dig, crate::MAX_BRUSH_RADIUS);
        for reason in [
            RejectReason::RateLimited,
            RejectReason::OutOfReach,
            RejectReason::TooSoon,
            RejectReason::VolumeLimited,
        ] {
            let mut client = Predictor::new(mass());
            let (_, changed) = client.edit_checked(big).unwrap();
            assert!(!changed.is_empty());
            // The Edit is out of the allowance: the Voxels, the work and the credit.
            assert!(!client.can_change(50_000));
            client.receive(rejected_with(0, reason));
            assert_eq!(
                client.budget,
                server_took(&big, Some(reason), 0),
                "{reason:?}"
            );
            assert_eq!(client.pending(), 0);
        }
        // For a rate or a reach that is the credit and nothing else, so the client does not hold
        // back a big Brush for seconds while the server has all of its allowance.
        let mut client = Predictor::new(mass());
        client.edit_checked(big).unwrap();
        client.receive(rejected_with(0, RejectReason::RateLimited));
        assert!(client.foresee(&big).is_ok() && client.can_change(531_441));
    }

    #[test]
    fn what_the_server_made_is_not_given_back_and_a_reject_is_given_back_once() {
        let big = cube(net::EditMode::Dig, crate::MAX_BRUSH_RADIUS);
        let mut client = Predictor::new(mass());
        let (_, changed) = client.edit_checked(big).unwrap();
        let after = client.budget;
        assert!(!changed.is_empty());
        // Acked: it was made, and the allowance stays as spent.
        client.receive(applied(big, 1, Some(0)));
        assert_eq!(client.budget, after);
        assert_eq!(client.budget, server_took(&big, None, 531_441));
        // A reject for an Edit that is no longer pending (or told twice) gives back nothing more.
        client.receive(rejected_with(0, RejectReason::RateLimited));
        assert_eq!(client.budget, after);
        let mut other = Predictor::new(mass());
        other.edit_checked(big).unwrap();
        other.receive(rejected_with(0, RejectReason::RateLimited));
        let once = other.budget;
        other.receive(rejected_with(0, RejectReason::RateLimited));
        assert_eq!(other.budget, once);
        // A rest is more than a full allowance: nothing is given back past it.
        other.edit_checked(big).unwrap();
        for _ in 0..600 {
            other.walk(MoveInput::default());
        }
        assert_eq!(other.budget, EditBudget::full());
        other.receive(rejected_with(1, RejectReason::VolumeLimited));
        assert_eq!(other.budget, EditBudget::full());
    }

    /// A reject of Edit number `client_seq`, for `reason`.
    fn rejected_with(client_seq: u64, reason: RejectReason) -> ServerMessage {
        ServerMessage::Rejected { client_seq, reason }
    }

    #[test]
    fn the_client_foresees_the_work_that_a_smooth_asks_and_that_one_over_1_m_is_too_big() {
        let mut client = Predictor::new(mass());
        let smooth = cube(net::EditMode::Smooth, crate::MAX_SMOOTH_RADIUS);
        // 6 times 68 921: six of them are 2.48 million of the 2.88 million, and they change nothing.
        for _ in 0..6 {
            assert_eq!(client.foresee(&smooth), Ok(()));
            client.edit(smooth);
        }
        assert_eq!(client.foresee(&smooth), Err(RejectReason::RateLimited));
        // Not Voxels changed: a Dig of a small cube is not afforded for lack of work either, but
        // that left is 400 000 units.
        assert_eq!(client.foresee(&cube(net::EditMode::Dig, 8)), Ok(()));
        assert_eq!(
            client.foresee(&cube(net::EditMode::Dig, crate::MAX_BRUSH_RADIUS)),
            Err(RejectReason::RateLimited)
        );
        assert!(client.can_change(crate::VOXEL_BURST as usize - 1));
        assert_eq!(
            client.foresee(&cube(net::EditMode::Smooth, crate::MAX_SMOOTH_RADIUS + 1)),
            Err(RejectReason::BrushTooLarge)
        );
        assert_eq!(
            client.foresee(&cube(net::EditMode::Flatten, crate::MAX_SMOOTH_RADIUS + 1)),
            Err(RejectReason::BrushTooLarge)
        );
    }

    /// The stone store with a pyramid of two coarse levels.
    fn stone_pyramid() -> BrickStore {
        let mut store = stone_store();
        store.build_lod(2);
        store
    }

    #[test]
    fn leaving_the_coarse_levels_to_the_caller_makes_the_levels_that_making_them_at_once_does() {
        let (mut now, mut later) = (
            Predictor::new(stone_pyramid()),
            Predictor::new(stone_pyramid()),
        );
        later.defer_lod();
        // What the caller does with the bricks that a call says changed, here as the scene thread
        // does: remake the levels over them. Until then they are stale.
        let remake = |later: &Predictor, touched: &[BrickCoord]| {
            let shared = later.shared_store();
            let mut store = shared.write().unwrap();
            store.refresh_lod(touched.iter().copied());
        };
        let same = |now: &Predictor, later: &Predictor, what: &str| {
            let (a, b) = (now.store(), later.store());
            assert_eq!(a.content_hash(), b.content_hash(), "{what}");
            assert_eq!(a.lod_hash(), b.lod_hash(), "{what}");
        };
        // Two overlapping Edits, one of them unrefreshed when the next is made.
        let (first, second) = (op([3, 3, 3]), op([5, 4, 3]));
        let (_, touched) = now.edit(first);
        let (_, deferred) = later.edit(first);
        assert_eq!(touched, deferred);
        assert_ne!(
            now.store().lod_hash(),
            later.store().lod_hash(),
            "not made yet"
        );
        now.edit(second);
        let (_, deferred_too) = later.edit(second);
        remake(&later, &[deferred, deferred_too].concat());
        same(&now, &later, "two Edits");
        // The first is turned down: the second stays on top of nothing, as the prediction says.
        let (touched, deferred) = (now.receive(rejected(0)), later.receive(rejected(0)));
        assert_eq!(touched, deferred);
        remake(&later, &deferred);
        same(&now, &later, "after a reject");
        // The server's own Edit goes under the pending one.
        let other = op([-6, 2, 2]);
        let (touched, deferred) = (
            now.receive(applied(other, 1, None)),
            later.receive(applied(other, 1, None)),
        );
        assert_eq!(touched, deferred);
        remake(&later, &deferred);
        same(&now, &later, "after another's Edit");
        // And the last, taken back.
        let (touched, deferred) = (now.receive(rejected(1)), later.receive(rejected(1)));
        assert_eq!(touched, deferred);
        remake(&later, &deferred);
        same(&now, &later, "after the last reject");
        assert_eq!(later.store().content_hash(), hash_after(&[other]));
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
    fn a_dig_that_is_too_soon_rolls_the_change_back_like_any_reject() {
        let mut client = Predictor::new(stone_store());
        let before = client.store().content_hash();
        client.edit(op([3, 3, 3]));
        client.edit(op([-3, 3, 3]));
        let reject = rejected_with(0, RejectReason::TooSoon);
        assert!(!client.receive(reject).is_empty());
        assert_eq!(client.pending(), 1, "the later Dig stays");
        assert_eq!(client.store().content_hash(), hash_after(&[op([-3, 3, 3])]));
        client.receive(rejected_with(1, RejectReason::TooSoon));
        assert_eq!(client.pending(), 0);
        assert_eq!(client.store().content_hash(), before);
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
            spawn: 1,
            from: 0,
            leash: ball(f64::from(crate::ground::REACH)),
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
        assert!(
            client
                .ground()
                .covers(&ball(f64::from(crate::ground::REACH)))
        );
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
            let store = client.store();
            let footing = Footing::new(&store, client.ground());
            for input in &inputs[..applied] {
                character.tick(&footing, input);
            }
            let mut told = *character.state();
            told.position[0] += 1.0;
            character.set_state(told);
            for (seq, input) in inputs.iter().enumerate().skip(applied) {
                character.set_leash(Some(ball(if seq < from { 80.0 } else { 14.0 })));
                character.tick(&footing, input);
            }
            (told, *character.state())
        };

        let mut client = Predictor::new(flat_store());
        client.spawn(START);
        client.receive(ServerMessage::Leash {
            spawn: 1,
            from: 0,
            leash: ball(f64::from(crate::ground::REACH)),
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
            spawn: 1,
            from: 20,
            leash: ball(14.0),
        });
        client.receive(ServerMessage::Body {
            spawn: 1,
            applied: 10,
            state: told,
        });
        assert_eq!(client.body_desyncs(), 1);
        let now = client.character().unwrap().state();
        assert!(now.identical(&want), "{now:?}\n{want:?}");
    }

    #[test]
    fn the_character_goes_where_the_server_put_it_when_that_is_not_where_it_asked() {
        let mut client = Predictor::new(flat_store());
        client.spawn(START);
        let mut put = *client.character().unwrap().state();
        put.position = [10.0, 3.0, 0.0];
        client.receive(ServerMessage::Body {
            spawn: 1,
            applied: 0,
            state: put,
        });
        assert!(client.character().unwrap().state().identical(&put));
        // Not a desync, which is a prediction that was wrong.
        assert_eq!(client.body_desyncs(), 0);
        // The ground is made around the place, and the walk goes on from it.
        client.receive(ServerMessage::Leash {
            spawn: 1,
            from: 0,
            leash: Leash {
                centre: [10.0, 3.0, 0.0],
                radius: 80.0,
            },
        });
        assert!((0..300).any(|_| client.walk(forward()).is_some()));
        assert!(client.ground().covers(&Leash {
            centre: [10.0, 3.0, 0.0],
            radius: 80.0,
        }));
    }

    #[test]
    fn inputs_the_server_has_not_dealt_with_are_bounded_and_the_character_waits_at_the_bound() {
        let mut client = Predictor::new(flat_store());
        client.spawn(START);
        client.receive(ServerMessage::Leash {
            spawn: 1,
            from: 0,
            leash: ball(f64::from(crate::ground::REACH)),
        });
        // The server says nothing more: the client sends no more than the bound.
        let sent = (0..MAX_PENDING_INPUTS * 3)
            .filter(|_| client.walk(forward()).is_some())
            .count();
        assert_eq!(sent, MAX_PENDING_INPUTS);
        // Its word frees room, and the numbering goes on without a gap.
        let mut told = *client.character().unwrap().state();
        told.position[0] += 1.0;
        client.receive(ServerMessage::Body {
            spawn: 1,
            applied: 100,
            state: told,
        });
        let next = (0..10).find_map(|_| client.walk(forward()));
        assert!(matches!(
            next,
            Some(ClientMessage::Input { seq, .. }) if seq == MAX_PENDING_INPUTS as u64
        ));
    }

    #[test]
    fn what_the_server_said_about_an_earlier_spawn_is_ignored() {
        let mut client = Predictor::new(flat_store());
        client.spawn(START);
        client.spawn(START);
        // In flight for the first spawn when the second was made.
        client.receive(ServerMessage::Leash {
            spawn: 1,
            from: 0,
            leash: ball(f64::from(crate::ground::REACH)),
        });
        let mut elsewhere = *client.character().unwrap().state();
        elsewhere.position[0] += 9.0;
        client.receive(ServerMessage::Body {
            spawn: 1,
            applied: 0,
            state: elsewhere,
        });
        assert_eq!(client.body_desyncs(), 0);
        for _ in 0..200 {
            assert!(client.walk(forward()).is_none(), "no leash for this spawn");
        }
        client.receive(ServerMessage::Leash {
            spawn: 2,
            from: 0,
            leash: ball(f64::from(crate::ground::REACH)),
        });
        assert!((0..200).any(|_| client.walk(forward()).is_some()));
    }

    #[test]
    fn inputs_run_again_under_a_leash_the_ground_does_not_reach_yet_make_the_ground_first() {
        let mut client = Predictor::new(flat_store());
        client.spawn(START);
        client.receive(ServerMessage::Leash {
            spawn: 1,
            from: 0,
            leash: ball(24.0),
        });
        let mut sent = 0;
        while sent < 6 {
            sent += i32::from(client.walk(forward()).is_some());
        }
        // The server turned out to have moved the leash a long way, from the fourth input.
        let far = Leash {
            centre: [200.0, 4.0, 4.0],
            radius: 40.0,
        };
        client.receive(ServerMessage::Leash {
            spawn: 1,
            from: 3,
            leash: far,
        });
        assert!(!client.ground().covers(&far));
        let mut elsewhere = *client.character().unwrap().state();
        elsewhere.position[0] += 1.0;
        client.receive(ServerMessage::Body {
            spawn: 1,
            applied: 2,
            state: elsewhere,
        });
        assert_eq!(client.body_desyncs(), 1);
        assert_eq!(client.full_fills(), 1);
        // The ground made in ticks was timed; taking the time empties it.
        assert!(client.take_fill_time() > Duration::ZERO);
        assert_eq!(client.take_fill_time(), Duration::ZERO);
        assert!(client.ground().covers(&far));
    }
}
