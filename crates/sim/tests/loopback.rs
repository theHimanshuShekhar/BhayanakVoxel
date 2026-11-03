//! Two clients and the in-process server, talking only through loopbacks, end up with the same
//! bricks after scripted runs of Edits.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use brick_store::Material;
use brick_store::{BrickCoord, BrickStore, VoxelCoord};
use net::{Alignment, Brush, ClientEnd, EditMode, EditOp, Shape, loopback};
use planet_gen::generate_region;
use sim::{Character, Movement, Predictor};

const SEED: u64 = 0xB4A7_A9AC;

/// Bricks around the surface on +x (the surface is near voxel x = 1500). The Planet's core is
/// not in a store, so Edits at the centre make the server and the clients store it as they dig.
fn surface() -> BrickStore {
    generate_region(SEED, BrickCoord([180, -3, -3]), BrickCoord([192, 3, 3]))
}

/// Where both clients stand: on the surface, with the Edits below within 8 m of their eyes.
const STANDING: [f64; 3] = [1495.0, 0.0, 0.0];

fn op(mode: EditMode, shape: Shape, radius: u16, centre: [i32; 3], alignment: Alignment) -> EditOp {
    EditOp {
        mode,
        brush: Brush { shape, radius },
        centre: VoxelCoord(centre),
        material: Material(35),
        alignment,
    }
}

/// Every mode and shape, in both alignments, overlapping the other client's, and some that the
/// server turns down: a Brush bigger than it allows, a centre out of reach (twice, one of them
/// where the Voxel range cannot hold the Brush around it), and a Material that is not defined.
fn first() -> Vec<EditOp> {
    use Alignment::{Local, World};
    use EditMode::{Dig, Flatten, Place, Smooth};
    use Shape::{Cube, Cylinder, Sphere};
    vec![
        op(Dig, Sphere, 10, [1490, 0, 0], Local),
        op(Place, Cube, 6, [1488, 5, 0], Local),
        op(Dig, Cylinder, 12, [1480, -8, 4], Local),
        op(Dig, Sphere, 50, [1488, 0, 0], Local),
        op(Dig, Sphere, 20, [-5, -5, -5], Local),
        op(Smooth, Sphere, 8, [1486, 0, 0], Local),
        EditOp {
            material: Material(60_000),
            ..op(Place, Sphere, 3, [1486, 3, 3], Local)
        },
        op(Flatten, Cube, 10, [1484, 2, 2], Local),
        op(Dig, Sphere, 10, [i32::MAX, 0, 0], Local),
        op(Place, Cylinder, 5, [1486, 3, 3], World),
        op(Dig, Cube, 0, [1486, 3, 3], World),
    ]
}

fn second() -> Vec<EditOp> {
    use Alignment::{Local, World};
    use EditMode::{Dig, Flatten, Place, Smooth};
    use Shape::{Cube, Cylinder, Sphere};
    vec![
        op(Dig, Cube, 9, [1493, -2, 1], World),
        op(Place, Sphere, 8, [1486, 0, 0], Local),
        op(Dig, Cylinder, 14, [1480, -8, 4], World),
        op(Dig, Sphere, 20, [0, i32::MIN, 0], Local),
        op(Flatten, Sphere, 12, [1495, 5, -3], Local),
        op(Smooth, Cube, 10, [1488, 0, 0], World),
        op(Dig, Sphere, 21, [1488, 0, 0], Local),
        op(Place, Cube, 4, [1490, 3, 3], World),
    ]
}

/// Whether the server accepts `op` from a character standing at [`STANDING`] who has made no
/// other Edits lately.
fn accepted(op: &EditOp) -> bool {
    let character = Character::spawn(Movement::default(), STANDING, None);
    sim::validate(op).is_ok() && sim::within_reach(character.eye(), op.centre)
}

fn valid(script: Vec<EditOp>) -> impl Iterator<Item = EditOp> {
    script.into_iter().filter(accepted)
}

/// A client that stands up, and then asks for every Edit of `script` at once, so that all of them
/// are pending.
fn ask(script: &[EditOp], end: &ClientEnd) -> Predictor {
    let mut client = Predictor::new(surface());
    end.send(client.spawn(STANDING)).unwrap();
    for &op in script {
        let (request, _) = client.edit(op);
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

    let (first_script, second_script) = (first(), second());
    let mut first = ask(&first_script, &first_end);
    let mut second = ask(&second_script, &second_end);
    let (first_pending, second_pending) = (first.pending(), second.pending());
    assert_eq!(
        first_pending + second_pending,
        first_script.len() + second_script.len()
    );
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
    for op in valid(second_script).chain(valid(first_script)) {
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
