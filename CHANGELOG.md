# Changelog

## 0.1.0

- Added typed project configuration and validation.
- Added timeout-aware command failure oracle.
- Added PostgreSQL and Redis snapshot/restore adapters.
- Added persistent hierarchical ddmin reduction sessions and 1-minimality reporting.
- Added safety checks, text/JSON reports, relationship hypotheses, and resumable CLI commands.
- Added a controlled MCP-compatible JSON-RPC stdio interface.
- Added a Docker Compose PostgreSQL + Redis checkout fixture with noise and a deterministic failure oracle.

Known limitations: Docker is required for the live fixture; PostgreSQL restore currently requires acyclic captured foreign-key dependencies; additional adapters, a TUI, export archives, and AI hypothesis tooling are not yet included.
