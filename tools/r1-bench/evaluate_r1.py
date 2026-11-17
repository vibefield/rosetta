#!/usr/bin/env python3
"""R1 gate evaluation (Task C): every gate of brief v3 "Final targets" and §12 E5, exactly as written,
from the pair directories pair_r1.sh wrote under PAIRS_DIR. Prints the gate table and writes a
sanitized results JSON (counts, digests, timings, binary and manifest SHAs; no paths or content).

usage: python3 -I evaluate_r1.py PAIRS_DIR OUT_JSON [--predictions stress-prediction.json] [--ns N]
"""
import argparse
import glob
import json
import os
import re
import statistics
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import summarize as SM  # noqa: E402

GATES = []
PROBLEMS = []


def gate(pair, name, threshold, measured, ok, note=None):
    g = {"pair": pair, "gate": name, "threshold": threshold, "measured": measured, "pass": bool(ok)}
    if note:
        g["note"] = note
    GATES.append(g)
    return ok


def med(xs):
    xs = [x for x in xs if isinstance(x, (int, float))]
    return statistics.median(xs) if xs else None


def iqr_pct(xs):
    m = SM.median_iqr(xs)
    return None if m is None else m["iqrPctOfMedian"]


def r3(x, n=3):
    return None if x is None else round(x, n)


def load_pair(pairs_dir, name):
    d = os.path.join(pairs_dir, name)
    if not os.path.isdir(d):
        return None
    meta = json.load(open(os.path.join(d, "pair.json")))
    sides = {}
    for run_dir in sorted(glob.glob(os.path.join(d, "[0-9][0-9]-*"))):
        base = os.path.basename(run_dir)
        if base.endswith("-swapdiscard") or not os.path.exists(os.path.join(run_dir, "report.json")):
            continue
        tag = base.split("-", 1)[1]
        sides.setdefault(tag, []).append(SM.summarize(run_dir))
    return {"name": name, "meta": meta, "sides": sides}


def merged(pairs_dir, names):
    """Several pair directories as one (a 7-round pair plus its extension to 11)."""
    out = None
    for n in names:
        p = load_pair(pairs_dir, n)
        if p is None:
            continue
        if out is None:
            out = {"name": "+".join(names), "meta": p["meta"], "metas": [p["meta"]], "sides": {}}
        else:
            out["metas"].append(p["meta"])
        for tag, runs in p["sides"].items():
            out["sides"].setdefault(tag, []).extend(runs)
    return out


# ---------------------------------------------------------------- per-run accessors
def tot(r, sec, k="TOTAL"):
    return (((r.get("digests") or {}).get(sec) or {}).get(k) or {}).get("digest")


def ms(r, name):
    return (r["milestones"].get(name) or {}).get("elapsed_s")


def frac(r, name, cr_ref):
    m, tm = ms(r, name), r["T_meta"]
    if m is None or tm is None or cr_ref is None or cr_ref <= tm:
        return None
    return (m - tm) / (cr_ref - tm)


def lane_s(r):
    p99, mx = [], []
    for agent, p in (r.get("prepare") or {}).items():
        for lane in p.get("lanes") or []:
            if lane.get("lane") == "S":
                sd = lane.get("serviceDelayMs") or {}
                if sd.get("count"):
                    p99.append(sd.get("p99Ms") or 0)
                    mx.append(sd.get("maxMs") or 0)
    return (max(p99) if p99 else None), (max(mx) if mx else None)


def crossings(r):
    return sum((p.get("crossTreeCrossings") or 0) for p in (r.get("prepare") or {}).values())


def residual(r):
    b = (r.get("writer") or {}).get("budgetResidualBytes")
    return None if b is None else sum(b.values())


def locked(r):
    locks = r.get("sharedCacheLocks") or {}
    reader = (r.get("reader") or {}).get("totals") or {}
    return {"stderrLockedLines": r.get("stderrLockedLines"), "readerLockedBodies": reader.get("lockedBodies"),
            "readerLockedRefusals": locks.get("readerLockedRefusals"), "readerLockedRetries": locks.get("readerLockedRetries"),
            "writerLockOrderViolations": locks.get("writerLockOrderViolations")}


def distinct(runs, getter):
    vals = [getter(r) for r in runs]
    return len(set(json.dumps(v, sort_keys=True) for v in vals)), vals


def all_equal(runs, getter):
    n, vals = distinct(runs, getter)
    return n == 1 and vals[0] is not None


def sset_per_runtime(r):
    lv = r.get("ledger") or {}
    return {"root": lv.get("sourceSetDigest"), **{x["runtime"]: x.get("sourceSetDigest") for x in lv.get("runtimes", [])}}


def usage_sum(r):
    w = r.get("writer") or {}
    vals = [w.get("usageIdentitiesOwned"), w.get("usageCopiesDropped"), w.get("usageCopiesDisplaced")]
    return None if any(v is None for v in vals) else sum(vals)


def side_stats(runs, cr_ref=None):
    """Sanitized per-side summary for the results file."""
    s = {
        "runs": len(runs),
        "binarySha256": runs[0]["binarySha256"] if runs else None,
        "extraFlags": runs[0]["extraFlags"] if runs else None,
        "CR": SM.median_iqr([r["CR"] for r in runs]),
        "T_meta": SM.median_iqr([r["T_meta"] for r in runs]),
        "peakFootprintBytes": SM.median_iqr([r["peakFootprintBytes"] for r in runs]),
        "processWallMs": SM.median_iqr([r["processWallMs"] for r in runs]),
        "milestones": {n: SM.median_iqr([ms(r, n) for r in runs]) for n in SM.RECENCY + ["canonical-ready", "metadata-ready"]},
        "finalizePublicationS": SM.median_iqr([((r["profile"].get("bulk finalization time") or 0) + (r["profile"].get("publication time") or 0)) / 1000.0
                                               for r in runs]),
        "guard": {"load1AtStart": SM.median_iqr([((r.get("guard") or {}).get("loadGuard") or {}).get("load1AtStart") for r in runs]),
                  "waitedS": SM.median_iqr([((r.get("guard") or {}).get("loadGuard") or {}).get("waitedS") for r in runs]),
                  "proceededOverGuard": sum(1 for r in runs if ((r.get("guard") or {}).get("loadGuard") or {}).get("proceededOverGuard")),
                  "swapGrewMbMax": max([r["swapGrewMb"] or 0 for r in runs]) if runs else None},
        "digests": {
            "logicalTotal": sorted({tot(r, "logical") for r in runs if tot(r, "logical")}),
            "strictTotal": sorted({tot(r, "strict") for r in runs if tot(r, "strict")}),
            "strictNoidTotal": sorted({tot(r, "strict_noid") for r in runs if tot(r, "strict_noid")}),
            "strictNoidTotalR": sorted({tot(r, "strict_noid", "TOTAL_R") for r in runs if tot(r, "strict_noid", "TOTAL_R")}),
            "strictLocalTotal": sorted({((r.get("cursorOrder") or {}).get("strict_local") or {}).get("TOTAL", {}).get("digest")
                                        for r in runs if r.get("cursorOrder")}),
            "cursorBases": sorted({json.dumps({a: v["minId"] for a, v in (r.get("cursorOrder") or {}).get("agents", {}).items()},
                                              sort_keys=True) for r in runs if r.get("cursorOrder")}),
            "surrogateMessageId": sorted({((r.get("digests") or {}).get("surrogate") or {}).get("message_id", {}).get("digest")
                                          for r in runs if r.get("digests")}),
            "countsDistinct": distinct(runs, lambda r: (r.get("digests") or {}).get("count"))[0] if runs else None,
            "counts": (runs[0].get("digests") or {}).get("count") if runs else None,
            "sourceDigest": sorted({(r.get("ledger") or {}).get("sourceDigest") for r in runs if r.get("ledger")}),
            "sourceSetDigest": sorted({(r.get("ledger") or {}).get("sourceSetDigest") for r in runs if r.get("ledger")}),
            "sourceSetDigestPerRuntime": sorted({json.dumps(sset_per_runtime(r), sort_keys=True) for r in runs if r.get("ledger")}),
            "entryDigestDistinct": len({(r.get("ledger") or {}).get("entryDigest") for r in runs}),
            "rankedIds": sorted({(r.get("ranked") or {}).get("ranked_ids_sha256") for r in runs if r.get("ranked")}),
            "walkDigest": sorted({json.dumps({a: w.get("digest") for a, w in r["walk"].items()}, sort_keys=True) for r in runs if r["walk"]}),
            "assets": sorted({json.dumps([r["outputs"]["assetsDirFiles"], r["outputs"]["assetsDirBytes"]]) for r in runs}),
        },
        "writer": {k: SM.median_iqr([(r.get("writer") or {}).get(k) for r in runs])
                   for k in ("longestApplyTxMs", "maxApplyTxBytes", "longestFlipTxMs", "maxFlipTailBytes", "resolveMs",
                             "busyShareOfCapture", "wallMs", "idleMs", "flipBatches", "flipTrees", "flipSessions",
                             "usageOwnerBytes", "usageDisplacedVisible")},
        "usage": {k: sorted({(r.get("writer") or {}).get(k) for r in runs}, key=lambda v: (v is None, v))
                  for k in ("usageIdentitiesOwned", "usageCopiesDropped", "usageCopiesDisplaced", "usageIdentitiesUnclaimed",
                            "usageDisplacedVisible")},
        "laneS": {"serviceDelayP99Ms": SM.median_iqr([lane_s(r)[0] for r in runs]), "serviceDelayMaxMs": SM.median_iqr([lane_s(r)[1] for r in runs])},
        "treeAdmission": {"shareOfWriterWall": SM.median_iqr([r["treeAdmission"]["shareOfWriterWall"] for r in runs]),
                          "p99Ms": SM.median_iqr([r["treeAdmission"]["p99Ms"] for r in runs]),
                          "batches": SM.median_iqr([r["treeAdmission"]["batches"] for r in runs]),
                          "renumberRows": sorted({r["treeAdmission"]["renumberRows"] for r in runs})},
        "crossTreeCrossings": sorted({crossings(r) for r in runs}),
        "budgetResidualBytes": sorted({residual(r) for r in runs}, key=lambda v: (v is None, v)),
        "locked": {k: sorted({locked(r)[k] for r in runs}, key=lambda v: (v is None, v)) for k in locked(runs[0])} if runs else None,
        "stderrPanicLines": sorted({r.get("stderrPanicLines") for r in runs}, key=lambda v: (v is None, v)),
        "capturePool": {k: SM.median_iqr([(r.get("capturePool") or {}).get(k) for r in runs])
                        for k in ("busyMs", "parkedMs", "idleMs", "tailMs", "wallMs", "peakHeadBytesTotal", "peakLookAheadBytes")},
        "claudePrepare": {k: SM.median_iqr([((r.get("prepare") or {}).get("claude-code") or {}).get(k) for r in runs])
                          for k in ("captureWallMs", "bigLaneChainMs", "laneSwitches", "peakPendingWithheldBytes", "lateSamples")},
        "profile": {k: SM.median_iqr([r["profile"].get(k) for r in runs])
                    for k in ("sequencer head wait time", "writer budget wait time", "canonical apply time", "apply time",
                              "bulk finalization time", "publication time", "capture step time")},
        "structural7dNewestOk": [SM.structural_7d(r).get("ok") for r in runs],
    }
    if cr_ref is not None:
        s["crRef"] = cr_ref
        s["fractions"] = {n: SM.median_iqr([frac(r, n, cr_ref) for r in runs]) for n in SM.RECENCY}
    return s


def lanes_profile(runs):
    """E6: per-lane head waits, held waits and service delay (claude-code), medians."""
    out = {}
    for r in runs:
        for lane in ((r.get("prepare") or {}).get("claude-code") or {}).get("lanes") or []:
            d = out.setdefault(lane["lane"], {"headWaitMs": [], "heldP99Ms": [], "serviceP99Ms": [], "serviceMaxMs": [], "endMs": []})
            d["headWaitMs"].append(lane.get("headWaitMs"))
            d["heldP99Ms"].append((lane.get("heldMs") or {}).get("p99Ms"))
            d["serviceP99Ms"].append((lane.get("serviceDelayMs") or {}).get("p99Ms"))
            d["serviceMaxMs"].append((lane.get("serviceDelayMs") or {}).get("maxMs"))
            d["endMs"].append(lane.get("endMs"))
    return {k: {m: r3(med(v), 2) for m, v in d.items()} for k, d in out.items()}


def parked_share(r):
    p = r.get("capturePool") or {}
    if not p.get("wallMs") or not p.get("workers"):
        return None
    return p.get("parkedMs", 0) / (p["workers"] * p["wallMs"])


# ---------------------------------------------------------------- gates
def eq_gate(pair, name, runs, getter, note=None):
    n, vals = distinct(runs, getter)
    ok = n == 1 and vals[0] is not None
    shown = sorted({(json.dumps(v, sort_keys=True)[:24] if v is not None else "null") for v in vals})
    return gate(pair, name, "equal", f"{n} distinct over {len(runs)} runs" + ("" if ok else f" {shown}"), ok, note)


def ratio_gate(pair, name, num, den, limit, unit="x"):
    if num is None or den is None:
        return gate(pair, name, f"<= {limit}{unit}", "missing", False)
    x = num / den
    return gate(pair, name, f"<= {limit}{unit}", f"{x:.4f} ({num:.3f} / {den:.3f})", x <= limit)


def iqr_note(pair, tag, runs):
    i = iqr_pct([r["CR"] for r in runs])
    j = iqr_pct([r["peakFootprintBytes"] for r in runs])
    return {"side": tag, "runs": len(runs), "crIqrPct": i, "peakIqrPct": j, "over3pct": (i or 0) > 3 or (j or 0) > 3}


def eval_P0(P):
    B0, BE = P["sides"]["B0"], P["sides"]["BE"]
    both = B0 + BE
    eq_gate("P0", "ledger source digest equal (B0 = BE)", both, lambda r: (r.get("ledger") or {}).get("sourceDigest"))
    eq_gate("P0", "logical equal", both, lambda r: tot(r, "logical"))
    eq_gate("P0", "strict equal", both, lambda r: tot(r, "strict"))
    eq_gate("P0", "counts equal", both, lambda r: (r.get("digests") or {}).get("count"))
    eq_gate("P0", "ranked digests equal", both, lambda r: (r.get("ranked") or {}).get("ranked_ids_sha256"))
    ratio_gate("P0", "CR(BE) / CR(B0)", med([r["CR"] for r in BE]), med([r["CR"] for r in B0]), 1.03)
    ratio_gate("P0", "peak(BE) / peak(B0)", med([r["peakFootprintBytes"] for r in BE]), med([r["peakFootprintBytes"] for r in B0]), 1.03)
    return {"BE_sourceSetDigest": sorted({(r.get("ledger") or {}).get("sourceSetDigest") for r in BE})}


def eval_P1(P):
    BE, B1 = P["sides"]["BE"], P["sides"]["B1"]
    both = BE + B1
    comps = sorted({c for r in both for c in ((r.get("digests") or {}).get("logical") or {}) if c != "TOTAL"})
    usage = {"session_usage", "session_usage_total", "usage_bucket"}
    differing = [c for c in comps if distinct(both, lambda r, c=c: ((r["digests"] or {}).get("logical") or {}).get(c))[0] != 1]
    unexplained = [c for c in differing if c not in usage]
    gate("P1", "every logical table equal except explained usage-delta classes", "equal",
         f"differing components: {differing or 'none'}", not unexplained,
         None if not differing else "usage components differ; class (a)-(c) explanation required (see usageClassDiff)")
    eq_gate("P1", "drift_agg equal, including the sample", both, lambda r: ((r["digests"] or {}).get("logical") or {}).get("drift_agg"))
    eq_gate("P1", "strict equal", both, lambda r: tot(r, "strict"))
    eq_gate("P1", "source digest equal", both, lambda r: (r.get("ledger") or {}).get("sourceDigest"))
    eq_gate("P1", "sourceSetDigest equal", both, lambda r: (r.get("ledger") or {}).get("sourceSetDigest"))
    eq_gate("P1", "ranked digests equal", both, lambda r: (r.get("ranked") or {}).get("ranked_ids_sha256"))
    eq_gate("P1", "counts equal except session_usage", both,
            lambda r: {k: v for k, v in ((r.get("digests") or {}).get("count") or {}).items() if not k.startswith("session_usage")})
    ratio_gate("P1", "CR(B1) / CR(BE)", med([r["CR"] for r in B1]), med([r["CR"] for r in BE]), 1.03)
    ratio_gate("P1", "peak(B1) / peak(BE)", med([r["peakFootprintBytes"] for r in B1]), med([r["peakFootprintBytes"] for r in BE]), 1.03)
    w = [(r.get("writer") or {}) for r in B1]
    dv = sorted({x.get("usageDisplacedVisible") for x in w}, key=lambda v: (v is None, v))
    gate("P1", "usage_displaced_visible (B1; expected 0, no flips)", "reported (expected 0)", f"{dv}", dv == [0])
    return {"usageReport": {k: sorted({x.get(k) for x in w}, key=lambda v: (v is None, v)) for k in
                            ("usageOwnerBytes", "usageIdentitiesOwned", "usageCopiesDropped", "usageCopiesDisplaced",
                             "usageIdentitiesUnclaimed", "usageDisplacedVisible")}, "differingLogicalComponents": differing}


def eval_P2(P, cr_be, fr_be, e6_walk_runs):
    B1, B2 = P["sides"]["B1"], P["sides"]["B2"]
    both = B1 + B2
    eq_gate("P2", "logical equal (usage included)", both, lambda r: tot(r, "logical"))
    eq_gate("P2", "strict equal", both, lambda r: tot(r, "strict"),
            "cursor ids per runtime vs B1; see strict_local diagnostic")
    eq_gate("P2", "counts equal", both, lambda r: (r.get("digests") or {}).get("count"))
    eq_gate("P2", "ranked digests equal", both, lambda r: (r.get("ranked") or {}).get("ranked_ids_sha256"))
    eq_gate("P2", "sourceSetDigest equal, per runtime and at the root", both, sset_per_runtime)
    eq_gate("P2", "asset file count and bytes equal", both, lambda r: [r["outputs"]["assetsDirFiles"], r["outputs"]["assetsDirBytes"]])
    eq_gate("P2", "B2 walk digest equal across its runs (+ E6 serial and N_s rows)", B2 + e6_walk_runs,
            lambda r: {a: w.get("digest") for a, w in r["walk"].items()})
    eq_gate("P2", "sum owned + dropped + displaced equal", both, usage_sum)
    cr1, cr2 = med([r["CR"] for r in B1]), med([r["CR"] for r in B2])
    ratio_gate("P2", "canonical-ready CR(B2) / CR(B1)", cr2, cr1, 1.03)
    ratio_gate("P2", "peak footprint B2 / B1", med([r["peakFootprintBytes"] for r in B2]), med([r["peakFootprintBytes"] for r in B1]), 1.03)
    fr2 = med([ms(r, "first-readable") for r in B2])
    tm2 = med([r["T_meta"] for r in B2])
    gate("P2", "first-readable <= T_meta + 2.0 s, and below BE", "<= T_meta + 2.0 s; < BE",
         f"{fr2} s (T_meta {tm2}; BE {fr_be})", fr2 is not None and tm2 is not None and fr2 <= tm2 + 2.0 and fr_be is not None and fr2 < fr_be)
    for name, label, lim in (("recent-ready:24h@newest", "24h@newest", 0.65), ("newest-trees-ready", "newest-10 trees", 0.80),
                             ("newest-giant-ready", "newest giant", 0.25), ("small-recent-ready:24h@newest", "small-lane 24h@newest", 0.20),
                             ("small-recent-ready:7d@newest", "small-lane 7d@newest", 0.30)):
        f = med([frac(r, name, cr_be) for r in B2])
        gate("P2", f"{label} f (CR_ref = CR(BE))", f"f <= {lim}", "missing" if f is None else f"{f:.4f}", f is not None and f <= lim)
    st = [SM.structural_7d(r) for r in B2]
    f7 = med([frac(r, "recent-ready:7d@newest", cr_be) for r in B2])
    gate("P2", "7d@newest structural check (reported)", "t <= CR(B2) - (finalize + publication) + 1 s",
         f"{sum(1 for s in st if s.get('ok'))}/{len(st)} runs ok; f {r3(f7, 4)}", all(s.get("ok") for s in st))
    a = med([(r.get("writer") or {}).get("longestApplyTxMs") for r in B2])
    amax = max([(r.get("writer") or {}).get("longestApplyTxMs") or 0 for r in B2])
    gate("P2", "longest apply tx (capture window)", "<= 250 ms", f"median {r3(a, 1)} ms (max run {r3(amax, 1)}); maxApplyTxBytes "
         f"{med([(r.get('writer') or {}).get('maxApplyTxBytes') for r in B2])}", a is not None and a <= 250)
    fl = med([(r.get("writer") or {}).get("longestFlipTxMs") for r in B2])
    flmax = max([(r.get("writer") or {}).get("longestFlipTxMs") or 0 for r in B2])
    gate("P2", "longest flip tx", "<= 250 ms", f"median {r3(fl, 1)} ms (max run {r3(flmax, 1)}); maxFlipTailBytes "
         f"{med([(r.get('writer') or {}).get('maxFlipTailBytes') for r in B2])}", fl is not None and fl <= 250)
    p99 = med([lane_s(r)[0] for r in B2])
    mx = med([lane_s(r)[1] for r in B2])
    gate("P2", "lane-S service delay", "p99 <= 100 ms, max <= 1 s", f"p99 {r3(p99, 2)} ms, max {r3(mx, 2)} ms",
         p99 is not None and mx is not None and p99 <= 100 and mx <= 1000)
    sh = med([r["treeAdmission"]["shareOfWriterWall"] for r in B2])
    tp = med([r["treeAdmission"]["p99Ms"] for r in B2])
    gate("P2", "tree admission", "sum wall <= 3% of writer wall; per-batch p99 <= 50 ms", f"{r3(100 * sh, 2) if sh is not None else None}%; p99 {r3(tp, 2)} ms",
         sh is not None and tp is not None and sh <= 0.03 and tp <= 50)
    lk = [locked(r) for r in B2]
    nl = sum((x["stderrLockedLines"] or 0) + (x["readerLockedBodies"] or 0) for x in lk)
    gate("P2", "SQLITE_LOCKED (writer and reader)", "0", f"{nl}", nl == 0)
    cc = sorted({crossings(r) for r in B2})
    gate("P2", "cross_tree_crossings", "0", f"{cc}", cc == [0])
    rs = sorted({residual(r) for r in B2}, key=lambda v: (v is None, v))
    gate("P2", "budgetResidualBytes at every runtime close", "0", f"{rs}", rs == [0])
    dv = sorted({(r.get("writer") or {}).get("usageDisplacedVisible") for r in B2}, key=lambda v: (v is None, v))
    h24 = med([(r["milestones"].get("recent-ready:24h@newest") or {}).get("horizon_sessions") for r in B2])
    gate("P2", "usage_displaced_visible", "reported; U-1 trigger if > 1% of 24h-horizon sessions",
         f"{dv} (24h-horizon sessions {h24})", all(v is not None and h24 and v <= 0.01 * h24 for v in dv))
    return {"crB1": cr1, "crB2": cr2}


def eval_S(P, n_s):
    B2, L1, BE = P["sides"]["B2S"], P["sides"]["L1"], P["sides"]["BES"]
    allr = B2 + L1 + BE
    eq_gate("S", "logical equal across B2, L1 and BE-stress", allr, lambda r: tot(r, "logical"))
    c2, cl, cb = (med([r["CR"] for r in x]) for x in (B2, L1, BE))
    ratio_gate("S", "CR(B2) / CR(L1)", c2, cl, 1.00)
    ratio_gate("S", "CR(B2) / CR(BE-stress)", c2, cb, 0.97)
    m = lambda runs, n: med([ms(r, n) for r in runs])
    ratio_gate("S", "small-lane 7d@newest / L1", m(B2, "small-recent-ready:7d@newest"), m(L1, "small-recent-ready:7d@newest"), 0.60)
    ratio_gate("S", "small-lane 24h@newest / L1", m(B2, "small-recent-ready:24h@newest"), m(L1, "small-recent-ready:24h@newest"), 1.00)
    ratio_gate("S", "24h@newest / L1", m(B2, "recent-ready:24h@newest"), m(L1, "recent-ready:24h@newest"), 1.10)
    f = med([frac(r, "recent-ready:24h@newest", cl) for r in B2])
    gate("S", "24h@newest f (CR_ref = CR(L1))", "f <= 0.65", f"{r3(f, 4)}", f is not None and f <= 0.65)
    ratio_gate("S", "newest-10 / L1", m(B2, "newest-trees-ready"), m(L1, "newest-trees-ready"), 1.10)
    ratio_gate("S", "newest giant / L1", m(B2, "newest-giant-ready"), m(L1, "newest-giant-ready"), 1.25)
    f = med([frac(r, "newest-giant-ready", cl) for r in B2])
    gate("S", "newest giant f (CR_ref = CR(L1))", "f <= 0.20", f"{r3(f, 4)}", f is not None and f <= 0.20)
    fr, tm = m(B2, "first-readable"), med([r["T_meta"] for r in B2])
    gate("S", "first-readable", "<= T_meta + 2.0 s", f"{fr} s (T_meta {tm})", fr is not None and tm is not None and fr <= tm + 2.0)
    a = med([(r.get("writer") or {}).get("longestApplyTxMs") for r in B2])
    fl = med([(r.get("writer") or {}).get("longestFlipTxMs") for r in B2])
    gate("S", "longest apply tx; longest flip tx", "<= 250 ms each", f"{r3(a, 1)} ms; {r3(fl, 1)} ms",
         a is not None and fl is not None and a <= 250 and fl <= 250)
    p99, mx = med([lane_s(r)[0] for r in B2]), med([lane_s(r)[1] for r in B2])
    gate("S", "lane-S service delay", "p99 <= 100 ms, max <= 1 s", f"p99 {r3(p99, 2)} ms, max {r3(mx, 2)} ms",
         p99 is not None and mx is not None and p99 <= 100 and mx <= 1000)
    cc, rs = sorted({crossings(r) for r in B2}), sorted({residual(r) for r in B2}, key=lambda v: (v is None, v))
    gate("S", "cross_tree_crossings; budgetResidualBytes", "0; 0", f"{cc}; {rs}", cc == [0] and rs == [0])
    pk = lambda runs: med([r["peakFootprintBytes"] for r in runs])
    x1, x2 = pk(B2) / pk(L1), pk(B2) / pk(BE)
    gate("S", "peak footprint", "<= 1.03x L1 and <= 1.03x BE-stress", f"{x1:.4f}x L1; {x2:.4f}x BE-stress", x1 <= 1.03 and x2 <= 1.03)
    return {"crB2S": c2, "crL1": cl, "crBES": cb, "N_s": n_s}


def eval_P3(P, label="P3"):
    L, N = P["sides"]["B2L"], P["sides"]["B2N"]
    ratio_gate(label, "CR with load / CR without", med([r["CR"] for r in L]), med([r["CR"] for r in N]), 1.03)
    tots = [((r.get("reader") or {}).get("totals") or {}) for r in L]
    h5, h5o, h5n = (sum(t.get(k) or 0 for t in tots) for k in ("http5xx", "http5xxOther", "http5xxNotReady"))
    gate(label, "HTTP 5xx", "0", f"{h5} (not_ready 503 {h5n}; other {h5o}) over {sum(t.get('requests') or 0 for t in tots)} requests",
         h5 == 0, None if h5 == 0 else "every 5xx counted as written; not_ready 503 is the API's designed still-importing answer")
    lk = [locked(r) for r in L]
    nl = sum((x["stderrLockedLines"] or 0) + (x["readerLockedBodies"] or 0) + (x["readerLockedRefusals"] or 0) for x in lk)
    nr = sum((x["readerLockedRetries"] or 0) for x in lk)
    gate(label, "SQLITE_LOCKED; reader_locked_retries", "0; 0", f"{nl}; {nr}", nl == 0 and nr == 0)

    def win(r, window, key):
        return (((r.get("sharedCacheLocks") or {}).get(window) or {}).get(key) or {})
    pw99 = med([win(r, "streaming", "readerPrologueWait").get("p99Ms") for r in L])
    pwmax = med([win(r, "streaming", "readerPrologueWait").get("maxMs") for r in L])
    pwn = med([win(r, "streaming", "readerPrologueWait").get("count") for r in L])
    gate(label, "reader prologue wait, streaming window", "p99 <= 500 ms, max <= 2 s",
         f"p99 {r3(pw99, 2)} ms, max {r3(pwmax, 2)} ms (count {pwn})", pw99 is not None and pwmax is not None and pw99 <= 500 and pwmax <= 2000)
    cw99 = med([win(r, "streaming", "readerClosureWall").get("p99Ms") for r in L])
    gate(label, "reader closure wall, streaming window", "p99 <= 1 s", f"p99 {r3(cw99, 2)} ms", cw99 is not None and cw99 <= 1000)
    rep = {"finalizeWindow": {k: {m: med([win(r, "finalize", k).get(m) for r in L]) for m in ("count", "p50Ms", "p99Ms", "maxMs")}
                              for k in ("readerPrologueWait", "readerClosureWall", "sessionLockWait")},
           "sessionLockWaitStreaming": {m: med([win(r, "streaming", "sessionLockWait").get(m) for r in L]) for m in ("count", "p50Ms", "p99Ms", "maxMs")},
           "readerTotals": {k: sum(t.get(k) or 0 for t in tots) for k in ("requests", "http5xx", "http5xxNotReady", "http5xxOther",
                                                                          "transportErrors", "lockedBodies", "notReadyBodies", "non2xx")},
           "readerErrorCodes": {}}
    for t in tots:
        for k, v in (t.get("errorCodes") or {}).items():
            rep["readerErrorCodes"][k] = rep["readerErrorCodes"].get(k, 0) + v
    return rep


def eval_X(P):
    B1, BX = P["sides"]["B1"], P["sides"]["B2X"]
    both = B1 + BX
    eq_gate("X", "ledger source digest equal (B2 runtime admission = B1)", both, lambda r: (r.get("ledger") or {}).get("sourceDigest"))
    eq_gate("X", "sourceSetDigest equal", both, lambda r: (r.get("ledger") or {}).get("sourceSetDigest"))
    eq_gate("X", "logical equal", both, lambda r: tot(r, "logical"))
    eq_gate("X", "strict equal", both, lambda r: tot(r, "strict"))
    return {}


def eval_R(P2):
    rows = [r for r in P2["sides"]["B2"] if r.get("rebuild")]
    out = []
    for r in rows:
        rb = r["rebuild"]
        out.append({"run": r["run"], "exit": (rb.get("run") or {}).get("exit"), "wallS": r3((rb.get("run") or {}).get("rebuildWallS"), 2),
                    "logicalEqual": rb["logicalEqual"], "countsEqual": rb["countsEqual"],
                    "strictNoidTotalEqual": rb["strictNoidTotalEqual"], "strictNoidTotalREqual": rb["strictNoidTotalREqual"],
                    "strictRawEqual": rb["strictEqual"], "surrogateEqual": rb["surrogateEqual"],
                    "differingComponents": rb["differingComponents"],
                    "rebuildCursorsInPathOrder": {a: v["inPathOrder"] for a, v in ((r.get("rebuildCursorOrder") or {}).get("agents") or {}).items()}})
    n = len(out)
    gate("R", "rebuilds run (3 of P2's B2 ledgers)", "3", f"{n}", n >= 3)
    gate("R", "logical equal (rebuild = fresh)", "equal", f"{sum(1 for o in out if o['logicalEqual'])}/{n}", n >= 3 and all(o["logicalEqual"] for o in out))
    gate("R", "strict equal without surrogate ids (strict_noid TOTAL)", "equal", f"{sum(1 for o in out if o['strictNoidTotalEqual'])}/{n}",
         n >= 3 and all(o["strictNoidTotalEqual"] for o in out),
         "U-2 parent decision: cursor and message ids dropped; TOTAL includes ingest_cursor_file_evidence")
    gate("R", "strict_noid TOTAL_R (all but checkpoint file evidence) [diagnostic]", "equal",
         f"{sum(1 for o in out if o['strictNoidTotalREqual'])}/{n}", n >= 3 and all(o["strictNoidTotalREqual"] for o in out))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("pairs_dir")
    ap.add_argument("out")
    ap.add_argument("--predictions")
    ap.add_argument("--ns", type=int)
    ap.add_argument("--ext", action="append", default=[], help="NAME=EXT: merge pair EXT into NAME (11-per-side extension)")
    a = ap.parse_args()
    ext = {}
    for e in a.ext:
        k, v = e.split("=", 1)
        ext.setdefault(k, []).append(v)
    get = lambda n: merged(a.pairs_dir, [n] + ext.get(n, []))
    res = {"pairs": {}, "iqr": []}
    P0, P1, P2, S, P3, P3s, X = (get(n) for n in ("P0", "P1", "P2", "S", "P3", "P3-stress", "X"))
    E6d, E6s = get("E6-default"), get("E6-stress")
    cr_be = fr_be = None
    if P0:
        res["pairs"]["P0"] = {"extra": eval_P0(P0)}
    if P1:
        res["pairs"]["P1"] = {"extra": eval_P1(P1)}
        cr_be = med([r["CR"] for r in P1["sides"]["BE"]])
        fr_be = med([ms(r, "first-readable") for r in P1["sides"]["BE"]])
    elif P0:
        cr_be = med([r["CR"] for r in P0["sides"]["BE"]])
        fr_be = med([ms(r, "first-readable") for r in P0["sides"]["BE"]])
    e6_walk = []
    for E in (E6d,):
        if E:
            for tag in ("W0", "WNS"):
                e6_walk += E["sides"].get(tag, [])
    if P2:
        res["pairs"]["P2"] = {"extra": eval_P2(P2, cr_be, fr_be, e6_walk)}
        res["pairs"]["R"] = {"rebuilds": eval_R(P2)}
    if S:
        res["pairs"]["S"] = {"extra": eval_S(S, a.ns)}
    if P3:
        res["pairs"]["P3"] = {"extra": eval_P3(P3)}
    if P3s:
        res["pairs"]["P3-stress"] = {"extra": eval_P3(P3s, "P3-stress (reported)")}
        for g in GATES:
            if g["pair"] == "P3-stress (reported)":
                g["reportedOnly"] = True
    if X:
        res["pairs"]["X"] = {"extra": eval_X(X)}
    refs = {"P0": None, "P1": None, "P2": cr_be, "X": None, "P3": None, "P3-stress": None}
    for name, P in (("P0", P0), ("P1", P1), ("P2", P2), ("S", S), ("P3", P3), ("P3-stress", P3s), ("X", X), ("E6-default", E6d), ("E6-stress", E6s)):
        if not P:
            continue
        ref = refs.get(name)
        if name == "S":
            ref = med([r["CR"] for r in P["sides"]["L1"]])
        if name == "E6-default":
            ref = cr_be
        if name == "E6-stress" and S:
            ref = med([r["CR"] for r in S["sides"]["BES"]])
        res["pairs"].setdefault(name, {})
        res["pairs"][name]["sides"] = {tag: side_stats(runs, ref) for tag, runs in P["sides"].items()}
        res["pairs"][name]["pairMeta"] = [{k: m.get(k) for k in ("format", "name", "rounds", "order", "sides", "snapshot", "machine",
                                                                 "harnessSha256", "env", "startedAt")} for m in P.get("metas", [P["meta"]])]
        for m in res["pairs"][name]["pairMeta"]:
            m["env"] = {k: v for k, v in (m.get("env") or {}).items() if k != "VERIFY_BIN"}
        if name.startswith("E6"):
            res["pairs"][name]["attribution"] = {tag: {"laneProfile": lanes_profile(runs),
                                                       "parkedShare": r3(med([parked_share(r) for r in runs]), 4)}
                                                 for tag, runs in P["sides"].items()}
            allr = [r for runs in P["sides"].values() for r in runs]
            n, _ = distinct(allr, lambda r: tot(r, "logical"))
            gate(name + " (diagnostic)", "all rows logically equal", "equal", f"{n} distinct over {len(allr)} runs", n == 1)
            for g in GATES[-1:]:
                g["reportedOnly"] = True
        for tag, runs in P["sides"].items():
            res["iqr"].append({"pair": name, **iqr_note(name, tag, runs)})
    if a.predictions and os.path.exists(a.predictions):
        res["predictions"] = compare_predictions(json.load(open(a.predictions)), P2, S, cr_be)
    res["gates"] = GATES
    res["problems"] = PROBLEMS
    with open(a.out, "w") as fh:
        json.dump(res, fh, indent=1, sort_keys=False)
        fh.write("\n")
    w = max(len(g["gate"]) for g in GATES) if GATES else 10
    for g in GATES:
        print(f'{g["pair"]:<24} {"PASS" if g["pass"] else "FAIL"}{"*" if g.get("reportedOnly") else " "} {g["gate"]:<{w}} | {g["threshold"]} | {g["measured"]}')
    for i in res["iqr"]:
        if i["over3pct"]:
            print("IQR > 3%:", json.dumps(i))


def compare_predictions(pred, P2, S, cr_be):
    out = []
    pp, sp = pred["predictions"]["P2"], pred["predictions"]["S"]

    def add(pair, key, measured):
        p = (pp if pair == "P2" else sp).get(key)
        if p is None or measured is None:
            out.append({"pair": pair, "metric": key, "predicted": p, "measured": measured, "modelError": None})
            return
        dev = (measured / p - 1) if p else None
        out.append({"pair": pair, "metric": key, "predicted": p, "measured": r3(measured, 4), "relDev": r3(dev, 4),
                    "modelError": dev is not None and abs(dev) > 0.25})
    m = lambda runs, n: med([ms(r, n) for r in runs])
    if P2:
        B1, B2 = P2["sides"]["B1"], P2["sides"]["B2"]
        add("P2", "canonicalReady_B2_over_B1", med([r["CR"] for r in B2]) / med([r["CR"] for r in B1]))
        add("P2", "peakProxy_B2_over_B1", med([r["peakFootprintBytes"] for r in B2]) / med([r["peakFootprintBytes"] for r in B1]))
        add("P2", "firstReadable_minus_Tmeta_s", med([ms(r, "first-readable") - r["T_meta"] for r in B2]))
        for key, n in (("f_24hAtNewest", "recent-ready:24h@newest"), ("f_newest10", "newest-trees-ready"), ("f_newestGiant", "newest-giant-ready"),
                       ("f_small24hAtNewest", "small-recent-ready:24h@newest"), ("f_small7dAtNewest", "small-recent-ready:7d@newest"),
                       ("f_7dAtNewest", "recent-ready:7d@newest")):
            add("P2", key, med([frac(r, n, cr_be) for r in B2]))
        add("P2", "longestApplyTx_s", med([(r.get("writer") or {}).get("longestApplyTxMs") for r in B2]) / 1000)
        add("P2", "longestFlipTx_s", med([(r.get("writer") or {}).get("longestFlipTxMs") for r in B2]) / 1000)
        add("P2", "writerBusyShareOfCapture", med([(r.get("writer") or {}).get("busyShareOfCapture") for r in B2]))
    if S:
        B2, L1, BE = S["sides"]["B2S"], S["sides"]["L1"], S["sides"]["BES"]
        c = lambda runs: med([r["CR"] for r in runs])
        add("S", "canonicalReady_B2_over_L1", c(B2) / c(L1))
        add("S", "canonicalReady_B2_over_BEstress", c(B2) / c(BE))
        for key, n in (("small7dAtNewest_B2_over_L1", "small-recent-ready:7d@newest"), ("small24hAtNewest_B2_over_L1", "small-recent-ready:24h@newest"),
                       ("24hAtNewest_B2_over_L1", "recent-ready:24h@newest"), ("newest10_B2_over_L1", "newest-trees-ready"),
                       ("newestGiant_B2_over_L1", "newest-giant-ready")):
            add("S", key, m(B2, n) / m(L1, n))
        add("S", "f_24hAtNewest_refL1", med([frac(r, "recent-ready:24h@newest", c(L1)) for r in B2]))
        add("S", "f_newestGiant_refL1", med([frac(r, "newest-giant-ready", c(L1)) for r in B2]))
        add("S", "firstReadable_minus_Tmeta_s", med([ms(r, "first-readable") - r["T_meta"] for r in B2]))
        add("S", "longestApplyTx_s", med([(r.get("writer") or {}).get("longestApplyTxMs") for r in B2]) / 1000)
        add("S", "longestFlipTx_s", med([(r.get("writer") or {}).get("longestFlipTxMs") for r in B2]) / 1000)
        add("S", "peakProxy_B2_over_L1", med([r["peakFootprintBytes"] for r in B2]) / med([r["peakFootprintBytes"] for r in L1]))
        add("S", "peakProxy_B2_over_BEstress", med([r["peakFootprintBytes"] for r in B2]) / med([r["peakFootprintBytes"] for r in BE]))
    return out


if __name__ == "__main__":
    sys.exit(main())
