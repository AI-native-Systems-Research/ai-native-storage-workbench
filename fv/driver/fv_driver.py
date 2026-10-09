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
    # NOT git(): its .strip() would eat the first line's leading status space and shift every path by one
    out = sh(["git", "-C", str(repo), "status", "--porcelain", "--untracked-files=all"], check=False).stdout
    return sorted(line[3:] for line in out.splitlines() if len(line) > 3)


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
    except subprocess.TimeoutExpired as e:
        dt = round(time.time() - t0)
        if log:   # keep whatever the agent printed - a timeout is exactly when the log matters most
            Path(log).write_text(f"$ {' '.join(cmd)} <prompt>\nTIMED OUT after {timeout} s\n\n"
                                 f"{(e.stdout or b'').decode(errors='replace') if isinstance(e.stdout, bytes) else (e.stdout or '')}")
        return False, f"timed out after {timeout} s", dt
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


def reprove_map(d):
    """level2_reprove as {tool: [ids]} (an older plain list means: every tool)."""
    rp = d.get("level2_reprove") or {}
    if isinstance(rp, list):
        return {"creusot": list(rp), "kani": list(rp)}
    return {t: list(v or []) for t, v in rp.items()}


AGREED = ("spec+code", "both", "paired")
DD_KEYS = ("spec_says", "code_does", "code_pointers", "methods", "assume", "assume_rust")


def validate_sync(rep, d):
    """Return a list of problems with a sync report; empty = apply it."""
    P = {p.get("id"): p for p in d.get("properties", [])}
    errs = []
    for r in rep.get("reclassify_divergent") or []:
        pid = (r or {}).get("id")
        if pid not in P:
            errs.append(f"reclassify: unknown id {pid}")
        elif str(P[pid].get("origin")) not in AGREED:
            errs.append(f"reclassify: {pid} is origin {P[pid].get('origin')}, not an agreed (spec+code) record")
        elif not str(r.get("divergence_note", "")).strip():
            errs.append(f"reclassify: {pid} has no divergence_note")
    for i, dd in enumerate(rep.get("domain_discordances") or []):
        miss = [k for k in DD_KEYS if not dd.get(k)]
        if miss:
            errs.append(f"code assumption #{i + 1}: missing {miss}")
    return errs


def step_sync(a):
    """Level-1 sync check: one agent reports -> the driver validates and applies it -> level1 -> page."""
    checkout, verif, branch = preflight(a.checkout, a.component)
    run = Run(verif, "sync")
    run.event("start", component=a.component, branch=branch, checkout_commit=commit_of(checkout))
    import yaml
    bundle = verif / "unified_properties.yaml"
    d = yaml.safe_load(open(bundle))
    comp_dir = checkout / "components" / a.component
    out = verif / ".run" / "sync_check.yaml"
    prompt = (PROMPTS / "sync.md").read_text().format(
        component=a.component, out=out, bundle=bundle, specs=comp_dir / "specs", src=comp_dir / "src",
        interfaces=checkout / "components" / "interfaces" / "src", spec_props=verif / "spec_properties.yaml",
        code_props=verif / "code_properties.yaml",
        skill=FV / "skills" / "build-property-inventory" / "SKILL.md")
    (run.dir / "prompt_sync.md").write_text(prompt)
    run.rec["prompt_sync_sha256"] = hashlib.sha256(prompt.encode()).hexdigest()[:16]
    if a.dry_run:
        run.event("dry-run", would_write=out)
        run.save("dry run")
        return 0
    before = set(changed_files(checkout))
    ok, text, dt = agent(prompt, checkout, ["Read", "Grep", "Glob", "Write"], model=a.model,
                         timeout=a.timeout, log=run.dir / "agent_sync.log")
    run.event("agent", ok=ok, seconds=dt)
    stray = [f for f in set(changed_files(checkout)) - before if "/.run/" not in f]
    if stray:
        run.event("FAIL", reason="agent wrote outside its output file", files=stray[:10])
        run.save("failed: stray writes")
        return 1
    if not ok or not out.exists():
        run.event("FAIL", reason="no sync report", detail=text[-300:])
        run.save("failed: no report")
        return 1
    try:
        rep = yaml.safe_load(open(out)) or {}
    except yaml.YAMLError as e:
        run.event("FAIL", reason=f"report does not parse: {e}")
        run.save("failed: bad report")
        return 1
    errs = validate_sync(rep, d)
    if errs:
        for e in errs:
            run.event("invalid", problem=e)
        run.save("failed: report invalid")
        return 1
    # APPLY - the driver is the single writer of the bundle
    P = {p.get("id"): p for p in d.get("properties", [])}
    for r in rep.get("reclassify_divergent") or []:
        P[r["id"]]["origin"] = "divergent"
        P[r["id"]]["divergence_note"] = r["divergence_note"]
    existing = {str(x.get("assume_rust")).strip() for x in d.get("domain_discordances") or []}
    new = [x for x in rep.get("domain_discordances") or [] if str(x["assume_rust"]).strip() not in existing]
    d["domain_discordances"] = list(d.get("domain_discordances") or []) + new
    yaml.safe_dump(d, open(bundle, "w"), sort_keys=False, allow_unicode=True, width=110)
    run.event("applied", reclassified=len(rep.get("reclassify_divergent") or []), new_code_assumptions=len(new))
    for script in ("level1.py", "render_discordances.py"):
        p = gate(script, verif)
        (run.dir / f"{script}.out").write_text(p.stdout + p.stderr)
        run.event(script, rc=p.returncode, tail=p.stdout.strip().splitlines()[-1][:120] if p.stdout.strip() else "")
        if p.returncode != 0:
            run.save(f"failed: {script}")
            return 1
    # properties now under a new code assumption must be re-proved under exactly it (the prove step)
    meths = {m for x in new for m in x.get("methods") or []}
    # PER TOOL: a property is stale for every tool that had proved it before the assumption existed
    dep = {t: [p["id"] for p in d["properties"] if p.get("verifiable") and set(p.get("methods") or []) & meths
               and (p.get(t) or {}).get("status") == "proved"] for t in ("creusot", "kani")}
    run.rec["dependents_to_reprove"] = dep
    if any(dep.values()):   # recorded in the bundle so check_done cannot say DONE until the prove step clears it
        d2 = yaml.safe_load(open(bundle))
        rp = reprove_map(d2)
        for t, ids in dep.items():
            rp[t] = sorted(set(rp.get(t, [])) | set(ids))
        d2["level2_reprove"] = {t: v for t, v in rp.items() if v}
        yaml.safe_dump(d2, open(bundle, "w"), sort_keys=False, allow_unicode=True, width=110)
    run.event("dependents", creusot=len(dep["creusot"]), kani=len(dep["kani"]))
    ok, outtxt = step_check(a.checkout, a.component, quiet=True)
    run.save("synced" + (" - DONE" if ok else ""))
    print(outtxt.strip())
    return 0


TOOLS = {
    "creusot": {"name": "Creusot", "skill": "tools-verify-creusot-with-properties", "scorer": "scorer_creusot.py",
                "dir_flag": "--crate-dir", "crate": "verif-creusot", "caps": ["--cap-seconds", "60", "--cap-max", "300"]},
    "kani": {"name": "Kani", "skill": "tools-verify-kani-with-properties", "scorer": "scorer_kani.py",
             "dir_flag": "--component-dir", "crate": "verif-kani", "caps": ["--cap-seconds", "60", "--cap-max", "600"]},
}
CREUSOT_STD = os.environ.get("FV_CREUSOT_STD", os.path.expanduser("~/ai-native-storage-certus/tools/creusot/creusot"))


def step_prove(a):
    """Prove the A rows and the re-prove list for ONE tool: agent writes artifacts -> driver folds its advisory ->
    scorer re-runs exactly those ids -> cross_check -> pages -> re-prove list cleared for what was scored."""
    import yaml
    checkout, verif, branch = preflight(a.checkout, a.component)
    T = TOOLS[a.tool]
    comp_dir = checkout / "components" / a.component
    crate = comp_dir / T["crate"]
    if not crate.is_dir():
        crate = comp_dir          # older Kani layout: harnesses inside the component itself
    run = Run(verif, f"prove-{a.tool}")
    run.event("start", component=a.component, tool=a.tool, branch=branch, crate=crate, checkout_commit=commit_of(checkout))
    bundle = verif / "unified_properties.yaml"
    d = yaml.safe_load(open(bundle))
    P = {p["id"]: p for p in d["properties"]}
    cls = d.get("classification") or {}
    ex = set(d.get("level2_excluded") or [])
    a_rows = [k for k, v in cls.items() if v.get("class") == "A" and k in P and k not in ex]   # always re-scored
    rp = reprove_map(d).get(a.tool, [])
    ids = sorted(set(a.ids.split(",")) if a.ids else set(a_rows) | set(rp))
    ids = [i for i in ids if i in P and i not in ex]
    run.event("work", a_rows=len(a_rows), reprove=len(rp), total=len(ids))
    if not ids:
        run.save("nothing to prove")
        return 0
    if a.tool == "creusot" and not (comp_dir / "creusot").exists() and Path(CREUSOT_STD).exists():
        os.symlink(CREUSOT_STD, comp_dir / "creusot")          # machine-local creusot-std link (git-excluded)
        run.event("linked creusot-std", target=CREUSOT_STD)
    advisory = verif / f"{a.tool}_advisory.yaml"
    assumptions = "\n".join(f"- {x.get('id')}: {x.get('assume')}  [assume_rust: {x.get('assume_rust')}]"
                            for x in d.get("level2_assumptions") or []) or "(none)"
    idlines = "\n".join(f"- {i}{'   (RE-PROOF under a new code assumption)' if i in rp else ''}" for i in ids)
    where = (f"{crate}" if crate != comp_dir else
             f"{comp_dir}/src/verification.rs ONLY (the #[cfg(kani)] harness file; every other file under src/ is "
             f"production code you must not touch)")
    prompt = (PROMPTS / "prove.md").read_text().format(
        tool_name=T["name"], component=a.component, skill=FV / "skills" / T["skill"] / "SKILL.md", crate=where,
        advisory=advisory, bundle=bundle, ids=idlines, assumptions=assumptions)
    (run.dir / "prompt_prove.md").write_text(prompt)
    run.rec["prompt_sha256"] = hashlib.sha256(prompt.encode()).hexdigest()[:16]
    run.rec["ids"] = ids
    if a.dry_run:
        run.event("dry-run", ids=len(ids))
        run.save("dry run")
        return 0
    before = set(changed_files(checkout))
    if a.score_only:   # resume: the agent's artifacts are already in the crate
        run.event("score-only", note="agent not re-run")
    else:
        ok, text, dt = agent(prompt, checkout, ["Read", "Grep", "Glob", "Write", "Edit", "Bash"], model=a.model,
                             timeout=a.timeout, log=run.dir / "agent_prove.log")
        run.event("agent", ok=ok, seconds=dt)
        if not ok:   # STOP before folding: folding strips the tool's statuses, which only a real scoring may replace
            run.event("FAIL", reason="agent did not finish", detail=text[-300:])
            run.save("failed: agent did not finish (statuses left untouched; resume with --score-only once the artifacts are there)")
            return 1
    # an agent may write only in its proof crate, its advisory file, run logs and the creusot-std link.
    # Older Kani layout (harnesses inside the component, e.g. logger): ONLY the harness file src/verification*.rs
    # may change - any other file under src/ is production code and fails the step.
    rel_crate = str(crate.relative_to(checkout)).rstrip("/") + "/"
    rel_adv = str(advisory.relative_to(checkout))
    def allowed(f):
        if f == rel_adv or "/.run/" in f or f.endswith("/creusot"):
            return True
        if crate != comp_dir:
            return f.startswith(rel_crate)
        return f.startswith(rel_crate + "src/verification") and f.endswith(".rs")
    stray = sorted(f for f in set(changed_files(checkout)) - before if not allowed(f))
    if stray:
        run.event("FAIL", reason="agent wrote outside the proof crate / advisory", files=stray[:10])
        run.save("failed: stray writes")
        return 1
    # FOLD the advisory for the listed ids (advisory fields only; an empty value never erases)
    adv = (yaml.safe_load(open(advisory)) or {}) if advisory.exists() else {}
    adv = adv.get("properties", adv)
    d = yaml.safe_load(open(bundle))
    P = {p["id"]: p for p in d["properties"]}
    for i in ids:
        old = P[i].get(a.tool) or {}
        blk = {"fidelity": old.get("fidelity"), "note": None, "evidence": None}
        v = adv.get(i) or {}
        for f in ("fidelity", "note", "delegate_to"):
            if v.get(f) is not None:
                blk[f] = v[f]
        ev = v.get("evidence") or {}
        keep = {k: ev[k] for k in ("modules", "module", "harness") if ev.get(k)}
        blk["evidence"] = keep or None
        P[i][a.tool] = blk                          # status stripped: only the scorer may write it back
    yaml.safe_dump(d, open(bundle, "w"), sort_keys=False, allow_unicode=True, width=110)
    run.event("folded advisory", ids=len(ids), with_entry=sum(1 for i in ids if i in adv))
    # SCORE exactly these ids with the gate shipped next to the driver
    t0 = time.time()
    p = sh([sys.executable, str(GATE / T["scorer"]), str(verif), T["dir_flag"], str(crate), *T["caps"],
            "--resume", "--only", ",".join(ids)], cwd=checkout, check=False, timeout=a.score_timeout)
    (run.dir / "scorer.out").write_text(p.stdout + p.stderr)
    summ = [l for l in p.stdout.splitlines() if l.startswith("SUMMARY")]
    run.event("scorer", rc=p.returncode, seconds=round(time.time() - t0), summary=summ[-1][9:] if summ else "none")
    if not summ:
        run.save("failed: scorer produced no SUMMARY")
        return 1
    if "ENVIRONMENT FAILURE" in p.stdout:
        run.event("FAIL", reason="scorer reports an environment failure (build/layout), not a proof result")
        run.save("failed: environment")
        return 1
    xc = gate("cross_check.py", verif)
    run.event("cross_check", rc=xc.returncode, tail=xc.stdout.strip().splitlines()[-1] if xc.stdout.strip() else "")
    for script in ("level1.py", "render_discordances.py", "render_scoring.py"):
        g = gate(script, verif)
        (run.dir / f"{script}.out").write_text(g.stdout + g.stderr)
    # CLEAR the re-prove list for this tool where the scorer now owns a status
    d = yaml.safe_load(open(bundle))
    P = {p["id"]: p for p in d["properties"]}
    rpm = reprove_map(d)
    left = [i for i in rpm.get(a.tool, []) if not (P.get(i, {}).get(a.tool) or {}).get("_scored_by")]
    rpm[a.tool] = left
    d["level2_reprove"] = {t: v for t, v in rpm.items() if v}
    if not d["level2_reprove"]:
        d.pop("level2_reprove")
    yaml.safe_dump(d, open(bundle, "w"), sort_keys=False, allow_unicode=True, width=110)
    st = collections_count([(P[i].get(a.tool) or {}).get("status") or "open" for i in ids])
    run.rec["results"] = st
    run.event("results", **st, reprove_left=len(left))
    ok_done, out = step_check(a.checkout, a.component, quiet=True)
    run.save(f"proved {st}" + (" - DONE" if ok_done else ""))
    print(out.strip())
    return 0 if xc.returncode == 0 else 1


def collections_count(xs):
    out = {}
    for x in xs:
        out[x] = out.get(x, 0) + 1
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    c = sub.add_parser("classify")
    c.add_argument("checkout"); c.add_argument("component")
    c.add_argument("--dry-run", action="store_true"); c.add_argument("--model", default=None)
    c.add_argument("--timeout", type=int, default=1800)
    s = sub.add_parser("sync")
    s.add_argument("checkout"); s.add_argument("component")
    s.add_argument("--dry-run", action="store_true"); s.add_argument("--model", default=None)
    s.add_argument("--timeout", type=int, default=2400)
    pv = sub.add_parser("prove")
    pv.add_argument("checkout"); pv.add_argument("component")
    pv.add_argument("--tool", choices=sorted(TOOLS), required=True)
    pv.add_argument("--ids", default=None, help="comma-separated ids (default: A rows + this tool's re-prove list)")
    pv.add_argument("--dry-run", action="store_true"); pv.add_argument("--model", default=None)
    pv.add_argument("--score-only", action="store_true", help="skip the agent; fold + score what is in the crate")
    pv.add_argument("--timeout", type=int, default=7200, help="agent time box, seconds")
    pv.add_argument("--score-timeout", type=int, default=14400, help="scorer time box, seconds")
    k = sub.add_parser("check")
    k.add_argument("checkout"); k.add_argument("component")
    a = ap.parse_args()
    if a.cmd == "prove":
        return step_prove(a)
    if a.cmd == "check":
        ok, _ = step_check(a.checkout, a.component)
        return 0 if ok else 1
    if a.cmd == "sync":
        return step_sync(a)
    return step_classify(a)


if __name__ == "__main__":
    sys.exit(main())
