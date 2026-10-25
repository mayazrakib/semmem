# Semantic Memory

`semantic-memory` is a persistent memory and retrieval system for AI. It stores human-readable Markdown, revision history, and typed relationships in SQLite, and uses Tantivy for full-text and fuzzy search. A terminal client and an OAuth-protected Model Context Protocol (MCP) server share one registry.

## Build and run

Use Rust on Linux. Install the binary from a checkout, then start the service:

```sh
cargo install --path . --locked
semantic-memory serve
```

The service owns the database and search index. Terminal clients connect through a private Unix socket. Data lives in `$XDG_DATA_HOME/semantic-memory`, or `$HOME/.local/share/semantic-memory` when `XDG_DATA_HOME` is unset. Use `--socket` or `SEMANTIC_MEMORY_SOCKET` for another socket; pass `--data-dir` to `serve` for another data directory.

## Use

Keep the service running in one terminal and use another to add, find, and edit memories:

```sh
semantic-memory add reading --title "Reading preferences" --tag personal
semantic-memory search "libary"
semantic-memory show reading
semantic-memory edit reading
semantic-memory history reading
```

`add` and `edit` open `VISUAL`, then `EDITOR`, then `vim`. The editor contains only the Markdown body. Set titles, kinds, tags, and links through command options. Keys are stable identifiers supplied by the caller; readable names and UUIDs are accepted. Unchanged edits create no revision, and failed saves preserve the draft.

Search supports exact phrases, prefixes, spelling mistakes, and kind or tag filters. `list` browses memories and their incoming or outgoing relationships. The `taxonomy` commands manage kinds, tags, and relation types. Add `--json` for structured output, and use `--help` for each command's options.

Reads return the version required for guarded writes. For a memory at version two, deletion and restoration use:

```sh
semantic-memory delete reading --expected-version 2
semantic-memory restore reading --expected-version 3
```

`delete` retains history, and `restore` appends a new revision. `purge` requires `--confirm` and permanently removes the memory, its history, and incoming references, including historical links. Lists and history return summaries; use `show --version` to read a historical body.

Writes commit to SQLite and become searchable before success is returned. Stale versions produce conflicts instead of overwriting newer edits. Startup recovers pending index changes, and identical creates with the same key are retry-safe. SQLite uses WAL with FULL synchronization; durability depends on storage honoring synchronization requests. See [the schema](docs/schema.md) for storage and lifecycle details.

## Remote MCP

The MCP interface provides search, batch reads and writes, browsing, history, restoration, purge, and taxonomy management. Responses include stable keys and versions for subsequent edits. The `memory:read`, `memory:write`, and `memory:purge` scopes control access separately.

Remote connections require an external OAuth provider and an HTTPS reverse proxy. Configure the service with:

```sh
semantic-memory serve \
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

The Rust library separates domain contracts, SQLite storage, search, write coordination, authentication, server transport, MCP tools, and terminal interaction under `src/`. Schema migrations live in `migrations/`, integration tests in `tests/`, benchmark code in `tests/perf/`, and benchmark results in `benchmark/`.
