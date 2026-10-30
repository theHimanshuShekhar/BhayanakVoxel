//! The client's view of the Planet: the bricks it predicts on, the packed copy that gets drawn,
//! and the loopback to the server. All Edits come through here, and reach the Voxels only via
//! [`sim::Predictor`], which calls [`sim::apply`].

use brick_store::VoxelCoord;
use net::{ClientEnd, Disconnected, EditOp, TryRecvError};
use render::reference::{Hit, Ray, trace};
use render::{Camera, RingsFull, Scene, SceneUpdate, camera};
use sim::Predictor;

/// Radius of the sphere a click digs: 1 m at 10 cm Voxels.
const DIG_RADIUS: u16 = 10;

pub struct World {
    scene: Scene,
    predictor: Predictor,
    link: ClientEnd,
}

/// The first solid Voxel along the camera's view through the middle of the screen.
fn centre_hit(scene: &Scene, camera: &Camera) -> Option<Hit> {
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
        }
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// Moves the rings of the Planet to centre on `camera`, a position in Voxels. Returns how
    /// the scene changed, if it did.
    pub fn recentre(&mut self, camera: [f64; 3]) -> Result<Option<SceneUpdate>, RingsFull> {
        self.scene.recentre(self.predictor.store(), camera)
    }

    /// Digs a sphere where the camera looks: shows it at once, and asks the server for it.
    /// Returns how the scene changed, if there was anything to dig.
    pub fn dig(&mut self, camera: &Camera) -> Result<Option<SceneUpdate>, Disconnected> {
        let Some(hit) = centre_hit(&self.scene, camera) else {
            return Ok(None);
        };
        let op = EditOp::dig_sphere(VoxelCoord(hit.voxel), DIG_RADIUS);
        let (request, changed) = self.predictor.edit(op);
        self.link.send(request)?;
        Ok(self.refresh(&changed))
    }

    /// Takes in what the server has sent. Returns how the scene changed, if it did.
    pub fn poll(&mut self) -> Result<Option<SceneUpdate>, Disconnected> {
        let mut changed = Vec::new();
        loop {
            match self.link.try_recv() {
                Ok(message) => changed.extend(self.predictor.receive(message)),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Err(Disconnected),
            }
        }
        Ok(self.refresh(&changed))
    }

    /// Packs the bricks that changed.
    fn refresh(&mut self, changed: &[brick_store::BrickCoord]) -> Option<SceneUpdate> {
        (!changed.is_empty()).then(|| self.scene.update(self.predictor.store(), changed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use brick_store::{Brick, BrickCoord, BrickStore, Material};
    use net::{ClientMessage, RejectReason, ServerEnd, ServerMessage, loopback};

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
        let update = world.dig(&camera()).unwrap();
        assert!(
            matches!(update, Some(SceneUpdate::Patched { .. })),
            "{update:?}"
        );
        // The wall's front face is z = -1; the view's middle is Voxel (4016, 16, -1).
        assert_eq!(
            server.try_recv(),
            Ok(ClientMessage::Edit {
                client_seq: 0,
                op: EditOp::dig_sphere(VoxelCoord([4016, 16, -1]), DIG_RADIUS)
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
        let scene = Scene::rings(&store, camera().position, 5).unwrap();
        let (client, _server) = loopback();
        let mut world = World::new(scene, Predictor::new(store), client);

        let update = world.dig(&camera()).unwrap();
        assert!(matches!(update, Some(SceneUpdate::Rings(_))), "{update:?}");
        let deeper = centre_hit(world.scene(), &camera()).unwrap();
        assert!(deeper.voxel[2] < -1, "{deeper:?}");

        // Where the camera is, the rings stay; far away, they move.
        assert_eq!(world.recentre(camera().position), Ok(None));
        let far = [camera().position[0] + 900.0, 16.5, 20.5];
        assert!(matches!(
            world.recentre(far),
            Ok(Some(SceneUpdate::Rings(_)))
        ));
    }

    #[test]
    fn a_click_on_the_sky_does_nothing() {
        let (mut world, server) = world();
        let up = Camera::looking_at([4016.5, 16.5, 20.5], [4016.5, 100.0, 20.5]);
        assert!(world.dig(&up).unwrap().is_none());
        assert_eq!(server.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn an_ack_keeps_the_hole_and_a_reject_fills_it_again() {
        let (mut world, server) = world();
        let hit = |world: &World| centre_hit(world.scene(), &camera()).unwrap().voxel;
        let whole = hit(&world);
        world.dig(&camera()).unwrap();
        let first = hit(&world);
        world.dig(&camera()).unwrap();
        let second = hit(&world);
        assert!(whole != first && first != second);
        let ClientMessage::Edit { op, .. } = server.try_recv().unwrap();

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
        assert_eq!(world.poll().err(), Some(Disconnected));
        assert_eq!(world.dig(&camera()).err(), Some(Disconnected));
    }
}
