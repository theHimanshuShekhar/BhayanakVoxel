#!/bin/bash
# THROWAWAY (#99): the whole capture, in the order to run it. On the 4090 (Windows) run the
# commands below one by one in cmd.exe, or this script under Git Bash / WSL if you have it.
#
#   measure-reference-run.sh CLIENT CAP_DIR [SIZE] [TRUTH_SPP] [CHECKPOINTS] [DISPATCHES] [K720]
#
# Defaults are the real measurement (2560x1440, 8192 spp, 1,4,16,64,256,1024). For a smoke test
# pass e.g. 160x90 256 1,4,16,64 24 1,2.
set -euo pipefail
CLIENT=${1:?path to client(.exe)}
CAP=${2:?capture folder}
SIZE=${3:-2560x1440}
TRUTH=${4:-8192}
CHECKPOINTS=${5:-1,4,16,64,256,1024}
DISPATCHES=${6:-192}
K720=${7:-1,2,4,8}
for SCALE in 1.0 0.5; do
  for VIEW in golden interreflect; do
    D="$CAP/$VIEW-s$SCALE"
    COMMON=(--view "$VIEW" --scale "$SCALE" --size "$SIZE")
    "$CLIENT" measure-reference converge --out "$D/converge" "${COMMON[@]}" --checkpoints "$CHECKPOINTS"
    "$CLIENT" measure-reference truth --out "$D/truthA" "${COMMON[@]}" --seed 1 --truth-spp "$TRUTH"
    "$CLIENT" measure-reference truth --out "$D/truthB" "${COMMON[@]}" --seed 2 --truth-spp "$TRUTH"
    "$CLIENT" measure-reference batch --out "$D/batch" "${COMMON[@]}" --dispatches "$DISPATCHES" --k720 "$K720"
    "$CLIENT" measure-reference split --out "$D/split" "${COMMON[@]}" --dispatches "$DISPATCHES"
  done
done
for VIEW in golden interreflect; do
  "$CLIENT" measure-reference stop-clean --out "$CAP/$VIEW-s1.0/stop-clean" --view "$VIEW" --scale 1.0 --size "$SIZE"
done
