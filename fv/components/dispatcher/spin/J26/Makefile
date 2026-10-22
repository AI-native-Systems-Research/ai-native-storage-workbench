MODEL  = remove_ssd_extent.pml
CC     = cc
CFLAGS = -O2 -DMEMLIM=16000
DEPTH  = 400000
MUTANT = INJECT_STALE_OFFSET
# Every pan run is memory-capped (60G) and time-limited (1800 s), per workbench rules.
RUN    = systemd-run --user --scope -q -p MemoryMax=60G -p MemorySwapMax=0 -p OOMPolicy=continue timeout 1800

.PHONY: all base control mutant liveness props prop1 prop2 prop3 prop4 prop5 dram clean

all: base control mutant

# BASE: the code as-is. Expected: assertion violations (see REPORT.md).
base:
	spin -a $(MODEL)
	$(CC) $(CFLAGS) -DSAFETY -o pan pan.c
	$(RUN) ./pan -m$(DEPTH)

# CONTROL: base + minimal fix (offset read under the dm.remove lock; no
# release_read after a NotExist lookup). Expected: 0 errors.
control:
	spin -DCONTROL -a $(MODEL)
	$(CC) $(CFLAGS) -DSAFETY -o pan-control pan.c
	$(RUN) ./pan-control -m$(DEPTH)

# MUTANT: control with the remove-side TOCTOU regressed (lookup-time offset).
# Expected: >= 1 assertion violation.
mutant:
	spin -DCONTROL -D$(MUTANT) -a $(MODEL)
	$(CC) $(CFLAGS) -DSAFETY -o pan-mutant pan.c
	$(RUN) ./pan-mutant -m$(DEPTH)

# Liveness: non-progress-cycle check on the CONTROL build (the BASE build
# stops at its safety counterexamples first). progress_job in the Writer marks
# the states that must be visited infinitely often in any infinite run.
liveness:
	spin -DCONTROL -a $(MODEL)
	$(CC) $(CFLAGS) -DNP -o pan-live pan.c
	$(RUN) ./pan-live -l -m$(DEPTH)

# Per-property runs on the BASE model (all other properties disabled), so each
# property gets its own counterexample trail.
props: prop1 prop2 prop3 prop4 prop5

prop1:
	spin -DDISABLE_P2 -DDISABLE_P3 -DDISABLE_P4 -DDISABLE_P5 -a $(MODEL)
	$(CC) $(CFLAGS) -DSAFETY -o pan-p1 pan.c
	$(RUN) ./pan-p1 -m$(DEPTH)

prop2:
	spin -DDISABLE_P1 -DDISABLE_P3 -DDISABLE_P4 -DDISABLE_P5 -a $(MODEL)
	$(CC) $(CFLAGS) -DSAFETY -o pan-p2 pan.c
	$(RUN) ./pan-p2 -m$(DEPTH)

prop3:
	spin -DDISABLE_P1 -DDISABLE_P2 -DDISABLE_P4 -DDISABLE_P5 -a $(MODEL)
	$(CC) $(CFLAGS) -DSAFETY -o pan-p3 pan.c
	$(RUN) ./pan-p3 -m$(DEPTH)

prop4:
	spin -DDISABLE_P1 -DDISABLE_P2 -DDISABLE_P3 -DDISABLE_P5 -a $(MODEL)
	$(CC) $(CFLAGS) -DSAFETY -o pan-p4 pan.c
	$(RUN) ./pan-p4 -m$(DEPTH)

prop5:
	spin -DDISABLE_P1 -DDISABLE_P2 -DDISABLE_P3 -DDISABLE_P4 -a $(MODEL)
	$(CC) $(CFLAGS) -DSAFETY -o pan-p5 pan.c
	$(RUN) ./pan-p5 -m$(DEPTH)

# Extra (not P1-P5): the SSD evictor frees the DRAM slot before its dm.remove
# gate — live MemoryTier entry left pointing at freed DRAM (background.rs:328).
dram:
	spin -DDISABLE_P1 -DDISABLE_P2 -DDISABLE_P3 -DDISABLE_P4 -DDISABLE_P5 -DCHECK_DRAM_HAZARD -a $(MODEL)
	$(CC) $(CFLAGS) -DSAFETY -o pan-dram pan.c
	$(RUN) ./pan-dram -m$(DEPTH)

clean:
	rm -f pan pan-control pan-mutant pan-live pan-p1 pan-p2 pan-p3 pan-p4 pan-p5 pan-dram pan.* *.trail
