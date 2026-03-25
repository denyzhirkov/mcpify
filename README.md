# mcpify

Config-driven CLI that turns `exec` commands and `http` endpoints into [MCP](https://modelcontextprotocol.io/) tools.

Define tools in a YAML file, run `mcpify serve`, and any MCP client (Claude Code, etc.) can use them.

## Install

**macOS / Linux:**
```bash
curl -fsSL https://raw.githubusercontent.com/denyzhirkov/mcpify/master/install.sh | sh
```

**Windows:**
```powershell
irm https://raw.githubusercontent.com/denyzhirkov/mcpify/master/install.ps1 | iex
```


## Quick start

```bash
# Create a config
mcpify init

# Check it
mcpify validate

# Run a tool locally
mcpify run hello

# Start MCP server
mcpify serve
```

## Config example

```yaml
server:
  name: my-project
  transport: stdio
  log_level: info

children:
  - name: api
    command: ./bin/api
    args: ["--port", "3010"]
    healthcheck:
      type: http
      url: http://127.0.0.1:3010/health
      interval_ms: 3000
      timeout_ms: 1000

tools:
  - name: git_status
    type: exec
    description: Show git status
    command: git
    args: ["status", "--short"]
    timeout_ms: 5000

  - name: get_user
    type: http
    description: Get user by id
    method: GET
    url: http://127.0.0.1:3010/users/{{id}}
    timeout_ms: 5000
    depends_on: ["api"]
    input:
      type: object
      properties:
        id:
          type: string
      required: ["id"]
```

## Tool types

**exec** -- runs a command directly (no shell). Supports `{{var}}` templating in args.

**http** -- makes an HTTP request. Supports `{{var}}` in URL, headers, and body. Methods: GET, POST, PUT, PATCH, DELETE. Optional retry policy:

```yaml
retry:
  max_retries: 3
  retry_delay_ms: 1000
```

## Child processes

mcpify can manage background services that your tools depend on. Children are started automatically, health-checked, and restarted on failure (configurable: `on-failure`, `always`, `never`).

Tools with `depends_on` are blocked until the referenced child is online.

## CLI

| Command | Description |
|---------|-------------|
| `mcpify init` | Create a starter `mcpify.yaml` |
| `mcpify validate` | Check config for errors |
| `mcpify serve` | Start MCP server (stdio) |
| `mcpify serve --watch` | Serve + auto-reload on config change |
| `mcpify reload` | Reload config in running server |
| `mcpify list` | List registered tools |
| `mcpify status` | Show tools, children, health |
| `mcpify run <tool>` | Execute a tool locally |

## MCP client config

**Claude Code** (`~/.claude/claude_code_config.json`):
```json
{
  "mcpServers": {
    "mcpify": {
      "command": "mcpify",
      "args": ["serve", "-c", "/path/to/mcpify.yaml"]
    }
  }
}
```

## Config reload

Three ways to reload without restarting:

- `mcpify reload` -- sends SIGHUP to the running server
- `kill -HUP <pid>` -- manual signal
- `mcpify serve --watch` -- auto-reload on file change

Reload is diff-based: only changed tools and children are affected.

## License

MIT
