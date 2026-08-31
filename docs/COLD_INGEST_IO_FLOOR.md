# Cold-ingest physical controls — 2026-08-31

## Question

How quickly can this machine ingest the real agent JSONL corpus if Rosetta's
parser, normalizer, canonical schema, reconciliation, raw ledger, indexes, and
search are all removed?

The control executable is `rosettad brutal-ingest`. It uses the ordinary agent
layout/driver boundary to discover the JSONL files Rosetta actually admits,
reads every selected byte into RAM, and then writes a deliberately minimal
SQLite database in one transaction. It has two granularities:

- `file`: one BLOB row per source file; this is the bulk-byte floor.
- `line`: one BLOB row per newline-framed source record; this adds SQLite's
  per-record/B-tree tax without parsing JSON.

A literal single BLOB for this corpus is not a usable control: the 9.48 GB
value is larger than SQLite's value-size limit. The 6,058-row `file` variant is
the practical equivalent of “one batch write”: it retains the source bytes as
opaque values, performs no JSON work, and commits the entire corpus in one
transaction.

The default private-build durability model is `journal_mode=OFF`,
`synchronous=OFF`, one final file and parent-directory `fsync`, and no atomic
publication. A crash before the final sync may destroy the private artifact;
that is acceptable for this lower-bound control because it has no readers.

## Corpus and machine

- Date: 2026-08-31, America/Los_Angeles.
- Branch: `codex/cold-under-60`, based on `b50e568` when the campaign began.
- Host memory: 64 GiB.
- Output: SamsungRed portable SSD.
- Selected scope: nine registered agents, admitted `jsonl` + `JsonLines`
  streams only.
- Roster: 6,058 files, approximately 9.482 GB / 8.83 GiB.
- Source files were live but stable during each pass: every run reported zero
  vanished and zero size-changed files. The corpus grew by about 0.38 MB and
  120 records across the campaign, so cross-run byte counts are recorded
  rather than assumed identical.
- SQLite page size: 65,536 bytes.
- Search and `quick_check` are outside the timed ingest unless explicitly
  stated.

This is a lower bound for Rosetta's full source scope. It omits admitted
SQLite/document snapshots and bounded metadata reads, and it does not checksum
or retain a rebuild ledger.

## Results

All phase times below are seconds. “Wall” is `/usr/bin/time -lp`; “internal” is
from process start through final sync, before Rust drops the retained raw
buffers.

| Variant | Bytes read | Rows | DB bytes | Discover | Read RAM | Insert | Commit | Pager close | Backup | Fsync | Internal | Wall |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| File BLOB, disk | 9,481,530,718 | 6,058 | 9,533,259,776 | 2.116 | 7.155 | 18.203 | 0.038 | ~18.13¹ | 0 | 0.056 | 45.699 | 46.87 |
| Line BLOB, disk/default cache | 9,481,626,638 | 2,411,717 | 10,616,700,928 | 1.765 | 6.695 | 39.851 | 0.047 | 9.983 | 0 | 0.067 | 58.482 | 59.70 |
| Line BLOB, disk/F_NOCACHE | 9,481,776,042 | 2,411,809 | 10,616,897,536 | 2.115 | 4.991 | 35.808 | 0.047 | 13.345 | 0 | 0.072 | 56.446 | 57.67 |
| Line BLOB, memory then backup | 9,481,820,195 | 2,411,837 | 10,616,963,072 | 1.775 | 4.883 | 13.200 | 0.103 | 1.047 | 50.460 | 0.012 | 71.548 | 72.38 |

¹ The first corrected file-granularity run predated explicit close timing. Its
unattributed `internal − measured phases` residual is approximately 18.13s and
is SQLite connection close/pager drain. Later line runs instrument this
directly.

An earlier file-granularity run included `PRAGMA quick_check`: it took 71.309s
internally, of which 37.898s was the validation scan alone. Its database passed.
Validation is therefore reported separately instead of being mislabeled as
ingest.

The retained line/F_NOCACHE database was independently checked after timing:

```text
SELECT count(*), sum(length(payload)) FROM raw_record;
2411809|9481776042
PRAGMA quick_check;
ok
real 36.34
```

The byte sum exactly equals the run's read and inventory byte counts.

## What the controls establish

1. Raw source reading is not the primary bottleneck on this host. It costs
   5–7s after a 1.8–2.1s discovery walk. `F_NOCACHE` was slightly faster here,
   presumably because it avoided page-cache pollution and contention; it did
   not reveal a hidden slow-device floor.
2. SQLite representation dominates. Writing 9.48 GB as only 6,058 file rows
   takes about 47s wall. Splitting the same bytes into 2.41 million rows raises
   the stable wall floor to 58–60s even with no JSON parsing or indexes.
3. The raw-line SQLite file is 1.120× the input size. The file-BLOB database is
   1.005×. Row/B-tree framing adds roughly 1.08 GB before any canonical model.
4. RAM SQLite is not a generic win. It reduces row construction from 35.8s to
   13.2s, but backing up the 10.62 GB database takes 50.46s, making the run
   roughly 14s slower overall. Its peak memory footprint was about 23.0 GB.
5. `commit` and the explicit final `fsync` are tiny in these runs. Most dirty
   page work occurs during inserts and SQLite pager close. Measuring only
   transaction commit would be badly misleading.

## Comparison with the typed Rosetta build

After the controls identified the important boundary, the fastest correct
candidate was repeated twice against the live nine-agent scope:

| Run | Parsed records | Canonical ops | Source bytes | Wall to canonical ready |
|---|---:|---:|---:|---:|
| V10 | ~2.412 million | ~1.765 million | ~9.95 GB | 52.88s |
| V11 | 2,412,420 | 1,764,756 | 9,956,829,327 | 55.31s |

Mean wall time was 54.10s, with a 2.43s spread. Each run produced a durable
checksummed raw ledger, extracted assets, a typed 1.05 GB canonical SQLite
projection, full canonical validation, and atomic publication. Search was
deliberately detached from canonical readiness. V11 reported zero hard source
errors and wrote 2,425,745 ledger frames containing 9,571,848,059 source bytes.

The result being faster than the raw-line SQLite control is not a
contradiction. The line control materializes all 9.48 GB plus row overhead in a
10.62 GB SQLite B-tree. Rosetta writes the large raw payload sequentially to
its ledger, writes only normalized user-facing data to a roughly 1.05 GB
SQLite projection, and overlaps those streams. The 46.87s file-BLOB control is
the closer physical reference for that architecture.

This changes the interpretation of “60 seconds”: it is a physical-edge goal
for this corpus if raw-source durability is retained, not an ordinary
engineering budget. Reaching it requires avoiding per-record storage of the
raw payload in SQLite and overlapping ledger output, normalization, and
canonical writes. A serial design cannot reach that range.

V11 was independently rebuilt from its published ledger. A semantic
relational comparison found zero differences across runtimes, projects,
sessions, messages, edits, usage, drift, and source cursors; extracted assets
were byte-identical. The comparison script is
`tools/compare_projection_semantics.sql`.

## Reproduction

```sh
/usr/bin/time -lp rosettad brutal-ingest \
  --db /path/to/new/raw.sqlite \
  --home /Users/jamesyong \
  --granularity line \
  --durability fsync \
  --page-size 65536 \
  --sqlite-location disk \
  --source-cache bypass \
  --json
```

Add `--quick-check` to measure validation explicitly. The command refuses an
existing output database so a control can never append to or overwrite prior
evidence.
