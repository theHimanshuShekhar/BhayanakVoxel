//! Sparse 8³ brick store for the Planet (ADR 0002). CPU-only: no GPU types live here.

mod brick;
mod coord;
mod lod;
mod store;

pub use brick::{BRICK_VOXELS, Brick, BrickView, Material};
pub use coord::{BRICK_SIZE, BrickCoord, LocalCoord, VoxelCoord};
pub use lod::{BrickSource, Interior};
pub use store::{BrickStore, LodRefresh};
