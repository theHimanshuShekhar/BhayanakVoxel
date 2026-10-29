//! The Materials of the game, defined in one data file (`materials.toml`, decision in #15).
//!
//! `planet-gen`, `sim` and `render` all read the same [`MaterialTable`], so a Material is added by
//! adding a table to the file and nothing else. CPU-only: no GPU types live here, so the server
//! can link it. The renderer packs the table for the GPU itself.
//!
//! A Voxel stores only a [`Material`] id (`brick-store`); everything about what that Material
//! looks like or how it behaves is looked up here.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::sync::OnceLock;

use brick_store::Material;
use serde::Deserialize;

/// The data file the game ships with, compiled in so that every binary has it.
const BUILT_IN: &str = include_str!("../materials.toml");

/// Which part of the palette a Material belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Natural,
    Building,
    Light,
}

/// How a Material flows once Fluids exist. Static in M1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FluidKind {
    None,
    Water,
    Sand,
    Lava,
}

/// One Material's properties, as written in the data file. Colours are linear RGB.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialDef {
    /// The id Voxels store. Stable: never renumbered or reused.
    pub id: u16,
    pub name: String,
    pub category: Category,
    /// Base albedo, each channel 0..=1.
    pub colour: [f32; 3],
    /// Edge, in Voxels, of the patches the variation changes over. Above zero.
    pub noise_scale: f32,
    /// Most the hue is turned either way per patch, in degrees: up to half a turn (180).
    pub hue_jitter: f32,
    /// Most the brightness is scaled either way per patch, as a fraction (0..=1).
    pub value_jitter: f32,
    /// 0 mirror-smooth to 1 fully diffuse.
    pub roughness: f32,
    /// 0 dielectric to 1 metal. Stored now; only M1b shades it.
    pub metalness: f32,
    /// Colour of the light given off, each channel 0..=1.
    pub emissive: [f32; 3],
    /// Multiplier on `emissive`; zero for a Material that gives off no light.
    pub emissive_intensity: f32,
    /// 0 opaque to 1 clear. Stored now; only M1b shades it.
    pub transparency: f32,
    /// Index of refraction, at least 1. Stored now; only M1b shades it.
    pub ior: f32,
    /// How long it takes to dig: 0 (none) to 10 (slowest).
    pub hardness: f32,
    /// In kg/m³: the mass of Debris made of it.
    pub density: f32,
    pub fluid: FluidKind,
}

/// The shape of the data file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    material: Vec<MaterialDef>,
}

/// Why a data file could not be loaded.
#[derive(Debug)]
pub enum LoadError {
    /// Not valid TOML, or a table is missing a property, has an unknown one, or one of the wrong
    /// type or out of range for it (for instance an id above 65535).
    Malformed(toml::de::Error),
    /// Two Materials share an id.
    DuplicateId {
        id: u16,
        first: String,
        second: String,
    },
    /// Two Materials have names that differ at most in case.
    DuplicateName { first: String, second: String },
    /// A property is outside what it can be.
    OutOfRange {
        material: String,
        property: &'static str,
        allowed: &'static str,
    },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(e) => write!(f, "malformed material table: {e}"),
            Self::DuplicateId { id, first, second } => {
                write!(
                    f,
                    "materials \"{first}\" and \"{second}\" both have id {id}"
                )
            }
            Self::DuplicateName { first, second } => write!(
                f,
                "materials \"{first}\" and \"{second}\" have the same name, ignoring case"
            ),
            Self::OutOfRange {
                material,
                property,
                allowed,
            } => write!(f, "material \"{material}\": {property} must be {allowed}"),
        }
    }
}

impl Error for LoadError {}

/// Checks one Material's numbers; the first one wrong is the error. NaN and infinity are in no
/// range.
fn check(def: &MaterialDef) -> Result<(), LoadError> {
    let between = |v: f32, lo: f32, hi: f32| (lo..=hi).contains(&v);
    let unit = |v: f32| between(v, 0.0, 1.0);
    let at_least = |v: f32, lo: f32| between(v, lo, f32::MAX);
    let checks = [
        (
            !def.name.trim().is_empty(),
            "name",
            "at least one character",
        ),
        (
            def.colour.into_iter().all(unit),
            "colour",
            "three numbers from 0 to 1",
        ),
        (
            def.emissive.into_iter().all(unit),
            "emissive",
            "three numbers from 0 to 1",
        ),
        (
            at_least(def.noise_scale, f32::MIN_POSITIVE),
            "noise_scale",
            "above 0",
        ),
        (
            between(def.hue_jitter, 0.0, 180.0),
            "hue_jitter",
            "from 0 to 180",
        ),
        (unit(def.value_jitter), "value_jitter", "from 0 to 1"),
        (unit(def.roughness), "roughness", "from 0 to 1"),
        (unit(def.metalness), "metalness", "from 0 to 1"),
        (
            at_least(def.emissive_intensity, 0.0),
            "emissive_intensity",
            "at least 0",
        ),
        (unit(def.transparency), "transparency", "from 0 to 1"),
        (at_least(def.ior, 1.0), "ior", "at least 1"),
        (between(def.hardness, 0.0, 10.0), "hardness", "from 0 to 10"),
        (at_least(def.density, 0.0), "density", "at least 0"),
    ];
    match checks.into_iter().find(|(ok, ..)| !ok) {
        Some((_, property, allowed)) => Err(LoadError::OutOfRange {
            material: def.name.clone(),
            property,
            allowed,
        }),
        None => Ok(()),
    }
}

/// Every Material, by id.
#[derive(Clone, Debug)]
pub struct MaterialTable {
    /// Ascending by id.
    defs: Vec<MaterialDef>,
}

impl MaterialTable {
    /// Reads a data file. Ids and names must be unique and every number in range.
    pub fn parse(text: &str) -> Result<Self, LoadError> {
        let File { material: mut defs } = toml::from_str(text).map_err(LoadError::Malformed)?;
        defs.sort_by_key(|d| d.id);
        // Lower-cased name to the name as written, so that "Rock" and "rock" collide.
        let mut names = BTreeMap::new();
        for (i, def) in defs.iter().enumerate() {
            if let Some(first) = i
                .checked_sub(1)
                .map(|p| &defs[p])
                .filter(|p| p.id == def.id)
            {
                return Err(LoadError::DuplicateId {
                    id: def.id,
                    first: first.name.clone(),
                    second: def.name.clone(),
                });
            }
            check(def)?;
            if let Some(first) = names.insert(def.name.to_lowercase(), def.name.as_str()) {
                return Err(LoadError::DuplicateName {
                    first: first.to_string(),
                    second: def.name.clone(),
                });
            }
        }
        Ok(Self { defs })
    }

    /// The table the game ships with, parsed on first use.
    ///
    /// # Panics
    ///
    /// If `materials.toml` is invalid, which a test rules out.
    pub fn built_in() -> &'static Self {
        static TABLE: OnceLock<MaterialTable> = OnceLock::new();
        TABLE.get_or_init(|| Self::parse(BUILT_IN).expect("materials.toml is valid"))
    }

    /// The Material with this id, or `None` if the table does not define it.
    pub fn get(&self, material: Material) -> Option<&MaterialDef> {
        self.defs
            .binary_search_by_key(&material.0, |d| d.id)
            .ok()
            .map(|i| &self.defs[i])
    }

    /// The Material called `name`, if any.
    pub fn by_name(&self, name: &str) -> Option<&MaterialDef> {
        self.defs.iter().find(|d| d.name == name)
    }

    /// Every Material, ascending by id.
    pub fn defs(&self) -> &[MaterialDef] {
        &self.defs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `[[material]]` table with every property, `extra` lines after them (to test overrides
    /// and unknown properties, which TOML rejects as duplicates or `deny_unknown_fields`).
    fn entry(id: u32, name: &str, extra: &str) -> String {
        format!(
            r#"[[material]]
id = {id}
name = "{name}"
category = "natural"
colour = [0.5, 0.5, 0.5]
noise_scale = 1.0
hue_jitter = 0.0
value_jitter = 0.1
roughness = 0.9
metalness = 0.0
emissive = [0.0, 0.0, 0.0]
emissive_intensity = 0.0
transparency = 0.0
ior = 1.0
hardness = 6.0
density = 2700.0
fluid = "none"
{extra}
"#
        )
    }

    /// `entry` with the line starting `property` replaced.
    fn with(property: &str, line: &str) -> String {
        entry(1, "rock", "")
            .lines()
            .map(|l| {
                if l.starts_with(&format!("{property} =")) {
                    line
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn load_error(text: &str) -> String {
        MaterialTable::parse(text).unwrap_err().to_string()
    }

    /// Every id and name of the shipped table. Saved Planets and Edits store ids, so an id keeps
    /// its meaning for good: this list only ever grows. Materials added to the file need no entry.
    const IDS: [(u16, &str); 55] = [
        (1, "rock"),
        (2, "soil"),
        (3, "grassy soil"),
        (4, "clay"),
        (5, "gravel"),
        (6, "sand"),
        (7, "sandstone"),
        (8, "limestone"),
        (9, "shale"),
        (10, "granite"),
        (11, "basalt"),
        (12, "obsidian"),
        (13, "deep rock"),
        (14, "snow"),
        (15, "ice"),
        (16, "molten core"),
        (17, "crystal"),
        (18, "bark"),
        (19, "wood"),
        (20, "leaves"),
        (21, "dry grass"),
        (22, "moss"),
        (23, "flowers"),
        (24, "cactus"),
        (25, "glowing mushroom"),
        (26, "glowing moss"),
        (27, "water"),
        (28, "lava"),
        (29, "red sand"),
        (30, "coal"),
        (31, "mud"),
        (32, "peat"),
        (33, "planks"),
        (34, "dark planks"),
        (35, "stone bricks"),
        (36, "clay bricks"),
        (37, "concrete"),
        (38, "marble"),
        (39, "iron"),
        (40, "steel"),
        (41, "copper"),
        (42, "clear glass"),
        (43, "blue glass"),
        (44, "amber glass"),
        (45, "roof tiles"),
        (46, "white cloth"),
        (47, "red cloth"),
        (48, "rope"),
        (49, "thatch"),
        (50, "white light"),
        (51, "warm light"),
        (52, "red light"),
        (53, "green light"),
        (54, "blue light"),
        (55, "purple light"),
    ];

    #[test]
    fn the_shipped_table_loads() {
        assert_eq!(MaterialTable::built_in().defs().len(), IDS.len());
    }

    #[test]
    fn every_id_keeps_its_meaning() {
        let table = MaterialTable::built_in();
        for (id, name) in IDS {
            let def = table.get(Material(id)).unwrap_or_else(|| panic!("id {id}"));
            assert_eq!(def.name, name, "id {id}");
        }
    }

    #[test]
    fn the_table_has_the_decided_mix_of_materials() {
        let count = |category| {
            MaterialTable::built_in()
                .defs()
                .iter()
                .filter(|d| d.category == category)
                .count()
        };
        assert!((28..=34).contains(&count(Category::Natural)));
        assert!((14..=18).contains(&count(Category::Building)));
        assert!((5..=7).contains(&count(Category::Light)));
    }

    #[test]
    fn water_sand_and_lava_are_placeable_fluids() {
        let table = MaterialTable::built_in();
        for (name, kind) in [
            ("water", FluidKind::Water),
            ("sand", FluidKind::Sand),
            ("lava", FluidKind::Lava),
        ] {
            assert_eq!(table.by_name(name).unwrap().fluid, kind, "{name}");
        }
        assert_eq!(table.by_name("rock").unwrap().fluid, FluidKind::None);
    }

    #[test]
    fn light_sources_crystal_and_the_molten_core_emit_and_rock_does_not() {
        let table = MaterialTable::built_in();
        for def in table.defs() {
            let emits = def.emissive_intensity > 0.0;
            let should = def.category == Category::Light
                || ["crystal", "molten core", "lava"].contains(&def.name.as_str());
            assert!(!should || emits, "{} should emit", def.name);
        }
        assert_eq!(table.by_name("rock").unwrap().emissive_intensity, 0.0);
    }

    #[test]
    fn glass_is_transparent_with_a_refractive_index() {
        let glass = MaterialTable::built_in().by_name("clear glass").unwrap();
        assert!(glass.transparency > 0.9 && glass.ior > 1.4);
    }

    #[test]
    fn an_id_the_table_does_not_define_is_none() {
        assert!(MaterialTable::built_in().get(Material(0)).is_none());
        assert!(MaterialTable::built_in().get(Material(60_000)).is_none());
    }

    #[test]
    fn materials_are_found_by_id_whatever_order_the_file_lists_them() {
        let text = format!("{}{}", entry(9, "nine", ""), entry(3, "three", ""));
        let table = MaterialTable::parse(&text).unwrap();
        assert_eq!(table.get(Material(3)).unwrap().name, "three");
        assert_eq!(table.get(Material(9)).unwrap().name, "nine");
        assert_eq!(table.defs()[0].id, 3);
    }

    #[test]
    fn a_new_material_in_the_file_needs_no_code() {
        let text = format!("{}{}", entry(1, "rock", ""), entry(500, "moonstone", ""));
        let table = MaterialTable::parse(&text).unwrap();
        assert_eq!(table.get(Material(500)).unwrap().name, "moonstone");
    }

    #[test]
    fn text_that_is_not_toml_is_a_malformed_error() {
        let message = load_error("[[material]\nid = ");
        assert!(message.starts_with("malformed material table"), "{message}");
    }

    #[test]
    fn a_missing_property_is_a_malformed_error_naming_it() {
        let text = entry(1, "rock", "").replace("ior = 1.0\n", "");
        let message = load_error(&text);
        assert!(message.starts_with("malformed material table"), "{message}");
        assert!(message.contains("ior"), "{message}");
    }

    #[test]
    fn an_unknown_property_is_a_malformed_error_naming_it() {
        let message = load_error(&entry(1, "rock", "shininess = 3.0"));
        assert!(message.contains("shininess"), "{message}");
    }

    #[test]
    fn an_id_that_does_not_fit_in_16_bits_is_a_malformed_error() {
        let message = load_error(&entry(70_000, "huge", ""));
        assert!(message.starts_with("malformed material table"), "{message}");
    }

    #[test]
    fn an_unknown_fluid_or_category_is_a_malformed_error() {
        assert!(load_error(&with("fluid", "fluid = \"steam\"")).contains("steam"));
        assert!(load_error(&with("category", "category = \"magic\"")).contains("magic"));
    }

    #[test]
    fn a_file_with_no_materials_is_a_malformed_error() {
        assert!(load_error("").starts_with("malformed material table"));
    }

    #[test]
    fn a_duplicate_id_is_an_error_naming_both_materials() {
        let text = format!("{}{}", entry(4, "clay", ""), entry(4, "mud", ""));
        assert_eq!(
            load_error(&text),
            "materials \"clay\" and \"mud\" both have id 4"
        );
    }

    #[test]
    fn a_duplicate_name_is_an_error() {
        let text = format!("{}{}", entry(1, "rock", ""), entry(2, "rock", ""));
        assert_eq!(
            load_error(&text),
            "materials \"rock\" and \"rock\" have the same name, ignoring case"
        );
    }

    #[test]
    fn names_that_differ_only_in_case_are_a_duplicate() {
        let text = format!("{}{}", entry(1, "Rock", ""), entry(2, "rock", ""));
        assert_eq!(
            load_error(&text),
            "materials \"Rock\" and \"rock\" have the same name, ignoring case"
        );
    }

    #[test]
    fn an_empty_name_is_an_error() {
        for name in ["", "  "] {
            assert_eq!(
                load_error(&entry(1, name, "")),
                format!("material \"{name}\": name must be at least one character")
            );
        }
    }

    #[test]
    fn a_hue_jitter_above_half_a_turn_is_an_error() {
        let message = load_error(&with("hue_jitter", "hue_jitter = 180.5"));
        assert!(
            message.contains("hue_jitter must be from 0 to 180"),
            "{message}"
        );
        // Half a turn itself is the most there can be.
        assert!(MaterialTable::parse(&with("hue_jitter", "hue_jitter = 180.0")).is_ok());
    }

    #[test]
    fn a_number_out_of_range_is_an_error_naming_the_material_and_property() {
        for (line, property) in [
            ("colour = [1.5, 0.5, 0.5]", "colour"),
            ("emissive = [0.0, -0.1, 0.0]", "emissive"),
            ("noise_scale = 0.0", "noise_scale"),
            ("value_jitter = 2.0", "value_jitter"),
            ("roughness = -0.5", "roughness"),
            ("metalness = 1.5", "metalness"),
            ("transparency = 3.0", "transparency"),
            ("ior = 0.5", "ior"),
            ("hardness = 11.0", "hardness"),
            ("density = -1.0", "density"),
            ("emissive_intensity = nan", "emissive_intensity"),
            ("noise_scale = inf", "noise_scale"),
        ] {
            let name = line.split(' ').next().unwrap();
            let message = load_error(&with(name, line));
            assert!(
                message.starts_with("material \"rock\":") && message.contains(property),
                "{line}: {message}"
            );
        }
    }
}
