MODEL = flush_clear.pml
PAN   = pan
CC    = cc
# MEMLIM (MB) and -w (hash bits) bound pan's memory (J19 memory-cap rule).
CFLAGS = -O2 -DMEMLIM=16000
# 5 procs + init + never claim: weak fairness needs NFAIR=3
LIVEFLAGS = -DNFAIR=3
DEPTH = 200000
CAP   = systemd-run --user --scope -q -p MemoryMax=60G -p MemorySwapMax=0 -p OOMPolicy=continue

.PHONY: all safety liveness clean

all: safety

# Safety: assertions (PA, PB, PC, PF) and invalid end states.
safety: $(PAN)
	$(CAP) ./$(PAN) -m$(DEPTH) -w24

# Liveness: the LTL claim L1 (the client finishes) under weak fairness, plus acceptance cycles.
liveness: $(PAN)-live
	$(CAP) ./$(PAN)-live -a -f -m$(DEPTH) -w24

$(PAN): $(MODEL)
	spin -a $(MODEL) && $(CC) $(CFLAGS) -DSAFETY -o $@ pan.c

$(PAN)-live: $(MODEL)
	spin -a -DLIVE $(MODEL) && $(CC) $(CFLAGS) $(LIVEFLAGS) -o $@ pan.c

clean:
	rm -f $(PAN) $(PAN)-live pan.* *.trail
