# Verified components: models and results

One folder for each Certus component that we verified. Each folder holds the models that each tool checks
(Creusot, Kani, Loom, Spin), the property bundle, and a README with the results and the insights.

The models change when the code changes. Each README names the Certus commit that its models follow. A model that
does not match the current code can pass and still miss a bug (see `dispatcher/README.md`, insight 2).

| component | Creusot | Kani | Loom | Spin | confirmed bugs | state |
|---|---|---|---|---|---|---|
| [dispatcher](dispatcher/) | 111 proved | 20 bugs confirmed | 6 hold, 5 bugs + repair tests | 5 new models, 4 re-synced | 10 merged, 1 accepted | 2026-10-22 |

The other components (extent-manager, memory-tier, dispatch-map, logger, the eviction policies, and more) are on the
`verif/creusot/<component>` and `verif/kani/<component>` branches of the Certus repository. We move them here one by
one.

How a folder is updated: at the end of each verification or repair job, the hand-written model files are copied here
from the job's work folder, with the Certus commit. The files that the tools generate (for example Creusot's `.coma`
files) stay on the `verif/<tool>/<component>-<date>` branches, where the full set can be run again.
