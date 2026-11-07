//! A CPU prefetch simulator (#87): replays camera paths through a synthetic brick cache to size
//! the 360° shell, its margins, the Guard and the generation and intake rates that #49 must
//! build to, without a GPU.
//!
//! [`model`] is the world (the Planet as a sphere of surface bricks, the band rule from
//! [`render::lod`], the margins and the Guard), [`path`] the camera paths, [`demand`] what a
//! path asks for per frame (the required set, the shell and prefetch), [`cache`] the generation
//! and intake queues and the eviction policy it is replayed through, and [`report`] the paths of
//! the ticket and the table of their results. `cargo run --release -p prefetch-sim` prints it.

pub mod cache;
pub mod demand;
pub mod key;
pub mod model;
pub mod path;
pub mod report;
