# semmem

`semmem` is a persistent memory and retrieval system for AI. It stores human-readable Markdown, revision history, and typed relationships in SQLite, and uses Tantivy for full-text and fuzzy search. A terminal client and an OAuth-protected Model Context Protocol (MCP) server share one registry.

## Build and run

Use Rust on Linux with a systemd user manager. Install the executable and user service from a checkout, then manage the service through the CLI:

```sh
./install.sh
semmem start
semmem status
```

`semmem start`, `semmem stop`, and `semmem status` manage `semmem.service` through `systemctl --user`. The installer puts the executable in `$HOME/.local/bin`, enables the unit without starting it, and preserves an existing environment file. Ensure `$HOME/.local/bin` is on `PATH`.

The service owns the database and search index. Terminal clients connect through a private Unix socket. Data lives in `$XDG_DATA_HOME/semmem`, or `$HOME/.local/share/semmem` when `XDG_DATA_HOME` is unset. Use `--socket` or `SEMMEM_SOCKET` for another socket; use the service launch configuration for another data directory. The hidden `run` command hosts the server when configuring a custom service supervisor.

## Use

After starting the service, add, find, and edit memories:

```sh
semmem register author "mayaz rakib"
semmem add reading --author "mayaz rakib" --title "Reading preferences" --tag personal
semmem search "libary"
semmem show reading
semmem edit reading
semmem history reading
```

`add [KEY]` prompts for missing metadata before opening a Markdown draft using `VISUAL`, then `EDITOR`, then `vim`. Select a registered author with the arrow keys, or select the empty entry and enter a custom name. New names register automatically. Noninteractive creation requires `--no-input`, a key, and `--author`. The default title is the key, and the default type is `general`. Prompts use stderr. End-of-input or cancellation stops before saving. Failed editor saves preserve the draft.

`edit KEY` opens YAML frontmatter and the exact Markdown body. Content fields, including author, are editable. Internal IDs, the key, and system audit fields are read-only. Unchanged edits create no revision. `semmem deregister author "Name"` removes an unused author; saved records and revisions prevent deregistration.

Search supports phrases, prefixes, fuzzy matching, and type, tag, scope, or basis filters. Results contain complete bodies and timestamps and are ordered by descending score. `--history` searches all retained revisions; `--as-of TIMESTAMP` searches the state at a UTC Unix millisecond timestamp. These modes are mutually exclusive. `--include-deleted` includes deleted revision states. Ordinary search returns current live memories.

`list` browses memories and references; `taxonomy` manages types, tags, and reference types. Search returns JSON by default. Use `--json` for structured output from other commands.

Reads return the version required for guarded writes. For a memory at version two, deletion and restoration use:

```sh
semmem delete reading --expected-version 2
semmem restore reading --expected-version 3
```

`delete` retains history, and `restore` appends a new revision. `purge` requires `--confirm` and permanently removes the memory, its history, and incoming references, including historical references. All reads and mutation responses return full bodies. Use `show --version` to select a historical revision.

Writes commit to SQLite and become searchable before success is returned. Stale versions produce conflicts instead of overwriting newer edits. Startup recovers pending index changes, and identical creates with the same key are retry-safe. SQLite uses WAL with FULL synchronization; durability depends on storage honoring synchronization requests. See [the schema](docs/schema.md) for storage and lifecycle details.

## Metadata and Retrieval

Set `summary`, `basis`, `scope`, `observed_at`, `verified_at`, and `review_after` in the edit frontmatter. The `add` command also accepts corresponding command options. Basis values are `stated`, `observed`, `inferred`, and `assumed`. Timestamp values use UTC milliseconds since Unix epoch. Omitted creation dates `observed_at` and `verified_at` default to the current timestamp. Explicit null remains unknown. Updates preserve omitted values, and explicit null clears nullable metadata.

```sh
semmem edit reading
semmem search explanations --scope personal
semmem show reading --view summary
semmem show reading
```

Every retrieval returns complete Markdown bodies, associations, and any stored summary. The accepted `summary` view is a compatibility alias for full records. No excerpts, embeddings, or token budgeting are used.

New databases initialize at schema version one. Earlier schemas are rejected and left intact; this version provides no conversion. The service removes application size, count, and runtime deadline restrictions. Caller-selected pagination, atomic writes, version checks, and unavoidable library representation constraints remain.

## Remote MCP

For private ChatGPT access, the Unix socket also exposes `/mcp` for OpenAI Secure MCP Tunnel. The tunnel connects to the same registry as the CLI without opening a public listener. Register the MCP server through ChatGPT’s Add custom MCP server flow and verify its tools; installing the skill package alone does not establish that connection. See [private tunnel deployment](docs/deployment.md#private-chatgpt-tunnel).

The MCP interface provides search, batch reads and writes, browsing, history, restoration, purge, and taxonomy management. Responses include stable keys and versions for subsequent edits. The `memory:read`, `memory:write`, and `memory:purge` scopes control access separately.

Remote connections require an external OAuth provider and an HTTPS reverse proxy. For the managed service, configure `SEMMEM_ISSUER`, `SEMMEM_AUDIENCE`, and `SEMMEM_PUBLIC_URL` in `$XDG_CONFIG_HOME/semmem/environment`, or `$HOME/.config/semmem/environment` when `XDG_CONFIG_HOME` is unset, then stop and start the service. For a custom supervisor, the hidden server command accepts the OAuth settings:

```sh
semmem run \
    --issuer https://identity.example.com \
    --audience https://memory.example.com/mcp \
    --public-url https://memory.example.com
```

The server binds to loopback by default and serves MCP at `/mcp`. Without OAuth configuration, it opens only the private host socket. See [deployment](docs/deployment.md) for provider requirements, proxy configuration, and client connection details. Hosting and live client verification are separate deployment steps.

## Develop

Run the tests and checks from a checkout:

```sh
cargo test --locked --features fault-injection --tests
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo fmt --check
```

The `fault-injection` feature enables subprocess crash-recovery tests. Build release deployments with the default features.

Run the deterministic million-memory benchmark with:

```sh
cargo bench --locked --bench search -- --memories 1000000 --clients 16 --iterations 100 --data-dir ./target/benchmark-million
```

The benchmark reports isolated and concurrent latency measurements, throughput, startup time, memory use, and storage size. See [performance](docs/performance.md) for the measured results, workload, and limits. These timings exclude HTTP, OAuth, MCP transport, and model generation.

The Rust library separates domain contracts, SQLite storage, search, write coordination, authentication, server transport, MCP tools, and terminal interaction under `src/`. The fresh-database initializer is `initialize.sql`, integration tests in `tests/`, benchmark code in `tests/perf/`, and benchmark results in `benchmark/`.
