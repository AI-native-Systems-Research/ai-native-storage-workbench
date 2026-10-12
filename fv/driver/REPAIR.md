# The repair agent - fix a bug that verification found, and prove the fix

## What it is for
Verification is useful when it leads to fixed bugs. The repair agent takes one finding (a property the
provers showed the code breaks) and produces a fix that a person can review: a test that shows the bug, the
code change, a proof that the fixed code keeps the property, and a short record of what was done.

## The steps (one agent session per attempt, at most 2 attempts)
| # | step | evidence |
|---|---|---|
| 1 | **Explain the problem**: one plain sentence, the spec and code locations that disagree, and a verdict: code wrong / spec wrong / both / needs a design decision. If it is not "code wrong", the agent stops here. | `problem.md` |
| 2 | **A test that reproduces it.** It must FAIL on today's code (a real failure, not a compile error), in its own file `src/**/repair_test_<name>.rs`. | the test file |
| 3 | **Fix the root cause** wherever it occurs; the test must now pass; the crate's tests are run 5 times. | the code change |
| 4 | **Prove the fix:** update the proof model of every function whose body changed, and add a proof of the property plus its mutant twin (which must fail). | the proof crate |
| 5 | **Report**: bug, impact, how to reproduce, the fix and the alternatives rejected, scope beyond the finding, risk, follow-ups. | `report.md` |

## Who decides: the acceptance check, never the agent
`fv/skills/component-repair/repair_accept_repo.sh` runs after the agent stops, from outside its reach:
- **builds**: every touched crate and every crate that depends on one (a crate whose native library is missing
  on this machine is listed as "not checked here", never silently passed);
- **tested**: the touched crates' tests pass in 5 runs out of 5;
- **red first**: the new test, put into a clean copy of the old code, fails there;
- **proved**: the property's new proof and the named callee modules prove; the twin fails;
- **model**: the proof model changed wherever a function it mirrors changed;
- **nothing lost**: every property proved before the repair is still proved after it (same gate, same scope).

If the check rejects an attempt, its output is saved and given to the next attempt. If the agent is blocked by
the environment (a missing library, a disabled kernel feature), it stops with `BLOCKED` and says why instead of
working around it.

## What it produces
- a **local commit** on the repair branch (never pushed; a person reviews it and opens the PR);
- `components/<c>/verif/fixes/fix_<component>_<finding>.md`: the header (component, finding, property, base and
  fix commits, status), the problem and verdict, how it was found, the agent's report, the check's evidence
  table, and the effort per attempt. It is the PR text and the source for the blog.

## How to run it
```
python3 fv/driver/fv_driver.py repair <checkout> <component> --property <ID> --finding <short-name> \
        [--callees <creusot modules>] [--attempts 2]
```
The checkout must be on a local branch, with the component's bundle and proof crate COMMITTED (the check compares
against that commit). Useful environment: `FV_GIT_NAME` / `FV_GIT_EMAIL` (commit author), `CERTUS_DEPS_DIR`
(native libraries such as SPDK), `ZYRE_BUILD_DIR`, `REPAIR_TEST_RUNS` (default 5). Run it inside a memory-capped
scope, as `fv_repair_queue.sh` does for a batch of repairs.

## Why this and not the generated agent
We first used a repair agent generated with a colleague's agent builder (vendored in
`fv/skills/component-repair/agent/`). On the same bug (zyre format strings) and the same acceptance check:

| | generated agent | this repair step |
|---|---|---|
| result | accepted on attempt 2 | accepted on attempt 1 |
| time | about 3 h 50 min, plus a manual stop | 1 h 07 min |
| machinery | its own runtime (~12 modules), approval loop | ~250 lines in our driver, our hooks |
| defects hit | 3 (inputs, rejection reasons not kept, "no approval answer" treated as rejection) | none |
| record | written by hand afterwards | generated |

The generated agent's second attempt did find a second bug (a race when two threads create a node), because its
tests happened to crash. That is why the check now runs the tests 5 times.
