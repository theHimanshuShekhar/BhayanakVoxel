//! Message protocol between client and server (ADR 0003).
//!
//! The messages are plain data, with no reference to how they travel. In M1 they cross an
//! in-memory [`loopback`]; in M2 renet carries the same types on its reliable-ordered channel,
//! so everything here must keep arriving in the order it was sent.

use std::sync::mpsc::{self, Receiver, Sender};

use brick_store::VoxelCoord;

pub use std::sync::mpsc::TryRecvError;

/// What an Edit does to the Voxels it covers. Place and smooth come with their Materials.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditMode {
    /// Removes the Voxels.
    Dig,
}

/// The shape an Edit covers. Cube and cylinder come with their rotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Brush {
    /// Every Voxel whose coordinate is within `radius` Voxels of the centre, so a radius of 0
    /// is the centre alone.
    Sphere { radius: u16 },
}

/// One Edit, small enough to broadcast Planet-wide. Fields that Edits do not need yet (a
/// Material, a rotation, an alignment mode) are added when they do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditOp {
    pub mode: EditMode,
    pub brush: Brush,
    /// The Voxel the Brush is centred on.
    pub centre: VoxelCoord,
}

impl EditOp {
    /// Digs a sphere of `radius` Voxels around `centre`.
    pub fn dig_sphere(centre: VoxelCoord, radius: u16) -> Self {
        Self {
            mode: EditMode::Dig,
            brush: Brush::Sphere { radius },
            centre,
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

/// How far from its spawn a character may go: the Planet's ground is generated only that far
/// (until it can be streamed, #49), so beyond it the controller treats space as solid rather
/// than let the character fall through the missing ground. It is the cylinder of `radius`
/// Voxels around the line from the Planet's centre through the spawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Leash {
    /// Unit vector from the Planet's centre through the spawn.
    pub axis: [f64; 3],
    pub radius: f64,
}

/// Client to server.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ClientMessage {
    /// Asks to make an Edit. `client_seq` numbers the client's requests from 0 upwards, and
    /// the reply names it.
    Edit { client_seq: u64, op: EditOp },
    /// Puts this client's character on the Planet, standing at `position` in Voxels. The server
    /// answers with [`ServerMessage::Body`] for no inputs applied yet.
    Spawn {
        position: [f64; 3],
        leash: Option<Leash>,
    },
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
    /// which any arrived.
    Body { applied: u64, state: BodyState },
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
