# layout.sh - sourced by the repair checks. Components were published in two layouts (both are on the verif branches):
#   NEW: components/<c>/verif-creusot/ is the Creusot crate; components/<c>/verif/ holds only the bundle and pages
#   OLD: components/<c>/verif/ is BOTH the Creusot crate (it has a Cargo.toml) and the bundle
# creusot_crate <component dir> prints the crate's directory name for that component (default: the new layout).
creusot_crate() {
  if [[ -d "$1/verif-creusot" ]]; then echo verif-creusot
  elif [[ -f "$1/verif/Cargo.toml" ]]; then echo verif
  else echo verif-creusot; fi
}
