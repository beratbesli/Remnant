# Remnant

Reduce a broken application to the smallest persistent state that still reproduces it.

A production-like environment may contain millions of database rows and cache entries. A bug may depend on only five of them. Remnant experimentally removes state, restores the environment, reruns your reproducer, and records which state is actually required for the failure to exist.

The core is deterministic and works without AI or cloud services. AI integrations, when enabled, call the same Remnant operations and cannot replace experimental verification.

## Status

Remnant is under active development toward its first usable release. The current foundation includes a typed, validated project configuration and a working CLI bootstrap. PostgreSQL, Redis, snapshot, reduction, reporting, and agent-interface milestones are being added incrementally.

## Quick start

```bash
cargo run -- init
export DATABASE_URL=postgres://localhost/remnant_fixture
export REDIS_URL=redis://127.0.0.1/
cargo run -- doctor
```

The generated `remnant.yaml` keeps credentials out of the repository by referring to environment variables. Destructive operations will require a verified environment fingerprint before they can run.

## Design principles

- Black-box oracle execution is the authority for every reduction decision.
- Every experiment starts from a known baseline snapshot and is persisted for resumption.
- PostgreSQL and Redis are adapter implementations, not special cases in the reducer.
- Safety checks favor isolated local environments and fail closed when restoration is uncertain.
- Human CLI, JSON automation, and optional MCP tooling use one core library.

## Development

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

The Docker-based integration fixture will be documented here as soon as the adapter and reducer milestones are complete.

## License

Apache-2.0. See [LICENSE](LICENSE).
