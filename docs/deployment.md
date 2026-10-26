# Deployment

`semmem` runs one persistent service per data directory. The CLI connects to its private Unix socket. Remote AI clients connect to an optional OAuth-protected Model Context Protocol (MCP) endpoint. The current server and CLI require a Unix host.

## Local service

The managed installation requires Linux, Rust, and a systemd user manager. From the repository checkout, install the executable and user unit:

```sh
./install.sh
semmem start
semmem status
```

The installer puts the executable in `$HOME/.local/bin/semmem` and the unit in `$XDG_CONFIG_HOME/systemd/user/semmem.service`, using `$HOME/.config` when `XDG_CONFIG_HOME` is unset. It enables the unit without starting it. Ensure `$HOME/.local/bin` is on `PATH`. The CLI uses `systemctl --user` to manage that unit. Stop the service with `semmem stop`. These commands manage the current user's service; they do not install a system-wide service.

For a custom service supervisor, build with `cargo build --release` and invoke the hidden server command with explicit paths:

```sh
semmem --socket /var/lib/semmem/service.sock run \
    --data-dir /var/lib/semmem
```

The service account must own the data directory. The server sets the directory permissions to `0700` and socket permissions to `0600`. Run the CLI as that account. The default data directory is `$XDG_DATA_HOME/semmem`, or `$HOME/.local/share/semmem` when `XDG_DATA_HOME` is unset. The default socket is `service.sock` inside that default directory. Changing `--data-dir` does not change the socket path; supply `--socket` to both the server and CLI when using a custom location.

Select the socket with `--socket` or `SEMMEM_SOCKET`. Without OAuth configuration, the service opens no TCP listener. `--listen` defaults to `127.0.0.1:8765` when remote MCP is enabled. Use `semmem stop` for the managed user service. A custom supervisor can send an interrupt or termination signal for graceful shutdown.

## Private ChatGPT tunnel

The same private Unix socket also serves stateless MCP at `/mcp`. It shares the CLI registry and uses the trusted private MCP identity for system audit attribution. Every mutation requires a nonempty `commit_summary`. Filesystem permissions protect the socket; this endpoint does not open a TCP listener or weaken the separate OAuth-protected remote router.

For OpenAI Secure MCP Tunnel, create a dedicated tunnel associated with the intended ChatGPT workspace. Store the runtime key in a private file outside this repository and use a file reference. An example foreground launch is:

```sh
tunnel-client run \
    --control-plane.tunnel-id tunnel_your_tunnel_id \
    --control-plane.api-key "file:$HOME/.config/semmem/tunnel-api-key" \
    --mcp.server-url "url=http://localhost/mcp,channel=main,unix-socket=$HOME/.local/share/semmem/service.sock" \
    --health.listen-addr 127.0.0.1:0 \
    --health.url-file "$HOME/.local/share/semmem/tunnel-health.url"
```

Keep the key file readable only by the service account, with mode `0600`. Adapt the socket path when using a custom data directory. Supervise long-running tunnels with a systemd user service that starts after and stops with `semmem.service`. Validate health, readiness, and an actual successful control-plane poll independently:

```sh
tunnel-client health \
    --url-file "$HOME/.local/share/semmem/tunnel-health.url" \
    --require-control-plane-poll \
    --json
```

The installed tunnel-client version `0.0.14` can incorrectly probe TCP in `doctor` for a Unix-socket target. Its running MCP transport supports the Unix socket; verify session initialization and successful polling instead of treating that diagnostic failure as a server failure. Readiness alone does not prove a successful control-plane poll or ChatGPT tool call.

Register the running MCP server in ChatGPT before relying on the packaged skill:

1. Open ChatGPT Plugins, select the plus button, and choose Add custom MCP server.
2. Give the server a name, choose Tunnel, and select the dedicated tunnel identity.
3. Choose No authentication for this private Unix-socket endpoint. Tunnel access is controlled by the OpenAI organization/workspace association and runtime credential.
4. Create and install the resulting server plugin. Confirm that ChatGPT discovers the memory tools.
5. Use the actual registered app ID when binding the existing skill package to that server. Never invent an app ID or substitute the tunnel ID.
6. Verify a read-only tool call from ChatGPT before claiming end-to-end operation.

The portable package under `plugin/` contains the skill and tunnel configuration, without credentials or a public server URL. Uploading or installing that package does not by itself prove that ChatGPT registered an MCP app. A readable skill can coexist with missing tools. Check the registered app binding and tool discovery separately from package installation and tunnel health. See [OpenAI's connection flow](https://developers.openai.com/plugins/deploy/connect-chatgpt) and [tunnel documentation](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels).

## Remote MCP

Use an external OpenID Connect (OIDC) provider that publishes discovery metadata and JSON Web Key Sets (JWKS). It must issue signed JWT access tokens with an issuer matching `--issuer`, an audience matching `--audience`, an expiration, a subject, and a space-separated `scope` claim. Supported signature algorithms are RS256, ES256, and EdDSA. Configure OAuth client registration and authorization with that provider; this service does not implement an authorization server or a login UI.

For the managed service, set all three variables in `$XDG_CONFIG_HOME/semmem/environment`, or `$HOME/.config/semmem/environment` when `XDG_CONFIG_HOME` is unset. The installer creates this private file only when it is absent and preserves existing configuration:

```sh
SEMMEM_ISSUER=https://identity.example.com
SEMMEM_AUDIENCE=https://memory.example.com/mcp
SEMMEM_PUBLIC_URL=https://memory.example.com
```

Run `semmem stop`, then `semmem start` after updating the environment file. For a custom supervisor, enable the endpoint by providing all three OAuth options:
```sh
semmem --socket /var/lib/semmem/service.sock run \
    --data-dir /var/lib/semmem \
    --listen 127.0.0.1:8765 \
    --issuer https://identity.example.com \
    --audience https://memory.example.com/mcp \
    --public-url https://memory.example.com
```

`--public-url` is the externally reachable HTTPS origin, without an `/mcp` suffix. The configured issuer and public URL require HTTPS. Put a TLS reverse proxy in front of the local TCP listener and forward the original public Host header and Authorization header. Forward `/mcp`, `/.well-known/oauth-protected-resource`, and `/.well-known/oauth-protected-resource/mcp` to the service. The MCP endpoint is `https://memory.example.com/mcp`. The remote `/health` route and protected-resource metadata are public; MCP operations require a Bearer token.

The server validates allowed hosts and origins. Its configured public origin is the allowed browser Origin. Signing keys are cached and refreshed, so ordinary authenticated requests do not fetch keys on every call.

Scopes are independent:

| Scope | Operations |
|---|---|
| `memory:read` | MCP discovery and session requests, search, reads, browsing, taxonomy lists, and commit audit reads |
| `memory:write` | Memory creation, updates, and taxonomy writes |
| `memory:purge` | Permanent memory deletion |

Give a client all scopes needed by its workflow. A write-only token does not substitute for `memory:read` during MCP initialization. The local socket uses filesystem access control instead of OAuth and permits all operations. It is intended for the service account's CLI access, not exposure through the public reverse proxy.

Remote writes are attributed to `ai client`; local writes are attributed to `human`. Actor labels describe the interface used and do not provide a per-user audit identity.

## Editing and Audit

Every mutating CLI command requires `--message "Describe the change."`. MCP mutation envelopes require `commit_summary`. The optional memory `summary` remains separate. Reads return an opaque `system_token`; existing-record writes require that value as `expected_token` through MCP or `--expected-token` where exposed by the CLI. The editor reads and preserves its concurrency token automatically. Conflicts require rereading and reconciling current state.

`add` opens a private Markdown body draft using `VISUAL`, then `EDITOR`, then `vim`. `edit KEY` opens the complete memory object as YAML frontmatter followed by its Markdown body. Editor command arguments and quoted executable paths are supported. The frontmatter exposes title, type, tags, references, and all six memory metadata fields. Internal IDs, the key, and system metadata are read-only; changing them rejects the save. Optional fields can be cleared with YAML `null`. The body after the closing frontmatter delimiter is preserved exactly. The database transaction starts after the editor exits.

Editor errors, invalid UTF-8, concurrency-token conflicts, and service errors preserve the draft and print its path. Successful saves remove the temporary draft. Search produces structured JSON by default, and `--json` selects structured output for other commands. An unchanged edit creates no audit commit. When a write error reports that the write was committed, read the current memory before retrying.

`delete` permanently removes a memory and its incoming and outgoing references. There is no soft deletion, restoration, historical search, or separate purge operation. Remote deletion requires `memory:purge`; `memory:write` alone is insufficient. `commits` lists audit entries, and `commit` reads an audit entry. Audit entries describe mutations without storing previous bodies or snapshots.

SQLite is authoritative. The search directory is derived, and the service replays pending index events or rebuilds unusable indexes during startup. A successful write response follows search-index publication. Stop the service before making a simple filesystem copy of its data directory for backup; copying a live SQLite file alone is not a consistent backup procedure.

These instructions describe the delivered configuration. They do not establish a live public deployment or compatibility with a particular OAuth provider or AI client.

## Schema Reset Deployment

The current initializer defines schema v2. Existing incompatible databases, including databases with the previous architecture, are rejected without migration. Build and verify the default-feature release before deployment.

For an explicitly authorized reset, stop both Semmem and its tunnel before permanently removing the old SQLite file, its WAL and SHM sidecars, the derived `search/` directory, and `search.identity`. Verify the exact configured data directory first. Preserve credentials, environment files, systemd overrides, tunnel state, and unrelated backups. Install the validated revision, restart the services, verify schema v2 and integrity, and confirm that no memories exist. Verify actual MCP calls separately from tunnel health.
