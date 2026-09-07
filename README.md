# Remnant

Reduce a broken application to the smallest persistent state that still reproduces it.

A production-like environment may contain millions of database rows and cache entries. A bug may depend on only five of them. Remnant experimentally removes state, restores the environment, reruns your reproducer, and records which state is actually required for the failure to exist.

The core is deterministic and works without AI or cloud services. AI integrations, when enabled, call the same Remnant operations and cannot replace experimental verification.

## Status

The first usable workflow is implemented: PostgreSQL and Redis snapshots, a command oracle, resumable reduction sessions, JSON/text reports, deterministic relationship hypotheses, and a Docker Compose fixture.

## Quick start

```bash
cargo run -- init
export DATABASE_URL=postgres://localhost/remnant_fixture
export REDIS_URL=redis://127.0.0.1/
cargo run -- doctor
```

The generated `remnant.yaml` keeps credentials out of the repository by referring to environment variables. Destructive operations will require a verified environment fingerprint before they can run.

## End-to-end fixture

With Docker and Docker Compose installed:

    ./fixtures/checkout/run-demo.sh

The fixture seeds 101 PostgreSQL users, 51 subscriptions, and 101 Redis keys. Its reproducer fails only when the active pro subscription and the stale user:174:plan Redis key coexist. Remnant verifies the failure, captures a baseline, performs real restore-and-oracle experiments, and emits a reduction report.

To run an assertion-based fixture check:

    ./fixtures/checkout/test-fixture.sh

The fixture uses host ports 55432 for PostgreSQL and 56379 for Redis. Tear it down, including its data volumes, with:

    docker compose -f fixtures/checkout/docker-compose.yml down -v

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

The Docker-based integration fixture is described above; the optional TUI and MCP stdio interface are planned for the next milestone.

## License

Apache-2.0. See [LICENSE](LICENSE).
