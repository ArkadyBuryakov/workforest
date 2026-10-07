# Code conventions

- Rust (stable, edition 2024). Verify changes with `make check` (rustfmt
  check, clippy with warnings denied, the tests under `cargo llvm-cov` with
  a 90% line-coverage floor). Run `cargo fmt` rather than hand-formatting.
- When a return value or constant bundles fields whose positions carry
  meaning, use a small named struct, not an anonymous tuple. Maps are for
  genuine key→value lookups only (environments, branch→remotes); where
  order is part of the meaning (config entries), an `IndexMap`.
- Errors are `errors::Error` — a kind, which decides the exit code, and
  the message the user reads. No panics on user input or on the state of
  the repository; `unsafe` only around a syscall, with a `SAFETY:` line
  saying why it holds.
- Pre-1.0 with zero users: on breaking changes keep the clean design and
  state what to re-run (e.g. re-eval shell-init). Never add
  backward-compatibility shims.
