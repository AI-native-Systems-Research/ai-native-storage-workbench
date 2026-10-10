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
import argparse, hashlib, json, os, re, subprocess, sys, time
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


FORBID_CMD = [r"\bgit\s+(push|commit|reset|checkout|switch|rebase)\b", r"cargo\s+creusot\s+clean",
              r"\brm\s+-rf\s+/(\s|$)", r"\bsudo\b"]


def hook_settings(policy, workdir, rundir, name):
    """Write this step's guard/stop policy and the Claude Code settings that attach our hooks (fv/driver/hooks.py)."""
    rundir = Path(rundir); rundir.mkdir(parents=True, exist_ok=True)
    pol = dict(policy); pol.setdefault("workdir", str(workdir)); pol.setdefault("max_stop_blocks", 3)
    pol["forbid_cmd"] = FORBID_CMD + list(pol.get("forbid_cmd") or [])
    pol["log"] = str(rundir / f"hooks_{name}.jsonl")
    pp = rundir / f"policy_{name}.json"; pp.write_text(json.dumps(pol, indent=2))
    hook = f"{sys.executable} {HERE / 'hooks.py'}"
    settings = {"hooks": {"PreToolUse": [{"matcher": "Write|Edit|MultiEdit|NotebookEdit|Bash|Read|Grep|Glob",
                                          "hooks": [{"type": "command", "timeout": 30, "command": f"{hook} guard {pp}"}]}]}}
    if pol.get("stop_check"):
        settings["hooks"]["Stop"] = [{"hooks": [{"type": "command", "timeout": 900,
                                                 "command": f"{hook} stop {pp} {rundir / f'stopstate_{name}.json'}"}]}]
    sp = rundir / f"settings_{name}.json"; sp.write_text(json.dumps(settings, indent=2))
    return sp


def selfcheck_cmd(kind, *args):
    return [sys.executable, str(HERE / "fv_driver.py"), "selfcheck", kind, *map(str, args)]


def agent(prompt, cwd, allowed_tools, model=None, timeout=1800, log=None, policy=None, name="agent"):
    """Run ONE Claude agent headless (claude -p) and return (ok, text, seconds). With a policy, our guard hook checks
    every tool call as it happens and our stop hook runs the step's cheap check before the agent may finish.
    Kept in one place on purpose: switching to the Claude Agent SDK later changes only this function."""
    cmd = ["claude", "-p", "--output-format", "json", "--permission-mode", "acceptEdits",
           "--allowedTools", ",".join(allowed_tools)]
    if policy is not None:
        cmd += ["--settings", str(hook_settings(policy, cwd, Path(log).parent if log else Path(cwd) / ".fv-hooks", name))]
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
        other = verif / ".run" / f"classify_run{3 - n}.yaml"
        ok, text, dt = agent(prompt, checkout, ["Read", "Grep", "Glob", "Write"], model=a.model,
                             timeout=a.timeout, log=run.dir / f"agent_run{n}.log", name=f"classify{n}",
                             policy={"writable": [str(out)], "hidden": [str(other)],
                                     "stop_check": selfcheck_cmd("classify", out, verif / "unified_properties.yaml")})
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
    import shutil, yaml
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
                         timeout=a.timeout, log=run.dir / "agent_sync.log", name="sync",
                         policy={"writable": [str(out)], "stop_check": selfcheck_cmd("yaml", out)})
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
        P[r["id"]].setdefault("origin_before_level1", P[r["id"]].get("origin"))   # audit: what reconcile said
        P[r["id"]]["origin"] = "divergent"
        P[r["id"]]["divergence_note"] = r["divergence_note"]
        P[r["id"]]["reclassified_by"] = run.dir.name
    shutil.copy2(out, run.dir / "sync_check.yaml")              # keep every pass's report, not only the last
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


def interface_methods(checkout, component):
    return interfaces(checkout, component)[0]


def interfaces(checkout, component):
    """(methods, factories). methods = the component's public methods: the fn names inside the define_interface! block of the interface it implements,
    plus those of any plain trait the component implements that an interface method hands out (`Box<dyn T>`)."""
    src = checkout / "components" / component / "src"
    impls = set()
    for f in src.rglob("*.rs"):
        impls |= set(re.findall(r"impl\s+(I[A-Z]\w*)\s+for\s+\w+", f.read_text(errors="replace")))

    def body(t, m):
        depth, i = 1, m.end()
        while i < len(t) and depth:
            depth += {"{": 1, "}": -1}.get(t[i], 0)
            i += 1
        return t[m.end():i]
    texts = [f.read_text(errors="replace") for f in (checkout / "components" / "interfaces" / "src").rglob("*.rs")]
    methods, handed_out, factories = [], set(), set()
    for t in texts:
        for name in impls:
            m = re.search(r"pub\s+" + re.escape(name) + r"\s*\{", t)
            if m:
                b = body(t, m)
                methods += re.findall(r"\bfn\s+([a-z_][a-z0-9_]*)\s*\(", b)
                for fn, ret in re.findall(r"\bfn\s+([a-z_][a-z0-9_]*)\s*\([^)]*\)\s*->\s*([^;{]*)", b):
                    got = set(re.findall(r"dyn\s+(I[A-Z]\w*)", ret)) & impls
                    if got:
                        handed_out |= got; factories.add(fn)
    for t in texts:
        for name in handed_out:
            m = re.search(r"pub\s+trait\s+" + re.escape(name) + r"\b[^{;]*\{", t)
            if m:
                methods += re.findall(r"\bfn\s+([a-z_][a-z0-9_]*)\s*\(", body(t, m))
    return sorted(dict.fromkeys(methods)), factories


def id_prefix(component):
    parts = component.split("-")
    return (parts[0][:3] if len(parts) == 1 else "".join(p[0] for p in parts)).upper()


def sandbox(checkout, component, side):
    """A throw-away directory holding ONLY one side - blindness enforced by what is on disk, not by a request."""
    import shutil, tempfile
    root = Path(tempfile.mkdtemp(prefix=f"fv-{component}-{side}-"))
    comp = checkout / "components" / component
    keep = [comp / "specs"] if side == "spec" else [comp / "src", comp / "Cargo.toml"]
    keep.append(checkout / "components" / "interfaces" / "src")     # the contracts both sides may see
    for k in keep:
        if k.exists():
            dst = root / k.relative_to(checkout)
            dst.parent.mkdir(parents=True, exist_ok=True)
            (shutil.copytree if k.is_dir() else shutil.copy2)(k, dst)
    shutil.copy2(FV / "skills" / "extract-verifiable-properties" / "SKILL.md", root / "EXTRACTION_SKILL.md")
    return root


def check_extraction(path):
    import yaml
    d = yaml.safe_load(open(path)) or {}
    props = d.get("properties") or []
    ids = [p.get("id") for p in props]
    c = d.get("counts") or {}
    errs = []
    if not props:
        errs.append("no records")
    if len(set(ids)) != len(ids):
        errs.append("duplicate ids")
    if c.get("total") not in (None, len(props)):
        errs.append(f"counts.total {c.get('total')} != {len(props)} records")
    nv = sum(1 for p in props if p.get("verifiable"))
    if c.get("verifiable") not in (None, nv):
        errs.append(f"counts.verifiable {c.get('verifiable')} != {nv}")
    if any(p.get("verifiable") and not str(p.get("statement", "")).strip() for p in props):
        errs.append("a verifiable record has no statement")
    return d, errs


def check_reconcile(unified, spec, code):
    """Every input id used in exactly one paired_from; no invented ids; every verifiable record has an origin."""
    sids = {p["id"] for p in spec.get("properties") or []}
    cids = {p["id"] for p in code.get("properties") or []}
    used = {"spec": [], "code": []}
    for p in unified.get("properties") or []:
        pf = p.get("paired_from") or p.get("derived_from") or {}
        for side in ("spec", "code"):
            v = pf.get(side) or []
            used[side] += v if isinstance(v, list) else [v]
    errs = []
    for side, ids in (("spec", sids), ("code", cids)):
        u = used[side]
        if set(u) - ids:
            errs.append(f"{side}: ids not in the input: {sorted(set(u) - ids)[:5]}")
        if ids - set(u):
            errs.append(f"{side}: {len(ids - set(u))} input ids never used (e.g. {sorted(ids - set(u))[0]})")
        dup = {x for x in u if u.count(x) > 1}
        if dup:
            errs.append(f"{side}: ids used more than once: {sorted(dup)[:5]}")
    bad = [p.get("id") for p in unified.get("properties") or [] if p.get("verifiable")
           and str(p.get("origin")) not in ("spec+code", "divergent", "spec-only", "code-only")]
    if bad:
        errs.append(f"records with no valid origin: {bad[:5]}")
    return errs


ORIGINS = ("spec+code", "divergent", "spec-only", "code-only", "not-verifiable")


def check_pairing(pairing, spec, code):
    """The reconcile agent's pairing table: every input id in exactly one group, no unknown id, sides consistent."""
    recs = {"spec": {p["id"]: p for p in spec.get("properties") or []},
            "code": {p["id"]: p for p in code.get("properties") or []}}
    groups = pairing.get("groups") if isinstance(pairing, dict) else None
    if not isinstance(groups, list) or not groups:
        return ["no `groups:` list"]
    errs, used = [], {"spec": [], "code": []}
    for n, g in enumerate(groups):
        if not isinstance(g, dict):
            errs.append(f"group {n}: not a mapping"); continue
        o = g.get("origin")
        sides = {s: g.get(s) or [] for s in ("spec", "code")}
        if o not in ORIGINS:
            errs.append(f"group {n}: origin {o!r} is not one of {ORIGINS}")
        if any(not isinstance(v, list) for v in sides.values()):
            errs.append(f"group {n}: spec/code must be lists"); continue
        for s, v in sides.items():
            used[s] += v
        if not sides["spec"] and not sides["code"]:
            errs.append(f"group {n}: empty")
        if o in ("spec+code", "divergent") and not (sides["spec"] and sides["code"]):
            errs.append(f"group {n}: {o} needs ids on both sides")
        if o == "spec-only" and sides["code"] or o == "code-only" and sides["spec"]:
            errs.append(f"group {n}: {o} has ids on the other side")
        if o == "divergent" and not str(g.get("note") or "").strip():
            errs.append(f"group {n}: divergent without a note")
        nv = [i for s, v in sides.items() for i in v if i in recs[s] and not recs[s][i].get("verifiable")]
        if o == "not-verifiable" and len(nv) != len(sides["spec"]) + len(sides["code"]):
            errs.append(f"group {n}: not-verifiable group holds a verifiable id")
        if o != "not-verifiable" and nv:
            errs.append(f"group {n}: verifiable group holds not-verifiable ids {nv[:3]}")
    for s in ("spec", "code"):
        u, ids = used[s], set(recs[s])
        if set(u) - ids:
            errs.append(f"{s}: ids not in the input: {sorted(set(u) - ids)[:5]}")
        if ids - set(u):
            errs.append(f"{s}: {len(ids - set(u))} input ids never used (e.g. {sorted(ids - set(u))[:3]})")
        dup = sorted({x for x in u if u.count(x) > 1})
        if dup:
            errs.append(f"{s}: ids used more than once: {dup[:5]}")
    return errs[:30]


def build_unified(pairing, spec, code, component, pin, methods, factories=()):
    """Unified list from a checked pairing table - statements, traces and methods copied, never rewritten."""
    recs = {"spec": {p["id"]: p for p in spec.get("properties") or []},
            "code": {p["id"]: p for p in code.get("properties") or []}}
    out, seen = [], set()

    def meths(rs):
        """Methods named in the records' subjects; if none, those named in their statements."""
        for field in ("subject", "statement"):
            found = [m for m in methods if any(re.search(rf"(?<![\w]){re.escape(m)}(?![\w])", str(r.get(field) or ""))
                                               for r in rs)]
            if len(found) > 1:                     # "node handle from create_node (shout)" is about shout
                found = [m for m in found if m not in factories] or found
            if found:
                return found
        return []
    for g in pairing["groups"]:
        sp = [recs["spec"][i] for i in g.get("spec") or []]
        cd = [recs["code"][i] for i in g.get("code") or []]
        first = (sp + cd)[0]
        rid = str(g.get("id") or first["id"])
        if rid in seen:
            rid = f"{rid}-{'CODE' if not sp else 'SPEC'}"
        k = 2
        while rid in seen:
            rid, k = f"{rid}-{k}", k + 1
        seen.add(rid)
        traces = []
        for r in sp + cd:
            traces += [t for t in r.get("traces") or [] if t not in traces]
        rec = {"id": rid, "origin": g["origin"], "kind": first.get("kind"),
               "verifiable": g["origin"] != "not-verifiable"}
        if rec["verifiable"]:
            rec["methods"] = meths(sp + cd)
            rec["statement"] = str(g.get("statement") or first.get("statement") or "").strip()
            if g["origin"] == "divergent":
                rec["divergence_note"] = str(g["note"]).strip()
                rec["code_statement"] = " ".join(str(r.get("statement") or "").strip() for r in cd)
        else:
            rec["scope"] = first.get("scope")
            rec["reason"] = " ".join(str(r.get("reason") or "").strip() for r in sp + cd)
        rec["traces"] = traces
        rec["paired_from"] = {"spec": [r["id"] for r in sp], "code": [r["id"] for r in cd]}
        out.append(rec)
    oc = collections_count([p["origin"] for p in out])
    return {"component": component, "pin": pin, "interface_methods": methods,
            "counts": {"total": len(out), "verifiable": sum(1 for p in out if p["verifiable"]), "origins": oc,
                       "spec_records": len(recs["spec"]), "code_records": len(recs["code"])},
            "reconciliation": {"method": "two blind readers; agent pairing table; driver-built records",
                               "pairing": "reconcile_pairing.yaml"},
            "properties": out}


def step_extract(a):
    """Role 1: two BLIND readers in separate sandboxes, then reconcile; the driver checks integrity."""
    import shutil, yaml
    checkout = Path(a.checkout).resolve()
    if not git(checkout, "branch", "--show-current", check=False):
        raise SystemExit("detached checkout: create a local branch first")
    verif = checkout / "components" / a.component / "verif"
    verif.mkdir(parents=True, exist_ok=True)
    run = Run(verif, "extract")
    methods, factories = interfaces(checkout, a.component)
    pin = commit_of(checkout)
    run.event("start", component=a.component, pin=pin, interface_methods=len(methods))
    if not methods:
        run.event("FAIL", reason="interface methods not found (no define_interface! block for an impl in src/)")
        run.save("failed: no interface")
        return 1
    outs = {}
    for side, contents, hint in (("spec", "the component's specs/ and the interface definitions", "FR / US / AS / SC ids"),
                                 ("code", "the component's src/, Cargo.toml and the interface definitions", "file:line")):
        prev = verif / f"{side}_properties.yaml"
        if prev.exists() and not a.fresh and not a.dry_run:
            d, errs = check_extraction(prev)
            if not errs and str(d.get("driver_pin")) == pin:     # same commit: the blind list is still valid
                outs[side] = d
                run.event("reused", side=side, records=len(d["properties"]), driver_pin=pin)
                continue
        box = sandbox(checkout, a.component, side)
        out = box / f"{side}_properties.yaml"
        prompt = (PROMPTS / "reader.md").read_text().format(
            side=side, component=a.component, contents=contents, skill=box / "EXTRACTION_SKILL.md",
            methods=", ".join(methods), prefix=id_prefix(a.component), trace_hint=hint, out=out)
        (run.dir / f"prompt_{side}.md").write_text(prompt)
        if a.dry_run:
            run.event("dry-run", side=side, sandbox=box, files=sum(1 for x in box.rglob("*") if x.is_file()))
            continue
        ok, text, dt = agent(prompt, box, ["Read", "Grep", "Glob", "Write"], model=a.model, timeout=a.timeout,
                             log=run.dir / f"agent_{side}.log", name=f"reader_{side}",
                             policy={"writable": [str(out)], "stop_check": selfcheck_cmd("extraction", out)})
        run.event("reader", side=side, ok=ok, seconds=dt)
        if not ok or not out.exists():
            run.save(f"failed: {side} reader produced nothing")
            return 1
        d, errs = check_extraction(out)
        if errs:
            for e in errs:
                run.event("invalid", side=side, problem=e)
            run.save(f"failed: {side} extraction invalid")
            return 1
        d["driver_pin"] = pin                                     # driver-owned: the commit this list was read at
        yaml.safe_dump(d, open(verif / f"{side}_properties.yaml", "w"), sort_keys=False, allow_unicode=True, width=110)
        outs[side] = d
        run.event("extracted", side=side, records=len(d["properties"]),
                  verifiable=sum(1 for p in d["properties"] if p.get("verifiable")))
        shutil.rmtree(box, ignore_errors=True)
    if a.dry_run:
        run.save("dry run")
        return 0
    out = verif / "unified_properties.yaml"
    pair = verif / "reconcile_pairing.yaml"
    prompt = (PROMPTS / "reconcile.md").read_text().format(
        component=a.component, out=pair, spec_props=verif / "spec_properties.yaml",
        code_props=verif / "code_properties.yaml", skill=FV / "skills" / "build-property-inventory" / "SKILL.md")
    (run.dir / "prompt_reconcile.md").write_text(prompt)
    before = set(changed_files(checkout))
    reuse = False
    if pair.exists() and not a.fresh:                       # a checked table from this commit is still valid
        try:
            prev = yaml.safe_load(open(pair)) or {}
            reuse = (isinstance(prev, dict) and str(prev.get("driver_pin")) == pin
                     and all(str(outs[s].get("driver_pin")) == pin for s in outs)
                     and not check_pairing(prev, outs["spec"], outs["code"]))
        except yaml.YAMLError:
            reuse = False
    if reuse:
        ok, dt = True, 0
        run.event("reused", side="pairing", groups=len(prev["groups"]), driver_pin=pin)
    else:
        ok, text, dt = agent(prompt, checkout, ["Read", "Grep", "Glob", "Write"], model=a.model, timeout=a.timeout,
                         log=run.dir / "agent_reconcile.log", name="reconcile",
                         policy={"writable": [str(pair)],
                                 "stop_check": selfcheck_cmd("reconcile", pair, verif / "spec_properties.yaml",
                                                             verif / "code_properties.yaml")})
        run.event("reconcile", ok=ok, seconds=dt)
    stray = [f for f in set(changed_files(checkout)) - before
             if not f.endswith(("reconcile_pairing.yaml", "spec_properties.yaml", "code_properties.yaml"))
             and "/.run/" not in f]
    if stray:
        run.event("FAIL", reason="reconcile agent wrote outside its output", files=stray[:10])
        run.save("failed: stray writes")
        return 1
    if not ok or not pair.exists():
        run.save("failed: no pairing table")
        return 1
    try:
        pairing = yaml.safe_load(open(pair)) or {}
    except yaml.YAMLError as e:
        run.event("invalid", problem=f"pairing does not parse: {e}")
        run.save("failed: pairing does not parse")
        return 1
    errs = check_pairing(pairing, outs["spec"], outs["code"])
    if errs:
        for e in errs:
            run.event("invalid", problem=e)
        run.save("failed: reconciliation integrity")
        return 1
    if str(pairing.get("driver_pin")) != pin:               # driver-owned stamp: checked at this commit
        pairing["driver_pin"] = pin
        yaml.safe_dump(pairing, open(pair, "w"), sort_keys=False, allow_unicode=True, width=110)
    u = build_unified(pairing, outs["spec"], outs["code"], a.component, pin, methods, factories)
    errs = check_reconcile(u, outs["spec"], outs["code"])          # belt and braces on the built list
    if errs:
        for e in errs:
            run.event("invalid", problem=e)
        run.save("failed: built list integrity")
        return 1
    yaml.safe_dump(u, open(out, "w"), sort_keys=False, allow_unicode=True, width=110)
    oc = u["counts"]["origins"]
    run.rec["origins"] = oc
    run.event("reconciled", records=len(u["properties"]), **{k.replace("+", "_").replace("-", "_"): v for k, v in oc.items()})
    run.save("extracted")
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
    tracked_harness = git(checkout, "ls-files", f"components/{a.component}/src/verification*.rs", check=False)
    if a.tool == "kani" and not crate.is_dir() and tracked_harness:
        crate = comp_dir          # older Kani layout: harnesses committed inside the component itself (e.g. logger)
    else:
        crate.mkdir(exist_ok=True)   # a fresh component: the agent creates its proof crate here
    run = Run(verif, f"prove-{a.tool}")
    run.event("start", component=a.component, tool=a.tool, branch=branch, crate=crate, checkout_commit=commit_of(checkout))
    bundle = verif / "unified_properties.yaml"
    d = yaml.safe_load(open(bundle))
    P = {p["id"]: p for p in d["properties"]}
    cls = d.get("classification") or {}
    ex = set(d.get("level2_excluded") or [])
    a_rows = [k for k, v in cls.items() if v.get("class") == "A" and k in P and k not in ex]   # always re-scored
    rp = reprove_map(d).get(a.tool, [])
    pol = d.get("polarity") or {}
    unproved = [p["id"] for p in d["properties"] if p.get("verifiable") and p["id"] not in ex
                and str((pol.get(p["id"]) or {}).get("polarity", "")).upper() != "HAZARD"
                and not (p.get(a.tool) or {}).get("_scored_by")]
    if a.ids:
        ids = sorted(set(a.ids.split(",")))
    elif getattr(a, "all", False):      # a fresh component: every level-2 property this tool has not scored yet
        ids = sorted(set(unproved) | set(a_rows) | set(rp))
    else:
        ids = sorted(set(a_rows) | set(rp))
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
    timed_out = False
    if a.score_only:   # resume: the agent's artifacts are already in the crate
        run.event("score-only", note="agent not re-run")
    else:
        wr = [str(crate) + "/**", str(advisory)] if crate != comp_dir else [str(comp_dir / "src" / "verification") + "*.rs", str(advisory)]
        ok, text, dt = agent(prompt, checkout, ["Read", "Grep", "Glob", "Write", "Edit", "Bash"], model=a.model,
                             timeout=a.timeout, log=run.dir / "agent_prove.log", name=f"prove_{a.tool}",
                             policy={"writable": wr, "hidden": [str(checkout / "components" / "*" / "verif" / "*_scoring.html")],
                                     "stop_check": selfcheck_cmd("prove", crate, ",".join(ids))})
        run.event("agent", ok=ok, seconds=dt)
        if not ok:
            # Folding strips the tool's statuses, which only a real scoring may replace (X149) - so a timed-out
            # agent's work is scored ONLY for ids that have no scored status yet: nothing can be lost, and work
            # already on disk is credited instead of waiting for another full agent run (zyre, 2026-10-09: two
            # 2 h Kani runs, 84 harnesses on disk, zero scored).
            run.event("FAIL", reason="agent did not finish", detail=text[-300:])
            ids = [i for i in ids if not (P[i].get(a.tool) or {}).get("_scored_by")]
            timed_out = True
            if not ids:
                run.save("failed: agent did not finish (nothing unscored to score)")
                return 1
            run.event("scoring partial work", ids=len(ids))
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
    run.save(("agent timed out; partial work scored: " if timed_out else "proved ") + f"{st}" + (" - DONE" if ok_done else ""))
    print(out.strip())
    return 0 if xc.returncode == 0 and not timed_out else 1


def collections_count(xs):
    out = {}
    for x in xs:
        out[x] = out.get(x, 0) + 1
    return out


def selfcheck(kind, args):
    """The stop hooks' cheap checks (seconds). Exit 0 = the agent may finish; otherwise print why."""
    import yaml
    try:
        if kind == "yaml":
            yaml.safe_load(open(args[0])); return 0
        if kind == "extraction":
            _, errs = check_extraction(args[0])
        elif kind == "reconcile":
            errs = check_pairing(yaml.safe_load(open(args[0])) or {}, yaml.safe_load(open(args[1])) or {},
                                 yaml.safe_load(open(args[2])) or {})
        elif kind == "classify":
            got = yaml.safe_load(open(args[0])) or {}
            d = yaml.safe_load(open(args[1])) or {}
            want = [p["id"] for p in d.get("properties", []) if p.get("verifiable")
                    and str(p.get("origin", "")) in ("divergent", "spec-only")]
            errs = [f"no verdict for {i}" for i in want if i not in got][:20]
            errs += [f"{i}: class {v.get('class')!r} is not A/B/C" for i, v in got.items()
                     if not isinstance(v, dict) or v.get("class") not in ("A", "B", "C")][:20]
            errs += [f"{i}: A/B without file:line evidence" for i, v in got.items()
                     if isinstance(v, dict) and v.get("class") in ("A", "B") and not v.get("evidence")][:20]
        elif kind == "prove":
            crate, ids = Path(args[0]), [i for i in args[1].split(",") if i]
            text = "\n".join(f.read_text(errors="replace") for f in crate.rglob("*.rs") if "/target/" not in str(f))
            errs = []
            for i in ids:
                h = "verify_" + i.lower().replace("-", "_")
                if h not in text and ("refute_" + i.lower().replace("-", "_")) not in text:
                    errs.append(f"{i}: no {h} (or refute_) in the crate")
                elif h in text and (h + "__mutant") not in text:
                    errs.append(f"{i}: {h} has no {h}__mutant twin")
        else:
            errs = [f"unknown check {kind}"]
    except (OSError, yaml.YAMLError) as e:
        errs = [f"cannot read: {e}"]
    for e in errs:
        print(e)
    return 1 if errs else 0


def main():
    if len(sys.argv) > 2 and sys.argv[1] == "selfcheck":
        return selfcheck(sys.argv[2], sys.argv[3:])
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
    ex = sub.add_parser("extract")
    ex.add_argument("checkout"); ex.add_argument("component")
    ex.add_argument("--dry-run", action="store_true"); ex.add_argument("--model", default=None)
    ex.add_argument("--timeout", type=int, default=3600)
    ex.add_argument("--fresh", action="store_true", help="re-run both readers even if lists from this commit exist")
    pv = sub.add_parser("prove")
    pv.add_argument("checkout"); pv.add_argument("component")
    pv.add_argument("--tool", choices=sorted(TOOLS), required=True)
    pv.add_argument("--ids", default=None, help="comma-separated ids (default: A rows + this tool's re-prove list)")
    pv.add_argument("--all", action="store_true", help="every level-2 property this tool has not scored yet")
    pv.add_argument("--dry-run", action="store_true"); pv.add_argument("--model", default=None)
    pv.add_argument("--score-only", action="store_true", help="skip the agent; fold + score what is in the crate")
    pv.add_argument("--timeout", type=int, default=7200, help="agent time box, seconds")
    pv.add_argument("--score-timeout", type=int, default=14400, help="scorer time box, seconds")
    k = sub.add_parser("check")
    k.add_argument("checkout"); k.add_argument("component")
    a = ap.parse_args()
    if a.cmd == "prove":
        return step_prove(a)
    if a.cmd == "extract":
        return step_extract(a)
    if a.cmd == "check":
        ok, _ = step_check(a.checkout, a.component)
        return 0 if ok else 1
    if a.cmd == "sync":
        return step_sync(a)
    return step_classify(a)


if __name__ == "__main__":
    sys.exit(main())
