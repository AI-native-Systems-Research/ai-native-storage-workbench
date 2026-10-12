#!/bin/bash
# fv overnight repair queue: one repair at a time, each in its own memory-capped scope; local commits only.
# usage: fv_repair_queue.sh <cap> <job> [<job> ...]   job = worktree|component|property|finding|callees[|needs-io_uring]
export PATH="$HOME/.local/share/creusot/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
export FV_GIT_NAME="Cornel Constantinescu" FV_GIT_EMAIL="cornel@green.sg.almaden.ibm.com"
export CERTUS_DEPS_DIR=$HOME/ai-native-storage-certus/deps REPAIR_TEST_RUNS=5
[ -d /opt/zyre-build ] && export ZYRE_BUILD_DIR=/opt/zyre-build
CAP=$1; shift
for job in "$@"; do
  IFS='|' read -r wt comp prop finding callees needs <<< "$job"
  echo "[$(date '+%F %T')] START $comp $finding ($prop)"
  if [ "$needs" = "needs-io_uring" ] && [ "$(cat /proc/sys/kernel/io_uring_disabled)" != "0" ]; then
    echo "[$(date '+%F %T')] SKIP $comp $finding: io_uring is disabled on this host (kernel.io_uring_disabled=$(cat /proc/sys/kernel/io_uring_disabled))"
    continue
  fi
  /usr/bin/time -f "[time] %e s wall, %M KB peak" systemd-run --user --scope --quiet --unit="fv-repair-$comp-$finding" \
    -p MemoryMax=$CAP -p MemorySwapMax=0 -p OOMPolicy=continue \
    python3 $HOME/ai-native-storage-workbench/fv/driver/fv_driver.py repair "$wt" "$comp" \
      --property "$prop" --finding "$finding" --callees "$callees" --attempts 2
  echo "[$(date '+%F %T')] END $comp $finding rc=$?"
done
echo "[$(date '+%F %T')] QUEUE DONE"
