//! The client's view of the Planet: the bricks it predicts on, the packed copy that gets drawn,
//! and the loopback to the server. All Edits come through here, and reach the Voxels only via
//! [`sim::Predictor`], which calls [`sim::apply`].

use brick_store::{Material, VoxelCoord};
use materials::MaterialTable;
use net::{ClientEnd, Disconnected, EditMode, EditOp, MoveInput, TryRecvError};
use render::reference::{Hit, Ray, trace};
use render::{Camera, RingsFull, Scene, SceneUpdate, camera};
use sim::Predictor;

use crate::dig::dig_seconds;
use crate::ghost::Ghost;
use crate::loader::{Arrival, Loader};
use crate::tool::{Tool, rotation_for};

/// What the frame thread aims to spend taking in bricks from the loader in one frame, the upload
/// to the GPU included. A resident set that is filling takes more frames, not longer ones.
pub const STREAM_BUDGET: std::time::Duration = std::time::Duration::from_micros(2000);
/// Bricks' children taken in at a time, between looks at the clock.
const BATCH: usize = 8;
/// Most bricks asked of the loader in one frame (the rest wait for the next): the first call has
/// every brick the Guard leaves to split, and the table that holds them is not made in a frame.
const WANTED_PER_FRAME: usize = 20_000;

/// A ghost that took some time to make is not made again for a new Edit for this many times as
/// long, and the last one is shown until then: so the frame thread spends at most a ninth of its
/// time on it, whatever the Brush, while the cursor moves.
const GHOST_BACKOFF: u32 = 8;

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
    /// tool to. Made again when the Edit it is of changes, or a brick it reads does.
    ghost: Option<Ghost>,
    /// Not before this is the ghost made again for a new Edit: see [`GHOST_BACKOFF`].
    ghost_next: Option<std::time::Instant>,
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
            ghost_next: None,
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

    /// The Edit that `tool` makes where the camera looks: at the first solid Voxel along the view
    /// or, to place, with the Brush sitting on the face that was hit (the Voxel in front of it,
    /// and then as far again out from the face as the Brush reaches). The Brush is turned to face
    /// the way the camera looks (see [`rotation_for`]). `None` if there is no Voxel, or the centre
    /// is further than [`AIM_REACH`] from the camera.
    pub fn aim(&self, camera: &Camera, tool: &Tool) -> Option<EditOp> {
        self.aim_hit(camera, tool).map(|(op, _)| op)
    }

    /// The Dig that holding the button would make, and how long it takes: by the hardness of the
    /// Material at the Voxel aimed at (see [`dig_seconds`]). `None` if the tool is not a Dig, or
    /// there is nothing to aim at.
    pub fn dig_aim(&self, camera: &Camera, tool: &Tool) -> Option<(EditOp, f64)> {
        if tool.mode != EditMode::Dig {
            return None;
        }
        let (op, hit) = self.aim_hit(camera, tool)?;
        let hardness = MaterialTable::built_in()
            .get(Material(hit.material))
            .map_or(0.0, |def| def.hardness);
        Some((op, dig_seconds(hardness)))
    }

    /// [`World::aim`] with the Voxel that the view hit.
    fn aim_hit(&self, camera: &Camera, tool: &Tool) -> Option<(EditOp, Hit)> {
        let hit = centre_hit(&self.scene, camera)?;
        let out = if tool.mode == EditMode::Place {
            i32::from(tool.radius) + 1
        } else {
            0
        };
        let centre: [i32; 3] = std::array::from_fn(|a| hit.voxel[a] + hit.normal[a] * out);
        let away: f64 = (0..3)
            .map(|a| {
                let d = f64::from(centre[a]) + 0.5 - camera.position[a];
                d * d
            })
            .sum();
        if away > AIM_REACH * AIM_REACH {
            return None;
        }
        let centre = VoxelCoord(centre);
        let rotation = rotation_for(centre, tool.alignment, camera.forward());
        Some((tool.op(centre, rotation), hit))
    }

    /// Makes the ghost for where the camera looks, if there is a Voxel to apply `tool` to. It is
    /// kept while the Edit stays the same and the Planet where it reads stays as it is. When it
    /// has to be made again it is not at once if the last took long (see [`GHOST_BACKOFF`]).
    pub fn show_ghost(&mut self, camera: &Camera, tool: &Tool) {
        self.show_ghost_at(camera, tool, std::time::Instant::now());
    }

    /// [`World::show_ghost`] at the time `now`.
    fn show_ghost_at(&mut self, camera: &Camera, tool: &Tool, now: std::time::Instant) {
        let Some(op) = self.aim(camera, tool) else {
            self.ghost = None;
            return;
        };
        let foreseen = self.predictor.foresee(&op);
        let stale = self.ghost.as_ref().is_none_or(|g| g.op != op || g.dirty);
        let waiting = self.ghost.is_some() && self.ghost_next.is_some_and(|next| now < next);
        if stale && !waiting {
            let started = std::time::Instant::now();
            let changes = self.predictor.plan(&op);
            self.ghost = Some(Ghost::new(op, changes));
            self.ghost_next = Some(now + started.elapsed() * GHOST_BACKOFF);
        }
        if let Some(ghost) = &mut self.ghost {
            // Grey if the server is known to turn it down: for what the op says, or for the
            // Voxels that the ghost shows it would change.
            ghost.blocked = foreseen.is_err() || !self.predictor.can_change(ghost.changed);
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
    /// Returns how the scene changed, if there was anything to apply it to. An Edit that the
    /// client knows the server will turn down (see [`sim::Predictor::foresee`]) is not made.
    pub fn edit(&mut self, camera: &Camera, tool: &Tool) -> Result<Option<SceneUpdate>, Stopped> {
        let Some(op) = self.aim(camera, tool) else {
            return Ok(None);
        };
        let Ok((request, changed)) = self.predictor.edit_checked(op) else {
            return Ok(None);
        };
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
        // What the ghost would change has changed with them, if they are where it reads.
        if let Some(ghost) = &mut self.ghost {
            ghost.dirty |= ghost.touches(changed);
        }
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
                op: EditOp {
                    // Looking along the wall's face, level with the horizon, the Brush is turned to
                    // face that way: up is +x here, and the view is down -z.
                    rotation: rotation_for(
                        VoxelCoord([4016, 16, -1]),
                        net::Alignment::Local,
                        camera().forward()
                    ),
                    ..EditOp::dig_sphere(VoxelCoord([4016, 16, -1]), crate::tool::DEFAULT_RADIUS)
                }
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
    fn placing_sits_the_brush_on_the_face_that_is_aimed_at() {
        let (mut world, server) = world();
        let update = world.edit(&camera(), &place_cube()).unwrap();
        assert!(update.is_some());
        // The wall's front face is z = -1, so the air Voxel in front of it is z = 0, and a cube of
        // radius 4 that sits on the face is centred 4 further out.
        let sent = server.try_recv();
        let Ok(ClientMessage::Edit { op, .. }) = sent else {
            panic!("{sent:?}");
        };
        assert_eq!(op.centre, VoxelCoord([4016, 16, 4]));
        assert_eq!((op.mode, op.material), (EditMode::Place, Material(35)));
        // It fills z = 0 to 8, all of it in the air, and none of it in the wall.
        assert_eq!(
            world.store().get(VoxelCoord([4016, 16, 0])),
            Some(Material(35))
        );
        assert_eq!(
            world.store().get(VoxelCoord([4016, 16, 8])),
            Some(Material(35))
        );
        assert_eq!(world.store().get(VoxelCoord([4016, 16, 9])), None);
        assert_eq!(
            world.store().get(VoxelCoord([4016, 16, -1])),
            Some(Material(7))
        );
        // And it is drawn: the same ray now stops where the cube's face is.
        let hit = centre_hit(world.scene(), &camera()).unwrap();
        assert_eq!(hit.voxel[2], 8);
    }

    #[test]
    fn the_brush_is_turned_to_face_the_way_the_camera_looks_and_not_on_the_grid() {
        let (world, _server) = world();
        let tool = place_cube();
        let op = world.aim(&camera(), &tool).unwrap();
        assert_eq!(
            op.rotation,
            rotation_for(op.centre, tool.alignment, camera().forward())
        );
        // Seen from the side, it is the Voxels on that side that it faces.
        let side = Camera::looking_at([4040.5, 16.5, 20.5], [4016.5, 16.5, 0.0]);
        let from_the_side = world.aim(&side, &tool).unwrap();
        assert_eq!(
            from_the_side.rotation,
            rotation_for(from_the_side.centre, tool.alignment, side.forward())
        );
        let grid = Tool {
            alignment: net::Alignment::World,
            ..tool
        };
        assert_eq!(world.aim(&side, &grid).unwrap().rotation, 0);
    }

    #[test]
    fn a_dig_takes_the_time_of_the_hardness_of_the_material_aimed_at() {
        let table = MaterialTable::built_in();
        let (world, _server) = world();
        let tool = Tool::default();
        let (op, seconds) = world.dig_aim(&camera(), &tool).unwrap();
        assert_eq!(Some(op), world.aim(&camera(), &tool));
        let hit = centre_hit(world.scene(), &camera()).unwrap();
        let hardness = table.get(Material(hit.material)).unwrap().hardness;
        assert_eq!(seconds, dig_seconds(hardness));
        // The other tools are instant: a hold does not dig for them.
        let place = Tool {
            mode: EditMode::Place,
            ..tool
        };
        assert_eq!(world.dig_aim(&camera(), &place), None);
    }

    #[test]
    fn nothing_is_aimed_at_beyond_the_reach() {
        let (mut world, server) = world();
        let far = Camera::looking_at([4016.5, 16.5, 120.5], [4016.5, 16.5, 0.0]);
        assert_eq!(world.aim(&far, &Tool::default()), None);
        assert!(world.edit(&far, &Tool::default()).unwrap().is_none());
        assert_eq!(server.try_recv(), Err(TryRecvError::Empty));
        // Just inside it, there is. To place, the Brush's centre is nearer than what is aimed at.
        let near = Camera::looking_at([4016.5, 16.5, 70.5], [4016.5, 16.5, 0.0]);
        assert!(world.aim(&near, &Tool::default()).is_some());
        assert!(world.aim(&near, &place_cube()).is_some());
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
                // An Edit that changed the Planet where the ghost reads calls for a new ghost.
                assert_eq!(world.ghost().unwrap().dirty, differing > 0);
            }
        }
        // Rounding off a flat wall has nothing to do, but the others do.
        assert!(non_empty >= 9, "{non_empty}");
    }

    #[test]
    fn the_ghost_follows_the_aim_and_goes_when_nothing_is_aimed_at() {
        let (mut world, _server) = world();
        let tool = Tool::default();
        let start = std::time::Instant::now();
        let later = |millis| start + std::time::Duration::from_millis(millis);
        assert!(world.ghost().is_none());
        world.show_ghost_at(&camera(), &tool, start);
        let first = world.ghost().unwrap().op;
        // Aimed a little to the side, it is a different Edit.
        let aside = Camera::looking_at([4016.5, 16.5, 20.5], [4026.5, 16.5, 0.0]);
        world.show_ghost_at(&aside, &tool, later(10_000));
        assert_ne!(world.ghost().unwrap().op, first);
        let up = Camera::looking_at([4016.5, 16.5, 20.5], [4016.5, 100.0, 20.5]);
        world.show_ghost_at(&up, &tool, later(20_000));
        assert!(world.ghost().is_none());
        world.show_ghost_at(&camera(), &tool, later(30_000));
        world.hide_ghost();
        assert!(world.ghost().is_none());
    }

    #[test]
    fn a_ghost_that_took_long_to_make_is_not_made_again_for_a_new_aim_until_a_while_has_gone() {
        let (mut world, _server) = world();
        let tool = Tool::default();
        let start = std::time::Instant::now();
        let later = |millis| start + std::time::Duration::from_millis(millis);
        world.show_ghost_at(&camera(), &tool, start);
        let first = world.ghost().unwrap().op;
        // Say that it took 10 ms: the next is not before 8 times that.
        world.ghost_next = Some(later(80));
        let aside = Camera::looking_at([4016.5, 16.5, 20.5], [4026.5, 16.5, 0.0]);
        world.show_ghost_at(&aside, &tool, later(40));
        assert_eq!(world.ghost().unwrap().op, first, "the last one is shown");
        world.show_ghost_at(&aside, &tool, later(81));
        assert_ne!(world.ghost().unwrap().op, first);
        // Nothing aimed at takes the ghost away at once, whatever the time.
        let up = Camera::looking_at([4016.5, 16.5, 20.5], [4016.5, 100.0, 20.5]);
        world.show_ghost_at(&up, &tool, later(82));
        assert!(world.ghost().is_none());
    }

    #[test]
    fn a_ghost_is_made_again_for_a_change_where_it_reads_and_not_for_one_elsewhere() {
        let (mut world, _server) = world();
        let tool = Tool::default();
        let start = std::time::Instant::now();
        world.show_ghost_at(&camera(), &tool, start);
        let ghost = world.ghost().unwrap().op;
        // A brick far from the sphere of 1 m at (4016, 16, -1): the ghost is as it was, and not
        // asked to be made again.
        world.refresh(&[BrickCoord([500, 0, -4])]).unwrap();
        assert!(!world.ghost().unwrap().dirty);
        // One it reads: it is, when the time allows.
        world.refresh(&[BrickCoord([502, 1, -1])]).unwrap();
        assert!(world.ghost().unwrap().dirty);
        let waits = start + std::time::Duration::from_millis(1);
        world.ghost_next = Some(start + std::time::Duration::from_secs(1));
        world.show_ghost_at(&camera(), &tool, waits);
        assert!(world.ghost().unwrap().dirty, "not yet");
        world.show_ghost_at(&camera(), &tool, start + std::time::Duration::from_secs(2));
        assert!(!world.ghost().unwrap().dirty);
        assert_eq!(world.ghost().unwrap().op, ghost);
    }

    #[test]
    fn an_edit_that_is_over_the_allowance_is_not_made_and_its_ghost_is_grey() {
        let (mut world, server) = world();
        // The smallest Brush, so that it is the 20 Edits a second and not the Voxels that run out.
        let tool = Tool {
            radius: 0,
            ..Tool::default()
        };
        world.show_ghost(&camera(), &tool);
        assert!(!world.ghost().unwrap().blocked);
        // 20 Edits in a second are the most; the 21st is not made, nor sent.
        for _ in 0..20 {
            assert!(world.edit(&camera(), &tool).unwrap().is_some());
        }
        assert!(world.edit(&camera(), &tool).unwrap().is_none());
        let mut sent = 0;
        while let Ok(ClientMessage::Edit { .. }) = server.try_recv() {
            sent += 1;
        }
        assert_eq!(sent, 20);
        world.show_ghost_at(
            &camera(),
            &tool,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        );
        assert!(world.ghost().unwrap().blocked);
        // The character's ticks bring the allowance back, three for an Edit.
        for _ in 0..3 {
            world.walk(MoveInput::default()).unwrap();
        }
        assert!(world.edit(&camera(), &tool).unwrap().is_some());
    }

    #[test]
    fn an_edit_that_would_change_more_voxels_than_are_left_is_not_made_and_its_ghost_is_grey() {
        let (mut world, server) = world();
        // A cube of 41 Voxels a side, on the wall's face, in the air: 68 921 Voxels of the 72 000.
        let tool = Tool {
            mode: EditMode::Place,
            shape: Shape::Cube,
            radius: 20,
            material: Material(35),
            alignment: net::Alignment::World,
        };
        let back = Camera::looking_at([4016.5, 16.5, 60.5], [4016.5, 16.5, 0.0]);
        world.show_ghost(&back, &tool);
        assert!(!world.ghost().unwrap().blocked);
        assert!(world.edit(&back, &tool).unwrap().is_some());
        // Another on the face of that one would change as many, and there are 3 079 left.
        world.show_ghost_at(
            &back,
            &tool,
            std::time::Instant::now() + std::time::Duration::from_secs(5),
        );
        let ghost = world.ghost().unwrap();
        assert!(ghost.changed > 3_079 && ghost.blocked, "{}", ghost.changed);
        assert!(world.edit(&back, &tool).unwrap().is_none());
        let mut sent = 0;
        while let Ok(ClientMessage::Edit { .. }) = server.try_recv() {
            sent += 1;
        }
        assert_eq!(sent, 1);
        // A small one fits, and is made.
        let small = Tool { radius: 4, ..tool };
        assert!(world.edit(&back, &small).unwrap().is_some());
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

        // A cube of 41 Voxels a side sitting on the wall's +x face: its far side is at x = 4072.
        let tool = Tool {
            radius: 20,
            ..place_cube()
        };
        world.edit(&eye, &tool).unwrap();
        let grown = world.scene().bound.unwrap();
        assert!(grown.radius > 4072, "{grown:?}");
        assert_eq!(
            stops(&world),
            Some(4072),
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
