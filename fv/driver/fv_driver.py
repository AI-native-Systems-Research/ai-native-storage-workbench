#!/usr/bin/env python3
"""fv driver - runs the FV pipeline steps in a fixed order, checks each step's result in code, and stops honestly.

Why a driver (2026-10-09): most of the project's lost time was orchestration, not reasoning - provenance lost
because a job used the wrong skills copy, a step skipped, results not checked before the next job. Prose
instructions are a weak way to enforce a sequence; this file is the sequence. LLM judgement (reading code,
classifying, writing proofs) still runs as Claude agents; every verdict still comes from the gate scripts.

Rules it enforces itself (not left to an agent):
  - the target checkout must be on a LOCAL branch (never a detached published commit), and is never pushed;
  - the gate is always the one shipped next to this driver (fv/skills/component-verify/gate), by absolute path;
  - two classifier agents run independently (separate processes, no shared context);
  - each agent may write only the file it was asked for - anything else it touched is reported and the step fails;
  - every run writes a provenance record (driver commit, gate commit, prompts, agent outputs, timings).

usage:
  python3 fv_driver.py classify <checkout> <component> [--dry-run] [--model M] [--timeout S]
  python3 fv_driver.py check    <checkout> <component>
"""
import argparse, hashlib, json, os, subprocess, sys, time
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
FV = HERE.parent
GATE = FV / "skills" / "component-verify" / "gate"
PROMPTS = HERE / "prompts"


# ---------------------------------------------------------------------------------------------------- helpers
def sh(cmd, cwd=None, check=True, timeout=None):
    p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout)
    if check and p.returncode != 0:
        raise SystemExit(f"FAILED ({p.returncode}): {' '.join(map(str, cmd))}\n{p.stdout[-2000:]}\n{p.stderr[-2000:]}")
    return p


def git(repo, *args, check=True):
    return sh(["git", "-C", str(repo), *args], check=check).stdout.strip()


def commit_of(path):
    return git(path, "rev-parse", "--short", "HEAD", check=False) or "unknown"


def changed_files(repo):
    """Tracked modifications + untracked files, relative to the repo root."""
    out = git(repo, "status", "--porcelain", "--untracked-files=all", check=False)
    return sorted(line[3:].strip() for line in out.splitlines() if line.strip())


class Run:
    """Provenance + log for one driver invocation, written to <verif>/.run/driver/<stamp>/."""

    def __init__(self, verif, step):
        stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        self.dir = Path(verif) / ".run" / "driver" / f"{stamp}-{step}"
        self.dir.mkdir(parents=True, exist_ok=True)
        self.rec = {"step": step, "started": stamp, "driver_commit": commit_of(FV), "gate_dir": str(GATE),
                    "gate_commit": commit_of(GATE), "events": []}

    def event(self, what, **kw):
        kw.update(what=what, t=round(time.time(), 1))
        self.rec["events"].append(kw)
        print(f"  [{what}] " + " ".join(f"{k}={v}" for k, v in kw.items() if k not in ("what", "t")), flush=True)

    def save(self, outcome):
        self.rec["outcome"] = outcome
        self.rec["finished"] = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        (self.dir / "provenance.json").write_text(json.dumps(self.rec, indent=2, default=str))
        print(f"provenance: {self.dir / 'provenance.json'}")


def agent(prompt, cwd, allowed_tools, model=None, timeout=1800, log=None):
    """Run ONE Claude agent headless (claude -p) and return (ok, text, seconds).
    Kept in one place on purpose: switching to the Claude Agent SDK later changes only this function."""
    cmd = ["claude", "-p", "--output-format", "json", "--permission-mode", "acceptEdits",
           "--allowedTools", ",".join(allowed_tools)]
    if model:
        cmd += ["--model", model]
    t0 = time.time()
    try:
        # the prompt goes on stdin: --allowedTools takes several values and would swallow a trailing argument
        p = subprocess.run(cmd, input=prompt, cwd=str(cwd), capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return False, f"timed out after {timeout} s", round(time.time() - t0)
    dt = round(time.time() - t0)
    text = p.stdout
    try:
        env = json.loads(p.stdout)
        text = str(env.get("result", p.stdout)) if isinstance(env, dict) else p.stdout
    except ValueError:
        pass
    if log:
        Path(log).write_text(f"$ {' '.join(cmd)} <prompt>\nrc={p.returncode} seconds={dt}\n\n{text}\n\nSTDERR:\n{p.stderr[-4000:]}")
    return p.returncode == 0, text, dt


def preflight(checkout, component):
    checkout = Path(checkout).resolve()
    branch = git(checkout, "branch", "--show-current", check=False)
    if not branch:
        raise SystemExit(f"{checkout} is on a detached commit. Create a local branch first (git switch -c ...): "
                         "the driver never edits a published commit.")
    verif = checkout / "components" / component / "verif"
    if not (verif / "unified_properties.yaml").exists():
        raise SystemExit(f"no bundle at {verif}/unified_properties.yaml")
    if not GATE.is_dir():
        raise SystemExit(f"gate not found next to the driver: {GATE}")
    return checkout, verif, branch


def gate(script, *args, cwd=None):
    return sh([sys.executable, str(GATE / script), *map(str, args)], cwd=cwd, check=False)


# ---------------------------------------------------------------------------------------------------- steps
def step_check(checkout, component, quiet=False):
    checkout, verif, _ = preflight(checkout, component)
    p = gate("check_done.py", verif)
    if not quiet:
        print(p.stdout.strip())
    return p.returncode == 0, p.stdout


def step_classify(a):
    """Level-1 classifier: two independent agents -> classify_merge -> level1 -> discordance page."""
    checkout, verif, branch = preflight(a.checkout, a.component)
    run = Run(verif, "classify")
    run.event("start", component=a.component, branch=branch, checkout_commit=commit_of(checkout))
    import yaml
    d = yaml.safe_load(open(verif / "unified_properties.yaml"))
    cands = [p for p in d.get("properties", []) if p.get("verifiable")
             and str(p.get("origin", "")) in ("divergent", "spec-only")]
    run.event("candidates", n=len(cands))
    if not cands:
        run.event("nothing to classify")
        run.save("no candidates")
        return 0
    before = set(changed_files(checkout))
    src = checkout / "components" / a.component / "src"
    rubric = GATE / "classify_rubric.yaml"
    template = (PROMPTS / "classifier.md").read_text()
    outs = []
    for n in (1, 2):
        out = verif / ".run" / f"classify_run{n}.yaml"
        prompt = template.format(component=a.component, rubric=rubric, bundle=verif / "unified_properties.yaml",
                                 src=src, interfaces=checkout / "components" / "interfaces" / "src",
                                 out=out, order="first to last" if n == 1 else "LAST to first")
        (run.dir / f"prompt_run{n}.md").write_text(prompt)
        run.rec[f"prompt_run{n}_sha256"] = hashlib.sha256(prompt.encode()).hexdigest()[:16]
        if a.dry_run:
            run.event("dry-run", agent=n, would_write=out)
            continue
        ok, text, dt = agent(prompt, checkout, ["Read", "Grep", "Glob", "Write"], model=a.model,
                             timeout=a.timeout, log=run.dir / f"agent_run{n}.log")
        run.event("agent", n=n, ok=ok, seconds=dt)
        if not ok or not out.exists():
            run.event("FAIL", reason=f"classifier {n} produced no output", detail=text[-300:])
            run.save("failed: classifier output missing")
            return 1
        outs.append(out)
    if a.dry_run:
        run.save("dry run")
        return 0
    # an agent may write only its own output file
    stray = [f for f in set(changed_files(checkout)) - before if not f.endswith(("classify_run1.yaml", "classify_run2.yaml"))
             and "/.run/" not in f]
    if stray:
        run.event("FAIL", reason="agent wrote outside its output file", files=stray[:10])
        run.save("failed: stray writes")
        return 1
    for script, args in (("classify_merge.py", [verif, *outs]), ("level1.py", [verif]),
                         ("render_discordances.py", [verif])):
        p = gate(script, *args)
        (run.dir / f"{script}.out").write_text(p.stdout + p.stderr)
        run.event(script, rc=p.returncode, tail=p.stdout.strip().splitlines()[-1][:120] if p.stdout.strip() else "")
        if p.returncode != 0:
            run.save(f"failed: {script}")
            return 1
    ok, out = step_check(a.checkout, a.component, quiet=True)
    run.event("check_done", done=ok, detail=" | ".join(out.strip().splitlines()[1:4]))
    run.save("classified" + (" - DONE" if ok else " - A rows still to prove"))
    print(out.strip())
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("classify")
    c.add_argument("checkout"); c.add_argument("component")
    c.add_argument("--dry-run", action="store_true"); c.add_argument("--model", default=None)
    c.add_argument("--timeout", type=int, default=1800)
    k = sub.add_parser("check")
    k.add_argument("checkout"); k.add_argument("component")
    a = ap.parse_args()
    if a.cmd == "check":
        ok, _ = step_check(a.checkout, a.component)
        return 0 if ok else 1
    return step_classify(a)


if __name__ == "__main__":
    sys.exit(main())
