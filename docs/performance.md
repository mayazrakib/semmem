# Performance qualification

The retained million-memory measurements predate the current-state architecture and required audit commits. They do not establish memory retrieval performance for the current implementation. The separate Work logging measurements below cover the new logging path only.

The historical version-two section describes the earlier schema and its former application limits. They are retained as historical evidence, not measurements of the metadata update. New benchmark runs use a fresh compatible fixture and report construction, process startup, first request, and warm requests separately. A process restart reuses operating-system caches and does not establish cold-disk performance.

Measurements use a release build on Linux x86-64 under a Microsoft hypervisor, with an AMD Ryzen 9 9950X3D reported by the host and 24 available logical CPUs. Rust 1.98.0 compiled the benchmark with thin LTO. SQLite uses WAL and FULL synchronization; successful writes include durable index publication. Storage and filesystem behavior affect these results.

## Current Review Filtering and Mutation Receipts

[The review and receipt report](../benchmarks/results/review-mutations-current-state.json) measures the current schema-v5 implementation on three fresh 10,000-memory fixtures, each with 4,423-byte bodies and 100 requests per scenario. One percent of fixture memories are due and have no recorded verification. Each read scenario has three warmup requests. First writes alternate the order of legacy and compact modes. Replays reuse the exact original inputs. The report includes source SHA-256 hashes, revision, toolchain, environment, query plans, response sizes, and p50/p95/p99 for every repetition.

| Scenario | Request p95 range across three repetitions |
|---|---:|
| Browse without review filters | 0.174 to 0.209 ms |
| Browse due for review | 0.359 to 0.411 ms |
| Browse with no recorded verification | 0.144 to 0.165 ms |
| Search without review filters | 8.652 to 9.985 ms |
| Search due for review | 1.692 to 1.862 ms |
| Search with no recorded verification | 1.525 to 1.731 ms |
| First write without a request ID | 194.871 to 309.202 ms |
| First write with a request ID | 167.423 to 280.856 ms |
| Identical request replay | 0.260 to 0.293 ms |

Compact first-write responses used 96.1 percent fewer serialized JSON bytes in this workload. This is a byte comparison, not a tokenizer measurement. SQLite query plans used the partial review-date index and the partial recent-order index for unverified records after fixture statistics were collected with `ANALYZE`. Due-date browsing still sorts the matching rows to preserve the existing recent-first order.

These sequential release-build measurements cover in-process `Service::dispatch`, including response construction, SQLite FULL durability, and search publication. They exclude transport, OAuth, MCP framing, final JSON encoding latency, and model latency. Operating-system caches were not flushed. First-write tails vary across repetitions; these results do not establish a general first-write speed improvement or performance at millions of memories. The retained legacy million-memory reports remain historical evidence.

Reproduce with `cargo bench --locked --bench review_mutations`. The benchmark uses dedicated temporary databases and never opens production data.

## Current Work Logging

[The logging report](../benchmarks/results/logging-current-state.json) records three repetitions of 2,000 identical events on fresh databases per scenario. A release build used Rust 1.98.0 on Linux x86-64, WSL2, an AMD Ryzen 9 9950X3D, and 24 logical CPUs. Each synchronous request includes SQLite FULL durability, normalized audit inserts, serialization, and writer coordination. There are no warmups or OS cache flushes. HTTP, OAuth, MCP framing, and AI latency are excluded.

| Events per request | Median events per second | Median request p95 | Transactions per repetition |
|---|---:|---:|---:|
| 1 | 179 | 8.95 ms | 2,000 |
| 100 | 11,906 | 26.28 ms | 20 |

Batching improved median event throughput by 66.6 times in this workload, using 100 times fewer transactions. Request latencies describe different batch sizes and are not per-event latencies. All runs verified the event count and zero search-index events. The report retains per-run storage growth, source-capture details, and the measured benchmark executable checksum. These results do not establish concurrent throughput or performance at millions of log events. Concurrent correctness and shared-transaction behavior are tested separately.

Reproduce with `cargo bench --locked --bench logging`. The harness uses temporary dedicated databases and never opens production data.

## Semmem schema version three

The final `semmem` run reused the compatible million-record schema-three fixture, with 16 clients and 100 iterations per client. [The final report](../benchmarks/results/semmem-million.json) records all samples and startup probes. [The initial schema-three report](../benchmarks/results/schema3-initial-million.json) preserves fixture creation and the first full index construction. Historical version-two reports below remain unchanged.

| Scenario | Final p95 |
|---|---:|
| Rare full-text term | 0.84 ms |
| Common full-text term | 2.41 ms |
| Rare fuzzy term | 2.08 ms |
| Common fuzzy term | 4.14 ms |
| Exact phrase | 6.78 ms |
| Prefix | 4.72 ms |
| Isolated durable searchable write | 169.56 ms |
| Concurrent reads | 6.41 ms |
| Concurrent durable searchable writes | 347.72 ms |

All isolated acceptance gates passed. The loaded write p95 exceeded the separate advisory 200 ms comparison and remains a reported failure of that comparison. Mixed throughput was 586.28 operations per second.

The final restart probe opened the existing index in 4.97 ms, then served its first request in 1.16 ms. The initial schema-three process built the index in 176.06 seconds, then served its first request in 1.63 ms. Those construction and reopening workloads are distinct. Every startup probe ran in its own process; operating-system caches were not flushed. These measurements exclude HTTP, OAuth, MCP framing, and client/model latency.

[Historical source checksums](../benchmarks/results/semmem-source.sha256) identify the source measured in that historical run. The current initializer and audited current-state retrieval require fresh measurements. Tantivy uses a configurable writer arena through `SEMMEM_INDEX_MEMORY_BYTES` (default 64 MiB) and worker count through `SEMMEM_INDEX_THREADS` (default available hardware threads, constrained by Tantivy's minimum arena per worker). Callers can increase these settings without an application maximum. The standard Tantivy merge policy applies; no custom document-count merge threshold is imposed. These index-construction settings do not cap database growth or reject caller input. Production request counts, payloads, query expansion, pool growth, and queues have no application ceilings, and no token budgeting or embeddings are used.

## Historical version-two results

### Workload and interpretation

The deterministic fixture contains 1,000,000 memories. Most Markdown bodies are approximately 512 bytes; every tenth body is approximately 4 KiB. Records have varied kinds, two tags, periodic links, Unicode text, and a second revision on one percent of records. The mixed workload uses 16 clients, each performing 100 operations: 70 percent search, 20 percent reads, and 10 percent writes. Every measured write is followed by a search asserting publication.

Latency measurements cover in-process `Service::dispatch`, including SQLite, query construction, result serialization, queueing, and Tantivy publication. They exclude HTTP, OAuth, MCP framing, internet, and ChatGPT latency. Integration tests exercise those transports separately without making latency claims.

Each isolated query has three warmup calls followed by 100 measured calls. Results are not served from a result cache. Counts include all matches, and score ties use internal memory identity. Concurrent read and write percentiles are reported separately from isolated acceptance gates. Operating-system caches are not flushed; service startup on a reused index is not a cold-disk measurement.

### Search tuning

The initial million-memory run missed the broad phrase target: p95 was 40.70 ms against a 20 ms target. Bounded fuzzy expansion and a four-thread executor reduced it to 26.96 ms, which still failed. Inspection found one segment holding approximately 94 percent of the corpus, preventing useful segment-level parallelism.

The measured implementation used Tantivy's existing log merge policy with four-segment merge groups and a 50,000-document merge-eligibility threshold. Resulting segments can exceed that threshold, but the policy avoids repeatedly merging them into one giant segment. The balanced rebuild brought the same broad phrase query to 10.07 ms p95 without changing its query, corpus, exact count, or ranking semantics.

The deletion-ratio setting applies only to segments eligible for merging. It does not establish a global tombstone-space bound. A rebuild can reclaim space retained by larger segments.

### Final results

The final schema-version-two run with the retained 2 ms batch window produced these p95 measurements:

| Scenario | p95 | Isolated acceptance limit |
|---|---:|---:|
| Rare full-text term | 0.39 ms | 20 ms |
| Common full-text term | 3.59 ms | 20 ms |
| Rare fuzzy term | 1.63 ms | 100 ms |
| Common fuzzy term | 5.00 ms | 100 ms |
| Broad exact phrase | 11.29 ms | 20 ms |
| Prefix | 5.92 ms | 20 ms |
| Durable searchable write | 119.92 ms | 200 ms |
| Reads under 16-client load | 11.13 ms | Reported separately |
| Writes under 16-client load | 281.59 ms | Reported separately |

All isolated gates passed. The mixed workload achieved 579 operations per second. Loaded writes exceeded the additional advisory 200 ms comparison recorded by the harness; they are not an isolated-gate pass. Mean queue wait was 69.28 ms, with a 174.25 ms maximum. Across 260 write requests, SQLite transactions consumed 1.30 seconds and search publication consumed 11.04 seconds. Publication and queueing dominate this workload.

The active data footprint was 1,648,668,160 bytes, approximately 1.54 GiB. Peak process RSS while reusing the index was 148,780 KiB, approximately 145 MiB. Reopening the existing, cached index took 4.85 ms. The earlier full balanced rebuild took approximately 180 seconds and had a much higher process peak RSS of approximately 1.36 GiB. These are different workloads. Retained replaced-index files occupied another 166,814,112 bytes and are reported separately from active storage.

The batch-window experiment retained identical queries and the same million-memory fixture:

| Batch window | Isolated write p95 | Loaded write p95 | Isolated write gate |
|---|---:|---:|---|
| 1 ms | 124.12 ms | 273.37 ms | Pass |
| 2 ms | 119.92 ms | 281.59 ms | Pass |
| 5 ms | 120.23 ms | 339.28 ms | Pass |
| 10 ms | 236.26 ms | 1,143.27 ms | Fail |

The 2 ms default is retained. The small 1 versus 2 ms difference does not establish a reliable winner from one run, and longer windows did not improve this workload. Raw measurements, including the earlier phrase failures and the 10 ms write failure, are preserved in [benchmark evidence](../benchmarks/results/). [The final default-window report](../benchmarks/results/benchmark-million-window-2.json) contains every percentile and the workload counters. [Source checksums](../benchmarks/results/source.sha256) identify the final implementation and benchmark source tree.

## Reproduction

Run qualification with an explicit, dedicated fixture directory:

```sh
cargo bench --locked --bench search -- \
    --memories 1000000 \
    --clients 16 \
    --iterations 100 \
    --batch-window-ms 2 \
    --data-dir ./target/benchmark-million
```

The harness rejects an existing database without its completed benchmark-fixture identity. It reports whether the fixture was created and whether an index existed before opening. Initial fixture generation and index construction take substantially longer than reopening a ready index. Incompatible database schemas are rejected; search-generation changes can require an index rebuild.

Reports include p50/p95/p99, mixed throughput, process peak RSS, active storage, retained replaced-index storage, mixed-workload disk growth, and queue/transaction/publication telemetry. Peak RSS during a full rebuild and peak RSS while reopening an index describe different workloads and must not be compared as equivalent steady-state measurements.

The isolated acceptance gates are p95 at most 20 ms for full-text search, 100 ms for fuzzy search, and 200 ms for durable searchable writes. Concurrent results are reported separately. Baseline failures remain part of the evidence rather than being averaged away.
