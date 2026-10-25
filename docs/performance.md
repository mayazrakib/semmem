# Performance qualification

Measurements use a release build on Linux x86-64 under a Microsoft hypervisor, with an AMD Ryzen 9 9950X3D reported by the host and 24 available logical CPUs. Rust 1.98.0 compiled the benchmark with thin LTO. SQLite uses WAL and FULL synchronization; successful writes include durable index publication. Storage and filesystem behavior affect these results.

## Workload and interpretation

The deterministic fixture contains 1,000,000 memories. Most Markdown bodies are approximately 512 bytes; every tenth body is approximately 4 KiB. Records have varied kinds, two tags, periodic links, Unicode text, and a second revision on one percent of records. The mixed workload uses 16 clients, each performing 100 operations: 70 percent search, 20 percent reads, and 10 percent writes. Every measured write is followed by a search asserting publication.

Latency measurements cover in-process `Service::dispatch`, including SQLite, query construction, result serialization, queueing, and Tantivy publication. They exclude HTTP, OAuth, MCP framing, internet, and ChatGPT latency. Integration tests exercise those transports separately without making latency claims.

Each isolated query has three warmup calls followed by 100 measured calls. Results are not served from a result cache. Counts include all matches, and score ties use internal memory identity. Concurrent read and write percentiles are reported separately from isolated acceptance gates. Operating-system caches are not flushed; service startup on a reused index is not a cold-disk measurement.

## Search tuning

The initial million-memory run missed the broad phrase target: p95 was 40.70 ms against a 20 ms target. Bounded fuzzy expansion and a four-thread executor reduced it to 26.96 ms, which still failed. Inspection found one segment holding approximately 94 percent of the corpus, preventing useful segment-level parallelism.

The current implementation uses Tantivy's existing log merge policy with four-segment merge groups and a 50,000-document merge-eligibility threshold. Resulting segments can exceed that threshold, but the policy avoids repeatedly merging them into one giant segment. The balanced rebuild brought the same broad phrase query to 10.07 ms p95 without changing its query, corpus, exact count, or ranking semantics.

The deletion-ratio setting applies only to segments eligible for merging. It does not establish a global tombstone-space bound. A rebuild can reclaim space retained by larger segments.

## Final results

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

The 2 ms default is retained. The small 1 versus 2 ms difference does not establish a reliable winner from one run, and longer windows did not improve this workload. Raw measurements, including the earlier phrase failures and the 10 ms write failure, are preserved in [benchmark evidence](../benchmark/). [The final default-window report](../benchmark/benchmark-million-window-2.json) contains every percentile and the workload counters. [Source checksums](../benchmark/source.sha256) identify the final implementation and benchmark source tree.

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

The harness rejects an existing database without its completed benchmark-fixture identity. It reports whether the fixture was created and whether an index existed before opening. Initial fixture generation and index construction take substantially longer than reopening a ready index. Schema and search-generation changes can trigger migrations or an index rebuild.

Reports include p50/p95/p99, mixed throughput, process peak RSS, active storage, retained replaced-index storage, mixed-workload disk growth, and queue/transaction/publication telemetry. Peak RSS during a full rebuild and peak RSS while reopening an index describe different workloads and must not be compared as equivalent steady-state measurements.

The isolated acceptance gates are p95 at most 20 ms for full-text search, 100 ms for fuzzy search, and 200 ms for durable searchable writes. Concurrent results are reported separately. Baseline failures remain part of the evidence rather than being averaged away.
