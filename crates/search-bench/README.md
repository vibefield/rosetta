# Frozen search campaigns

`search-bench` captures FTS latency distributions without retaining organic
content in the repository. It is the required evidence lane for search
optimizations, not a general-purpose load generator.

The tool measures the production detached search sidecar through two paths:

- `shared-backend`: repeated calls through one long-lived `SearchBackend` handle;
- `fresh-backend`: a new backend handle per query. Both use the exact product
  query path, including canonical/search coherence checks and read-only SQLite
  snapshots.

Each case, declared query class, and full declared mix reports p50, p95, and
p99 using nearest-rank percentiles. At least 100 measured cycles are mandatory.
The committed `workloads/product-diagnostic-v3.json` is deliberately a diagnostic
mix. Its aggregate percentile is not evidence about the product's actual query
mix. An `observed` suite needs a separately reviewed, privacy-preserving source
such as a histogram of query classes; per-class tails must remain visible so a
selective-heavy mix cannot hide broad-query latency.

Suite queries use the product grammar, not FTS5 or Tantivy syntax: whitespace
separates required terms and double quotes delimit required phrases. The
harness validates the same 4,096-byte/64-clause bounds as the HTTP boundary and
compiles the common query independently for each engine. Suite format 2 binds
queries to this grammar and deterministic FTS tie ordering. Run format 3 binds
new evidence to the detached generation selected by `CURRENT`; earlier reports
remain historical evidence and cannot be mixed into a new comparison.

## Frozen protocol

1. Build one canonical projection and its ledger from the intended source
   corpus. Quiesce ingestion.
2. Copy the artifacts to a campaign directory outside the repository. Remove
   write permission from every ledger directory/file and from the SQLite file.
3. Freeze the exact ledger and query suite:

   ```text
   search-bench freeze \
     --ledger CAMPAIGN/projection.ledger \
     --queries crates/search-bench/workloads/product-diagnostic-v3.json \
     --out CAMPAIGN/campaign.json
   ```

4. Run the baseline on the read-only projection:

   ```text
   search-bench run \
     --campaign CAMPAIGN/campaign.json \
     --ledger CAMPAIGN/projection.ledger \
     --queries crates/search-bench/workloads/product-diagnostic-v3.json \
     --db CAMPAIGN/before.sqlite \
     --implementation BEFORE_REV \
     --machine STABLE_MACHINE_LABEL \
     --warmup 5 --cycles 100 \
     --out CAMPAIGN/before.json
   ```

5. Build the candidate projection from that exact frozen ledger, make the
   resulting canonical database, search generation, `CURRENT`, and containing
   sidecar directories read-only, then run the same command to produce
   `after.json`. The campaign ID, machine label, cycle counts, schema version,
   logical row counts, query/filter digests, hit counts, and ranked-result
   digests must match.
6. Produce the admissibility check:

   ```text
   search-bench compare \
     --before CAMPAIGN/before.json \
     --after CAMPAIGN/after.json \
     --out CAMPAIGN/comparison.json
   ```

The benchmark fingerprints and re-verifies the ledger and query suite before
and after every run. It separately fingerprints the canonical SQLite file and
the selected search generation, opens both read-only with `query_only`, runs
integrity/cardinality/coherence checks, and rejects ranked-result drift.
Fingerprinting warms the filesystem cache, so these are warm search
distributions. Cold process/startup cost needs a separately declared lane.

## Benchmark-only Tantivy prototype

The optional `tantivy-prototype` feature builds a disposable Tantivy 0.26.1
index from the ready production search generation and runs the same frozen
query schedule. It copies the five materialized semantic lanes, including full
externalized text, and the exact indexed document set (prompts and replies).
Tantivy run format 3 distinguishes this input from historical canonical-table
scans and measures SQLite FTS bytes in the detached generation:

```text
cargo run --release -p rosetta-search-bench \
  --features tantivy-prototype --bin tantivy-bench -- run \
  --campaign CAMPAIGN/campaign.json \
  --ledger CAMPAIGN/projection.ledger \
  --queries CAMPAIGN/product-diagnostic-v3.json \
  --db CAMPAIGN/projection.sqlite \
  --baseline CAMPAIGN/fts5-run.json \
  --index CAMPAIGN/tantivy-candidate \
  --implementation REV --machine STABLE_MACHINE_LABEL \
  --threads 4 --memory-mb 1024 --warmup 5 --cycles 100 \
  --out CAMPAIGN/tantivy-run.json
```

This is deliberately not a production backend. It performs no live writes and
adds no synchronization tables. Before building, it replays the FTS queries and
requires their counts and ordered-result digests to match the supplied baseline.
The candidate indexes every filter key and stores all five searchable lanes so
snippet cost and disk usage are not hidden. It validates filters on returned
documents and reports FTS/candidate overlap, but overlap is not a relevance
judgment; adoption still requires reviewed judgments or product evidence.

The completed generation directory and data files are permission-read-only.
Tantivy's mmap reader requires one persistent, empty `.tantivy-meta.lock` file to
remain writable for OS-level coordination; the harness fingerprints it before and
after the query run and rejects non-empty or changed index state.

## Merge and privacy gate

Every performance optimization commit must cite a passing frozen before/after
pair from the same campaign, machine, and configuration, plus the ordinary
correctness suite. A historical estimate or unpaired organic run is not merge
evidence. If either side is deleted before the pair exists, rerun that side.

Organic ledgers, databases, query suites, and raw reports remain local and are
deleted after recording a structure-free summary. Never commit query text,
snippets, row/session IDs, paths, ledger manifests, or database fingerprints.
The generated run report intentionally contains only declared safe labels,
counts, timings, and digests; labels are user-controlled and must not contain
private data.
