#!/bin/zsh
# R1 pair driver (brief v3 §12 E3/E4), from imgdefer/pair_driver.sh.
#
# usage: pair_r1.sh NAME ROUNDS TAG=BINARY [TAG=BINARY ...]
#   Interleaved: for each round, every side in the order given (A B A B ...).
#   Per-side serve flags: env FLAGS_<TAG>="--capture-workers 4 --capture-lookahead 240m"
#
# env (all optional):
#   OUT_ROOT     where NAME/ goes                    (default $S/pairs)
#   SNAP         frozen snapshot                     (default $S/snapshot)
#   VERIFY_BIN   binary for `ledger verify --json`   (default: the side's own; the brief uses B2 for every run)
#   LOAD=1       P3: fork r1_reader_load.js in every run
#   LOAD_TAGS="B2L"  P3: fork it only in these sides' runs
#   RANKED=1     ranked-result digest per run (warm start of the run's own db; r1_ranked.py)
#   REBUILD_TAGS="B2" REBUILD_ROUNDS=3   evidence R: `rebuild` that side's ledger in rounds 1..N
#                and digest the rebuilt projection (rebuild_digest.out)
#   KEEP=1       keep dbs and private logs (default: delete once digested)
#   LOAD_MAX=2.0 LOAD_WAIT_S=600   load guard: wait up to LOAD_WAIT_S for load1 < LOAD_MAX,
#                then proceed anyway (the user's iOS Simulator can hold load above 2);
#                load and swap are recorded on every run
#   SWAP_REDO_MB=256   redo a run slot once if swap grew by more than this during it (0 = never)
#   MIN_FREE_GB=25     refuse to start a run below this much free disk
#
# Before every run: verify_r1_snapshot.js (walkInputsDigest + SHA256SUMS +
# read-only modes; refuses to start on a mismatch). After every run: the
# read-only digest SQL (file:...?mode=ro&immutable=1), ledger verify, then the
# optional rebuild and ranked steps, then cleanup. Finally summarize.py per
# side and over all runs (self CR reference; re-run summarize.py with --cr-ref
# or --ref for the brief's fractions, e.g. CR_ref = CR(BE)).
set -uo pipefail
zmodload zsh/datetime
S=/private/tmp/claude-501/-Users-jasper-james-test/392f0bf2-e033-4042-8270-d9688bb9fed2/scratchpad/r1
export PATH="$HOME/.cargo/bin:$PATH"
SQLITE=/usr/bin/sqlite3

if (( $# < 3 )); then
  sed -n '2,30p' $0
  exit 2
fi
NAME=$1; ROUNDS=$2; shift 2
OUT_ROOT=${OUT_ROOT:-$S/pairs}
SNAP=${SNAP:-$S/snapshot}
LOAD_MAX=${LOAD_MAX:-2.0}
LOAD_WAIT_S=${LOAD_WAIT_S:-600}
IDLE_MIN=${IDLE_MIN:-75}
GUARD_WAIT_S=${GUARD_WAIT_S:-1800}
SWAP_REDO_MB=${SWAP_REDO_MB:-256}
MIN_FREE_GB=${MIN_FREE_GB:-25}
REBUILD_TAGS=${REBUILD_TAGS:-}
REBUILD_ROUNDS=${REBUILD_ROUNDS:-3}
DIR=$OUT_ROOT/$NAME
if [[ -e $DIR ]]; then echo "pair directory exists; refusing" >&2; exit 2; fi
mkdir -p $DIR
LOG=$DIR/pair.log

typeset -a TAGS BINS
for spec in "$@"; do
  TAGS+=(${spec%%=*}); BINS+=(${spec#*=})
  [[ -x ${spec#*=} ]] || { echo "not executable: side ${spec%%=*}" >&2; exit 2; }
done

load1() { sysctl -n vm.loadavg | awk '{print $2}'; }
cpu_idle() { top -l 2 -n 0 -s 2 | awk '/CPU usage/ {v=$7} END {sub("%","",v); print v}'; }
swap_mb() { sysctl -n vm.swapusage | sed -E 's/.*used = ([0-9.]+)M.*/\1/'; }
free_gb() { df -g $S | awk 'NR==2 {print $4}'; }
stamp() { print -r -- "== $1 $(date +%H:%M:%S) load=$(sysctl -n vm.loadavg) swap_mb=$(swap_mb) free_gb=$(free_gb)" | tee -a $LOG; }

# Pair manifest: sides, binaries, flags, snapshot identity, machine.
HARNESS_DIR=$S python3 -I - "$DIR/pair.json" "$NAME" "$ROUNDS" "$SNAP" "${TAGS[*]}" "${BINS[*]}" <<'EOF'
import hashlib, json, os, platform, subprocess, sys, time
out, name, rounds, snap, tags, bins = sys.argv[1:]
meta = json.load(open(os.path.join(snap, 'snapshot.json')))
sides = []
for tag, binary in zip(tags.split(), bins.split()):
    sides.append({'tag': tag, 'binarySha256': hashlib.sha256(open(binary, 'rb').read()).hexdigest(),
                  'flags': os.environ.get(f'FLAGS_{tag}', ''), 'commit': os.environ.get(f'COMMIT_{tag}')})
model = subprocess.run(['sysctl', '-n', 'hw.model'], capture_output=True, text=True).stdout.strip()
json.dump({'format': 'r1-pair/1', 'name': name, 'rounds': int(rounds), 'order': 'interleaved, sides in the order given each round',
           'sides': sides, 'snapshot': {'walkInputsDigest': meta['walkInputsDigest'], 'manifestSha256': meta['manifestSha256'],
                                        'files': meta['totals']['files'], 'bytes': meta['totals']['bytes']},
           'machine': {'model': model, 'os': platform.mac_ver()[0]},
           'harnessSha256': {f: hashlib.sha256(open(os.path.join(os.environ['HARNESS_DIR'], f), 'rb').read()).hexdigest()
                             for f in ('run_r1.js', 'r1_logical_digest.sql', 'summarize.py', 'r1_reader_load.js', 'r1_ranked.py',
                                       'verify_r1_snapshot.js', 'r1_snapshot_lib.js', 'pair_r1.sh', 'product-diagnostic-v3.json',
                                       'r1_cursor_order.sql')
                             if os.path.exists(os.path.join(os.environ['HARNESS_DIR'], f))},
           'env': {k: os.environ.get(k) for k in ('LOAD', 'LOAD_TAGS', 'RANKED', 'REBUILD_TAGS', 'REBUILD_ROUNDS', 'VERIFY_BIN', 'LOAD_MAX',
                                                   'IDLE_MIN', 'GUARD_WAIT_S', 'SWAP_REDO_MB', 'CR_REF')},
           'startedAt': time.strftime('%Y-%m-%dT%H:%M:%S%z')}, open(out, 'x'), indent=1)
EOF

sanitize_verify() {  # stdin: ledger verify --json; stdout: no paths, segment lists as counts
  python3 -I -c '
import json, sys
lines = [l for l in sys.stdin.read().splitlines() if l.startswith("{")]
v = json.loads(lines[-1])
v.pop("ledger", None)
for r in v.get("runtimes", []):
    segs = r.pop("segments", None)
    r["segmentCount"] = len(segs) if isinstance(segs, list) else segs
print(json.dumps(v, indent=1, sort_keys=True))'
}

run_one() {  # $1 tag  $2 binary  $3 round  $4 run dir
  local tag=$1 bin=$2 round=$3 run=$4
  local flags_var="FLAGS_$tag"; local flags=${(P)flags_var:-}
  local free=$(free_gb)
  if (( free < MIN_FREE_GB )); then stamp "STOP free disk ${free} GB < ${MIN_FREE_GB}"; return 3; fi
  # Quiet-machine guard (Task C). The user's iOS Simulator holds load1 near 4
  # with ~1 core busy of 10, so load1 < 2.0 is unreachable here; the guard
  # instead waits until two consecutive 2 s `top` samples show CPU idle >=
  # IDLE_MIN (no foreign heavy job), up to GUARD_WAIT_S, then proceeds anyway
  # (recorded). load1 and swap are still stamped on every run.
  local g0=$EPOCHSECONDS ok=0 idle=$(cpu_idle) l=$(load1)
  while (( EPOCHSECONDS - g0 < GUARD_WAIT_S )); do
    if (( idle >= IDLE_MIN )); then (( ok += 1 )); else ok=0; fi
    (( ok >= 2 )) && break
    sleep 5; idle=$(cpu_idle); l=$(load1)
  done
  local waited=$(( EPOCHSECONDS - g0 ))
  local proceeded=$(( ok >= 2 ? 0 : 1 ))
  # Snapshot identity right before every run (it also warms the page cache
  # identically for every side).
  local vfile=$DIR/.verify-$$.json
  if ! node $S/verify_r1_snapshot.js $SNAP > $vfile; then
    stamp "STOP snapshot verification failed before $run"; cat $vfile | tee -a $LOG; return 4
  fi
  local swap0=$(swap_mb)
  stamp "$tag round=$round start (waited ${waited}s, cpu_idle=$idle, load1=$l, proceeded_over_guard=$proceeded)"
  local -a extra=(); [[ -n ${LOAD:-} || " ${LOAD_TAGS:-} " == *" $tag "* ]] && extra+=(--load)
  node $S/run_r1.js $bin $SNAP $run --label "$tag-$round" $extra -- ${=flags} 2>&1 | tee -a $LOG
  local rc=${pipestatus[1]}
  local swap1=$(swap_mb)
  mv $vfile $run/verify_snapshot.json 2>/dev/null || mv $vfile $DIR/verify-$tag-$round.json
  python3 -I -c '
import json, sys
tag, rnd, waited, l, proceeded, s0, s1, lim, wait_s, rc = sys.argv[1:11]
print(json.dumps({"tag": tag, "round": int(rnd), "loadGuard": {"max": float(lim), "waitS": int(wait_s), "waitedS": int(waited),
  "rule": "two consecutive 2 s top samples with CPU idle >= idleMin, up to waitS; load1 recorded only",
  "idleMin": float(sys.argv[11]), "cpuIdleAtStart": float(sys.argv[12]),
  "load1AtStart": float(l), "proceededOverGuard": proceeded == "1"}, "swapUsedMbBefore": float(s0), "swapUsedMbAfter": float(s1),
  "swapGrewMb": round(float(s1) - float(s0), 2), "runExit": int(rc)}, indent=1))' \
    $tag $round $waited $l $proceeded $swap0 $swap1 $LOAD_MAX $GUARD_WAIT_S $rc $IDLE_MIN $idle > $run/guard.json
  [[ $rc == 0 && -f $run/db/rosetta.sqlite ]] || { stamp "$tag round=$round FAILED rc=$rc"; return 1; }
  # Read-only digest and ledger verify.
  $SQLITE -readonly "file:$run/db/rosetta.sqlite?mode=ro&immutable=1" < $S/r1_logical_digest.sql > $run/r1_logical_digest.out \
    || { stamp "$tag round=$round digest FAILED"; return 1; }
  # Cursor-order diagnostic (Task C; reported, never gated).
  $SQLITE -readonly "file:$run/db/rosetta.sqlite?mode=ro&immutable=1" < $S/r1_cursor_order.sql > $run/r1_cursor_order.out \
    || stamp "$tag round=$round cursor-order diagnostic FAILED"
  ${VERIFY_BIN:-$bin} ledger verify --db $run/db/rosetta.sqlite --json 2>/dev/null | sanitize_verify > $run/ledger_verify.json \
    || stamp "$tag round=$round ledger verify FAILED"
  # Evidence R: rebuild this ledger and digest the rebuilt projection.
  if [[ " $REBUILD_TAGS " == *" $tag "* ]] && (( round <= REBUILD_ROUNDS )); then
    mkdir -p $run/rebuild
    local t0=$EPOCHREALTIME
    if $bin rebuild --db $run/rebuild/rosetta.sqlite --ledger $run/db/rosetta.ledger > $run/rebuild/rebuild.log 2>&1; then
      $SQLITE -readonly "file:$run/rebuild/rosetta.sqlite?mode=ro&immutable=1" < $S/r1_logical_digest.sql > $run/rebuild_digest.out
      $SQLITE -readonly "file:$run/rebuild/rosetta.sqlite?mode=ro&immutable=1" < $S/r1_cursor_order.sql > $run/rebuild_cursor_order.out
      print -r -- "{\"rebuildWallS\": $(( EPOCHREALTIME - t0 )), \"exit\": 0}" > $run/rebuild.json
    else
      print -r -- "{\"rebuildWallS\": $(( EPOCHREALTIME - t0 )), \"exit\": 1}" > $run/rebuild.json
      stamp "$tag round=$round rebuild FAILED"
    fi
  fi
  if [[ -n ${RANKED:-} ]]; then
    python3 -I $S/r1_ranked.py $bin $run/db/rosetta.sqlite $SNAP/home $run/ranked.json 2>&1 | tee -a $LOG
  fi
  # Delete intermediate outputs once digested.
  if [[ -z ${KEEP:-} ]]; then
    rm -rf $run/db $run/rebuild $run/stdout.log $run/stderr.log
  fi
  local grew=$(python3 -I -c "import sys; print(1 if float(sys.argv[2]) - float(sys.argv[1]) > float(sys.argv[3]) else 0)" $swap0 $swap1 $SWAP_REDO_MB)
  if (( SWAP_REDO_MB > 0 && grew )); then return 10; fi
  return 0
}

stamp "start $NAME rounds=$ROUNDS sides=${TAGS[*]}"
typeset -a RUNS
for (( round = 1; round <= ROUNDS; round++ )); do
  for (( i = 1; i <= ${#TAGS}; i++ )); do
    tag=${TAGS[$i]}; bin=${BINS[$i]}
    run=$DIR/$(printf %02d $round)-$tag
    run_one $tag $bin $round $run; rc=$?
    if (( rc == 10 )); then
      stamp "$tag round=$round swap grew > ${SWAP_REDO_MB} MB: redo once (first attempt kept as ${run:t}-swapdiscard)"
      mv $run $run-swapdiscard
      run_one $tag $bin $round $run; rc=$?
      (( rc == 10 )) && rc=0
    fi
    (( rc == 0 )) || { stamp "abort rc=$rc"; exit $rc; }
    RUNS+=($run)
  done
done
stamp done

for tag in $TAGS; do
  python3 -I $S/summarize.py --out $DIR/summary-$tag.json ${CR_REF:+--cr-ref=$CR_REF} $DIR/*-$tag(/) > $DIR/summary-$tag.txt 2>&1
done
python3 -I $S/summarize.py --out $DIR/summary-all.json ${CR_REF:+--cr-ref=$CR_REF} $RUNS > $DIR/summary-all.txt 2>&1
cat $DIR/summary-all.txt | tee -a $LOG
