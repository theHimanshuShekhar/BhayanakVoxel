//! A player's Edits are held to the server's limits, and a client that went ahead of the server
//! with one that was turned down takes it back and ends with the bricks the server has.

use brick_store::{BrickCoord, BrickStore, Material, VoxelCoord};
use net::{Alignment, Brush, EditMode, EditOp, RejectReason, ServerMessage, Shape, loopback};
use planet_gen::generate_region;
use sim::{Predictor, Server, TICK_RATE_HZ};

const SEED: u64 = 0xB4A7_A9AC;

/// Where the character stands: on the surface at +x, which is about Voxel x = 1500.
const STANDING: [f64; 3] = [1500.0, 0.0, 0.0];

fn surface() -> BrickStore {
    generate_region(SEED, BrickCoord([180, -12, -12]), BrickCoord([192, 12, 12]))
}

fn place(shape: Shape, radius: u16, centre: [i32; 3]) -> EditOp {
    EditOp {
        mode: EditMode::Place,
        brush: Brush { shape, radius },
        centre: VoxelCoord(centre),
        material: Material(35),
        alignment: Alignment::World,
        rotation: 0,
    }
}

/// What came of asking for `ops` all at once: the reasons the server gave (`None` for an Edit it
/// made), and whether the client's bricks are the server's in the end, with nothing pending.
fn ask(ops: &[EditOp]) -> Vec<Option<RejectReason>> {
    let (link, end) = loopback();
    let mut client = Predictor::new(surface());
    let mut server = Server::new(surface());
    link.send(client.spawn(STANDING)).unwrap();
    for &op in ops {
        link.send(client.edit(op).0).unwrap();
    }
    assert_eq!(client.pending(), ops.len());
    // The client's bricks, with every Edit predicted, before the server's word.
    let predicted = client.store().content_hash();

    server.tick(std::slice::from_ref(&end));
    let mut told = Vec::new();
    while let Ok(message) = link.try_recv() {
        match message {
            ServerMessage::Applied { ack: Some(_), .. } => told.push(None),
            ServerMessage::Rejected { reason, .. } => told.push(Some(reason)),
            _ => {}
        }
        client.receive(message);
    }
    assert_eq!(told.len(), ops.len(), "the server answered each Edit");
    assert_eq!(client.pending(), 0);
    assert_eq!(client.desyncs(), 0);
    assert_eq!(client.store().content_hash(), server.store().content_hash());
    if told.iter().any(Option::is_some) {
        assert_ne!(
            client.store().content_hash(),
            predicted,
            "it took them back"
        );
    }
    told
}

#[test]
fn a_brush_over_2_m_is_turned_down_and_the_client_takes_it_back() {
    let told = ask(&[
        place(Shape::Sphere, 21, [1505, 0, 0]),
        place(Shape::Sphere, 20, [1505, 0, 0]),
    ]);
    assert_eq!(told, [Some(RejectReason::BrushTooLarge), None]);
}

#[test]
fn an_edit_over_8_m_from_the_eyes_is_turned_down_and_the_client_takes_it_back() {
    // The eyes are at about x = 1516, so these are 8.5 m away along the surface, and 7.5 m.
    let far = place(Shape::Cube, 3, [1516, 85, 0]);
    let near = place(Shape::Cube, 3, [1516, 75, 0]);
    assert_eq!(ask(&[far, near]), [Some(RejectReason::OutOfReach), None]);
}

#[test]
fn a_21st_edit_in_a_second_is_turned_down_and_the_client_takes_it_back() {
    let ops: Vec<_> = (0..21)
        .map(|i| place(Shape::Sphere, 1, [1505, i * 4 - 40, 0]))
        .collect();
    let told = ask(&ops);
    assert!(told[..20].iter().all(Option::is_none));
    assert_eq!(told[20], Some(RejectReason::RateLimited));
}

#[test]
fn edits_over_8_m3_of_voxels_in_a_burst_are_turned_down_and_the_client_takes_them_back() {
    // Cubes of 21 Voxels a side, 9 261 Voxels each, in the air over the surface, side by side.
    let cube = |i: i32| place(Shape::Cube, 10, [1520, (i % 3 - 1) * 22, (i / 3 - 1) * 22]);
    let ops: Vec<_> = (0..9).map(cube).collect();
    let told = ask(&ops);
    assert!(told[..7].iter().all(Option::is_none), "{told:?}");
    assert_eq!(told[7..], [Some(RejectReason::VolumeLimited); 2]);
}

#[test]
fn a_material_that_is_not_defined_is_turned_down_and_the_client_takes_it_back() {
    let unknown = EditOp {
        material: Material(900),
        ..place(Shape::Sphere, 5, [1505, 0, 0])
    };
    assert_eq!(
        ask(&[unknown, place(Shape::Sphere, 5, [1505, 0, 0])]),
        [Some(RejectReason::UnknownMaterial), None]
    );
}

#[test]
fn the_limits_come_back_with_time() {
    let (link, end) = loopback();
    let mut server = Server::new(surface());
    let mut client = Predictor::new(surface());
    link.send(client.spawn(STANDING)).unwrap();
    let op = place(Shape::Sphere, 1, [1505, 0, 0]);
    let accepted = |link: &net::ClientEnd, client: &mut Predictor, server: &mut Server, n| {
        for _ in 0..n {
            link.send(client.edit(op).0).unwrap();
        }
        server.tick(std::slice::from_ref(&end));
        let mut accepted = 0;
        while let Ok(message) = link.try_recv() {
            accepted += usize::from(matches!(message, ServerMessage::Applied { .. }));
            client.receive(message);
        }
        accepted
    };
    assert_eq!(accepted(&link, &mut client, &mut server, 30), 20);
    // Three ticks bring back one Edit's worth of the 20 a second.
    for _ in 0..2 {
        server.tick(std::slice::from_ref(&end));
    }
    assert_eq!(accepted(&link, &mut client, &mut server, 5), 1);
    for _ in 0..TICK_RATE_HZ {
        server.tick(std::slice::from_ref(&end));
    }
    assert_eq!(accepted(&link, &mut client, &mut server, 30), 20);
}
