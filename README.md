# `semmem`

![Version](https://img.shields.io/badge/Version-0.1.0-737373?style=flat-square)
![License](https://img.shields.io/badge/License-Unknown-737373?style=flat-square)
![Languages](https://img.shields.io/badge/Languages-Rust-737373?style=flat-square)
![Frameworks](https://img.shields.io/badge/Frameworks-SQLite%20and%20Tantivy-737373?style=flat-square)
![Compatibility](https://img.shields.io/badge/Compatibility-Unknown-737373?style=flat-square)
![Build](https://img.shields.io/badge/Build-Not%20verified-737373?style=flat-square)
![Tests](https://img.shields.io/badge/Tests-Not%20measured-737373?style=flat-square)
![Vulnerabilities](https://img.shields.io/badge/Vulnerabilities-Unknown-737373?style=flat-square)

## 1. Introduction

A persistent Markdown memory registry with current-state writes, an append-only audit log, typed relationships, lexical retrieval, a terminal client, and an OAuth-protected MCP server.

## 2. Usage

Use Rust 1.98.0 and Task 3.54.0. Run `task init`, `task test`, and `task build`. On Linux, `./install.sh` installs the executable and user service; `semmem start` starts it. Installation preserves existing configuration.

Create a memory with `semmem add reading --message "Record reading preferences."`, then use `semmem search "reading"`. See [deployment](docs/deployment.md), [schema](docs/schema.md), and `semmem --help` for configuration and command details.

## 3. Features

Atomic writes use opaque concurrency tokens. Every mutation requires an audit message. Deletion permanently removes the selected records and their incoming and outgoing references. Previous bodies, snapshots, and historical search are unavailable. Retrieval returns complete Markdown bodies with metadata.

## 4. Architecture

SQLite is authoritative, and Tantivy is rebuildable. The service owns storage; clients connect through a private Unix socket. MCP can use the same registry through a private tunnel or an OAuth-protected HTTP endpoint. `database/initialize.sql` initializes new databases. Existing incompatible schemas are rejected intact. Benchmark code lives in `tests/perf/`; historical results remain byte-preserved under `benchmarks/results/`.

## 5. Dependencies

| Dependency | Version | Use |
|---|---|---|
| Rust | 1.98.0 | Compiler and Cargo toolchain. |
| SQLite and Tantivy | See `Cargo.lock` | Durable records and rebuildable lexical search. |
| Axum, Tokio, and rmcp | See `Cargo.lock` | HTTP, asynchronous runtime, and MCP transport. |
| Clap and terminal libraries | See `Cargo.toml` and `Cargo.lock` | Command parsing and terminal interface. |

## 6. Roadmap

The CLI, registry, current-state mutations, audit log, search, and MCP integration are implemented. [Performance notes](docs/performance.md) distinguish historical measurements from the current source.

## 7. Contributing

Read `AGENTS.md` and the applicable skills in `.ai/skills/` before contributing. Set up the development environment with `task init`. Follow the repository conventions, preserve supported behavior, and add relevant tests for behavior changes. Run `task test` and `task build` before opening a pull request. Describe the change, its purpose, and the validation results in the pull request.

## 8. License

The licensing classification of `semmem` is unresolved. No license grant is made by this README.

See `ACKNOWLEDGEMENTS.md` for third-party notices and attributions.
