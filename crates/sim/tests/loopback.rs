//! Two clients and the in-process server, talking only through loopbacks, end up with the same
//! bricks after scripted runs of Edits.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use brick_store::{BrickCoord, BrickStore, VoxelCoord};
use net::{ClientEnd, EditOp, loopback};
use planet_gen::generate_region;
use sim::Predictor;

const SEED: u64 = 0xB4A7_A9AC;

/// Bricks around the surface on +x (the surface is near voxel x = 1500). The Planet's core is
/// not in a store, so Edits at the centre make the server and the clients store it as they dig.
fn surface() -> BrickStore {
    generate_region(SEED, BrickCoord([180, -3, -3]), BrickCoord([192, 3, 3]))
}

/// (centre, radius). Some are rejected: a brush bigger than the server allows, and a centre
/// that the Voxel range cannot hold the brush around.
type Script = &'static [([i32; 3], u16)];

const FIRST: Script = &[
    ([1490, 0, 0], 10),
    ([0, 0, 0], 10),
    ([1480, -8, 4], 20),
    ([1488, 0, 0], 50),
    ([-5, -5, -5], 20),
    ([1470, 2, 2], 20),
    ([i32::MAX, 0, 0], 10),
    ([1486, 3, 3], 0),
    ([3, 3, 3], 7),
];

/// Overlaps the other client's Edits at the surface and in the core.
const SECOND: Script = &[
    ([1493, -2, 1], 15),
    ([4, 0, 0], 12),
    ([1480, -8, 4], 20),
    ([0, i32::MIN, 0], 20),
    ([-12, 0, 6], 9),
    ([1495, 5, -3], 10),
    ([1488, 0, 0], 21),
];

fn valid(script: Script) -> impl Iterator<Item = EditOp> {
    script
        .iter()
        .map(|&(centre, radius)| EditOp::dig_sphere(VoxelCoord(centre), radius))
        .filter(|op| sim::validate(op).is_ok())
}

/// A client that asks for every Edit of `script` at once, so that all of them are pending.
fn ask(script: Script, end: &ClientEnd) -> Predictor {
    let mut client = Predictor::new(surface());
    for &(centre, radius) in script {
        let (request, _) = client.edit(EditOp::dig_sphere(VoxelCoord(centre), radius));
        end.send(request).unwrap();
    }
    assert_eq!(client.pending(), script.len());
    client
}

fn take_in(client: &mut Predictor, end: &ClientEnd) {
    while let Ok(reply) = end.try_recv() {
        client.receive(reply);
    }
}

#[test]
fn two_clients_and_the_server_have_identical_bricks_after_overlapping_runs_of_edits() {
    let (first_end, first_server) = loopback();
    let (second_end, second_server) = loopback();
    // The server ticks only once every request is in: then it handles all of the second client's
    // Edits (it serves its clients in the order given), then all of the first's, so the order
    // does not depend on timing. The first client's Edits are still pending when the second's
    // arrive, and the second's are acked before the first's arrive.
    let (go, wait) = mpsc::channel::<()>();
    let server = sim::spawn(
        move || {
            wait.recv().expect("the test releases the server");
            surface()
        },
        vec![second_server, first_server],
    );
    let untouched = surface().content_hash();

    let mut first = ask(FIRST, &first_end);
    let mut second = ask(SECOND, &second_end);
    let (first_pending, second_pending) = (first.pending(), second.pending());
    assert_eq!(first_pending + second_pending, FIRST.len() + SECOND.len());
    go.send(()).unwrap();

    let started = Instant::now();
    while first.pending() > 0 || second.pending() > 0 {
        assert!(started.elapsed() < Duration::from_secs(10), "no reply");
        take_in(&mut first, &first_end);
        take_in(&mut second, &second_end);
        std::thread::sleep(Duration::from_millis(1));
    }

    // Stopping joins the thread, after which every message it will ever send is in a channel.
    let server = server.stop();
    take_in(&mut first, &first_end);
    take_in(&mut second, &second_end);

    // What the server should have: the valid Edits, the second client's first.
    let mut expected = surface();
    for op in valid(SECOND).chain(valid(FIRST)) {
        sim::apply(&mut expected, &op);
    }
    let want = server.store().content_hash();
    assert_eq!(
        want,
        expected.content_hash(),
        "the server's order and rejects"
    );
    assert_ne!(want, untouched, "the Edits dug");
    assert_eq!(first.store().content_hash(), want);
    assert_eq!(second.store().content_hash(), want);
    assert_eq!((first.desyncs(), second.desyncs()), (0, 0));
}
