//! The tunable numbers of character movement, read from `movement.toml` (decision in #16).
//!
//! The file is in metres and seconds, as a designer thinks of them. [`Movement`] holds the same
//! numbers in Voxels, which the controller works in; converting is a multiplication by 10, so
//! it is exact in the sense that matters: the client and the server get the same bits from the
//! same file.

use std::error::Error;
use std::fmt;
use std::sync::OnceLock;

use serde::Deserialize;

/// The data file the game ships with, compiled in so that every binary has it.
const BUILT_IN: &str = include_str!("../movement.toml");

/// Voxels in a metre: Voxels are 10 cm.
pub const VOXELS_PER_METRE: f64 = 10.0;

/// The file as written, in metres and seconds.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MovementConfig {
    /// m/s², towards the Planet's centre.
    pub gravity: f64,
    pub walk_speed: f64,
    pub run_speed: f64,
    pub crouch_speed: f64,
    /// How high a jump goes, in m.
    pub jump_height: f64,
    /// Highest step walked up without jumping, in m.
    pub step_height: f64,
    /// Capsule height standing, in m.
    pub height: f64,
    /// Capsule height crouching, in m.
    pub crouch_height: f64,
    /// Capsule radius, in m.
    pub radius: f64,
    pub eye_height: f64,
    pub crouch_eye_height: f64,
    /// m/s² towards the wanted horizontal velocity while in the air.
    pub air_acceleration: f64,
    /// Fastest fall, in m/s.
    pub terminal_speed: f64,
}

/// Why a data file could not be loaded.
#[derive(Debug)]
pub enum ConfigError {
    /// Not valid TOML, or a number is missing, unknown or of the wrong type.
    Malformed(toml::de::Error),
    /// A number is outside what it can be.
    OutOfRange {
        property: &'static str,
        allowed: &'static str,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(e) => write!(f, "malformed movement config: {e}"),
            Self::OutOfRange { property, allowed } => {
                write!(f, "movement config: {property} must be {allowed}")
            }
        }
    }
}

impl Error for ConfigError {}

impl MovementConfig {
    /// Reads a data file. Every number must be there and in range.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let config: Self = toml::from_str(text).map_err(ConfigError::Malformed)?;
        config.check()?;
        Ok(config)
    }

    /// The config the game ships with.
    ///
    /// # Panics
    ///
    /// If `movement.toml` is invalid, which a test rules out.
    pub fn built_in() -> Self {
        static CONFIG: OnceLock<MovementConfig> = OnceLock::new();
        *CONFIG.get_or_init(|| Self::parse(BUILT_IN).expect("movement.toml is valid"))
    }

    fn check(&self) -> Result<(), ConfigError> {
        let positive = |v: f64| v.is_finite() && v > 0.0;
        let checks = [
            (positive(self.gravity), "gravity", "above 0"),
            (positive(self.walk_speed), "walk_speed", "above 0"),
            (positive(self.run_speed), "run_speed", "above 0"),
            (positive(self.crouch_speed), "crouch_speed", "above 0"),
            (positive(self.jump_height), "jump_height", "above 0"),
            (positive(self.step_height), "step_height", "above 0"),
            (positive(self.radius), "radius", "above 0"),
            (
                positive(self.crouch_height) && self.crouch_height >= 2.0 * self.radius,
                "crouch_height",
                "at least the capsule's diameter",
            ),
            (
                positive(self.height) && self.height >= self.crouch_height,
                "height",
                "at least crouch_height",
            ),
            (
                positive(self.eye_height) && self.eye_height <= self.height,
                "eye_height",
                "above 0 and within height",
            ),
            (
                positive(self.crouch_eye_height) && self.crouch_eye_height <= self.crouch_height,
                "crouch_eye_height",
                "above 0 and within crouch_height",
            ),
            (
                positive(self.air_acceleration),
                "air_acceleration",
                "above 0",
            ),
            (positive(self.terminal_speed), "terminal_speed", "above 0"),
        ];
        match checks.into_iter().find(|(ok, ..)| !ok) {
            Some((_, property, allowed)) => Err(ConfigError::OutOfRange { property, allowed }),
            None => Ok(()),
        }
    }
}

/// The movement numbers in Voxels and seconds, with what follows from them worked out. Only
/// `+ - * /` and `sqrt` are used to make it, so every machine gets the same bits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Movement {
    /// Voxels/s².
    pub gravity: f64,
    pub walk_speed: f64,
    pub run_speed: f64,
    pub crouch_speed: f64,
    /// Upward speed at take-off for the wanted height: `sqrt(2 g h)`.
    pub jump_speed: f64,
    pub step_height: f64,
    pub height: f64,
    pub crouch_height: f64,
    pub radius: f64,
    pub eye_height: f64,
    pub crouch_eye_height: f64,
    pub air_acceleration: f64,
    pub terminal_speed: f64,
}

impl Movement {
    pub fn new(config: &MovementConfig) -> Self {
        let v = |metres: f64| metres * VOXELS_PER_METRE;
        let gravity = v(config.gravity);
        Self {
            gravity,
            walk_speed: v(config.walk_speed),
            run_speed: v(config.run_speed),
            crouch_speed: v(config.crouch_speed),
            jump_speed: (2.0 * gravity * v(config.jump_height)).sqrt(),
            step_height: v(config.step_height),
            height: v(config.height),
            crouch_height: v(config.crouch_height),
            radius: v(config.radius),
            eye_height: v(config.eye_height),
            crouch_eye_height: v(config.crouch_eye_height),
            air_acceleration: v(config.air_acceleration),
            terminal_speed: v(config.terminal_speed),
        }
    }
}

impl Default for Movement {
    /// The shipped numbers.
    fn default() -> Self {
        Self::new(&MovementConfig::built_in())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_config_has_the_defaults_of_decision_16() {
        let c = MovementConfig::built_in();
        assert_eq!(
            (c.gravity, c.walk_speed, c.run_speed, c.crouch_speed),
            (9.8, 4.5, 7.0, 2.0)
        );
        assert_eq!((c.jump_height, c.step_height), (1.2, 0.5));
        assert_eq!((c.height, c.radius), (1.8, 0.3));
    }

    #[test]
    fn movement_is_the_config_in_voxels() {
        let m = Movement::default();
        assert_eq!(
            (m.walk_speed, m.run_speed, m.crouch_speed),
            (45.0, 70.0, 20.0)
        );
        assert_eq!((m.step_height, m.height, m.radius), (5.0, 18.0, 3.0));
        // Leaves the ground at the speed that takes it 1.2 m up against 9.8 m/s^2.
        assert!((m.jump_speed - 48.5).abs() < 0.1, "{}", m.jump_speed);
    }

    fn with(line: &str) -> String {
        format!("{BUILT_IN}\n{line}\n")
    }

    #[test]
    fn a_duplicate_unknown_or_missing_number_is_rejected() {
        assert!(matches!(
            MovementConfig::parse(&with("walk_speed = 1.0")),
            Err(ConfigError::Malformed(_))
        ));
        assert!(matches!(
            MovementConfig::parse(&with("flying_speed = 1.0")),
            Err(ConfigError::Malformed(_))
        ));
        assert!(matches!(
            MovementConfig::parse("gravity = 9.8"),
            Err(ConfigError::Malformed(_))
        ));
    }

    #[test]
    fn numbers_out_of_range_are_named() {
        for (from, to, property) in [
            ("gravity = 9.8", "gravity = 0.0", "gravity"),
            ("walk_speed = 4.5", "walk_speed = -1.0", "walk_speed"),
            (
                "crouch_height = 1.2",
                "crouch_height = 0.5",
                "crouch_height",
            ),
            ("height = 1.8", "height = 1.0", "height"),
            ("eye_height = 1.6", "eye_height = 2.0", "eye_height"),
            (
                "terminal_speed = 50.0",
                "terminal_speed = nan",
                "terminal_speed",
            ),
        ] {
            let text = BUILT_IN.replace(from, to);
            assert_ne!(text, BUILT_IN, "{from}");
            match MovementConfig::parse(&text) {
                Err(ConfigError::OutOfRange { property: p, .. }) => assert_eq!(p, property),
                other => panic!("{property}: {other:?}"),
            }
        }
    }
}
