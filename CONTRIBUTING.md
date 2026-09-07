# Contributing to Remnant

Keep changes small and verifiable. A logical milestone should leave the repository buildable and should be committed separately with a conventional commit message such as `feat(oracle): add timeout-aware command execution`.

Before opening a pull request, run:

```bash
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
```

Never point experiments at production systems. Add or extend a fixture when a change affects persistence, restoration, or reduction behavior.
