#!/usr/bin/env python3
"""fv agent - verifies one component end to end and stops honestly.

    python3 fv/agent/fv_agent.py verify  <checkout> <component> [--rounds 2] [--tools creusot,kani] [--model M]
    python3 fv/agent/fv_agent.py publish <checkout> <component> [--date YYYYMMDD]

The SEQUENCE is code (fv/driver): extract -> sync -> classify -> prove (each tool) -> check. The agent's own judgement
is used in exactly one place, the SUPERVISOR: when check_done is not DONE, one Claude call chooses ONE action from a
fixed menu - retry named ids for one tool, or stop with a reason for a person. It cannot skip a step, write a status,
edit the gate, push, or touch production code; the driver enforces all of that. Rounds are bounded.

publish creates the two LOCAL verif branches (verif/creusot/<c>-<date>, verif/kani/<c>-<date>) from the checkout's
base commit, each with the bundle + pages and that tool's proof crate, component-only. It never pushes.

Runs on Python 3.9 with no dependencies beyond PyYAML; agents run as headless Claude Code (`claude -p`).
"""
import argparse, json, subprocess, sys, time
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "driver"))
import fv_driver as D  # noqa: E402

MENU = """Reply with ONE JSON object and nothing else, one of:
  {"action": "retry", "tool": "creusot"|"kani", "ids": ["<id>", ...], "why": "<one sentence>"}
  {"action": "stop", "why": "<one sentence a person can act on>"}
Choose "retry" only for properties that an agent could still prove within the rules (e.g. an agent ran out of time,
or wrote no proof module). Choose "stop" when what remains needs a person: a precedence question for the spec owners,
an environment or build failure, a proof that would need an undeclared assumption, or a tool limit already measured."""


def sh(cmd, cwd=None, timeout=None):
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout)


def drv(*args, timeout=None):
    """Run one driver step as a subprocess (its own provenance record); return (rc, output)."""
    p = sh([sys.executable, str(D.HERE / "fv_driver.py"), *map(str, args)], timeout=timeout)
    out = (p.stdout + p.stderr).strip()
    print(out, flush=True)
    return p.returncode, out


def log(msg):
    print(f"[fv-agent {datetime.now().strftime('%H:%M:%S')}] {msg}", flush=True)


def open_rows(checkout, component):
    """(id, tool, status, note) of every level-2 property a tool has not closed."""
    import yaml
    verif = Path(checkout) / "components" / component / "verif"
    d = yaml.safe_load(open(verif / "unified_properties.yaml"))
    ex = set(d.get("level2_excluded") or [])
    rows = []
    for p in d["properties"]:
        if not p.get("verifiable") or p["id"] in ex:
            continue
        for t in ("creusot", "kani"):
            b = p.get(t) or {}
            if not b.get("_scored_by") or b.get("status") not in ("proved", "refuted", "delegated", "tool-boundary"):
                rows.append({"id": p["id"], "tool": t, "status": b.get("status") or "unscored",
                             "note": str(b.get("note") or "")[:300]})
    return rows


def supervise(checkout, component, report, history=(), model=None, timeout=900):
    """One bounded judgement call: what to do about the check_done report."""
    open_rows_ = open_rows(checkout, component)
    past = "\n".join(f"- round {h['round']}: {h['decision'].get('action')} {h['decision'].get('tool', '')} "
                     f"{len(h['decision'].get('ids') or [])} ids -> {h['closed']} of them closed" for h in history)
    prompt = (f"You supervise the formal verification of `{component}`. The definition-of-done check says:\n\n"
              f"{report}\n\nThe properties still open (tool, status, the scorer's note):\n"
              f"{json.dumps(open_rows_[:80], indent=1)}\n\n"
              + (f"Earlier rounds:\n{past}\nDo not repeat a retry that closed nothing.\n\n" if past else "")
              + MENU)
    ok, text, dt = D.agent(prompt, checkout, ["Read", "Grep", "Glob"], model=model, timeout=timeout)
    try:
        j = json.loads(text[text.index("{"): text.rindex("}") + 1])
    except (ValueError, json.JSONDecodeError):
        return {"action": "stop", "why": f"supervisor reply was not valid JSON: {text[:200]}"}
    if j.get("action") == "retry":
        known = {r["id"] for r in open_rows_ if r["tool"] == j.get("tool")}
        j["ids"] = [i for i in j.get("ids") or [] if i in known]
        if j.get("tool") not in ("creusot", "kani") or not j["ids"]:
            return {"action": "stop", "why": "supervisor chose a retry with no valid ids/tool"}
    elif j.get("action") != "stop":
        return {"action": "stop", "why": f"supervisor chose an action outside the menu: {j.get('action')}"}
    return j


def cmd_verify(a):
    co, c = Path(a.checkout).resolve(), a.component
    verif = co / "components" / c / "verif"
    tools = a.tools.split(",")
    t0 = time.time()
    record = {"component": c, "checkout": str(co), "started": datetime.now(timezone.utc).isoformat(), "steps": []}

    def step(name, *args, timeout=None):
        log(f"step {name}")
        rc, out = drv(*args, timeout=timeout)
        record["steps"].append({"step": name, "rc": rc, "tail": out.splitlines()[-1] if out else ""})
        return rc
    if not (verif / "unified_properties.yaml").exists():
        if step("extract", "extract", co, c, *(["--model", a.model] if a.model else [])):
            return finish(record, verif, "stopped: extract failed", t0)
    for name in ("sync", "classify"):
        if step(name, name, co, c, *(["--model", a.model] if a.model else [])):
            return finish(record, verif, f"stopped: {name} failed", t0)
    for t in tools:
        if step(f"prove-{t}", "prove", co, c, "--tool", t, "--all", *(["--model", a.model] if a.model else [])):
            log(f"prove {t} did not complete cleanly - the supervisor will see what is open")
    history = []
    for r in range(1, a.rounds + 1):
        rc, report = drv("check", co, c)
        if rc == 0:
            return finish(record, verif, "DONE", t0)
        log(f"supervisor round {r}")
        j = supervise(co, c, report, history=history, model=a.model)
        if j["action"] == "retry" and any(h["decision"].get("tool") == j["tool"] and h["closed"] == 0
                                          and set(j["ids"]) <= set(h["decision"]["ids"]) for h in history):
            j = {"action": "stop", "why": f"no progress: an earlier {j['tool']} retry of these ids closed none of them"}
        record["steps"].append({"step": f"supervise-{r}", "decision": j})
        log(f"decision: {json.dumps(j)}")
        if j["action"] == "stop":
            return finish(record, verif, f"stopped by supervisor: {j['why']}", t0)
        before = {(x["id"], x["tool"]) for x in open_rows(co, c)}
        step(f"retry-{j['tool']}-{r}", "prove", co, c, "--tool", j["tool"], "--ids", ",".join(j["ids"]))
        after = {(x["id"], x["tool"]) for x in open_rows(co, c)}
        closed = sum(1 for i in j["ids"] if (i, j["tool"]) in before and (i, j["tool"]) not in after)
        history.append({"round": r, "decision": j, "closed": closed})
        log(f"round {r}: {closed} of {len(j['ids'])} retried ids closed")
    rc, report = drv("check", co, c)
    return finish(record, verif, "DONE" if rc == 0 else f"stopped: {a.rounds} supervisor rounds used", t0)


def finish(record, verif, outcome, t0):
    record["outcome"] = outcome
    record["seconds"] = round(time.time() - t0)
    p = verif / ".run" / f"fv_agent_{datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')}.json"
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(record, indent=2, default=str))
    log(f"OUTCOME: {outcome}  ({record['seconds']} s; record {p})")
    return 0 if outcome == "DONE" else 1


def cmd_publish(a):
    """Two LOCAL verif branches from the checkout's base, component-only. Never pushes."""
    co, c = Path(a.checkout).resolve(), a.component
    date = a.date or datetime.now().strftime("%Y%m%d")
    base = sh(["git", "-C", str(co), "merge-base", "HEAD", a.base]).stdout.strip()
    if not base:
        print(f"cannot find the base commit against {a.base}"); return 1
    comp = co / "components" / c
    for tool, crate in (("creusot", "verif-creusot"), ("kani", "verif-kani")):
        br = f"verif/{tool}/{c}-{date}"
        wt = Path(f"/tmp/fv-publish-{c}-{tool}")
        sh(["git", "-C", str(co), "worktree", "remove", "--force", str(wt)])
        p = sh(["git", "-C", str(co), "worktree", "add", "-q", "-b", br, str(wt), base])
        if p.returncode:
            print(p.stderr); return 1
        paths = [f"components/{c}/verif"] + ([f"components/{c}/{crate}"] if (comp / crate).exists() else [])
        for rel in paths:
            sh(["rsync", "-a", "--delete", "--exclude", ".run", "--exclude", "target", "--exclude", "*_advisory.yaml",
                f"{co / rel}/", f"{wt / rel}/"])
        sh(["git", "-C", str(wt), "add", "-A", f"components/{c}"])
        names = sh(["git", "-C", str(wt), "diff", "--cached", "--name-only"]).stdout.split()
        stray = [n for n in names if not n.startswith(f"components/{c}/")]
        if stray or not names:
            print(f"{br}: refused ({'stray files' if stray else 'nothing to commit'}) {stray[:5]}"); return 1
        sh(["git", "-C", str(wt), "commit", "-q", "-m",
            f"verif({tool}): {c} - fv agent result\n\nPublished by fv/agent/fv_agent.py publish (local branch; not pushed)."
            f"\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"])
        print(f"{br}: {sh(['git', '-C', str(wt), 'rev-parse', '--short', 'HEAD']).stdout.strip()} ({len(names)} files)")
    print("local branches only - push them yourself when ready")
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    v = sub.add_parser("verify")
    v.add_argument("checkout"); v.add_argument("component")
    v.add_argument("--rounds", type=int, default=2); v.add_argument("--tools", default="creusot,kani")
    v.add_argument("--model", default=None)
    p = sub.add_parser("publish")
    p.add_argument("checkout"); p.add_argument("component")
    p.add_argument("--date", default=None); p.add_argument("--base", default="origin/unstable")
    a = ap.parse_args()
    return cmd_verify(a) if a.cmd == "verify" else cmd_publish(a)


if __name__ == "__main__":
    sys.exit(main())
