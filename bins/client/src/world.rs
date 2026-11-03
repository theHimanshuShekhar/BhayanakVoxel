//! The client's view of the Planet: the bricks it predicts on, the packed copy that gets drawn,
//! and the loopback to the server. All Edits come through here, and reach the Voxels only via
//! [`sim::Predictor`], which calls [`sim::apply`].

use brick_store::VoxelCoord;
use net::{ClientEnd, Disconnected, EditMode, EditOp, MoveInput, TryRecvError};
use render::reference::{Hit, Ray, trace};
use render::{Camera, RingsFull, Scene, SceneUpdate, camera};
use sim::Predictor;

use crate::ghost::Ghost;
use crate::loader::{Arrival, Loader};
use crate::tool::Tool;

/// What the frame thread aims to spend taking in bricks from the loader in one frame, the upload
/// to the GPU included. A resident set that is filling takes more frames, not longer ones.
pub const STREAM_BUDGET: std::time::Duration = std::time::Duration::from_micros(2000);
/// Bricks' children taken in at a time, between looks at the clock.
const BATCH: usize = 8;
/// Most bricks asked of the loader in one frame (the rest wait for the next): the first call has
/// every brick the Guard leaves to split, and the table that holds them is not made in a frame.
const WANTED_PER_FRAME: usize = 20_000;

/// How far the player aims an Edit, in Voxels: 7.5 m, which leaves half a metre of the server's
/// reach ([`sim::EDIT_REACH`], measured from where the server has the character) for the character
/// to have moved on from where the client saw it.
pub(crate) const AIM_REACH: f64 = sim::EDIT_REACH - 5.0;

/// Why the Planet cannot be kept going.
#[derive(Debug, PartialEq, Eq)]
pub enum Stopped {
    /// The server is gone.
    Disconnected,
    /// The Planet no longer fits the rings' buffers; the scene is not to be used again.
    RingsFull,
}

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disconnected => write!(f, "the server stopped"),
            Self::RingsFull => write!(f, "{RingsFull}"),
        }
    }
}

impl std::error::Error for Stopped {}

impl From<Disconnected> for Stopped {
    fn from(_: Disconnected) -> Self {
        Self::Disconnected
    }
}

impl From<RingsFull> for Stopped {
    fn from(_: RingsFull) -> Self {
        Self::RingsFull
    }
}

/// How long the frame thread spent on the scene each frame (following the camera, taking in
/// bricks, copying the changes to the GPU), for reporting the worst and the 99th percentile.
#[derive(Debug, Default)]
pub struct SceneWork {
    millis: Vec<f64>,
}

impl SceneWork {
    pub fn record(&mut self, took: std::time::Duration) {
        self.millis.push(took.as_secs_f64() * 1000.0);
    }

    /// The longest frame, in milliseconds.
    pub fn worst(&self) -> Option<f64> {
        self.millis.iter().copied().max_by(f64::total_cmp)
    }

    /// The 99th percentile (nearest rank): the time that 99% of the frames did not exceed.
    pub fn p99(&self) -> Option<f64> {
        let mut sorted = self.millis.clone();
        sorted.sort_by(f64::total_cmp);
        let rank = (sorted.len() * 99).div_ceil(100);
        rank.checked_sub(1).map(|i| sorted[i])
    }

    /// One line for a log, if any frame was recorded.
    pub fn summary(&self) -> Option<String> {
        Some(format!(
            "scene work on the frame thread: {} frames, p99 {:.2} ms, worst {:.2} ms",
            self.millis.len(),
            self.p99()?,
            self.worst()?
        ))
    }
}

pub struct World {
    scene: Scene,
    predictor: Predictor,
    link: ClientEnd,
    /// Generates the bricks that the scene is waiting for, if the Planet is streamed in.
    loader: Option<Loader>,
    /// What [`World::stream`] expects taking in a batch to cost.
    batch_cost: std::time::Duration,
    /// What the next click would change, while there is something under the cursor to apply the
    /// tool to. Made again when the aim, the tool or the Planet changes.
    ghost: Option<Ghost>,
}

/// The first solid Voxel along the camera's view through the middle of the screen.
pub(crate) fn centre_hit(scene: &Scene, camera: &Camera) -> Option<Hit> {
    let (cell, origin) = camera::split(camera.position);
    trace(
        scene,
        &Ray {
            cell,
            origin,
            dir: camera.forward().map(|c| c as f32),
        },
    )
}

impl World {
    /// `scene` must be the packing of the predictor's bricks.
    pub fn new(scene: Scene, predictor: Predictor, link: ClientEnd) -> Self {
        Self {
            scene,
            predictor,
            link,
            loader: None,
            batch_cost: std::time::Duration::ZERO,
            ghost: None,
        }
    }

    /// Streams the Planet in with `loader`: see [`World::stream`]. The scene must have been made
    /// from the predictor's store, which must stream ([`brick_store::BrickStore::stream_from`]).
    pub fn with_loader(mut self, loader: Loader) -> Self {
        self.loader = Some(loader);
        self
    }

    /// The loader, if there is one.
    pub fn loader(&self) -> Option<&Loader> {
        self.loader.as_ref()
    }

    /// Bricks that wait for the bricks under them to be resident. Zero, and nothing pending with
    /// the loader, once the resident set is complete for the bands.
    pub fn waiting(&self) -> usize {
        self.scene.waiting()
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// The Planet's bricks as the client holds them.
    #[cfg(test)]
    pub fn store(&self) -> &brick_store::BrickStore {
        self.predictor.store()
    }

    /// Puts the character on the Planet at `position` (Voxels), and tells the server. It falls
    /// from there once the ground around it is ready here and the server has said how far it
    /// may go (see [`sim::Predictor::walk`]).
    pub fn spawn(&mut self, position: [f64; 3]) -> Result<(), Stopped> {
        self.link.send(self.predictor.spawn(position))?;
        Ok(())
    }

    /// Runs one tick of the character on `input` here and on the server. Does nothing before
    /// the character has spawned, and while it waits for its ground.
    pub fn walk(&mut self, input: MoveInput) -> Result<(), Stopped> {
        if let Some(request) = self.predictor.walk(input) {
            self.link.send(request)?;
        }
        Ok(())
    }

    /// What bounds the character, once the server has said.
    pub fn leash(&self) -> Option<net::Leash> {
        self.predictor.character().and_then(sim::Character::leash)
    }

    /// Where the character's eyes are, if it has spawned.
    pub fn eye(&self) -> Option<[f64; 3]> {
        self.predictor.character().map(sim::Character::eye)
    }

    /// Moves the rings of the Planet to centre on `camera`, a position in Voxels. Returns how
    /// the scene changed, if it did.
    pub fn recentre(&mut self, camera: [f64; 3]) -> Result<Option<SceneUpdate>, RingsFull> {
        self.scene.recentre(self.predictor.store(), camera)
    }

    /// Asks the loader for what the bands are waiting for, nearest `camera` first, and takes in
    /// what it has generated, for [`STREAM_BUDGET`] at the most: a few bricks' children at a time
    /// are held, the scene splits the bricks they are under, and `show` copies what that changed
    /// to the GPU (its time is in the budget). This is all the frame thread does of loading: the
    /// bricks are generated on the loader's threads, at most [`crate::loader::MAX_AHEAD`] ahead
    /// of what is taken in here, and what is not taken in this frame waits for the next.
    pub fn stream<E: From<RingsFull>>(
        &mut self,
        camera: [f64; 3],
        mut show: impl FnMut(&Scene, &SceneUpdate) -> Result<(), E>,
    ) -> Result<(), E> {
        let Self {
            scene,
            predictor,
            loader: Some(loader),
            batch_cost,
            ..
        } = self
        else {
            return Ok(());
        };
        let Some(bands) = scene.bands() else {
            return Ok(());
        };
        let started = std::time::Instant::now();
        loader.cancel(scene.take_cancelled());
        loader.request(
            predictor.store(),
            scene.take_wanted_up_to(WANTED_PER_FRAME),
            camera,
        );
        loader.sync(camera, bands);
        let mut worst = std::time::Duration::ZERO;
        while started.elapsed() + *batch_cost <= STREAM_BUDGET {
            let batch_started = std::time::Instant::now();
            let mut resident = Vec::new();
            for arrival in loader.receive(BATCH) {
                match arrival {
                    Arrival::Children { key, bricks } => {
                        predictor
                            .store_for_loading()
                            .hold_children(key.0, key.1, bricks);
                        resident.push(key);
                    }
                    Arrival::Dropped(key) => scene.unwant(key),
                }
            }
            if resident.is_empty() {
                break;
            }
            if let Some(update) = scene.arrive(predictor.store(), &resident)? {
                show(scene, &update)?;
            }
            worst = worst.max(batch_started.elapsed());
        }
        // What the next frame expects a batch to cost: a running mean of the worst of each frame,
        // and never more than a third of the budget, so that one slow batch (a table that grew)
        // does not stop taking bricks in for the frames after it.
        if !worst.is_zero() {
            *batch_cost = (batch_cost.mul_f64(0.8) + worst.mul_f64(0.2)).min(STREAM_BUDGET / 3);
        }
        Ok(())
    }

    /// Selects the bricks again for other LOD `bands` (a new bias). Returns how the scene
    /// changed, if it did.
    pub fn set_bands(
        &mut self,
        bands: render::lod::Bands,
    ) -> Result<Option<SceneUpdate>, RingsFull> {
        self.scene.set_bands(self.predictor.store(), bands)
    }

    /// The Edit that `tool` makes where the camera looks: at the first solid Voxel along the view,
    /// or, to place, at the air Voxel in front of it. `None` if there is none within
    /// [`AIM_REACH`].
    pub fn aim(&self, camera: &Camera, tool: &Tool) -> Option<EditOp> {
        let hit = centre_hit(&self.scene, camera)?;
        if f64::from(hit.t) > AIM_REACH {
            return None;
        }
        let centre = match tool.mode {
            EditMode::Place => std::array::from_fn(|a| hit.voxel[a] + hit.normal[a]),
            _ => hit.voxel,
        };
        Some(tool.op(VoxelCoord(centre)))
    }

    /// Makes the ghost for where the camera looks, if there is a Voxel to apply `tool` to. It is
    /// kept while the aim, the tool and the Planet stay as they are.
    pub fn show_ghost(&mut self, camera: &Camera, tool: &Tool) {
        let Some(op) = self.aim(camera, tool) else {
            self.ghost = None;
            return;
        };
        if self.ghost.as_ref().is_none_or(|ghost| ghost.op != op) {
            let changes = sim::plan(self.predictor.store(), &op);
            self.ghost = Some(Ghost::new(op, changes));
        }
    }

    /// Stops showing the ghost.
    pub fn hide_ghost(&mut self) {
        self.ghost = None;
    }

    /// The Voxels the next click would change, as [`World::show_ghost`] last found them.
    pub fn ghost(&self) -> Option<&Ghost> {
        self.ghost.as_ref()
    }

    /// Applies `tool` where the camera looks: shows the Edit at once, and asks the server for it.
    /// Returns how the scene changed, if there was anything to apply it to.
    pub fn edit(&mut self, camera: &Camera, tool: &Tool) -> Result<Option<SceneUpdate>, Stopped> {
        let Some(op) = self.aim(camera, tool) else {
            return Ok(None);
        };
        let (request, changed) = self.predictor.edit(op);
        self.link.send(request)?;
        Ok(self.refresh(&changed)?)
    }

    /// Takes in what the server has sent. Returns how the scene changed, if it did.
    pub fn poll(&mut self) -> Result<Option<SceneUpdate>, Stopped> {
        let mut changed = Vec::new();
        loop {
            match self.link.try_recv() {
                Ok(message) => changed.extend(self.predictor.receive(message)),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err(Disconnected.into()),
            }
        }
        Ok(self.refresh(&changed)?)
    }

    /// Packs the bricks that changed.
    fn refresh(
        &mut self,
        changed: &[brick_store::BrickCoord],
    ) -> Result<Option<SceneUpdate>, RingsFull> {
        if changed.is_empty() {
            return Ok(None);
        }
        // What the ghost would change has changed with them.
        self.ghost = None;
        self.scene.update(self.predictor.store(), changed).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brick_store::{Brick, BrickCoord, BrickStore, Material};
    use net::{
        ClientMessage, EditMode, Leash, RejectReason, ServerEnd, ServerMessage, Shape, loopback,
    };

    const STONE: Material = Material(7);

    /// A wall of stone, well outside the Planet: Voxels x 4000..4032, y 0..32, z -32..0, in bricks of their own.
    fn wall() -> BrickStore {
        let mut store = BrickStore::new();
        for x in 500..504 {
            for y in 0..4 {
                for z in -4..0 {
                    store.insert(BrickCoord([x, y, z]), Brick::solid(STONE));
                }
            }
        }
        store
    }

    /// Looking at the wall's middle from in front of it.
    fn camera() -> Camera {
        Camera::looking_at([4016.5, 16.5, 20.5], [4016.5, 16.5, 0.0])
    }

    fn world() -> (World, ServerEnd) {
        let store = wall();
        let scene = Scene::new(&store, None);
        let (client, server) = loopback();
        (World::new(scene, Predictor::new(store), client), server)
    }

    #[test]
    fn a_click_digs_at_the_voxel_in_the_middle_of_the_view_and_asks_the_server() {
        let (mut world, server) = world();
        let update = world.edit(&camera(), &Tool::default()).unwrap();
        assert!(
            matches!(update, Some(SceneUpdate::Patched { .. })),
            "{update:?}"
        );
        // The wall's front face is z = -1; the view's middle is Voxel (4016, 16, -1).
        assert_eq!(
            server.try_recv(),
            Ok(ClientMessage::Edit {
                client_seq: 0,
                op: EditOp::dig_sphere(VoxelCoord([4016, 16, -1]), crate::tool::DEFAULT_RADIUS)
            })
        );
        // The hole is drawn at once: the same ray now reaches deeper than the front face.
        let deeper = centre_hit(world.scene(), &camera()).unwrap();
        assert!(deeper.voxel[2] < -1, "{deeper:?}");
    }

    #[test]
    fn a_dig_in_a_scene_of_rings_is_drawn_at_once_and_the_rings_follow_the_camera() {
        let mut store = wall();
        store.build_lod(4);
        let bound = render::scene::Bound {
            centre: [0; 3],
            radius: 4200,
        };
        let scene = Scene::rings(
            &store,
            camera().position,
            render::lod::Bands::with_d0(64.0),
            bound,
        )
        .unwrap();
        let (client, _server) = loopback();
        let mut world = World::new(scene, Predictor::new(store), client);

        let update = world.edit(&camera(), &Tool::default()).unwrap();
        assert!(matches!(update, Some(SceneUpdate::Rings(_))), "{update:?}");
        let deeper = centre_hit(world.scene(), &camera()).unwrap();
        assert!(deeper.voxel[2] < -1, "{deeper:?}");

        // Where the camera is, the bands stay; a little way off, the bricks that change level are
        // patched; and a jump of more than a band's width makes the scene again.
        assert_eq!(world.recentre(camera().position), Ok(None));
        let [x, y, z] = camera().position;
        assert!(matches!(
            world.recentre([x + 60.0, y, z]),
            Ok(None | Some(SceneUpdate::Rings(_)))
        ));
        assert!(matches!(
            world.recentre([x + 900.0, y, z]),
            Ok(Some(SceneUpdate::Rebuilt))
        ));
    }

    #[test]
    fn the_character_waits_for_the_leash_then_lands_on_the_wall() {
        let (mut world, server) = world();
        assert_eq!(world.eye(), None);
        world.walk(MoveInput::default()).unwrap();
        assert_eq!(
            server.try_recv(),
            Err(TryRecvError::Empty),
            "not spawned yet"
        );

        // Above the middle of the wall's +x face (x = 4032).
        let spawn = [4036.0, 16.0, -16.0];
        world.spawn(spawn).unwrap();
        for _ in 0..10 {
            world.walk(MoveInput::default()).unwrap();
        }
        // Standing where it was put, eyes 1.6 m up: it waits.
        let eye = world.eye().unwrap();
        assert!((eye[0] - (4036.0 + 16.0)).abs() < 0.1, "{eye:?}");

        // The server's word: a ball round the spawn's brick, wide enough for the whole wall. The
        // wall's bricks are in the store, and the rest of the ground is air.
        server
            .send(ServerMessage::Leash {
                spawn: 1,
                from: 0,
                leash: Leash {
                    centre: [4036.0, 20.0, -12.0],
                    radius: 40.0,
                },
            })
            .unwrap();
        world.poll().unwrap();
        for _ in 0..200 {
            world.walk(MoveInput::default()).unwrap();
        }
        let eye = world.eye().unwrap();
        assert!((4032.0 + 15.0..4032.0 + 17.0).contains(&eye[0]), "{eye:?}");

        assert_eq!(
            server.try_recv(),
            Ok(ClientMessage::Spawn {
                spawn: 1,
                position: spawn
            })
        );
        let mut inputs = 0;
        while let Ok(ClientMessage::Input { seq, .. }) = server.try_recv() {
            assert_eq!(seq, inputs);
            inputs += 1;
        }
        // The ten ticks that waited for the server's word were not sent: all the others were.
        assert_eq!(inputs, 200);
    }

    fn place_cube() -> Tool {
        Tool {
            mode: EditMode::Place,
            shape: Shape::Cube,
            radius: 4,
            material: Material(35),
            ..Tool::default()
        }
    }

    #[test]
    fn placing_builds_in_the_air_voxel_in_front_of_the_one_that_is_aimed_at() {
        let (mut world, server) = world();
        let update = world.edit(&camera(), &place_cube()).unwrap();
        assert!(update.is_some());
        // The wall's front face is z = -1, so the air Voxel in front of it is z = 0.
        let sent = server.try_recv();
        let Ok(ClientMessage::Edit { op, .. }) = sent else {
            panic!("{sent:?}");
        };
        assert_eq!(op.centre, VoxelCoord([4016, 16, 0]));
        assert_eq!((op.mode, op.material), (EditMode::Place, Material(35)));
        // A cube of 9 Voxels a side, half of it in the wall already: z = 0 to 4 are new.
        let built = world.store().get(VoxelCoord([4016, 16, 4]));
        assert_eq!(built, Some(Material(35)));
        assert_eq!(world.store().get(VoxelCoord([4016, 16, 5])), None);
        // And it is drawn: the same ray now stops 5 Voxels sooner.
        let hit = centre_hit(world.scene(), &camera()).unwrap();
        assert_eq!(hit.voxel[2], 4);
    }

    #[test]
    fn nothing_is_aimed_at_beyond_the_reach() {
        let (mut world, server) = world();
        let far = Camera::looking_at([4016.5, 16.5, 120.5], [4016.5, 16.5, 0.0]);
        assert_eq!(world.aim(&far, &Tool::default()), None);
        assert!(world.edit(&far, &Tool::default()).unwrap().is_none());
        assert_eq!(server.try_recv(), Err(TryRecvError::Empty));
        // Just inside it, there is.
        let near = Camera::looking_at([4016.5, 16.5, 70.5], [4016.5, 16.5, 0.0]);
        assert!(world.aim(&near, &Tool::default()).is_some());
    }

    #[test]
    fn the_ghost_is_exactly_the_voxels_the_click_changes_and_the_op_it_sends() {
        let mut non_empty = 0;
        for mode in [
            EditMode::Dig,
            EditMode::Place,
            EditMode::Smooth,
            EditMode::Flatten,
        ] {
            for shape in [Shape::Sphere, Shape::Cube, Shape::Cylinder] {
                let (mut world, server) = world();
                let tool = Tool {
                    mode,
                    shape,
                    radius: 6,
                    material: Material(35),
                    ..Tool::default()
                };
                world.show_ghost(&camera(), &tool);
                let ghost = world.ghost().expect("something is aimed at").clone();
                let before = world.store().clone();

                world.edit(&camera(), &tool).unwrap();
                let Ok(ClientMessage::Edit { op, .. }) = server.try_recv() else {
                    panic!("no Edit sent");
                };
                assert_eq!(ghost.op, op, "the ghost is of the op that is sent");
                let after = world.store();
                // The Voxels that differ now are the ghost's, and are what it said they become.
                let mut differing = 0;
                for x in 4016 - 20..=4016 + 20 {
                    for y in -4..=36 {
                        for z in -26..=26 {
                            let voxel = VoxelCoord([x, y, z]);
                            let now = after.get(voxel);
                            let change = ghost.changes.iter().find(|c| c.voxel == voxel);
                            assert_eq!(before.get(voxel) != now, change.is_some(), "{op:?}");
                            differing += usize::from(change.is_some());
                            if let Some(change) = change {
                                assert_eq!(change.to, now);
                            }
                        }
                    }
                }
                assert_eq!(differing, ghost.changes.len(), "{op:?}");
                non_empty += usize::from(differing > 0);
                // An Edit that changed the Planet calls for a new ghost; one that did not leaves it.
                assert_eq!(world.ghost().is_none(), differing > 0);
            }
        }
        // Rounding off a flat wall has nothing to do, but the others do.
        assert!(non_empty >= 9, "{non_empty}");
    }

    #[test]
    fn the_ghost_follows_the_aim_and_goes_when_nothing_is_aimed_at() {
        let (mut world, _server) = world();
        let tool = Tool::default();
        assert!(world.ghost().is_none());
        world.show_ghost(&camera(), &tool);
        let first = world.ghost().unwrap().op;
        // Aimed a little to the side, it is a different Edit.
        let aside = Camera::looking_at([4016.5, 16.5, 20.5], [4026.5, 16.5, 0.0]);
        world.show_ghost(&aside, &tool);
        assert_ne!(world.ghost().unwrap().op, first);
        let up = Camera::looking_at([4016.5, 16.5, 20.5], [4016.5, 100.0, 20.5]);
        world.show_ghost(&up, &tool);
        assert!(world.ghost().is_none());
        world.show_ghost(&camera(), &tool);
        world.hide_ghost();
        assert!(world.ghost().is_none());
    }

    #[test]
    fn placing_beyond_the_ball_the_scene_is_clipped_to_grows_it_and_the_new_voxels_are_drawn() {
        let mut store = wall();
        store.build_lod(4);
        // The wall reaches to x = 4031, and the ball a little past that.
        let bound = render::scene::Bound {
            centre: [0; 3],
            radius: 4040,
        };
        let eye = Camera::looking_at([4060.5, 16.5, -15.5], [4000.0, 16.5, -15.5]);
        let scene = Scene::rings(
            &store,
            eye.position,
            render::lod::Bands::with_d0(64.0),
            bound,
        )
        .unwrap();
        let (client, _server) = loopback();
        let mut world = World::new(scene, Predictor::new(store), client);
        let far = Camera::looking_at([4100.5, 16.5, -15.5], [4000.0, 16.5, -15.5]);
        let stops = |world: &World| centre_hit(world.scene(), &far).map(|hit| hit.voxel[0]);
        assert_eq!(stops(&world), Some(4031));

        // A cube of 41 Voxels a side on the wall's +x face: its far side is at x = 4052.
        let tool = Tool {
            radius: 20,
            ..place_cube()
        };
        world.edit(&eye, &tool).unwrap();
        let grown = world.scene().bound.unwrap();
        assert!(grown.radius > 4052, "{grown:?}");
        assert_eq!(
            stops(&world),
            Some(4052),
            "the new Voxels are not clipped away"
        );
    }

    #[test]
    fn a_click_on_the_sky_does_nothing() {
        let (mut world, server) = world();
        let up = Camera::looking_at([4016.5, 16.5, 20.5], [4016.5, 100.0, 20.5]);
        assert!(world.edit(&up, &Tool::default()).unwrap().is_none());
        assert_eq!(server.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn an_ack_keeps_the_hole_and_a_reject_fills_it_again() {
        let (mut world, server) = world();
        let hit = |world: &World| centre_hit(world.scene(), &camera()).unwrap().voxel;
        let whole = hit(&world);
        world.edit(&camera(), &Tool::default()).unwrap();
        let first = hit(&world);
        world.edit(&camera(), &Tool::default()).unwrap();
        let second = hit(&world);
        assert!(whole != first && first != second);
        let Ok(ClientMessage::Edit { op, .. }) = server.try_recv() else {
            panic!("no Edit sent");
        };

        server
            .send(ServerMessage::Applied {
                global_seq: 1,
                op,
                ack: Some(0),
            })
            .unwrap();
        assert!(world.poll().unwrap().is_none(), "the prediction was right");
        assert_eq!(hit(&world), second);

        // The second dig was predicted on top of the first; rejecting it leaves the first.
        server
            .send(ServerMessage::Rejected {
                client_seq: 1,
                reason: RejectReason::BrushTooLarge,
            })
            .unwrap();
        assert!(world.poll().unwrap().is_some());
        assert_eq!(hit(&world), first);
    }

    #[test]
    fn a_server_that_is_gone_is_an_error() {
        let (mut world, server) = world();
        drop(server);
        assert_eq!(world.poll().err(), Some(Stopped::Disconnected));
        assert_eq!(
            world.edit(&camera(), &Tool::default()).err(),
            Some(Stopped::Disconnected)
        );
    }
}
