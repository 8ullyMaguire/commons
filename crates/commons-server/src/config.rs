//! Configuration and XDG paths (T-P0-009, spec §3.7).
//!
//! Precedence is CLI flag > config file > default, in that order. The
//! config file is TOML and every key is optional, so a partial file is valid.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Which role this process plays (§3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    /// A local library. SQLite in the data directory, served to localhost.
    Library,
    /// A public index. Postgres, served to the network, consent enforced.
    Index,
    /// A peer: a library that also federates (§13).
    Peer,
}

impl RunMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RunMode::Library => "library",
            RunMode::Index => "index",
            RunMode::Peer => "peer",
        }
    }

    /// Whether this mode listens on a non-loopback address by default. An index
    /// is meant to be public; a library is not, and binding one to 0.0.0.0 by
    /// default would be a security surprise.
    pub fn default_bind(self) -> &'static str {
        match self {
            RunMode::Library | RunMode::Peer => "127.0.0.1:9999",
            RunMode::Index => "0.0.0.0:9999",
        }
    }
}

/// The file form. Every field is `Option` so "absent" is distinguishable from
/// "set to the default", which is what makes the precedence rule work.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub mode: Option<RunMode>,
    pub data_dir: Option<PathBuf>,
    pub bind: Option<String>,
    /// Off by default: a metrics endpoint exposes library statistics.
    pub metrics: Option<bool>,
}

/// The resolved configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub mode: RunMode,
    pub data_dir: PathBuf,
    pub bind: String,
    pub metrics: bool,
}

/// XDG base directories (stash#2814). `$XDG_DATA_HOME/commons` and friends,
/// each falling back to the documented `~/.local/share` default.
pub fn xdg_data_dir() -> PathBuf {
    base("XDG_DATA_HOME", ".local/share")
}

pub fn xdg_config_dir() -> PathBuf {
    base("XDG_CONFIG_HOME", ".config")
}

pub fn xdg_cache_dir() -> PathBuf {
    base("XDG_CACHE_HOME", ".cache")
}

fn base(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v).join("commons"),
        _ => home_dir().join(fallback).join("commons"),
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("unknown mode {0:?}: expected library, index or peer")]
    UnknownMode(String),
    #[error("cannot read config file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("config file {0} is not valid TOML: {1}")]
    Parse(PathBuf, String),
}

/// CLI arguments. Parsed by hand rather than with a dependency: there are six
/// of them and a hand parser makes the precedence rule readable.
#[derive(Debug, Clone, Default)]
pub struct Cli {
    pub mode: Option<String>,
    pub data_dir: Option<PathBuf>,
    pub bind: Option<String>,
    pub config: Option<PathBuf>,
    pub metrics: bool,
}

impl Cli {
    /// Parse `std::env::args`, skipping the program name.
    pub fn parse_from<I: IntoIterator<Item = String>>(args: I) -> Result<Self, String> {
        let mut cli = Cli::default();
        let mut it = args.into_iter();
        // argv[0]
        let _ = it.next();
        while let Some(arg) = it.next() {
            let (flag, inline) = match arg.split_once('=') {
                Some((f, v)) => (f.to_string(), Some(v.to_string())),
                None => (arg.clone(), None),
            };
            let value = |it: &mut dyn Iterator<Item = String>| -> Result<String, String> {
                match inline.clone() {
                    Some(v) => Ok(v),
                    None => it.next().ok_or_else(|| format!("{flag} needs a value")),
                }
            };
            match flag.as_str() {
                "--mode" => cli.mode = Some(value(&mut it)?),
                "--data-dir" => cli.data_dir = Some(PathBuf::from(value(&mut it)?)),
                "--bind" => cli.bind = Some(value(&mut it)?),
                "--config" => cli.config = Some(PathBuf::from(value(&mut it)?)),
                "--metrics" => cli.metrics = true,
                "--help" | "-h" => return Err(USAGE.to_string()),
                "--version" | "-V" => return Err(format!("commons {}", env!("CARGO_PKG_VERSION"))),
                other => return Err(format!("unknown flag {other}\n\n{USAGE}")),
            }
        }
        Ok(cli)
    }

    pub fn from_env() -> Result<Self, String> {
        Self::parse_from(std::env::args())
    }
}

pub const USAGE: &str = "\
commons — a consent-first media index

USAGE:
    commons-server --mode <library|index|peer> [OPTIONS]

OPTIONS:
    --mode <MODE>       library (local, SQLite), index (public, Postgres), or peer
    --data-dir <DIR>    library location; overrides the XDG data dir
    --bind <ADDR>       listen address [default: 127.0.0.1:9999 for library/peer,
                        0.0.0.0:9999 for index]
    --config <FILE>     TOML config file
    --metrics           expose /metrics (off by default: it reports library stats)
    -h, --help          print this help
    -V, --version       print the version
";

/// Resolve CLI over file over default.
pub fn resolve(cli: &Cli, file: Option<&FileConfig>) -> Result<Config, ConfigError> {
    let mode = match cli.mode.as_deref() {
        Some(s) => Some(parse_mode(s)?),
        None => file.and_then(|f| f.mode),
    }
    .unwrap_or(RunMode::Library);

    let data_dir = cli
        .data_dir
        .clone()
        .or_else(|| file.and_then(|f| f.data_dir.clone()))
        .unwrap_or_else(xdg_data_dir);

    let bind = cli
        .bind
        .clone()
        .or_else(|| file.and_then(|f| f.bind.clone()))
        .unwrap_or_else(|| mode.default_bind().to_string());

    let metrics = cli.metrics || file.and_then(|f| f.metrics).unwrap_or(false);

    Ok(Config {
        mode,
        data_dir,
        bind,
        metrics,
    })
}

pub fn parse_mode(s: &str) -> Result<RunMode, ConfigError> {
    match s {
        "library" => Ok(RunMode::Library),
        "index" => Ok(RunMode::Index),
        "peer" => Ok(RunMode::Peer),
        other => Err(ConfigError::UnknownMode(other.to_string())),
    }
}

/// Load a config file. A missing file is not an error: every key is optional
/// and the defaults are the documented ones.
pub fn load_file(path: &std::path::Path) -> Result<Option<FileConfig>, ConfigError> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let cfg =
        toml::from_str(&text).map_err(|e| ConfigError::Parse(path.to_path_buf(), e.to_string()))?;
    Ok(Some(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(args: &[&str]) -> Cli {
        let mut it = vec!["commons-server".to_string()];
        it.extend(args.iter().map(|s| s.to_string()));
        Cli::parse_from(it).expect("args parse")
    }

    #[test]
    fn cli_flag_beats_config_file_beats_default() {
        let file = FileConfig {
            mode: Some(RunMode::Index),
            data_dir: Some(PathBuf::from("/from/file")),
            bind: Some("1.1.1.1:1".into()),
            metrics: Some(true),
        };

        // Default only.
        let c = resolve(&cli(&[]), Some(&file)).unwrap();
        assert_eq!(c.mode, RunMode::Index);
        assert_eq!(c.data_dir, PathBuf::from("/from/file"));
        assert_eq!(c.metrics, true);

        // CLI overrides each field.
        let c = resolve(
            &cli(&["--mode", "library", "--data-dir", "/from/cli"]),
            Some(&file),
        )
        .unwrap();
        assert_eq!(c.mode, RunMode::Library);
        assert_eq!(c.data_dir, PathBuf::from("/from/cli"));
        // Not overridden on the CLI, so still from the file.
        assert_eq!(c.bind, "1.1.1.1:1");
    }

    #[test]
    fn an_empty_config_yields_the_documented_defaults() {
        let c = resolve(&cli(&[]), Some(&FileConfig::default())).unwrap();
        assert_eq!(c.mode, RunMode::Library);
        assert_eq!(c.data_dir, xdg_data_dir());
        assert_eq!(c.bind, "127.0.0.1:9999");
        assert!(!c.metrics, "metrics must be off unless asked for");
    }

    #[test]
    fn an_index_binds_publicly_and_a_library_does_not() {
        // Binding a library to 0.0.0.0 by default would be a security surprise.
        let lib = resolve(&cli(&["--mode", "library"]), None).unwrap();
        assert!(lib.bind.starts_with("127.0.0.1"));
        let idx = resolve(&cli(&["--mode", "index"]), None).unwrap();
        assert_eq!(idx.bind, "0.0.0.0:9999");
        let peer = resolve(&cli(&["--mode", "peer"]), None).unwrap();
        assert!(peer.bind.starts_with("127.0.0.1"));
    }

    #[test]
    fn equals_form_and_space_form_agree() {
        assert_eq!(cli(&["--mode=index"]).mode.as_deref(), Some("index"));
        assert_eq!(cli(&["--mode", "index"]).mode.as_deref(), Some("index"));
        assert_eq!(cli(&["--data-dir=/x"]).data_dir, Some(PathBuf::from("/x")));
    }

    #[test]
    fn a_flag_without_its_value_is_an_error_not_a_silent_default() {
        let mut it = vec!["commons-server".to_string(), "--mode".to_string()];
        assert!(Cli::parse_from(it.clone()).is_err());
        it.push("index".to_string());
        assert!(Cli::parse_from(it).is_ok());
    }

    #[test]
    fn an_unknown_flag_is_rejected_rather_than_ignored() {
        assert!(Cli::parse_from(["commons-server".into(), "--wat".into()]).is_err());
    }

    #[test]
    fn an_unknown_mode_names_the_valid_ones() {
        let err = parse_mode("nope").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("library") && msg.contains("index"), "{msg}");
    }

    #[test]
    fn a_config_file_round_trips_and_a_missing_one_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commons.toml");
        let original = FileConfig {
            mode: Some(RunMode::Peer),
            data_dir: Some(PathBuf::from("/srv/library")),
            bind: Some("0.0.0.0:8080".into()),
            metrics: Some(false),
        };
        std::fs::write(&path, toml::to_string(&original).unwrap()).unwrap();

        let loaded = load_file(&path).unwrap().expect("file exists");
        assert_eq!(loaded.mode, Some(RunMode::Peer));
        assert_eq!(loaded.data_dir, Some(PathBuf::from("/srv/library")));
        assert_eq!(loaded.metrics, Some(false));

        assert!(load_file(&dir.path().join("absent.toml"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_malformed_config_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "this is not = = toml").unwrap();
        let err = load_file(&path).unwrap_err();
        assert!(err.to_string().contains("bad.toml"), "{err}");
    }

    #[test]
    fn an_unknown_config_key_is_rejected_rather_than_ignored() {
        // A typo'd key that is silently dropped is a config that appears to
        // work and does not.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("typo.toml");
        std::fs::write(&path, "mod = \"index\"\n").unwrap();
        assert!(load_file(&path).is_err());
    }

    #[test]
    fn xdg_paths_follow_the_environment() {
        // Uses the process environment, so set and restore rather than assert
        // against whatever the host happens to have.
        let prev_data = std::env::var_os("XDG_DATA_HOME");
        unsafe { std::env::set_var("XDG_DATA_HOME", "/xdg/data") };
        assert_eq!(xdg_data_dir(), PathBuf::from("/xdg/data/commons"));
        unsafe {
            match prev_data {
                Some(v) => std::env::set_var("XDG_DATA_HOME", v),
                None => std::env::remove_var("XDG_DATA_HOME"),
            }
        }

        // An empty value is treated as unset, per the XDG spec.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", "") };
        let c = xdg_config_dir();
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
        assert!(c.ends_with(".config/commons"), "{c:?}");
    }
}
