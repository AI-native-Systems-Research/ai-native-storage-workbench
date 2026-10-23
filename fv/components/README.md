# Verified components: models and results

One folder for each Certus component that we verified. Each folder holds the models that each tool checks
(Creusot, Kani, Loom, Spin), the property bundle, and a README with the results and the insights.

The models change when the code changes. Each README names the Certus commit that its models follow. A model that
does not match the current code can pass and still miss a bug (see `dispatcher/README.md`, insight 2).

| component | Creusot | Kani | Loom | Spin | confirmed bugs | state |
|---|---|---|---|---|---|---|
| [dispatcher](dispatcher/) | 111 proved | 20 bugs confirmed | 6 hold, 5 bugs + repair tests | 5 new models, 4 re-synced | 12 merged, 2 accepted, 4 confirmed by a test | 2026-10-23 |

The other components (extent-manager, memory-tier, dispatch-map, logger, the eviction policies, and more) are on the
`verif/creusot/<component>` and `verif/kani/<component>` branches of the Certus repository. We move them here one by
one.

## Newest verification branch of each component

The branches are in the Certus repository. "Newest" means the latest commit date; an old branch name has no date,
and a newer run adds the date (`-YYYYMMDD`). The date and the short commit in brackets are of the branch's last
commit. Generated on 2026-10-23 with `fv/tools/verif_branch_table.py <certus-checkout>` (after `git fetch`); run it
again after each publish.

| component | newest creusot branch | newest kani branch | newest loom branch | newest spin branch |
|---|---|---|---|---|
| block-device-filesys | `verif/creusot/block-device-filesys` (2026-10-07, 217c0a49e) | `verif/kani/block-device-filesys` (2026-10-07, b1bb31e58) | — | — |
| disk-partition-manager | `verif/creusot/disk-partition-manager-20261001` (2026-10-08, 8e7d1e3ba) | `verif/kani/disk-partition-manager-20261001` (2026-10-08, 0da9826b8) | — | — |
| dispatch-map | `verif/creusot/dispatch-map-20261006` (2026-10-07, 09cbcabfc) | `verif/kani/dispatch-map-20261006` (2026-10-07, 7ec297031) | — | — |
| dispatcher | `verif/creusot/dispatcher` (2026-08-24, 6e0d28cf7) | — | — | — |
| eviction-policy-lru | `verif/creusot/eviction-policy-lru` (2026-10-07, 07596ee3a) | `verif/kani/eviction-policy-lru` (2026-10-09, f295072f4) | — | — |
| eviction-policy-optimized | `verif/creusot/eviction-policy-optimized` (2026-10-08, d05d263b1) | `verif/kani/eviction-policy-optimized` (2026-10-08, 580adb4f8) | — | — |
| eviction-policy-session-lists | `verif/creusot/eviction-policy-session-lists` (2026-10-08, 25c963b16) | `verif/kani/eviction-policy-session-lists` (2026-10-08, feeda00b7) | — | — |
| extended-metadata-store | `verif/creusot/extended-metadata-store` (2026-10-07, af3c7fad5) | `verif/kani/extended-metadata-store` (2026-10-09, 011ef7e89) | — | — |
| extent-manager | `verif/creusot/extent-manager-20261008` (2026-10-08, 8a0f3ab93) | `verif/kani/extent-manager-20261008` (2026-10-08, 6ece9adf7) | — | — |
| logger | `verif/creusot/logger` (2026-10-07, 8b5bf208c) | `verif/kani/logger` (2026-10-09, 2f6df376a) | — | — |
| memory-tier | `verif/creusot/memory-tier-20261003` (2026-10-08, 3dcd6c9ac) | `verif/kani/memory-tier-20261003` (2026-10-08, 36d12edf1) | — | — |
| remote-lookup | `verif/creusot/remote-lookup` (2026-10-07, 59d4812d1) | `verif/kani/remote-lookup` (2026-10-07, 871293a6e) | — | — |

**Not on GitHub yet:** the dispatcher verification of 2026-10-15 and its repairs (local branches on our machines). The
branch `verif/creusot/dispatcher` above is from August and out of date; the current dispatcher models are in
`dispatcher/` in this folder. No component has a Loom or Spin branch yet.

How a folder is updated: at the end of each verification or repair job, the hand-written model files are copied here
from the job's work folder, with the Certus commit. The files that the tools generate (for example Creusot's `.coma`
files) stay on the `verif/<tool>/<component>-<date>` branches, where the full set can be run again.
