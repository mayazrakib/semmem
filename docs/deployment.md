# Deployment

`semantic-memory` runs one persistent service per data directory. The CLI connects to its private Unix socket. Remote AI clients connect to an optional OAuth-protected Model Context Protocol (MCP) endpoint. The current server and CLI require a Unix host.

## Local service

Build the executable with `cargo build --release`. Start the service with explicit paths when running it under a service supervisor:
```sh
semantic-memory --socket /var/lib/semantic-memory/service.sock serve \
    --data-dir /var/lib/semantic-memory
```

The service account must own the data directory. The server sets the directory permissions to `0700` and socket permissions to `0600`. Run the CLI as that account. The default data directory is `$XDG_DATA_HOME/semantic-memory`, or `$HOME/.local/share/semantic-memory` when `XDG_DATA_HOME` is unset. The default socket is `service.sock` inside that default directory. Changing `--data-dir` does not change the socket path; supply `--socket` to both the server and CLI when using a custom location.

Select the socket with `--socket` or `SEMANTIC_MEMORY_SOCKET`. Without OAuth configuration, the service opens no TCP listener. `--listen` defaults to `127.0.0.1:8765` when remote MCP is enabled. Send an interrupt signal for graceful shutdown.

## Remote MCP

Use an external OpenID Connect (OIDC) provider that publishes discovery metadata and JSON Web Key Sets (JWKS). It must issue signed JWT access tokens with an issuer matching `--issuer`, an audience matching `--audience`, an expiration, a subject, and a space-separated `scope` claim. Supported signature algorithms are RS256, ES256, and EdDSA. Configure OAuth client registration and authorization with that provider; this service does not implement an authorization server or a login UI.

Enable the endpoint by providing all three OAuth options:
```sh
semantic-memory --socket /var/lib/semantic-memory/service.sock serve \
    --data-dir /var/lib/semantic-memory \
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
| `memory:read` | MCP discovery and session requests, search, reads, browsing, taxonomy lists, and history |
| `memory:write` | Memory creation, updates, soft deletion, restoration, and taxonomy writes |
| `memory:purge` | Permanent memory purges |

Give a client all scopes needed by its workflow. A write-only token does not substitute for `memory:read` during MCP initialization. The local socket uses filesystem access control instead of OAuth and permits all operations. It is intended for the service account's CLI access, not exposure through the public reverse proxy.

Remote writes are attributed to `ai client`; local writes are attributed to `human`. Actor labels describe the interface used and do not provide a per-user audit identity.

## Editing and recovery

`add` and `edit` open a private Markdown draft using `VISUAL`, then `EDITOR`, then `vim`. Editor command arguments and quoted executable paths are supported. The draft contains the body only. Titles, kinds, tags, and relationships are explicit CLI fields. The database transaction starts after the editor exits.

Editor errors, invalid UTF-8, version conflicts, and service errors preserve the draft and print its path. Successful saves remove the temporary draft. An unchanged edit creates no revision. When a write error reports that the write was committed, read the current memory before retrying.

SQLite is authoritative. The search directory is derived, and the service replays pending index events or rebuilds unusable indexes during startup. A successful write response follows search-index publication. Stop the service before making a simple filesystem copy of its data directory for backup; copying a live SQLite file alone is not a consistent backup procedure.

These instructions describe the delivered configuration. They do not establish a live public deployment or compatibility with a particular OAuth provider or AI client.
