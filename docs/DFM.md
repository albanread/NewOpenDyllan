# NewOpenDylan DFM IR — design stub

*The DFM IR — the typed-SSA contract at the permanent front-end/back-end boundary — has been implemented and stable since Sprint 06, and every compiler stage now targets it. Maintained reference: [`manual/compiler/dfm.md`](manual/compiler/dfm.md) and the `nod-dfm` crate.*

The **Dylan Flow Machine** (DFM) is the typed SSA IR sitting between the
namespace-resolved AST and LLVM IR. Inspired by upstream Open Dylan's DFMC
pass tree (`E:\opendylan\sources\dfmc\`), adapted for a Rust+LLVM JIT.

Shape:

- **SSA-style** with explicit basic blocks; one terminator per block.
- **Typed values** — every DFM value carries a Dylan type. Sealing analysis
  refines these toward concrete classes for direct-call lowering.
- **Generic-function dispatch is first-class** in the IR — separate node kinds
  for sealed-direct, sealed-cached, and unsealed-dispatch calls. This makes
  the optimiser's job visible.
- **Phase-stable textual dump** via `format_dfm(module) -> String`, exposed
  through the `nod-driver dump-dfm` subcommand and rendered live in the IDE
  DFM panel.

Open questions:
- How many optimisation passes live in `nod-opt` vs in LLVM downstream?
- Library-merge optimisation (cross-module sealing) — IR or driver level?
- Inline cache representation for unsealed generics.

See PLAN.md §4 (compiler architecture) and SPRINTS.md Sprint 06.
