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

/// Client to server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientMessage {
    /// Asks to make an Edit. `client_seq` numbers the client's requests from 0 upwards, and
    /// the reply names it.
    Edit { client_seq: u64, op: EditOp },
}

/// Server to client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
