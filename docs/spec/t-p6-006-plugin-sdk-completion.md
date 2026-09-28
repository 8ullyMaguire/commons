---
title: T-P6-006 — Plugin SDK completion
date: 2026-09-28
status: proposed
ticket: T-P6-006
platform-spec: §11.4
---

# T-P6-006 — Plugin SDK completion

**Status: not started.** Written after T-P6-005 was completed, gated and
tagged (`phase-7-070-cast-dlna-external-players`, `b1ec6b3`).

**Platform spec:** `~/secondbrain/10-Projects/2026-09-26T110000+0200-commons-platform-spec.md`
**§11.4**, "Plugin and scraper SDK (C67)".

---

## 1. What already exists, and why the ticket is smaller than it looks

`crates/commons-plugin/src/lib.rs` is 735 lines and is not a stub. Read
before planning, because the ticket's own file list implies otherwise:

| Thing §11.4 promises | State |
|---|---|
| `Capability` enum, 5 variants, `snake_case` on the wire | **exists** |
| `Capability::disclosure()` — the §5.18.1 plain-language sentence | **exists** |
| `Capability::is_privileged()` | **exists** |
| `HostPolicy` with `allow`/`deny`, `deny` always winning | **exists** |
| `HostPolicy::disclosure()` naming grants *and* refusals | **exists** |
| `Manifest` struct, `PluginKind` (Locator/General) | **exists** |
| `InstallError` — 5 distinct refusal reasons | **exists** |
| `API_VERSION` and the refuse-don't-adapt rule | **exists** |
| `HostApi` trait + `PluginHostApi` capability enforcement | **exists** |
| `Installer::install` | **exists** |
| 12 unit tests + 21 integration tests | **exist and pass** |
| `api.rs` | absent |
| `ui/src/lib/plugins/` | absent |

So the *policy* layer is done and well covered. Both accept criteria are
unmet, and they are unmet for one reason: **there is no validation step
between "read the manifest" and "act on it."**

## 2. The gap, measured rather than assumed

A throwaway probe (`tests/capprobe2.rs`, run then deleted) fed twelve
malformed manifests to `serde_json::from_str::<Manifest>`. **Eleven were
accepted:**

```
ACCEPTED misspelled field:  ... ,"capabilties":[]      <- a typo'd key, ignored
ACCEPTED empty id:          id=""
ACCEPTED empty version:     version=""
ACCEPTED empty name:        name=""
ACCEPTED id with a path:    id="../../etc/passwd"
ACCEPTED locator requesting network: requested={Network}, kind=locator
ACCEPTED api_version 0:     api=0
ACCEPTED api_version 9999:  api=9999
ACCEPTED duplicate capabilities: requested={ReadLibrary}   (twice, deduped)
REJECTED capability as an object                           <- the only one
```

Only a **wrong-typed value** is caught, because only that is something serde
checks by construction. Everything that is *semantically* wrong passes.

**This is the same class of bug as T-P6-005's `deny_unknown_fields` finding,
one level down.** There it was a nested config block that silently ignored a
typo. Here it is the trust boundary of a plugin sandbox: a manifest is
attacker-controlled by definition, and `Manifest` is documented as "every
field is untrusted" — while having no validation at all.

## 3. Why this is a security property, not a tidiness one

`Installer::install` does check several things: the API version, a duplicate
id, a Locator asking for a privileged capability, a denied capability, and an
empty grant. Those are real checks and they are tested.

But **every one of them is downstream of fields that parse can set to
anything.** A manifest with `id: "../../etc/passwd"` installs under an id that
is a path traversal, and the id is what the installer records and what a
future "uninstall", "reinstall" (#6987) or "update" path will use as a
lookup key. A manifest with `api_version: 0` reaches `install`'s version
check and is refused — good — but the refusal is a *policy* decision made
after parsing, and a caller that displays a manifest without installing it has
no protection at all.

And `is_privileged` is only consulted **when `kind == Locator`**. A manifest
declaring `kind: "general"` with `requested: ["network", "process_spawn"]`
parses and reaches `install`, where `HostPolicy::default().allow` excludes
both, so it is refused — but only because the *default policy* is narrow, not
because anything about the manifest is invalid.

**So the design decision for this ticket:** validation belongs on the
`Manifest` itself, is callable without an `Installer`, and returns named
reasons. Not inside `install`, because `install` cannot be the only thing
that reads a manifest.

## 4. Acceptance criteria

The plan names two, and the second is the "done when".

### A1 — the manifest schema validates a valid plugin and rejects an invalid one

A documented, callable validation that returns **named** reasons, tested
against both a well-formed manifest and each malformed class from §2.

Named rather than a bool, because the disclosure surface needs to tell a user
*which* part of their plugin is wrong, and a single `false` cannot. This is
the same reasoning as `InstallError` having five distinct variants.

### A2 — a plugin with an undeclared capability fails to instantiate

The plan's "done when". A manifest claiming a capability that does not exist
must fail **at parse/validate**, not be silently dropped and not be granted.

Note the probe showed `requested: ["read_library", "read_library"]`
deduplicating silently — which is correct and fine — but also showed that an
unknown *string* in that array is a hard serde error already. So the
"undeclared capability" case has two halves: an unknown name (already caught,
and must stay caught) and a known name the manifest does not otherwise
declare (which is not currently expressible, and should be).

## 5. The five ways this is wrong

1. **Validating inside `install` instead of on `Manifest`.** Then
   `Manifest::validate` does not exist, anything else that reads a manifest
   skips the checks, and the "undeclared capability" test has to go through an
   `Installer` to reach the only validation there is.
2. **`deny_unknown_fields` without also validating values.** Adding the
   attribute fixes the typo'd-key row and nothing else — ten of the twelve
   probe rows still pass. An implementer who stops there will have closed one
   row of the probe table and believed the ticket done.
3. **A `validate()` that returns `bool`.** Then A1's "rejects an invalid
   plugin" cannot say *why*, and the §5.18.1 disclosure cannot name the field.
4. **Rejecting every manifest that asks for a privileged capability.** That
   is a *policy* decision (`HostPolicy`, already implemented and tested), not
   a schema decision. A manifest asking for `network` is well-formed; a user
   may grant it. Conflating the two makes the plugin API unusable for the
   first-party scraper plugins, which §11.4 says are expected to ask for
   internet access.
5. **Validating the id as "any non-empty string".** `../../etc/passwd` is
   non-empty. If the id is ever used as a path — and a reinstall (#6987) and
   an uninstall both want a stable key — the id needs a shape, not a
   non-emptiness check. Reverse-DNS, lowercase, `[a-z0-9._-]`, no separators
   that mean anything to a filesystem.

## 6. Not in this ticket

- **The WASM sandbox itself** (§4.4). This ticket is the manifest boundary.
  Sandboxing is a separate, much larger piece of work, and pretending a
  validated manifest is a sandboxed plugin is how you ship a plugin API that
  reads as safe and is not.
- **`ui/src/lib/plugins/`**, the services tab (#5118), settings UI with
  defaults (#5002, #6899), toasts (#1695), reinstall (#6987), the
  pre-install backup (#6185). All named in the plan's file list; all UI or
  workflow features that need a UI to verify. This ticket makes the boundary
  they will sit on trustworthy.
- **Scraper plugin features** (§11.4's rate limits, pinned scrapers, overlap
  warnings, health dashboard). Real work, separate ticket; they consume this
  API rather than needing it changed.

## 7. Files

New:
- `crates/commons-plugin/tests/manifest_schema.rs` — A1 and A2.
- `docs/spec/t-p6-006-plugin-sdk-completion.md` — this file.

Modified:
- `crates/commons-plugin/src/lib.rs` — `deny_unknown_fields` on `Manifest`,
  a `ManifestError` enum, and `Manifest::validate`.
- Possibly split into `src/api.rs` if the module grows past what belongs in
  `lib.rs`. The plan names that file; it is not required, and moving code to
  satisfy a file list is worse than a 900-line `lib.rs`. Decide when writing.
