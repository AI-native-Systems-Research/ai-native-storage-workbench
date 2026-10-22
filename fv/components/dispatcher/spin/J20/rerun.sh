#!/bin/bash
# Independent re-run of J20: each re-synced model and its control, safety + liveness, fresh build, memory-limited pan.
export PATH=$HOME/.local/bin:$PATH
W=/tmp/j20rerun; rm -rf $W; mkdir -p $W
for m in no-lost-extents populate-lookup-linearizability promotion-atomicity write-before-evict; do
  for v in "" _control; do
    f=$(ls ~/fv-jobs/j20/$m/*${v}.pml | grep -v "_control" | head -1); [ -n "$v" ] && f=$(ls ~/fv-jobs/j20/$m/*_control.pml)
    d=$W/$m$v; mkdir -p $d; cp $f $d/m.pml; cd $d
    spin -a m.pml > spin_a.txt 2>&1 || { printf "%-36s %-9s SPIN-A FAILED\n" "$m$v" "-"; continue; }
    cc -O2 -DSAFETY -DMEMLIM=16000 -o pan pan.c 2>/dev/null; cc -O2 -DMEMLIM=16000 -o pan-live pan.c 2>/dev/null
    for t in safety liveness; do
      [ $t = safety ] && cmd="./pan -m200000" || cmd="./pan-live -a -m200000"
      /usr/bin/time -f "%e %M" -o t.$t timeout 600 $cmd > $t.txt 2>&1; rc=$?
      read wall rss < t.$t
      v1=$(grep -m1 -oE 'assertion violated[^(]*\([^)]*\)|acceptance cycle|invalid end state|non-progress cycle' $t.txt)
      printf "%-36s %-9s %-11s %-24s %6ss %8sKB  %s\n" "$m$v" $t "$(grep -oE 'errors: [0-9]+' $t.txt)" "$(grep -oE '[0-9]+ states, stored' $t.txt)" "$wall" "$rss" "$v1"
    done
  done
done
