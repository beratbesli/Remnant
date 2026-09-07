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

The PostgreSQL adapter enumerates non-system base tables and captures rows as JSON. Tables with primary keys use key values for stable object IDs; tables without primary keys use a row-content digest. Redis keys are captured with native serialized payloads and TTLs, allowing type-preserving restoration.
