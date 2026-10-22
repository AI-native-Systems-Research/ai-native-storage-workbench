# Spin, re-synced: Daniel's 4 old models fail on the code of today

We re-synced Daniel's 4 old Promela models to the code of today, and all 4 fail. They show 7 candidate bugs. Creusot,
Kani and Loom did not find 3 of these bugs. We re-ran every result on green (the notes are at the end).

## Results (re-run on green, Spin 6.5.2, unstable acd24025)
| model | code of today | with the minimal fixes ("control") |
|---|---|---|
| no-lost-extents | **1 error** (0.03 s) | 0 errors, 9.2 M states, 5.7 s |
| populate-lookup-linearizability | **1 error** (0.03 s) | 0 errors, 41.9 M states, 35 s |
| promotion-atomicity | **1 error** (0.03 s) | 0 errors, 2.8 M states, 1.2 s |
| write-before-evict | **1 error** (0.04 s) | 0 errors, 8.6 M states, 4.1 s |

Safety and liveness give the same result in every case. In the control rows, "invalid end state" is the name of a
check that was on. It is not an error: the errors are 0.

## The bugs that Spin found, compared with the results of the other tools
| # | bug (plain words) | code | Spin model | already known? |
|---|---|---|---|---|
| S1 | `remove` leaks the SSD extent of a key when the SSD write of that key finished while the key was still in memory | `lib.rs:3105-3116` | no-lost-extents | **yes**: Kani #9 (`remove` releases an extent only for BlockDevice entries) |
| S2 | `populate` leaks its memory slot when the GPU-stream sync fails | `lib.rs:3182-3194` | populate-lookup-linearizability | **yes**: Creusot #10, **fixed last night** (64f97f7a0) |
| S3 | **a cold `lookup` releases its pin before the SSD read**. Thus a remove can delete the entry and release its slot while NVMe still writes into the slot (use-after-free) | `lib.rs:3064-3065` | populate-lookup-linearizability | **NEW** |
| S4 | the **SSD evictor releases the memory slot of a key before** it checks that `dm.remove` succeeds. If the key is pinned, the slot is gone all the same | `background.rs:328-331` | populate-lookup-linearizability, promotion-atomicity, write-before-evict | **NEW** (3 models found it independently) |
| S5 | two cold lookups of one key: the caller of the second lookup gets **`AlreadyExists`** | `lib.rs:3064`, `1208-1217` | promotion-atomicity | **NEW** (same root cause as S3: the code releases the pin too early) |
| S6 | a **failed promotion leaks the memory slot** (the SSD read fails, and no code calls `mt.remove`) | `lib.rs:882-884`, `830`, `851-855` | promotion-atomicity | **yes**: Creusot #11 (same function, map-promotion failure). Spin adds the SSD-read path and the channel-checkout path |
| S7 | the "blocked unpersisted" counter counts every type of eviction refusal | spec 002 FR-033 | write-before-evict | **yes**: Kani #22 |
| (S8) | the key `u64::MAX` gives the map a dangling SSD offset | P6 | no-lost-extents | edge case, minor |

**Spin found 2 new root causes.** S3 and S5 share one root cause: the cold path releases its pin before it promotes.
The second root cause is S4: the SSD evictor releases the slot before it checks. Both root causes can cause a
use-after-free on DRAM slots that a DMA can still write.

## What this shows
- **The result "all 8 models pass" had little value.** The 4 models that encode a removed protocol passed because
  they did not describe the code any more. After the re-sync, every one of them fails in less than 0.05 s.
- **Four tools give evidence that overlaps.** Creusot or Kani found S1, S2, S6 and S7 before. Spin confirms them on a
  model of the full protocol, with many threads and the background evictors together. The other tools cannot do
  this at that scale.
- **Spin found bugs that the other tools did not find** (S3, S4, S5). All three bugs need a lookup, an evictor and a
  remove that run at the same time. Our Loom runs had a maximum of two actors per test, and Kani cannot model
  threads.

## Notes and caveats
- I re-ran the models on green from the files of node7. I ran every model and control, for safety and liveness,
  with a fresh build and `-DMEMLIM=16000`. All 16 results agree with the results of node7. One control model
  (`promotion_atomicity_control.pml`) includes the main model file. Thus you must run it in the same folder (my first
  run did not do this).
- I examined the three code locations for S3, S4 and S6 myself (`lib.rs:3062-3065`, `background.rs:327-332`,
  `lib.rs:882-884`). They agree with the reports.
- These bugs are **candidates**, not confirmed bugs, because a Promela model is an abstraction that a person wrote.
  A bug becomes confirmed when the test of a repair fails on the code of today.
- The model files, the trails and the full reports of node7 are in `~/fv-jobs/j20/` (a copy of node7:`~/FV/J20/`).
  The re-run on green is in `~/fv-jobs/j20/green_rerun.txt`.
