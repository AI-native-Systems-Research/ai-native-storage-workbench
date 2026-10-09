#!/usr/bin/env python3
"""DEFINITION OF DONE for one component's verification bundle — prints DONE or the exact missing steps.

Cornel (2026-10-07): progress was hard to see because "done" was never defined; a component was "published", then
"needs a sync check", then "needs a re-score". This script is the single answer. It checks a verif/ directory (a
branch checkout or a SEPT_28 folder) against the CURRENT method and never edits anything.

A component is DONE when:
  L1   Level 1 ran: discordances.yaml + level2_excluded exist
  SYNC the input-range ("code assumptions") check ran: `domain_discordances` is present (an empty list is fine)
  CLS  every current Level-1 candidate has a classification (or there are none)
  REPROVE no property waits to be re-proved under a newly declared code assumption (`level2_reprove`)
  SCR  every Level-2 property has a scorer-owned status for every tool that is not withheld by a tool_note
  UNR  no Level-2 property is UNRESOLVED (or unscored) in a non-withheld tool
  XT   cross-tool check: no property proved by one tool and refuted by the other
  TWIN no Kani credit on the bounded path (--no-unwinding-checks) without a twin that ran (the F15/F17 defect)
  PAGE <c>_scoring.html and <c>_discordances.html exist next to the bundle

usage: check_done.py <verif_dir> [<verif_dir> ...]      (exit 0 only if every one is DONE)
"""
import os, subprocess, sys
import yaml

TWIN_FIX = "7d3105e9"
GATE_DIR = os.path.dirname(os.path.abspath(__file__))


def gate_has_fix(commit):
    if not commit or commit.startswith("unknown"):
        return False
    r = subprocess.run(["git", "-C", GATE_DIR, "merge-base", "--is-ancestor", TWIN_FIX, commit],
                       capture_output=True)
    return r.returncode == 0


def check(vd):
    miss = []
    bp = os.path.join(vd, "unified_properties.yaml")
    if not os.path.exists(bp):
        return ["no unified_properties.yaml"]
    d = yaml.safe_load(open(bp)) or {}
    comp = d.get("component") or os.path.basename(os.path.dirname(os.path.abspath(vd)))
    if not os.path.exists(os.path.join(vd, "discordances.yaml")) or "level2_excluded" not in d:
        miss.append("L1: level1.py has not run (no discordances.yaml / level2_excluded)")
    if "domain_discordances" not in d:
        miss.append("SYNC: the code-assumption (input-range) check has not run")
    cand = [p for p in d.get("properties", []) if p.get("verifiable")
            and str(p.get("origin", "")) in ("divergent", "spec-only")]
    cls = d.get("classification") or {}
    uncls = [p["id"] for p in cand if p["id"] not in cls]
    if cand and not cls:
        miss.append(f"CLS: classifier has not run on {len(cand)} candidates")
    elif uncls:   # e.g. records a later sync check reclassified as divergent (found by the driver, 2026-10-09)
        miss.append(f"CLS: {len(uncls)} candidate(s) not classified yet (e.g. {uncls[0]})")
    rp = [x for x in (d.get("level2_reprove") or []) if x]
    if rp:        # proofs that predate a newly declared code assumption must be re-proved under exactly it
        miss.append(f"REPROVE: {len(rp)} propert(y/ies) must be re-proved under a new code assumption (e.g. {rp[0]})")
    ex = set(d.get("level2_excluded") or [])
    pol = d.get("polarity") or {}
    l2 = [p for p in d.get("properties", []) if p.get("verifiable") and p.get("id") not in ex
          and str((pol.get(p.get("id")) or {}).get("polarity", "")).upper() != "HAZARD"]   # same scope as the scorers
    withheld = {str(n.get("tool", "")).lower() for n in (d.get("tool_notes") or [])}
    run = d.get("run") or {}
    for t in ("creusot", "kani"):
        if t in withheld:
            continue
        uns = [p["id"] for p in l2 if not (p.get(t) or {}).get("_scored_by")]
        unr = [p["id"] for p in l2 if (p.get(t) or {}).get("status") == "UNRESOLVED"]
        if uns:
            miss.append(f"SCR: {t}: {len(uns)} of {len(l2)} level-2 properties unscored (e.g. {uns[0]})")
        if unr:
            miss.append(f"UNR: {t}: {len(unr)} UNRESOLVED (e.g. {unr[0]})")
    xt = [p["id"] for p in l2 if {"proved", "refuted"} <= {(p.get(t) or {}).get("status") for t in ("creusot", "kani")}]
    if xt:
        miss.append(f"XT: {len(xt)} cross-tool contradictions (e.g. {xt[0]})")
    if "kani" not in withheld:
        tw = [p["id"] for p in l2 if (p.get("kani") or {}).get("status") == "proved"
              and ((p["kani"].get("evidence") or {}).get("unwinding_checks") is False)
              and (p["kani"]["evidence"].get("mutant_twin") == "absent"
                   or not gate_has_fix(str(run.get("gate_commit", ""))))]
        if tw:
            miss.append(f"TWIN: {len(tw)} Kani 'proved' on the bounded path without a verified twin (e.g. {tw[0]})")
    for page in (f"{comp}_scoring.html", f"{comp}_discordances.html"):
        if not os.path.exists(os.path.join(vd, page)):
            miss.append(f"PAGE: {page} missing")
    return miss


def main():
    allok = True
    for vd in sys.argv[1:]:
        m = check(vd)
        name = os.path.basename(os.path.abspath(vd.rstrip("/")))
        if name == "verif":
            name = os.path.basename(os.path.dirname(os.path.abspath(vd.rstrip("/"))))
        print(f"{name:32s} {'DONE' if not m else 'NOT DONE'}")
        for x in m:
            print(f"    - {x}")
        allok &= not m
    return 0 if allok else 1


if __name__ == "__main__":
    sys.exit(main())
