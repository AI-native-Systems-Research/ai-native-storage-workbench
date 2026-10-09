# fv changelog

One line per push, newest first. Details are in the commit messages (`git log -- fv/`).

- 2026-10-09 - fv/driver `sync` step (code-assumption check: an agent reports, the driver validates and applies);
  check_done now also fails on unclassified candidates and on proofs that predate a new code assumption.
- 2026-10-09 - fv/driver: first step of the Python orchestrator - `classify` (two independent classifier agents ->
  merge -> Level 1 -> page) and `check` (check_done). Tested end to end on logger (5 min 42 s).
- 2026-10-09 - Import of the formal-verification kit from ai-native-storage-certus (`.claude/skills/` at 6397e046):
  orchestrator + gate, the two blind readers, Creusot and Kani proving skills, repair agent, setup skills (new:
  tools-kani-install), `fv/install.sh`. The Certus copies are frozen from here on.
