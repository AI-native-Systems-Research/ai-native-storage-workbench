"""Print a Markdown table of the newest verif/<tool>/<component>[-<date>] branch of each component, from the
remote-tracking branches of a Certus checkout. The newest = the latest commit date (the name's date is not trusted).
Usage: python3 verif_branch_table.py <certus-checkout> [remote]   (run `git fetch <remote>` first)"""
import subprocess, sys, re, collections
co = sys.argv[1]; remote = sys.argv[2] if len(sys.argv) > 2 else "origin"
TOOLS = ("creusot", "kani", "loom", "spin")
COMPONENTS = set(subprocess.run(["git", "-C", co, "ls-tree", "-d", "--name-only", f"{remote}/unstable", "components/"],
                                capture_output=True, text=True, check=True).stdout.replace("components/", "").split())
out = subprocess.run(["git", "-C", co, "for-each-ref", f"refs/remotes/{remote}/verif/",
                      "--format=%(refname:short) %(committerdate:short) %(objectname:short)"],
                     capture_output=True, text=True, check=True).stdout.split("\n")
best = collections.defaultdict(dict)
for line in filter(None, out):
    ref, date, sha = line.split()
    m = re.match(rf"{remote}/verif/([^/]+)/(.+)$", ref)
    if not m or m.group(1) not in TOOLS: continue
    tool, name = m.groups()
    comp = re.sub(r"-\d{8}$", "", name)
    if comp not in COMPONENTS: continue   # a branch for one fix (e.g. <component>-stale-handle-fix), not a run
    cur = best[comp].get(tool)
    if cur is None or (date, ref) > (cur[0], cur[1]): best[comp][tool] = (date, ref.split("/", 1)[1], sha)
rows = ["| component | " + " | ".join(f"newest {t} branch" for t in TOOLS) + " |", "|---|" + "---|" * len(TOOLS)]
for comp in sorted(best):
    cells = []
    for t in TOOLS:
        b = best[comp].get(t)
        cells.append(f"`{b[1]}` ({b[0]}, {b[2]})" if b else "—")
    rows.append(f"| {comp} | " + " | ".join(cells) + " |")
print("\n".join(rows))
