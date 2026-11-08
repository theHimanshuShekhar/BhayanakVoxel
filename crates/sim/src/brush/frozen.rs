//! The Edit as it was before #156 made it brick by brick, kept as it was so that a test can hold
//! the brick by brick one to it: the plan tests every Voxel of every brick the Brush reaches and
//! lists the changes one by one, and the apply writes them with a lookup in the store each. Not to
//! be changed, and not used but by that test (`edit::equivalence`).

use super::*;

/// [`super::plan_from`] as it was.
pub fn plan_from(reads: &mut Reads, op: &EditOp) -> Vec<Change> {
    let Some(footprint) = Footprint::of(op) else {
        return Vec::new();
    };
    let (first, last) = footprint.bounds();
    // Flatten's plane is the tangent one: square to the exact radial direction.
    let normal = footprint.axes[2];
    let snapshot = (op.mode == EditMode::Smooth).then(|| {
        let beside = |box_: Vec3, step: i64| box_.map(|c| c + step);
        Snapshot::new(reads, beside(first, -1), beside(last, 1))
    });

    let mut changes = Vec::new();
    let (low, high) = bricks_of(first, last);
    for x in low[0]..=high[0] {
        for y in low[1]..=high[1] {
            for z in low[2]..=high[2] {
                let coord = BrickCoord([x, y, z]);
                let origin = coord.origin().0.map(i64::from);
                // A brick that the Brush cannot reach is not read: the box that holds it has
                // bricks at its corners that the shape does not touch.
                let from_centre = |a: usize, voxel: i64| voxel - footprint.centre[a];
                let low: Vec3 = std::array::from_fn(|a| from_centre(a, origin[a].max(first[a])));
                let high: Vec3 = std::array::from_fn(|a| {
                    from_centre(a, (origin[a] + i64::from(BRICK_SIZE) - 1).min(last[a]))
                });
                if footprint.misses(low, high) {
                    continue;
                }
                // A Smooth reads the snapshot, and the others read the brick: nothing to dig in
                // air, and nowhere to place in one that is full.
                let brick = snapshot.is_none().then(|| reads.brick(coord));
                match (op.mode, &brick) {
                    (EditMode::Dig, Some(brick)) if brick.is_air() => continue,
                    (EditMode::Place, Some(brick)) if brick.is_full() => continue,
                    _ => {}
                }
                let locals = locals_within(origin, first, last);
                for lz in locals[2].clone() {
                    for ly in locals[1].clone() {
                        for lx in locals[0].clone() {
                            let local = [lx, ly, lz];
                            let at: Vec3 = [
                                origin[0] + i64::from(lx),
                                origin[1] + i64::from(ly),
                                origin[2] + i64::from(lz),
                            ];
                            let d: Vec3 = std::array::from_fn(|a| at[a] - footprint.centre[a]);
                            if !footprint.contains(d) {
                                continue;
                            }
                            let now = match (&snapshot, &brick) {
                                (Some(snapshot), _) => snapshot.get(at),
                                (None, Some(brick)) => brick.get(local),
                                (None, None) => unreachable!("one of the two is made"),
                            };
                            let to = match op.mode {
                                EditMode::Dig => now.is_some().then_some(None),
                                EditMode::Place => now.is_none().then_some(Some(op.material)),
                                EditMode::Flatten if dot(normal, d) > 0 => {
                                    now.is_some().then_some(None)
                                }
                                EditMode::Flatten => now.is_none().then_some(Some(op.material)),
                                EditMode::Smooth => {
                                    let snapshot = snapshot.as_ref().expect("made for Smooth");
                                    let to = smoothed(snapshot, at, now);
                                    (to.is_some() != now.is_some()).then_some(to)
                                }
                            };
                            if let Some(to) = to {
                                changes.push(Change {
                                    voxel: coord.voxel(local),
                                    to,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    changes
}

/// What [`apply_changes_from`] overwrote, as `Undo` was.
pub struct Undone {
    pub bricks: Vec<(BrickCoord, Option<Brick>)>,
    pub lod: brick_store::LodRefresh,
}

impl Undone {
    /// `Undo::revert` as it was.
    pub fn revert(self, store: &mut BrickStore) -> brick_store::LodRefresh {
        let changed: Vec<_> = self.bricks.iter().map(|&(coord, _)| coord).collect();
        for (coord, before) in self.bricks {
            match before {
                Some(brick) => store.insert(coord, brick),
                None => {
                    store.remove(coord);
                }
            }
        }
        store.refresh_lod(changed)
    }
}

/// `edit::apply_changes_from` as it was, with no ground and no cache.
pub fn apply_changes(store: &mut BrickStore, changes: &[Change]) -> Undone {
    let mut bricks = Vec::new();
    for group in changes.chunk_by(|a, b| a.voxel.brick() == b.voxel.brick()) {
        let brick = group[0].voxel.brick();
        let before = store.brick(brick).cloned();
        if before.is_none() {
            let implied = implied_brick(store, None, None, brick);
            if !implied.is_air() {
                store.insert(brick, implied);
            }
        }
        for change in group {
            store.set(change.voxel, change.to);
        }
        bricks.push((brick, before));
    }
    let lod = store.refresh_lod(bricks.iter().map(|&(coord, _)| coord));
    Undone { bricks, lod }
}
