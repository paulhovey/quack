# defaults/mcp.d

Example MCP servers, one per file, all `enabled = false`. To use one, copy it to `mcp.d`
in your config folder (`cutoffharness config paths` shows where) and delete the
`enabled = false` line. `/mcp` shows whether it connected.

A local server needs only `command` (and `args`); a remote one needs only `url`. The full
list of options is in `../schemas/mcp.json`.
