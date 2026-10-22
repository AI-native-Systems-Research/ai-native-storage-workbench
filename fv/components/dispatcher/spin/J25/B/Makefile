# Part B — failed cold promotion and its memory slot (Certus)
#
# All pan runs go through RUN: systemd-run memory cap (60G) + timeout (1800s),
# per the job rules. pan is compiled with -DMEMLIM=16000.
#
#   make control   — control copy (minimal fix): must be 0 errors, safety+liveness
#   make orig      — original code model, all properties: fails P1 (depth 43)
#   make prop-p1 … prop-p5  — per-property attribution runs on the original
#   make mutant-flip / mutant-dfree / mutant-drem — control rejection tests
#   make liveness-orig / liveness-control        — no_permanent_orphan
#   make all       — everything above
#   make clean

SPIN   = spin
CC     = cc
CFLAGS = -O2 -DMEMLIM=16000
RUN    = systemd-run --user --scope -q -p MemoryMax=60G -p MemorySwapMax=0 \
         -p OOMPolicy=continue timeout 1800

ORIG   = failed_promotion_slot.pml
CTRL   = failed_promotion_slot_control.pml

.PHONY: all control orig prop-p1 prop-p2 prop-p3 prop-p4 prop-p5 \
        mutant-flip mutant-dfree mutant-drem liveness-orig liveness-control clean

all: control orig prop-p1 prop-p3 prop-p5 prop-p2 prop-p4 \
     mutant-flip mutant-dfree mutant-drem liveness-orig liveness-control

# ---- control (minimal fix): must pass safety AND liveness with 0 errors ----
control:
	$(SPIN) -a $(CTRL)
	$(CC) $(CFLAGS) -DSAFETY -o pan_control pan.c
	$(RUN) ./pan_control -m100000
	$(SPIN) -DLIVENESS -a $(CTRL)
	$(CC) $(CFLAGS) -o pan_control_live pan.c
	$(RUN) ./pan_control_live -a -f -m100000

# ---- original, all properties: first failure is P1 (depth 43) ----
orig:
	$(SPIN) -a $(ORIG)
	$(CC) $(CFLAGS) -DSAFETY -o pan_orig pan.c
	$(RUN) ./pan_orig -m100000

# ---- per-property attribution runs on the original ----
# P1 violated (depth 43); P3 violated (depth 48); P5 violated (depth 46);
# P2 and P4 hold (0 errors with P1/P3/P5 off).
prop-p1:
	$(SPIN) -a $(ORIG)
	$(CC) $(CFLAGS) -DSAFETY -o pan_orig pan.c
	$(RUN) ./pan_orig -m100000

prop-p3:
	$(SPIN) -DNO_P1 -DNO_P5 -a $(ORIG)
	$(CC) $(CFLAGS) -DSAFETY -o pan_orig pan.c
	$(RUN) ./pan_orig -m100000

prop-p5:
	$(SPIN) -DNO_P1 -DNO_P3 -a $(ORIG)
	$(CC) $(CFLAGS) -DSAFETY -o pan_orig pan.c
	$(RUN) ./pan_orig -m100000

prop-p2 prop-p4:
	$(SPIN) -DNO_P1 -DNO_P3 -DNO_P5 -a $(ORIG)
	$(CC) $(CFLAGS) -DSAFETY -o pan_orig pan.c
	$(RUN) ./pan_orig -m100000

# ---- control mutants (rejection tests) ----
mutant-flip:
	$(SPIN) -DMUTANT_FLIP_BAD_SLOT -a $(CTRL)
	$(CC) $(CFLAGS) -DSAFETY -o pan_mut pan.c
	$(RUN) ./pan_mut -m100000

mutant-dfree:
	$(SPIN) -DMUTANT_DOUBLE_FREE -a $(CTRL)
	$(CC) $(CFLAGS) -DSAFETY -o pan_mut pan.c
	$(RUN) ./pan_mut -m100000

mutant-drem:
	$(SPIN) -DMUTANT_DOUBLE_REMOVE -a $(CTRL)
	$(CC) $(CFLAGS) -DSAFETY -o pan_mut pan.c
	$(RUN) ./pan_mut -m100000

# ---- liveness (never claim no_permanent_orphan, weak fairness) ----
liveness-orig:
	$(SPIN) -DLIVENESS -DNO_P1 -DNO_P3 -DNO_P5 -a $(ORIG)
	$(CC) $(CFLAGS) -o pan_orig_live pan.c
	$(RUN) ./pan_orig_live -a -f -m100000

liveness-control:
	$(SPIN) -DLIVENESS -a $(CTRL)
	$(CC) $(CFLAGS) -o pan_control_live pan.c
	$(RUN) ./pan_control_live -a -f -m100000

clean:
	rm -f pan pan.c pan.b pan.h pan.m pan.p pan.t pan_orig pan_control pan_mut \
	      pan_orig_live pan_control_live pan_mut_flip pan_mut_dfree pan_mut_drem \
	      pan-control pan-p2 pan-p3 pan-p4 pan-p5 _spin_nvr.tmp trail.tmp
