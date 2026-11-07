//! The synthetic brick cache: replays a [`Demand`] through a generation queue, an intake queue
//! and an eviction policy, and counts what a frame would lack.
//!
//! **The pipeline** mirrors the client's loader (#86): wanted bricks wait in a queue, nearest
//! first (the required ones before the rest of the shell, and that before prefetch); the workers
//! generate them at a rate; what is generated waits for the frame thread to take it in at a
//! separate rate, the *intake*, which is what limits the client today; and a brick is resident
//! once it is taken in. A brick that is no longer wanted is dropped from the queue (the loader
//! cancels it), but one that is generated or being taken in finishes.
//!
//! **A hole and a soft miss.** A required brick that is not resident is drawn from the nearest
//! resident brick that holds it (a coarser level: the Guard, or an older ring), which is a soft
//! miss; if none does, nothing is drawn there, and that is a hole.
//!
//! **Eviction** is when the resident bricks outweigh the budget by more than the overflow: the
//! bricks that nothing wants are evicted until the budget is met, those furthest out of their
//! band first and then the least recently wanted ([`evict_order`]). Bricks that are wanted
//! (required, in the shell or in prefetch), in the queues, and the Guard are never evicted, so
//! the budget is a target that wanted bricks can exceed (ADR 0004).
//!
//! The frame is a step of the path, and runs in this order: the demand changes, the counts, then
//! the workers and the intake for the step's time.

use std::collections::VecDeque;

use render::lod::brick_distance;

use crate::demand::{Demand, PREFETCH, SHELL, StepDemand, VISIBLE};
use crate::key::{Key, KeyMap, KeySet};
use crate::model::Model;

/// The rates, sizes and budget of the cache.
#[derive(Clone, Debug)]
pub struct Params {
    /// Generation threads: the loader runs cores - 2 of them.
    pub workers: usize,
    /// Bricks a worker generates a second, of any level, air and buried ones included.
    pub gen_per_core: f64,
    /// Bricks in a generation job: the loader's job makes all eight children of a split brick, so
    /// a drawn brick is generated with its siblings, and costs the same whether one or all eight
    /// are wanted. A job costs the children that are not resident already. 1 makes every brick
    /// a job of its own, as the tests of the queue do.
    pub job_size: usize,
    /// Drawn bricks a second that the scene takes in once they are generated; infinite if it
    /// takes them as they come (the incremental paths of #49).
    pub intake: f64,
    /// The loader's `MAX_AHEAD`: most jobs that are generated and not yet taken in. `None` for no
    /// bound.
    pub max_ahead: Option<usize>,
    /// Bytes a resident brick costs.
    pub brick_bytes: f64,
    /// The budget for the shell's bricks, in bytes (the Guard is not in it). `None` is the
    /// shell at its largest over the path, the 360° shell without prefetch (the research's
    /// "27-105 MB"): prefetch, what a path leaves behind and the siblings that jobs bring are
    /// the overflow that eviction works on.
    pub budget_bytes: Option<f64>,
    /// How far over the budget the resident bricks go before eviction, as a fraction of it.
    pub overflow: f64,
    /// Whether the shell of the first frame is resident already: the camera has stood there. A
    /// path's first frame is at rest (see `demand::build`), so that this is the shell and nothing
    /// of where the camera is going.
    pub prefill: bool,
}

impl Params {
    /// Total generation rate, in bricks a second.
    pub fn generation(&self) -> f64 {
        self.workers as f64 * self.gen_per_core
    }
}

/// A queued job: the brick that is split (the parent of the bricks wanted), or the brick itself
/// with a job size of 1.
struct Queued {
    job: Key,
    /// Smaller is sooner: the class (required, shell, prefetch), then the distance.
    priority: f64,
}

/// What one frame had.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    /// Bricks the frame needs: the required set, including the Guard's.
    pub required: u32,
    /// Bricks of the shell that are resident (the Guard is not counted).
    pub resident: usize,
    pub holes: u32,
    pub soft_misses: u32,
    pub resident_bytes: f64,
    /// Bricks waiting to be generated, and generated and waiting to be taken in.
    pub queue_depth: usize,
    /// Bricks that the frame needs and the one before did not, and how many of them were
    /// resident at the start of it.
    pub first_uses: u32,
    pub first_use_hits: u32,
    /// The fraction of the required set that is resident at its level.
    pub coverage: f64,
    /// Bricks wanted by the shell or prefetch this frame that were not resident and so were
    /// queued: the load that the frame put on the generator.
    pub queued_new: u32,
    /// Drawn bricks newly wanted this frame and not resident (`queued_new` is the bricks that
    /// their jobs generate, siblings and all).
    pub queued_drawn: u32,
    /// Bricks evicted so far.
    pub evictions: u64,
    /// Bricks in the shell, and in the shell and prefetch.
    pub shell_bricks: usize,
    pub target_bricks: usize,
}

/// The cache and its queues.
pub struct Cache<'m> {
    model: &'m Model,
    params: Params,
    /// Wanted bricks, with which of [`VISIBLE`], [`SHELL`] and [`PREFETCH`] want them.
    mask: KeyMap<u8>,
    resident: KeySet,
    /// For each brick above a wanted one, how many bricks that it is above are wanted. Those are
    /// the bricks drawn in place of a wanted brick that is not there yet, and the chain that a
    /// pyramid keeps over what it holds: not evicted (ADR 0004: never the current frame's set),
    /// since evicting one turns a soft miss into a hole, and makes more generation worse.
    under: KeyMap<u32>,
    /// Resident bricks that nothing wants, with the frame they were last wanted in.
    evictable: KeyMap<u32>,
    pending: Vec<Queued>,
    /// The wanted bricks of each queued job.
    kids: KeyMap<Vec<Key>>,
    /// Wanted bricks in a queued job.
    queued: KeySet,
    /// Jobs generated and waiting to be taken in, with the bricks in them (nearest last).
    finished: VecDeque<Vec<Key>>,
    finished_set: KeySet,
    /// Required bricks that are not resident.
    missing: KeySet,
    required: usize,
    shell: usize,
    target: usize,
    generate_credit: f64,
    intake_credit: f64,
    frame: u32,
    pos: [f64; 3],
    pub evictions: u64,
    /// For debugging: keep the required bricks that were missing at the last frame, if set.
    pub record_missing: bool,
    pub last_missing: Vec<Key>,
    /// The most bytes the demand wants at once, if the budget is that.
    budget_from_demand: f64,
}

/// What to evict first: bricks furthest out of their band (a brick of level 3 where the rule now
/// asks for level 0, or level 6), then the longest unwanted (a smaller frame number), then by key
/// for a stable order.
pub fn evict_order(model: &Model, camera: [f64; 3], candidates: &mut [(Key, u32)]) {
    let gap = |key: Key| {
        let d = brick_distance(camera, key.level(), key.coord());
        let owner = model.bands.level_at(d, model.top()) as i64;
        (key.level() as i64 - owner).abs()
    };
    candidates.sort_by_cached_key(|&(key, wanted)| (std::cmp::Reverse(gap(key)), wanted, key));
}

impl<'m> Cache<'m> {
    pub fn new(model: &'m Model, params: Params) -> Self {
        Self {
            model,
            params,
            mask: KeyMap::default(),
            resident: KeySet::default(),
            under: KeyMap::default(),
            evictable: KeyMap::default(),
            pending: Vec::new(),
            kids: KeyMap::default(),
            queued: KeySet::default(),
            finished: VecDeque::new(),
            finished_set: KeySet::default(),
            missing: KeySet::default(),
            required: 0,
            shell: 0,
            target: 0,
            generate_credit: 0.0,
            intake_credit: 0.0,
            frame: 0,
            pos: [0.0; 3],
            evictions: 0,
            record_missing: false,
            last_missing: Vec::new(),
            budget_from_demand: f64::INFINITY,
        }
    }

    /// Makes `key` resident without generating it, as if it had been long ago.
    pub fn seed(&mut self, key: Key) {
        if self.resident.insert(key) && !self.mask.contains_key(&key) {
            self.evictable.insert(key, 0);
        }
    }

    /// Which of VISIBLE, SHELL and PREFETCH want `key` now (0 for none).
    pub fn mask_of(&self, key: Key) -> u8 {
        self.mask.get(&key).copied().unwrap_or(0)
    }

    pub fn is_resident(&self, key: Key) -> bool {
        self.resident.contains(&key)
    }

    pub fn resident_count(&self) -> usize {
        self.resident.len()
    }

    /// Whether some resident brick holds `key`'s place at a coarser level: the Guard, or a
    /// ring.
    fn has_fallback(&self, key: Key) -> bool {
        let mut at = key;
        while at.level() < self.model.top() {
            at = at.parent();
            if self.model.in_guard(at.level()) || self.resident.contains(&at) {
                return true;
            }
        }
        false
    }

    fn apply(
        &mut self,
        change: crate::demand::Change,
        first: &mut (u32, u32),
        queued_new: &mut u32,
        queued_drawn: &mut u32,
    ) {
        let crate::demand::Change { key, old, new } = change;
        if new == 0 {
            self.mask.remove(&key);
        } else {
            self.mask.insert(key, new);
        }
        if (old == 0) != (new == 0) {
            let mut at = key;
            while at.level() < self.model.top() {
                at = at.parent();
                if old == 0 {
                    *self.under.entry(at).or_default() += 1;
                } else if let Some(n) = self.under.get_mut(&at) {
                    *n -= 1;
                    if *n == 0 {
                        self.under.remove(&at);
                    }
                }
            }
        }
        let count = |mask: u8, bits: u8| usize::from(mask & bits != 0);
        self.shell = self.shell + count(new, SHELL) - count(old, SHELL);
        self.target = self.target + count(new, SHELL | PREFETCH) - count(old, SHELL | PREFETCH);
        let resident = self.resident.contains(&key);
        if old & VISIBLE == 0 && new & VISIBLE != 0 {
            self.required += 1;
            first.0 += 1;
            if resident {
                first.1 += 1;
            } else {
                self.missing.insert(key);
            }
        } else if old & VISIBLE != 0 && new & VISIBLE == 0 {
            self.required -= 1;
            self.missing.remove(&key);
        }
        if old == 0 && new != 0 {
            if resident {
                self.evictable.remove(&key);
            } else if !self.finished_set.contains(&key) && self.queued.insert(key) {
                *queued_drawn += 1;
                let job = self.job_of(key);
                let kids = self.kids.entry(job).or_default();
                if kids.is_empty() {
                    let held = self.held_children(job);
                    *queued_new += (self.params.job_size - held) as u32;
                    self.pending.push(Queued { job, priority: 0.0 });
                }
                self.kids.get_mut(&job).expect("just made").push(key);
            }
        } else if old != 0 && new == 0 && resident {
            self.evictable.insert(key, self.frame);
        }
    }

    fn arrive(&mut self, key: Key) {
        if !self.resident.insert(key) {
            return;
        }
        if self.mask.contains_key(&key) {
            self.missing.remove(&key);
        } else {
            self.evictable.insert(key, self.frame);
        }
    }

    fn job_of(&self, key: Key) -> Key {
        if self.params.job_size == 1 {
            key
        } else {
            key.parent()
        }
    }

    /// How many of a job's bricks are resident already.
    fn held_children(&self, job: Key) -> usize {
        if self.params.job_size == 1 {
            return 0;
        }
        job.coord()
            .children()
            .iter()
            .filter(|&&c| self.resident.contains(&Key::new(job.level() - 1, c)))
            .count()
    }

    /// Generates what the workers can in `dt` seconds, in priority order.
    fn generate(&mut self, dt: f64) {
        // Dropped from the queue: no longer wanted, or resident already.
        let (mask, resident) = (&self.mask, &self.resident);
        let queued = &mut self.queued;
        self.kids.retain(|_, kids| {
            kids.retain(|k| {
                let keep = mask.contains_key(k) && !resident.contains(k);
                if !keep {
                    queued.remove(k);
                }
                keep
            });
            !kids.is_empty()
        });
        let kids = &self.kids;
        self.pending.retain(|q| kids.contains_key(&q.job));
        self.generate_credit += self.params.generation() * dt;
        if self.pending.is_empty() {
            self.generate_credit = 0.0;
            return;
        }
        let room = self
            .params
            .max_ahead
            .map_or(usize::MAX, |m| m.saturating_sub(self.finished.len()));
        let affordable = if self.generate_credit.is_finite() {
            self.generate_credit.ceil() as usize + 1
        } else {
            usize::MAX
        };
        let n = affordable.min(room).min(self.pending.len());
        if n == 0 {
            // Held back by `max_ahead`: idle workers bank nothing.
            self.generate_credit = 0.0;
            return;
        }
        let camera = self.pos;
        for q in &mut self.pending {
            // The soonest of its bricks: the class (required, shell, prefetch), then distance.
            q.priority = self.kids[&q.job]
                .iter()
                .map(|k| {
                    let class = match self.mask[k] {
                        m if m & VISIBLE != 0 => 0.0,
                        m if m & SHELL != 0 => 1.0,
                        _ => 2.0,
                    };
                    class * 1e15 + brick_distance(camera, k.level(), k.coord())
                })
                .fold(f64::INFINITY, f64::min);
        }
        let by = |a: &Queued, b: &Queued| a.priority.total_cmp(&b.priority).then(a.job.cmp(&b.job));
        if n < self.pending.len() {
            self.pending.select_nth_unstable_by(n, by);
        }
        self.pending[..n].sort_by(by);
        let mut taken = 0;
        while taken < n && self.generate_credit > 0.0 {
            let job = self.pending[taken].job;
            self.generate_credit -= (self.params.job_size - self.held_children(job)) as f64;
            let mut keys = self.kids.remove(&job).expect("a pending job has bricks");
            for k in &keys {
                self.queued.remove(k);
            }
            if self.params.job_size > 1 {
                // The siblings that are surface come with it, wanted or not.
                for c in job.coord().children() {
                    let key = Key::new(job.level() - 1, c);
                    if !self.resident.contains(&key)
                        && !keys.contains(&key)
                        && self.model.is_surface(key.level(), c.0)
                    {
                        keys.push(key);
                    }
                }
            }
            // Taken in nearest first: the nearest is last, and popped first.
            keys.sort_by(|a, b| {
                let d = |k: &Key| brick_distance(camera, k.level(), k.coord());
                d(b).total_cmp(&d(a))
            });
            self.finished_set.extend(keys.iter().copied());
            self.finished.push_back(keys);
            taken += 1;
        }
        self.pending.drain(..taken);
        if self.generate_credit > 0.0 {
            // Out of work, or held back: idle workers bank nothing.
            self.generate_credit = 0.0;
        }
    }

    /// The scene takes in what is generated, up to the intake rate.
    fn take_in(&mut self, dt: f64) {
        self.intake_credit += self.params.intake * dt;
        while self.intake_credit >= 1.0 {
            let Some(front) = self.finished.front_mut() else {
                break;
            };
            let key = front.pop().expect("a finished job has bricks");
            if front.is_empty() {
                self.finished.pop_front();
            }
            self.finished_set.remove(&key);
            self.arrive(key);
            self.intake_credit -= 1.0;
        }
        if self.finished.is_empty() {
            self.intake_credit = 0.0;
        }
    }

    fn budget(&self) -> f64 {
        self.params.budget_bytes.unwrap_or(self.budget_from_demand)
    }

    fn bytes(&self) -> f64 {
        self.resident.len() as f64 * self.params.brick_bytes
    }

    fn evict(&mut self) {
        if self.bytes() <= self.budget() * (1.0 + self.params.overflow) {
            return;
        }
        let mut candidates: Vec<(Key, u32)> = self
            .evictable
            .iter()
            .filter(|(k, _)| !self.under.contains_key(k))
            .map(|(&k, &f)| (k, f))
            .collect();
        evict_order(self.model, self.pos, &mut candidates);
        for (key, _) in candidates {
            if self.bytes() <= self.budget() {
                break;
            }
            self.resident.remove(&key);
            self.evictable.remove(&key);
            self.evictions += 1;
        }
    }

    /// Runs one frame of `step`, `dt` seconds long.
    pub fn step(&mut self, step: &StepDemand, dt: f64) -> Frame {
        self.pos = step.pos;
        let mut first = (0, 0);
        let (mut queued_new, mut queued_drawn) = (0, 0);
        for &change in &step.changes {
            self.apply(change, &mut first, &mut queued_new, &mut queued_drawn);
        }
        if self.params.prefill && self.frame == 0 {
            let standing: Vec<Key> = self
                .mask
                .iter()
                .filter(|&(_, &m)| m & (VISIBLE | SHELL) != 0)
                .map(|(&k, _)| k)
                .collect();
            for key in standing {
                self.resident.insert(key);
                self.missing.remove(&key);
                self.queued.remove(&key);
            }
            first = (0, 0);
            queued_new = 0;
            queued_drawn = 0;
        }
        if self.record_missing {
            self.last_missing = self.missing.iter().copied().collect();
        }
        let (mut holes, mut soft) = (0, 0);
        for &key in &self.missing {
            if self.has_fallback(key) {
                soft += 1;
            } else {
                holes += 1;
            }
        }
        let required = self.required as u32 + step.guard_visible;
        let frame = Frame {
            required,
            resident: self.resident.len(),
            holes,
            soft_misses: soft,
            resident_bytes: self.bytes(),
            queue_depth: self.queued.len() + self.finished_set.len(),
            first_uses: first.0,
            first_use_hits: first.1,
            coverage: if required == 0 {
                1.0
            } else {
                1.0 - f64::from(holes + soft) / f64::from(required)
            },
            queued_new,
            queued_drawn,
            evictions: self.evictions,
            shell_bricks: self.shell,
            target_bricks: self.target,
        };
        self.generate(dt);
        self.take_in(dt);
        self.evict();
        self.frame += 1;
        frame
    }
}

/// What a whole path had.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub frames: usize,
    pub hole_frames: usize,
    pub soft_miss_frames: usize,
    /// Bricks missing, summed over frames.
    pub hole_bricks: u64,
    pub soft_miss_bricks: u64,
    pub peak_soft_misses: u32,
    pub peak_resident_bytes: f64,
    /// The most bricks that the shell held, and the shell with prefetch.
    pub peak_shell_bricks: usize,
    pub peak_target_bricks: usize,
    pub peak_required: u32,
    pub peak_queue: usize,
    pub first_uses: u64,
    pub first_use_hits: u64,
    pub min_coverage: f64,
    /// Most bricks newly queued in one second, and in 0.16 s: the load on the generator.
    pub peak_demand_per_s: f64,
    pub peak_demand_burst: f64,
    pub evictions: u64,
    /// Soft misses in the onset frame (see `Demand::onset`): counted apart, and not in
    /// `soft_miss_seconds`, since no prefetch can avoid them.
    pub onset_soft_misses: u32,
    /// Over the path after its first frame: bricks that jobs generated for the bricks newly
    /// wanted, and those bricks.
    pub generated_total: u64,
    pub drawn_total: u64,
    /// Seconds from the first frame with a soft miss to the last, 0 if none.
    pub soft_miss_seconds: f64,
}

impl Summary {
    pub fn hit_rate(&self) -> f64 {
        if self.first_uses == 0 {
            1.0
        } else {
            self.first_use_hits as f64 / self.first_uses as f64
        }
    }

    /// Bricks generated for each drawn brick that was wanted: the siblings of a job (air and
    /// buried ones among them) are the cost of a brick that is drawn.
    pub fn generated_per_drawn(&self) -> f64 {
        if self.drawn_total == 0 {
            0.0
        } else {
            self.generated_total as f64 / self.drawn_total as f64
        }
    }

    /// Zero holes, and soft misses in at most `allowed` seconds (an unavoidable few after a
    /// teleport).
    pub fn is_clean(&self, allowed_soft_seconds: f64) -> bool {
        self.hole_frames == 0 && self.soft_miss_seconds <= allowed_soft_seconds + 1e-9
    }
}

/// Seconds of the burst window of [`Summary::peak_demand_burst`].
const BURST: f64 = 0.16;

/// Replays `demand` through a cache, returning each frame.
pub fn run_frames(model: &Model, demand: &Demand, params: &Params) -> Vec<Frame> {
    let mut cache = Cache::new(model, params.clone());
    cache.budget_from_demand = demand.peak_shell() as f64 * params.brick_bytes;
    demand
        .steps
        .iter()
        .map(|step| cache.step(step, demand.dt))
        .collect()
}

/// Replays `demand` and summarises it.
pub fn run(model: &Model, demand: &Demand, params: &Params) -> Summary {
    summarise(&run_frames(model, demand, params), demand.dt, demand.onset)
}

pub fn summarise(frames: &[Frame], dt: f64, onset: usize) -> Summary {
    let mut s = Summary {
        frames: frames.len(),
        min_coverage: 1.0,
        ..Summary::default()
    };
    let mut soft_frames = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        s.hole_frames += usize::from(f.holes > 0);
        if f.soft_misses > 0 {
            s.soft_miss_frames += 1;
            soft_frames.push(i);
        }
        s.hole_bricks += u64::from(f.holes);
        s.soft_miss_bricks += u64::from(f.soft_misses);
        s.peak_soft_misses = s.peak_soft_misses.max(f.soft_misses);
        s.peak_resident_bytes = s.peak_resident_bytes.max(f.resident_bytes);
        s.peak_shell_bricks = s.peak_shell_bricks.max(f.shell_bricks);
        s.peak_target_bricks = s.peak_target_bricks.max(f.target_bricks);
        s.peak_required = s.peak_required.max(f.required);
        s.peak_queue = s.peak_queue.max(f.queue_depth);
        s.first_uses += u64::from(f.first_uses);
        s.first_use_hits += u64::from(f.first_use_hits);
        s.min_coverage = s.min_coverage.min(f.coverage);
        s.evictions = f.evictions;
    }
    s.generated_total = frames.iter().skip(1).map(|f| u64::from(f.queued_new)).sum();
    s.drawn_total = frames
        .iter()
        .skip(1)
        .map(|f| u64::from(f.queued_drawn))
        .sum();
    s.onset_soft_misses = frames.get(onset).map_or(0, |f| f.soft_misses);
    soft_frames.retain(|&i| i != onset);
    if frames.get(onset).is_some_and(|f| f.soft_misses > 0) {
        s.soft_miss_frames -= 1;
    }
    // Frame 0 is the shell appearing, not load: the demand after it is.
    let queued: Vec<f64> = frames
        .iter()
        .skip(1)
        .map(|f| f64::from(f.queued_new))
        .collect();
    let window = |seconds: f64| {
        let n = ((seconds / dt).round() as usize).max(1);
        let best = queued
            .windows(n.min(queued.len()).max(1))
            .map(|w| w.iter().sum::<f64>())
            .fold(0.0, f64::max);
        best / (n as f64 * dt)
    };
    if !queued.is_empty() {
        s.peak_demand_per_s = window(1.0);
        s.peak_demand_burst = window(BURST);
    }
    if let (Some(&a), Some(&b)) = (soft_frames.first(), soft_frames.last()) {
        s.soft_miss_seconds = (b - a + 1) as f64 * dt;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demand::Change;
    use brick_store::BrickCoord;
    use render::lod::Bands;

    fn model() -> Model {
        let mut model = Model::real();
        model.planet_radius = 800.0;
        model.crust = 0.0;
        model.bands = Bands::with_d0(20.0);
        model
    }

    fn key(level: usize, c: [i32; 3]) -> Key {
        Key::new(level, BrickCoord(c))
    }

    fn params() -> Params {
        Params {
            workers: 1,
            gen_per_core: 0.0,
            job_size: 1,
            intake: f64::INFINITY,
            max_ahead: None,
            brick_bytes: 100.0,
            budget_bytes: Some(1e12),
            overflow: 0.0,
            prefill: false,
        }
    }

    fn wanting(keys: &[(Key, u8)]) -> StepDemand {
        StepDemand {
            pos: [0.0, 0.0, 802.0],
            changes: keys
                .iter()
                .map(|&(key, new)| Change { key, old: 0, new })
                .collect(),
            guard_visible: 0,
        }
    }

    #[test]
    fn a_missing_brick_with_nothing_resident_above_it_is_a_hole() {
        let mut model = model();
        model.guard_first = None;
        let mut cache = Cache::new(&model, params());
        let frame = cache.step(&wanting(&[(key(0, [4, 4, 99]), VISIBLE | SHELL)]), 0.1);
        assert_eq!((frame.holes, frame.soft_misses), (1, 0));
        assert_eq!(frame.required, 1);
        assert_eq!(frame.coverage, 0.0);
    }

    #[test]
    fn a_missing_brick_under_a_resident_one_is_a_soft_miss() {
        let mut model = model();
        model.guard_first = None;
        let mut cache = Cache::new(&model, params());
        // Its grandparent, two levels up, is resident.
        let brick = key(0, [4, 4, 99]);
        cache.seed(brick.parent().parent());
        let frame = cache.step(&wanting(&[(brick, VISIBLE | SHELL)]), 0.1);
        assert_eq!((frame.holes, frame.soft_misses), (0, 1));
    }

    #[test]
    fn the_guard_is_a_fallback_for_every_brick_below_it() {
        let mut model = model();
        model.guard_first = Some(2);
        let mut cache = Cache::new(&model, params());
        let frame = cache.step(&wanting(&[(key(0, [4, 4, 99]), VISIBLE | SHELL)]), 0.1);
        assert_eq!((frame.holes, frame.soft_misses), (0, 1));
    }

    #[test]
    fn a_resident_brick_is_a_hit_and_a_miss_is_not() {
        let mut model = model();
        model.guard_first = None;
        let mut cache = Cache::new(&model, params());
        let (a, b) = (key(0, [4, 4, 99]), key(0, [5, 4, 99]));
        cache.seed(a);
        let frame = cache.step(&wanting(&[(a, VISIBLE | SHELL), (b, VISIBLE | SHELL)]), 0.1);
        assert_eq!((frame.first_uses, frame.first_use_hits), (2, 1));
        assert_eq!(frame.required, 2);
        assert_eq!(frame.coverage, 0.5);
    }

    #[test]
    fn a_brick_arrives_at_the_generation_rate_and_the_miss_ends() {
        let mut model = model();
        model.guard_first = None;
        let mut p = params();
        // 10 bricks a second, 0.1 s a frame: one brick a frame.
        p.gen_per_core = 10.0;
        let mut cache = Cache::new(&model, p);
        let keys: Vec<_> = (0..3)
            .map(|i| (key(0, [i, 4, 99]), VISIBLE | SHELL))
            .collect();
        let first = cache.step(&wanting(&keys), 0.1);
        assert_eq!(first.holes, 3);
        let idle = StepDemand {
            pos: [0.0, 0.0, 802.0],
            ..StepDemand::default()
        };
        let holes: Vec<u32> = (0..3).map(|_| cache.step(&idle, 0.1).holes).collect();
        assert_eq!(holes, [2, 1, 0]);
    }

    #[test]
    fn the_nearest_required_brick_is_generated_first() {
        let mut model = model();
        model.guard_first = None;
        let mut p = params();
        p.gen_per_core = 10.0;
        let mut cache = Cache::new(&model, p);
        let near = key(0, [0, 0, 100]);
        let far = key(0, [20, 0, 90]);
        let shell_near = key(0, [1, 0, 100]);
        cache.step(
            &wanting(&[
                (far, VISIBLE | SHELL),
                (shell_near, SHELL),
                (near, VISIBLE | SHELL),
            ]),
            0.1,
        );
        // One brick a frame: the visible near one, the visible far one, then the shell's.
        assert!(cache.is_resident(near) && !cache.is_resident(far));
        let idle = StepDemand {
            pos: [0.0, 0.0, 802.0],
            ..StepDemand::default()
        };
        cache.step(&idle, 0.1);
        assert!(cache.is_resident(far) && !cache.is_resident(shell_near));
        cache.step(&idle, 0.1);
        assert!(cache.is_resident(shell_near));
    }

    #[test]
    fn intake_limits_what_generation_has_made() {
        let mut model = model();
        model.guard_first = None;
        let mut p = params();
        p.gen_per_core = 1e9;
        p.intake = 10.0;
        let mut cache = Cache::new(&model, p);
        let keys: Vec<_> = (0..5)
            .map(|i| (key(0, [i, 4, 99]), VISIBLE | SHELL))
            .collect();
        cache.step(&wanting(&keys), 0.1);
        let idle = StepDemand::default();
        // All five are generated at once, and taken in one a frame.
        assert_eq!(cache.step(&idle, 0.1).queue_depth, 4);
        assert_eq!(cache.step(&idle, 0.1).queue_depth, 3);
    }

    #[test]
    fn max_ahead_holds_the_workers_back() {
        let mut model = model();
        model.guard_first = None;
        let mut p = params();
        p.gen_per_core = 1e9;
        p.intake = 0.0;
        p.max_ahead = Some(2);
        let mut cache = Cache::new(&model, p);
        let keys: Vec<_> = (0..5)
            .map(|i| (key(0, [i, 4, 99]), VISIBLE | SHELL))
            .collect();
        cache.step(&wanting(&keys), 0.1);
        let frame = cache.step(&StepDemand::default(), 0.1);
        // Two finished and waiting, three still queued.
        assert_eq!(frame.queue_depth, 5);
        assert_eq!(cache.finished.len(), 2);
    }

    #[test]
    fn a_brick_that_is_no_longer_wanted_is_dropped_from_the_queue() {
        let mut model = model();
        model.guard_first = None;
        let mut p = params();
        p.gen_per_core = 10.0;
        let mut cache = Cache::new(&model, p);
        let (a, b) = (key(0, [0, 0, 100]), key(0, [1, 0, 100]));
        cache.step(&wanting(&[(a, SHELL), (b, SHELL)]), 0.1);
        // One brick a frame, nearest first: `a` is generated at once, and `b` is dropped before
        // its turn.
        let drop_b = StepDemand {
            pos: [0.0, 0.0, 802.0],
            changes: vec![Change {
                key: b,
                old: SHELL,
                new: 0,
            }],
            guard_visible: 0,
        };
        cache.step(&drop_b, 0.1);
        let idle = StepDemand::default();
        for _ in 0..3 {
            cache.step(&idle, 0.1);
        }
        assert!(cache.is_resident(a));
        assert!(!cache.is_resident(b), "never generated");
    }

    #[test]
    fn eviction_takes_the_furthest_out_of_band_first_and_then_the_least_recently_wanted() {
        let model = model();
        let camera = [0.0, 0.0, 802.0];
        // d0 is 20, so level 0 owns up to 40 Voxels. A brick of level 0 far away (the surface
        // at x = 400 is 400 Voxels off) is far out of its band: the rule asks for level 4.
        let far = key(0, [50, 0, 86]);
        let near = key(0, [0, 0, 100]);
        // A brick of level 4 at the same distance is in band.
        let in_band = key(4, [3, 0, 5]);
        let mut candidates = vec![(near, 1), (in_band, 0), (key(0, [1, 0, 100]), 7), (far, 9)];
        evict_order(&model, camera, &mut candidates);
        assert_eq!(candidates[0].0, far, "most out of band");
        // The two near ones are in band alike: the older stamp goes first.
        let near_first = candidates.iter().position(|c| c.0 == near).unwrap();
        let near_second = candidates
            .iter()
            .position(|c| c.0 == key(0, [1, 0, 100]))
            .unwrap();
        assert!(near_first < near_second);
    }

    #[test]
    fn over_budget_evicts_unwanted_bricks_down_to_the_budget_and_never_wanted_ones() {
        let mut model = model();
        model.guard_first = None;
        let mut p = params();
        p.budget_bytes = Some(500.0);
        p.overflow = 0.2;
        let mut cache = Cache::new(&model, p);
        let wanted = key(0, [0, 0, 100]);
        cache.step(&wanting(&[(wanted, SHELL)]), 0.1);
        cache.seed(wanted);
        // Five unwanted bricks: 600 bytes with the wanted one, which is not over 600.
        for i in 1..6 {
            cache.seed(key(0, [i, 0, 100]));
        }
        cache.step(&StepDemand::default(), 0.1);
        assert_eq!(cache.resident_count(), 6);
        // One more: 700 bytes, over; evicts down to 500 and keeps the wanted one.
        cache.seed(key(0, [6, 0, 100]));
        cache.step(&StepDemand::default(), 0.1);
        assert_eq!(cache.resident_count(), 5);
        assert!(cache.is_resident(wanted));
        assert_eq!(cache.evictions, 2);
    }

    #[test]
    fn prefill_makes_the_first_shell_resident_and_not_a_load() {
        let mut model = model();
        model.guard_first = None;
        let mut p = params();
        p.prefill = true;
        let mut cache = Cache::new(&model, p);
        let a = key(0, [4, 4, 99]);
        let frame = cache.step(&wanting(&[(a, VISIBLE | SHELL)]), 0.1);
        assert_eq!(
            (frame.holes, frame.soft_misses, frame.queued_new),
            (0, 0, 0)
        );
        assert!(cache.is_resident(a));
    }

    #[test]
    fn a_job_of_eight_brings_the_siblings_that_are_surface_and_costs_the_missing_ones() {
        let mut model = model();
        model.guard_first = None;
        let mut p = params();
        p.job_size = 8;
        p.gen_per_core = 1e9;
        let mut cache = Cache::new(&model, p);
        let (wanted, sibling) = (key(0, [4, 4, 99]), key(0, [5, 4, 99]));
        assert_eq!(wanted.parent(), sibling.parent());
        let first = cache.step(&wanting(&[(wanted, VISIBLE | SHELL)]), 0.1);
        assert_eq!(first.queued_new, 8, "generated bricks, not drawn ones");
        assert!(cache.is_resident(wanted) && cache.is_resident(sibling));
        // A second job over the same parent costs only the children that are not resident.
        let held = cache.held_children(wanted.parent());
        assert!((2..=8).contains(&held), "{held}");
        assert_eq!(
            wanted
                .parent()
                .coord()
                .children()
                .iter()
                .filter(|&&c| cache.is_resident(Key::new(0, c)))
                .count(),
            held
        );
    }

    #[test]
    fn the_soft_misses_of_the_onset_frame_are_counted_apart() {
        let soft = |n| Frame {
            soft_misses: n,
            ..Frame::default()
        };
        let frames = [soft(0), soft(0), soft(7), soft(0), soft(2)];
        let s = summarise(&frames, 0.1, 2);
        assert_eq!((s.onset_soft_misses, s.soft_miss_frames), (7, 1));
        assert!((s.soft_miss_seconds - 0.1).abs() < 1e-9);
        assert!(!s.is_clean(0.0));
        let only_onset = summarise(&frames[..4], 0.1, 2);
        assert_eq!(only_onset.soft_miss_frames, 0);
        assert!(only_onset.is_clean(0.0));
    }
}
