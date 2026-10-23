MODEL = promotion_lookup_race.pml
PAN   = pan
CC    = cc
CFLAGS = -O2 -DMEMLIM=16000
DEPTH = 200000
PANFLAGS = -m$(DEPTH) -w26

.PHONY: all safety liveness clean

all: safety

safety: $(PAN)
	./$(PAN) $(PANFLAGS)

liveness: $(PAN)-live
	./$(PAN)-live -a -m$(DEPTH) -w26

$(PAN): pan.c
	$(CC) $(CFLAGS) -DSAFETY -o $@ $<

$(PAN)-live: pan.c
	$(CC) $(CFLAGS) -o $@ $<

pan.c: $(MODEL)
	spin -a $<

clean:
	rm -f $(PAN) $(PAN)-live pan.* *.trail
