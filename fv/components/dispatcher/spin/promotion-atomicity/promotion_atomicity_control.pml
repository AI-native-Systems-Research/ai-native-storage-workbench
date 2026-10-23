/*
 * CONTROL copy of promotion_atomicity.pml: same model, with the three
 * minimal code fixes switched on (see the FIX_* hooks in the main file and
 * RESULT.md). Not a model of the shipped code.
 *   FIX_RECOVER  lookup_async (lib.rs:3063-3068) maps AlreadyExists from
 *                promote_and_serve to serve_concurrently_promoted (1349-1388),
 *                as batch_lookup does at 2937-2945.
 *   FIX_UNDO     promote_and_serve calls mt.remove(key) before returning Err
 *                after a successful evict_and_insert (883, 904).
 *   FIX_PIN      lookup_async releases the read pin (3064) only after
 *                promote_and_serve returns, and BackgroundEvictor
 *                (background.rs:327-333) calls mt.remove only after dm.remove
 *                succeeded.
 */
#define FIX_RECOVER 1
#define FIX_UNDO    1
#define FIX_PIN     1
#include "promotion_atomicity.pml"
