#!/usr/bin/env python3
"""Task C: assemble the sanitized R1 results draft (brief v3 §12 E1.8, E4, E5, E6).

Inputs: the gate evaluation evaluate_r1.py wrote (EVAL_JSON), the pair directories, the frozen
snapshot manifest, snapshot-structure.json, stress-prediction.json, ns.json and bin/commits.txt.
Adds the checks evaluate_r1.py does not make (R ledger digests across the rebuild, E6 serial and
N_s rows' sourceSetDigest, P2 §19 big_source_splits) and a profile-backed diagnosis for every
failing gate. Output: counts, digests, timings, binary and manifest SHAs. No paths or content.

usage: python3 -I assemble_results.py PAIRS_DIR EVAL_JSON OUT_JSON
"""
import glob
import hashlib
import json
import os
import re
import statistics
import sys
import time

TASKC = os.path.dirname(os.path.abspath(__file__))
R1 = os.path.dirname(TASKC)
sys.path.insert(0, R1)
import summarize as SM  # noqa: E402
import evaluate_r1 as EV  # noqa: E402

pairs_dir, eval_path, out_path = sys.argv[1:4]
ev = json.load(open(eval_path))
EXT = {"P0": ["P0x"]}


def sha(p):
    return hashlib.sha256(open(p, "rb").read()).hexdigest()


def med(xs):
    xs = [x for x in xs if isinstance(x, (int, float))]
    return statistics.median(xs) if xs else None


def r(x, n=3):
    return None if x is None else round(x, n)


def pair(name):
    return EV.merged(pairs_dir, [name] + EXT.get(name, []))


def report_metric(run_dir, metric):
    rep = json.load(open(os.path.join(run_dir, "report.json")))
    for p in rep.get("profiles", []):
        v = (p.get("metrics") or {}).get(metric)
        if isinstance(v, dict):
            return v.get("value")
    return None


def run_dirs(name):
    out = {}
    for n in [name] + EXT.get(name, []):
        for d in sorted(glob.glob(os.path.join(pairs_dir, n, "[0-9][0-9]-*"))):
            if d.endswith("-swapdiscard") or not os.path.exists(os.path.join(d, "report.json")):
                continue
            out.setdefault(os.path.basename(d).split("-", 1)[1], []).append(d)
    return out


P = {n: pair(n) for n in ("P0", "P1", "P2", "S", "P3", "P3-stress", "X", "E6-default", "E6-stress")}
gates = list(ev["gates"])
extra_gates = []


def add_gate(pair_name, name, threshold, measured, ok, note=None, reported_only=False):
    g = {"pair": pair_name, "gate": name, "threshold": threshold, "measured": measured, "pass": bool(ok)}
    if note:
        g["note"] = note
    if reported_only:
        g["reportedOnly"] = True
    extra_gates.append(g)


# ------------------------------------------------------------------ R: the ledger across the rebuild
rk = os.path.join(pairs_dir, "Rk", "01-B2")
rk_out = None
if os.path.isdir(rk):
    before = json.load(open(os.path.join(rk, "ledger_verify.json")))
    after = json.load(open(os.path.join(rk, "ledger_verify_after_rebuild.json")))
    keys = ("sourceDigest", "sourceSetDigest", "entryDigest", "frameDigest", "metaDigest")
    same = {k: before.get(k) == after.get(k) and before.get(k) is not None for k in keys}
    rks = SM.summarize(rk)
    p2_sset = sorted({(x.get("ledger") or {}).get("sourceSetDigest") for x in P["P2"]["sides"]["B2"]})
    rk_out = {"ledgerDigestsUnchangedByRebuild": same,
              "sourceSetDigestEqualsP2B2": [before.get("sourceSetDigest")] == p2_sset,
              "rebuild": rks.get("rebuild"), "rebuildWallS": None}
    try:
        rk_out["rebuildWallS"] = r(json.load(open(os.path.join(rk, "rebuild.json"))).get("rebuildWallS"), 2)
    except OSError:
        pass
    add_gate("R", "ledger source digest and sourceSetDigest equal across the rebuild (extra B2 run kept, verified before and after)",
             "equal", f"source {'equal' if same['sourceDigest'] else 'DIFFERENT'}; sourceSetDigest "
             f"{'equal' if same['sourceSetDigest'] else 'DIFFERENT'}; = P2's B2 sourceSetDigest: {rk_out['sourceSetDigestEqualsP2B2']}",
             same["sourceDigest"] and same["sourceSetDigest"] and rk_out["sourceSetDigestEqualsP2B2"],
             "rebuild reads the run's ledger; equal by construction unless the rebuild writes to it, so a fourth B2 run was kept "
             "and verified before and after its rebuild")

# ------------------------------------------------------------------ E6: serial and N_s rows vs P2
e6 = P.get("E6-default")
if e6 and P.get("P2"):
    p2_sset = sorted({json.dumps(EV.sset_per_runtime(x), sort_keys=True) for x in P["P2"]["sides"]["B2"]})
    for tag, label in (("W0", "--capture-workers 0 (serial)"), ("WNS", "--capture-workers N_s")):
        runs = e6["sides"].get(tag) or []
        got = sorted({json.dumps(EV.sset_per_runtime(x), sort_keys=True) for x in runs})
        add_gate("E6-default (diagnostic)", f"{label}: sourceSetDigest equal to P2's B2", "equal",
                 f"{len(got)} distinct over {len(runs)} runs; equal to P2: {got == p2_sset}", got == p2_sset and len(runs) > 0,
                 reported_only=True)
        wk = sorted({json.dumps({a: w.get("digest") for a, w in x["walk"].items()}, sort_keys=True) for x in runs})
        p2w = sorted({json.dumps({a: w.get("digest") for a, w in x["walk"].items()}, sort_keys=True) for x in P["P2"]["sides"]["B2"]})
        add_gate("E6-default (diagnostic)", f"{label}: walk digest equal to P2's B2", "equal",
                 f"{len(wk)} distinct over {len(runs)} runs; equal to P2: {wk == p2w}", wk == p2w and len(runs) > 0, reported_only=True)

# ------------------------------------------------------------------ E1 step 5 (d): codex groups hoisted on B2
b2_runs = []
for name, tags in (("P2", ("B2",)), ("S", ("B2S", "L1")), ("P3", ("B2N", "B2L")), ("P3-stress", ("B2N", "B2L")),
                   ("E6-default", None), ("E6-stress", None)):
    if P.get(name):
        for t, runs in P[name]["sides"].items():
            if tags is None or t in tags:
                b2_runs += [x for x in runs if (x.get("walk") or {}).get("codex")]
rec = [x for x in b2_runs if x["walk"]["codex"].get("order") == "recency"]
path_walk = [x for x in b2_runs if x["walk"]["codex"].get("order") != "recency"]
gh = sorted({x["walk"]["codex"].get("groups_hoisted") for x in rec}, key=lambda v: (v is None, v))
gp = sorted({x["walk"]["codex"].get("groups_hoisted") for x in path_walk}, key=lambda v: (v is None, v))
add_gate("E1", "dry-run check (d): B2 walk: line reports groups_hoisted >= 1 for codex", ">= 1",
         f"groups_hoisted {gh} over {len(rec)} B2 recency-walk runs", len(rec) > 0 and all(v is not None and v >= 1 for v in gh),
         f"the {len(path_walk)} runs under --experiment-cold-admission runtime (E6 AR rows) take HEAD's path walk, which does not "
         f"hoist groups by design: groups_hoisted {gp}")

# ------------------------------------------------------------------ worst-run view of the mechanism gates
worst = {}
for name, tag in (("P2", "B2"), ("S", "B2S")):
    if not P.get(name):
        continue
    runs = P[name]["sides"][tag]
    worst[f"{name}:{tag}"] = {
        "runs": len(runs),
        "longestApplyTxMsMax": r(max((x.get("writer") or {}).get("longestApplyTxMs") or 0 for x in runs), 1),
        "longestFlipTxMsMax": r(max((x.get("writer") or {}).get("longestFlipTxMs") or 0 for x in runs), 1),
        "laneSServiceDelayP99MsMax": r(max(EV.lane_s(x)[0] or 0 for x in runs), 2),
        "laneSServiceDelayMaxMsMax": r(max(EV.lane_s(x)[1] or 0 for x in runs), 2),
        "treeAdmissionP99MsMax": r(max(x["treeAdmission"]["p99Ms"] or 0 for x in runs), 2),
        "treeAdmissionShareMax": r(max(x["treeAdmission"]["shareOfWriterWall"] or 0 for x in runs), 4),
        "crossTreeCrossingsMax": max(EV.crossings(x) for x in runs),
        "budgetResidualMax": max((EV.residual(x) or 0) for x in runs),
    }
if P.get("P3"):
    L = P["P3"]["sides"]["B2L"]
    win = lambda x, k, m: ((((x.get("sharedCacheLocks") or {}).get("streaming") or {}).get(k) or {}).get(m) or 0)
    worst["P3:B2L"] = {"runs": len(L), "readerPrologueWaitP99MsMax": r(max(win(x, "readerPrologueWait", "p99Ms") for x in L), 2),
                       "readerPrologueWaitMaxMsMax": r(max(win(x, "readerPrologueWait", "maxMs") for x in L), 2),
                       "readerClosureWallP99MsMax": r(max(win(x, "readerClosureWall", "p99Ms") for x in L), 2)}

# ------------------------------------------------------------------ diagnoses (profile-backed)
def e6_rows(E):
    """Per-row medians the diagnoses cite: peak, pool look-ahead, small-lane milestones, writer lane-S queue."""
    if not P.get(E):
        return {}
    out = {}
    for t, runs in P[E]["sides"].items():
        def wq(x, k):
            v = [l.get(k) or 0 for l in (x.get("writer") or {}).get("lanes") or []
                 if l["lane"] in ("S", "single") and l["runtime"] == next((w["runtime"] for w in (x.get("writer") or {}).get("lanes") or []
                                                                        if w.get("txs", 0) > 10), None)]
            return max(v) if v else None
        out[t] = {"flags": " ".join(runs[0]["extraFlags"]) or "(none)", "runs": len(runs), "CR": med([x["CR"] for x in runs]),
                  "peakFootprintMb": r(med([x["peakFootprintBytes"] for x in runs]) / 1e6, 1),
                  "poolPeakLookAheadMb": r((med([(x.get("capturePool") or {}).get("peakLookAheadBytes") for x in runs]) or 0) / 1e6, 1),
                  "small24hAtNewestS": med([EV.ms(x, "small-recent-ready:24h@newest") for x in runs]),
                  "small7dAtNewestS": med([EV.ms(x, "small-recent-ready:7d@newest") for x in runs]),
                  "newestGiantS": med([EV.ms(x, "newest-giant-ready") for x in runs]),
                  "longestApplyTxMs": r(med([(x.get("writer") or {}).get("longestApplyTxMs") for x in runs]), 1),
                  "writerLaneSQueueWaitP50Ms": r(med([wq(x, "queueWaitP50Ms") for x in runs]), 1)}
    return out


E6D, E6S = e6_rows("E6-default"), e6_rows("E6-stress")

diag = {}
structure = json.load(open(os.path.join(R1, "snapshot-structure.json")))
ns = json.load(open(os.path.join(R1, "ns.json")))


def cursor_first(runs):
    out = {"claudeFirst": 0, "codexFirst": 0}
    for x in runs:
        a = (x.get("cursorOrder") or {}).get("agents") or {}
        if a and a.get("codex", {}).get("minId") == 1:
            out["codexFirst"] += 1
        elif a:
            out["claudeFirst"] += 1
    return out


def strict_local(runs):
    return len({((x.get("cursorOrder") or {}).get("strict_local") or {}).get("TOTAL", {}).get("digest") for x in runs})


def in_path_order(runs):
    return all(all(v["inPathOrder"] == v["cursors"] for v in ((x.get("cursorOrder") or {}).get("agents") or {}).values()) for x in runs)


for pname, sides in (("P0", ("B0", "BE")), ("P2", ("B1", "B2")), ("X", ("B1", "B2X"))):
    if not P.get(pname):
        continue
    allr = [x for s in sides for x in P[pname]["sides"][s]]
    diag[f"{pname}: strict equal"] = {
        "likelyCause": "cursor ids are SQLite rowids handed out in the order the two runtimes (claude-code, codex) first write their "
                       "checkpoints; the runtimes race, so which runtime's block starts at id 1 varies. Within each runtime the ids are "
                       "path rank in every run (strict_local, ids rebased per runtime, is one digest over all runs). No logical, count "
                       "or provenance difference: only the inter-runtime id offset. This is rowid order, the class U-2 lets vary; the "
                       "gate is reported as written",
        "cursorBlockOrder": {s: cursor_first(P[pname]["sides"][s]) for s in sides},
        "strictLocalDistinct": strict_local(allr),
        "idsArePathRankWithinEachRuntime": in_path_order(allr),
    }

if P.get("P1"):
    d = run_dirs("P1")
    m = {t: {k: med([report_metric(x, k) for x in d[t]]) for k in
             ("drift apply time", "canonical apply time", "apply time", "writer budget wait time")} for t in ("BE", "B1")}
    wwall = {t: med([(SM.summarize(x).get("writer") or {}).get("wallMs") for x in d[t]]) for t in ("BE", "B1")}
    diag["P1: CR(B1) / CR(BE)"] = {
        "resolved": "first campaign: CR(B1)/CR(BE) 1.049 FAILED (drift apply 655 -> 832 ms): T1's order-free drift sample read "
                    "the offered sample's cursor path and the current sample joined to its cursor on the writer, for each of 185,025 "
                    "drift events. The writer now keeps each cursor's path and each key's current sample in process RAM "
                    "(store apply/drift.rs DriftCache, taken out for each writer transaction and dropped on rollback), so only an "
                    "event that may sort first, or a key's first event, reads its stored sample back. The medians below are this "
                    "campaign's",
        "medianMs": {"driftApply": {t: r(m[t]["drift apply time"], 1) for t in m}, "canonicalApply": {t: r(m[t]["canonical apply time"], 1) for t in m},
                     "applyTotal": {t: r(m[t]["apply time"], 1) for t in m}, "writerBudgetWait": {t: r(m[t]["writer budget wait time"], 1) for t in m},
                     "streamedWriterWall": {t: r(wwall[t], 1) for t in wwall}},
    }

if P.get("P2"):
    B1, B2 = P["P2"]["sides"]["B1"], P["P2"]["sides"]["B2"]
    pool = lambda runs, k: med([(x.get("capturePool") or {}).get(k) for x in runs])
    e6p = {}
    for E in ("E6-default", "E6-stress"):
        if P.get(E):
            e6p[E] = {t: r(med([x["peakFootprintBytes"] for x in runs]) / 1e6, 1) for t, runs in P[E]["sides"].items()}
    diag["P2: peak footprint B2 / B1"] = {
        "resolved": "first campaign: peak B2/B1 1.203 FAILED (987 -> 1,187 MB). Two causes, measured with a tagging allocator, "
                    "vmmap and a C probe: (1) macOS libmalloc keeps the 64 most recently freed blocks over 2 MiB dirty in the "
                    "footprint, and capture steps free gigabytes of up-to-32 MiB frame and compression buffers; the recency walk "
                    "and lane S keep the writer busy from the start, so a larger RAM projection coincided with full look-ahead; "
                    "(2) a running look-ahead step was not counted against the byte bounds, so workers that started steps "
                    "together overshot them by a batch each, systematically on the big lane. Fixed by rosettad::large_alloc "
                    "(every block over 2 MiB its own mapping, unmapped on free; macOS only) and by counting a running step's "
                    "unread payload (at most one batch) against its lane's bounds (pipeline capture/pool). The medians below "
                    "are this campaign's",
        "medianBytes": {"peakFootprint": {"B1": med([x["peakFootprintBytes"] for x in B1]), "B2": med([x["peakFootprintBytes"] for x in B2])},
                        "poolPeakLookAhead": {"B1": pool(B1, "peakLookAheadBytes"), "B2": pool(B2, "peakLookAheadBytes")},
                        "poolPeakReservedLookAheadB2": pool(B2, "peakReservedLookAheadBytes"),
                        "poolPeakSharedLookAheadB2": pool(B2, "peakSharedLookAheadBytes"),
                        "laneHeadUnemittedPeakB2": {l["lane"]: l.get("peakUnemittedHeadBytes") for l in
                                                    (B2[0].get("prepare") or {}).get("claude-code", {}).get("lanes") or []},
                        "peakPendingWithheldB2": med([((x.get("prepare") or {}).get("claude-code") or {}).get("peakPendingWithheldBytes") for x in B2])},
        "e6PeakFootprintMbMedian": e6p,
        "e6DefaultRows": {t: {k: v[k] for k in ("flags", "peakFootprintMb", "poolPeakLookAheadMb")} for t, v in E6D.items()},
    }
    big = [t for t in structure["walkPlan"]["claude-code"]["treesByLaneByRecency"] if t["lane"] != "S"]
    newest_big = min(big, key=lambda t: t["newestAgeMs"]) if big else None
    cr_be = med([x["CR"] for x in P["P1"]["sides"]["BE"]])
    tm = med([x["T_meta"] for x in B2])
    floor_s = newest_big["maxSourceBytes"] / ns["r1_M4"] if newest_big else None
    diag["P2: newest giant f"] = {
        "likelyCause": "structural on this corpus: the newest tree over 64 MiB is 114 h old and holds a single 452 MB transcript. One "
                       "capture worker reads one source, so that source alone takes about its size / r1_M4 before the tree can close; "
                       "with CR_ref = CR(BE) that is f of about 0.64. The f <= 0.25 target assumed the newest giant is the newest tree "
                       "(126 MB, no 452 MB source; brief §12 E1 census, since grown). Splitting one source across workers is §19 (R1.5, "
                       "streamed pieces). The pre-registered prediction was f 0.673",
        "newestGiantTree": {"bytes": newest_big["bytes"] if newest_big else None, "sources": newest_big["sources"] if newest_big else None,
                            "largestSourceBytes": newest_big["maxSourceBytes"] if newest_big else None,
                            "ageHoursFromNewest": r(newest_big["newestAgeMs"] / 3.6e6, 1) if newest_big else None},
        "singleSourceReadFloorS": r(floor_s, 3), "r1_M4_bytesPerS": r(ns["r1_M4"], 0),
        "fFloor": r(floor_s / (cr_be - tm), 4) if floor_s and cr_be else None,
        "measuredMedianS": med([EV.ms(x, "newest-giant-ready") for x in B2]),
        "bigLaneChainMsMedian": r(med([((x.get("prepare") or {}).get("claude-code") or {}).get("bigLaneChainMs") for x in B2]), 1),
    }
    s_trees = [t for t in structure["walkPlan"]["claude-code"]["treesByLaneByRecency"] if t["lane"] == "S"]
    seq = lambda runs, lane, k: med([next((l.get(k) for l in ((x.get("prepare") or {}).get("claude-code") or {}).get("lanes") or []
                                           if l["lane"] == lane), None) for x in runs])
    wq = lambda runs, k: med([max([l.get(k) or 0 for l in (x.get("writer") or {}).get("lanes") or [] if l["lane"] == "S"] or [0]) for x in runs])
    e6m = {}
    for E in ("E6-default",):
        if P.get(E):
            e6m[E] = {t: {"small24hAtNewestS": med([EV.ms(x, "small-recent-ready:24h@newest") for x in runs]),
                          "small7dAtNewestS": med([EV.ms(x, "small-recent-ready:7d@newest") for x in runs]),
                          "CR": med([x["CR"] for x in runs])} for t, runs in P[E]["sides"].items()}
    diag["P2: small-lane 24h@newest and 7d@newest f"] = {
        "likelyCause": (f"lane S is writer-bound from the start, not blocked at the sequencer: lane-S writer transactions sit in a "
                        f"standing writer queue (queue wait p50 {r(wq(B2, 'queueWaitP50Ms'), 1)} ms median over runs; "
                        f"{E6D.get('L1', {}).get('writerLaneSQueueWaitP50Ms')} ms in the single lane, E6). The 24h@newest small set is "
                        f"the first 4 lane-S trees, {r(sum(t['bytes'] for t in s_trees[:4]) / 1e6, 0)} MB led by the 204 MB live tree "
                        f"of 234 sources; it lands at {min(v['small24hAtNewestS'] for v in E6D.values() if v.get('small24hAtNewestS'))}-"
                        f"{max(v['small24hAtNewestS'] for v in E6D.values() if v.get('small24hAtNewestS'))} s in every E6 default row "
                        f"with tree milestones, --experiment-cold-lanes 1 included, so neither lanes nor chunking explain the miss: lane "
                        f"S's own throughput through the writer does. Lanes are what make small-lane 7d@newest work at all "
                        f"({E6D.get('G', {}).get('small7dAtNewestS')} s against {E6D.get('L1', {}).get('small7dAtNewestS')} s with one "
                        f"lane). Both fractions improved on the first campaign (0.247 and 0.450). The pre-registered f 0.150 and "
                        f"0.280 are model errors"),
        "laneS": {"sourcesBytes": sum(t["bytes"] for t in s_trees), "first4TreesBytes": sum(t["bytes"] for t in s_trees[:4]),
                  "seqHeadWaitMsMedian": r(seq(B2, "S", "headWaitMs"), 1), "seqEndMsMedian": r(seq(B2, "S", "endMs"), 1),
                  "seqInlineMsMedian": r(seq(B2, "S", "inlineMs"), 1),
                  "writerQueueWaitP50MsMedian": r(wq(B2, "queueWaitP50Ms"), 1), "writerQueueWaitP99MsMedian": r(wq(B2, "queueWaitP99Ms"), 1),
                  "writerQueueWaitMaxMsMedian": r(wq(B2, "queueWaitMaxMs"), 1)},
        "longestApplyTxMsMedian": r(med([(x.get("writer") or {}).get("longestApplyTxMs") for x in B2]), 1),
        "writerBusyShareOfCaptureMedian": r(med([(x.get("writer") or {}).get("busyShareOfCapture") for x in B2]), 4),
        "e6DefaultRows": E6D,
    }

if P.get("S"):
    B2S, L1, BES = (P["S"]["sides"][t] for t in ("B2S", "L1", "BES"))
    pool = lambda runs, k: med([(x.get("capturePool") or {}).get(k) for x in runs])
    diag["S: peak footprint"] = {
        "resolved": "first campaign: peak 1.146x BE-stress FAILED; the same two causes and fixes as P2's peak",
        "medianBytes": {"peakFootprint": {t: med([x["peakFootprintBytes"] for x in runs]) for t, runs in (("B2S", B2S), ("L1", L1), ("BES", BES))},
                        "poolPeakLookAhead": {t: pool(runs, "peakLookAheadBytes") for t, runs in (("B2S", B2S), ("L1", L1), ("BES", BES))}},
        "e6StressRows": {t: {k: v[k] for k in ("flags", "peakFootprintMb", "poolPeakLookAheadMb")} for t, v in E6S.items()},
    }
    def mwu(a, b):
        import math
        allv = sorted([(v, 0) for v in a] + [(v, 1) for v in b])
        rk = [0.0] * len(allv)
        i = 0
        while i < len(allv):
            j = i
            while j + 1 < len(allv) and allv[j + 1][0] == allv[i][0]:
                j += 1
            for k in range(i, j + 1):
                rk[k] = (i + j) / 2 + 1
            i = j + 1
        n1, n2 = len(a), len(b)
        u = sum(x for x, (_, g) in zip(rk, allv) if g == 0) - n1 * (n1 + 1) / 2
        z = (u - n1 * n2 / 2) / ((n1 * n2 * (n1 + n2 + 1) / 12) ** 0.5)
        return {"U": r(u, 1), "z": r(z, 2), "pTwoSided": r(math.erfc(abs(z) / 2 ** 0.5), 3)}
    at = lambda x, n: ((x.get("milestones") or {}).get(n) or {}).get("atMs")
    a24 = [at(x, "small-recent-ready:24h@newest") for x in B2S]
    l24 = [at(x, "small-recent-ready:24h@newest") for x in L1]
    a24m = [v - x["T_meta"] * 1000 for v, x in zip(a24, B2S)]
    l24m = [v - x["T_meta"] * 1000 for v, x in zip(l24, L1)]
    diag["S: small-lane 24h@newest / L1"] = {
        "likelyCause": "a tie, not a slowdown: on this corpus the 24h@newest set is small trees only, which the one-lane recency walk "
                       "also takes first, so lanes cannot beat L1 here and the gate reads as a no-regression bound. The milestone "
                       "prints at 10 ms resolution and the medians differ by one step; at 0.1 ms (atMs) the ratio is "
                       f"{r(med(a24) / med(l24), 4)}, and {r(med(a24m) / med(l24m), 4)} measured from metadata readiness; neither is a "
                       "significant difference (Mann-Whitney below). The first campaign's extension pair Sx had 1.008 at 0.1 ms and "
                       "passed only by rounding (0.915 against 0.91 s). Lane-S service delay is unchanged",
        "atMsMedian": {"B2S": r(med(a24), 1), "L1": r(med(l24), 1)},
        "fromTmetaMsMedian": {"B2S": r(med(a24m), 1), "L1": r(med(l24m), 1)},
        "mannWhitneyAtMs": mwu(a24, l24), "mannWhitneyFromTmeta": mwu(a24m, l24m),
        "laneSServiceDelayP99MsMedian": r(med([EV.lane_s(x)[0] for x in B2S]), 2),
    }
    cr_l1 = med([x["CR"] for x in L1])
    diag["S: newest giant f (CR_ref = CR(L1))"] = {
        "likelyCause": "the same single-source floor as P2 (the 452 MB transcript in the newest giant is read by one worker); "
                       "the ratio gate newest giant / L1 passes",
        "fFloor": r((structure and diag.get("P2: newest giant f", {}).get("singleSourceReadFloorS") or 0) / (cr_l1 - med([x["T_meta"] for x in B2S])), 4),
    }

if P.get("P2"):
    rb = [x for x in P["P2"]["sides"]["B2"] if x.get("rebuild")]
    diag["R: strict equal without surrogate ids (strict_noid TOTAL)"] = {
        "likelyCause": "the only differing strict_noid component is ingest_cursor_file_evidence (the checkpoint's $.tail for JSONL and "
                       "$.snapshot_stamp / $.snapshot_revision for SQLite members): rebuild never stats source files and leaves them "
                       "null. Task B found the same on B0 3ca7410's rebuild, so it predates R1. Every other strict_noid component, "
                       "logical and counts are equal (TOTAL_R)",
        "differingStrictNoidComponents": sorted({c for x in rb for c in (x["rebuild"].get("differingComponents") or [])
                                                 if c.startswith("strict_noid:")}),
        "differingOtherComponents (expected: raw cursor ids and message ids, renumbered in ledger frame order)":
            sorted({c for x in rb for c in (x["rebuild"].get("differingComponents") or []) if not c.startswith("strict_noid:")}),
        "rebuildCursorsInPathOrder": [((x.get("rebuildCursorOrder") or {}).get("agents") or {}) and
                                      {a: v["inPathOrder"] for a, v in x["rebuildCursorOrder"]["agents"].items()} for x in rb],
    }

# ------------------------------------------------------------------ E6 attribution table
e6_table = {}
for E in ("E6-default", "E6-stress"):
    if not P.get(E):
        continue
    ref = med([x["CR"] for x in P["P1"]["sides"]["BE"]]) if E == "E6-default" else med([x["CR"] for x in P["S"]["sides"]["BES"]])
    rows = {}
    for t, runs in P[E]["sides"].items():
        lanes = EV.lanes_profile(runs)
        rows[t] = {
            "flags": runs[0]["extraFlags"], "runs": len(runs), "CR": med([x["CR"] for x in runs]),
            "peakFootprintMb": r(med([x["peakFootprintBytes"] for x in runs]) / 1e6, 1),
            "captureWallMs": r(med([((x.get("prepare") or {}).get("claude-code") or {}).get("captureWallMs") for x in runs]), 1),
            "parkedShare": r(med([EV.parked_share(x) for x in runs]), 4),
            "lanes": lanes,
            "bigLaneChainMs": r(med([((x.get("prepare") or {}).get("claude-code") or {}).get("bigLaneChainMs") for x in runs]), 1),
            "writerBusyShareOfCapture": r(med([(x.get("writer") or {}).get("busyShareOfCapture") for x in runs]), 4),
            "resolveMs": r(med([(x.get("writer") or {}).get("resolveMs") for x in runs]), 1),
            "longestApplyTxMs": r(med([(x.get("writer") or {}).get("longestApplyTxMs") for x in runs]), 1),
            "milestonesS": {n: med([EV.ms(x, n) for x in runs]) for n in SM.RECENCY},
            "fractions": {n: r(med([EV.frac(x, n, ref) for x in runs]), 4) for n in SM.RECENCY},
            "crRef": ref,
            "walkDigest": sorted({json.dumps({a: w.get("digest") for a, w in x["walk"].items()}, sort_keys=True) for x in runs}),
            "logicalTotal": sorted({EV.tot(x, "logical") for x in runs}),
            "sourceSetDigest": sorted({(x.get("ledger") or {}).get("sourceSetDigest") for x in runs}),
        }
    e6_table[E] = rows

# ------------------------------------------------------------------ provenance
bins = []
for line in open(os.path.join(R1, "bin2", "commits.txt")):
    if line.startswith("#") or not line.strip():
        continue
    name, role, commit, tree, digest = line.split()
    bins.append({"binary": name, "role": role, "commit": commit, "tree": tree, "sha256": digest})
snap = json.load(open(os.path.join(R1, "snapshot", "snapshot.json")))
harness = {f: sha(os.path.join(R1, f)) for f in ("pair_r1.sh", "run_r1.js", "summarize.py", "r1_logical_digest.sql", "r1_cursor_order.sql",
                                                 "r1_reader_load.js", "r1_ranked.py", "verify_r1_snapshot.js", "r1_snapshot_lib.js",
                                                 "product-diagnostic-v3.json", "evaluate_r1.py", "fix_ns.py", "make_structure.py",
                                                 "make_r1_snapshot.js", "sim-stress/stress_m4_v3.py")}
harness["taskc2/run_v2.sh"] = sha(os.path.join(TASKC, "run_v2.sh"))
harness["taskc2/assemble_results.py"] = sha(os.path.abspath(__file__))

all_gates = gates + extra_gates
if P.get("P2") and P.get("P1"):
    at = lambda runs, n: r(med([((x["milestones"].get(n) or {}).get("atMs")) for x in runs]), 1)
    for g in all_gates:
        if g["pair"] == "P2" and g["gate"].startswith("first-readable"):
            g["note"] = (f"at 0.1 ms resolution (milestone atMs medians): B2 {at(P['P2']['sides']['B2'], 'first-readable')} ms, "
                         f"T_meta {at(P['P2']['sides']['B2'], 'metadata-ready')} ms; BE {at(P['P1']['sides']['BE'], 'first-readable')} ms. "
                         "The pre-registered prediction (T_meta + 0.50 s) simulated claude-code only; the milestone covers every runtime")
DIAG_FOR = [("P0", "strict equal", "P0: strict equal"), ("P2", "strict equal", "P2: strict equal"), ("X", "strict equal", "X: strict equal"),
            ("P1", "CR(B1) / CR(BE)", "P1: CR(B1) / CR(BE)"), ("P2", "peak footprint", "P2: peak footprint B2 / B1"),
            ("P2", "newest giant f", "P2: newest giant f"), ("P2", "small-lane", "P2: small-lane 24h@newest and 7d@newest f"),
            ("S", "peak footprint", "S: peak footprint"), ("S", "small-lane 24h@newest / L1", "S: small-lane 24h@newest / L1"), ("S", "newest giant f", "S: newest giant f (CR_ref = CR(L1))"),
            ("R", "strict equal without surrogate ids", "R: strict equal without surrogate ids (strict_noid TOTAL)")]
for g in all_gates:
    if g["pass"]:
        continue
    for pair_name, prefix, key in DIAG_FOR:
        if g["pair"] == pair_name and g["gate"].startswith(prefix) and key in diag:
            g["diagnosis"] = key
            break
    else:
        g.setdefault("diagnosis", "none recorded: see the gate's measured value and the pair's side statistics")

gated = [g for g in all_gates if not g.get("reportedOnly")]
doc = {
    "format": "r1-results-draft/1",
    "generatedAt": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
    "campaign": "R1 evidence, brief v3 §12 E1.8/E4/E5/E6 (Task C), frozen M4 corpus",
    "privatePathsRecorded": False,
    "machine": {"model": "Mac16,10", "chip": "Apple M4", "cores": "10 (4P + 6E)", "memGb": 16, "os": "macOS 26.7.2",
                "note": "the user's iOS Simulator ran throughout (load1 about 2.3-3.9 during runs, swap flat at 4,094 MB); guard: two consecutive "
                        "2 s samples with CPU idle >= 75% before each run"},
    "binaries": bins,
    "snapshot": {"manifestSha256": snap["manifestSha256"], "walkInputsDigest": snap["walkInputsDigest"], "totals": snap["totals"],
                 "agents": {a: {"files": v["files"], "bytes": v["bytes"], "byDriver": v.get("byDriver")} for a, v in snap["agents"].items()},
                 "snapshotJsonSha256": sha(os.path.join(R1, "snapshot", "snapshot.json")),
                 "verifiedBeforeEveryRun": "verify_r1_snapshot.js: walkInputsDigest + SHA256SUMS + read-only modes (639 files)"},
    "harnessSha256": harness,
    "decisions": {
        "U-1": "accepted",
        "U-2": "accepted: RootTree ledger frame and rowid order may vary per run; the logical projection must not",
        "parentR": "evidence R's strict section drops surrogate row ids (cursor and message ids), because rebuild numbers cursors in ledger "
                   "frame order (falls under U-2). Implemented as strict_noid: every cursor id replaced by (runtime_id, path), message ids "
                   "out. The gate is strict_noid TOTAL; TOTAL_R (without checkpoint file evidence) is reported beside it",
        "iqrPolicy": "every timed pair ran 11 runs per side from the start (P1, P2, S for all three sides, P3), interleaved; P0 is the "
                     "first campaign's 5+6; X (equalities only) ran 7, and the reported-only P3-stress and E6 rows 5 and 3",
        "E6StressRow6": "--capture-workers N_s at the default look-ahead is the same command at both flag sets; it ran once (E6-default WNS)",
        "rerun": "second campaign (r1-newest-trees-cold-20261102b) after the first one (r1-newest-trees-cold-20261102) failed peak "
                 "footprint (P2 1.20x, S 1.15x BE-stress) and R1-U's canonical readiness (P1 1.049x). New B1 and B2 carry the fixes; "
                 "B0 and BE are the first campaign's binaries, byte for byte. P0 (+P0x) is the first campaign's, reused unchanged "
                 "because neither of its binaries changed. E1.8 structure, stress-prediction.json and N_s are also reused (they "
                 "depend only on the snapshot and the machine); the predictions were registered against the first campaign's B2",
    },
    "E1.8": {"structureSha256": sha(os.path.join(R1, "snapshot-structure.json")), "binary": structure.get("binary"),
             "walk": structure.get("walk"), "treesByLaneByRecency": structure["walkPlan"]["claude-code"]["treesByLaneByRecency"],
             "bigSourceSplits": structure.get("bigSourceSplits"), "codexDryRunCheckD": structure.get("codexDryRunCheckD")},
    "predictions": {"sha256": sha(os.path.join(R1, "stress-prediction.json")), "comparison": ev.get("predictions")},
    "E4": {k: ns[k] for k in ("rule", "jsonlPayloadBytes", "appliedPayloadBytes", "r1_M4", "W_M4", "ratio_1_9_W_over_r1", "N_s",
                               "T_meta_median", "finalizePublication_median_s")} | {"nsJsonSha256": sha(os.path.join(R1, "ns.json")), "fromRuns": len(ns["runs"])},
    "gates": all_gates,
    "gateSummary": {"gated": len(gated), "pass": sum(1 for g in gated if g["pass"]), "fail": sum(1 for g in gated if not g["pass"]),
                    "reportedOnly": len(all_gates) - len(gated)},
    "diagnoses": diag,
    "worstRun": worst,
    "R": {"rebuilds": ev["pairs"].get("R", {}).get("rebuilds"), "ledgerCheck": rk_out},
    "P2_section19_bigSourceSplits": structure.get("bigSourceSplits"),
    "E6": e6_table,
    "iqr": ev.get("iqr"),
    "pairs": {k: v for k, v in ev["pairs"].items() if k != "R"},
}
text = json.dumps(doc, indent=1, sort_keys=False)
bad = re.findall(r"(/Users/|/private/|/tmp/|~/|jasper|\.jsonl\b|projects/-)", text)
if bad:
    sys.exit(f"refusing: path-like strings in the results: {sorted(set(bad))}")
with open(out_path, "w") as fh:
    fh.write(text + "\n")
print(json.dumps(doc["gateSummary"]))
