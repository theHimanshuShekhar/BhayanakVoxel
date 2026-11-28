//! The player's settings (#50): what is kept between launches.
//!
//! The only one so far is **how rays find the Planet**: hardware ray queries, which are the default
//! wherever the adapter has them (the owner, 2026-11-21), or the compute walk, for a GPU that ray
//! queries do not suit. The overlay changes it at run time and the window saves it at once.
//!
//! It is one small TOML file, `settings.toml`, in the platform's per-user config directory:
//! `%LOCALAPPDATA%\BhayanakVoxel\` on Windows (not the roaming `%APPDATA%`: the setting depends on
//! this machine's GPU, so it must not follow the player to another one), `$XDG_CONFIG_HOME/BhayanakVoxel/` or
//! `~/.config/BhayanakVoxel/` elsewhere. `BHAYANAK_SETTINGS` names another file (for a test, or a
//! machine with no home). A file that is missing is the defaults; one that cannot be read or parsed
//! is the defaults too, with a warning, and is left as it is until the player changes a setting. A key that
//! this version does not know is ignored, and is not kept when the file is written again.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use render::RtBackend;
use serde::Deserialize;

/// The file's name, in the config directory.
pub const FILE_NAME: &str = "settings.toml";
/// The config directory's name, in the platform's per-user one.
const DIRECTORY: &str = "BhayanakVoxel";
/// The environment variable that names the settings file.
pub const PATH_VARIABLE: &str = "BHAYANAK_SETTINGS";

/// How rays find the Planet, as the player's setting says.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RayTracing {
    /// Ray queries where the adapter has them, and the compute walk where it has not. The default.
    #[default]
    Hardware,
    /// The compute walk, always.
    Compute,
}

impl RayTracing {
    /// The backend to ask the renderer for.
    pub fn backend(self) -> RtBackend {
        match self {
            Self::Hardware => RtBackend::Hardware,
            Self::Compute => RtBackend::Compute,
        }
    }

    /// The setting for the backend the overlay chose (a nearest-hit hardware backend is hardware).
    pub fn of(backend: RtBackend) -> Self {
        if backend.is_hardware() {
            Self::Hardware
        } else {
            Self::Compute
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Hardware => "hardware",
            Self::Compute => "compute",
        }
    }
}

/// What is kept between launches.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    pub ray_tracing: RayTracing,
}

impl Settings {
    /// The settings in `text`: a key that is left out is its default.
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// The file's text.
    pub fn to_text(&self) -> String {
        format!(
            "# BhayanakVoxel's settings; the game rewrites this file when one changes.\n\
             # ray_tracing: \"hardware\" (ray queries where the GPU has them, the default) or\n\
             # \"compute\" (the compute-shader walk, for a GPU that ray queries do not suit).\n\
             ray_tracing = \"{}\"\n",
            self.ray_tracing.name()
        )
    }

    /// Reads the settings from `path`: the defaults if there is no file, and the defaults with a
    /// warning (what went wrong) if it cannot be read or is not valid.
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return (Self::default(), None),
            Err(e) => {
                return (
                    Self::default(),
                    Some(format!("cannot read {}: {e}", path.display())),
                );
            }
        };
        match Self::parse(&text) {
            Ok(settings) => (settings, None),
            Err(e) => (
                Self::default(),
                Some(format!("{} is not valid: {e}", path.display())),
            ),
        }
    }

    /// Writes the settings to `path`, in a file of its own that replaces the old one in one step,
    /// so a crash never leaves half a file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(directory) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(directory)?;
        }
        // A name of this process's own, so that two games saving at once do not write one file.
        let mut temporary: OsString = path.as_os_str().to_owned();
        temporary.push(format!(".{}.tmp", std::process::id()));
        let temporary = PathBuf::from(temporary);
        let written = (|| {
            let mut file = fs::File::create(&temporary)?;
            file.write_all(self.to_text().as_bytes())?;
            // On the disk before the rename, so that a crash leaves the old file or the new one.
            file.sync_all()
        })();
        written
            .and_then(|()| fs::rename(&temporary, path))
            .inspect_err(|_| {
                let _ = fs::remove_file(&temporary);
            })
    }
}

/// Where the settings file is, given the environment (`var` reads a variable): the variable's
/// own path if it is set, else the per-user config directory. `None` where there is no home to
/// keep a file in.
pub fn path_in(var: impl Fn(&str) -> Option<OsString>, windows: bool) -> Option<PathBuf> {
    let set = |name: &str| var(name).filter(|v| !v.is_empty());
    if let Some(path) = set(PATH_VARIABLE) {
        return Some(PathBuf::from(path));
    }
    let directory = if windows {
        PathBuf::from(set("LOCALAPPDATA")?)
    } else if let Some(config) = set("XDG_CONFIG_HOME") {
        PathBuf::from(config)
    } else {
        PathBuf::from(set("HOME")?).join(".config")
    };
    Some(directory.join(DIRECTORY).join(FILE_NAME))
}

/// Where this machine keeps the settings file (see [`path_in`]).
pub fn path() -> Option<PathBuf> {
    path_in(|name| std::env::var_os(name), cfg!(windows))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of the test's own in the system's temporary one, which is gone when it is dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("bhayanak-settings-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn hardware_is_the_default_and_no_file_is_the_defaults_with_no_warning() {
        assert_eq!(Settings::default().ray_tracing, RayTracing::Hardware);
        assert_eq!(
            Settings::default().ray_tracing.backend(),
            RtBackend::Hardware
        );
        let scratch = Scratch::new("missing");
        let (settings, warning) = Settings::load(&scratch.0.join(FILE_NAME));
        assert_eq!(settings, Settings::default());
        assert_eq!(warning, None);
    }

    #[test]
    fn a_setting_persists_across_launches() {
        let scratch = Scratch::new("persists");
        // A directory that does not exist yet is made.
        let path = scratch.0.join("config").join(FILE_NAME);
        let compute = Settings {
            ray_tracing: RayTracing::Compute,
        };
        compute.save(&path).unwrap();
        // The next launch reads it, and the backend it asks the renderer for is compute.
        let (loaded, warning) = Settings::load(&path);
        assert_eq!(loaded, compute);
        assert_eq!(warning, None);
        assert_eq!(loaded.ray_tracing.backend(), RtBackend::Compute);
        // And back: the file is replaced, and no temporary file is left.
        Settings::default().save(&path).unwrap();
        assert_eq!(Settings::load(&path).0.ray_tracing, RayTracing::Hardware);
        let files: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(files, [OsString::from(FILE_NAME)]);
    }

    #[test]
    fn a_file_that_does_not_parse_is_the_defaults_with_a_warning_and_is_left_alone() {
        let scratch = Scratch::new("broken");
        let path = scratch.0.join(FILE_NAME);
        for text in ["ray_tracing = \"raytraced\"\n", "ray_tracing = ", "[[[["] {
            fs::write(&path, text).unwrap();
            let (settings, warning) = Settings::load(&path);
            assert_eq!(settings, Settings::default(), "{text}");
            assert!(
                warning.is_some_and(|w| w.contains("is not valid")),
                "{text}"
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), text, "left alone");
        }
    }

    #[test]
    fn a_key_that_is_left_out_is_its_default_and_one_that_is_not_known_is_ignored() {
        assert_eq!(Settings::parse("").unwrap(), Settings::default());
        let parsed = Settings::parse("ray_tracing = \"compute\"\nfuture = 3\n").unwrap();
        assert_eq!(parsed.ray_tracing, RayTracing::Compute);
    }

    #[test]
    fn the_overlays_backend_maps_to_the_setting_and_back() {
        assert_eq!(RayTracing::of(RtBackend::Compute), RayTracing::Compute);
        assert_eq!(RayTracing::of(RtBackend::Hardware), RayTracing::Hardware);
        assert_eq!(
            RayTracing::of(RtBackend::HardwareNearest),
            RayTracing::Hardware
        );
    }

    #[test]
    fn the_file_is_where_the_variable_says_else_in_the_platforms_config_directory() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        assert_eq!(
            path_in(
                env(&[(PATH_VARIABLE, "/tmp/a.toml"), ("HOME", "/home/p")]),
                false
            ),
            Some(PathBuf::from("/tmp/a.toml"))
        );
        assert_eq!(
            path_in(env(&[("HOME", "/home/p")]), false),
            Some(PathBuf::from("/home/p/.config/BhayanakVoxel/settings.toml"))
        );
        assert_eq!(
            path_in(
                env(&[("XDG_CONFIG_HOME", "/x"), ("HOME", "/home/p")]),
                false
            ),
            Some(PathBuf::from("/x/BhayanakVoxel/settings.toml"))
        );
        assert_eq!(
            path_in(
                env(&[
                    ("LOCALAPPDATA", "C:/Users/p/AppData/Local"),
                    ("APPDATA", "C:/Users/p/AppData/Roaming")
                ]),
                true
            ),
            Some(PathBuf::from(
                "C:/Users/p/AppData/Local/BhayanakVoxel/settings.toml"
            ))
        );
        assert_eq!(
            path_in(env(&[("APPDATA", "C:/Users/p/AppData/Roaming")]), true),
            None
        );
        assert_eq!(path_in(env(&[]), false), None);
        assert_eq!(path_in(env(&[("HOME", "")]), false), None);
    }
}
