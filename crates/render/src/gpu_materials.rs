//! The Material table packed for the GPU: what the shaders shade from.
//!
//! One record per id, found by indexing with the id, so a Voxel's Material is one load away.
//! Ids the table does not define are all zeros, which the shader reads (by `defined`, below) as
//! "unknown": magenta, and no light. A record is four `vec4<f32>`:
//!
//! | vec4 | x, y, z                         | w                |
//! |------|---------------------------------|------------------|
//! | 0    | base colour (linear RGB)        | roughness        |
//! | 1    | noise scale (Voxels), hue jitter (radians), value jitter | metalness |
//! | 2    | emitted light: colour times intensity | unused     |
//! | 3    | transparency, IOR, `defined` (1 if the table has the id) | unused |
//!
//! The ids are 16 bits, so the table is at most 64 Ki records of 64 bytes. Hardness, density and
//! the Fluid kind are for the simulation, not the shaders, so they are not here. Keep the layout
//! in step with `materials` in `common.wgsl`.

use materials::MaterialTable;

/// `vec4<f32>`s per Material record.
pub const RECORD_VEC4S: usize = 4;

/// The records for `table`: `max id + 1` of them, or one empty one if it defines nothing.
pub fn pack(table: &MaterialTable) -> Vec<[f32; 4]> {
    let records = table.defs().last().map_or(1, |d| usize::from(d.id) + 1);
    let mut packed = vec![[0.0; 4]; records * RECORD_VEC4S];
    for d in table.defs() {
        let [r, g, b] = d.colour;
        let [er, eg, eb] = d.emissive.map(|c| c * d.emissive_intensity);
        packed[usize::from(d.id) * RECORD_VEC4S..][..RECORD_VEC4S].copy_from_slice(&[
            [r, g, b, d.roughness],
            [
                d.noise_scale,
                d.hue_jitter.to_radians(),
                d.value_jitter,
                d.metalness,
            ],
            [er, eg, eb, 0.0],
            [d.transparency, d.ior, 1.0, 0.0],
        ]);
    }
    packed
}

#[cfg(test)]
mod tests {
    use brick_store::Material;

    use super::*;

    fn record(packed: &[[f32; 4]], id: u16) -> &[[f32; 4]] {
        &packed[usize::from(id) * RECORD_VEC4S..][..RECORD_VEC4S]
    }

    #[test]
    fn a_record_holds_the_materials_numbers_at_its_id() {
        let table = MaterialTable::built_in();
        let packed = pack(table);
        let lava = table.by_name("lava").unwrap();
        let r = record(&packed, lava.id);
        assert_eq!(
            r[0],
            [
                lava.colour[0],
                lava.colour[1],
                lava.colour[2],
                lava.roughness
            ]
        );
        assert_eq!(r[1][0], lava.noise_scale);
        assert!((r[1][1] - lava.hue_jitter.to_radians()).abs() < 1e-6);
        assert_eq!(r[1][2], lava.value_jitter);
        // Emitted light is colour times intensity.
        assert_eq!(r[2][0], lava.emissive[0] * lava.emissive_intensity);
        assert_eq!(r[3], [lava.transparency, lava.ior, 1.0, 0.0]);
    }

    #[test]
    fn rock_is_the_colour_the_hardcoded_shader_gave_it() {
        let packed = pack(MaterialTable::built_in());
        assert_eq!(record(&packed, 1)[0][..3], [0.45, 0.43, 0.40]);
        assert_eq!(record(&packed, 2)[0][..3], [0.30, 0.50, 0.20]);
    }

    #[test]
    fn ids_the_table_does_not_define_are_all_zeros_so_they_read_as_unknown() {
        // The shipped table starts at 1, so 0 is a gap below its Materials.
        assert!(MaterialTable::built_in().get(Material(0)).is_none());
        let packed = pack(MaterialTable::built_in());
        assert!(record(&packed, 0).iter().all(|v| *v == [0.0; 4]));
    }

    #[test]
    fn the_table_ends_at_the_highest_id() {
        let table = MaterialTable::built_in();
        let highest = table.defs().last().unwrap().id;
        assert_eq!(pack(table).len(), (usize::from(highest) + 1) * RECORD_VEC4S);
    }
}
