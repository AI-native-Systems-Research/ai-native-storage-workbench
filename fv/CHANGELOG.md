# fv changelog

One line per push, newest first. Details are in the commit messages (`git log -- fv/`).

- 2026-10-17 - Imported Daniel's Spin skills, tools-spin-model and tools-spin-sync (certus acd24025), with the J18 rule: every returned value is modelled or listed as "Left out" and checked on sync; README citations are re-derived on every sync; no-sudo Spin install.
- 2026-10-13 - Repair step: several properties per repair, Loom targets (`--loom`, repair_accept_loom.sh), both
  published Creusot layouts, the component's test features, self-setup of native-library links, `--check-only`;
  the check rejects empty baselines and states Loom bounds; fix records open with In short + At a glance. Used to
  fix and re-prove 6 bugs (dispatch-map, memory-tier, zyre, block-device-kernel, 2 in extended-metadata-store).
- 2026-10-12 - Repair agent in our own driver (`fv_driver.py repair`: explain -> red-first test -> fix -> proof of
  the fix; the acceptance check decides; local commit + fix_<component>_<finding>.md), tests run 5x, red-first
  leg fixed for native libraries, `--check-only`; Kani split harnesses + contract proofs in the scorer;
  docs FV_AGENT.md and REPAIR.md. Used overnight: dispatch-map, memory-tier and zyre bugs fixed and proved.
- 2026-10-10 - First unattended fv-agent runs on two never-verified components (zyre, block-device-kernel) and
  the fixes they forced: jobs run memory-capped (systemd user scope, OOMPolicy=continue), leftover processes
  swept, no background shells, at most 2 provers at a time, timed-out work still scored, supervisor stops on
  no progress, fresh components get their own proof crate, scorer contradiction check fixed.
- 2026-10-09 - fv/agent: the FV agent (`verify`: extract -> sync -> classify -> prove -> check, a bounded
  supervisor, `publish` to local branches) and its runtime (job queue, one worker per machine, budgets, heartbeat,
  resume, status table). Driver: `extract` step with sandboxed blind readers (the reconcile agent writes a pairing
  table, the driver builds the unified list); `prove --all`; our own guard + stop hooks on every agent.
- 2026-10-09 - fv/driver `prove` step (one tool at a time; agent writes, scorer decides) + per-tool re-prove list;
  scorer finds proof modules inside impl blocks; driver stops cleanly when an agent fails. Tested on logger: all 16
  Creusot re-proofs and its A row proved.
- 2026-10-09 - fv/driver `sync` step (code-assumption check: an agent reports, the driver validates and applies);
  check_done now also fails on unclassified candidates and on proofs that predate a new code assumption.
- 2026-10-09 - fv/driver: first step of the Python orchestrator - `classify` (two independent classifier agents ->
  merge -> Level 1 -> page) and `check` (check_done). Tested end to end on logger (5 min 42 s).
- 2026-10-09 - Import of the formal-verification kit from ai-native-storage-certus (`.claude/skills/` at 6397e046):
  orchestrator + gate, the two blind readers, Creusot and Kani proving skills, repair agent, setup skills (new:
  tools-kani-install), `fv/install.sh`. The Certus copies are frozen from here on.
- 2026-10-14: driver - step agents may not start background work (helper agents, workflows, monitors, cron), which headless mode kills 10 min after the turn; FV_TIMEOUT_SCALE scales every step's time box for large components (dispatcher).
- 2026-10-14: FV agent - a restarted verify reuses level1-check and classify when their latest run finished on the same commit (--redo repeats them).
- 2026-10-15: repair check - the re-verification time box scales with FV_TIMEOUT_SCALE (REPAIR_GATE_TIMEOUT overrides); its working folder is kept until a verdict (keyed by base + fix + gate) and an interrupted pass resumes with the scorer's --resume.
- 2026-10-15: driver - prove --candidates: with --ids, also attempt named bug candidates (class B) to confirm or clear them before repair.
- 2026-10-16: repair --continue: continue an earlier repair still on disk (its work + its last rejection) instead of starting from the base.
- 2026-10-16: repair prompt - write and run the red-first test BEFORE changing other source files; helpers go inside the test file (two dispatcher repairs were rejected because their test used a helper from the fix).
- 2026-10-18: repair check - clears inherited RUSTFLAGS/CARGO_TARGET_DIR (a leaked --cfg loom broke the server build and the Loom target leg, so two correct dispatcher repairs were rejected).
- 2026-10-22: fv/components/ - the models of each verified component, with results and insights; first: dispatcher (Creusot, Kani, Loom, 5 new + 4 re-synced Spin models, J18-J26).
- 2026-10-22: fv/QUICKSTART_DRAFT.md (new-user guide: install, verif-branch rules, verify, repair, Loom/Spin) and skill tools-loom-test.
- 2026-10-22: fv/QUICKSTART.md (no longer a draft; linked from fv/README.md) - how to run and write Loom tests and Spin models, tested on the dispatcher.
- 2026-10-23: fv/components/dispatcher/spin: one folder per question with a self-documenting name (no Jxy), current version only; every README has a header (what, result, bugs, Certus commit, history) and a Returned values table; spin/README.md index; all 9 models and controls re-run.
- 2026-10-23: fv/components/README.md - table of the newest verif branch of each component (generated by fv/tools/verif_branch_table.py); linked from QUICKSTART section 2.
- 2026-10-23: fv/QUICKSTART.md - an example at the top: verify one component with all 4 tools in 5 steps, with pointers to the sections.
