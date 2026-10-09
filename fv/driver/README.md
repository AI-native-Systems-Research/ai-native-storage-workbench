# fv driver

Runs the FV pipeline steps in a fixed order, checks each step's result in code, and stops honestly.
LLM judgement runs as Claude agents (headless `claude -p`); every verdict comes from the gate scripts.

```
python3 fv/driver/fv_driver.py classify <checkout> <component> [--dry-run] [--model M] [--timeout S]
python3 fv/driver/fv_driver.py sync     <checkout> <component> [--dry-run] [--model M] [--timeout S]
python3 fv/driver/fv_driver.py check    <checkout> <component>
```
The checkout must be on a local branch (the driver never edits a published commit) and is never pushed.
Each run writes `components/<c>/verif/.run/driver/<stamp>-<step>/` with the prompts, agent logs and a
`provenance.json` (driver commit, gate commit, prompt hashes, timings, outcome).

Steps: `sync` (code-assumption check) and `classify` and `check` (done); next: `prove` (the A rows and the
properties listed in `level2_reprove`),
`run` (chain until check_done says DONE).
