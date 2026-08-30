# kn9t-mcp

MCP (Model Context Protocol) client plugin for kn9t.

This plugin allows kn9t to use tools from any MCP-compatible server, expanding
the agent's capabilities to include integrations with Jira, GitHub, Slack,
databases, filesystems, and hundreds of other services.

## What it proves

This plugin demonstrates that **kn9t's plugin system is truly language-agnostic**.
While kn9t itself is written in Rust, this plugin is pure Python with zero Rust
dependencies. It communicates with kn9t-server using the same stdio JSON protocol
that Rust plugins use.

## Architecture

```
kn9t-server <--plugin v2--> kn9t-mcp.py <--MCP stdio--> MCP servers
                (JSON/stdin/stdout)           (JSON-RPC)
```

1. kn9t-server spawns `kn9t-mcp` as a subprocess
2. `kn9t-mcp` reads config and spawns MCP servers (Jira, GitHub, etc.)
3. `kn9t-mcp` discovers tools from each MCP server
4. Tools are exposed to kn9t with prefixed names (`mcp_github_list_prs`)
5. When the model calls a tool, `kn9t-mcp` routes it to the right MCP server

## Installation

```bash
# From source
cd plugins/kn9t-mcp
pip install -e .

# Or directly
pip install kn9t-mcp  # (future: when published to PyPI)
```

## Configuration

### Step 1: Configure MCP servers

Create `~/.kn9t/mcp.toml`:

```toml
# GitHub integration
[[mcp]]
name = "github"
cmd = ["npx", "-y", "@modelcontextprotocol/server-github"]
[mcp.env]
GITHUB_PERSONAL_ACCESS_TOKEN = "env:GITHUB_TOKEN"

# Jira integration
[[mcp]]
name = "jira"
cmd = ["npx", "-y", "@anthropic/mcp-server-jira"]
[mcp.env]
JIRA_URL = "https://mycompany.atlassian.net"
JIRA_USERNAME = "user@example.com"
JIRA_API_TOKEN = "env:JIRA_TOKEN"

# Local filesystem access
[[mcp]]
name = "fs"
cmd = ["npx", "-y", "@modelcontextprotocol/server-filesystem", "/home/user/projects"]

# SQLite database
[[mcp]]
name = "db"
cmd = ["uvx", "mcp-server-sqlite", "--db-path", "/path/to/data.db"]
```

### Step 2: Register plugin with kn9t

Add to `~/.kn9t/config.toml`:

```toml
[[plugin]]
name = "kn9t-mcp"
cmd = ["python", "-m", "kn9t_mcp"]
```

Or if installed globally:

```toml
[[plugin]]
name = "kn9t-mcp"
cmd = ["kn9t-mcp"]
```

## Usage

Once configured, MCP tools appear automatically in kn9t:

```
$ kn9t chat

You: List open PRs in the kn9t repo
[calling mcp_github_list_pull_requests...]
Assistant: Here are the open PRs:
1. #42 - Add MCP support (draft)
2. #41 - Fix token accounting
```

## Tool naming

Tools from MCP servers are prefixed to avoid name collisions:

```
Original MCP tool    ->  kn9t tool name
-------------------------------------------
github/list_prs      ->  mcp_github_list_prs
jira/create_issue    ->  mcp_jira_create_issue
fs/read_file         ->  mcp_fs_read_file
```

## Testing the plugin standalone

You can test the plugin without kn9t by sending it raw JSON:

```bash
# Test handshake
echo '{"t":"hello","proto":1,"kn9t":"0.1.0"}' | python -m kn9t_mcp

# Should output hello response with discovered tools
```

## Supported MCP servers

Any MCP-compatible server works. Popular ones:

| Server | Install | Description |
|--------|---------|-------------|
| GitHub | `npx @modelcontextprotocol/server-github` | PRs, issues, repos |
| Filesystem | `npx @modelcontextprotocol/server-filesystem` | File operations |
| SQLite | `uvx mcp-server-sqlite` | Database queries |
| Postgres | `npx @modelcontextprotocol/server-postgres` | PostgreSQL access |
| Slack | `npx @modelcontextprotocol/server-slack` | Send/read messages |
| Google Drive | `npx @anthropic/mcp-server-gdrive` | Drive file access |
| Brave Search | `npx @anthropic/mcp-server-brave-search` | Web search |

See [MCP servers directory](https://github.com/modelcontextprotocol/servers) for more.

## Limitations

- **No OAuth support yet**: Servers requiring OAuth must be configured with static tokens
- **No resources/prompts**: Only MCP tools are exposed (resources and prompts are not)
- **Python 3.11+**: Uses `tomllib` from stdlib (3.11+) and type hints

## Development

```bash
cd plugins/kn9t-mcp
pip install -e ".[dev]"

# Run tests
pytest

# Type check
mypy kn9t_mcp

# Lint
ruff check kn9t_mcp
```

## License

MIT
