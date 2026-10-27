# `semmem`

![Version](https://img.shields.io/badge/Version-0.1.0-737373?style=flat-square)
![License](https://img.shields.io/badge/License-Unknown-737373?style=flat-square)
![Languages](https://img.shields.io/badge/Languages-Rust-737373?style=flat-square)
![Frameworks](https://img.shields.io/badge/Frameworks-SQLite%2C%20Tantivy-737373?style=flat-square)
![Compatibility](https://img.shields.io/badge/Compatibility-Unknown-737373?style=flat-square)
![Build](https://img.shields.io/badge/Build-Not%20verified-737373?style=flat-square)
![Tests](https://img.shields.io/badge/Tests-Not%20measured-737373?style=flat-square)
![Vulnerabilities](https://img.shields.io/badge/Vulnerabilities-Unknown-737373?style=flat-square)

## 1. Introduction

`semmem` is a persistent Markdown memory registry. It offers current-state writes, an append-only audit log, typed relationships, lexical retrieval, a terminal client, and an OAuth-protected MCP server.

## 2. Usage

Install Rust 1.98.0 and Task 3.54.0. Then run `task init`, `task test`, and `task build`. On Linux, `./install.sh` installs the executable and a user service without overwriting existing configuration, and `semmem start` starts the service.

Create a memory and search for it:

```sh
semmem add reading --message "Record reading preferences."
semmem search "reading"
```

[Deployment](docs/deployment.md), [schema](docs/schema.md), and `semmem --help` cover configuration and command details.

## 3. Features

- Atomic writes guarded by opaque concurrency tokens.
- A required audit message on every mutation.
- Permanent deletion of the selected records and their incoming and outgoing references. Previous bodies, snapshots, and historical search aren't available.
- Retrieval of complete Markdown bodies with metadata.
- Immutable Work events with atomic batched appends, idempotent UUID retries, and filtered chronological browsing through MCP and the CLI.

## 4. Architecture

SQLite is authoritative, and the Tantivy index can be rebuilt from it. The service owns storage, and clients connect through a private Unix socket. MCP can reach the same registry through a private tunnel or an OAuth-protected HTTP endpoint. `database/initialize.sql` initializes new databases, and existing incompatible schemas are rejected intact. Benchmark code lives in `tests/perf/`, and historical results stay byte-preserved under `benchmarks/results/`.

## 5. Dependencies

| Dependency | Version | Use |
|---|---|---|
| `anyhow` | 1.0.104 | Runtime: error handling |
| `axum` | 0.8.9 | Runtime: HTTP server |
| `chrono` | 0.4.45 | Runtime: strict event timestamp parsing and fixed-offset formatting |
| `clap` | 4.6.7 | Runtime: command-line parsing |
| `colored_json` | 5.0.0 | Runtime: colored JSON output |
| `comfy-table` | 7.2.2 | Runtime: terminal tables |
| `crossterm` | 0.29.0 | Runtime: terminal control |
| `fs2` | 0.4.3 | Runtime: file locking |
| `jsonwebtoken` | 10.4.0 | Runtime: OAuth token validation |
| `parking_lot` | 0.12.5 | Runtime: synchronization |
| `reqwest` | 0.13.5 | Runtime: HTTP client |
| `rmcp` | 3.7.0 | Runtime: MCP server and streamable HTTP transport |
| `rusqlite` | 0.41.0 | Runtime: SQLite storage, with SQLite bundled |
| `schemars` | 1.2.3 | Runtime: JSON Schema generation |
| `serde` | 1.0.229 | Runtime: serialization |
| `serde_json` | 1.0.154 | Runtime: JSON |
| `serde_yaml_ng` | 0.10.0 | Runtime: YAML |
| `sha2` | 0.10.9 | Runtime: hashing |
| `shell-words` | 1.1.1 | Runtime: shell word splitting |
| `tantivy` | 0.27.0 | Runtime: lexical search |
| `tempfile` | 3.27.0 | Runtime: temporary files |
| `termimad` | 0.34.1 | Runtime: terminal Markdown rendering |
| `thiserror` | 2.0.21 | Runtime: error types |
| `tokio` | 1.54.1 | Runtime: asynchronous runtime |
| `tokio-util` | 0.7.20 | Runtime: asynchronous utilities |
| `tracing` | 0.1.44 | Runtime: instrumentation |
| `tracing-subscriber` | 0.3.23 | Runtime: log output |
| `uuid` | 1.29.0 | Runtime: identifiers |
| `assert_cmd` | 2.2.2 | Development: command-line tests |
| `base64` | 0.22.1 | Development: encoding in tests |
| `rand` | 0.8.8 | Development: random test data |
| `rmcp` | 3.7.0, with client features | Development: MCP client tests |
| `rsa` | 0.9.10 | Development: OAuth key fixtures |

## 6. Roadmap

The CLI, registry, current-state mutations, audit log, search, and MCP integration are implemented. The [performance notes](docs/performance.md) separate historical measurements from the current source.

## 7. Contributing

Read `AGENTS.md` and the applicable skills in `.ai/skills/` before contributing. Set up the development environment with `task init`. Follow the repository conventions, preserve supported behavior, and add relevant tests for behavior changes. Run `task test` and `task build` before opening a pull request. Describe the change, its purpose, and the validation results in the pull request.

## 8. License

`semmem` is pending license classification. No license is granted until the owner resolves it.

See `ACKNOWLEDGEMENTS.md` for third-party notices and attributions.
