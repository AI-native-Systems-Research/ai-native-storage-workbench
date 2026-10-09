#!/usr/bin/env bash
# Install the FV skills into a target repository (e.g. a Certus checkout), so Claude Code finds them there.
#   usage: fv/install.sh <target-repo> [--link]
#   default: copy the skills into <target-repo>/.claude/skills/ (a skill of the same name is replaced)
#   --link : symlink them instead, so edits in the workbench are seen immediately
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
target="${1:?usage: fv/install.sh <target-repo> [--link]}"; mode="${2:-copy}"
[ -d "$target/.git" ] || [ -f "$target/.git" ] || { echo "not a git checkout: $target" >&2; exit 1; }
dest="$target/.claude/skills"; mkdir -p "$dest"
version="$(git -C "$here" rev-parse --short HEAD 2>/dev/null || echo unknown)"
for s in "$here"/skills/*/; do
  name="$(basename "$s")"; rm -rf "${dest:?}/$name"
  if [ "$mode" = "--link" ]; then ln -s "$s" "$dest/$name"; else cp -r "$s" "$dest/$name"; fi
  echo "installed $name"
done
echo "$version" > "$dest/.fv-workbench-version"
echo "FV skills @ $version installed into $dest ($mode). The gate stamps its own commit into every result."
