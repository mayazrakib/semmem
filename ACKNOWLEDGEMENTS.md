# `ACKNOWLEDGEMENTS.md`

## 1. Third-Party Components

| Component | Version | License | Source | Use |
|---|---|---|---|---|
| SQLite | 3.54.1, bundled through `libsqlite3-sys` 0.39.1 | Public domain | https://sqlite.org/ | Durable records |
| anyhow | 1.0.104 | MIT OR Apache-2.0 | https://github.com/dtolnay/anyhow | Error handling |
| axum | 0.8.9 | MIT | https://github.com/tokio-rs/axum | HTTP server |
| chrono | 0.4.45 | MIT OR Apache-2.0 | https://github.com/chronotope/chrono | Timestamps |
| clap | 4.6.7 | MIT OR Apache-2.0 | https://github.com/clap-rs/clap | Command-line parsing |
| colored_json | 5.0.0 | EPL-2.0 | https://github.com/ctron/colored_json | Colored JSON output |
| comfy-table | 7.2.2 | MIT | https://github.com/nukesor/comfy-table | Terminal tables |
| crossterm | 0.29.0 | MIT | https://github.com/crossterm-rs/crossterm | Terminal control |
| fs2 | 0.4.3 | MIT OR Apache-2.0 | https://github.com/danburkert/fs2-rs | File locking |
| jsonwebtoken | 10.4.0 | MIT | https://github.com/Keats/jsonwebtoken | OAuth token validation |
| parking_lot | 0.12.5 | MIT OR Apache-2.0 | https://github.com/Amanieu/parking_lot | Synchronization |
| reqwest | 0.13.5 | MIT OR Apache-2.0 | https://github.com/seanmonstar/reqwest | HTTP client |
| rmcp | 3.7.0 | Apache-2.0 | https://github.com/modelcontextprotocol/rust-sdk | MCP server and transport |
| rusqlite | 0.41.0 | MIT | https://github.com/rusqlite/rusqlite | SQLite bindings |
| schemars | 1.2.3 | MIT | https://github.com/GREsau/schemars | JSON Schema generation |
| serde | 1.0.229 | MIT OR Apache-2.0 | https://github.com/serde-rs/serde | Serialization |
| serde_json | 1.0.154 | MIT OR Apache-2.0 | https://github.com/serde-rs/json | JSON |
| serde_yaml_ng | 0.10.0 | MIT | https://github.com/acatton/serde-yaml-ng | YAML |
| sha2 | 0.10.9 | MIT OR Apache-2.0 | https://github.com/RustCrypto/hashes | Hashing |
| shell-words | 1.1.1 | MIT OR Apache-2.0 | https://github.com/tmiasko/shell-words | Shell word splitting |
| tantivy | 0.27.0 | MIT | https://github.com/quickwit-oss/tantivy | Lexical search |
| tempfile | 3.27.0 | MIT OR Apache-2.0 | https://github.com/Stebalien/tempfile | Temporary files |
| termimad | 0.34.1 | MIT | https://github.com/Canop/termimad | Terminal Markdown rendering |
| thiserror | 2.0.21 | MIT OR Apache-2.0 | https://github.com/dtolnay/thiserror | Error types |
| tokio and tokio-util | 1.54.1 and 0.7.20 | MIT | https://github.com/tokio-rs/tokio | Asynchronous runtime |
| tracing and tracing-subscriber | 0.1.44 and 0.3.23 | MIT | https://github.com/tokio-rs/tracing | Instrumentation and log output |
| uuid | 1.29.0 | Apache-2.0 OR MIT | https://github.com/uuid-rs/uuid | Identifiers |

## 2. Required Notices

The executable statically links the components above. Each crate's distribution provides its complete license and copyright notices; preserve them when redistributing the executable. `Cargo.lock` resolves the transitive dependencies, which keep their own terms. Project licensing is unresolved, and this file grants no additional rights.
