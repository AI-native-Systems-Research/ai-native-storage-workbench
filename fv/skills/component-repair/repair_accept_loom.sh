#!/usr/bin/env bash
# repair_accept_loom.sh <repo> <component> <OBLIGATION-ID> <base-rev>
#
# Acceptance for a repair whose target property is proved with LOOM (all thread interleavings) rather than
# Creusot. Run by code, outside the agent; exit 0 only if every leg holds:
#   1. REAL       components/<c>/src changed relative to <base-rev>.
#   2. RED-FIRST  the repair's own regression test components/<c>/src/**/repair_test_*.rs FAILS on a clean copy of
#                 the unfixed code (native libraries linked in, so a missing library is reported as ENVIRONMENT).
#   3. WAS BROKEN on the unfixed code the Loom test verify_<id> FAILS: the target really was violated there.
#   4. PROVED     on the fixed code verify_<id> PASSES over all interleavings, and its twin verify_<id>__mutant
#                 exists and FAILS (the test has content).
#   5. MIRROR     verif-loom/mirrored_sources.sha256 matches the FIXED source files it lists (the mirror was synced
#                 to the fix), and if a listed source file changed, the mirror's own sources changed too.
set -uo pipefail
REPO="${1:?usage: repair_accept_loom.sh <repo> <component> <ID> <base>}"; COMP="${2:?}"; OBL="${3:?}"; BASE="${4:?}"
export PATH="$HOME/.cargo/bin:$PATH"
source "$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)/layout.sh"
CDIR="$REPO/components/$COMP"; LDIR="$CDIR/verif-loom"
PKG="$(sed -nE 's/^name[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' "$CDIR/Cargo.toml" | head -1)"
T="verify_$(tr 'A-Z-' 'a-z_' <<<"$OBL")"
fail=0
echo "ACCEPT-LOOM: $OBL in $COMP (base $BASE), test $T"
[[ -d "$LDIR" ]] || { echo "  ACCEPT-LOOM: FAIL - no verif-loom crate in components/$COMP"; exit 1; }

loom_test() {   # <crate dir> <test name> -> rc 0 pass / 1 fail / 124 timeout / 3 absent
  local d="$1" t="$2" f
  f="$(grep -rlE "fn ${t}\s*\(" "$d/tests" "$d/src" 2>/dev/null | head -1)"; [[ -n "$f" ]] || return 3
  local args=(--release -q)
  [[ "$f" == "$d/tests/"* ]] && args+=(--test "$(basename "$f" .rs)") || args+=(--lib)
  (cd "$d" && RUSTFLAGS="--cfg loom" timeout "${LOOM_TIMEOUT:-1800}" cargo test "${args[@]}" "$t" -- --exact > "${LOOM_LOG_DIR:-/tmp}/loom_${COMP}_$t.log" 2>&1)
  local rc=$?
  (( rc == 0 || rc == 124 )) && return $rc
  # A build failure shows compiler/build-script errors; anything else non-zero means the test RAN and failed -
  # a Loom violation can end in "test result: FAILED" or, when a destructor panics during unwinding, in SIGABRT.
  grep -E "could not compile|failed to run custom build command|^error(\[E[0-9]+\])?: " \
    "${LOOM_LOG_DIR:-/tmp}/loom_${COMP}_$t.log" | grep -qv "^error: test failed" && return 2  # it did not build
  return 1                                                                                  # it ran and failed
}

# 1. REAL
if git -C "$REPO" diff --quiet "$BASE" -- "components/$COMP/src"; then echo "  1. REAL      : NO - src/ unchanged"; fail=1
else echo "  1. REAL      : yes ($(git -C "$REPO" diff --shortstat "$BASE" -- "components/$COMP/src" | sed 's/^ *//'))"; fi

# clean copy of the unfixed code, with the native libraries
TMPR="$(mktemp -d)"; git -C "$REPO" worktree add -q --detach "$TMPR/base" "$BASE"
for n in spdk spdk-build zyre-build; do
  for src in "$REPO/deps/$n" "${CERTUS_DEPS_DIR:-/opt}/$n"; do
    [[ -e "$TMPR/base/deps/$n" ]] && break; [[ -e "$src" ]] && ln -s "$(readlink -f "$src")" "$TMPR/base/deps/$n" && break
  done
done

# 2. RED-FIRST
added="$( { git -C "$REPO" diff --name-only --diff-filter=A "$BASE" -- "components/$COMP/src";
            git -C "$REPO" ls-files --others --exclude-standard -- "components/$COMP/src"; } | grep -E '/repair_test_[^/]+\.rs$' | sort -u)"
if [[ -z "$added" ]]; then echo "  2. RED-FIRST : NO - no regression test components/$COMP/src/**/repair_test_*.rs"; fail=1
else
  rf_ok=1
  for t in $added; do
    b="$(basename "$t")"; host="$(grep -rlE "#\[path *= *\"$b\"\]" "$CDIR/src" | head -1)"
    [[ -n "$host" ]] || { echo "  2. RED-FIRST : NO - $b is not wired in by a #[path = \"$b\"] mod line"; rf_ok=0; continue; }
    cp "$REPO/$t" "$TMPR/base/$t"; printf '\n#[cfg(test)]\n#[path = "%s"]\nmod %s;\n' "$b" "${b%.rs}" >> "$TMPR/base/${host#$REPO/}"
  done
  if (( rf_ok )); then
    bout="$(cd "$TMPR/base" && timeout 1800 cargo test -q -p "$PKG" $(test_features "$CDIR") repair_test_ 2>&1)"
    if grep -qE 'failed to run custom build command|could not find native static library' <<<"$bout" && ! grep -q 'test result:' <<<"$bout"; then
      echo "  2. RED-FIRST : ENVIRONMENT - the unfixed code could not be built here (a native library is missing)"; fail=1
    elif grep -qE 'error(\[E[0-9]+\])?: ' <<<"$bout" && ! grep -q 'test result:' <<<"$bout"; then
      echo "  2. RED-FIRST : NO - the regression test does not COMPILE against the unfixed code; it must use the existing API"
      grep -E '^error' <<<"$bout" | head -2 | sed 's/^/       │ /'; fail=1
    elif grep -qE 'test result: FAILED\. [0-9]+ passed; [1-9][0-9]* failed' <<<"$bout"; then
      echo "  2. RED-FIRST : yes (fails on the unfixed code, as it must)"
    else echo "  2. RED-FIRST : NO - the regression test PASSES on the unfixed code, so it does not show the defect"; fail=1; fi
  else fail=1; fi
fi

# 3. WAS BROKEN - the Loom test on the unfixed code (its mirror as committed at base)
if [[ -d "$TMPR/base/components/$COMP/verif-loom" ]]; then
  loom_test "$TMPR/base/components/$COMP/verif-loom" "$T"; rc=$?
  case $rc in
    1) echo "  3. WAS BROKEN: yes (Loom finds the violation on the unfixed code)";;
    0) echo "  3. WAS BROKEN: NO - $T PASSES on the unfixed code, so it does not show the defect"; fail=1;;
    124) echo "  3. WAS BROKEN: NO - timed out on the unfixed code"; fail=1;;
    3) echo "  3. WAS BROKEN: NO - $T is not in the base Loom crate (commit the Loom crate with the base)"; fail=1;;
    *) echo "  3. WAS BROKEN: NO - the base Loom crate did not build or run (log ${LOOM_LOG_DIR:-/tmp}/loom_${COMP}_$T.log)"; fail=1;;
  esac
else echo "  3. WAS BROKEN: NO - no verif-loom crate at the base commit"; fail=1; fi
git -C "$REPO" worktree remove --force "$TMPR/base"; rm -rf "$TMPR"

# 4. PROVED - on the fixed code, with a twin that fails
loom_test "$LDIR" "$T"; rc=$?
if [[ $rc -eq 0 ]]; then
  loom_test "$LDIR" "${T}__mutant"; mrc=$?
  case $mrc in
    1) f="$(grep -rlE "fn ${T}\s*\(" "$LDIR/tests" "$LDIR/src" 2>/dev/null | head -1)"
       b="$(grep -A3 -E "fn ${T}\s*\(" "$f" | grep -oE "[A-Z_]*BOUND" | head -1)"
       bv="$( [[ -n "$b" ]] && grep -m1 -oE "const $b: Option<usize> = [^;]+" "$f" | sed 's/.*= //')"
       if [[ -n "$bv" && "$bv" != None ]]; then sc="search: BOUNDED, at most ${bv#Some(} preemptions"; sc="${sc%)}"; else sc="search: all interleavings"; fi
       echo "  4. PROVED    : yes ($T passes; ${T}__mutant fails; $sc)";;
    0) echo "  4. PROVED    : NO - the twin ${T}__mutant also passes: the test has no content"; fail=1;;
    3) echo "  4. PROVED    : NO - no twin ${T}__mutant"; fail=1;;
    *) echo "  4. PROVED    : NO - the twin did not finish (rc $mrc)"; fail=1;;
  esac
else echo "  4. PROVED    : NO - $T does not pass on the fixed code (rc $rc; log ${LOOM_LOG_DIR:-/tmp}/loom_${COMP}_$T.log)"; fail=1; fi

# 5. MIRROR - hashes match the fixed source; changed sources -> changed mirror
if [[ -f "$LDIR/mirrored_sources.sha256" ]]; then
  if (cd "$CDIR" && sha256sum -c --quiet "$LDIR/mirrored_sources.sha256" >/dev/null 2>&1); then
    listed="$(awk '{print $2}' "$LDIR/mirrored_sources.sha256")"; changed_src=0
    for f in $listed; do git -C "$REPO" diff --quiet "$BASE" -- "components/$COMP/$f" || changed_src=1; done
    if (( changed_src )) && git -C "$REPO" diff --quiet "$BASE" -- "components/$COMP/verif-loom/src" "components/$COMP/verif-loom/tests"; then
      echo "  5. MIRROR    : NO - a mirrored source file changed but the mirror did not"; fail=1
    else echo "  5. MIRROR    : yes (mirrored_sources.sha256 matches the fixed source)"; fi
  else echo "  5. MIRROR    : NO - mirrored_sources.sha256 does not match the fixed source: re-sync the mirror"; fail=1; fi
else echo "  5. MIRROR    : NO - no verif-loom/mirrored_sources.sha256"; fail=1; fi

(( fail )) && { echo "ACCEPT-LOOM: FAIL"; exit 1; }
echo "ACCEPT-LOOM: PASS"
