//! Message protocol between client and server (ADR 0003).
//!
//! The messages are plain data, with no reference to how they travel. In M1 they cross an
//! in-memory [`loopback`]; in M2 renet carries the same types on its reliable-ordered channel,
//! so everything here must keep arriving in the order it was sent.

use std::sync::mpsc::{self, Receiver, Sender};

use brick_store::{Material, VoxelCoord};

pub use std::sync::mpsc::TryRecvError;

/// What an Edit does to the Voxels its Brush covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditMode {
    /// Removes the solid Voxels.
    Dig,
    /// Fills the air Voxels with the op's Material.
    Place,
    /// Rounds the surface off: a Voxel becomes solid if most of the 3 x 3 x 3 Voxels around it
    /// are, and air if not.
    Smooth,
    /// Levels the Brush against the tangent plane at its centre: the solid Voxels above the
    /// plane are removed, and the air Voxels on and below it are filled with the op's Material.
    Flatten,
}

/// The shape of a Brush. Its axis, for those that have one, is the local up (see [`Alignment`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Every Voxel within `radius` Voxels of the centre.
    Sphere,
    /// The box that reaches `radius` Voxels from the centre along each of its three axes.
    Cube,
    /// The disc of `radius` Voxels about the axis through the centre, and as far along the axis
    /// as it is across.
    Cylinder,
}

/// How a Brush's axes are laid out against the Voxel grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alignment {
    /// Up is the local up at the centre (away from the Planet's centre), snapped to the nearest
    /// 45 degrees: what a Brush on the side of the Planet stands on.
    Local,
    /// Up is the grid's +y, and the axes are the grid's.
    World,
}

/// The shape and size an Edit applies to the Planet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Brush {
    pub shape: Shape,
    /// In Voxels: 0 is the centre Voxel alone, and 20 (2 m) is the most a player may use.
    pub radius: u16,
}

/// One Edit, small enough to broadcast Planet-wide: 24 bytes. The Voxels it changes follow from
/// it and the Planet's bricks by integer maths alone (see `sim::plan`), so the op carries no
/// Voxel list: the up axis follows from `alignment` and `centre`, and `rotation` turns the Brush
/// about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditOp {
    pub mode: EditMode,
    pub brush: Brush,
    /// The Voxel the Brush is centred on.
    pub centre: VoxelCoord,
    /// What `Place` and `Flatten` fill with. The other modes ignore it.
    pub material: Material,
    pub alignment: Alignment,
    /// How far the Brush is turned about its up axis, in steps of 45 degrees: 0 to 7. A cube and
    /// a cylinder are the same shape after a quarter turn, so for them only odd and even differ.
    pub rotation: u8,
}

impl EditOp {
    /// Digs a sphere of `radius` Voxels around `centre`.
    pub fn dig_sphere(centre: VoxelCoord, radius: u16) -> Self {
        Self {
            mode: EditMode::Dig,
            brush: Brush {
                shape: Shape::Sphere,
                radius,
            },
            centre,
            material: Material(0),
            alignment: Alignment::Local,
            rotation: 0,
        }
    }
}

/// Why the server turned an Edit down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    /// The Brush is bigger than a player may use.
    BrushTooLarge,
    /// The Brush reaches beyond the Voxel coordinates the Planet has.
    OutOfRange,
    /// The centre is further from the player's eyes than a player may reach, or the player has no
    /// character to reach from.
    OutOfReach,
    /// A `Place` or `Flatten` of a Material that the Material table does not define.
    UnknownMaterial,
    /// The player asked for more Edits in the last second than they may, or the server has done
    /// as much Edit work as it will this tick.
    RateLimited,
    /// The Edit would change more Voxels than the player has left to change.
    VolumeLimited,
    /// A rotation that is not 0 to 7.
    BadRotation,
}

/// What a player asks of their character for one tick (the sim's fixed step): plain data, so
/// that the client and the server feed the controller the same bits. Vectors are in the Planet
/// frame.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct MoveInput {
    /// Towards `heading` (1) or away from it (-1). Anything else is clamped to these.
    pub forward: i8,
    /// To the right of `heading` (1) or the left (-1). Anything else is clamped to these.
    pub right: i8,
    /// The way "forward" points, as a vector the controller flattens onto the ground (so a
    /// camera's view direction will do) and normalises. Zero, or straight up, moves nowhere.
    pub heading: [f64; 3],
    pub jump: bool,
    pub run: bool,
    pub crouch: bool,
}

/// Where a character is and how it is moving: everything that evolves from one tick to the next.
/// The server's copy is the authority; the client predicts the same numbers and takes the
/// server's if they ever differ.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyState {
    /// The middle of the capsule's base, in Voxels.
    pub position: [f64; 3],
    /// Voxels per second.
    pub velocity: [f64; 3],
    /// Standing on something, so that it can jump and walk up steps.
    pub grounded: bool,
    pub crouching: bool,
}

impl BodyState {
    /// Whether every bit is the same, which `==` is not for `-0.0` against `0.0`.
    pub fn identical(&self, other: &Self) -> bool {
        let bits = |s: &Self| [s.position, s.velocity].map(|v| v.map(f64::to_bits));
        bits(self) == bits(other)
            && self.grounded == other.grounded
            && self.crouching == other.crouching
    }
}

/// How far a character may go: the ball of `radius` Voxels around `centre`. The ground is
/// held only that far (see `sim::Ground`), so beyond it the controller treats space as solid
/// rather than let the character fall through the missing ground. The server decides it, moving
/// it as the ground around the character fills in, and tells the client (see
/// [`ServerMessage::Leash`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Leash {
    /// The ball's centre, in Voxels.
    pub centre: [f64; 3],
    pub radius: f64,
}

/// Client to server.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ClientMessage {
    /// Asks to make an Edit. `client_seq` numbers the client's requests from 0 upwards, and
    /// the reply names it.
    Edit { client_seq: u64, op: EditOp },
    /// Puts this client's character on the Planet, standing at `position` in Voxels. `spawn`
    /// numbers the client's spawns, and every [`ServerMessage::Body`] and
    /// [`ServerMessage::Leash`] about this character names the one it is for, so that a client
    /// that spawns again ignores what was still in flight for the last. The server answers with
    /// a `Body` for no inputs applied yet, and with the first `Leash` once the ground around the
    /// character is ready.
    ///
    /// **Inputs before the first leash.** A client sends no input until it has that first
    /// leash. If one arrives anyway (a stale leash let it go early), the server holds it, in
    /// order, up to a bound, and applies it when the leash exists: it is neither applied
    /// without a leash nor dropped, which would leave every later input a gap.
    Spawn { spawn: u64, position: [f64; 3] },
    /// The character's input for one tick. `seq` numbers the client's inputs from 0 upwards;
    /// the server applies them in order, each as one tick.
    Input { seq: u64, input: MoveInput },
}

/// Server to client.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ServerMessage {
    /// The server made this Edit. Every client gets every Edit, in `global_seq` order (1 for
    /// the first Edit of the Planet). The client that asked for it also gets `ack`, the
    /// `client_seq` it asked with, and so knows that this is the Edit it already predicted.
    Applied {
        global_seq: u64,
        op: EditOp,
        ack: Option<u64>,
    },
    /// The server turned an Edit down; the client takes back its prediction. A reject carries
    /// only a reason: the server does not yet send the true bricks, since a client that
    /// predicted correctly has them and one that did not is out of reach of this milestone.
    Rejected {
        client_seq: u64,
        reason: RejectReason,
    },
    /// The server has applied the first `applied` inputs of this client's character (none, just
    /// after a spawn), and this is where the character is then. Sent once per server tick in
    /// which any arrived. `spawn` is the [`ClientMessage::Spawn`] it is for.
    Body {
        spawn: u64,
        applied: u64,
        state: BodyState,
    },
    /// The character is leashed to `leash` from its input number `from` on (the `from + 1`th it
    /// sends), and until the next one says otherwise. `from` is the number of inputs the server
    /// had applied when it decided, so it is the tick, as the server counts it, at which the
    /// leash starts to hold. The client has usually run ahead of it by then, and applies it to
    /// the same inputs when it runs them again. Sent ahead of the [`ServerMessage::Body`] of the
    /// tick it was decided in. `spawn` is the [`ClientMessage::Spawn`] it is for.
    Leash { spawn: u64, from: u64, leash: Leash },
}

/// The other end is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Disconnected;

/// One end of a connection: sends `S` and receives `R`, in order.
#[derive(Debug)]
pub struct Endpoint<S, R> {
    sender: Sender<S>,
    receiver: Receiver<R>,
}

/// The client's end of a [`loopback`].
pub type ClientEnd = Endpoint<ClientMessage, ServerMessage>;
/// The server's end of a [`loopback`].
pub type ServerEnd = Endpoint<ServerMessage, ClientMessage>;

impl<S, R> Endpoint<S, R> {
    pub fn send(&self, message: S) -> Result<(), Disconnected> {
        self.sender.send(message).map_err(|_| Disconnected)
    }

    /// The next message, if one has arrived.
    pub fn try_recv(&self) -> Result<R, TryRecvError> {
        self.receiver.try_recv()
    }
}

/// A connected pair of endpoints in one process, over channels.
pub fn loopback() -> (ClientEnd, ServerEnd) {
    let (to_server, from_client) = mpsc::channel();
    let (to_client, from_server) = mpsc::channel();
    (
        Endpoint {
            sender: to_server,
            receiver: from_server,
        },
        Endpoint {
            sender: to_client,
            receiver: from_client,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(client_seq: u64) -> ClientMessage {
        ClientMessage::Edit {
            client_seq,
            op: EditOp::dig_sphere(VoxelCoord([client_seq as i32, -1, 0]), 10),
        }
    }

    #[test]
    fn an_edit_op_is_a_small_fixed_size_message() {
        // ADR 0003: about 24 bytes at most, so that Edits broadcast Planet-wide stay cheap. The op has no
        // list or pointer that could grow: it is `Copy`.
        fn plain<T: Copy>() -> usize {
            size_of::<T>()
        }
        assert_eq!(plain::<EditOp>(), 24);
        // The request that carries it is not much bigger.
        assert!(plain::<ClientMessage>() <= 64);
    }

    #[test]
    fn messages_arrive_in_the_order_they_were_sent_in_each_direction() {
        let (client, server) = loopback();
        for seq in 0..5 {
            client.send(edit(seq)).unwrap();
        }
        for seq in 0..5 {
            assert_eq!(server.try_recv(), Ok(edit(seq)));
        }
        assert_eq!(server.try_recv(), Err(TryRecvError::Empty));

        let rejected = ServerMessage::Rejected {
            client_seq: 3,
            reason: RejectReason::BrushTooLarge,
        };
        server.send(rejected).unwrap();
        assert_eq!(client.try_recv(), Ok(rejected));
        assert_eq!(client.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn a_dropped_end_is_seen_by_the_other() {
        let (client, server) = loopback();
        drop(server);
        assert_eq!(client.send(edit(0)), Err(Disconnected));
        assert_eq!(client.try_recv(), Err(TryRecvError::Disconnected));
    }
}
