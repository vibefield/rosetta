"""E1.8 pre-registration (brief v3 §12 E1 step 8): stress_m4 extended with what r1/v3sim already does.

Added here (by import; stress_m4.py, ../sim and ../v3sim stay byte-identical, so stress_results.json,
g_vs_l1.json and fair_heads.json remain reproducible):
  - the L1 scenario `b_chunk` = (b) + chunked apply, one 256 MiB budget (v3sim/g_vs_l1.py);
  - the fair-head pick (v3sim/fair_heads.py `FairHeads`, copied verbatim; that module runs at import);
  - the recent_7d and all_readable metrics (engine.report already returns them; reported here);
  - --structure (snapshot-structure.json, E1.8) replacing m4_population: the real walk plan's
    trees, lanes, read lengths and ages, one session per source;
  - --workers N_s, --r1 r1_M4, --w W_M4 (measured from BE's P0 runs, E4), replacing the hard-coded
    P in {3, 4}, r1 = 71 MB/s and W = 340 MB/s.
Every other per-operation cost of the MacBook calibration (c_open, seq_rate, c_event, c_close, c_doc,
c_tx, c_src, admit_runtime, remainder, fold, flip) is divided by the same speed factor
k = r1_M4 / 71 MB/s; T_META and finalize + publication are BE's measured medians (--t-meta, --t-fp).
The structure is the real corpus, so one deterministic run per configuration (no seeds).
Structure only; indicative, not a measurement.

usage: python3 -I stress_m4_v3.py --structure S.json --r1 B/s --w B/s --workers N_s --t-meta S --t-fp S
       [--agent claude-code] [--doc-max-bytes 4096] --out stress-prediction.json
"""
import argparse
import hashlib
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
SIM = os.path.join(HERE, "..", "sim")
sys.path.insert(0, SIM)
sys.path.insert(0, HERE)
import engine  # noqa: E402
import scenarios as S  # noqa: E402
import stress_m4  # noqa: E402,F401  (unchanged; imported so its module state matches the v3sim runs)
from population import Source, Tree  # noqa: E402
from constants import MIB, LARGE_BYTES  # noqa: E402
from common import fit_seq_rate, population as mac_population  # noqa: E402


class FairHeads(engine.Sim):  # verbatim from v3sim/fair_heads.py
    def pick(self):
        if not hasattr(self, "_grant"):
            self._grant = 0
        best = None
        for L in self.lanes:
            if not L.exempt:
                continue
            u = L.runnable_head()
            if u is None:
                continue
            key = (getattr(L, "_last_grant", -1), -L.remaining)
            if best is None or key < best[2]:
                best = (L, u, key)
        if best is not None:
            self._grant += 1
            best[0]._last_grant = self._grant
            return best[0], best[1]
        saved = [L.exempt for L in self.lanes]
        try:
            for L in self.lanes:
                L.exempt = False if L.runnable_head() is None else L.exempt
            return super().pick()
        finally:
            for L, e in zip(self.lanes, saved):
                L.exempt = e


SC = dict(S.SCENARIOS)
SC["b_chunk"] = dict(SC["b"], chunked=True)
M = ["t_complete", "recent_24h", "recent_7d", "all_readable", "newest10", "newest_giant", "s_24h", "s_7d",
     "first_readable", "max_apply_tx", "max_flip_tx", "peak_broad", "peak_narrow", "lane_s_writer_wait_max",
     "writer_busy", "capture_wall", "parked_share", "head_wait", "budget_wait"]
MAC_R1, MAC_C_OPEN = 71e6, 0.0076


def load_structure(path, agent, doc_max_bytes):
    """Sources and trees from snapshot-structure.json's walk plan (global order; within a tree, path order)."""
    st = json.load(open(path))
    plan = st["walkPlan"][agent]
    sources, trees = [], {}
    for path_rank, lane, tree, read_len, age_ms in plan["sources"]:
        sid = len(sources)
        kind = "doc" if (lane == "P" or read_len <= doc_max_bytes) else "jsonl"
        src = Source(sid, tree, sid, kind, float(read_len), age_ms / 1000.0)
        src.path_key = path_rank
        sources.append(src)
        t = trees.get(tree)
        if t is None:
            t = trees[tree] = Tree(tree, 0)
            t.uuid = tree
            t.project = 0
        src.within = len(t.sources)
        t.sources.append(sid)
        t.sessions.append(sid)
    for t in trees.values():
        t.bytes = sum(sources[i].size for i in t.sources)
        t.k_age = min(sources[i].age for i in t.sources)
        t.max_source = max(sources[i].size for i in t.sources)
        t.lane = "B" if t.max_source > LARGE_BYTES else "S"
    # Tree ids must index 0..n-1 for the engine's maps; remap densely, keeping order.
    order = sorted(trees)
    remap = {old: new for new, old in enumerate(order)}
    out_trees = []
    for old in order:
        t = trees[old]
        t.id = remap[old]
        out_trees.append(t)
    for s in sources:
        s.tree = remap[s.tree]
    return sources, out_trees, st


def params(r1, W, t_meta, t_fp):
    k = r1 / MAC_R1
    msrc, _ = mac_population(1)
    seq_mac = fit_seq_rate(msrc, dict(r1=MAC_R1, c_open=MAC_C_OPEN))
    d = engine.DEFAULT_PARAMS
    p = dict(r1=r1, W=W, c_open=MAC_C_OPEN / k, seq_rate=seq_mac * k,
             c_event=d["c_event"] / k, c_close=d["c_close"] / k, c_doc=d["c_doc"] / k,
             c_tx=d["c_tx"] / k, c_src=d["c_src"] / k,
             admit_runtime=3.0 * 1.70 / 21.25 / k, remainder=0.3 * 1.70 / 21.25 / k,
             fold_rate=d["fold_rate"] / k, c_flip=d["c_flip"] / k, c_flip_session=d["c_flip_session"] / k)
    glob = dict(T_META=t_meta, T_FINALIZE_PUBLISH=t_fp, POOL_EXTRA_IDLE=(139.75 - 134.3) / k)
    return p, glob, k, seq_mac


def run(sources, trees, sc_name, cls, G, P, p, glob, inflight=256 * MIB):
    engine.P = P
    engine.FLOOR = min(64 * MIB, G // 8)
    engine.IN_FLIGHT_BYTES = inflight
    for k, v in glob.items():
        setattr(engine, k, v)
    s = dict(SC[sc_name])
    s["G"] = G
    s["budgets"] = {"S": inflight // 4, "B": inflight - inflight // 4} if "budgets" in s else {"all": inflight}
    # The engine mutates Source/Tree only through its own state; rebuild per run anyway.
    r = cls(sources, trees, s, p).run()
    return {m: r.get(m) for m in M}


def sha(path):
    return hashlib.sha256(open(path, "rb").read()).hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--structure", required=True)
    ap.add_argument("--r1", type=float, required=True)
    ap.add_argument("--w", type=float, required=True)
    ap.add_argument("--workers", type=int, required=True)
    ap.add_argument("--t-meta", type=float, required=True)
    ap.add_argument("--t-fp", type=float, required=True)
    ap.add_argument("--agent", default="claude-code")
    ap.add_argument("--doc-max-bytes", type=int, default=4096)
    ap.add_argument("--inputs-json", help="JSON object recorded verbatim as the measured-input provenance")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    p, glob, k, seq_mac = params(a.r1, a.w, a.t_meta, a.t_fp)
    TM = a.t_meta

    def fresh():
        src, trees, _ = load_structure(a.structure, a.agent, a.doc_max_bytes)
        return src, trees

    configs = {"default": (512 * MIB, 9), "stress": (240 * MIB, a.workers)}
    raw = {}
    for cname, (G, P) in configs.items():
        raw[cname] = {}
        for label, sc, cls in [("a_HEAD", "a", engine.Sim), ("L1_b_chunk", "b_chunk", engine.Sim),
                               ("g_fair", "g_rr", FairHeads), ("e_chunks0", "e_rr", FairHeads)]:
            src, trees = fresh()
            raw[cname][label] = run(src, trees, sc, cls, G, P, p, glob)

    def f(m, ref):
        return None if m is None else round((m - TM) / (ref - TM), 4)

    def ratio(x, y):
        return None if (x is None or not y) else round(x / y, 4)

    d, s = raw["default"], raw["stress"]
    A, G_, L = d["a_HEAD"], d["g_fair"], d["L1_b_chunk"]
    p2 = {
        "canonicalReady_B2_over_B1": ratio(G_["t_complete"], A["t_complete"]),
        "peakProxy_B2_over_B1": ratio(G_["peak_broad"], A["peak_broad"]),
        "firstReadable_minus_Tmeta_s": round(G_["first_readable"] - TM, 4),
        "f_24hAtNewest": f(G_["recent_24h"], A["t_complete"]),
        "f_newest10": f(G_["newest10"], A["t_complete"]),
        "f_newestGiant": f(G_["newest_giant"], A["t_complete"]),
        "f_small24hAtNewest": f(G_["s_24h"], A["t_complete"]),
        "f_small7dAtNewest": f(G_["s_7d"], A["t_complete"]),
        "f_7dAtNewest": f(G_["recent_7d"], A["t_complete"]),
        "f_allReadable": f(G_["all_readable"], A["t_complete"]),
        "BE_f_24hAtNewest": f(A["recent_24h"], A["t_complete"]),
        "longestApplyTx_s": round(G_["max_apply_tx"], 4),
        "longestFlipTx_s": round(G_["max_flip_tx"], 4),
        "laneSWriterWaitMax_s": round(G_["lane_s_writer_wait_max"], 4),
        "writerBusyShareOfCapture": round(G_["writer_busy"] / G_["capture_wall"], 4) if G_["capture_wall"] else None,
        "canonicalReady_s": {"B1_HEAD": round(A["t_complete"], 3), "B2_g": round(G_["t_complete"], 3),
                             "L1": round(L["t_complete"], 3)},
        "e6_chunks0_over_g_canonicalReady": ratio(d["e_chunks0"]["t_complete"], G_["t_complete"]),
        "e6_L1_over_g_canonicalReady": ratio(L["t_complete"], G_["t_complete"]),
        "e6_HEAD_over_g_canonicalReady": ratio(A["t_complete"], G_["t_complete"]),
    }
    A, G_, L = s["a_HEAD"], s["g_fair"], s["L1_b_chunk"]
    sp = {
        "canonicalReady_B2_over_L1": ratio(G_["t_complete"], L["t_complete"]),
        "canonicalReady_B2_over_BEstress": ratio(G_["t_complete"], A["t_complete"]),
        "small7dAtNewest_B2_over_L1": ratio(G_["s_7d"], L["s_7d"]),
        "small24hAtNewest_B2_over_L1": ratio(G_["s_24h"], L["s_24h"]),
        "24hAtNewest_B2_over_L1": ratio(G_["recent_24h"], L["recent_24h"]),
        "f_24hAtNewest_refL1": f(G_["recent_24h"], L["t_complete"]),
        "newest10_B2_over_L1": ratio(G_["newest10"], L["newest10"]),
        "newestGiant_B2_over_L1": ratio(G_["newest_giant"], L["newest_giant"]),
        "f_newestGiant_refL1": f(G_["newest_giant"], L["t_complete"]),
        "firstReadable_minus_Tmeta_s": round(G_["first_readable"] - TM, 4),
        "longestApplyTx_s": round(G_["max_apply_tx"], 4),
        "longestFlipTx_s": round(G_["max_flip_tx"], 4),
        "peakProxy_B2_over_L1": ratio(G_["peak_broad"], L["peak_broad"]),
        "peakProxy_B2_over_BEstress": ratio(G_["peak_broad"], A["peak_broad"]),
        "canonicalReady_s": {"BEstress_HEAD": round(A["t_complete"], 3), "B2_g": round(G_["t_complete"], 3),
                             "L1": round(L["t_complete"], 3)},
    }
    src, trees = fresh()
    doc = {
        "format": "r1-stress-prediction/1",
        "preRegistered": True,
        "note": "Pre-registered before any P2 or S run (brief v3 §12 E1 step 8). Not gating; a measured ratio more "
                "than 25% from its prediction is a model-error finding. The Final-targets thresholds are frozen and "
                "are not re-derived from these numbers.",
        "inputs": {"r1_M4": a.r1, "W_M4": a.w, "N_s": a.workers, "T_meta_s": a.t_meta, "finalizePublication_s": a.t_fp,
                   "speedFactorVsMacBook": round(k, 4), "seqRate": seq_mac * k, "agent": a.agent,
                   "docMaxBytes": a.doc_max_bytes, "provenance": json.loads(a.inputs_json) if a.inputs_json else None},
        "population": {"sources": len(src), "jsonl": sum(1 for x in src if x.kind == "jsonl"),
                       "docs": sum(1 for x in src if x.kind == "doc"), "trees": len(trees),
                       "bigTrees": sum(1 for t in trees if t.lane == "B"),
                       "bytes": int(sum(x.size for x in src))},
        "scripts": {"stress_m4_v3.py": sha(os.path.abspath(__file__)),
                    "stress_m4.py": sha(os.path.join(HERE, "stress_m4.py")),
                    "engine.py": sha(os.path.join(SIM, "engine.py")),
                    "scenarios.py": sha(os.path.join(SIM, "scenarios.py")),
                    "structure": sha(a.structure)},
        "configs": {"default": {"G_MiB": 512, "P": 9}, "stress": {"G_MiB": 240, "P": a.workers}},
        "predictions": {"P2": p2, "S": sp},
        "raw": raw,
    }
    with open(a.out, "x") as fh:
        json.dump(doc, fh, indent=1)
        fh.write("\n")
    print(json.dumps(doc["predictions"], indent=1))


if __name__ == "__main__":
    main()
