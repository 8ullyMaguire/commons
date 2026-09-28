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
    /// The origin share links are built against (T-P5-007 part 2B).
    pub public_base_url: Option<String>,
    /// DLNA/UPnP discovery. T-P6-005.
    ///
    /// `Option` because "the section is absent" and "the section says
    /// `enabled = false`" are different files, and the second is a decision
    /// somebody made. Collapsing them would make it impossible to record that
    /// a user deliberately turned it off.
    pub dlna: Option<DlnaConfig>,
}

/// DLNA/UPnP, and it is OFF unless someone turns it on.
///
/// Not a default-on feature with a default-on-off-switch. A media server that
/// advertises itself to the LAN on upgrade exposes the whole library to every
/// device on that network, and SSDP has no authentication to gate on -- the
/// `media_path` consent check every HTTP route uses has nothing to attach to
/// here, because the client is on the LAN and has never authenticated.
///
/// `deny_unknown_fields` is here rather than inherited, because
/// **`deny_unknown_fields` does not propagate into a nested struct.**
/// `FileConfig` carries it, and a `[dlna]` block with a misspelled key inside
/// it still parsed silently until a test caught it. For a feature whose
/// failure mode is "the file loaded and the feature quietly stayed off", that
/// is the exact bug the attribute exists to prevent, applied one level too
/// shallow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DlnaConfig {
    /// `#[serde(default)]` so a config file that sets only `bind` means
    /// "enabled at the default", which is `false`, rather than failing to
    /// parse. A `[dlna]` block naming a bind address and nothing else is a
    /// reasonable thing for a person to write.
    #[serde(default)]
    pub enabled: bool,
    /// The address the SSDP responder binds. `0.0.0.0:1900` reaches the LAN;
    /// `127.0.0.1:1900` does not. Defaulting to loopback is the difference
    /// between "on, for me" and "on, for the office".
    pub bind: String,
    /// The `LOCATION` advertised in the SSDP reply. It must be a URL a client
    /// can fetch, and it must NOT be `0.0.0.0` -- that string means "this
    /// machine" to nobody, and a client that fetches it fails in a way that
    /// looks like a broken television.
    pub location_base: String,
    /// The friendly name a TV shows in its source list.
    pub friendly_name: String,
}

/// The resolved configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub mode: RunMode,
    pub data_dir: PathBuf,
    pub bind: String,
    pub metrics: bool,
    /// DLNA/UPnP. T-P6-005, and `enabled` is false unless a config file or a
    /// deliberate default says otherwise.
    pub dlna: DlnaConfig,
    /// The origin share links are built against, e.g. `https://media.example`.
    ///
    /// A config field rather than derived from `bind`, because `bind` is where
    /// the process listens and the link needs where the *user* is: behind a
    /// reverse proxy, a TLS terminator, or a different hostname, `bind` is
    /// wrong in every one of those cases and the link still resolves for the
    /// person who was sent it. Defaulting to the request's own host would
    /// produce a link that works until somebody opens it from a different
    /// network, which is the worst time to find out.
    pub public_base_url: String,
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

impl Config {
    /// Where derived data lives: transcodes, and anything else recomputable.
    ///
    /// **Not `data_dir`.** The distinction is the whole reason XDG has three
    /// directories, and putting a proxy cache beside the library means a user
    /// who clears "cached data" to reclaim disk has to decide whether they are
    /// deleting their index. Derived data is safe to delete and expensive to
    /// rebuild, which is the definition of a cache.
    ///
    /// **But under `data_dir` when one is set**, and that is not a convenience.
    /// A test harness gives each test its own `data_dir` precisely so tests
    /// cannot see each other's files; a cache keyed off a global XDG path
    /// ignores that and writes into the developer's real
    /// `~/.cache/commons/proxy`. That is not only untidy: parallel tests then
    /// share one cache, so a test asserting "the proxy succeeded" can be reading
    /// a file another test wrote, and the suite passes or fails depending on
    /// scheduling. A per-test cache is the only way the proxy tests mean
    /// anything.
    pub fn cache_dir(&self) -> PathBuf {
        if self.data_dir.as_os_str().is_empty() {
            xdg_cache_dir()
        } else {
            self.data_dir.join("cache")
        }
    }
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
    /// The origin share links are built against (T-P5-007 part 2B).
    pub public_base_url: Option<String>,
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
                "--public-base-url" => cli.public_base_url = Some(value(&mut it)?),
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
    --public-base-url   origin share links are built against, e.g.
                        https://media.example [default: http://<bind>]
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

    // Defaulting to the bind address is right for the common case (a library on
    // localhost) and wrong behind a reverse proxy, which is why it is
    // overridable rather than derived from the request. The `http://` prefix is
    // added because `bind` is a socket address and an origin is a URL, and
    // making the operator write a full URL to say "the same host I already
    // said" is a chance to get it wrong.
    let public_base_url = cli
        .public_base_url
        .clone()
        .or_else(|| file.and_then(|f| f.public_base_url.clone()))
        .unwrap_or_else(|| format!("http://{bind}"));

    // Default OFF, and default to LOOPBACK even when someone turns it on.
    // Both defaults are the safe direction: enabling DLNA is a decision
    // somebody has to type, and having done that they get a responder that
    // reaches this machine rather than the whole office network.
    //
    // `location_base` defaults to the resolved `public_base_url` rather than
    // to the bind address, because the bind address is a socket to listen on
    // (`0.0.0.0:8096`, a port) and a LOCATION is a URL a client fetches. Same
    // reasoning as the `public_base_url` default above, applied twice.
    let dlna = file
        .and_then(|f| f.dlna.clone())
        .unwrap_or_else(|| DlnaConfig {
            enabled: false,
            bind: "127.0.0.1:1900".to_string(),
            location_base: public_base_url.clone(),
            friendly_name: "commons".to_string(),
        });

    Ok(Config {
        mode,
        data_dir,
        bind,
        metrics,
        dlna,
        public_base_url,
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
            public_base_url: None,
            mode: Some(RunMode::Index),
            data_dir: Some(PathBuf::from("/from/file")),
            bind: Some("1.1.1.1:1".into()),
            metrics: Some(true),
            dlna: None,
        };

        // Default only.
        let c = resolve(&cli(&[]), Some(&file)).unwrap();
        assert_eq!(c.mode, RunMode::Index);
        assert_eq!(c.data_dir, PathBuf::from("/from/file"));
        assert!(c.metrics);

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
            public_base_url: None,
            mode: Some(RunMode::Peer),
            data_dir: Some(PathBuf::from("/srv/library")),
            bind: Some("0.0.0.0:8080".into()),
            metrics: Some(false),
            dlna: None,
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

    // ---------- DLNA (T-P6-005 step 2) ----------

    #[test]
    fn dlna_is_off_and_loopback_bound_by_default() {
        // The three defaults that matter, asserted together because they are
        // one decision: "off, and when you turn it on, only to this machine,
        // advertising an address a client can actually fetch."
        let c = resolve(&cli(&[]), Some(&FileConfig::default())).unwrap();
        assert!(
            !c.dlna.enabled,
            "DLNA must be a decision somebody types, not a default"
        );
        assert!(
            c.dlna.bind.starts_with("127.0.0.1:"),
            "and loopback-bound even when enabled: {}",
            c.dlna.bind
        );
        assert!(
            !c.dlna.location_base.contains("0.0.0.0"),
            "a LOCATION of 0.0.0.0 means 'this machine' to nobody, and a client \
             that fetches it fails in a way that looks like a broken TV: {}",
            c.dlna.location_base
        );
    }

    #[test]
    fn dlna_with_no_config_file_at_all_is_also_off() {
        // `resolve` takes `Option<&FileConfig>`, and "no file" is the common
        // case on a first run. A default that only applies when a file exists
        // is not a default.
        let c = resolve(&cli(&[]), None).unwrap();
        assert!(!c.dlna.enabled);
    }

    #[test]
    fn a_config_file_can_turn_dlna_on_and_say_where() {
        // The point of the section: a user who wants it writes it.
        let file = FileConfig {
            dlna: Some(DlnaConfig {
                enabled: true,
                bind: "0.0.0.0:1900".into(),
                location_base: "http://192.168.1.10:8096".into(),
                friendly_name: "The library".into(),
            }),
            ..FileConfig::default()
        };
        let c = resolve(&cli(&[]), Some(&file)).unwrap();
        assert!(c.dlna.enabled, "an explicit true is honoured");
        assert_eq!(c.dlna.bind, "0.0.0.0:1900", "reaching the LAN is allowed");
        assert_eq!(c.dlna.location_base, "http://192.168.1.10:8096");
        assert_eq!(c.dlna.friendly_name, "The library");
    }

    #[test]
    fn a_dlna_block_that_names_only_a_bind_is_still_off() {
        // `#[serde(default)]` on `enabled`, asserted through a real TOML file
        // rather than a struct literal -- the question is what a person
        // writing a config file gets, and a struct literal cannot answer it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commons.toml");
        std::fs::write(
            &path,
            "[dlna]\nbind = \"0.0.0.0:1900\"\nlocation_base = \"http://h:8096\"\nfriendly_name = \"x\"\n",
        )
        .unwrap();

        let loaded = load_file(&path).unwrap().expect("the file parses");
        // Borrowed rather than moved, because `loaded` is handed to `resolve`
        // on the next line. `.expect()` on an `Option<DlnaConfig>` field of a
        // struct MOVES the field, and a test that consumes its own fixture
        // halfway through is a confusing error to read.
        let dlna = loaded.dlna.as_ref().expect("a [dlna] section");
        assert!(!dlna.enabled, "omitting `enabled` means off, not on");
        assert_eq!(dlna.bind, "0.0.0.0:1900", "and the rest is read");

        let c = resolve(&cli(&[]), Some(&loaded)).unwrap();
        assert!(!c.dlna.enabled, "and it stays off through resolve");
    }

    #[test]
    fn a_typo_in_the_dlna_block_is_an_error_rather_than_a_setting_that_does_nothing() {
        // `deny_unknown_fields` is what makes this true, and the failure it
        // prevents is the worst kind: a config file that parses, a feature
        // that stays off, and a user with no idea why their television cannot
        // see the library.
        //
        // This test exists because that exact bug was present and invisible.
        // `FileConfig` has carried `deny_unknown_fields` all along, and it does
        // NOT propagate into a nested struct -- so a `[dlna]` block with
        // `enabeld = true` alongside every required field parsed cleanly and
        // was ignored. `DlnaConfig` now carries the attribute itself. Written
        // through a real TOML file rather than a struct literal, because a
        // struct literal cannot exercise a serde attribute at all.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commons.toml");
        std::fs::write(
            &path,
            "[dlna]\nbind = \"127.0.0.1:1900\"\nlocation_base = \"http://h:8096\"\n\
             friendly_name = \"x\"\nenabeld = true\n",
        )
        .unwrap();

        let e = load_file(&path).expect_err("a misspelled key is not a config");
        assert!(
            format!("{e}").contains("enabeld"),
            "the error names the key the person mistyped: {e}"
        );
    }

    #[test]
    fn a_complete_dlna_block_with_no_typos_loads() {
        // The other half, so the test above cannot be satisfied by rejecting
        // every `[dlna]` block. Serde's error for a missing required field
        // reads `missing field \`bind\``, which is a rejection too -- a test
        // that only checked "it errors" would pass on a config that never
        // worked.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("commons.toml");
        std::fs::write(
            &path,
            "[dlna]\nenabled = true\nbind = \"0.0.0.0:1900\"\n\
             location_base = \"http://192.168.1.10:8096\"\nfriendly_name = \"The library\"\n",
        )
        .unwrap();

        let loaded = load_file(&path).unwrap().expect("the file parses");
        let dlna = loaded.dlna.expect("a [dlna] section");
        assert!(dlna.enabled);
        assert_eq!(dlna.bind, "0.0.0.0:1900");
        assert_eq!(dlna.location_base, "http://192.168.1.10:8096");
        assert_eq!(dlna.friendly_name, "The library");
    }

    #[test]
    fn the_dlna_location_defaults_to_the_public_base_url_and_not_the_bind_address() {
        // The bind address is a socket to LISTEN on (`0.0.0.0:8096`, a port)
        // and a LOCATION is a URL a client FETCHES. Deriving one from the
        // other produces `http://0.0.0.0:8096`, which is unroutable.
        let c = resolve(
            &cli(&["--public-base-url", "https://media.example"]),
            Some(&FileConfig::default()),
        )
        .unwrap();
        assert_eq!(c.dlna.location_base, "https://media.example");
        // The socket this process listens on is a different thing entirely, and
        // must not leak into the advertised URL.
        assert_ne!(
            c.dlna.location_base,
            format!("http://{}", c.bind),
            "the LOCATION is a URL, not a socket: bind is {}",
            c.bind
        );
    }
}
