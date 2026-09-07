# Remnant architecture

Remnant is a black-box, cross-service persistent-state reducer. The CLI, JSON output, and MCP interface all call the same library types.

## Runtime layers

1. Configuration validates a project file and resolves credentials only from environment variables.
2. Adapters implement StateSource. PostgreSQL captures schema metadata and JSON rows; Redis captures keys with native DUMP/RESTORE payloads.
3. Snapshot orchestration captures one typed baseline per source and derives stable StateObject identifiers.
4. The oracle runs the user-provided command and treats the configured exit code as failure reproduced.
5. ReductionEngine applies hierarchical ddmin. Each candidate is restored from the baseline, tested by the oracle, recorded, and followed by a full baseline restore.
6. SessionStore atomically persists the complete session after every experiment.
7. Reports include counts, retained objects, relationship hypotheses, and experiment evidence.

## Minimality guarantee

The current reducer reports 1-minimality: after reduction, removing any single retained object was tested and did not preserve the failure. This is a local guarantee, not a claim of global mathematical minimality. A candidate that times out or cannot be restored aborts the reduction instead of being classified as unnecessary.

## Relationship model

PostgreSQL scalar values and Redis key segments are matched deterministically to produce relationship hypotheses. They are useful for inspection and future grouping, but are marked unverified. The oracle experiments, not relationship heuristics or AI confidence, determine whether an object is necessary.

## Safety boundary

Mutating commands refuse suspicious non-local URLs unless the project enables safety.allow_non_local or the operator passes --allow-non-local. Replay also requires --confirm. Baseline payloads carry source fingerprints, and any adapter or restore error aborts the session.

## Extension point

New databases implement StateSource and define a serializable SourceSnapshot payload. The reducer does not need to know the storage technology. Future adapters can add message queues, files, MongoDB, S3-compatible storage, or Kubernetes-backed state without creating a second reduction implementation.
