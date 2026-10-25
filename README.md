# semantic-memory

Semantic, human-readable, persistent memory and retrieval system for AI.

Semantic Memory is a Linux-first Rust server with a host-terminal CLI and an OAuth-protected remote Model Context Protocol (MCP) interface. SQLite owns the readable Markdown, relational metadata, and revision history. A rebuildable Tantivy index supplies ranked full-text, phrase, prefix, and typo-tolerant search. No embedding model or external database server is required.

## Build and run

Build the release binary and start the local service:

```sh
cargo build --release --locked
./target/release/semantic-memory serve
```

The default data directory is `$XDG_DATA_HOME/semantic-memory`, or `$HOME/.local/share/semantic-memory` when `XDG_DATA_HOME` is unset. The CLI connects to the service's private Unix socket. It never opens a second database or search writer.

## Terminal use

Add a memory, search it, and inspect its full Markdown:

```sh
semantic-memory add reading --title "Reading preferences" --tag personal
semantic-memory search "libary"
semantic-memory show reading
semantic-memory edit reading
semantic-memory history reading
```

Add and edit open `VISUAL`, then `EDITOR`, then `vim`. Only the Markdown body goes into the editor. Use named flags for titles, kinds, tags, and links. Memory keys are stable public identifiers supplied by the caller; UUIDs and readable names are accepted. An unchanged edit creates no revision. Failed saves retain the draft and its path.

Reads return the version needed for guarded mutations. Deletion retains history; purge removes the memory, its history, and all incoming references, including historical references:

```sh
semantic-memory delete reading --expected-version 2
semantic-memory restore reading --expected-version 3
semantic-memory purge reading --expected-version 4 --confirm
```

Browse kinds, tags, and relation types with the taxonomy commands. Search and browse support filters and bounded results. Lists and history return summaries; use `show --version` for historical Markdown. Add `--json` for machine-readable output, and use `--help` on any command for its complete options.

## Remote MCP

Remote access requires an external OAuth provider and an HTTPS reverse proxy. Without OAuth configuration, only the private host socket is enabled. The service exposes typed read and write tools, including atomic batches, taxonomy management, history, restoration, and purge. Separate scopes control reading, writing, and purging.

See [deployment](docs/deployment.md) for server configuration and ChatGPT connection requirements, and [schema](docs/schema.md) for the normalized storage model. Actual hosting and live ChatGPT connection verification are separate deployment steps.

## Consistency

Successful writes have committed to SQLite and have been published to search. Concurrent edits require the version that was read, so stale changes cannot overwrite a newer revision. SQLite uses WAL with FULL synchronization. Durability depends on storage honoring synchronization requests.

After an interrupted write, startup replays durable indexing events before accepting requests. If a write commits but search publication fails, its error identifies the committed state; inspect the returned keys and versions before retrying. Identical creates using the same key are retry-safe.

## Verification

Run the test suite, including subprocess crash recovery, and strict lint checks:

```sh
cargo test --locked --features fault-injection --tests
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --check
```

The `fault-injection` feature is for recovery tests. Release deployments should use the default features.

Run the deterministic million-memory qualification benchmark:

```sh
cargo bench --locked --bench search -- --memories 1000000 --clients 16 --iterations 100 --data-dir ./target/benchmark-million
```

The benchmark reports isolated and concurrent latency distributions, throughput, startup time, process memory, and data-directory size. See [performance](docs/performance.md) for measured results and limitations.
