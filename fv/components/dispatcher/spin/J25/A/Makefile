MODEL_ORIG = clear_memory_tier.pml
MODEL_CTRL = clear_memory_tier_control.pml
CC    = cc
CFLAGS = -O2 -DMEMLIM=16000
DEPTH = 100000
# Pan must ALWAYS run under the memory-cap + time-limit wrapper:
RUN = systemd-run --user --scope -q -p MemoryMax=60G -p MemorySwapMax=0 \
      -p OOMPolicy=continue timeout 1800
MUTANT = MUTANT_RELEASE_PINS

.PHONY: all safety control orig prop-a prop-b prop-c mutant liveness clean

all: control

# Control (fixed) build: expected 0 errors, full search.
control:
	spin -a $(MODEL_CTRL)
	$(CC) $(CFLAGS) -DSAFETY -o pan-control pan.c
	$(RUN) ./pan-control -m$(DEPTH)

# Original code (known-bad, the skill's "mutant"): expected violations of
# properties (a) and (b). This is the faithful model of the current code.
orig:
	spin -a $(MODEL_ORIG)
	$(CC) $(CFLAGS) -DSAFETY -o pan pan.c
	$(RUN) ./pan -m$(DEPTH)

safety: orig

# Per-property attribution runs on the ORIGINAL model.
prop-a:
	spin -DNO_B -DNO_C -a $(MODEL_ORIG)
	$(CC) $(CFLAGS) -DSAFETY -o pan-a pan.c
	$(RUN) ./pan-a -m$(DEPTH)

prop-b:
	spin -DNO_A -DNO_C -a $(MODEL_ORIG)
	$(CC) $(CFLAGS) -DSAFETY -o pan-b pan.c
	$(RUN) ./pan-b -m$(DEPTH)

prop-c:
	spin -DNO_A -DNO_B -a $(MODEL_ORIG)
	$(CC) $(CFLAGS) -DSAFETY -o pan-c pan.c
	$(RUN) ./pan-c -m$(DEPTH)

# Known-bad mutant of the CONTROL (property (c) rejection test): a plausible
# wrong fix that force-releases outstanding pins before removing. Expected:
# assertion violated (clrmypin).
mutant:
	spin -D$(MUTANT) -a $(MODEL_CTRL)
	$(CC) $(CFLAGS) -DSAFETY -o pan-mutant pan.c
	$(RUN) ./pan-mutant -m$(DEPTH)

# Liveness on the control: once both bounded actors are done, clear must
# eventually return. Expected: 0 errors.
liveness:
	spin -DLIVENESS -a $(MODEL_CTRL)
	$(CC) $(CFLAGS) -o pan-live pan.c
	$(RUN) ./pan-live -a -m$(DEPTH)

clean:
	rm -f pan pan-a pan-b pan-c pan-control pan-mutant pan-live pan.* *.trail
