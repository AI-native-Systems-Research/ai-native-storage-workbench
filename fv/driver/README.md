# fv driver

Runs the FV pipeline steps in a fixed order, checks each step's result in code, and stops honestly.
LLM judgement runs as Claude agents (headless `claude -p`); every verdict comes from the gate scripts.

```
python3 fv/driver/fv_driver.py classify <checkout> <component> [--dry-run] [--model M] [--timeout S]
python3 fv/driver/fv_driver.py check    <checkout> <component>
```
The checkout must be on a local branch (the driver never edits a published commit) and is never pushed.
Each run writes `components/<c>/verif/.run/driver/<stamp>-<step>/` with the prompts, agent logs and a
`provenance.json` (driver commit, gate commit, prompt hashes, timings, outcome).

Steps: `classify` (done), `check` (done); next: `sync` (code-assumption check), `prove-a` (prove the A rows),
`run` (chain until check_done says DONE).
