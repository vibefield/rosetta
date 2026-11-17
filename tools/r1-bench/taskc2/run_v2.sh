#!/bin/zsh
# R1 evidence rerun after the memory and drift fixes (campaign r1-newest-trees-cold-20261102b).
# Same snapshot, harness, machine, flags and run counts as the first campaign; new B1 (R1-U with
# the drift cache) and B2 (R1 with the look-ahead reservation and direct large blocks). P0
# (3ca7410 vs R1-E) is unchanged and reused. Strictly sequential; every run goes through
# pair_r1.sh (snapshot verify + idle guard + digests + cleanup).
set -u
S=/private/tmp/claude-501/-Users-jasper-james-test/392f0bf2-e033-4042-8270-d9688bb9fed2/scratchpad/r1
B=$S/bin2
export OUT_ROOT=$S/pairs2
CE=$(awk '$1=="rosettad-BE"{print $3}' $B/commits.txt)
C1=$(awk '$1=="rosettad-B1"{print $3}' $B/commits.txt)
C2=$(awk '$1=="rosettad-B2"{print $3}' $B/commits.txt)
STRESS="--capture-lookahead 240m --capture-workers 9"
V=$B/rosettad-B2
cd $S
step() { print -r -- "### $1 $(date +%H:%M:%S)"; }
only=${1:-all}
want() { [[ $only == all || " $only " == *" $1 "* ]]; }

if want P1; then
step P1
VERIFY_BIN=$V RANKED=1 COMMIT_BE=$CE COMMIT_B1=$C1 \
  zsh $S/pair_r1.sh P1 11 BE=$B/rosettad-BE B1=$B/rosettad-B1 > $OUT_ROOT/P1.console 2>&1 || { step "P1 FAILED"; exit 1; }
fi

if want P2; then
step P2
VERIFY_BIN=$V RANKED=1 REBUILD_TAGS=B2 REBUILD_ROUNDS=3 COMMIT_B1=$C1 COMMIT_B2=$C2 \
  zsh $S/pair_r1.sh P2 11 B1=$B/rosettad-B1 B2=$B/rosettad-B2 > $OUT_ROOT/P2.console 2>&1 || { step "P2 FAILED"; exit 1; }
fi

if want S; then
step S
VERIFY_BIN=$V COMMIT_B2S=$C2 COMMIT_L1=$C2 COMMIT_BES=$CE \
  FLAGS_B2S="$STRESS" FLAGS_L1="$STRESS --experiment-cold-lanes 1" FLAGS_BES="$STRESS" \
  zsh $S/pair_r1.sh S 11 B2S=$B/rosettad-B2 L1=$B/rosettad-B2 BES=$B/rosettad-BE > $OUT_ROOT/S.console 2>&1 || { step "S FAILED"; exit 1; }
fi

if want P3; then
step P3
VERIFY_BIN=$V LOAD_TAGS="B2L" COMMIT_B2N=$C2 COMMIT_B2L=$C2 \
  zsh $S/pair_r1.sh P3 11 B2N=$B/rosettad-B2 B2L=$B/rosettad-B2 > $OUT_ROOT/P3.console 2>&1 || { step "P3 FAILED"; exit 1; }
fi

if want P3s; then
step P3-stress
VERIFY_BIN=$V LOAD_TAGS="B2L" COMMIT_B2N=$C2 COMMIT_B2L=$C2 FLAGS_B2N="$STRESS" FLAGS_B2L="$STRESS" \
  zsh $S/pair_r1.sh P3-stress 5 B2N=$B/rosettad-B2 B2L=$B/rosettad-B2 > $OUT_ROOT/P3-stress.console 2>&1 || { step "P3-stress FAILED"; exit 1; }
fi

if want X; then
step X
VERIFY_BIN=$V COMMIT_B1=$C1 COMMIT_B2X=$C2 FLAGS_B2X="--experiment-cold-admission runtime" \
  zsh $S/pair_r1.sh X 7 B1=$B/rosettad-B1 B2X=$B/rosettad-B2 > $OUT_ROOT/X.console 2>&1 || { step "X FAILED"; exit 1; }
fi

if want Rk; then
# Evidence R, ledger side: one B2 run kept, rebuilt, then its ledger verified again after the
# rebuild (the rebuild must leave the ledger's source digest and sourceSetDigest unchanged).
step Rk
KEEP=1 VERIFY_BIN=$V REBUILD_TAGS=B2 REBUILD_ROUNDS=1 COMMIT_B2=$C2 \
  zsh $S/pair_r1.sh Rk 1 B2=$B/rosettad-B2 > $OUT_ROOT/Rk.console 2>&1 || { step "Rk FAILED"; exit 1; }
R=$OUT_ROOT/Rk/01-B2
$V ledger verify --db $R/db/rosetta.sqlite --json 2>/dev/null | python3 -I -c '
import json, sys
lines = [l for l in sys.stdin.read().splitlines() if l.startswith("{")]
v = json.loads(lines[-1]); v.pop("ledger", None)
for r in v.get("runtimes", []):
    segs = r.pop("segments", None); r["segmentCount"] = len(segs) if isinstance(segs, list) else segs
print(json.dumps(v, indent=1, sort_keys=True))' > $R/ledger_verify_after_rebuild.json
ls -la $R/db > /dev/null 2>&1 && print -r -- "ledger dir entries: $(ls $R/db | wc -l | tr -d ' ')"
rm -rf $R/db $R/rebuild $R/stdout.log $R/stderr.log
fi

if want E6d; then
step E6-default
VERIFY_BIN=$V \
  COMMIT_G=$C2 COMMIT_C0=$C2 COMMIT_L1=$C2 COMMIT_AR=$C2 COMMIT_W0=$C2 COMMIT_WNS=$C2 \
  FLAGS_G="" FLAGS_C0="--experiment-cold-chunks 0" FLAGS_L1="--experiment-cold-lanes 1" \
  FLAGS_AR="--experiment-cold-admission runtime" FLAGS_W0="--capture-workers 0" FLAGS_WNS="--capture-workers 9" \
  zsh $S/pair_r1.sh E6-default 3 G=$B/rosettad-B2 C0=$B/rosettad-B2 L1=$B/rosettad-B2 AR=$B/rosettad-B2 W0=$B/rosettad-B2 WNS=$B/rosettad-B2 \
  > $OUT_ROOT/E6-default.console 2>&1 || { step "E6-default FAILED"; exit 1; }
fi

if want E6s; then
step E6-stress
VERIFY_BIN=$V \
  COMMIT_G=$C2 COMMIT_C0=$C2 COMMIT_L1=$C2 COMMIT_AR=$C2 COMMIT_W0=$C2 \
  FLAGS_G="$STRESS" FLAGS_C0="$STRESS --experiment-cold-chunks 0" FLAGS_L1="$STRESS --experiment-cold-lanes 1" \
  FLAGS_AR="$STRESS --experiment-cold-admission runtime" FLAGS_W0="--capture-lookahead 240m --capture-workers 0" \
  zsh $S/pair_r1.sh E6-stress 3 G=$B/rosettad-B2 C0=$B/rosettad-B2 L1=$B/rosettad-B2 AR=$B/rosettad-B2 W0=$B/rosettad-B2 \
  > $OUT_ROOT/E6-stress.console 2>&1 || { step "E6-stress FAILED"; exit 1; }
fi
step done
