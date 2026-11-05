# standalone-check

Proof for capability-port row 5.4: **the four `commons` crates are usable on their own** — a service can
take one without taking the estate's schema.

This crate is deliberately **not** a workspace member. It depends on the four crates by path, uses the API
a stranger would use, and prints what it got:

```bash
cargo run --manifest-path consumers/standalone-check/Cargo.toml
```

If a crate secretly needed the estate (a domain type, a shared schema, a workspace-inherited feature), this
would not compile — which is why the check is a build rather than a claim.

It earned its place immediately: writing it against the real signatures found that `strip_markdown` left
`[link](https://…)` intact (markdown in a plain-text feed) and deleted the underscores in `snake_case`, both
of which `commons-feed`'s own tests had not covered. Fixed, with `markdown_flattening_keeps_words_and_drops_syntax`
pinning the cases this consumer found.
