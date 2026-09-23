# Configuration reference

Remnant reads YAML from remnant.yaml by default.

    version: 1

    project:
      name: checkout-service

    sources:
      postgres:
        type: postgres
        url_env: DATABASE_URL
        schema: public
      redis:
        type: redis
        url_env: REDIS_URL
        database: 0

    oracle:
      command: ./scripts/reproduce.sh
      timeout: 30s
      failure_exit_code: 1

    reduction:
      strategy: hierarchical
      state_dir: .remnant
      max_experiments: 10000

    safety:
      allow_non_local: false
      require_fingerprint: true

Credentials are never generated into this file. url_env names environment variables resolved at runtime. Supported duration suffixes are ms, s, and m. The oracle is executed by the platform shell, so keep the command in a reviewed script for repeatability.

When `require_fingerprint` is true, run `remnant doctor` to read the connected target fingerprint, verify the databases, and pass `--target-fingerprint <value>` to `capture`, `reduce`, `resume`, and `replay`. The fingerprint is built from identities queried from the connected PostgreSQL database and Redis server, not from snapshot contents. Redis restarts change its run ID, so obtain a fresh fingerprint after a restart. `--allow-non-local` does not bypass fingerprint confirmation. Unknown single-label DNS names are treated as non-local.

The PostgreSQL adapter enumerates non-system base tables and captures rows as JSON, along with sequence `last_value` and `is_called` state. Tables with primary keys use key values for stable object IDs; tables without primary keys use a row-content digest. Redis keys are captured with native serialized payloads and TTLs, allowing type-preserving restoration.
