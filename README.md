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
semmem add reading --title "Reading preferences" --tag personal
semmem search "libary"
semmem show reading
semmem edit reading
semmem history reading
```

`add [KEY]` prompts for missing metadata before opening a Markdown-only draft using `VISUAL`, then `EDITOR`, then `vim`. Supplied metadata flags skip their corresponding prompts. Blank optional answers remain unknown; the default title is the key, and the default kind is `note`. Tags and links are entered one at a time, ending with a blank answer. Prompts go to standard error so JSON output remains structured. Use `--no-input` with a key to use flags and defaults without prompts. End-of-input cancels before the editor or a database write. `edit KEY` opens the complete memory object: YAML frontmatter contains the structured fields, and the remaining text is the exact Markdown body. Edit the title, kind, tags, links, summary, claim type, scope, and observation or review dates in the frontmatter. Internal IDs, the key, the actor, and system metadata are read-only. Unchanged edits create no revision, and failed saves preserve the draft. Keys are stable identifiers supplied by the caller; readable names and UUIDs are accepted.

Search supports exact phrases, prefixes, spelling mistakes, and kind, tag, scope, or claim-type filters. `list` browses memories and their incoming or outgoing relationships. The `taxonomy` commands manage kinds, tags, and relation types. Search returns structured JSON by default. Add `--json` for structured output from other commands, and use `--help` for each command's options.

Reads return the version required for guarded writes. For a memory at version two, deletion and restoration use:

```sh
semmem delete reading --expected-version 2
semmem restore reading --expected-version 3
```

`delete` retains history, and `restore` appends a new revision. `purge` requires `--confirm` and permanently removes the memory, its history, and incoming references, including historical links. Lists and history return summaries; use `show --version` to read a historical body.

Writes commit to SQLite and become searchable before success is returned. Stale versions produce conflicts instead of overwriting newer edits. Startup recovers pending index changes, and identical creates with the same key are retry-safe. SQLite uses WAL with FULL synchronization; durability depends on storage honoring synchronization requests. See [the schema](docs/schema.md) for storage and lifecycle details.

## Metadata and staged retrieval

Set `summary`, `claim_type`, `scope`, `observed_at`, `verified_at`, and `review_after` in the edit frontmatter. The `add` command also accepts corresponding command options. Claim types are `explicit user statement`, `verified observation`, and `inference`. Timestamp values use UTC milliseconds since Unix epoch. Set optional values to YAML `null` to clear them; unknown values are not inferred.

```sh
semmem edit reading
semmem search explanations --scope personal
semmem show reading --view summary
semmem show reading
```

Search is compact stage one; `show` and MCP `get_memories` provide full content by default as stage two. Their `summary` view avoids full bodies and associations. Manual summaries are returned intact; fallback previews contain complete paragraphs. No embeddings or token budgeting are used.

New databases initialize at schema version three. Earlier schemas are rejected and left intact; this version provides no conversion. The service removes application size, count, and runtime deadline restrictions. Caller-selected pagination, atomic writes, version checks, and unavoidable library representation constraints remain.

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
