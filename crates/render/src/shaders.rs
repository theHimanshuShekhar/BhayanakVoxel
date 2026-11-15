//! WGSL sources. WGSL has no includes, so a module's source is `trace.wgsl` (and, for the
//! kernels that draw, `common.wgsl`) joined with the file that uses it; the upscaler needs
//! neither. Every module is
//! validated by naga in the tests below, with no GPU needed.

/// `trace(ray) -> Hit`, the one entry point the rest of the renderer calls.
#[cfg(test)]
const TRACE: &str = include_str!("shaders/trace.wgsl");

/// Primary visibility: the compute kernel that shades one ray per pixel.
pub const PRIMARY: &str = concat!(
    include_str!("shaders/trace.wgsl"),
    "\n",
    include_str!("shaders/common.wgsl"),
    "\n",
    include_str!("shaders/primary.wgsl")
);

/// The reference path tracer: the compute kernel that adds one sample per pixel.
pub const PATHTRACE: &str = concat!(
    include_str!("shaders/trace.wgsl"),
    "\n",
    include_str!("shaders/common.wgsl"),
    "\n",
    include_str!("shaders/sampling.wgsl"),
    "\n",
    include_str!("shaders/pathtrace.wgsl")
);

/// Random numbers and the distributions the path tracer samples. It needs nothing else, so tests
/// can compile it alone.
#[cfg(test)]
pub const SAMPLING: &str = include_str!("shaders/sampling.wgsl");

/// The temporal upscaler: the compute kernel that builds the output image from the traced one.
pub const UPSCALE: &str = include_str!("shaders/upscale.wgsl");

/// Copies the traced image to the window.
pub const BLIT: &str = include_str!("shaders/blit.wgsl");

/// Casts caller-supplied rays through `trace()`.
pub const PROBE: &str = concat!(
    include_str!("shaders/trace.wgsl"),
    "\n",
    include_str!("shaders/probe.wgsl")
);

#[cfg(test)]
mod tests {
    use naga::valid::{Capabilities, ValidationFlags, Validator};

    use super::*;

    /// Parses and validates `source`, returning its entry point names.
    fn entry_points(source: &str) -> Vec<String> {
        let module = naga::front::wgsl::parse_str(source)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(source)));
        Validator::new(ValidationFlags::all(), Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{}", e.emit_to_string(source)));
        module.entry_points.iter().map(|e| e.name.clone()).collect()
    }

    #[test]
    fn the_primary_kernel_is_valid_wgsl_and_calls_trace() {
        assert_eq!(entry_points(PRIMARY), ["primary"]);
        assert!(PRIMARY.contains("fn trace(ray: Ray) -> Hit"));
        assert!(PRIMARY.contains("trace(ray)"));
    }

    #[test]
    fn the_reference_kernel_is_valid_wgsl_and_sees_the_planet_only_through_trace() {
        assert_eq!(entry_points(PATHTRACE), ["reference"]);
        let kernel = include_str!("shaders/pathtrace.wgsl");
        // One call for the path's rays and one for the shadow rays.
        assert!(kernel.matches("trace(").count() >= 2);
        // Nothing of how the Planet is stored, so a hardware ray tracing backend can replace
        // trace() without touching the lighting.
        for storage in ["scene.", "table[", "regions[", "brick_refs", "bricks["] {
            assert!(!kernel.contains(storage), "pathtrace.wgsl reads {storage}");
        }
    }

    #[test]
    fn the_sampling_module_is_valid_wgsl_on_its_own() {
        // Only helper functions: it validates, and has no entry point.
        assert_eq!(entry_points(SAMPLING), Vec::<String>::new());
    }

    #[test]
    fn the_upscale_kernel_is_valid_wgsl_and_does_not_touch_the_planet() {
        assert_eq!(entry_points(UPSCALE), ["upscale_pass"]);
        assert!(!UPSCALE.contains("trace("));
    }

    #[test]
    fn the_blit_shader_is_valid_wgsl() {
        assert_eq!(entry_points(BLIT), ["vertex", "fragment"]);
    }

    #[test]
    fn the_probe_kernel_is_valid_wgsl() {
        assert_eq!(entry_points(PROBE), ["probe"]);
    }

    #[test]
    fn the_shader_and_the_scene_agree_on_how_many_levels_there_can_be() {
        let line = format!("const MAX_LEVELS = {};", crate::scene::MAX_LEVELS);
        assert!(TRACE.contains(&line), "trace.wgsl lacks {line:?}");
    }

    // -----------------------------------------------------------------------------------------
    // The contract's table of lighting features (ADR 0005, "Lighting features").
    //
    // How features are listed: a line of a view's shader that reads a source of light carries
    // (or follows, in its comment block) a `// lighting: <feature>` comment, naming the feature
    // by its row in the ADR's table. This checks both ways: every feature a shader names has a
    // row, and every source of light that the shader reads, which `READS` lists by the identifier
    // that reads it, is named by such a comment. So a PR that adds a lighting feature to Primary
    // has to say what it is in the shader, and the test then asks for its row in the ADR: it adds
    // the row and the feature to the Reference view, or marks it absent with a reason. A feature
    // that reads a new kind of source adds it to `READS` here (a shader that uses an identifier
    // that is not in the list and is not marked is the one thing this cannot see; review is
    // what is left for it).

    const ADR: &str = include_str!("../../../docs/adr/0005-reference-view-contract.md");

    /// The lighting features of the ADR's table, lower-cased, with their states.
    fn table_rows() -> Vec<(String, String)> {
        let table = ADR
            .split("### Lighting features")
            .nth(1)
            .expect("the ADR has a lighting features section")
            .split("\n### ")
            .next()
            .unwrap();
        table
            .lines()
            .filter(|l| l.starts_with('|'))
            .skip(2) // the header and its rule
            .map(|l| {
                let cells: Vec<&str> = l.trim_matches('|').split('|').map(str::trim).collect();
                (cells[0].to_lowercase(), cells[1].to_lowercase())
            })
            .collect()
    }

    /// What reads a source of light, and the feature it is: an identifier of the shaders.
    const READS: [(&str, &str); 5] = [
        ("frame.sun", "sun and shadows"),
        ("sky(", "sky and atmosphere"),
        ("emission(", "emissive voxels"),
        ("surface_albedo(", "per-voxel colour variation"),
        ("varied_albedo(", "per-voxel colour variation"),
    ];

    /// The problems with `source` against `rows`: a feature it names that has no row, and a
    /// source of light it reads that it does not name.
    fn lighting_problems(source: &str, rows: &[(String, String)]) -> Vec<String> {
        let mut problems = Vec::new();
        let mut named = Vec::new();
        for line in source.lines() {
            if let Some(feature) = line.trim().strip_prefix("// lighting:") {
                let feature = feature.trim().to_lowercase();
                if !rows.iter().any(|(row, _)| *row == feature) {
                    problems.push(format!("`{feature}` has no row in ADR 0005's table"));
                }
                named.push(feature);
            }
        }
        for (identifier, feature) in READS {
            let reads = source.lines().any(|l| {
                let code = l.trim();
                !code.starts_with("//") && code.contains(identifier) && !code.starts_with("fn ")
            });
            if reads && !named.iter().any(|n| n == feature) {
                problems.push(format!(
                    "reads `{identifier}` and does not say `// lighting: {feature}`"
                ));
            }
        }
        problems
    }

    #[test]
    fn the_adrs_table_has_the_rows_the_views_are_held_to() {
        let rows = table_rows();
        assert!(rows.len() >= 10, "{rows:?}");
        for (_, feature) in READS {
            assert!(
                rows.iter().any(|(row, _)| row == feature),
                "no row for {feature}"
            );
        }
        for (feature, state) in &rows {
            assert!(
                ["exact", "approximate, shared model", "shared", "absent"]
                    .contains(&state.as_str()),
                "{feature}: state {state:?}"
            );
        }
    }

    #[test]
    fn every_lighting_feature_in_primary_and_the_reference_has_a_row_in_the_adrs_table() {
        let rows = table_rows();
        for (name, source) in [
            ("primary.wgsl", include_str!("shaders/primary.wgsl")),
            ("pathtrace.wgsl", include_str!("shaders/pathtrace.wgsl")),
        ] {
            let problems = lighting_problems(source, &rows);
            assert!(problems.is_empty(), "{name}: {problems:?}");
        }
        // And Primary really names some: the check is not passing on an empty shader.
        let named = include_str!("shaders/primary.wgsl")
            .matches("// lighting:")
            .count();
        assert!(named >= 4, "{named}");
    }

    #[test]
    fn a_feature_primary_has_is_in_the_reference_unless_the_table_says_it_is_shared() {
        // The table's rule for how a feature enters: in the same PR, the Reference view gets it, or
        // the table marks it absent (and then Primary may not have it either, until it is
        // exact or approximate with a shared model).
        let named = |source: &str| -> Vec<String> {
            source
                .lines()
                .filter_map(|l| l.trim().strip_prefix("// lighting:"))
                .map(|f| f.trim().to_lowercase())
                .collect()
        };
        let reference = named(include_str!("shaders/pathtrace.wgsl"));
        for feature in named(include_str!("shaders/primary.wgsl")) {
            let state = table_rows()
                .into_iter()
                .find(|(row, _)| *row == feature)
                .map(|(_, state)| state)
                .unwrap_or_else(|| panic!("`{feature}` has no row"));
            assert_ne!(
                state, "absent",
                "Primary has `{feature}`, which the table calls absent"
            );
            assert!(
                state == "shared" || reference.contains(&feature),
                "Primary has `{feature}` ({state}) and the Reference view does not"
            );
        }
    }

    #[test]
    fn a_lighting_feature_with_no_row_fails_the_check() {
        let rows = table_rows();
        // A feature that Primary's shader names and the table does not have.
        let ambient = "// lighting: Ambient occlusion\nlet ao = 1.0;";
        assert_eq!(
            lighting_problems(ambient, &rows),
            ["`ambient occlusion` has no row in ADR 0005's table"]
        );
        // A source of light that is read and not named.
        let unmarked = "fn shade() {\n    return sky(dir);\n}";
        assert_eq!(
            lighting_problems(unmarked, &rows),
            ["reads `sky(` and does not say `// lighting: sky and atmosphere`"]
        );
        // The same with the name: it passes.
        let marked = "// lighting: Sky and atmosphere\nreturn sky(dir);";
        assert!(lighting_problems(marked, &rows).is_empty());
    }

    #[test]
    fn primary_and_the_reference_take_their_colours_from_the_one_function() {
        // Neither view has a colour function of its own: both call `surface_albedo`, which is
        // defined once, in the code they share, and which is the only caller of `varied_albedo`.
        let common = include_str!("shaders/common.wgsl");
        assert_eq!(common.matches("fn surface_albedo(").count(), 1);
        assert_eq!(common.matches("fn varied_albedo(").count(), 1);
        for (name, source) in [
            ("primary.wgsl", include_str!("shaders/primary.wgsl")),
            ("pathtrace.wgsl", include_str!("shaders/pathtrace.wgsl")),
        ] {
            assert!(
                source.contains("surface_albedo("),
                "{name} does not call it"
            );
            assert!(
                !source.contains("fn surface_albedo("),
                "{name} defines its own"
            );
            assert!(
                !source.contains("varied_albedo("),
                "{name} has its own colours"
            );
            assert!(
                !source.contains("albedo(hit.material"),
                "{name} reads a base colour"
            );
        }
        assert_eq!(common.matches("varied_albedo(hit.material").count(), 1);
    }

    #[test]
    fn trace_is_defined_exactly_once() {
        assert_eq!(TRACE.matches("fn trace(").count(), 1);
        for other in [
            include_str!("shaders/common.wgsl"),
            include_str!("shaders/sampling.wgsl"),
            include_str!("shaders/pathtrace.wgsl"),
            include_str!("shaders/primary.wgsl"),
            include_str!("shaders/probe.wgsl"),
            UPSCALE,
            BLIT,
        ] {
            assert_eq!(other.matches("fn trace(").count(), 0);
        }
    }
}
