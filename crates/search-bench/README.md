# Frozen search campaigns

`search-bench` captures FTS latency distributions without retaining organic
content in the repository. It is the required evidence lane for search
optimizations, not a general-purpose load generator.

The tool measures two paths:

- `shared-connection`: repeated calls through one read-only SQLite connection;
- `fresh-connection`: the current HTTP request shape, including one read-only
  connection open per query.

Each case, declared query class, and full declared mix reports p50, p95, and
p99 using nearest-rank percentiles. At least 100 measured cycles are mandatory.
The committed `workloads/fts5-diagnostic-v1.json` is deliberately a diagnostic
mix. Its aggregate percentile is not evidence about the product's actual query
mix. An `observed` suite needs a separately reviewed, privacy-preserving source
such as a histogram of query classes; per-class tails must remain visible so a
selective-heavy mix cannot hide broad-query latency.

## Frozen protocol

1. Build one canonical projection and its ledger from the intended source
   corpus. Quiesce ingestion.
2. Copy the artifacts to a campaign directory outside the repository. Remove
   write permission from every ledger directory/file and from the SQLite file.
3. Freeze the exact ledger and query suite:

   ```text
   search-bench freeze \
     --ledger CAMPAIGN/projection.ledger \
     --queries crates/search-bench/workloads/fts5-diagnostic-v1.json \
     --out CAMPAIGN/campaign.json
   ```

4. Run the baseline on the read-only projection:

   ```text
   search-bench run \
     --campaign CAMPAIGN/campaign.json \
     --ledger CAMPAIGN/projection.ledger \
     --queries crates/search-bench/workloads/fts5-diagnostic-v1.json \
     --db CAMPAIGN/before.sqlite \
     --implementation BEFORE_REV \
     --machine STABLE_MACHINE_LABEL \
     --warmup 5 --cycles 100 \
     --out CAMPAIGN/before.json
   ```

5. Build the candidate projection from that exact frozen ledger, make the
   resulting database read-only, and run the same command to produce
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
and after every run. It also fingerprints the SQLite file, opens it read-only
with `query_only`, runs SQLite integrity/cardinality checks, and rejects ranked
result drift. Fingerprinting warms the filesystem cache, so these are warm
search distributions. Cold process/startup cost needs a separately declared
lane.

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
