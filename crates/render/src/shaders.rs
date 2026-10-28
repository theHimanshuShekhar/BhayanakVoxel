//! WGSL sources. WGSL has no includes, so a module's source is `trace.wgsl` joined with the file
//! that uses it. Every module is validated by naga in the tests below, with no GPU needed.

/// `trace(ray) -> Hit`, the one entry point the rest of the renderer calls.
#[cfg(test)]
const TRACE: &str = include_str!("shaders/trace.wgsl");

/// Primary visibility: the compute kernel that shades one ray per pixel.
pub const PRIMARY: &str = concat!(
    include_str!("shaders/trace.wgsl"),
    "\n",
    include_str!("shaders/primary.wgsl")
);

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
    fn the_blit_shader_is_valid_wgsl() {
        assert_eq!(entry_points(BLIT), ["vertex", "fragment"]);
    }

    #[test]
    fn the_probe_kernel_is_valid_wgsl() {
        assert_eq!(entry_points(PROBE), ["probe"]);
    }

    #[test]
    fn trace_is_defined_exactly_once() {
        assert_eq!(TRACE.matches("fn trace(").count(), 1);
        for other in [
            include_str!("shaders/primary.wgsl"),
            include_str!("shaders/probe.wgsl"),
            BLIT,
        ] {
            assert_eq!(other.matches("fn trace(").count(), 0);
        }
    }
}
