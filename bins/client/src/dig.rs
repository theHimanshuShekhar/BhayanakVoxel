//! Hardness-timed digging: how long the player holds the button before a Dig is made.
//!
//! The client times the hold and then sends one ordinary `EditOp`, so the network message and the
//! simulation do not know of it. How long it takes is [`sim::dig_seconds`], which the server also
//! uses to hold a player to a Dig no sooner than that after the last.

use net::EditOp;

/// The progress of the Dig being held.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Dig {
    /// The Edit being dug towards and the seconds held on it. Aiming at another Edit starts over.
    target: Option<(EditOp, f64)>,
}

impl Dig {
    /// Holds for `dt` seconds on `aim`, the Edit now aimed at and the seconds it takes. Returns
    /// whether the Dig is done, which starts the next one. Looking away to another Edit, or at
    /// nothing, loses what was held.
    pub fn hold(&mut self, dt: f64, aim: Option<(EditOp, f64)>) -> bool {
        let Some((op, seconds)) = aim else {
            self.target = None;
            return false;
        };
        let held = match self.target {
            Some((target, held)) if target == op => held + dt,
            _ => dt,
        };
        if held >= seconds {
            self.target = None;
            true
        } else {
            self.target = Some((op, held));
            false
        }
    }

    /// Lets go of the button.
    pub fn release(&mut self) {
        self.target = None;
    }

    /// How much of the Dig is done, 0 to 1, if one is being held; `seconds` is what it takes.
    pub fn progress(&self, seconds: f64) -> Option<f32> {
        self.target
            .map(|(_, held)| (held / seconds).clamp(0.0, 1.0) as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brick_store::VoxelCoord;
    use materials::MaterialTable;
    use sim::{MIN_DIG_SECONDS, dig_seconds};

    fn hardness(name: &str) -> f32 {
        MaterialTable::built_in().by_name(name).unwrap().hardness
    }

    fn at(x: i32) -> EditOp {
        EditOp::dig_sphere(VoxelCoord([x, 0, 0]), 10)
    }

    #[test]
    fn the_time_scales_with_the_hardness() {
        let (sand, soil, rock, obsidian) = (
            dig_seconds(hardness("sand")),
            dig_seconds(hardness("soil")),
            dig_seconds(hardness("rock")),
            dig_seconds(hardness("obsidian")),
        );
        assert!(sand < soil && soil < rock && rock < obsidian);
        // In proportion: twice as hard takes twice as long.
        assert!((dig_seconds(4.0) - 2.0 * dig_seconds(2.0)).abs() < 1e-12);
        assert!((rock / soil - 3.0).abs() < 1e-9);
        assert!((obsidian - 1.2).abs() < 1e-9);
    }

    #[test]
    fn a_material_with_no_hardness_still_takes_the_least_time() {
        assert_eq!(dig_seconds(hardness("water")), MIN_DIG_SECONDS);
        assert_eq!(dig_seconds(0.0), MIN_DIG_SECONDS);
    }

    #[test]
    fn the_dig_is_made_once_it_has_been_held_for_the_time() {
        let mut dig = Dig::default();
        let aim = Some((at(0), 1.0));
        assert!(!dig.hold(0.4, aim));
        assert_eq!(dig.progress(1.0), Some(0.4));
        assert!(!dig.hold(0.4, aim));
        assert!(dig.hold(0.4, aim), "1.2 s of 1 s");
        // The next one starts from nothing.
        assert_eq!(dig.progress(1.0), None);
        assert!(!dig.hold(0.4, aim));
    }

    #[test]
    fn harder_material_needs_more_frames_of_the_same_hold() {
        let frames = |hardness: f32| {
            let mut dig = Dig::default();
            (1..)
                .find(|_| dig.hold(1.0 / 60.0, Some((at(0), dig_seconds(hardness)))))
                .unwrap()
        };
        assert!(frames(hardness("sand")) < frames(hardness("rock")));
        assert!(frames(hardness("rock")) < frames(hardness("obsidian")));
    }

    #[test]
    fn aiming_elsewhere_or_at_nothing_or_letting_go_starts_over() {
        let mut dig = Dig::default();
        dig.hold(0.5, Some((at(0), 1.0)));
        assert!(!dig.hold(0.6, Some((at(1), 1.0))), "another Edit");
        assert_eq!(dig.progress(1.0), Some(0.6));
        assert!(!dig.hold(0.1, None));
        assert_eq!(dig.progress(1.0), None);
        dig.hold(0.9, Some((at(1), 1.0)));
        dig.release();
        assert!(!dig.hold(0.2, Some((at(1), 1.0))));
    }

    #[test]
    fn the_least_dig_is_made_once_the_least_time_has_been_held() {
        let mut dig = Dig::default();
        assert!(!dig.hold(MIN_DIG_SECONDS / 2.0, Some((at(0), MIN_DIG_SECONDS))));
        assert!(dig.hold(MIN_DIG_SECONDS / 2.0, Some((at(0), MIN_DIG_SECONDS))));
    }

    /// A player who holds the button on one Material after another, a frame at a time, as the
    /// client does, is never turned down by the server: not on a step from a hard Material to a
    /// soft one, nor when the first Dig is made right after the hold of its own time.
    #[test]
    fn the_server_never_turns_down_the_digs_of_a_player_who_holds_for_the_time() {
        use brick_store::{Brick, BrickCoord, BrickStore, Material, VoxelCoord};
        use net::{ClientMessage, ServerMessage, loopback};
        use sim::{Server, TICK_RATE_HZ};

        let table = MaterialTable::built_in();
        let id = |name: &str| Material(table.by_name(name).unwrap().id);
        // Obsidian, sand and rock side by side, with the character standing among them.
        let mut store = BrickStore::new();
        for (x, name) in [(0, "obsidian"), (1, "sand"), (-1, "rock")] {
            store.insert(BrickCoord([x, 0, 0]), Brick::solid(id(name)));
        }
        let (client, end) = loopback();
        let mut server = Server::new(store, [0.0; 3]);
        client
            .send(ClientMessage::Spawn {
                spawn: 1,
                position: [0.0; 3],
            })
            .unwrap();

        let mut dig = Dig::default();
        let mut made = 0;
        let mut seq = 0;
        // Each Dig is of a Voxel of its own Material, held for in frames of 1/60 s with a
        // server tick to each frame, and a rest between some.
        let script = [
            ("obsidian", [3, 3, 3], 0),
            ("sand", [9, 3, 3], 0),
            ("rock", [-3, 3, 3], 0),
            ("sand", [10, 3, 3], 0),
            ("obsidian", [4, 3, 3], 30),
            ("sand", [11, 3, 3], 0),
            ("sand", [12, 3, 3], 0),
        ];
        for (name, centre, rest) in script {
            for _ in 0..rest {
                server.tick(std::slice::from_ref(&end));
            }
            let seconds = dig_seconds(sim::target_hardness(Some(id(name))));
            let op = EditOp::dig_sphere(VoxelCoord(centre), 0);
            // Frames until the hold is done, and then the Edit goes to the server in that frame.
            let frames = (0..1000)
                .find(|_| {
                    let done = dig.hold(1.0 / f64::from(TICK_RATE_HZ), Some((op, seconds)));
                    if done {
                        client
                            .send(ClientMessage::Edit {
                                client_seq: seq,
                                op,
                            })
                            .unwrap();
                        seq += 1;
                    }
                    server.tick(std::slice::from_ref(&end));
                    done
                })
                .expect("the hold is done");
            assert!(frames > 0, "{name}");
            made += 1;
        }
        let told: Vec<_> = std::iter::from_fn(|| client.try_recv().ok())
            .filter(|m| {
                matches!(
                    m,
                    ServerMessage::Applied { .. } | ServerMessage::Rejected { .. }
                )
            })
            .collect();
        assert_eq!(told.len(), made);
        assert!(
            told.iter()
                .all(|m| matches!(m, ServerMessage::Applied { .. })),
            "{told:?}"
        );
    }
}
