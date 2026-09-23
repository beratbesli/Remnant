#!/usr/bin/env bash
set -euo pipefail

compose_file="fixtures/checkout/docker-compose.yml"
project_file="fixtures/checkout/remnant.yaml"

docker compose -f "$compose_file" up -d --build
docker compose -f "$compose_file" run --rm app python /app/seed.py

export DATABASE_URL="postgres://remnant:remnant@127.0.0.1:55432/remnant"
export REDIS_URL="redis://127.0.0.1:56379/0"

cargo run --quiet -- --project "$project_file" verify
target_fingerprint=$(cargo run --quiet -- --project "$project_file" doctor --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_fingerprint"])')
if cargo run --quiet -- --project "$project_file" reduce --json --target-fingerprint wrong > /tmp/remnant-wrong-target.json 2>&1; then
  echo "wrong target fingerprint unexpectedly permitted reduction" >&2
  exit 1
fi
cargo run --quiet -- --project "$project_file" reduce --json --target-fingerprint "$target_fingerprint" > /tmp/remnant-fixture-result.json
cargo run --quiet -- --project "$project_file" report --json > /tmp/remnant-fixture-report.json

docker compose -f "$compose_file" exec -T postgres psql -U remnant -d remnant -c "SELECT setval('fixture_sequence', 5, false)" > /dev/null
cargo run --quiet -- --project "$project_file" replay --confirm --target-fingerprint "$target_fingerprint" > /dev/null
sequence_state=$(docker compose -f "$compose_file" exec -T postgres psql -U remnant -d remnant -Atc "SELECT last_value::text || ':' || is_called::text FROM fixture_sequence")
test "$sequence_state" = "451:true"

python3 - <<'PY'
import json

with open("/tmp/remnant-fixture-result.json", encoding="utf-8") as handle:
    result = json.load(handle)
with open("/tmp/remnant-fixture-report.json", encoding="utf-8") as handle:
    report = json.load(handle)

assert result["failure_reproduced"] is True
assert result["retained_count"] <= 3, result
assert report["failure_reproduced"] is True
assert report["reduced_count"] == result["retained_count"]
assert report["experiments"] > 0
print(f"fixture reduction verified: {report['original_count']} -> {report['reduced_count']} objects")
PY
