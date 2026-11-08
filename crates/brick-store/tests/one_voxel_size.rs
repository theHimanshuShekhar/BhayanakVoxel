//! The size of a Voxel is written down once (ADR 0007): `brick_store::VOXELS_PER_METRE`. This
//! reads every Rust source file of the workspace and fails on a second definition of it, or on
//! a second name for the same thing.

use std::fs;
use std::path::{Path, PathBuf};

fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn nothing_but_brick_store_defines_how_many_voxels_make_a_metre() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    for dir in ["crates", "bins"] {
        sources(&root.join(dir), &mut files);
    }
    assert!(files.len() > 50, "found only {} sources", files.len());
    let mut definitions = Vec::new();
    for file in files {
        // This file names what it looks for.
        if file.ends_with("brick-store/tests/one_voxel_size.rs") {
            continue;
        }
        let text = fs::read_to_string(&file).unwrap();
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap().to_lowercase();
            let defines = code.contains("const voxels_per_metre")
                || code.contains("static voxels_per_metre")
                || code.contains("const metres_per_voxel")
                || code.contains("const voxel_size");
            if defines {
                definitions.push((file.clone(), n + 1));
            }
        }
    }
    assert_eq!(definitions.len(), 1, "{definitions:#?}");
    assert!(definitions[0].0.ends_with("brick-store/src/coord.rs"));
    assert_eq!(brick_store::VOXELS_PER_METRE, 20.0);
}
