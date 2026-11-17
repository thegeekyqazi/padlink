//! padlink's config file (R1.23).
//!
//! It lives at `~/.config/padlink/config.toml` (`$XDG_CONFIG_HOME` is
//! honoured). A missing file means the defaults. An unknown key or an
//! invalid value is an error that names the key, so a typo is never
//! silently ignored.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Which video encoder to use (R1.13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoder {
    /// The GPU if it works, then x264, then OpenH264.
    #[default]
    Auto,
    /// The GPU only (`h264_vaapi`): least CPU.
    Vaapi,
    /// x264 on the CPU: can be faster than a small GPU, at a much higher
    /// CPU cost.
    X264,
    /// OpenH264 on the CPU: slowest, always available on Fedora.
    Openh264,
}

/// Which capture protocol to use (SPEC.md section 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Capture {
    /// `wlr` on Hyprland below 0.57, `ext` from 0.57.
    #[default]
    Auto,
    Wlr,
    Ext,
}

/// Where the iPad's display goes relative to the other monitors (R1.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Position {
    #[default]
    Right,
    Left,
    Above,
    Below,
    /// An exact layout position, written `"x,y"`.
    At(i32, i32),
}

impl std::str::FromStr for Position {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "right" => Ok(Position::Right),
            "left" => Ok(Position::Left),
            "above" => Ok(Position::Above),
            "below" => Ok(Position::Below),
            other => other
                .split_once(',')
                .and_then(|(x, y)| {
                    Some(Position::At(x.trim().parse().ok()?, y.trim().parse().ok()?))
                })
                .ok_or_else(|| {
                    format!(
                        "`{s}` is not a position. Use \"right\", \"left\", \"above\", \
                         \"below\", or exact coordinates such as \"2877,0\""
                    )
                }),
        }
    }
}

impl<'de> Deserialize<'de> for Position {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

/// A whole or fractional number: `scale = 2` and `scale = 1.25` both work.
fn number<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Number {
        Whole(i64),
        Fraction(f64),
    }
    Ok(match Number::deserialize(d)? {
        Number::Whole(n) => n as f64,
        Number::Fraction(f) => f,
    })
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    pub encoder: Encoder,
    /// The display's scale. It must divide the iPad's resolution into whole
    /// pixels; that is checked when the iPad connects, since it depends on
    /// the panel.
    #[serde(deserialize_with = "number")]
    pub scale: f64,
    pub position: Position,
    /// Upper limit on frames per second (R1.14).
    pub fps: u32,
    /// Video bitrate in megabits per second (R1.14).
    pub bitrate_mbps: u32,
    /// The GPU to encode on. Unset means the first render node found.
    pub render_node: Option<PathBuf>,
    pub capture: Capture,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            encoder: Encoder::Auto,
            scale: 2.0,
            position: Position::Right,
            fps: 60,
            bitrate_mbps: 25,
            render_node: None,
            capture: Capture::Auto,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot read the config file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },

    /// Carries TOML's own message, which names the key, the line and, for an
    /// unknown key, the ones that exist.
    #[error("the config file {path} has a problem:\n{detail}")]
    Invalid { path: PathBuf, detail: String },
}

impl Config {
    /// Parse config text. `path` is only used in error messages.
    pub fn parse(text: &str, path: &Path) -> Result<Config, Error> {
        let invalid = |detail: String| Error::Invalid {
            path: path.to_path_buf(),
            detail,
        };
        let config: Config = toml::from_str(text).map_err(|e| invalid(e.to_string()))?;
        config.check().map_err(invalid)?;
        Ok(config)
    }

    /// Read the file at `path`. A file that does not exist gives the defaults.
    pub fn load(path: &Path) -> Result<Config, Error> {
        match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text, path),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(source) => Err(Error::Read {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Values that parse but make no sense.
    fn check(&self) -> Result<(), String> {
        if !self.scale.is_finite() || self.scale <= 0.0 || self.scale > 8.0 {
            return Err(format!(
                "`scale = {}` must be a number above 0 and at most 8, such as 2 or 1.25",
                self.scale
            ));
        }
        if !(1..=240).contains(&self.fps) {
            return Err(format!("`fps = {}` must be between 1 and 240", self.fps));
        }
        if !(1..=200).contains(&self.bitrate_mbps) {
            return Err(format!(
                "`bitrate-mbps = {}` must be between 1 and 200",
                self.bitrate_mbps
            ));
        }
        Ok(())
    }

    pub fn bitrate_bps(&self) -> u32 {
        self.bitrate_mbps * 1_000_000
    }
}

/// `$XDG_CONFIG_HOME/padlink/config.toml`, or `~/.config/padlink/config.toml`.
pub fn default_path() -> Option<PathBuf> {
    path_from(
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

fn path_from(xdg_config_home: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    let base = xdg_config_home
        .filter(|p| p.is_absolute())
        .or_else(|| home.map(|h| h.join(".config")))?;
    Some(base.join("padlink").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Config, Error> {
        Config::parse(text, Path::new("/home/u/.config/padlink/config.toml"))
    }

    fn problem(text: &str) -> String {
        parse(text).unwrap_err().to_string()
    }

    #[test]
    fn an_empty_file_is_the_defaults() {
        let c = parse("").unwrap();
        assert_eq!(c, Config::default());
        assert_eq!((c.scale, c.fps, c.bitrate_mbps), (2.0, 60, 25));
        assert_eq!(c.encoder, Encoder::Auto);
        assert_eq!(c.position, Position::Right);
        assert_eq!(c.bitrate_bps(), 25_000_000);
    }

    #[test]
    fn every_key_can_be_set() {
        let c = parse(
            r#"
            encoder = "x264"
            scale = 1.25
            position = "left"
            fps = 30
            bitrate-mbps = 12
            render-node = "/dev/dri/renderD129"
            capture = "wlr"
            "#,
        )
        .unwrap();
        assert_eq!(
            c,
            Config {
                encoder: Encoder::X264,
                scale: 1.25,
                position: Position::Left,
                fps: 30,
                bitrate_mbps: 12,
                render_node: Some("/dev/dri/renderD129".into()),
                capture: Capture::Wlr,
            }
        );
    }

    #[test]
    fn the_encoder_can_be_switched() {
        for (text, want) in [
            ("auto", Encoder::Auto),
            ("vaapi", Encoder::Vaapi),
            ("x264", Encoder::X264),
            ("openh264", Encoder::Openh264),
        ] {
            assert_eq!(
                parse(&format!("encoder = \"{text}\"")).unwrap().encoder,
                want
            );
        }
    }

    #[test]
    fn a_whole_number_scale_is_accepted() {
        assert_eq!(parse("scale = 2").unwrap().scale, 2.0);
        assert_eq!(parse("scale = 2.5").unwrap().scale, 2.5);
    }

    #[test]
    fn positions_are_a_side_or_exact_coordinates() {
        let pos = |s: &str| parse(&format!("position = \"{s}\"")).map(|c| c.position);
        assert_eq!(pos("below").unwrap(), Position::Below);
        assert_eq!(pos("Above").unwrap(), Position::Above);
        assert_eq!(pos("2877,0").unwrap(), Position::At(2877, 0));
        assert_eq!(pos("-1180, -820").unwrap(), Position::At(-1180, -820));
        let text = pos("sideways").unwrap_err().to_string();
        assert!(text.contains("`sideways` is not a position"), "{text}");
        assert!(text.contains("\"right\""), "{text}");
    }

    /// R1.23: a typo is an error, and the message names it.
    #[test]
    fn an_unknown_key_is_an_error_that_names_it() {
        let text = problem("encodr = \"x264\"");
        assert!(
            text.contains("/home/u/.config/padlink/config.toml"),
            "{text}"
        );
        assert!(text.contains("encodr"), "{text}");
        // TOML's message lists the keys that do exist.
        assert!(text.contains("encoder"), "{text}");
    }

    #[test]
    fn an_unknown_encoder_lists_the_real_ones() {
        let text = problem("encoder = \"nvenc\"");
        assert!(text.contains("nvenc"), "{text}");
        assert!(
            text.contains("vaapi") && text.contains("openh264"),
            "{text}"
        );
    }

    #[test]
    fn values_of_the_wrong_type_are_errors() {
        assert!(parse("fps = \"fast\"").is_err());
        assert!(parse("scale = \"big\"").is_err());
        assert!(parse("fps = -5").is_err());
        assert!(parse("this is not toml").is_err());
    }

    #[test]
    fn out_of_range_values_are_errors_that_say_the_range() {
        assert!(problem("scale = 0").contains("`scale = 0` must be"));
        assert!(problem("scale = -2").contains("above 0"));
        assert!(problem("fps = 0").contains("between 1 and 240"));
        assert!(problem("fps = 1000").contains("between 1 and 240"));
        assert!(problem("bitrate-mbps = 0").contains("between 1 and 200"));
        assert!(problem("bitrate-mbps = 900").contains("between 1 and 200"));
    }

    #[test]
    fn a_missing_file_is_the_defaults_and_an_unreadable_one_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert_eq!(Config::load(&path).unwrap(), Config::default());

        std::fs::write(&path, "fps = 30\n").unwrap();
        assert_eq!(Config::load(&path).unwrap().fps, 30);

        // A directory where the file should be.
        let err = Config::load(dir.path()).unwrap_err();
        assert!(matches!(err, Error::Read { .. }), "{err}");
    }

    #[test]
    fn the_path_follows_xdg_config_home_then_home() {
        let p = |x: Option<&str>, h: Option<&str>| {
            path_from(x.map(PathBuf::from), h.map(PathBuf::from))
        };
        assert_eq!(
            p(Some("/x/cfg"), Some("/home/u")).unwrap(),
            PathBuf::from("/x/cfg/padlink/config.toml")
        );
        assert_eq!(
            p(None, Some("/home/u")).unwrap(),
            PathBuf::from("/home/u/.config/padlink/config.toml")
        );
        // A relative XDG_CONFIG_HOME is invalid by the XDG spec: ignore it.
        assert_eq!(
            p(Some("cfg"), Some("/home/u")).unwrap(),
            PathBuf::from("/home/u/.config/padlink/config.toml")
        );
        assert_eq!(p(None, None), None);
    }

    /// The example shipped in the repository must stay valid and must show
    /// the defaults.
    #[test]
    fn the_example_config_parses_to_the_defaults() {
        let text = include_str!("../../../config.example.toml");
        assert_eq!(parse(text).unwrap(), Config::default());
    }
}
