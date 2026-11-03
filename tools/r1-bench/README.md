# tools/r1-bench

The harness behind `results-2026-11-02-r1.json` (campaign `r1-newest-trees-cold-20261102b`).
Every script here is byte-identical to the file of the same name in that JSON's `harnessSha256`,
so the campaign stays reproducible. Do not edit them: a changed harness is a new campaign with
new hashes. The gates and their results are described in `docs/ARCHITECTURE.md` (R1 pairs).

| File | Role |
|---|---|
| `pair_r1.sh` | Interleaved pair driver: idle guard, snapshot verify, one `run_r1.js` per slot, digests, cleanup |
| `run_r1.js` | One cold `rosettad serve --json --profile` run over the frozen snapshot |
| `verify_r1_snapshot.js`, `r1_snapshot_lib.js` | Snapshot check before every run: `walkInputsDigest`, `SHA256SUMS`, read-only modes |
| `make_r1_snapshot.js` | How the frozen snapshot was made (sizes and counts only) |
| `r1_logical_digest.sql`, `r1_cursor_order.sql`, `r1_ranked.py` | Logical, cursor-order and ranked-result digests |
| `r1_reader_load.js` | The P3 reader load |
| `summarize.py`, `evaluate_r1.py`, `taskc2/assemble_results.py` | Per-run summaries, gate evaluation, the sanitized results JSON |
| `fix_ns.py`, `make_structure.py`, `sim-stress/stress_m4_v3.py`, `product-diagnostic-v3.json` | Stress worker count, snapshot structure, pre-registered predictions |
| `taskc2/run_v2.sh` | The campaign's run order |

**Snapshot.** The frozen corpus (639 files, 1,778,445,372 bytes, `manifestSha256`
`818911071fc4…cb6d3`, `walkInputsDigest` `09bddcd040d2…625bb`) is never committed. It lives on
the bench Mac outside the repository. Check it before use:
`node verify_r1_snapshot.js SNAPSHOT_DIR`.

**Paths.** `pair_r1.sh` and `taskc2/run_v2.sh` hard-code `S=` to the scratch directory the
campaign ran in (binaries in `$S/bin2`, pairs in `$S/pairs2`). To rerun, copy the kit to a working
directory, point `S` there and `SNAP=` at the snapshot, and record the copies' new hashes.
`stress_m4_v3.py` imports `stress_m4.py`, `../sim` and `../v3sim`, which are not part of the
hashed harness.
