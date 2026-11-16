//! WGSL sources. WGSL has no includes, so a module's source is `trace.wgsl` (and, for the
//! kernels that draw, `common.wgsl`) joined with the file that uses it; the upscaler needs
//! neither. Every module is
//! validated by naga in the tests below, with no GPU needed.

/// `trace(ray) -> Hit`, the one entry point the rest of the renderer calls.
#[cfg(test)]
const TRACE: &str = include_str!("shaders/trace.wgsl");

/// Primary visibility: the compute kernel that casts one ray per pixel and writes the G-buffer.
pub const PRIMARY: &str = concat!(
    include_str!("shaders/trace.wgsl"),
    "\n",
    include_str!("shaders/common.wgsl"),
    "\n",
    include_str!("shaders/primary.wgsl")
);

/// Primary's light: the kernels that compute the smooth normals (#202), cast the sun's shadow rays
/// (#52) and then shade the lit image from the G-buffer, as linear light. It has the sampling
/// module for the sun's samples, which are the Reference's sequence.
pub const SHADE: &str = concat!(
    include_str!("shaders/trace.wgsl"),
    "\n",
    include_str!("shaders/common.wgsl"),
    "\n",
    include_str!("shaders/sampling.wgsl"),
    "\n",
    include_str!("shaders/shade.wgsl")
);

/// The display transform (#202): auto-exposure, bloom and the tone curve, on either view's linear
/// image. It reads no Planet and no light of its own, so it is a module of its own.
pub const POST: &str = include_str!("shaders/post.wgsl");

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
    fn the_shade_kernels_are_valid_wgsl_and_cast_one_ray_a_pixel_in_one_kernel() {
        assert_eq!(entry_points(SHADE), ["normals", "shadows", "shade_pass"]);
        // The smooth normal reads the Planet through `solid_at` of trace.wgsl, and the light is
        // drawn from what primary visibility found: the one ray of Primary's light is the sun's
        // shadow ray, in `shadows`, and nothing else casts one.
        let kernel = include_str!("shaders/shade.wgsl");
        let calls = kernel
            .lines()
            .filter(|l| !l.trim().starts_with("//") && l.contains("trace("))
            .count();
        assert_eq!(calls, 1);
        let shadows = kernel
            .split("fn shadows(")
            .nth(1)
            .and_then(|rest| rest.split("\nfn ").next())
            .expect("a shadows kernel");
        assert!(shadows.contains("trace(leave("));
    }

    #[test]
    fn the_post_module_is_valid_wgsl_and_touches_neither_the_planet_nor_the_accumulation() {
        assert_eq!(
            entry_points(POST),
            [
                "exposure",
                "exposure_adapt",
                "bloom_down",
                "bloom_h",
                "bloom_v",
                "tone"
            ]
        );
        for storage in ["trace(", "scene.", "bricks[", "accum", "frame."] {
            assert!(!POST.contains(storage), "post.wgsl reads {storage}");
        }
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
    // How features are listed: the shader says it. A whole-line `// lighting: <feature>` comment,
    // naming a row of the ADR's table, must stand on one of the two lines directly above (a
    // comment at the end of a line does not count; one comment covers the next two lines, so one
    // term to a line) for:
    //   - every read of a source of light, by the identifiers in `READS` (the sun, the sky, a
    //     Material's emission, the Voxel's colour), and that comment must name that source's
    //     feature;
    //   - every call of `trace(`, which is how light is found (the camera ray, a bounce, a shadow
    //     ray, an ambient-occlusion ray all pass through it), with any row;
    //   - every line of `shade()` (Primary's light) but its control flow, so that a constant term, like the flat 0.25
    //     ambient, has its row too, and every line of `radiance()` that changes `light`.
    // It scans the whole source of each view's kernel, `PRIMARY` and `PATHTRACE`: `common.wgsl`,
    // `sampling.wgsl` and any file that is added to either. A binding that is not in `BINDINGS`
    // fails it (a radiance cache, a light buffer): whoever adds one decides there whether it
    // carries light, and gives it a row. And a shader file that no list knows fails it.
    //
    // What it cannot see: a source of light with its own name in a function that is neither
    // `shade` nor `radiance`, and a marked line whose comment is wrong about the feature (a `trace(`
    // may carry any row). Review is what is left for those.

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

    /// The bindings the shaders have, none of which is a source of light that has no row. A new
    /// one fails the check until it is added here with a decision.
    const BINDINGS: [&str; 17] = [
        "scene",
        "table",
        "regions",
        "brick_refs",
        "bricks",
        "camera",
        "output",
        "frame",
        "materials",
        "accum",
        "gbuffer",
        "surface",
        "shading_normals_out",
        "shading_normals",
        // The sun's light at each hit (`shadows` writes it, `shade_pass` reads it): carries
        // "Sun and shadows", whose row it is covered by.
        "sun_weights_out",
        "sun_weights",
        "probe_rays",
    ];

    /// The shader files, all of them known: a new one is neither of a view nor checked until it is
    /// listed here.
    const FILES: [&str; 10] = [
        "trace.wgsl",
        "common.wgsl",
        "sampling.wgsl",
        "primary.wgsl",
        "shade.wgsl",
        "post.wgsl",
        "pathtrace.wgsl",
        "probe.wgsl",
        "upscale.wgsl",
        "blit.wgsl",
    ];

    /// Whether `line` (trimmed, code) contains `needle` as a call or member, not in `fn needle(`.
    fn reads(line: &str, needle: &str) -> bool {
        !line.starts_with("//")
            && !line.starts_with("fn ")
            && line.match_indices(needle).any(|(at, _)| {
                // not the tail of a longer name: `varied_albedo(` is not a read of `albedo(`.
                !line[..at]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
            })
    }

    /// The shader files that `PRIMARY` and `PATHTRACE` are joined from, in order (a test holds
    /// that they are), so that a problem is reported as `file:line` of the file that has it.
    const PRIMARY_FILES: [(&str, &str); 3] = [
        ("trace.wgsl", include_str!("shaders/trace.wgsl")),
        ("common.wgsl", include_str!("shaders/common.wgsl")),
        ("primary.wgsl", include_str!("shaders/primary.wgsl")),
    ];
    const SHADE_FILES: [(&str, &str); 4] = [
        ("trace.wgsl", include_str!("shaders/trace.wgsl")),
        ("common.wgsl", include_str!("shaders/common.wgsl")),
        ("sampling.wgsl", include_str!("shaders/sampling.wgsl")),
        ("shade.wgsl", include_str!("shaders/shade.wgsl")),
    ];
    const PATHTRACE_FILES: [(&str, &str); 4] = [
        ("trace.wgsl", include_str!("shaders/trace.wgsl")),
        ("common.wgsl", include_str!("shaders/common.wgsl")),
        ("sampling.wgsl", include_str!("shaders/sampling.wgsl")),
        ("pathtrace.wgsl", include_str!("shaders/pathtrace.wgsl")),
    ];

    /// The problems with the `files` of a kernel against `rows`, each as `file:line: ...`.
    fn lighting_problems(files: &[(&str, &str)], rows: &[(String, String)]) -> Vec<String> {
        files
            .iter()
            .flat_map(|(name, source)| file_problems(name, source, rows))
            .collect()
    }

    /// The problems with one file.
    fn file_problems(file: &str, source: &str, rows: &[(String, String)]) -> Vec<String> {
        let raw: Vec<&str> = source.lines().collect();
        let lines: Vec<&str> = raw.iter().map(|l| l.trim()).collect();
        let mut problems = Vec::new();
        // The features named by whole-line comments on the two lines before line `i`.
        let marks = |i: usize| -> Vec<String> {
            (i.saturating_sub(2)..i)
                .filter_map(|j| lines[j].strip_prefix("// lighting:"))
                .map(|f| f.trim().to_lowercase())
                .collect()
        };
        for (i, line) in lines.iter().enumerate() {
            if let Some(feature) = line.strip_prefix("// lighting:") {
                let feature = feature.trim().to_lowercase();
                if !rows.iter().any(|(row, _)| *row == feature) {
                    problems.push(format!(
                        "{file}:{}: `{feature}` has no row in ADR 0005's table",
                        i + 1
                    ));
                }
            }
        }
        let mut in_function = "";
        for (i, line) in lines.iter().enumerate() {
            if let Some(rest) = line.strip_prefix("fn ") {
                in_function = rest.split('(').next().unwrap();
            } else if raw[i] == "}" {
                in_function = "";
            }
            let code = !line.is_empty() && !line.starts_with("//") && *line != "}";
            for (identifier, feature) in READS {
                if reads(line, identifier) && !marks(i).iter().any(|m| m == feature) {
                    problems.push(format!(
                        "{file}:{}: reads `{identifier}` with no whole-line `// lighting: {feature}` \
                         comment on one of the two lines above: {line}",
                        i + 1
                    ));
                }
            }
            let finds_light = reads(line, "trace(");
            let is_light = code
                && ((in_function == "shade"
                    && !line.starts_with("fn ")
                    && !["if ", "} else", "for ", "while ", "loop"]
                        .iter()
                        .any(|c| line.starts_with(c)))
                    || (in_function == "radiance"
                        && ["light +=", "light -=", "light *=", "light ="]
                            .iter()
                            .any(|c| line.starts_with(c))));
            if (finds_light || is_light) && marks(i).is_empty() {
                problems.push(format!(
                    "{file}:{}: light is found or added with no whole-line `// lighting:` comment on \
                     one of the two lines above: {line}",
                    i + 1
                ));
            }
            if line.starts_with("@group(") {
                let name = line
                    .split("var")
                    .nth(1)
                    .and_then(|v| v.split(':').next())
                    .map(|v| v.rsplit(['>', ' ']).next().unwrap_or(v).trim())
                    .unwrap_or("");
                if !BINDINGS.contains(&name) {
                    problems.push(format!(
                        "{file}:{}: the binding `{name}` is not one the check knows; say whether it \
                         carries light",
                        i + 1
                    ));
                }
            }
        }
        problems
    }

    /// `lighting_problems` of one piece of source.
    fn problems_of(source: &str, rows: &[(String, String)]) -> Vec<String> {
        lighting_problems(&[("source", source)], rows)
    }

    #[test]
    fn the_files_that_are_checked_are_the_ones_the_kernels_are_joined_from() {
        let join =
            |files: &[(&str, &str)]| files.iter().map(|(_, s)| *s).collect::<Vec<_>>().join("\n");
        assert_eq!(join(&PRIMARY_FILES), PRIMARY);
        assert_eq!(join(&SHADE_FILES), SHADE);
        assert_eq!(join(&PATHTRACE_FILES), PATHTRACE);
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
                [
                    "exact",
                    "approximate, shared model",
                    "shared",
                    "primary only",
                    "absent"
                ]
                .contains(&state.as_str()),
                "{feature}: state {state:?}"
            );
        }
    }

    #[test]
    fn every_lighting_feature_in_primary_and_the_reference_has_a_row_in_the_adrs_table() {
        let rows = table_rows();
        for (name, files) in [
            ("Primary", &PRIMARY_FILES[..]),
            ("Primary's light", &SHADE_FILES[..]),
            ("the Reference", &PATHTRACE_FILES[..]),
        ] {
            let problems = lighting_problems(files, &rows);
            assert!(problems.is_empty(), "{name}: {problems:#?}");
        }
        // And both really name some: the check is not passing on an empty shader.
        for source in [PRIMARY, SHADE, PATHTRACE] {
            assert!(source.matches("// lighting:").count() >= 4);
        }
    }

    #[test]
    fn every_shader_file_is_one_the_lighting_check_knows() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/shaders");
        for entry in std::fs::read_dir(dir).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            assert!(
                FILES.contains(&name.as_str()),
                "{name} is a new shader: join it to PRIMARY or PATHTRACE, which the lighting check \
                 reads, or list it in FILES as one that lights nothing"
            );
        }
    }

    #[test]
    fn a_feature_primary_has_is_in_the_reference_unless_the_table_says_otherwise() {
        // The table's rule for how a feature enters: in the same PR, the Reference view gets it, or
        // the table says what it is: shared data, or Primary's alone (a stand-in the Reference does
        // not have, and the views that show it are out of the gate). Absent is for features that
        // neither view has, so Primary may not name one.
        let named = |source: &str| -> Vec<String> {
            source
                .lines()
                .filter_map(|l| l.trim().strip_prefix("// lighting:"))
                .map(|f| f.trim().to_lowercase())
                .collect()
        };
        // The kernels' own files, not `common.wgsl`, which both are joined from: a feature marked
        // there would count for the Reference whichever kernel it was for.
        let reference = named(include_str!("shaders/pathtrace.wgsl"));
        let primary = [
            named(include_str!("shaders/primary.wgsl")),
            named(include_str!("shaders/shade.wgsl")),
        ]
        .concat();
        for feature in primary {
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
                state == "shared" || state == "primary only" || reference.contains(&feature),
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
            problems_of(ambient, &rows),
            ["source:1: `ambient occlusion` has no row in ADR 0005's table"]
        );
        // The same with the name: it passes.
        let marked = "// lighting: Sky and atmosphere\nreturn sky(dir);";
        assert!(problems_of(marked, &rows).is_empty());
    }

    #[test]
    fn a_source_of_light_read_without_its_comment_fails_the_check() {
        let rows = table_rows();
        let has = |source: &str, what: &str| {
            let problems = problems_of(source, &rows);
            assert!(
                problems.iter().any(|p| p.contains(what)),
                "{source:?} gives {problems:?}, not {what}"
            );
        };
        // A read that is not marked.
        has("fn f() {\n    return sky(dir);\n}", "reads `sky(`");
        // A second, unmarked read of a source that the shader names elsewhere: the old check
        // passed it, because the name was in the file.
        has(
            "// lighting: Sky and atmosphere\nlet a = sky(d);\nlet x = 1.0;\nlet y = 2.0;\nlet b = sky(e);",
            "source:5: reads `sky(`",
        );
        // A read marked as another feature.
        has(
            "// lighting: Emissive Voxels\nlet a = sky(d);",
            "reads `sky(`",
        );
        // A new ray: ambient occlusion, a shadow ray of another light.
        has("fn shade() {\n    let ao = trace(r);\n}", "light is found");
        has("let ao = trace(r);", "light is found");
        // A constant term of Primary's light, the flat ambient.
        has(
            "fn shade() {\n    let k = 0.25;\n}",
            "light is found or added",
        );
        // A term of the Reference's light.
        has(
            "fn radiance() {\n    light += vec3<f32>(0.1);\n}",
            "light is found or added",
        );
        // A comment at the end of the line is not a whole-line comment: it does not count.
        has(
            "let a = sky(d); // lighting: Sky and atmosphere",
            "source:1: reads `sky(`",
        );
        // A buffer of light: a radiance cache.
        has(
            "@group(1) @binding(9) var<storage, read_write> radiance_cache: array<vec4<f32>>;",
            "the binding `radiance_cache`",
        );
        // The marked versions pass, and a name that only ends like a source does not count.
        for fine in [
            "// lighting: Ambient term\nlet k = 0.25;",
            "// lighting: Geometry (LOD and what is loaded)\nlet h = trace(r);",
            "let c = base_sky(d);",
            "@group(1) @binding(3) var<storage, read_write> accum: array<vec4<f32>>;",
        ] {
            assert_eq!(problems_of(fine, &rows), Vec::<String>::new(), "{fine}");
        }
        // Inside `shade`, one comment covers a line and the two after it, no more.
        has(
            "fn shade() {\n    // lighting: Ambient term\n    let a = 0.25;\n    let b = 1.0;\n    let c = 2.0;\n    let d = 3.0;\n}",
            "source:6: light is found",
        );
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
            ("shade.wgsl", include_str!("shaders/shade.wgsl")),
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
            include_str!("shaders/shade.wgsl"),
            include_str!("shaders/post.wgsl"),
            include_str!("shaders/probe.wgsl"),
            UPSCALE,
            BLIT,
        ] {
            assert_eq!(other.matches("fn trace(").count(), 0);
        }
    }
}
