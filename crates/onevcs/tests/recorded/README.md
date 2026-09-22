# Recorded streams

One stream this crate wrote on a real host, held by `tests/recorded.rs` to
round-trip through this crate's own reader and `serde_json::to_string` with no
byte changed. Nothing in the file was edited; a fixture that had been touched
would prove the touch rather than the producer.

- `onevcs-session.ndjson` — the whole stream of the `onevcs` publication session
  `publish-branch-onevcs-s-b5c195333f94` (7 envelopes, `v: 1`, source `vcs`,
  `phase` stamped on every line).

## Provenance

Copied byte for byte from `onemessagebus` at tag `onemessagebus-agent-v0.8.0`,
path `crates/onemessagebus-agent/tests/recorded/onevcs-session.ndjson`, where it
was held to the same property by that crate's `tests/recorded.rs`. Its paragraph
of that directory's `README.md` is the bullet above, unchanged.

The words on those lines are this crate's — the source `vcs`, the kinds, the four
phases — so the fixture came here when the vocabulary did. The generic envelope
shape around them is still `onemessagebus`'s; what this fixture proves is that
moving the words out of a shared profile crate and into
`crates/onevcs/src/vocabulary.rs` moved no byte on the wire.

`sha256(onevcs-session.ndjson)` is
`f060b169e9d43c4d12bd821131d3e4e6be746d3516dbac7effe83b62c554a256`, at that tag
and here.
