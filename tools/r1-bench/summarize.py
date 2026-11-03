#!/usr/bin/env python3
"""R1 run summaries (brief v3 §12 E3): sizes/counts/timings/digests only.

usage: python3 -I summarize.py --out OUT_JSON [--cr-ref SECONDS | --ref RUN_DIR ...] RUN_DIR...

Per run (RUN_DIR holds report.json from run_r1.js, plus the optional
r1_logical_digest.out, ledger_verify.json, ranked.json and verify_snapshot.json
pair_r1.sh writes beside it):
  - every `milestone:` line (keyed name[:horizon|:agent]) and T_meta (= metadata-ready);
  - CR (= canonical-ready), peak footprint, wall, CPU, load/swap stamps;
  - `walk:` lines, `census:` lines, `usage-copies:`, tolerated-imperfection lines;
  - the per-lane profile (parallel-stream-prepare lanes): service delay
    p50/p99/max, heldMs p99, inlineMs, peakUnemittedHeadBytes, head waits;
  - the writer profile (streamed-writer): longestApplyTxMs, maxApplyTxBytes,
    longestFlipTxMs, maxFlipTailBytes, resolveMs, budgetResidualBytes, busy share,
    per-(runtime, lane) txs and queue waits;
  - tree-admission per batch (p50/p99/max) and totals, with renumberRows;
  - shared-cache-locks per window (p50/p99/max; retries, refusals, violations);
  - capture-pool stats including peakHeadBytesTotal;
  - crossTreeCrossings, lateSamples, peakPendingWithheldBytes, bigSourceSplits;
  - fractions f(m) = (m - T_meta) / (CR_ref - T_meta) for the recency
    milestones, CR_ref = --cr-ref, else the median CR of the --ref runs, else
    the run's own CR (reported as reference "self");
  - the 7d@newest structural check t(7d@newest) <= CR - (finalize + publication
    wall) + 1 s, finalize + publication wall = the final profile's
    "bulk finalization time" + "publication time";
  - digest sections (logical/strict/strict_noid TOTAL, counts), ledger digests,
    ranked digest, reader-load summary.
Across runs: medians and IQR (as % of median) of the headline numbers, and
which digests are equal across all runs.
"""
import argparse
import json
import os
import re
import statistics
import sys

PROFILE_KEYS = [
    "normalize time", "capture step time", "sequencer head wait time", "writer budget wait time",
    "read time", "apply time", "canonical apply time", "ledger write time", "ledger compression time",
    "bulk finalization time", "publication time", "records framed", "lines parsed", "ops emitted",
    "batches applied", "bytes read", "payload bytes processed", "usage identities owned",
    "usage copies dropped", "usage owners displaced", "folded tool-row usage records unclaimed",
    "ledger frames", "ledger frame bytes", "ledger disk bytes",
]
RECENCY = [
    "first-readable", "recent-ready:24h@newest", "recent-ready:7d@newest", "newest-trees-ready",
    "newest-giant-ready", "small-recent-ready:24h@newest", "small-recent-ready:7d@newest",
    "recent-ready:24h", "recent-ready:7d", "first-session-complete",
]
KV = re.compile(r"(\w+)=(\S+)")


def num(v):
    try:
        return int(v)
    except ValueError:
        try:
            return float(v)
        except ValueError:
            return v


def kv(text):
    return {k: num(v) for k, v in KV.findall(text)}


def pct(values, p):
    v = sorted(x for x in values if x is not None)
    if not v:
        return None
    import math
    return v[max(1, math.ceil(p / 100 * len(v))) - 1]


def milestones(text_lines):
    out = {}
    for t in text_lines:
        line = t["line"]
        if not line.startswith("milestone:"):
            continue
        m = re.match(r"milestone: (\S+)(.*)", line)
        name, rest = m.group(1), m.group(2).strip()
        fields = kv(rest)
        if name in ("recent-ready", "small-recent-ready"):
            name += ":" + str(fields.get("horizon"))
        elif name in ("runtime-readable", "runtime-metadata-public"):
            name += ":" + str(fields.get("agent"))
        fields.pop("runtime", None)
        fields["atMs"] = t["ms"]
        out[name] = fields
    return out


def lines_with(text_lines, prefix):
    return [t["line"] for t in text_lines if t["line"].startswith(prefix)]


def digest_file(path):
    if not os.path.exists(path):
        return None
    out = {"logical": {}, "strict": {}, "strict_noid": {}, "surrogate": {}, "aux": {}, "count": {}}
    for line in open(path):
        parts = line.rstrip("\n").split("|")
        if len(parts) == 4 and parts[0] in out:
            out[parts[0]][parts[1]] = {"rows": int(parts[2]), "digest": parts[3]}
        elif len(parts) == 3 and parts[0] == "count":
            out["count"][parts[1]] = int(parts[2])
    return out


def cursor_order_file(path):
    """r1_cursor_order.sql output (Task C diagnostic): per-agent id ranges and strict_local."""
    if not os.path.exists(path):
        return None
    out = {"agents": {}, "strict_local": {}}
    for line in open(path):
        parts = line.rstrip("\n").split("|")
        if parts[0] == "cursor_order" and len(parts) == 6:
            out["agents"][parts[1]] = {"cursors": int(parts[2]), "minId": int(parts[3]), "maxId": int(parts[4]),
                                       "inPathOrder": int(parts[5])}
        elif parts[0] == "strict_local" and len(parts) == 4:
            out["strict_local"][parts[1]] = {"rows": int(parts[2]), "digest": parts[3]}
    return out


def load_json(path):
    return json.load(open(path)) if os.path.exists(path) else None


def stage(report, name, agent=None):
    return [s for s in report["stages"] if s.get("stage") == name and (agent is None or s.get("agent") == agent)]


def lane_view(lane):
    def d(key):
        v = lane.get(key) or {}
        return {"count": v.get("count"), "p50Ms": v.get("p50Ms"), "p99Ms": v.get("p99Ms"), "maxMs": v.get("maxMs")}
    return {
        "lane": lane.get("lane"), "units": lane.get("units"), "sources": lane.get("sources"), "bytes": lane.get("bytes"),
        "headWaitMs": lane.get("headWaitMs"), "heldMs": d("heldMs"), "endMs": lane.get("endMs"),
        "inlineMs": lane.get("inlineMs"), "serviceDelayMs": d("serviceDelayMs"),
        "peakUnemittedHeadBytes": lane.get("peakUnemittedHeadBytes"),
    }


def summarize(run_dir):
    report = json.load(open(os.path.join(run_dir, "report.json")))
    ms = milestones(report["textLines"])
    t_meta = ms.get("metadata-ready", {}).get("elapsed_s")
    cr = ms.get("canonical-ready", {}).get("elapsed_s")
    final = report["profiles"][-1]["metrics"] if report["profiles"] else {}
    out = {
        "run": os.path.basename(run_dir.rstrip("/")),
        "label": report.get("label"),
        "startedAt": report["startedAt"],
        "binarySha256": report["binarySha256"],
        "walkInputsDigest": report["snapshot"]["walkInputsDigest"],
        "extraFlags": report.get("extraFlags", []),
        "exitCode": report["exitCode"],
        "machineBefore": report["machineBefore"],
        "machineAfter": report["machineAfter"],
        "swapGrewMb": None,
        "processWallMs": report["processWallMs"],
        "userCpuS": report["time"].get("userS"),
        "sysCpuS": report["time"].get("sysS"),
        "maxRssBytes": report["time"].get("maximum resident set size"),
        "peakFootprintBytes": report["time"].get("peak memory footprint"),
        "T_meta": t_meta,
        "CR": cr,
        "milestones": ms,
        "walk": {},
        "census": {},
        "usageCopies": None,
        "tolerated": [],
        "profile": {k: final[k]["value"] for k in PROFILE_KEYS if k in final},
        "outputs": report["outputs"],
    }
    b, a = report["machineBefore"].get("swapUsedMb"), report["machineAfter"].get("swapUsedMb")
    if b is not None and a is not None:
        out["swapGrewMb"] = round(a - b, 2)
    for line in lines_with(report["textLines"], "walk: "):
        f = kv(line)
        f.pop("runtime", None)
        out["walk"][str(f.get("agent"))] = f
    for line in lines_with(report["textLines"], "census: "):
        f = kv(line)
        f.pop("runtime", None)
        out["census"][str(f.get("agent"))] = f
    uc = lines_with(report["textLines"], "usage-copies: ")
    out["usageCopies"] = kv(uc[-1]) if uc else None
    for line in report["textLines"]:
        m = re.match(r"(\S+): tolerated per-file imperfections \((.*?)\)", line["line"])
        if m:
            out["tolerated"].append({"agent": m.group(1), **kv(m.group(2).replace(",", " "))})

    # Producer side, per runtime.
    prep = {}
    for s in stage(report, "parallel-stream-prepare"):
        prep[s.get("agent")] = {
            "wallMs": s.get("wallMs"), "captureWallMs": s.get("captureWallMs"), "cpuMs": s.get("cpuMs"),
            "resolvedSessions": s.get("resolvedSessions"), "resolvedToolRows": s.get("resolvedToolRows"),
            "crossedSessions": s.get("crossedSessions"), "treesClosed": s.get("treesClosed"),
            "treesBound": s.get("treesBound"), "treesTainted": s.get("treesTainted"),
            "crossTreeCrossings": s.get("crossTreeCrossings"), "lateSamples": s.get("lateSamples"),
            "peakPendingWithheldBytes": s.get("peakPendingWithheldBytes"),
            "bigSourceSplits": s.get("bigSourceSplits"),
            "laneSwitches": s.get("laneSwitches"), "bigLaneChainMs": s.get("bigLaneChainMs"),
            "lanes": [lane_view(l) for l in (s.get("lanes") or [])],
        }
    out["prepare"] = prep
    out["streamPrepared"] = {e.get("agent"): {k: e.get(k) for k in ("records", "parsed", "unknown", "hardErrors", "oversized",
                                                                      "quarantined", "filesMissing", "retriesExhausted",
                                                                      "conflicts", "filesIncomplete", "captureWallMs")}
                             for e in report["otherEvents"] if e.get("event") == "stream-prepared"}

    # Writer.
    w = stage(report, "streamed-writer")
    if w:
        w = w[-1]
        out["writer"] = {k: w.get(k) for k in (
            "wallMs", "idleMs", "cpuMs", "applyCalls", "longestApplyTxMs", "maxApplyTxBytes", "busyShareOfCapture",
            "resolveMs", "budgetResidualBytes", "flipBatches", "flipTrees", "flipSessions", "flipWallMs",
            "longestFlipTxMs", "treeAdmissionP99Ms", "maxFlipTailBytes", "flipRenumberRows", "flipLockWaitMs",
            "usageIdentitiesOwned", "usageCopiesDropped", "usageCopiesDisplaced", "usageIdentitiesUnclaimed",
            "usageDisplacedVisible", "usageOwnerBytes", "lanes")}
        out["writer"]["sharedCacheLocksStreaming"] = w.get("sharedCacheLocks")
    else:
        out["writer"] = None

    ta = stage(report, "tree-admission")
    walls = [s.get("wallMs") for s in ta]
    writer_wall = (out["writer"] or {}).get("wallMs")
    out["treeAdmission"] = {
        "batches": len(ta), "trees": sum(s.get("trees", 0) for s in ta), "sessions": sum(s.get("sessions", 0) for s in ta),
        "tailBytes": sum(s.get("tailBytes", 0) for s in ta), "renumberRows": sum(s.get("renumberRows", 0) for s in ta),
        "wallMsTotal": round(sum(walls), 3) if walls else 0, "p50Ms": pct(walls, 50), "p99Ms": pct(walls, 99),
        "maxMs": max(walls) if walls else None, "lockWaitMsTotal": round(sum(s.get("lockWaitMs", 0) for s in ta), 3),
        "shareOfWriterWall": (round(sum(walls) / writer_wall, 5) if walls and writer_wall else None),
    }
    locks = stage(report, "shared-cache-locks")
    out["sharedCacheLocks"] = locks[-1].get("locks") if locks else None
    pool = stage(report, "capture-pool")
    out["capturePool"] = {k: v for k, v in pool[-1].items() if k not in ("command", "stage", "agent", "metrics", "__ms")} if pool else None
    out["stages"] = {f'{s.get("command")}:{s.get("stage")}' + (f':{s.get("agent")}' if s.get("agent") else ""): s.get("wallMs")
                     for s in report["stages"] if s.get("stage") != "tree-admission"}
    walk_plans = [e for e in report["otherEvents"] if e.get("event") == "walk-plan"]
    out["walkPlan"] = {e.get("agent"): {"mode": e.get("mode"), "digest": e.get("digest"), "sources": len(e.get("sources") or [])}
                       for e in walk_plans}

    # Fractions and the 7d@newest structural check (computed in main, once CR_ref is known).
    out["digests"] = digest_file(os.path.join(run_dir, "r1_logical_digest.out"))
    out["cursorOrder"] = cursor_order_file(os.path.join(run_dir, "r1_cursor_order.out"))
    out["rebuildCursorOrder"] = cursor_order_file(os.path.join(run_dir, "rebuild_cursor_order.out"))
    lv = load_json(os.path.join(run_dir, "ledger_verify.json"))
    out["ledger"] = None if lv is None else {
        "sourceDigest": lv.get("sourceDigest"), "sourceSetDigest": lv.get("sourceSetDigest"),
        "entryDigest": lv.get("entryDigest"), "metaDigest": lv.get("metaDigest"), "frameDigest": lv.get("frameDigest"),
        "runtimes": [{k: r.get(k) for k in ("runtime", "sourceDigest", "sourceSetDigest", "entries", "rawRecords",
                                              "documents", "seals", "metas", "tornSegments")} for r in lv.get("runtimes", [])],
        "total": lv.get("total"),
    }
    reb = digest_file(os.path.join(run_dir, "rebuild_digest.out"))
    if reb is None:
        out["rebuild"] = None
    else:
        fresh = out["digests"] or {}
        tot = lambda d, sec, k="TOTAL": ((d.get(sec) or {}).get(k) or {}).get("digest")
        out["rebuild"] = {
            "run": load_json(os.path.join(run_dir, "rebuild.json")),
            "logicalEqual": tot(reb, "logical") == tot(fresh, "logical"),
            "countsEqual": reb.get("count") == fresh.get("count"),
            "strictNoidTotalREqual": tot(reb, "strict_noid", "TOTAL_R") == tot(fresh, "strict_noid", "TOTAL_R"),
            "strictNoidTotalEqual": tot(reb, "strict_noid") == tot(fresh, "strict_noid"),
            "strictEqual": tot(reb, "strict") == tot(fresh, "strict"),
            "surrogateEqual": reb.get("surrogate") == fresh.get("surrogate"),
            "differingComponents": sorted(f"{sec}:{c}" for sec in ("logical", "strict", "strict_noid", "surrogate")
                                          for c in set((reb.get(sec) or {})) | set((fresh.get(sec) or {}))
                                          if (reb.get(sec) or {}).get(c) != (fresh.get(sec) or {}).get(c)),
        }
    out["ranked"] = load_json(os.path.join(run_dir, "ranked.json"))
    out["snapshotVerify"] = load_json(os.path.join(run_dir, "verify_snapshot.json"))
    rd = report.get("reader")
    out["reader"] = None if rd is None else {"totals": rd.get("totals"), "sessions": rd.get("sessions"),
                                             "websockets": rd.get("websockets"), "routes": rd.get("routes"),
                                             "phases": rd.get("phases")}
    out["phaseAt"] = report.get("phaseAt")
    out["stderrLockedLines"] = report.get("stderrLockedLines")
    out["stderrPanicLines"] = report.get("stderrPanicLines")
    out["guard"] = load_json(os.path.join(run_dir, "guard.json"))
    return out


def fractions(run, cr_ref, ref_name):
    t_meta = run["T_meta"]
    out = {"crRef": cr_ref, "crRefSource": ref_name, "f": {}}
    if t_meta is None or cr_ref is None or cr_ref <= t_meta:
        return out
    for name in RECENCY:
        m = run["milestones"].get(name, {}).get("elapsed_s")
        out["f"][name] = None if m is None else round((m - t_meta) / (cr_ref - t_meta), 4)
    return out


def structural_7d(run):
    m = run["milestones"].get("recent-ready:7d@newest", {}).get("elapsed_s")
    fin = run["profile"].get("bulk finalization time")
    pub = run["profile"].get("publication time")
    if m is None or run["CR"] is None or fin is None or pub is None:
        return {"ok": None, "reason": "missing 7d@newest, CR, or finalize/publication metrics"}
    bound = run["CR"] - (fin + pub) / 1000.0 + 1.0
    return {"ok": m <= bound, "t7dNewest": m, "bound": round(bound, 3), "finalizePublicationWallS": round((fin + pub) / 1000.0, 3)}


def median_iqr(values):
    v = [x for x in values if isinstance(x, (int, float))]
    if not v:
        return None
    med = statistics.median(v)
    if len(v) >= 4:
        q = statistics.quantiles(v, n=4, method="inclusive")
        iqr = q[2] - q[0]
    else:
        iqr = max(v) - min(v)
    return {"n": len(v), "median": med, "iqr": iqr, "iqrPctOfMedian": round(100 * iqr / med, 2) if med else None,
            "min": min(v), "max": max(v)}


def equal_across(runs, getter):
    vals = [getter(r) for r in runs]
    vals = [v for v in vals if v is not None]
    if not vals:
        return None
    return {"equal": len(set(json.dumps(v, sort_keys=True) for v in vals)) == 1, "n": len(vals),
            "distinct": len(set(json.dumps(v, sort_keys=True) for v in vals))}


def group(runs):
    g = {
        "runs": [r["run"] for r in runs],
        "CR": median_iqr([r["CR"] for r in runs]),
        "T_meta": median_iqr([r["T_meta"] for r in runs]),
        "processWallMs": median_iqr([r["processWallMs"] for r in runs]),
        "peakFootprintBytes": median_iqr([r["peakFootprintBytes"] for r in runs]),
        "milestones": {n: median_iqr([r["milestones"].get(n, {}).get("elapsed_s") for r in runs]) for n in RECENCY},
        "fractions": {n: median_iqr([r["fractions"]["f"].get(n) for r in runs]) for n in RECENCY},
        "load1Before": median_iqr([r["machineBefore"].get("load1") for r in runs]),
        "swapGrewMb": [r["swapGrewMb"] for r in runs],
        "equal": {
            "logical": equal_across(runs, lambda r: (r["digests"] or {}).get("logical", {}).get("TOTAL")),
            "strict": equal_across(runs, lambda r: (r["digests"] or {}).get("strict", {}).get("TOTAL")),
            "strict_noid": equal_across(runs, lambda r: (r["digests"] or {}).get("strict_noid", {}).get("TOTAL")),
            "strict_noid_R": equal_across(runs, lambda r: (r["digests"] or {}).get("strict_noid", {}).get("TOTAL_R")),
            "strict_local": equal_across(runs, lambda r: ((r.get("cursorOrder") or {}).get("strict_local") or {}).get("TOTAL")),
            "cursorBases": equal_across(runs, lambda r: {a: v["minId"] for a, v in ((r.get("cursorOrder") or {}).get("agents") or {}).items()} or None),
            "surrogate": equal_across(runs, lambda r: (r["digests"] or {}).get("surrogate") or None),
            "counts": equal_across(runs, lambda r: (r["digests"] or {}).get("count") or None),
            "sourceDigest": equal_across(runs, lambda r: (r["ledger"] or {}).get("sourceDigest")),
            "sourceSetDigest": equal_across(runs, lambda r: (r["ledger"] or {}).get("sourceSetDigest")),
            "walkDigest": equal_across(runs, lambda r: {a: w.get("digest") for a, w in r["walk"].items()} or None),
            "ranked": equal_across(runs, lambda r: (r["ranked"] or {}).get("ranked_ids_sha256")),
            "assets": equal_across(runs, lambda r: [r["outputs"]["assetsDirFiles"], r["outputs"]["assetsDirBytes"]]),
            "walkInputsDigest": equal_across(runs, lambda r: r["walkInputsDigest"]),
        },
    }
    return g


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--cr-ref", type=float)
    ap.add_argument("--ref", nargs="*", default=[])
    ap.add_argument("runs", nargs="+")
    a = ap.parse_args()
    runs = [summarize(d) for d in a.runs]
    if a.cr_ref is not None:
        ref, ref_name = a.cr_ref, "--cr-ref"
    elif a.ref:
        refs = [summarize(d)["CR"] for d in a.ref]
        ref, ref_name = statistics.median([x for x in refs if x is not None]), "median CR of --ref runs"
    else:
        ref, ref_name = None, "self"
    for r in runs:
        r["fractions"] = fractions(r, ref if ref is not None else r["CR"], ref_name)
        r["structural7dNewest"] = structural_7d(r)
    out = {"format": "r1-summary/1", "crRef": ref, "crRefSource": ref_name, "runs": runs, "group": group(runs)}
    with open(a.out, "x") as fh:
        json.dump(out, fh, indent=2)
        fh.write("\n")

    cols = [r["run"][-18:] for r in runs]
    row = lambda k, vals: print("%-40s" % k, *("%18s" % (v if v is not None else "-") for v in vals))
    row("", cols)
    row("exit", [r["exitCode"] for r in runs])
    row("load1 before / swap grew MB", [f'{r["machineBefore"].get("load1")}/{r["swapGrewMb"]}' for r in runs])
    row("processWallMs", [r["processWallMs"] for r in runs])
    row("T_meta s", [r["T_meta"] for r in runs])
    row("CR s", [r["CR"] for r in runs])
    row("peak footprint MB", [round(r["peakFootprintBytes"] / 2**20) if r["peakFootprintBytes"] else None for r in runs])
    for n in RECENCY:
        row("ms:" + n, [r["milestones"].get(n, {}).get("elapsed_s") for r in runs])
    for n in RECENCY:
        row("f:" + n, [r["fractions"]["f"].get(n) for r in runs])
    row("7d@newest structural ok", [r["structural7dNewest"].get("ok") for r in runs])
    row("writer longestApplyTxMs", [(r["writer"] or {}).get("longestApplyTxMs") for r in runs])
    row("writer longestFlipTxMs", [(r["writer"] or {}).get("longestFlipTxMs") for r in runs])
    row("writer budgetResidual", [json.dumps((r["writer"] or {}).get("budgetResidualBytes")) for r in runs])
    row("tree-admission p99 ms / share", [f'{r["treeAdmission"]["p99Ms"]}/{r["treeAdmission"]["shareOfWriterWall"]}' for r in runs])
    row("crossTreeCrossings", [sum((p.get("crossTreeCrossings") or 0) for p in r["prepare"].values()) for r in runs])
    for k in ("logical", "strict", "strict_noid"):
        row(f"{k} TOTAL", [((r["digests"] or {}).get(k, {}).get("TOTAL") or {}).get("digest", "-")[:16] for r in runs])
    row("sourceDigest", [((r["ledger"] or {}).get("sourceDigest") or "-")[:16] for r in runs])
    row("sourceSetDigest", [((r["ledger"] or {}).get("sourceSetDigest") or "-")[:16] for r in runs])
    row("reader 5xx other/not_ready/locked", [None if not r["reader"] else
        f'{r["reader"]["totals"].get("http5xxOther")}/{r["reader"]["totals"].get("http5xxNotReady")}/{r["reader"]["totals"].get("lockedBodies")}'
        for r in runs])
    row("rebuild logical/strictR eq", [None if not r["rebuild"] else f'{r["rebuild"]["logicalEqual"]}/{r["rebuild"]["strictNoidTotalREqual"]}' for r in runs])
    row("ranked ids", [((r["ranked"] or {}).get("ranked_ids_sha256") or "-")[:16] for r in runs])
    for agent in sorted({a for r in runs for a in r["walk"]}):
        row(f"walk {agent}", [f'{r["walk"].get(agent, {}).get("order")}/{r["walk"].get(agent, {}).get("digest")}' for r in runs])
    for k, v in out["group"]["equal"].items():
        print("equal across runs:", k, json.dumps(v))


if __name__ == "__main__":
    sys.exit(main())
