# mcpify

Config-driven CLI that turns `exec` commands, `http` endpoints, and `sql` queries into [MCP](https://modelcontextprotocol.io/) tools.

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
                               → sql:  SELECT * FROM users
                               → http: GET localhost:3010/users/1
                                        ↑
                               managed service: local API server
```

There are three core concepts: **tools**, **services**, and **resources**.

**Tools** are actions the AI agent can call — run a command, make an HTTP request, or query a database. They are stateless.

**Services** are background processes that mcpify starts and keeps alive. Only needed when a tool depends on a local process that mcpify should manage.

**Resources** are read-only data the agent can access — files, command output, or any static context.

## Variables and secrets

Define shared variables in the config. Use `${env:VAR}` to read from environment — secrets never pass through the agent.

```yaml
vars:
  api_token: ${env:GITHUB_TOKEN}
  base_url: https://api.github.com
  db_url: ${env:DATABASE_URL}

tools:
  - name: list_prs
    type: http
    method: GET
    url: "{{base_url}}/repos/{{owner}}/{{repo}}/pulls"
    headers:
      Authorization: "Bearer {{api_token}}"
    timeout_ms: 10000
    input:
      type: object
      properties:
        owner:
          type: string
        repo:
          type: string
      required: ["owner", "repo"]
```

Variables are merged with tool input at render time. Input parameters take precedence over config vars.

## Exec tools

Wrap any CLI command. No shell — commands run directly. Use `{{var}}` to inject parameters.

**Simple command:**
```yaml
tools:
  - name: git_status
    type: exec
    description: Show current git status
    command: git
    args: ["status", "--short"]
    timeout_ms: 5000
```

**With input parameters:**
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

**With custom env and working directory:**
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

**GET request to an external API:**
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

**POST with JSON body and auth from vars:**
```yaml
vars:
  gh_token: ${env:GITHUB_TOKEN}

tools:
  - name: create_issue
    type: http
    description: Create a GitHub issue
    method: POST
    url: https://api.github.com/repos/{{owner}}/{{repo}}/issues
    headers:
      Authorization: "Bearer {{gh_token}}"
    body: '{"title": "{{title}}"}'
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
      required: ["owner", "repo", "title"]
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

## SQL tools

Query databases directly. Supports SQLite and PostgreSQL.

**SQLite query:**
```yaml
tools:
  - name: find_user
    type: sql
    description: Find user by email
    driver: sqlite
    dsn: "sqlite:./data.db"
    query: "SELECT * FROM users WHERE email = '{{email}}'"
    timeout_ms: 5000
    annotations:
      read_only: true
    input:
      type: object
      properties:
        email:
          type: string
      required: ["email"]
```

**PostgreSQL with DSN from env:**
```yaml
vars:
  db_url: ${env:DATABASE_URL}

tools:
  - name: recent_orders
    type: sql
    description: Get recent orders
    driver: postgres
    dsn: "{{db_url}}"
    query: "SELECT id, status, total FROM orders ORDER BY created_at DESC LIMIT {{limit}}"
    timeout_ms: 10000
    annotations:
      read_only: true
    input:
      type: object
      properties:
        limit:
          type: string
          description: Number of orders to return
      required: ["limit"]
```

SELECT queries return a JSON array of rows. INSERT/UPDATE/DELETE return the number of affected rows.

## Tool annotations

Mark tools with hints for the MCP client. Clients like Claude Code can use these to ask for confirmation before running destructive actions.

```yaml
tools:
  - name: drop_table
    type: sql
    driver: postgres
    dsn: "{{db_url}}"
    query: "DROP TABLE {{table}}"
    timeout_ms: 10000
    annotations:
      destructive: true
    input:
      type: object
      properties:
        table:
          type: string
      required: ["table"]

  - name: list_tables
    type: sql
    driver: postgres
    dsn: "{{db_url}}"
    query: "SELECT tablename FROM pg_tables WHERE schemaname = 'public'"
    timeout_ms: 5000
    annotations:
      read_only: true
```

Available annotations: `destructive`, `read_only`, `idempotent`, `open_world` (all boolean).

## Resources

Resources provide read-only data to the agent without a tool call. The agent sees them as context.

**File resource:**
```yaml
resources:
  - name: readme
    type: file
    uri: "file:///project/README.md"
    path: ./README.md
    mime_type: text/markdown
    description: Project README
```

**Command output as resource:**
```yaml
resources:
  - name: db_schema
    type: exec
    uri: "mcpify://db-schema"
    command: pg_dump
    args: ["--schema-only", "mydb"]
    description: Current database schema

  - name: git_log
    type: exec
    uri: "mcpify://git-log"
    command: git
    args: ["log", "--oneline", "-20"]
    description: Recent git history
```

## Services

Services are optional. Use them only when mcpify needs to **start and manage** a local process.

**You DON'T need services when:**
- Calling an external API (`https://api.example.com/...`)
- Hitting a service already running (`localhost:8080` from docker-compose)
- Using exec or sql tools

**You DO need services when:**
- A tool depends on a local server that mcpify should start
- You want automatic health checking and restart

**Example:**
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

`depends_on` blocks the tool until the service is online. One service can serve many tools.

**Service lifecycle:**
- `autostart: true` — starts with `mcpify serve` (default)
- `restart: on-failure` — restarts on crash (up to 3 times). Also: `always`, `never`
- Health check: `starting` → `online` → `degraded`/`failed`
- Without healthcheck, "process alive" = online

## Config reload

Update config without restarting:

```bash
mcpify reload              # send SIGHUP to running server
mcpify serve --watch       # auto-reload on file change
```

Reload is diff-based — only changed tools and services are affected. Invalid config is rejected.

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

vars:
  key: value                   # plain value
  secret: ${env:SECRET_KEY}    # from environment variable

supervisor:
  restart_policy: on-failure
  healthcheck_interval_ms: 3000
  graceful_shutdown_timeout_ms: 5000

services:
  - name: service_name
    command: ./bin/server
    args: ["--flag", "value"]
    cwd: .
    env:
      KEY: value
    autostart: true            # default: true
    restart: on-failure        # on-failure | always | never
    healthcheck:
      type: http               # http | process
      url: http://127.0.0.1:PORT/health
      interval_ms: 3000
      timeout_ms: 1000

resources:
  - name: resource_name
    type: file                 # file | exec
    uri: "file:///path"
    path: ./file.md            # for file type
    command: cmd               # for exec type
    args: ["--flag"]           # for exec type
    mime_type: text/plain
    description: What this resource contains

tools:
  - name: tool_name
    type: exec                 # exec | http | sql
    description: What it does
    enabled: true

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

    # sql fields
    driver: postgres           # postgres | sqlite
    dsn: "{{db_url}}"
    query: "SELECT * FROM t WHERE id = '{{id}}'"

    # common
    timeout_ms: 5000
    depends_on: ["service_name"]
    retry:
      max_retries: 3
      retry_delay_ms: 1000
    annotations:
      destructive: false
      read_only: true
      idempotent: true
      open_world: false

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
