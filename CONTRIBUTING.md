# Contributing

1. `make verify` must pass: fmt-check, clippy with warnings denied, the
   tests, the smoke test.
2. Fixtures are synthetic: built from the shapes Labcorp answers with,
   never copied from a real answer.
3. No personal data anywhere: code, docs, commit messages, pull requests,
   issues. `tests/no_personal_data.rs` enforces the shapes; review the rest.
4. A new read comes with its endpoint and traps in `docs/api.md`.
5. Squash merges only, with an approval and green CI.
