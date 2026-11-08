//! Sparse 8³ brick store for the Planet (ADR 0002). CPU-only: no GPU types live here.

mod brick;
mod coord;
mod lod;
mod store;

pub use brick::{BRICK_VOXELS, Brick, BrickView, Material, VoxelMask};
pub use coord::{BRICK_SIZE, BrickCoord, LocalCoord, VOXELS_PER_METRE, VoxelCoord};
pub use lod::{
    BrickSource, Interior, children_from, children_generated, children_to_generate,
    children_with_solid,
};
pub use store::{BrickStore, Budget, Candidate, LodRefresh, LodWork, TableSize};
