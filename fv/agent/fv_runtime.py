#!/usr/bin/env python3
"""fv runtime - runs fv agent jobs unattended, one at a time per machine, and shows them all in one table.

    fv_runtime.py submit  <component> [--checkout PATH] [--base origin/unstable] [--tools creusot,kani]
                          [--budget-hours 10] [--machine HOST]        # HOST: run it there (ssh), default this machine
    fv_runtime.py worker  [--once]                                    # take ONE job at a time, forever (or once)
    fv_runtime.py status  [--machines this,node7]                     # every job on every machine, one table
    fv_runtime.py cancel  <job-id>

Replaces this week's hand-made pieces (a jobs file read by prose, a 30-minute watcher, background shells):
  - a job is a small JSON file in $FV_JOBS (default ~/fv-jobs): queue/ -> running/ -> done/ | failed/
  - the worker runs `fv_agent.py verify` for the job in its own process group, under a wall-clock budget; on breach
    the whole tree (agents, cargo, provers) is killed and the job fails with that reason - no orphan provers
  - a heartbeat (step + last log line) is written every 30 s; a job whose heartbeat is older than 10 min with no
    live worker is put back in the queue and RESUMES (finished steps are not redone: extract is skipped when the
    property list exists, prove only takes what the scorer has not scored)
  - a job that reaches DONE is published to two LOCAL verif branches; nothing is ever pushed
Python 3.9, standard library + PyYAML.
"""
import argparse, json, os, signal, socket, subprocess, sys, time
from datetime import datetime, timezone
from pathlib import Path

HERE = Path(__file__).resolve().parent
JOBS = Path(os.environ.get("FV_JOBS", os.path.expanduser("~/fv-jobs")))
STATES = ("queue", "running", "done", "failed")
STALE_S = 600


def now():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def dirs():
    for s in STATES:
        (JOBS / s).mkdir(parents=True, exist_ok=True)


def save(job, state):
    dirs()
    for s in STATES:                                   # a job lives in exactly one state directory
        p = JOBS / s / f"{job['id']}.json"
        if s != state and p.exists():
            p.unlink()
    tmp = JOBS / state / f".{job['id']}.tmp"
    tmp.write_text(json.dumps(job, indent=2))
    os.replace(tmp, JOBS / state / f"{job['id']}.json")


def load_all():
    dirs()
    out = []
    for s in STATES:
        for p in sorted((JOBS / s).glob("*.json")):
            try:
                j = json.loads(p.read_text()); j["_state"] = s; out.append(j)
            except ValueError:
                pass
    return out


# ------------------------------------------------------------------------------------------------- submit
def cmd_submit(a):
    if a.machine and a.machine not in ("this", socket.gethostname()):
        args = [a.component] + (["--checkout", a.checkout] if a.checkout else []) + [
            "--base", a.base, "--tools", a.tools, "--budget-hours", str(a.budget_hours)]
        p = subprocess.run(["ssh", a.machine, "python3", str(remote_runtime()), "submit", *args],
                           capture_output=True, text=True)
        print(p.stdout.strip() or p.stderr.strip()); return p.returncode
    jid = f"{datetime.now().strftime('%Y%m%d-%H%M%S')}-{a.component}"
    job = {"id": jid, "component": a.component, "checkout": a.checkout, "base": a.base, "tools": a.tools,
           "budget_s": int(a.budget_hours * 3600), "submitted": now(), "host": socket.gethostname(),
           "attempts": 0, "history": []}
    save(job, "queue")
    print(f"queued {jid} on {socket.gethostname()}")
    return 0


def remote_runtime():
    """Path of this file on the other machine: the same workbench layout under the remote home directory."""
    rel = os.path.relpath(Path(__file__).resolve(), Path.home())
    return Path("~") / rel


# ------------------------------------------------------------------------------------------------- worker
def prepare_checkout(job):
    """A fresh local branch from the base when the job names no checkout (never a detached published commit)."""
    if job.get("checkout"):
        return Path(job["checkout"])
    repo = Path(os.environ.get("FV_CERTUS_REPO", os.path.expanduser("~/ai-native-storage-certus")))
    co = Path(os.environ.get("FV_RUNS", os.path.expanduser("~/fv-runs"))) / job["id"]
    if not co.exists():
        subprocess.run(["git", "-C", str(repo), "fetch", "-q", "origin"], check=False)
        subprocess.run(["git", "-C", str(repo), "worktree", "add", "-q", "-b", f"fv/{job['id']}", str(co), job["base"]],
                       check=True)
    job["checkout"] = str(co)
    return co


def sweep(job_id):
    """Kill every process tagged with this job (FV_JOB in its environment) - also those that left the job's
    process group, e.g. an agent's background shell in its own session (zyre, 2026-10-09: 7 parallel Kani
    runs kept going after the budget kill). Returns how many were killed."""
    tag = f"FV_JOB={job_id}".encode()
    n = 0
    for d in Path("/proc").iterdir():
        if not d.name.isdigit() or int(d.name) == os.getpid():
            continue
        try:
            if tag in (d / "environ").read_bytes().split(b"\0"):
                os.kill(int(d.name), signal.SIGKILL); n += 1
        except (OSError, ValueError):
            pass
    return n


def heartbeat(job, step, line):
    job["heartbeat"] = {"at": now(), "pid": os.getpid(), "host": socket.gethostname(), "step": step, "last": line[:200]}
    save(job, "running")


def mem_scope(job):
    """A systemd user scope that caps the WHOLE job's memory (default 60% of RAM, FV_JOB_MEM_PCT). Without it one
    agent's parallel CBMC runs filled green's 377 GB and the kernel killed the user's systemd manager
    (2026-10-09 16:45). Fail-closed: no scope, no job."""
    pct = int(os.environ.get("FV_JOB_MEM_PCT", "60"))
    total_kb = int(next(l.split()[1] for l in open("/proc/meminfo") if l.startswith("MemTotal:")))
    cap = f"{total_kb * pct // 100 // 1024}M"
    probe = subprocess.run(["systemd-run", "--user", "--scope", "--quiet", "-p", "MemoryMax=1G", "true"],
                           capture_output=True, text=True)
    if probe.returncode:
        raise RuntimeError(f"no systemd user scope ({probe.stderr.strip()[:120]}) - refusing to run uncapped; "
                           f"restore the user manager (log out of every session and back in)")
    return ["systemd-run", "--user", "--scope", "--quiet", f"--unit=fv-{job['id']}",
            "-p", f"MemoryMax={cap}", "-p", "MemorySwapMax=0",
            # a memory kill fails ONE process (one harness); systemd's default OOMPolicy=stop would SIGTERM the
            # whole job (node7, 2026-10-10 02:31: one CBMC killed at the cap -> the job died before Kani was scored)
            "-p", "OOMPolicy=continue"], cap


def run_job(job):
    co = prepare_checkout(job)
    log = Path(co) / "components" / job["component"] / "verif" / ".run" / f"runtime_{job['id']}.log"
    log.parent.mkdir(parents=True, exist_ok=True)
    scope, cap = mem_scope(job)
    cmd = scope + [sys.executable, str(HERE / "fv_agent.py"), "verify", str(co), job["component"], "--tools", job["tools"]]
    job["mem_cap"] = cap
    job["attempts"] += 1
    job["history"].append({"started": now(), "cmd": " ".join(cmd), "log": str(log)})
    heartbeat(job, "starting", "")
    t0 = time.time()
    with open(log, "a") as fh:
        proc = subprocess.Popen(cmd, stdout=fh, stderr=subprocess.STDOUT, start_new_session=True,
                                env={**os.environ, "FV_JOB": job["id"]})
        step, last = "starting", ""
        while proc.poll() is None:
            time.sleep(30)
            try:
                tail = log.read_text(errors="replace").splitlines()[-40:]
            except OSError:
                tail = []
            for ln in reversed(tail):
                if "step " in ln and "[fv-agent" in ln:
                    step = ln.split("step ", 1)[1].strip(); break
            last = tail[-1] if tail else ""
            heartbeat(job, step, last)
            if time.time() - t0 > job["budget_s"]:
                os.killpg(proc.pid, signal.SIGKILL)   # the whole tree: agents, cargo, provers
                proc.wait()
                strays = sweep(job["id"])              # and anything that left the process group
                job["history"][-1].update(ended=now(), outcome=f"budget of {job['budget_s']} s exceeded at step {step}",
                                          strays_killed=strays)
                save(job, "failed")
                return
    strays = sweep(job["id"])                          # a finished job leaves nothing running
    out = log.read_text(errors="replace")
    outcome = next((l.split("OUTCOME:", 1)[1].strip() for l in reversed(out.splitlines()) if "OUTCOME:" in l),
                   f"agent exited {proc.returncode} without an outcome line")
    job["history"][-1].update(ended=now(), rc=proc.returncode, outcome=outcome, seconds=round(time.time() - t0),
                              strays_killed=strays)
    if outcome.startswith("DONE"):
        p = subprocess.run([sys.executable, str(HERE / "fv_agent.py"), "publish", str(co), job["component"],
                            "--base", job["base"]], capture_output=True, text=True)
        job["published"] = (p.stdout + p.stderr).strip().splitlines()
        save(job, "done")
    else:
        save(job, "failed")


def requeue_stale():
    for j in load_all():
        if j["_state"] != "running":
            continue
        hb = j.get("heartbeat") or {}
        age = time.time() - datetime.strptime(hb.get("at", "1970-01-01T00:00:00Z"), "%Y-%m-%dT%H:%M:%SZ").replace(
            tzinfo=timezone.utc).timestamp()
        alive = hb.get("host") == socket.gethostname() and pid_alive(hb.get("pid"))
        if age > STALE_S and not alive:
            j["history"].append({"requeued": now(), "why": f"heartbeat {int(age)} s old, worker gone - resuming"})
            j.pop("_state", None); save(j, "queue")
            print(f"requeued stale {j['id']}")


def pid_alive(pid):
    try:
        os.kill(int(pid), 0); return True
    except (TypeError, ValueError, OSError):
        return False


def cmd_worker(a):
    print(f"fv worker on {socket.gethostname()}, jobs in {JOBS}", flush=True)
    while True:
        requeue_stale()
        q = [j for j in load_all() if j["_state"] == "queue"]
        if q:
            job = sorted(q, key=lambda j: j["submitted"])[0]
            job.pop("_state", None)
            print(f"[{now()}] running {job['id']}", flush=True)
            save(job, "running")
            try:
                run_job(job)
            except Exception as e:                         # never leave a job silently in running/
                job.setdefault("history", []).append({"ended": now(), "outcome": f"runtime error: {e}"})
                save(job, "failed")
            print(f"[{now()}] finished {job['id']}", flush=True)
            if a.once:
                return 0
        elif a.once:
            print("queue empty"); return 0
        else:
            time.sleep(60)


# ------------------------------------------------------------------------------------------------- status
def cmd_status(a):
    rows = []
    for m in a.machines.split(","):
        if m in ("this", socket.gethostname()):
            jobs = load_all()
        else:
            p = subprocess.run(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", m, "python3",
                                str(remote_runtime()), "status", "--json"], capture_output=True, text=True)
            try:
                jobs = json.loads(p.stdout)
            except ValueError:
                rows.append((m, "-", "unreachable", "", p.stderr.strip()[:60])); continue
        for j in jobs:
            h = j.get("history") or [{}]
            info = (j.get("heartbeat") or {}).get("step", "") if j["_state"] == "running" else h[-1].get("outcome", "")
            rows.append((m if m != "this" else socket.gethostname(), j["id"], j["_state"], j["component"], str(info)[:70]))
    if a.json:
        print(json.dumps(load_all())); return 0
    w = [max(len(str(r[i])) for r in rows + [("machine", "job", "state", "component", "step / outcome")]) for i in range(5)]
    line = "+" + "+".join("-" * (x + 2) for x in w) + "+"
    print(line)
    for i, r in enumerate([("machine", "job", "state", "component", "step / outcome")] + rows):
        print("| " + " | ".join(str(r[k]).ljust(w[k]) for k in range(5)) + " |")
        if i == 0:
            print(line)
    print(line)
    return 0


def cmd_cancel(a):
    for j in load_all():
        if j["id"] == a.job and j["_state"] in ("queue", "running"):
            pid = (j.get("heartbeat") or {}).get("pid")
            j.setdefault("history", []).append({"ended": now(), "outcome": "cancelled"})
            j.pop("_state", None); save(j, "failed")
            print(f"cancelled {a.job} (a running worker finishes its current step)" if pid else f"cancelled {a.job}")
            return 0
    print("no such queued/running job"); return 1


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("submit"); s.add_argument("component")
    s.add_argument("--checkout", default=None); s.add_argument("--base", default="origin/unstable")
    s.add_argument("--tools", default="creusot,kani"); s.add_argument("--budget-hours", type=float, default=10)
    s.add_argument("--machine", default=None)
    w = sub.add_parser("worker"); w.add_argument("--once", action="store_true")
    st = sub.add_parser("status"); st.add_argument("--machines", default="this"); st.add_argument("--json", action="store_true")
    c = sub.add_parser("cancel"); c.add_argument("job")
    a = ap.parse_args()
    return {"submit": cmd_submit, "worker": cmd_worker, "status": cmd_status, "cancel": cmd_cancel}[a.cmd](a)


if __name__ == "__main__":
    sys.exit(main())
