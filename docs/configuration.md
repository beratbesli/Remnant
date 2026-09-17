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
      max_output_bytes: 65536

    reduction:
      strategy: hierarchical
      state_dir: .remnant
      max_experiments: 10000

    safety:
      allow_non_local: false
      require_fingerprint: true

Credentials are never generated into this file. `url_env` names environment variables resolved at runtime. Supported duration suffixes are `ms`, `s`, and `m`.

With no `args` field, `command` retains the compatible shell-command behavior; keep that command in a reviewed script. For a shell-free invocation, set `command` to the program path and provide structured arguments:

    oracle:
      command: ./scripts/reproduce
      args: [--scenario, known-bug]
      timeout: 30s
      failure_exit_code: 1

Only `failure_exit_code` means the known failure was reproduced. Exit code `0` means it was absent; all other codes, launch failures, and timeouts invalidate the experiment. Remnant starts each oracle in its own process group and kills that group on timeout. It retains at most `max_output_bytes` from each output stream (default 64 KiB, maximum 1 MiB).

The PostgreSQL adapter enumerates non-system base tables and captures rows as JSON. Tables with primary keys use key values for stable object IDs; tables without primary keys use a row-content digest. Redis keys are captured with native serialized payloads and TTLs, allowing type-preserving restoration.
