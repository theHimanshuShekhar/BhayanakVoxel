//! Sparse 8³ brick store for the Planet (ADR 0002). CPU-only: no GPU types live here.

mod brick;
mod coord;
mod store;

pub use brick::{BRICK_VOXELS, Brick, Material};
pub use coord::{BRICK_SIZE, BrickCoord, LocalCoord, VoxelCoord};
pub use store::BrickStore;
