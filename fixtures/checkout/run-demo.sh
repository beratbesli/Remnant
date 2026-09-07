#!/usr/bin/env bash
set -euo pipefail

compose_file="fixtures/checkout/docker-compose.yml"
project_file="fixtures/checkout/remnant.yaml"

docker compose -f "$compose_file" up -d --build
docker compose -f "$compose_file" run --rm app python /app/seed.py

export DATABASE_URL="postgres://remnant:remnant@127.0.0.1:55432/remnant"
export REDIS_URL="redis://127.0.0.1:56379/0"

cargo run --quiet -- --project "$project_file" doctor
cargo run --quiet -- --project "$project_file" verify
cargo run --quiet -- --project "$project_file" reduce
cargo run --quiet -- --project "$project_file" report
