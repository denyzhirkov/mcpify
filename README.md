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
mcpify init          # create mcpify.yaml
mcpify validate      # check config
mcpify run hello     # run a tool locally
mcpify serve         # start MCP server
```

## How it works

mcpify sits between an MCP client and the outside world:

```
MCP client  ←stdio→  mcpify  → exec: git status
                               → http: GET api.example.com/data
                               → http: GET localhost:3010/users/1
                                        ↑
                               managed service: local API server
```

There are two core concepts: **tools** and **services**.

**Tools** are actions the AI agent can call — run a command or make an HTTP request. They are stateless.

**Services** are background processes that mcpify starts and keeps alive. They exist only when a tool depends on a local process that needs to be running. If your service is already running elsewhere (external API, cloud service, shared dev server), you don't need a service entry — just point the tool at the URL.

## Exec tools

Wrap any CLI command. No shell — commands run directly. Use `{{var}}` to inject input parameters into args.

**Run a command with no input:**
```yaml
tools:
  - name: git_status
    type: exec
    description: Show current git status
    command: git
    args: ["status", "--short"]
    timeout_ms: 5000
```

**Run a command with input:**
```yaml
tools:
  - name: create_commit
    type: exec
    description: Create a git commit
    command: git
    args: ["commit", "-m", "{{message}}"]
    timeout_ms: 10000
    input:
      type: object
      properties:
        message:
          type: string
      required: ["message"]
```

**Run with custom env and working directory:**
```yaml
tools:
  - name: run_tests
    type: exec
    description: Run project tests
    command: npm
    args: ["test"]
    cwd: ./frontend
    env:
      NODE_ENV: test
    timeout_ms: 60000
```

## HTTP tools

Call any HTTP endpoint. Use `{{var}}` in URL, headers, and body.

**GET request to an external API (no service needed):**
```yaml
tools:
  - name: get_weather
    type: http
    description: Get current weather for a city
    method: GET
    url: https://api.weather.com/current?city={{city}}
    timeout_ms: 5000
    input:
      type: object
      properties:
        city:
          type: string
      required: ["city"]
```

**POST with a JSON body:**
```yaml
tools:
  - name: create_issue
    type: http
    description: Create a GitHub issue
    method: POST
    url: https://api.github.com/repos/{{owner}}/{{repo}}/issues
    headers:
      Authorization: "Bearer {{token}}"
    body: '{"title": "{{title}}", "body": "{{body}}"}'
    timeout_ms: 10000
    input:
      type: object
      properties:
        owner:
          type: string
        repo:
          type: string
        title:
          type: string
        body:
          type: string
        token:
          type: string
      required: ["owner", "repo", "title", "token"]
```

**With retry on server errors (5xx):**
```yaml
tools:
  - name: flaky_api
    type: http
    method: GET
    url: http://unstable-service.internal/data
    timeout_ms: 5000
    retry:
      max_retries: 3
      retry_delay_ms: 1000
```

## Services

Services are optional. Use them when mcpify needs to **start and manage** a local process that your tools depend on.

**You DON'T need services when:**
- Calling an external API (`https://api.example.com/...`)
- Hitting a service that's already running (`localhost:8080` started by docker-compose)
- Using only exec tools

**You DO need services when:**
- A tool depends on a local server that mcpify should start
- You want automatic health checking and restart

**Example: local API server as a managed service:**
```yaml
services:
  - name: api
    command: ./bin/api-server
    args: ["--port", "3010"]
    autostart: true
    restart: on-failure
    healthcheck:
      type: http
      url: http://127.0.0.1:3010/health
      interval_ms: 3000
      timeout_ms: 1000

tools:
  - name: get_user
    type: http
    description: Get user by ID
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

  - name: list_users
    type: http
    description: List all users
    method: GET
    url: http://127.0.0.1:3010/users
    timeout_ms: 5000
    depends_on: ["api"]
```

The `depends_on` field blocks the tool until the service is online. One service can serve many tools.

**Service lifecycle:**
- `autostart: true` — starts with `mcpify serve` (default)
- `restart: on-failure` — restarts if the process crashes (up to 3 times)
- `restart: always` — always restart, `never` — don't restart
- Health check runs periodically; service state: `starting` → `online` → `degraded`/`failed`
- Without a healthcheck, "process alive" = online

## Config reload

Update tools and services without restarting the server:

```bash
# Edit mcpify.yaml, then:
mcpify reload

# Or with auto-reload:
mcpify serve --watch
```

Reload is diff-based — only changed tools and services are affected:
1. New services start
2. Tool registry updates
3. Changed services restart
4. Removed services stop

If the new config is invalid, the old one stays active.

## CLI

| Command | Description |
|---------|-------------|
| `mcpify init` | Create a starter `mcpify.yaml` |
| `mcpify validate` | Check config for errors |
| `mcpify serve` | Start MCP server (stdio) |
| `mcpify serve --watch` | Serve + auto-reload on config change |
| `mcpify reload` | Reload config in running server |
| `mcpify list` | List registered tools |
| `mcpify status` | Show tools, services, health |
| `mcpify run <tool>` | Execute a tool locally |
| `mcpify run <tool> -i '{"key":"val"}'` | Execute with input |

## MCP client config

**Claude Code** (`~/.claude.json` or `~/.claude/claude_code_config.json`):
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

## Full config reference

```yaml
server:
  name: my-project       # server name (default: "mcpify")
  transport: stdio        # only stdio for now
  log_level: info         # trace, debug, info, warn, error

supervisor:
  restart_policy: on-failure       # default restart policy
  healthcheck_interval_ms: 3000   # how often to check health
  graceful_shutdown_timeout_ms: 5000

services:
  - name: service_name
    command: ./bin/server
    args: ["--flag", "value"]
    cwd: .                     # working directory
    env:
      KEY: value
    autostart: true            # start with serve (default: true)
    restart: on-failure        # on-failure | always | never
    healthcheck:
      type: http               # http | process
      url: http://127.0.0.1:PORT/health
      interval_ms: 3000
      timeout_ms: 1000

tools:
  - name: tool_name
    type: exec                 # exec | http
    description: What it does
    enabled: true              # default: true

    # exec fields
    command: binary
    args: ["--flag", "{{var}}"]
    cwd: ./subdir
    env:
      KEY: value

    # http fields
    method: GET                # GET | POST | PUT | PATCH | DELETE
    url: http://host/path/{{var}}
    headers:
      Authorization: "Bearer {{token}}"
    body: '{"key": "{{value}}"}'

    # common
    timeout_ms: 5000
    depends_on: ["service_name"]
    retry:
      max_retries: 3
      retry_delay_ms: 1000

    input:
      type: object
      properties:
        var:
          type: string
          description: Description for the AI
      required: ["var"]
```

## License

MIT
