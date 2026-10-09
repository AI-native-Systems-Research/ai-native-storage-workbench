# fv agent and runtime

**fv_agent.py** verifies one component end to end and stops honestly:
```
python3 fv/agent/fv_agent.py verify  <checkout> <component> [--tools creusot,kani] [--rounds 2]
python3 fv/agent/fv_agent.py publish <checkout> <component>        # two LOCAL verif branches; never pushes
```
The sequence is code (fv/driver): extract (two blind readers in sandboxes that contain only their side, then
reconcile) -> sync -> classify -> prove each tool -> check. The agent's judgement is used only in the supervisor,
which picks ONE action from a fixed menu (retry named ids for one tool, or stop with a reason for a person), for a
bounded number of rounds. It cannot skip a step, write a status, edit the gate, push, or touch production code.

**fv_runtime.py** runs agent jobs unattended, one at a time per machine:
```
python3 fv/agent/fv_runtime.py submit <component> [--machine node7] [--budget-hours 10]
python3 fv/agent/fv_runtime.py worker            # leave running on each machine
python3 fv/agent/fv_runtime.py status --machines this,node7
```
Jobs are JSON files in `$FV_JOBS` (default `~/fv-jobs`): queue -> running -> done | failed. Each job runs in its own
process group under a wall-clock budget (the whole tree is killed on breach), writes a heartbeat every 30 s, resumes
after a crash, and is published to local branches when it reaches DONE.
Python 3.9 and PyYAML only; agents run as headless Claude Code (`claude -p`).
