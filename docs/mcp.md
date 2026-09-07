# MCP-compatible interface

Run the controlled stdio server with:

    remnant --project remnant.yaml mcp

It accepts newline-delimited JSON-RPC messages and supports the standard initialize, tools/list, and tools/call methods. The exposed tools are:

- get_project_status
- doctor
- verify_failure
- list_state_sources
- inspect_state
- start_reduction
- run_reduction
- get_reduction_status
- generate_report

Tool calls return structured JSON and a text representation for clients that display MCP content blocks. The interface does not expose arbitrary shell commands, SQL, Redis commands, or database credentials. start_reduction and run_reduction apply the same safety checks and persistence lifecycle as the CLI.
