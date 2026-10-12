# saphyr-diff — the serde-saphyr spike's differential harness

Throwaway (spike `serde-yaml-replacement/spike-onemessagebus-yaml`; never lands).
It reads this repository's real YAML with `serde_norway` 0.9 (what the core uses
today) and with `serde-saphyr` 1.3 (and `saphyr` 0.2's node tree for the
schema-less walks), and prints every difference in what is read, what is
written, and what errors say, per document kind, with the serde-saphyr option or
code change that restores today's behaviour.

    cargo run -q -p saphyr-diff --release

- `corpus/captured/` — every configuration, event filter and predicate text the
  core, CLI and e2e suites hand to `Config::parse`, `Filter::parse` and
  `Predicate::read`. Re-capture with `spikes/saphyr-diff/capture.sh` (applies
  `capture.patch`, runs the suites, reverts).
- The docs' YAML blocks, the asserted error cases, onevcs's filter cases, the SDK
  spellings and the labelled synthetic probes are built in `src/corpus.rs`.
