//! A client predicts its character, the in-process server runs it for real, and both end with
//! the same bits, talking only through the loopback.

use std::time::{Duration, Instant};

use brick_store::{BrickCoord, BrickStore};
use net::{BodyState, ClientEnd, Leash, MoveInput, ServerMessage, loopback};
use planet_gen::generate_region;
use sim::Predictor;

const SEED: u64 = 0xB4A7_A9AC;

/// Bricks around the surface on +x (the surface is near voxel x = 1500).
fn surface() -> BrickStore {
    generate_region(SEED, BrickCoord([180, -3, -3]), BrickCoord([192, 3, 3]))
}

/// A scripted walk: round in a circle with the heading turning (by exact steps, since the sim
/// takes whatever vector it is given), running, jumping and crouching at times.
fn script(i: usize) -> MoveInput {
    let heading = [
        [0.0, 1.0, 0.0],
        [0.0, 0.0, 1.0],
        [0.0, -1.0, 0.0],
        [0.0, 0.0, -1.0],
    ][i / 90 % 4];
    MoveInput {
        forward: 1,
        right: [0, 0, 1, 0, -1][i / 70 % 5],
        heading,
        jump: i.is_multiple_of(83),
        run: i % 200 > 120,
        crouch: i % 310 > 270,
    }
}

const TICKS: usize = 900;
const GOLDEN: u64 = 0x2d22_bd42_f6c8_c81b;

/// FNV-1a over the bits of the state.
fn fingerprint(state: &BodyState) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let words = state
        .position
        .into_iter()
        .chain(state.velocity)
        .map(f64::to_bits)
        .chain([u64::from(state.grounded), u64::from(state.crouching)]);
    for word in words {
        for byte in word.to_le_bytes() {
            hash = (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

/// Takes in everything the server has said.
fn take_in(client: &mut Predictor, end: &ClientEnd) -> Option<(u64, BodyState)> {
    let mut last = None;
    while let Ok(message) = end.try_recv() {
        if let ServerMessage::Body { applied, state } = message {
            last = Some((applied, state));
        }
        client.receive(message);
    }
    last
}

#[test]
fn golden_the_client_and_the_server_agree_on_every_bit_of_a_long_walk() {
    let (end, server_end) = loopback();
    let server = sim::spawn(surface, vec![server_end]);

    let mut client = Predictor::new(surface());
    let leash = Leash {
        axis: [1.0, 0.0, 0.0],
        radius: 30.0,
    };
    end.send(client.spawn([1530.0, 0.0, 0.0], Some(leash)))
        .unwrap();
    for i in 0..TICKS {
        let request = client.walk(script(i)).expect("spawned");
        end.send(request).unwrap();
    }
    let predicted = *client.character().expect("spawned").state();

    // Wait for the server to have applied them all.
    let started = Instant::now();
    let mut told = None;
    while told.is_none_or(|(applied, _)| applied < TICKS as u64) {
        assert!(started.elapsed() < Duration::from_secs(20), "no reply");
        told = take_in(&mut client, &end).or(told);
        std::thread::sleep(Duration::from_millis(2));
    }
    let server = server.stop();

    let (applied, said) = told.unwrap();
    assert_eq!(applied, TICKS as u64);
    let authority = *server.character(0).expect("spawned").state();
    assert!(
        authority.identical(&predicted),
        "server {authority:?}\nclient {predicted:?}"
    );
    assert!(said.identical(&authority));
    assert_eq!(
        client.body_desyncs(),
        0,
        "the server never had to correct it"
    );

    // Pinned, so that CI (Linux and Windows, debug and release) fails if any machine's
    // arithmetic differs: if the controller changes on purpose, replace it with the `actual`.
    let fingerprint = fingerprint(&authority);
    assert_eq!(fingerprint, GOLDEN, "actual {fingerprint:#018x}");

    // It really walked: fell onto the ground, moved, and stayed on the leash.
    let [x, y, z] = authority.position;
    assert!((1440.0..1530.0).contains(&x), "x = {x}");
    assert!(y * y + z * z <= 30.0 * 30.0 + 1.0, "{y} {z}");
    assert!(y.abs() + z.abs() > 5.0, "went nowhere: {y} {z}");
}

#[test]
fn a_server_that_disagrees_corrects_the_client_which_replays_what_the_server_has_not_seen() {
    let mut client = Predictor::new(surface());
    let _ = client.spawn([1530.0, 0.0, 0.0], None);
    for i in 0..10 {
        client.walk(script(i)).unwrap();
    }
    let before = *client.character().unwrap().state();
    // The server says that after 4 inputs it was somewhere else.
    let mut said = before;
    said.position[1] += 50.0;
    client.receive(ServerMessage::Body {
        applied: 4,
        state: said,
    });
    assert_eq!(client.body_desyncs(), 1);
    let now = client.character().unwrap().state();
    assert!(now.position[1] > before.position[1] + 40.0, "{now:?}");
}
