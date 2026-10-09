---
name: tools-kani-install
description: Install the Kani model checker at the exact version the FV pipeline's results were produced with (cargo-kani 0.67.0), and check it works. Use before the first Kani run on a new machine, or when `cargo kani --version` differs - results from different Kani versions are not comparable.
---

## Why the version is pinned
Every published Kani result records the tool version in its `run:` block. A different Kani version can prove or
time out differently, so a second machine must match exactly before its results are compared or merged.

## Install (user-level, no system packages)
```
cargo install --locked kani-verifier --version 0.67.0
cargo kani setup
cargo kani --version          # must print: cargo-kani 0.67.0
```
`cargo kani setup` downloads Kani's own Rust toolchain and CBMC into `~/.kani/`; it needs network access once.

## Smoke test (about a minute)
```
cargo new --lib /tmp/kani-smoke && cd /tmp/kani-smoke
cat >> src/lib.rs <<'RS'
#[cfg(kani)]
#[kani::proof]
fn add_does_not_overflow() {
    let a: u8 = kani::any(); let b: u8 = kani::any();
    kani::assume(a < 100 && b < 100);
    assert!(a.checked_add(b).is_some());
}
RS
cargo kani            # expect: VERIFICATION:- SUCCESSFUL
```

## Notes from the project
- Run harnesses with unwinding checks ON by default. `--no-unwinding-checks` can prune every path and make a proof
  pass with no content; the gate gives such a proof no credit without a failing mutant twin.
- Several `cargo kani` processes sharing one target directory corrupt each other's build output: run one invocation
  with `-j`, or give each process its own `CARGO_TARGET_DIR`.
- The scorer adds `~/.local/share/creusot/bin` to PATH by default; set `FV_CREUSOT_BIN` if Creusot lives elsewhere.
