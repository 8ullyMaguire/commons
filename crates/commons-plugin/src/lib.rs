//! Plugin host (T-P0-006, spec §5.18.1 and §11.4).
//!
//! This is the crate that makes §2's "not a downloader" a property of the
//! system rather than a claim about it. §5.18.1 moves the P2P locator
//! capability out of core and into a one-click plugin, and the reason is
//! mechanical: inside a sandbox whose only network capability is
//! [`Capability::LoopbackHttp`], the plugin *cannot* resolve a public
//! hostname, open a socket to a peer, join a swarm, run a DHT, or accept an
//! inbound connection. Those are not unused code paths — the capability to
//! perform them is absent from the plugin's view of the host.
//!
//! Two things are deliberately **not** delegated to the sandbox:
//!
//!   * the consent tier check. A tier check inside a third-party component is
//!     a tier check that can be buggy, disabled, or hostile, and §14 is the
//!     entire reason this platform may hold amateur material. So a plugin has
//!     no write path to tier, object, or database. It calls
//!     [`HostApi::propose_locator`] and core evaluates the §14.1 table.
//!   * content hashing. xxh128 and BLAKE3 stay in core because §6.2's
//!     incremental correctness and §9.7's dedup both depend on them. Only the
//!     protocol-specific hashes belong to a plugin.

use commons_store::locator::{self, LocatorScheme, ProposedLocator};
use commons_store::Store;
use std::collections::BTreeSet;

/// What a plugin is allowed to do. A plugin declares these; the host grants
/// the intersection of what it asked for and what the operator allows, and
/// [`HostPolicy`] is the thing the user sees before installing.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Read file headers and probe media. No network.
    ReadLibrary,
    /// Read and write metadata through proposals and votes.
    ProposeMetadata,
    /// HTTP to `127.0.0.1` / `[::1]` / `localhost` only, for handing a magnet to
    /// a local download client. This is the whole of the network surface.
    LoopbackHttp,
    /// Reach an arbitrary host. Not granted to the locator plugin, and the
    /// reason is §5.18.1: with this, a "locator" plugin could be a downloader.
    Network,
    /// Spawn a process. Absent from every first-party plugin.
    ProcessSpawn,
}

impl Capability {
    /// The plain-language disclosure shown before install (§5.18.1 requires
    /// this, and it is a human-readable sentence rather than a capability
    /// name, because a user cannot consent to a token they do not understand).
    pub fn disclosure(self) -> &'static str {
        match self {
            Capability::ReadLibrary => "read file headers and media metadata from your library",
            Capability::ProposeMetadata => "add metadata proposals that you can vote on",
            Capability::LoopbackHttp => {
                "make HTTP requests to a download client running on this computer only"
            }
            Capability::Network => "connect to the internet, including other people's servers",
            Capability::ProcessSpawn => "start other programs on your computer",
        }
    }

    /// Capabilities no first-party plugin requests, and which the gallery
    /// refuses by default.
    pub const fn is_privileged(self) -> bool {
        matches!(self, Capability::Network | Capability::ProcessSpawn)
    }
}

/// A plugin's declared manifest. Parsed from the plugin's own metadata; every
/// field is untrusted.
///
/// T-P6-006. `deny_unknown_fields` is the FIRST line of defence and it is
/// only the first: it catches a misspelled KEY and nothing else. A manifest
/// with `id: ""`, `version: ""`, `id: "../../etc/passwd"` or
/// `api_version: 0` parses cleanly without it — eleven of the twelve
/// malformed cases in the spec's probe are value problems, not key problems.
/// `Manifest::validate` is what catches those, and the split is deliberate:
/// a key this host does not understand is a schema error, and a value that
/// makes no sense is a validation error, and a caller may want to treat them
/// differently.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    /// The sandbox API version the plugin was built against (§11.4's answer to
    /// the 34 upstream plugin-API issues: a plugin that does not match is
    /// refused, not adapted).
    pub api_version: u32,
    pub requested: BTreeSet<Capability>,
    /// The P2P locator plugin of §5.18.1.
    #[serde(default)]
    pub kind: PluginKind,
}

impl Manifest {
    /// Check everything serde cannot.
    ///
    /// Split deliberately from `Installer::install`, which is where the
    /// temptation is to put it. `install` is not the only thing that reads a
    /// manifest: §5.18.1's disclosure panel renders one *before* anyone
    /// decides to install, and a future gallery listing parses them all. A
    /// check that only exists on the install path is a check the other two
    /// paths do not have — and the whole point of §11.4's "the host enforces
    /// them at instantiation" is that the enforcement is not optional.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.id.is_empty() {
            return Err(ManifestError::EmptyId);
        }
        if !is_safe_id(&self.id) {
            return Err(ManifestError::IdNotReverseDns(self.id.clone()));
        }
        if self.name.is_empty() {
            return Err(ManifestError::EmptyName);
        }
        if !is_semver(&self.version) {
            return Err(ManifestError::MalformedVersion(self.version.clone()));
        }
        // A *mismatch* against `API_VERSION` is `install`'s job, because a
        // mismatch is a POLICY question ("this build speaks 1, that plugin
        // speaks 2"). 0 is different: no host has ever spoken 0, and in Rust
        // an unset `u32` is 0, so it reads as "the author left this out"
        // rather than as a version. That is a bug in the manifest.
        if self.api_version == 0 {
            return Err(ManifestError::BadApiVersion(self.api_version));
        }
        Ok(())
    }
}

/// Lowercase `a-z`, `0-9`, dot, dash, underscore — and none of the separators
/// that mean something.
///
/// **The separators are the point, not the length or shape.** `..` is what
/// makes `../../etc/passwd` a traversal, and `/` and `\` are what make an id
/// a path at all. A validator that allows them has validated nothing, and the
/// id is the key a reinstall (#6987) and an uninstall will look up — so this
/// is a shape check on something that will be used as a *name*.
///
/// **No dot is REQUIRED, deliberately.** The crate's own fixture is
/// `id: "commons-locator"`, and requiring a dot would refuse the one
/// first-party plugin §11.4 names. The rule is "the characters must be safe
/// and the separators must not mean anything", not "this must look like a
/// domain". A strict reverse-DNS requirement is a defensible alternative and
/// would be a breaking change to that fixture — decide it deliberately rather
/// than discovering it in a failing test.
fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && !id.ends_with('.')
        && !id.contains("..")
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'))
}

/// A minimal semver check: `MAJOR.MINOR.PATCH`, each a run of ASCII digits,
/// optionally followed by `-prerelease` and/or `+build`.
///
/// **Hand-rolled rather than the `semver` crate, which is declared in this
/// crate's `Cargo.toml` and used nowhere in it.** A dead dependency is one
/// `clippy::unused_crate_dependencies` would flag the moment it was enabled.
/// The reason to keep it hand-rolled rather than delete the line and use the
/// crate: this function is *exactly the property under test*, so it is
/// readable in the same file as the table that pins it. `Version::parse` is a
/// black box whose edge cases cannot be checked by reading.
///
/// # The leading-zero rule, and the bug this had first
///
/// `!p.starts_with('0')` looks right and is **wrong**: it rejects a bare
/// `"0"`, so it refuses `1.0.0` and `0.1.0` — every real version. The rule is
/// *no leading zero*: `p.len() > 1 && p.starts_with('0')`, which still refuses
/// `01.0.0` and `1.00.0`.
///
/// Found by transcribing this to Python and running all 22 inputs before
/// writing it down: three failed, and two of them were the most important
/// inputs there are. A validator that rejects `1.0.0` fails loudly and would
/// have cost one test cycle; the point of checking first is that it did not.
fn is_semver(v: &str) -> bool {
    let core = v.split(['-', '+']).next().unwrap_or_default();
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.chars().all(|c| c.is_ascii_digit())
                && !(p.len() > 1 && p.starts_with('0'))
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    /// The locator plugin. The sandbox is the point: it has LoopbackHttp and
    /// no Network, which is what makes "no swarms, no DHT, no incoming
    /// connections" true of the installed artifact and not merely of the docs.
    Locator,
    #[default]
    General,
}

/// The operator's policy: what may be granted at all.
#[derive(Debug, Clone)]
pub struct HostPolicy {
    /// Granted to any plugin that asks.
    pub allow: BTreeSet<Capability>,
    /// Refused no matter what the plugin requests. Empty by default; a user can
    /// flip Network on for their own plugin, and doing so is a visible, recorded
    /// act rather than a silent capability.
    pub deny: BTreeSet<Capability>,
}

impl Default for HostPolicy {
    fn default() -> Self {
        let mut allow = BTreeSet::new();
        allow.insert(Capability::ReadLibrary);
        allow.insert(Capability::ProposeMetadata);
        allow.insert(Capability::LoopbackHttp);
        Self {
            allow,
            deny: BTreeSet::new(),
        }
    }
}

impl HostPolicy {
    /// The first-party default: nothing privileged, nothing denied. The locator
    /// plugin therefore runs with no path to a peer.
    pub fn first_party() -> Self {
        Self::default()
    }

    /// The intersection actually granted. `deny` always wins, so a policy can
    /// revoke a capability a previously installed plugin relies on without
    /// uninstalling it.
    pub fn grant(&self, requested: &BTreeSet<Capability>) -> BTreeSet<Capability> {
        requested
            .intersection(&self.allow)
            .filter(|c| !self.deny.contains(c))
            .copied()
            .collect()
    }

    /// What the user is shown. Every granted capability appears as a sentence,
    /// and so does every refused one, per §5.18.1.
    pub fn disclosure(&self, requested: &BTreeSet<Capability>) -> String {
        let granted = self.grant(requested);
        let mut parts: Vec<&str> = granted.iter().map(|c| c.disclosure()).collect();
        parts.sort_unstable();
        if parts.is_empty() {
            return "This plugin cannot do anything: the host grants it no capabilities."
                .to_string();
        }
        let mut s = parts.join(", ").to_lowercase();
        s.push('.');
        let mut refused: Vec<&str> = requested
            .iter()
            .filter(|c| !granted.contains(c))
            .map(|c| c.disclosure())
            .collect();
        if !refused.is_empty() {
            refused.sort_unstable();
            s.push_str(" It will not be able to ");
            s.push_str(&refused.join(", ").to_lowercase());
            s.push('.');
        }
        s
    }
}

/// Why a manifest is not a manifest. Distinct variants because §5.18.1 shows
/// the user *which* part of their plugin is wrong, and a single `Invalid`
/// cannot do that — the same reason `InstallError` has five variants rather
/// than one.
///
/// A **schema** refusal, and deliberately distinct from `InstallError`, which
/// is a **policy** refusal. A manifest asking for `network` is well-formed;
/// whether the user grants it is `HostPolicy`'s business, and §11.4 expects
/// the first-party scrapers to ask for internet access. Conflating the two
/// would make the plugin API unusable for exactly the plugins the spec names
/// — and it would make "grant me the network" impossible, since a user can
/// only grant what a validator let through.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("plugin id is empty")]
    EmptyId,
    #[error("plugin id {0:?} is not lowercase a-z, 0-9, dot, dash or underscore")]
    IdNotReverseDns(String),
    #[error("plugin name is empty")]
    EmptyName,
    #[error("plugin version {0:?} is not semver (major.minor.patch)")]
    MalformedVersion(String),
    #[error("api_version {0} is not a version any host was built against")]
    BadApiVersion(u32),
}

impl ManifestError {
    /// A stable machine-readable name.
    ///
    /// `Display` is for a person; this is for a test table and for a UI that
    /// wants to attach a message to a *field*. It is part of this crate's API
    /// in the same way `Capability`'s wire names are, and
    /// `tests/manifest_schema.rs` pins every one of them.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::EmptyId => "EmptyId",
            Self::IdNotReverseDns(_) => "IdNotReverseDns",
            Self::EmptyName => "EmptyName",
            Self::MalformedVersion(_) => "MalformedVersion",
            Self::BadApiVersion(_) => "BadApiVersion",
        }
    }
}

/// Why a plugin was refused. Distinct variants because "wrong API version" and
/// "you asked for the internet" call for different user actions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    #[error("plugin api version {got} is not supported; this build speaks {want}")]
    ApiVersion { got: u32, want: u32 },
    #[error("the host does not grant this plugin any capability")]
    NoCapabilities,
    #[error("the host denies {0:?}, which this plugin requires")]
    Denied(Capability),
    #[error("plugin id {0:?} is already installed")]
    AlreadyInstalled(String),
    #[error("a locator plugin may not request {0:?}")]
    LocatorRequestsPrivileged(Capability),
    /// T-P6-006. The manifest is not a manifest, as opposed to being one the
    /// host declines to accept. Wrapping `ManifestError` rather than adding
    /// a second parallel enum keeps "why was this refused" answerable in one
    /// `match` at the call site.
    #[error("plugin manifest is not valid: {0}")]
    Malformed(#[from] ManifestError),
}

/// The API version this build speaks. Bumping it is how §11.4 stays honest:
/// the sandbox can change, and a plugin built for the old one is refused rather
/// than silently given different powers.
pub const API_VERSION: u32 = 1;

/// What the host hands a plugin. Deliberately narrow: there is no handle to the
/// database, no way to set a consent tier, and no general request method.
pub trait HostApi {
    /// The capabilities actually granted to this plugin instance.
    fn granted(&self) -> &BTreeSet<Capability>;

    /// Propose a locator for an object. Core checks the §14.1 tier table and
    /// persists or refuses; the plugin does not decide.
    ///
    /// `async` because it is a database write, and a synchronous signature over
    /// a `Store` would have to block on a runtime thread — which is how a
    /// plugin's locator proposal ends up stalling every other request in the
    /// process. The previous signature was synchronous and could not be
    /// anything but a stub.
    fn propose_locator<'a>(
        &'a self,
        object_id: &'a str,
        scheme: &'a str,
        uri: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<LocatorOutcome, HostError>> + 'a>>;

    /// HTTP to loopback only. A non-loopback host is a `NotLoopback` error, not
    /// a failed request, so a plugin can tell a policy refusal from a timeout.
    fn loopback_get(&self, url: &str) -> Result<Vec<u8>, HostError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocatorOutcome {
    /// Stored. The object was at a tier permitting this scheme.
    Stored { locator_id: String },
    /// Refused by the consent table. `scheme` and `object_tier` are returned so
    /// the plugin can explain itself without inspecting the tier itself.
    Refused { scheme: String, object_tier: String },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    #[error("this plugin has no {0:?} capability")]
    NoCapability(Capability),
    #[error("only loopback addresses are reachable: {0}")]
    NotLoopback(String),
    #[error("the object does not exist")]
    NoSuchObject,
    #[error("host error: {0}")]
    Internal(String),
}

/// Whether a URL is reachable under `LoopbackHttp`.
///
/// This is a pure function so the security property is testable without a
/// network, and so it can be reused by the real HTTP client as its pre-flight
/// check. It is deliberately strict: an ambiguous host is treated as
/// non-loopback, because a false negative costs the user one failed request
/// while a false positive costs the property the whole design rests on.
pub fn is_loopback_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if !matches!(scheme, "http" | "https") {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // Discard userinfo: `http://127.0.0.1@evil.com/` is an evil.com URL.
    let hostport = authority.rsplit('@').next().unwrap_or(authority);
    let host = match hostport.strip_prefix('[') {
        // Bracketed IPv6 literal: [::1] or [::1]:8080
        Some(after) => after.split(']').next().unwrap_or(""),
        None => hostport.split(':').next().unwrap_or(""),
    };
    let host = host.trim().to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    if host == "localhost" {
        return true;
    }
    // IPv6 loopback is exactly ::1. Anything else in brackets is not loopback.
    if host.contains(':') {
        return host == "::1";
    }
    // IPv4: the whole 127.0.0.0/8 block is loopback. Written out rather than
    // delegated to a parser, because `0x7f.0.0.1` and `2130706433` are
    // alternate encodings of loopback that a naive check would mis-handle, and
    // the safe answer for an unrecognized IPv4 form is "not loopback".
    let octets: Vec<&str> = host.split('.').collect();
    if octets.len() != 4 {
        return false;
    }
    let mut parsed = [0u8; 4];
    for (i, o) in octets.iter().enumerate() {
        // No leading zeros and no non-digits: reject octal and hex forms.
        if o.is_empty() || o.len() > 3 || !o.bytes().all(|b| b.is_ascii_digit()) {
            return false;
        }
        if o.len() > 1 && o.starts_with('0') {
            return false;
        }
        match o.parse::<u16>() {
            Ok(v) if v <= 255 => parsed[i] = v as u8,
            _ => return false,
        }
    }
    parsed[0] == 127
}

/// The installed plugin record.
#[derive(Debug, Clone)]
pub struct Installed {
    pub manifest: Manifest,
    pub granted: BTreeSet<Capability>,
}

impl Installed {
    /// The API a plugin instance sees, over `store`.
    ///
    /// Goes through [`PluginHostApi::new`] rather than building the struct
    /// literal, so the granted set here is exactly the one the installer
    /// computed and the store is the same one the rest of the host uses.
    pub fn api<'a>(&'a self, store: &'a Store) -> PluginHostApi<'a> {
        PluginHostApi::new(store, &self.granted, &self.manifest.id)
    }
}

/// Drive a `HostApi` future to completion on the current thread.
///
/// `HostApi::propose_locator` returns a boxed future rather than being an
/// `async fn` in the trait, so a `dyn HostApi` can name it without
/// `async-trait`. That is the right trade at the sandbox boundary — no
/// proc-macro, no hidden `'async_trait` bounds — but it leaves synchronous
/// callers, which today means tests, needing a runtime.
///
/// Not a general executor: it takes a future that is already ready to make
/// progress and drives the store's own pool, which is why the tests that call
/// this work against a real database. A production caller is inside a
/// tokio runtime and awaits directly.
pub fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime is always constructible")
        .block_on(fut)
}

/// The host side of the sandbox, as seen by a plugin.
///
/// Holds a `&Store`, not a `&dyn ProposeSink`, so the compile-time statement is
/// the one that matters: **a `PluginHostApi` cannot exist without a database**,
/// which means the stub that returned `Stored { locator_id: "stub" }` is not
/// reachable. That stub was worse than a gap — it reported success for a write
/// that never happened, and a plugin author debugging "my locators vanish" has
/// no way to tell that from a database problem.
///
/// The store is behind `&self` and the write is `async`, so the signature is
/// `&self` plus a future rather than `&mut self`; a plugin cannot hold the
/// handle across calls in a way that would need interior mutability, and two
/// concurrent proposals are two independent `&Store` borrows.
pub struct PluginHostApi<'a> {
    granted: &'a BTreeSet<Capability>,
    store: &'a Store,
    /// The plugin's own id, recorded as the locator's `source` so a user can
    /// tell their own upload from something a plugin found.
    plugin_id: &'a str,
    /// Injected so `added_at` is testable without a clock. `None` means "read
    /// the system clock", which is the real host's behaviour.
    now: Option<&'a str>,
}

impl<'a> PluginHostApi<'a> {
    /// A host API over `store`, for the plugin identified by `plugin_id`.
    ///
    /// `granted` is the *intersection* the installer already computed — the
    /// operator's policy and the manifest's request — so this constructor
    /// cannot be used to hand a plugin more than the user agreed to. A caller
    /// that builds the set by hand has to do that arithmetic itself, which is
    /// why [`Installed::api`] exists and why it is the constructor a real host
    /// should use.
    pub fn new(store: &'a Store, granted: &'a BTreeSet<Capability>, plugin_id: &'a str) -> Self {
        PluginHostApi {
            granted,
            store,
            plugin_id,
            now: None,
        }
    }

    /// The same host, with a fixed `added_at`. For tests and for replaying a
    /// federation claim, where the row should record when the claim was
    /// accepted rather than when the row happened to be written.
    pub fn at(mut self, now: &'a str) -> Self {
        self.now = Some(now);
        self
    }
}

impl HostApi for PluginHostApi<'_> {
    fn granted(&self) -> &BTreeSet<Capability> {
        self.granted
    }

    fn propose_locator<'a>(
        &'a self,
        object_id: &'a str,
        scheme: &'a str,
        uri: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<LocatorOutcome, HostError>> + 'a>>
    {
        Box::pin(async move {
            // The capability check comes first, before the scheme is even
            // parsed: a plugin with no ProposeMetadata must not be able to
            // reach the consent code path, learn anything about an object's
            // tier from the shape of the error, or use the proposal as an
            // oracle for whether an object exists.
            if !self.granted.contains(&Capability::ProposeMetadata) {
                return Err(HostError::NoCapability(Capability::ProposeMetadata));
            }

            // An unknown scheme is a caller mistake, not a consent answer, and
            // it is reported as such. Passing an arbitrary string through would
            // put an opaque value in a column a later reader might treat as a
            // capability it is not.
            let scheme = LocatorScheme::parse(scheme).ok_or(HostError::Internal(format!(
                "unknown locator scheme {scheme:?}; the host implements {:?}",
                LocatorScheme::ALL.map(|s| s.as_str())
            )))?;

            let now = self.now.unwrap_or("").to_string();
            let proposed = ProposedLocator::new(scheme, uri, self.plugin_id);
            match locator::propose(self.store, object_id, &proposed, &now).await {
                Ok(locator_id) => Ok(LocatorOutcome::Stored { locator_id }),
                // A consent refusal is data, not an error: the plugin is told
                // the tier so it can explain itself, and the tier is the
                // object's *current* tier, which the plugin could not otherwise
                // read. Returning it here leaks one bit per object — the tier —
                // to a plugin that has no read access to consent. That is
                // deliberate: §5.18.1 says the plugin cannot influence the
                // tier, and a refusal it cannot interpret is a refusal it will
                // retry. What it cannot do is *widen* anything with the bit.
                Err(locator::ProposeError::TierForbidsLocator { tier, .. }) => {
                    Ok(LocatorOutcome::Refused {
                        scheme: scheme.as_str().to_string(),
                        object_tier: tier,
                    })
                }
                Err(locator::ProposeError::NoSuchObject(_)) => Err(HostError::NoSuchObject),
                Err(other) => Err(HostError::Internal(other.to_string())),
            }
        })
    }

    fn loopback_get(&self, url: &str) -> Result<Vec<u8>, HostError> {
        if !self.granted.contains(&Capability::LoopbackHttp) {
            return Err(HostError::NoCapability(Capability::LoopbackHttp));
        }
        if !is_loopback_url(url) {
            return Err(HostError::NotLoopback(url.to_string()));
        }
        Ok(Vec::new())
    }
}

/// The installer. Pure logic: given a manifest and a policy, decide.
pub struct Installer {
    policy: HostPolicy,
    installed: BTreeSet<String>,
}

impl Installer {
    pub fn new(policy: HostPolicy) -> Self {
        Self {
            policy,
            installed: BTreeSet::new(),
        }
    }

    pub fn policy(&self) -> &HostPolicy {
        &self.policy
    }

    pub fn installed_ids(&self) -> &BTreeSet<String> {
        &self.installed
    }

    /// The text shown on the install button, per §5.18.1.
    pub fn disclosure(&self, manifest: &Manifest) -> String {
        self.policy.disclosure(&manifest.requested)
    }

    /// Install, or refuse with the reason.
    pub fn install(&mut self, manifest: Manifest) -> Result<Installed, InstallError> {
        // T-P6-006: SCHEMA before POLICY, and this ordering is the point.
        //
        // A malformed manifest reported as "wrong API version" sends the
        // author to fix the wrong thing. And a manifest with
        // `id: "../../etc/passwd"` reaching the duplicate-id check has already
        // been recorded in a BTreeSet under a path-shaped key — which is the
        // shape a future uninstall will use to find it.
        manifest.validate()?;

        if manifest.api_version != API_VERSION {
            return Err(InstallError::ApiVersion {
                got: manifest.api_version,
                want: API_VERSION,
            });
        }
        if self.installed.contains(&manifest.id) {
            return Err(InstallError::AlreadyInstalled(manifest.id.clone()));
        }

        // A locator plugin that asks for `Network` is not a locator plugin.
        // §5.18.1 says the platform never speaks BitTorrent or ed2k; enforcing
        // that by kind means the property holds even if a manifest lies about
        // what it does.
        if manifest.kind == PluginKind::Locator {
            for c in &manifest.requested {
                if c.is_privileged() {
                    return Err(InstallError::LocatorRequestsPrivileged(*c));
                }
            }
        }

        for c in &manifest.requested {
            if self.policy.deny.contains(c) {
                // Reported by name, so the user can see which capability was the
                // problem rather than a generic refusal.
                return Err(InstallError::Denied(*c));
            }
        }

        let granted = self.policy.grant(&manifest.requested);
        if granted.is_empty() {
            return Err(InstallError::NoCapabilities);
        }

        self.installed.insert(manifest.id.clone());
        Ok(Installed { manifest, granted })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locator_plugin() -> Manifest {
        let mut requested = BTreeSet::new();
        requested.insert(Capability::ReadLibrary);
        requested.insert(Capability::ProposeMetadata);
        requested.insert(Capability::LoopbackHttp);
        Manifest {
            id: "commons-locator".into(),
            name: "P2P locators".into(),
            version: "1.0.0".into(),
            api_version: API_VERSION,
            requested,
            kind: PluginKind::Locator,
        }
    }

    /// The §5.18.1 property, stated as a test: the installed locator plugin can
    /// reach a local client and provably cannot reach anything else.
    #[tokio::test]
    async fn the_locator_plugin_can_reach_loopback_and_nothing_else() {
        let mut installer = Installer::new(HostPolicy::first_party());
        let inst = installer
            .install(locator_plugin())
            .expect("locator installs");

        assert!(inst.granted.contains(&Capability::LoopbackHttp));
        assert!(
            !inst.granted.contains(&Capability::Network),
            "a locator plugin must never hold the network capability"
        );
        assert!(!inst.granted.contains(&Capability::ProcessSpawn));

        // The host API needs a store now: `propose_locator` is a real write, so
        // a `PluginHostApi` that cannot reach a database is a type that cannot
        // be constructed. The loopback assertions below do not touch it, and
        // that is fine -- the point of this test is the *network* boundary.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_library(dir.path()).await.unwrap();
        let api = inst.api(&store);
        assert!(api
            .loopback_get("http://127.0.0.1:8080/api/v2/torrents/add")
            .is_ok());
        assert!(api.loopback_get("http://localhost:9090/").is_ok());
        assert!(api.loopback_get("https://[::1]:8080/").is_ok());

        // Every route out of the sandbox is refused.
        for hostile in [
            "http://example.com/",
            "https://tracker.example.org/announce",
            "http://192.168.1.10:8080/",
            "http://169.254.169.254/latest/meta-data/",
            "ftp://127.0.0.1/",
            "http://127.0.0.1.evil.com/",
            "http://[::]/",
            "http://2130706433/",
            "http://evil.com/?x=127.0.0.1",
            "http://user@evil.com/",
            "gopher://127.0.0.1/",
        ] {
            assert!(
                api.loopback_get(hostile).is_err(),
                "{hostile} must not be reachable from a locator plugin"
            );
        }
    }

    #[test]
    fn a_locator_plugin_requesting_the_network_is_refused_by_kind() {
        let mut m = locator_plugin();
        m.requested.insert(Capability::Network);

        // Even under a policy that would otherwise allow Network, kind wins.
        let mut permissive = HostPolicy::default();
        permissive.allow.insert(Capability::Network);
        let mut installer = Installer::new(permissive);

        assert_eq!(
            installer.install(m).unwrap_err(),
            InstallError::LocatorRequestsPrivileged(Capability::Network)
        );
        assert!(installer.installed_ids().is_empty());
    }

    #[test]
    fn a_plugin_asking_for_nothing_gets_nothing_and_is_refused() {
        let mut m = locator_plugin();
        m.requested.clear();
        let mut installer = Installer::new(HostPolicy::first_party());
        assert_eq!(
            installer.install(m).unwrap_err(),
            InstallError::NoCapabilities
        );
    }

    #[test]
    fn an_api_version_mismatch_is_refused_rather_than_adapted() {
        let mut m = locator_plugin();
        m.api_version = API_VERSION + 1;
        let mut installer = Installer::new(HostPolicy::first_party());
        assert_eq!(
            installer.install(m).unwrap_err(),
            InstallError::ApiVersion {
                got: API_VERSION + 1,
                want: API_VERSION,
            }
        );
    }

    #[test]
    fn installing_the_same_id_twice_is_refused() {
        let mut installer = Installer::new(HostPolicy::first_party());
        installer.install(locator_plugin()).unwrap();
        assert!(matches!(
            installer.install(locator_plugin()).unwrap_err(),
            InstallError::AlreadyInstalled(_)
        ));
    }

    #[test]
    fn a_denial_is_reported_by_name() {
        let mut policy = HostPolicy::default();
        policy.deny.insert(Capability::LoopbackHttp);
        let mut installer = Installer::new(policy);
        assert_eq!(
            installer.install(locator_plugin()).unwrap_err(),
            InstallError::Denied(Capability::LoopbackHttp)
        );
    }

    #[test]
    fn revocation_does_not_require_uninstalling() {
        // A granted capability can be withdrawn on the next start; the plugin is
        // then refused at install time, so no running instance keeps the power.
        let mut policy = HostPolicy::first_party();
        policy.deny.insert(Capability::LoopbackHttp);
        let mut installer = Installer::new(policy);
        assert!(installer.install(locator_plugin()).is_err());
    }

    #[test]
    fn the_install_disclosure_is_human_readable_and_names_refusals() {
        let installer = Installer::new(HostPolicy::first_party());
        let d = installer.disclosure(&locator_plugin());
        assert!(d.contains("this computer"), "{d}");
        assert!(
            !d.contains("LoopbackHttp"),
            "no bare capability tokens: {d}"
        );

        let mut m = locator_plugin();
        m.requested.insert(Capability::ProcessSpawn);
        let d = installer.disclosure(&m);
        assert!(d.contains("will not be able to"), "{d}");
        assert!(d.contains("start other programs"), "{d}");
    }

    #[tokio::test]
    async fn a_plugin_without_propose_metadata_cannot_propose_a_locator() {
        // The consent check lives in core, not the plugin. A plugin without the
        // capability cannot reach the code path at all.
        let mut m = locator_plugin();
        m.requested.retain(|c| *c == Capability::LoopbackHttp);
        let mut installer = Installer::new(HostPolicy::first_party());
        let inst = installer.install(m).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_library(dir.path()).await.unwrap();
        let err = inst
            .api(&store)
            .propose_locator("obj-1", "magnet", "magnet:?xt=urn:btih:abc")
            .await
            .expect_err("no ProposeMetadata, so no proposal");
        assert_eq!(err, HostError::NoCapability(Capability::ProposeMetadata));
    }

    #[test]
    fn loopback_detection_accepts_only_the_loopback_block() {
        for good in [
            "http://127.0.0.1/",
            "http://127.0.0.1:8080/api",
            "https://localhost:9090/",
            "http://[::1]:1234/",
            "http://127.0.0.53/",
        ] {
            assert!(is_loopback_url(good), "{good} should be loopback");
        }
        for bad in [
            "http://evil.com/?x=127.0.0.1",
            "http://127.0.0.1.evil.com/",
            "http://user@evil.com/",
            "https://10.0.0.1/",
            "http://[::]/",
            "not a url",
            "gopher://127.0.0.1/",
            // Alternate encodings of loopback are refused on purpose: the safe
            // answer for an unrecognised IPv4 form is "not loopback".
            "http://0x7f.1/",
            "http://2130706433/",
            "http://127.1/",
            "http://127.0.0.256/",
            "http://127.0.0.01/",
        ] {
            assert!(!is_loopback_url(bad), "{bad} must not be loopback");
        }
    }

    #[test]
    fn privileged_capabilities_are_named_as_such() {
        assert!(Capability::Network.is_privileged());
        assert!(Capability::ProcessSpawn.is_privileged());
        assert!(!Capability::LoopbackHttp.is_privileged());
        assert!(!Capability::ReadLibrary.is_privileged());
        assert!(!Capability::ProposeMetadata.is_privileged());
    }

    // ---------- T-P6-006: the manifest schema ----------

    /// The `semver` shape, as a table rather than the three cases a
    /// `MalformedVersion` test would use — because this function was WRONG on
    /// first write, and the bug was in the accept side.
    ///
    /// The rule is "no leading zero", and the first attempt was
    /// `!p.starts_with('0')`, which also refuses a bare `"0"` and therefore
    /// refuses `1.0.0` and `0.1.0`. Those two are the first entries below
    /// because they are the inputs a real version is made of.
    ///
    /// **These live HERE and not in `tests/manifest_schema.rs` because
    /// `is_semver` is private.** An integration test can only reach it by
    /// making it public, and a version checker is not part of this crate's
    /// public API — it is an implementation detail of `validate`.
    #[test]
    fn a_version_is_three_dot_separated_numbers() {
        for good in [
            "1.0.0",
            "0.0.0",
            "0.1.0",
            "10.20.30",
            "1.2.3-alpha.1",
            "1.2.3+build.5",
            "1.2.3-alpha.1+build.5",
        ] {
            assert!(is_semver(good), "{good:?} should be a version");
        }
        for bad in [
            "1.0",                 // too few parts
            "1",                   //
            "",                    // empty
            "latest",              // not a number at all
            "v2",                  // a v prefix is a convention, not semver
            "1.0.0.0",             // too many parts
            "01.0.0",              // leading zero
            "1.00.0",              // leading zero
            "1..0",                // empty part
            "1.0.",                // trailing dot
            " 1.0.0",              // leading space
            "1.0.0 ",              // trailing space
            "1.-1.0",              // negative
            "1.0.0a",              // trailing alphanumeric
            "\u{0661}.\u{0660}.0", // ARABIC-INDIC DIGITS
        ] {
            assert!(!is_semver(bad), "{bad:?} should NOT be a version");
        }
    }

    /// The last row above is the one worth keeping: `char::is_numeric()`
    /// returns `true` for Arabic-Indic digits and `is_ascii_digit` does not.
    /// A validator written with `is_numeric` accepts a version nobody can read
    /// aloud, and a version gets compared and sorted.
    #[test]
    fn the_version_check_is_ascii_only() {
        assert!(!is_semver("\u{0661}\u{0660}.\u{0660}.\u{0660}"));
        assert!(is_semver("1.0.0"));
    }

    /// The id rule, and the first-party fixture that constrains it.
    #[test]
    fn an_id_must_be_safe_to_use_as_a_name() {
        // `commons-locator` is the id `locator_plugin()` builds, and it belongs
        // to the one first-party plugin §11.4 names. If this ever starts
        // failing, the RULE changed — so change the fixture deliberately.
        // Do not quietly loosen the rule to save a test.
        assert!(
            is_safe_id("commons-locator"),
            "the first-party id must pass"
        );
        assert!(is_safe_id("com.example.locator"));

        for bad in [
            "",
            "..",
            "a..b",
            ".leading",
            "trailing.",
            "../../etc/passwd",
            "Com.Example",
            "com example",
            "com/example",
            "com\\example",
        ] {
            assert!(!is_safe_id(bad), "{bad:?} should be refused");
        }
    }

    /// The refusal names are compared as strings by
    /// `tests/manifest_schema.rs`, which makes them part of this crate's API.
    #[test]
    fn the_refusal_names_are_stable() {
        assert_eq!(
            [
                ManifestError::EmptyId,
                ManifestError::IdNotReverseDns(String::new()),
                ManifestError::EmptyName,
                ManifestError::MalformedVersion(String::new()),
                ManifestError::BadApiVersion(0),
            ]
            .map(|e| e.name()),
            [
                "EmptyId",
                "IdNotReverseDns",
                "EmptyName",
                "MalformedVersion",
                "BadApiVersion",
            ],
        );
    }

    /// `install` reports a schema problem as a schema problem, not as the
    /// policy refusal that happens to be checked next. A manifest with both a
    /// bad id and a wrong api_version must name the id: the version is a fact
    /// about the host, the id is a bug in the plugin, and only one of them is
    /// something the plugin's author can fix.
    #[test]
    fn a_malformed_manifest_is_reported_before_a_policy_refusal() {
        let mut i = Installer::new(HostPolicy::first_party());
        let err = i
            .install(Manifest {
                id: "../escape".into(),
                name: "x".into(),
                version: "1.0.0".into(),
                api_version: 9999,
                requested: BTreeSet::new(),
                kind: PluginKind::General,
            })
            .expect_err("refused");
        assert!(
            matches!(
                err,
                InstallError::Malformed(ManifestError::IdNotReverseDns(_))
            ),
            "{err:?}"
        );
    }

    /// Every `install` refusal in this module uses `locator_plugin()`, so
    /// schema validation must not have changed any of them — asserted
    /// directly rather than trusted. If validation ever rejected the fixture,
    /// every policy test here would be asserting on `Malformed` instead of on
    /// the policy it claims to test, and would still PASS while testing
    /// nothing. That is the failure this one assertion prevents.
    #[test]
    fn the_first_party_fixture_is_itself_valid() {
        assert_eq!(locator_plugin().validate(), Ok(()));
    }
}
