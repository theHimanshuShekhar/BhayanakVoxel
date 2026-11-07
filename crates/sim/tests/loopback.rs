//! Two clients and the in-process server, talking only through loopbacks, end up with the same
//! bricks after scripted runs of Edits.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use brick_store::Material;
use brick_store::{BrickCoord, BrickStore, VoxelCoord};
use net::{Alignment, Brush, ClientEnd, EditMode, EditOp, ServerMessage, Shape, loopback};
use planet_gen::generate_region;
use sim::{Character, Movement, Predictor, Server};

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
        rotation: 0,
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
        op(Dig, Cylinder, 8, [1480, -8, 4], Local),
        op(Dig, Sphere, 50, [1488, 0, 0], Local),
        op(Dig, Sphere, 20, [-5, -5, -5], Local),
        op(Smooth, Sphere, 8, [1486, 0, 0], Local),
        EditOp {
            material: Material(60_000),
            ..op(Place, Sphere, 3, [1486, 3, 3], Local)
        },
        op(Flatten, Cube, 7, [1484, 2, 2], Local),
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
        op(Dig, Cube, 7, [1493, -2, 1], World),
        op(Place, Sphere, 8, [1486, 0, 0], Local),
        op(Dig, Cylinder, 9, [1480, -8, 4], World),
        op(Dig, Sphere, 20, [0, i32::MIN, 0], Local),
        op(Flatten, Sphere, 8, [1495, 5, -3], Local),
        op(Smooth, Cube, 7, [1488, 0, 0], World),
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

/// Gives the client what the server has said, and notes the Edits that it made, in its order.
fn take_in(client: &mut Predictor, end: &ClientEnd, made: &mut Vec<(u64, EditOp)>) {
    while let Ok(reply) = end.try_recv() {
        if let ServerMessage::Applied { global_seq, op, .. } = reply {
            made.push((global_seq, op));
        }
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
            Server::new(surface(), [0.0; 3])
                .with_debug_client(0)
                .with_debug_client(1)
                .without_dig_timing()
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

    // Every client hears every Edit, so the first client's log is all of them.
    let (mut made, mut unused) = (Vec::new(), Vec::new());
    let started = Instant::now();
    while first.pending() > 0 || second.pending() > 0 {
        assert!(started.elapsed() < Duration::from_secs(10), "no reply");
        take_in(&mut first, &first_end, &mut made);
        take_in(&mut second, &second_end, &mut unused);
        std::thread::sleep(Duration::from_millis(1));
    }

    // Stopping joins the thread, after which every message it will ever send is in a channel.
    let server = server.stop();
    take_in(&mut first, &first_end, &mut made);
    take_in(&mut second, &second_end, &mut unused);

    // What the server made is only Edits that are allowed (reach, Brush, Material), and in the
    // order it counted them; the second client's go first. It made all of them: the Brushes are
    // small enough that no limit is reached (the limits have tests of their own).
    let allowed: Vec<_> = valid(second_script.clone())
        .chain(valid(first_script.clone()))
        .collect();
    assert!(
        made.windows(2).all(|w| w[0].0 + 1 == w[1].0),
        "numbered in turn"
    );
    let made: Vec<EditOp> = made.into_iter().map(|(_, op)| op).collect();
    assert!(made.iter().all(|op| allowed.contains(op)), "{made:?}");
    assert_eq!(
        made.len(),
        allowed.len(),
        "the server made {} of the {} Edits it was allowed",
        made.len(),
        allowed.len()
    );
    assert_eq!(made, allowed, "in the server's order");
    let mut expected = surface();
    for op in &made {
        sim::apply(&mut expected, op);
    }
    let want = server.store().content_hash();
    assert_eq!(
        want,
        expected.content_hash(),
        "the Edits the server said it made are what it made"
    );
    assert_ne!(want, untouched, "the Edits dug");
    assert_eq!(first.store().content_hash(), want);
    assert_eq!(second.store().content_hash(), want);
    assert_eq!((first.desyncs(), second.desyncs()), (0, 0));
}
