# layout.sh - sourced by the repair checks. Components were published in two layouts (both are on the verif branches):
#   NEW: components/<c>/verif-creusot/ is the Creusot crate; components/<c>/verif/ holds only the bundle and pages
#   OLD: components/<c>/verif/ is BOTH the Creusot crate (it has a Cargo.toml) and the bundle
# creusot_crate <component dir> prints the crate's directory name for that component (default: the new layout).
creusot_crate() {
  if [[ -d "$1/verif-creusot" ]]; then echo verif-creusot
  elif [[ -f "$1/verif/Cargo.toml" ]]; then echo verif
  else echo verif-creusot; fi
}

# test_features <component dir> prints the cargo arguments the component's own tests need. Some components compile
# parts of themselves only with a `testing` feature (extended-metadata-store: FlushManager), so a test of those parts
# cannot even be built with default features. REPAIR_TEST_FEATURES overrides ("none" = default features).
test_features() {
  if [[ -n "${REPAIR_TEST_FEATURES:-}" ]]; then
    [[ "$REPAIR_TEST_FEATURES" == none ]] || echo "--features $REPAIR_TEST_FEATURES"
  elif sed -n '/^\[features\]/,/^\[/p' "$1/Cargo.toml" 2>/dev/null | grep -qE '^testing[[:space:]]*='; then
    echo "--features testing"
  fi
}
