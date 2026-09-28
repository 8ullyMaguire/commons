//! T-P6-006, accept criterion A1: the manifest schema validates a valid
//! plugin and rejects an invalid one.
//!
//! # The shape of this file
//!
//! One table of cases, applied through the SAME public entry point in every
//! row. A manifest that is validated by one path and installed by another is a
//! manifest that will end up validated by neither.
//!
//! # Why `verdict` returns a NAME rather than a bool
//!
//! A table of twenty cases where every one prints `false` tells you nothing
//! about which class regressed. The names are part of the API — §5.18.1 shows
//! the user *which* part of their plugin is wrong, and a single `Invalid`
//! cannot do that — so they are pinned by
//! [`the_refusal_names_are_stable_strings`] rather than left free.

use commons_plugin::{Capability, HostPolicy, InstallError, Installer, Manifest, ManifestError};

/// A well-formed manifest, and the single source of every other case here.
///
/// Every case is this string with ONE edit, so a case cannot be wrong in two
/// ways at once and a failure names the edit rather than the fixture.
const GOOD: &str = r#"{
  "id": "com.example.locator",
  "name": "Example Locator",
  "version": "1.0.0",
  "api_version": 1,
  "requested": ["read_library", "loopback_http"]
}"#;

/// Parse, then validate, and return the NAME of the refusal — or `"ok"`.
///
/// A parse failure is reported as `"parse error"` rather than a
/// `ManifestError`, because a key serde rejects is a *schema* refusal and
/// `validate` never sees it. A case that lands here has to be written as a
/// parse-error case, not a validate-error case, and the string is what makes
/// that distinction visible instead of guessed at.
fn verdict(base: &str, edit: impl FnOnce(&str) -> String) -> &'static str {
    let json = edit(base);
    let m: Manifest = match serde_json::from_str(&json) {
        Ok(m) => m,
        Err(_) => return "parse error",
    };
    match m.validate() {
        Ok(()) => "ok",
        Err(e) => e.name(),
    }
}

// ---------------------------------------------------------------- A1: valid

#[test]
fn a_well_formed_manifest_parses_and_validates() {
    let m: Manifest = serde_json::from_str(GOOD).expect("a good manifest parses");
    assert_eq!(m.id, "com.example.locator");
    assert_eq!(m.name, "Example Locator");
    assert_eq!(m.version, "1.0.0");
    assert!(m.requested.contains(&Capability::ReadLibrary));
    assert!(m.requested.contains(&Capability::LoopbackHttp));
    assert_eq!(m.validate(), Ok(()), "and it is valid");
}

/// The control for every other case in this file. If `verdict(GOOD, |s| s)`
/// is not `"ok"`, every failing case below is failing for the wrong reason
/// and the file needs debugging before its results mean anything.
#[test]
fn the_unedited_manifest_is_valid() {
    assert_eq!(verdict(GOOD, |s| s.to_string()), "ok");
}

// ------------------------------------------------- A1: invalid, value by value

#[test]
fn an_empty_id_is_refused() {
    assert_eq!(
        verdict(GOOD, |s| s.replace("com.example.locator", "")),
        "EmptyId",
    );
}

#[test]
fn an_id_that_is_a_path_is_refused() {
    // NOT "non-empty". `../../etc/passwd` is non-empty, and the id is the key
    // a reinstall (#6987) and an uninstall will look up.
    assert_eq!(
        verdict(GOOD, |s| s
            .replace("com.example.locator", "../../etc/passwd")),
        "IdNotReverseDns",
    );
}

#[test]
fn an_uppercase_or_spacey_id_is_refused() {
    for bad in ["Com.Example", "com example", "com/example"] {
        let v = verdict(GOOD, |s| s.replace("com.example.locator", bad));
        assert_eq!(v, "IdNotReverseDns", "{bad:?}");
    }
    // A BACKSLASH is a valid id character to refuse, but it cannot be tested
    // through `verdict`: a raw `\` inside a JSON string is an invalid escape,
    // so serde reports a PARSE error before `validate` ever runs. The rule
    // still refuses it -- `is_safe_id` allows no backslash -- and the
    // integration test cannot see that. Asserted through `validate` on a
    // constructed manifest instead, so the coverage is real rather than
    // accidentally absent.
    let m: Manifest = serde_json::from_str(GOOD).expect("parses");
    let mut with_backslash = m.clone();
    with_backslash.id = r"com\example".into();
    assert_eq!(
        with_backslash.validate(),
        Err(ManifestError::IdNotReverseDns(r"com\example".into())),
    );
}

#[test]
fn an_empty_name_is_refused() {
    // A plugin with no name shows the user a blank row in a list of plugins.
    assert_eq!(
        verdict(GOOD, |s| s.replace("Example Locator", "")),
        "EmptyName"
    );
}

#[test]
fn a_version_that_is_not_semver_is_refused() {
    for bad in ["latest", "v2", "1.0", "1.0.0.0", "01.0.0", ""] {
        let v = verdict(GOOD, |s| s.replace("\"1.0.0\"", &format!("\"{bad}\"")));
        assert_eq!(v, "MalformedVersion", "{bad:?}");
    }
}

#[test]
fn a_zero_api_version_is_refused() {
    // `install` refuses a *mismatch* against API_VERSION, which leaves 0 and
    // 9999 equally "a mismatch". Neither is a version anybody built against;
    // 0 in particular reads as "unset" in a language whose default is 0.
    assert_eq!(
        verdict(GOOD, |s| s
            .replace("\"api_version\": 1", "\"api_version\": 0")),
        "BadApiVersion",
    );
}

/// The other api_version cases are POLICY, not schema. A plugin built for a
/// future sandbox is a well-formed manifest that this host happens not to
/// speak to; `install` refuses it, and `validate` must not.
#[test]
fn an_api_version_this_host_does_not_speak_is_a_policy_refusal_not_a_schema_one() {
    let m: Manifest =
        serde_json::from_str(&GOOD.replace("\"api_version\": 1", "\"api_version\": 9999"))
            .expect("parses");
    assert_eq!(m.validate(), Ok(()), "well-formed, just not for us");

    let mut i = Installer::new(HostPolicy::first_party());
    assert!(
        matches!(
            i.install(m),
            Err(InstallError::ApiVersion { got: 9999, .. })
        ),
        "and the policy layer is what refuses it"
    );
}

/// The separator case, which is the whole reason `is_reverse_dns` is not a
/// length check. `..` is what makes a path traversal a path traversal.
#[test]
fn a_double_dot_in_an_id_is_refused() {
    for bad in ["a..b", "..", ".leading", "trailing."] {
        let v = verdict(GOOD, |s| s.replace("com.example.locator", bad));
        assert_eq!(v, "IdNotReverseDns", "{bad:?}");
    }
}

// --------------------------------------------- A1: invalid, key by key (serde)

#[test]
fn a_misspelled_field_is_refused() {
    // `capabilties`. WITHOUT `deny_unknown_fields` this parses and is
    // silently ignored — the plugin installs with no capabilities at all, and
    // its author never finds out why.
    let json = GOOD.replace("\"requested\"", "\"capabilties\"");
    assert!(
        serde_json::from_str::<Manifest>(&json).is_err(),
        "a typo'd key must not be ignored -- the plugin would install inert"
    );
}

#[test]
fn a_missing_required_field_is_refused() {
    // `requested` has no `#[serde(default)]`, so omitting it is an error.
    // Worth pinning because §11.4's manifest schema is a stability promise
    // and a field quietly becoming optional is a breaking change made by
    // adding one attribute.
    let json = GOOD.replace(
        r#""requested": ["read_library", "loopback_http"]"#,
        r#""capabilities": ["read_library"]"#,
    );
    assert!(
        serde_json::from_str::<Manifest>(&json).is_err(),
        "an unknown key must not stand in for a missing one"
    );
}

#[test]
fn an_unknown_kind_is_refused() {
    let v = verdict(GOOD, |s| s.replace("}", r#""kind": "supervisor"}"#));
    assert_eq!(
        v, "parse error",
        "an unknown kind is a serde error, not a value one"
    );
}

// ----------------------------------------- A2: the ticket's "done when"

#[test]
fn a_capability_that_does_not_exist_fails_to_instantiate() {
    // The unknown-NAME half. serde already catches it, and it is here because
    // it is the criterion the ticket names — a test that disappears when the
    // behaviour it guards is refactored has stopped guarding anything.
    for typo in ["loopback_htt", "read-library", "READ_LIBRARY", "net"] {
        let json = GOOD.replace("\"loopback_http\"", &format!("\"{typo}\""));
        assert!(
            serde_json::from_str::<Manifest>(&json).is_err(),
            "{typo:?} is not a capability; it must not instantiate"
        );
    }
}

#[test]
fn a_manifest_requesting_nothing_is_refused_rather_than_installed_useless() {
    // The other half. An empty `requested` is *schema*-valid and `install`
    // already refuses it with `NoCapabilities`, so this asserts the two
    // layers AGREE rather than duplicating the installer. Both layers having
    // an opinion is the property; which one speaks first is not.
    let m: Manifest =
        serde_json::from_str(&GOOD.replace(r#"["read_library", "loopback_http"]"#, "[]"))
            .expect("an empty request parses");
    assert_eq!(m.validate(), Ok(()), "an empty request is well-formed...");

    let mut i = Installer::new(HostPolicy::first_party());
    assert!(
        matches!(i.install(m), Err(InstallError::NoCapabilities)),
        "...and the policy layer refuses it. Two layers, one outcome."
    );
}

/// A plugin asking for a privileged capability is a POLICY question, never a
/// schema one. §11.4 expects the first-party scrapers to ask for internet
/// access, so a validator that rejects `network` as malformed would make the
/// plugin API unusable for exactly the plugins the spec names.
#[test]
fn asking_for_the_network_is_well_formed_even_though_it_is_privileged() {
    let m: Manifest = serde_json::from_str(
        &GOOD.replace(r#"["read_library", "loopback_http"]"#, r#"["network"]"#),
    )
    .expect("parses");
    assert_eq!(
        m.validate(),
        Ok(()),
        "a schema check that refused this would break the scraper plugins"
    );
    assert!(
        Capability::Network.is_privileged(),
        "while still being privileged"
    );

    // And the first-party policy — which is the thing that actually refuses
    // it — still does. Note which variant: NOT `Denied`, because
    // `HostPolicy::first_party()` has an empty `deny` set. `network` simply
    // is not in `allow`, so the intersection comes out empty and install
    // reports `NoCapabilities`.
    //
    // That distinction is worth keeping rather than papering over: `Denied`
    // means "the operator has forbidden this and no grant will change it",
    // and `NoCapabilities` means "you asked for something nobody here
    // allows". A user who adds `network` to `allow` gets a working plugin
    // (the next test); a user who cannot is looking at a policy, not at a
    // bug in the manifest.
    let mut i = Installer::new(HostPolicy::first_party());
    assert!(
        matches!(i.install(m), Err(InstallError::NoCapabilities)),
        "the refusal belongs to the policy, and the default policy is narrow"
    );
}

/// A user who WANTS to grant it can. This is the other half of the test
/// above, and the reason the split between schema and policy matters: if
/// validation refused privileged requests there would be no way to grant
/// them at all.
#[test]
fn a_user_who_grants_the_network_gets_a_working_plugin() {
    let mut policy = HostPolicy::first_party();
    policy.allow.insert(Capability::Network);

    let m: Manifest = serde_json::from_str(
        &GOOD.replace(r#"["read_library", "loopback_http"]"#, r#"["network"]"#),
    )
    .expect("parses");
    let installed = Installer::new(policy)
        .install(m)
        .expect("a user may grant it");
    assert!(installed.granted.contains(&Capability::Network));
}

// ------------------------------------------------------------- the contract

#[test]
fn the_refusal_names_are_stable_strings() {
    // `verdict` compares strings, so these names are part of this crate's API.
    // Delete this test and they become free to change, and every `verdict`
    // assertion above silently starts passing or failing for the wrong reason.
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

/// A refusal message names the field without leaking the rest of the manifest.
#[test]
fn a_refusal_message_names_the_field_without_the_rest_of_the_manifest() {
    let e = ManifestError::IdNotReverseDns("../../etc/passwd".into());
    let msg = e.to_string();
    assert!(
        msg.contains("../../etc/passwd"),
        "the offending value helps: {msg}"
    );
    assert!(
        !msg.contains("Example Locator"),
        "and nothing else does: {msg}"
    );
}
