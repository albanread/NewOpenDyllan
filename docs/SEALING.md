# NewOpenDylan Sealing — design stub

*Sealed dispatch shipped (Sprint 14+): sealing is planned and enforced in sema, enabling devirtualised calls. Maintained reference: [`manual/compiler/sema.md`](manual/compiler/sema.md) (dispatch + sealing).*

**Sealing is load-bearing** — it is what makes Dylan "feel dynamic, compile
like static" (MANIFESTO.md §Core decisions #10). The sealing analyser lives
in `nod-sema`; its outputs flow into `nod-dfm` to pick direct-call vs
inline-cached vs full-dispatch lowering.

Surface the user sees:

- `define sealed class` / `define open class` — class-level seal.
- `define sealed domain` — generic-function-level seal over a class tuple.
- `sealed method` / `open method` — method-level seal.

What the analyser computes:

- For each generic function: the **sealing closure** — the set of
  `(class-tuple → method)` mappings that the optimiser can rely on remaining
  stable across the current library.
- For each call site: whether dispatch can be **resolved to a single method
  at compile time** (sealed-direct), **resolved to a small cached set**
  (sealed-cached), or **must go through full multimethod dispatch**.

The **sealed-domain visualiser** in the IDE (MANIFESTO §Live inspection) is a
direct view of these tables.

Library boundaries:

- Sealing decisions are **library-local by default**. Cross-library sealing
  is opt-in via explicit `define sealed domain` declarations.
- **Sealing breaks** during incremental compilation produce structured
  diagnostics; the compiler refuses the patch until the user acknowledges
  the dependent-code invalidation (MANIFESTO §Live incremental compilation).

See SPRINTS.md Sprints 13–16.
