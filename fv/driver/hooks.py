#!/usr/bin/env python3
"""Claude Code hooks for the fv driver's agents - a check AS each tool call happens, and a check when the agent
tries to finish. Our own, deliberately small. Configured per step by fv_driver.agent(policy=...).

  guard (PreToolUse): blocks a write outside the step's writable paths, a read of a hidden path, a forbidden shell
                      command, or added text matching a forbidden pattern. The driver's check after the session stays
                      authoritative (a shell command can change files this hook never sees).
  stop  (Stop):       runs the step's cheap check command; if it fails, the agent is told why and keeps working,
                      at most `max_stop_blocks` times.
Exit code 2 = block (Claude Code shows our stderr to the agent). Policy = JSON file:
  {"workdir": "...", "writable": [glob, ...], "hidden": [glob, ...], "forbid_cmd": [regex, ...],
   "forbid_add": [regex, ...], "stop_check": [cmd, ...] | null, "max_stop_blocks": 3, "log": "path.jsonl"}
"""
import fnmatch, json, os, re, subprocess, sys, time
from pathlib import Path


def load(path):
    try:
        return json.loads(Path(path).read_text())
    except (OSError, ValueError) as e:
        print(f"fv hook: cannot load policy {path}: {e} - blocking (fail closed)", file=sys.stderr)
        sys.exit(2)


def note(pol, rec):
    if pol.get("log"):
        rec["t"] = round(time.time(), 1)
        with open(pol["log"], "a") as f:
            f.write(json.dumps(rec) + "\n")


def absolute(p, wd):
    return os.path.normpath(p if os.path.isabs(p) else os.path.join(wd, p))


def matches(path, globs):
    return any(fnmatch.fnmatch(path, g) for g in globs)


def guard(pol):
    ev = json.load(sys.stdin)
    tool, inp = ev.get("tool_name", ""), ev.get("tool_input") or {}
    wd = pol.get("workdir") or ev.get("cwd") or os.getcwd()
    why = []
    if tool in ("Write", "Edit", "MultiEdit", "NotebookEdit"):
        fp = absolute(inp.get("file_path") or inp.get("notebook_path") or "", wd)
        if not matches(fp, pol.get("writable") or []):
            why.append(f"{tool} to {fp} is outside this step's writable paths {pol.get('writable')}")
        added = " ".join(str(inp.get(k, "")) for k in ("content", "new_string"))
        added += " ".join(str(e.get("new_string", "")) for e in inp.get("edits") or [])
        for rx in pol.get("forbid_add") or []:
            if re.search(rx, added):
                why.append(f"added text matches a forbidden pattern: {rx}")
    if tool in ("Read", "Grep", "Glob"):
        fp = absolute(inp.get("file_path") or inp.get("path") or wd, wd)
        if matches(fp, pol.get("hidden") or []):
            why.append(f"{fp} is hidden from this step")
    if tool == "Bash":
        cmd = inp.get("command", "")
        for rx in pol.get("forbid_cmd") or []:
            if re.search(rx, cmd):
                why.append(f"command matches a forbidden pattern: {rx}")
    note(pol, {"hook": "guard", "tool": tool, "blocked": bool(why), "why": why})
    if why:
        print("Blocked by the fv guard: " + "; ".join(why) + ". Stay within this step's rules.", file=sys.stderr)
        return 2
    return 0


def stop(pol, state_path):
    if not pol.get("stop_check"):
        return 0
    st = {}
    if Path(state_path).exists():
        st = json.loads(Path(state_path).read_text() or "{}")
    p = subprocess.run(pol["stop_check"], capture_output=True, text=True, cwd=pol.get("workdir"), timeout=600)
    if p.returncode == 0:
        note(pol, {"hook": "stop", "decision": "allow"})
        return 0
    blocks = int(st.get("blocks", 0))
    if blocks >= int(pol.get("max_stop_blocks", 3)):
        note(pol, {"hook": "stop", "decision": "gave_up", "blocks": blocks})
        return 0                      # let it end; the driver's check after the session records the failure
    Path(state_path).write_text(json.dumps({"blocks": blocks + 1}))
    note(pol, {"hook": "stop", "decision": "block", "blocks": blocks + 1, "failures": (p.stdout + p.stderr)[-1500:]})
    print("Not finished - the step's check failed:\n" + (p.stdout + p.stderr)[-1500:] +
          "\nFix this before you stop. Do not weaken the check or the requirement.", file=sys.stderr)
    return 2


if __name__ == "__main__":
    kind, policy = sys.argv[1], load(sys.argv[2])
    sys.exit(guard(policy) if kind == "guard" else stop(policy, sys.argv[3]))
