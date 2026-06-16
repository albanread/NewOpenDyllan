//! NewOpenDylan compiler driver.
//!
//! Sprint 02: `dump-tokens` lights up. `compile` and `repl` are still
//! stubs; they land in later sprints.
//!
//! # Platform notes
//!
//! The `build` subcommand invokes `link.exe` with Windows `.lib`
//! import libraries. That linker invocation is the main Windows-
//! specific surface in this crate. The macOS variant will swap it for
//! `clang` / `ld` with `-framework`/`-l` flags. See
//! `docs/PLATFORMS.md` for the platform-strategy policy.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod dylan_lex_jit;
mod dylan_parse_check;
mod dylan_parse_wire;
mod dylan_to_ast;
mod project;

/// LLVM major version this driver is targeted against. Read at
/// `--version` time; the inkwell linkage itself lights up in Sprint 06.
const LLVM_VERSION: &str = "22.1";

/// NewOpenDylan compiler driver.
#[derive(Parser)]
#[command(
    name = "nod-driver",
    version = env!("CARGO_PKG_VERSION"),
    long_version = concat!(env!("CARGO_PKG_VERSION"), " (LLVM 22.1)"),
    about = "NewOpenDylan compiler driver",
    long_about = "NewOpenDylan: a from-scratch Rust+LLVM JIT for the Dylan language.\n\
                  See PLAN.md and SPRINTS.md in the workspace root.",
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Sprint 51b — JIT-strap the Dylan-side lexer
    /// (`tests/nod-tests/fixtures/dylan-lexer.dylan` +
    /// `dylan-lex-shim.dylan`) into an isolated MCJIT engine and
    /// install it as `nod_reader::lex`'s override.
    ///
    /// Effect: every subsequent lex call inside this driver process
    /// (build, dump-ast, eval, dump-dfm, dump-llvm — anything that
    /// runs the front-end) dispatches through Dylan-compiled code
    /// instead of the Rust `lex` in `nod-reader`. The Rust lexer
    /// stays compiled in — it's the canonical fallback if init fails
    /// and the reference path the oracle test compares against.
    ///
    /// Cost: ~3 s one-shot for the JIT compile on first call;
    /// subsequent lex calls run from the leaked MCJIT engine. There's
    /// no on-disk cache yet (the shim sources are small enough that
    /// the in-process JIT is the sweet spot for v1 — a future sprint
    /// can wire Sprint 37's bitcode cache into this path).
    ///
    /// Also settable via the `NOD_LEX_WITH_DYLAN=1` environment
    /// variable, for use from `cargo test` etc.
    #[arg(long = "lex-with-dylan", global = true)]
    lex_with_dylan: bool,

    /// Sprint 51c — verify-mode parser side-load. For each
    /// `parse_module` call in this driver process, also run the
    /// Dylan-side parser (`dylan-parse-collect`) on the same source
    /// and compare verdicts. Disagreement is a hard error; agreement
    /// is silent.
    ///
    /// Implies `--lex-with-dylan` (the parser shares the lexer's
    /// resolver). Also settable via `NOD_VERIFY_PARSE=1`.
    ///
    /// This is the cheapest way to exercise the Dylan parser in
    /// production: every build, every dump-ast call, both parsers
    /// run, and we surface divergence loudly. Once the AST wire
    /// format lands (Sprint 51d), the Dylan parser becomes the
    /// authoritative path and this flag retires.
    #[arg(long = "verify-parse", global = true)]
    verify_parse: bool,

    /// Sprint 51e — authoritative-mode parser side-load. Run the
    /// Dylan-side parser, translate its AST wire output into the
    /// canonical `ast::Module` (`dylan_to_ast`), and USE that as the
    /// parse result — replacing `parse_module` for files the
    /// translator fully understands. Any construct the translator
    /// can't yet reconstruct (unknown kind, modifiers, variadic params,
    /// statement bodies, …) makes the whole file fall back to the Rust
    /// parser, so the output is never wrong — only "translated" or
    /// "fell back".
    ///
    /// Implies `--lex-with-dylan` (shared resolver). Also settable via
    /// `NOD_PARSE_WITH_DYLAN=1`. Currently wired into `dump-ast`; the
    /// translation-coverage harness asserts the Dylan path's AST is
    /// byte-identical to the Rust parser's on every fixture.
    #[arg(long = "parse-with-dylan", global = true)]
    parse_with_dylan: bool,

    /// Sprint 51e.6 — opt OUT of the now-default Dylan parser. The Dylan
    /// parser is the default real-pipeline front-end (Rust stays the
    /// verify oracle + the per-file fall-back); this flag (or
    /// `NOD_PARSE_WITH_RUST=1`) forces the legacy Rust parser as the
    /// authoritative path for `compile`/`eval`/`build`.
    #[arg(long = "parse-with-rust", global = true)]
    parse_with_rust: bool,

    /// Sprint 54c — make the Dylan sema recording load-bearing: the back-end
    /// consumes the model the Dylan walk produces (in-process via the
    /// `dylan-sema-emit` shim entry) instead of the Rust recompute. Also
    /// settable via `NOD_SEMA_WITH_DYLAN=1`. Currently wired into `dump-dfm`
    /// (gated `dump-dfm --sema-with-dylan` byte-identical to plain `dump-dfm`).
    /// Requires the shim to be statically linked.
    #[arg(long = "sema-with-dylan", global = true)]
    sema_with_dylan: bool,

    /// Sprint 55 — make the Dylan AST→DFM lowering load-bearing: the back-end
    /// consumes the DFM the Dylan lowering produces (in-process via the
    /// `dylan-lower-emit` shim entry, reconstructed host-side) instead of the
    /// Rust Phase-3/4 lowering, then runs the SAME passes on it. Also settable
    /// via `NOD_LOWER_WITH_DYLAN=1`. Wired into `dump-dfm` (gated
    /// `dump-dfm --lower-with-dylan` byte-identical to plain `dump-dfm` on the
    /// covered subset; the Dylan lowering bails to Rust otherwise). Requires the
    /// shim to be statically linked.
    #[arg(long = "lower-with-dylan", global = true)]
    lower_with_dylan: bool,

    /// Sprint 56 — the combined front-end flag: compose `--sema-with-dylan` +
    /// `--lower-with-dylan` + Dylan macro expansion (`NOD_EXPAND_WITH_DYLAN`) so
    /// the WHOLE front-end (lex / parse / expand / sema / AST→DFM lowering) runs
    /// in Dylan, with the back-end consuming a single Dylan-produced handoff.
    /// The lowering still bails per-module to Rust on any uncovered form. Also
    /// settable via `NOD_FRONTEND_WITH_DYLAN=1`. Requires the shim to be linked.
    #[arg(long = "frontend-with-dylan", global = true)]
    frontend_with_dylan: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Compile a Dylan source file or LID-rooted library. Not yet implemented.
    Compile {
        /// Path to a `.dylan` file or a `.lid` library manifest.
        input: Option<PathBuf>,
    },
    /// Sprint 39a — compile a Dylan source file (or, Sprint 44, set of
    /// source files in the same module) to a standalone Windows EXE.
    /// Pipeline: parse → expand → lower → codegen → AOT entry-stub
    /// injection → emit `.obj` → link against `nod_runtime.lib`.
    ///
    /// **Multi-file (Sprint 44):** pass more than one positional path
    /// to merge them into one build. Every input file's `Module:`
    /// header must declare the same module name; cross-file collisions
    /// (two files defining the same top-level function) are an error.
    /// Files are lowered front-to-back, so later files can reference
    /// classes/methods defined in earlier files. The default output
    /// name is derived from the FIRST positional path.
    ///
    /// Out of scope: cross-module imports (waits for a real Dylan
    /// library system — see DEFERRED.md).
    Build {
        /// One or more `.dylan` source files. Exactly one of them must
        /// contain `define function main` (the EXE entry point).
        /// Either pass positional inputs OR `--project <foo.prj>` —
        /// never both. `clap` enforces this via the `conflicts_with`
        /// attribute below.
        #[arg(required_unless_present = "project", conflicts_with = "project")]
        inputs: Vec<PathBuf>,
        /// Output EXE path. Defaults to `<first input stem>.exe`, or
        /// the project file's `output` field when `--project` is used.
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
        /// Sprint 49 — load build inputs from a `.prj` project file.
        /// Relative paths inside the file are anchored at the project
        /// file's directory. Mutually exclusive with positional
        /// `inputs`.
        #[arg(long = "project")]
        project: Option<PathBuf>,
        /// Sprint 49 — print wall-clock stage timings (parse+lower,
        /// codegen, emit-object, link) to stderr after the build
        /// finishes. Inert when off.
        #[arg(long = "time")]
        time: bool,
        /// Print the chosen target triple, object path, and linker
        /// command before invoking it.
        #[arg(long = "verbose")]
        verbose: bool,
        /// Sprint 51b — emit a statically-linkable `.obj` instead of a
        /// standalone EXE. Skips the synthetic `i32 @main()` injection
        /// and the user-entry → `nod_user_main` rename, so the object
        /// can be linked into a host binary (e.g. `nod-driver` itself,
        /// for `--lex-with-dylan`) without colliding on `main`. The
        /// resolver `nod_aot_resolve_relocs` is still emitted; the host
        /// must call it once before invoking any of the Dylan-side
        /// functions. `--output` should point at the desired `.obj`
        /// path; linking is skipped entirely.
        #[arg(long = "library")]
        library: bool,
    },
    /// Start an interactive REPL. Not yet implemented.
    Repl,
    /// Lex a Dylan source file and print the token stream.
    ///
    /// Output format is fixed by `specs/01-lexer.md` §5 — line-oriented,
    /// stable, suitable for diffing.
    DumpTokens {
        /// Path to a `.dylan` source file.
        input: PathBuf,
    },
    /// Lex + parse a Dylan source file and print the AST.
    DumpAst {
        /// Path to a `.dylan` source file.
        input: PathBuf,
    },
    /// Load a `.lid` (resolving any `LID:` include chain) and print the
    /// library/module graph as Graphviz.
    DumpGraph {
        /// Path to a `.lid` file.
        input: PathBuf,
    },
    /// Lex + parse + lower a Dylan source file and print the DFM IR.
    DumpDfm {
        /// One or more `.dylan` source files. With more than one path the
        /// files are compiled together as a single library unit (AST-level
        /// merge), so cross-file references resolve. A single path is the
        /// original per-file behaviour, byte-for-byte.
        #[arg(required = true, num_args = 1..)]
        input: Vec<PathBuf>,
    },
    /// Sprint 53 — print the sema recording model (top-names, generics,
    /// classes, sealing) as deterministic text. The byte-identical oracle
    /// for the Dylan sema port.
    DumpSema {
        /// Path to a `.dylan` source file.
        input: PathBuf,
    },
    /// Lex + parse + lower + codegen a Dylan source file; print textual LLVM IR.
    DumpLlvm {
        /// Path to a `.dylan` source file.
        input: PathBuf,
    },
    /// Parse + lower + codegen + JIT one Dylan expression; print the result.
    Eval {
        /// Dylan expression source.
        expr: String,
    },
    /// Sprint 45a — run the Dylan-in-Dylan lexer over the input file and
    /// print the canonical token dump to stdout.
    ///
    /// The lexer source itself lives at
    /// `tests/nod-tests/fixtures/dylan-lexer.dylan` and is baked into
    /// the driver via `include_str!` so this subcommand works from
    /// anywhere on disk, not just inside the repo. The 45a stub `lex`
    /// returns one `<eof-token>`; the canonical dump for any input is
    /// therefore exactly `1:1-1:1  EOF\n` until 45b lands the real
    /// implementation.
    DumpDylanTokens {
        /// Path to a `.dylan` source file.
        input: PathBuf,
        /// Ask the lexer process to print GC stats to stderr after dumping tokens.
        #[arg(long = "gc-stats")]
        gc_stats: bool,
    },
    /// Sprint 51d — Dylan-side parser side-load: lex + parse the input
    /// through the statically-linked shim, decode the AST wire format
    /// (`docs/DYLAN_AST_WIRE.md`), print the resulting tree as an
    /// indented S-expression. v1 supports a small subset of node
    /// kinds (`Body`, `DefineFunction`, `Call`, `VariableRef`,
    /// `StringLit`, `IntegerLit`, `BinaryOp`); anything unrecognised
    /// appears as `Error` and the host falls back to the Rust parser
    /// for that part. Implies `--lex-with-dylan` (shared resolver).
    DumpDylanAst {
        /// Path to a `.dylan` source file.
        input: PathBuf,
    },
    /// Sprint 54b — run the Dylan-side sema recording walk IN-PROCESS (via the
    /// statically-linked `dylan-sema-emit` shim entry) and print its
    /// four-section model dump. Byte-identical to `dump-sema --parse-with-rust`
    /// (the Rust oracle) on the gated corpus — this is the in-process verify
    /// of the load-bearing Dylan sema path. Requires the shim to be linked.
    DumpDylanSema {
        /// Path to a `.dylan` source file.
        input: PathBuf,
    },
    /// Sprint 55 Phase 0 — run the Dylan-side AST→DFM lowering IN-PROCESS (via
    /// the statically-linked `dylan-lower-emit` shim entry) and print its
    /// `dump-dfm` text. Byte-identical to `dump-dfm` on the Phase-0 subset
    /// (straight-line functions); prints nothing for modules outside it. This
    /// is the byte-match gate for the lowering port. Requires the shim linked.
    DumpDylanDfm {
        /// Path to a `.dylan` source file.
        input: PathBuf,
    },
    /// Run the Dylan-in-Dylan parser over a source file and print the AST dump.
    ///
    /// Builds [dylan-lexer.dylan, dylan-parser.dylan] into a cached EXE,
    /// then spawns it with the input path as argv[1].
    ParseDylan {
        /// Path to a `.dylan` source file to parse.
        input: PathBuf,
        /// Sprint 49 — print wall-clock for the parser-EXE run to
        /// stderr after the parse finishes. Does NOT include the
        /// (cached) one-time build of `dylan-parser.exe`.
        #[arg(long = "time")]
        time: bool,
    },
    /// Symbolicate a crash dump's raw hex IPs against a linker `.map`.
    ///
    /// Reads stderr / saved-log text from stdin or `--in <file>`, finds
    /// `0x` 16-hex tokens, and replaces them with `name+0xNN (0xIP)`
    /// rewriting the file to stdout (or `--out`). Designed for the
    /// `[GAP-011] push caller backtrace` style output the runtime
    /// emits, but works on any backtrace shape — it just rewrites
    /// every 16-hex `0x...` token it sees.
    ///
    /// Default base: the EXE's preferred load address from the `.map`.
    /// Override with `--runtime-base <hex>` if the crash log captured
    /// a different ASLR slide (it usually didn't — Windows EXEs
    /// commonly map at the preferred base).
    Symbolicate {
        /// `.map` file emitted by `link.exe /MAP` next to the EXE.
        #[arg(long)]
        map: PathBuf,
        /// Input file (default: stdin).
        #[arg(long = "in")]
        input: Option<PathBuf>,
        /// Output file (default: stdout).
        #[arg(long = "out")]
        output: Option<PathBuf>,
        /// Runtime EXE base address in hex (override the .map's
        /// `Preferred load address`). Rarely needed.
        #[arg(long = "runtime-base")]
        runtime_base: Option<String>,
    },
}

// ─── Sprint 51e.5 — Dylan parser in the real pipeline ─────────────────────
//
// The Dylan-side parser is wired into the *real* compile/eval/build
// pipeline (not just the `dump-ast` diagnostic) via the parser override
// hook in `nod-reader`. `install_dylan_parse_override` installs
// `dylan_parse_module` as `nod_reader::parse_module_with_macros`'s
// override; every `parse_user_module` call inside this driver process
// (single-file `eval`/`dump-*`, and crucially the multi-file
// `compile_files_for_aot` AST-merge) then routes through the Dylan
// parser, with a whole-file fall-back to the canonical Rust parser for
// any file it can't translate.
//
// This mirrors `dylan_lex_jit`'s `set_lex_override` install. The Rust
// parser stays compiled in as the canonical fall-back AND the
// verify-mode oracle.

/// `nod_reader::ParseFn`-compatible Dylan parse entry point.
///
/// Ignores `tokens`, `preamble`, and `seed_macros` — the Dylan parser
/// re-lexes `src` inside the statically-linked shim and seeds its own
/// macro recognition. Runs `dylan_parse_wire::parse_to_tree(src)` then
/// `dylan_to_ast::to_ast_module(&tree, src)`.
///
/// * On `Ok(module)` — returns the Dylan-translated `ast::Module` (the
///   real pipeline lowers it directly), UNLESS `NOD_VERIFY_PARSE=1`, in
///   which case it also runs `parse_module_with_macros_rust`, compares
///   the two via `format_ast_module`, prints a loud divergence note if
///   they differ, and **proceeds with the Rust result** (the safety net
///   before defaulting — Sprint 51e.6).
/// * On `Unsupported` OR a wire error — falls back by calling
///   `nod_reader::parse_module_with_macros_rust` (the canonical, NOT the
///   dispatcher, so a fall-back can't recurse into this override) and
///   returns that.
///
/// The translated/fell-back note goes to stderr, reusing the wording
/// from `run_dump_ast` so the same harness reads it.
fn dylan_parse_module(
    src: &str,
    tokens: &[nod_reader::Token],
    preamble: Option<&nod_reader::Preamble>,
    seed_macros: &std::collections::HashSet<String>,
) -> Result<nod_reader::Module, Vec<nod_reader::Diagnostic>> {
    let verify = std::env::var("NOD_VERIFY_PARSE").map(|v| v == "1").unwrap_or(false);

    // Sprint 52.6 (locus B) — under `NOD_EXPAND_WITH_DYLAN`, expand the
    // source's macro calls Dylan-side BEFORE parsing, then run the normal
    // parse on the expanded (macro-free) source. The host stays oblivious;
    // nod-sema's own macro-expansion pass then no-ops on the already-
    // expanded AST. Falls back to the original source if expansion fails
    // (no shim, or a UTF-8/wire error).
    let want_expand = std::env::var("NOD_EXPAND_WITH_DYLAN")
        .map(|v| v == "1")
        .unwrap_or(false);
    let expanded_holder: Option<(String, Vec<nod_reader::Token>, Option<nod_reader::Preamble>)> =
        if want_expand {
            // The shim's resolver must have fired before `dylan-expand-source`
            // runs (it lexes, which sets the lexer's `*src*`/`*pos*` module
            // variables via AOT-resolved name literals). `init` is idempotent.
            match dylan_lex_jit::init() {
                Err(e) => {
                    eprintln!("expand-with-dylan: fell back (shim init: {e}) [pipeline]");
                    None
                }
                Ok(()) => match dylan_parse_wire::expand_source_via_shim(
                    src,
                    nod_sema::stdlib::stdlib_macro_source(),
                ) {
                    Ok(exp) => {
                        eprintln!("expand-with-dylan: expanded [pipeline]");
                        let mut sm = nod_reader::SourceMap::new();
                        let fid = sm
                            .add(std::path::PathBuf::from("<expanded>"), exp.clone())
                            .expect("source map add for expanded source");
                        let toks = nod_reader::lex_rust(&exp, fid);
                        let pre = nod_reader::scan_preamble(&exp);
                        Some((exp, toks, pre))
                    }
                    Err(e) => {
                        eprintln!("expand-with-dylan: fell back ({e}) [pipeline]");
                        None
                    }
                },
            }
        } else {
            None
        };
    // Re-bind the parse inputs to the expanded source when expansion ran.
    let (src, tokens, preamble): (&str, &[nod_reader::Token], Option<&nod_reader::Preamble>) =
        match &expanded_holder {
            Some((s, t, p)) => (s.as_str(), t.as_slice(), p.as_ref()),
            None => (src, tokens, preamble),
        };

    // The Dylan path needs the shim's resolver to have fired. `init` is
    // idempotent (a `OnceLock` guards the resolver), so calling it here
    // is cheap on warm processes. On a no-shim build it errors and we
    // fall straight back to the Rust parser.
    let dylan_module: Option<nod_reader::Module> = match dylan_lex_jit::init() {
        Err(e) => {
            eprintln!("parse-with-dylan: fell back (shim init: {e}) [pipeline]");
            None
        }
        Ok(()) => match dylan_parse_wire::parse_to_tree(src) {
            Ok(tree) => match dylan_to_ast::to_ast_module(&tree, src) {
                Ok(m) => {
                    eprintln!("parse-with-dylan: translated [pipeline]");
                    Some(m)
                }
                Err(dylan_to_ast::Unsupported(why)) => {
                    eprintln!("parse-with-dylan: fell back ({why}) [pipeline]");
                    None
                }
            },
            Err(e) => {
                eprintln!("parse-with-dylan: fell back (wire: {e}) [pipeline]");
                None
            }
        },
    };

    // Verify-mode: parse with the Rust oracle too, compare the dumps,
    // and proceed with the Rust result regardless. This is the safety
    // net that runs both parsers on the *pipeline* parse (not just
    // dump-ast) and surfaces any divergence loudly. Only meaningful when
    // the Dylan path actually produced a module to compare against.
    if verify {
        let rust = nod_reader::parse_module_with_macros_rust(src, tokens, preamble, seed_macros);
        if let Some(dyl) = &dylan_module
            && let Ok(rust_m) = &rust
        {
            let dyl_dump = nod_reader::format_ast_module(dyl);
            let rust_dump = nod_reader::format_ast_module(rust_m);
            if dyl_dump != rust_dump {
                eprintln!(
                    "parse-verify: DIVERGENCE [pipeline] — the Dylan parser's AST \
                     differs from the Rust parser's:\n\
                     --- rust parse_module_with_macros ---\n{rust_dump}\n\
                     --- dylan parse ---\n{dyl_dump}"
                );
            } else {
                eprintln!("parse-verify: ok [pipeline] (rust+dylan AST byte-identical)");
            }
        }
        return rust;
    }

    // Authoritative mode: use the Dylan result when it translated, else
    // fall back to the canonical Rust parser for the whole file.
    match dylan_module {
        Some(m) => Ok(m),
        None => nod_reader::parse_module_with_macros_rust(src, tokens, preamble, seed_macros),
    }
}

/// Install [`dylan_parse_module`] as the process-wide
/// `nod_reader::parse_module_with_macros` override. Idempotent and
/// install-once (the `OnceLock` in `nod-reader` keeps the first
/// installation). Called from `main` under the same
/// `--parse-with-dylan` / `NOD_PARSE_WITH_DYLAN` condition as the lexer
/// override.
fn install_dylan_parse_override() {
    let _ = nod_reader::set_parse_override(dylan_parse_module);
    // NOTE: the status wording deliberately avoids the substring
    // `parse-with-dylan:` — the `dylan_parse_translate` gate scans
    // stderr for the FIRST line containing that substring to tally
    // translated-vs-fell-back, and a startup status line carrying it
    // would shadow the per-parse note. The per-parse notes (emitted by
    // `dylan_parse_module`) own that substring.
    if nod_reader::has_parse_override() {
        eprintln!("nod-driver: Dylan parser override installed (real pipeline active)");
    } else {
        eprintln!(
            "nod-driver: WARNING — Dylan parse-override slot already occupied; \
             nod_reader::parse_module_with_macros will use whatever was installed first"
        );
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    // Sprint 51b — JIT-strap the Dylan-side lexer if the flag or the
    // `NOD_LEX_WITH_DYLAN=1` env var is set. Wire it through BEFORE
    // dispatching to the subcommand so the first lex call in
    // `compile_file_for_aot` / `dump_tokens` / `eval` sees the
    // override.
    // Sprint 51c — `--verify-parse` shares the lexer shim's resolver,
    // so triggering parse-verify implies running the lex-init path
    // too. Same env-var fallback as the lexer flag.
    let want_verify_parse = cli.verify_parse
        || std::env::var("NOD_VERIFY_PARSE").map(|v| v == "1").unwrap_or(false);
    // Persist into the env so `run_dump_ast` (and any future parse
    // call site) picks the flag up uniformly.
    if want_verify_parse {
        // SAFETY: single-threaded process startup; no other thread is
        // reading env yet.
        unsafe { std::env::set_var("NOD_VERIFY_PARSE", "1"); }
    }

    // Sprint 51e — `--parse-with-dylan` authoritative mode. Persist to
    // the env so `run_dump_ast` picks it up. Deliberately does NOT imply
    // `--lex-with-dylan`: the Dylan path uses the statically-linked shim
    // (its own resolver, fired lazily in `run_dump_ast`), while the Rust
    // FALLBACK path must keep using the Rust lexer so a fallback's AST is
    // identical to plain `dump-ast`. That keeps the byte-identical gate
    // measuring the translator, not the lexer.
    let want_parse_with_dylan = cli.parse_with_dylan
        || std::env::var("NOD_PARSE_WITH_DYLAN").map(|v| v == "1").unwrap_or(false);
    if want_parse_with_dylan {
        // SAFETY: single-threaded process startup.
        unsafe { std::env::set_var("NOD_PARSE_WITH_DYLAN", "1"); }
    }

    // Sprint 54c — `--sema-with-dylan` makes the Dylan sema recording
    // load-bearing for the back-end (currently `dump-dfm`). Install a provider
    // that produces the model dump in-process via the `dylan-sema-emit` shim;
    // `nod_sema`'s `lower_with_sema_choice` reconstructs a `SemaModel` from it
    // and feeds the DFM construction. Requires the shim to be linked.
    // Sprint 56 — the combined `--frontend-with-dylan` flag implies all three
    // front-end opt-ins (sema + lower + Dylan expansion), so the back-end takes
    // a single all-Dylan front-end handoff. Setting NOD_EXPAND_WITH_DYLAN here
    // (before the parse path reads it) routes the parse-path expander to Dylan
    // too; the lowering path already expands Dylan-side under --lower-with-dylan.
    let want_frontend_with_dylan = cli.frontend_with_dylan
        || std::env::var("NOD_FRONTEND_WITH_DYLAN").map(|v| v == "1").unwrap_or(false);
    if want_frontend_with_dylan {
        // SAFETY: single-threaded process startup.
        unsafe { std::env::set_var("NOD_EXPAND_WITH_DYLAN", "1"); }
        // Sprint 56c (CONSUME) — propagate the combined flag into the env so
        // the lowering (`nod_sema::lower`) builds its `methods` table FROM the
        // Dylan `=== methods ===` section instead of the Rust AST walk. The
        // CLI flag alone wouldn't reach `lower.rs`, which reads this var.
        unsafe { std::env::set_var("NOD_FRONTEND_WITH_DYLAN", "1"); }
    }

    let want_sema_with_dylan = cli.sema_with_dylan
        || want_frontend_with_dylan
        || std::env::var("NOD_SEMA_WITH_DYLAN").map(|v| v == "1").unwrap_or(false);
    if want_sema_with_dylan {
        if cfg!(dylan_lex_shim_linked) {
            // SAFETY: single-threaded process startup.
            unsafe { std::env::set_var("NOD_SEMA_WITH_DYLAN", "1"); }
            let _ = nod_sema::set_sema_dump_provider(dylan_sema_dump_provider);
        } else {
            eprintln!(
                "nod-driver: --sema-with-dylan requested but the dylan-lex-shim is \
                 not statically linked; using the Rust sema. Build the shim to activate \
                 the Dylan sema."
            );
        }
    }

    // Sprint 55 — `--lower-with-dylan` makes the Dylan AST→DFM lowering
    // load-bearing for the back-end (currently `dump-dfm`). Install a provider
    // that produces the DFM dump in-process via the `dylan-lower-emit` shim;
    // `nod_sema`'s `lower_with_sema_choice` reconstructs `Vec<Function>` from it
    // (when non-empty) and runs the back-end passes on it. Requires the shim.
    let want_lower_with_dylan = cli.lower_with_dylan
        || want_frontend_with_dylan
        || std::env::var("NOD_LOWER_WITH_DYLAN").map(|v| v == "1").unwrap_or(false);
    if want_lower_with_dylan {
        if cfg!(dylan_lex_shim_linked) {
            // SAFETY: single-threaded process startup.
            unsafe { std::env::set_var("NOD_LOWER_WITH_DYLAN", "1"); }
            let _ = nod_sema::set_dfm_dump_provider(dylan_dfm_dump_provider);
        } else {
            eprintln!(
                "nod-driver: --lower-with-dylan requested but the dylan-lex-shim is \
                 not statically linked; using the Rust lowering. Build the shim to \
                 activate the Dylan lowering."
            );
        }
    }

    // Sprint 51e.6 — the Dylan parser is now the DEFAULT real-pipeline
    // parser; `--parse-with-rust` (or NOD_PARSE_WITH_RUST=1) opts back out.
    let want_parse_with_rust = cli.parse_with_rust
        || std::env::var("NOD_PARSE_WITH_RUST").map(|v| v == "1").unwrap_or(false)
        // `eval` is a debug/REPL command: always route it through the reference
        // Rust parser. The Dylan-parser shim mis-translates eval's synthetic
        // `define function <eval-entry> () … end` wrapper (the angle-bracket
        // name), dropping the entry so eval fails with "<eval-entry> missing
        // after lowering". The Rust parser handles it, and eval never needs the
        // self-hosting default path. (Fixes a regression from a shim rebuild.)
        || matches!(cli.command, Some(Command::Eval { .. }));

    // Sprint 51e.5 — `--parse-with-dylan` deliberately does NOT install
    // the Rust-side lex OVERRIDE. The Dylan parse path lexes internally
    // inside the shim (`dylan-parse-emit`), firing the resolver via its
    // own `dylan_lex_jit::init()` call; it never needs `nod_reader::lex`
    // to be redirected. Critically, the whole-file Rust FALL-BACK must
    // keep using the Rust lexer so a fallback's AST stays byte-identical
    // to plain `dump-ast` — the Dylan and Rust lexers differ on a few
    // surfaces (e.g. `-1` as a call arg: Rust lexes a signed `Integer`,
    // the Dylan lexer a `Minus` + `Integer` that the Rust parser would
    // then fold into a `UnOp`). Installing the lex override would route
    // the fall-back through the Dylan lexer and silently diverge the
    // dump-ast gate. `--verify-parse` still implies lex-with-dylan (it
    // shares the lexer shim's resolver for the collect path).
    let want_dylan_lex = cli.lex_with_dylan
        || want_verify_parse
        || std::env::var("NOD_LEX_WITH_DYLAN").map(|v| v == "1").unwrap_or(false);
    if want_dylan_lex {
        eprintln!("nod-driver: --lex-with-dylan: JIT-strapping the Dylan-side lexer …");
        match dylan_lex_jit::init() {
            Ok(()) => {
                // Install the override. Result is ignored — a second
                // `set_lex_override` (e.g. on a retry) returns Err
                // with the already-installed fn, which is fine.
                let _ = nod_reader::set_lex_override(dylan_lex_jit::lex);
                if nod_reader::has_lex_override() {
                    eprintln!("nod-driver: --lex-with-dylan: Dylan lex active");
                } else {
                    eprintln!(
                        "nod-driver: --lex-with-dylan: WARNING — override slot already \
                         occupied; nod_reader::lex will use whatever was installed first"
                    );
                }
            }
            Err(e) => {
                eprintln!(
                    "nod-driver: --lex-with-dylan: init failed: {e}\n\
                     falling back to the Rust lexer"
                );
            }
        }
    }

    // Sprint 51e.5 — install the Dylan parser into the REAL pipeline.
    // Done here, alongside the lexer override. Every
    // `parse_module_with_macros` call in this process (single-file
    // `eval`/`dump-*`, and the multi-file `compile_files_for_aot`
    // AST-merge that `build` drives) then routes through the Dylan
    // parser, with a whole-file fall-back to the Rust parser.
    // `run_dump_ast` keeps its own inline Dylan path and is unaffected
    // (it calls `parse_module_with_macros_rust` directly on fall-back).
    //
    // Installed under `--parse-with-dylan` (authoritative mode: use the
    // Dylan result) OR `--verify-parse` (verify mode: `dylan_parse_module`
    // runs BOTH parsers, compares their `format_ast_module` dumps on the
    // *pipeline* parse, logs any divergence, and proceeds with the Rust
    // result — the safety net the spec mandates before defaulting).
    // Sprint 51e.6 — default-on. Install the Dylan parser override unless
    // the user opted out (--parse-with-rust). Gate on shim availability via
    // the build.rs-set cfg: a fresh checkout without the statically-linked
    // shim cleanly keeps the Rust parser (no install, no per-file fall-back
    // noise), exactly as before. The override fn JIT-straps the shim
    // lazily on first parse, so non-parsing commands pay nothing.
    // (`--parse-with-dylan` is now the default; `--verify-parse` still
    // additionally runs both parsers and compares — see `dylan_parse_module`.)
    if !want_parse_with_rust && cfg!(dylan_lex_shim_linked) {
        install_dylan_parse_override();
    } else if !want_parse_with_rust && (want_parse_with_dylan || want_verify_parse) {
        eprintln!(
            "nod-driver: Dylan parser requested but the dylan-lex-shim is not \
             statically linked; using the Rust parser. Build the shim to activate \
             the Dylan front-end."
        );
    }

    match cli.command {
        None => {
            println!(
                "nod-driver {} (LLVM {LLVM_VERSION})",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        Some(Command::Compile { input }) => {
            let target = input
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<no input>".into());
            eprintln!("nod-driver compile: not yet implemented (input: {target})");
            ExitCode::from(2)
        }
        Some(Command::Build { inputs, output, project, time, verbose, library }) => {
            // Sprint 49 — accept inputs from either positional args
            // (Sprint 44 multi-file shape) OR a `.prj` project file.
            // `clap`'s `conflicts_with` on `project` rules out the
            // both-set case at parse time; `required_unless_present`
            // rules out both-empty. So at most one of the two is
            // populated here.
            let (resolved_inputs, default_out, project_tag, entry_function) =
                if let Some(prj_path) = project {
                    match project::ResolvedProject::load(&prj_path) {
                        Ok(p) => {
                            if verbose {
                                eprintln!(
                                    "nod build: project={} ({}), {} source file{}, entry=`{}`",
                                    p.name,
                                    p.project_path.display(),
                                    p.sources.len(),
                                    if p.sources.len() == 1 { "" } else { "s" },
                                    p.start_function,
                                );
                            }
                            let tag = format!("project `{}`", p.name);
                            (p.sources.clone(), Some(p.output), Some(tag), p.start_function)
                        }
                        Err(e) => {
                            eprintln!("nod build: {e}");
                            return ExitCode::from(1);
                        }
                    }
                } else {
                    (inputs.clone(), None, None, "main".to_string())
                };
            let out = output
                .or(default_out)
                .unwrap_or_else(|| default_exe_path(&resolved_inputs[0]));
            let stopwatch = if time { Some(std::time::Instant::now()) } else { None };
            let code = run_build_full(&resolved_inputs, &out, verbose, &entry_function, library);
            if let Some(start) = stopwatch {
                let dt = start.elapsed();
                let what = project_tag
                    .unwrap_or_else(|| format!("{} input file{}",
                        resolved_inputs.len(),
                        if resolved_inputs.len() == 1 { "" } else { "s" }));
                eprintln!("nod build: total wall-clock {:.3}s ({what})", dt.as_secs_f64());
            }
            code
        }
        Some(Command::Repl) => {
            eprintln!("nod-driver repl: not yet implemented (see Sprint 08).");
            ExitCode::from(2)
        }
        Some(Command::DumpTokens { input }) => run_dump_tokens(&input),
        Some(Command::DumpAst { input }) => run_dump_ast(&input),
        Some(Command::DumpGraph { input }) => run_dump_graph(&input),
        Some(Command::DumpDfm { input }) => run_dump_dfm(&input),
        Some(Command::DumpSema { input }) => run_dump_sema(&input),
        Some(Command::DumpLlvm { input }) => run_dump_llvm(&input),
        Some(Command::Eval { expr }) => run_eval(&expr),
        Some(Command::DumpDylanTokens { input, gc_stats }) => run_dump_dylan_tokens(&input, gc_stats),
        Some(Command::DumpDylanAst { input }) => run_dump_dylan_ast(&input),
        Some(Command::DumpDylanSema { input }) => run_dump_dylan_sema(&input),
        Some(Command::DumpDylanDfm { input }) => run_dump_dylan_dfm(&input),
        Some(Command::ParseDylan { input, time }) => {
            let stopwatch = if time { Some(std::time::Instant::now()) } else { None };
            let code = run_parse_dylan(&input);
            if let Some(start) = stopwatch {
                let dt = start.elapsed();
                eprintln!(
                    "nod parse-dylan: total wall-clock {:.3}s",
                    dt.as_secs_f64()
                );
            }
            code
        }
        Some(Command::Symbolicate { map, input, output, runtime_base }) => {
            run_symbolicate(&map, input.as_deref(), output.as_deref(), runtime_base.as_deref())
        }
    }
}

// ─── Sprint 39a `build` subcommand ────────────────────────────────────────
//
// End-to-end: source.dylan → .obj → link.exe → exe. See PLAN.md /
// SPRINTS.md for the full Sprint 39 scope. The pipeline below stays
// minimal: no -O dial, no cross-compile, no incremental builds. A
// future sprint can layer those on without disturbing the shape here.

/// Sprint 39b — walk the manifest's `RelocKind::StubEntry` rows and
/// return the unique DLL names referenced (case-insensitive dedup).
/// The driver then asks `nod_winapi::import_lib_for_dll` for each one
/// and appends the resulting `.lib` to the `link.exe` arg list.
///
/// Returns DLLs in the lowercased form `nod-winapi` expects. Ordering
/// is deterministic (lowercase-sorted) so verbose link.exe args /
/// debug output are stable across runs.
fn collect_user_dlls(manifest: &nod_llvm::ModuleManifest) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut set: BTreeSet<String> = BTreeSet::new();
    for entry in &manifest.entries {
        if let nod_llvm::RelocKind::StubEntry { dll, .. } = &entry.kind {
            set.insert(dll.to_ascii_lowercase());
        }
    }
    set.into_iter().collect()
}

/// Default `<input stem>.exe` next to the input file. Mirrors `rustc`'s
/// behaviour when `-o` is omitted.
fn default_exe_path(input: &std::path::Path) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("a");
    let mut p = input.to_path_buf();
    p.set_file_name(format!("{stem}.exe"));
    p
}

/// Locate the `nod_runtime.lib` staticlib that Sprint 39a Phase A's
/// `[lib] crate-type = ["rlib", "staticlib"]` setting produces. We
/// look in the workspace's `target/<profile>/` directory.
///
/// **Profile selection**: prefer `target/debug/nod_runtime.lib` for
/// fastest iteration; a future sprint can add `--release`. The build
/// caller is responsible for ensuring nod-runtime has been compiled
/// (the easiest way: `cargo build -p nod-runtime` before invoking
/// `nod build`).
///
/// Returns `Err` if the staticlib isn't where we expect — a clearer
/// error than `link.exe` blowing up with "library not found".
fn locate_runtime_staticlib() -> Result<PathBuf, String> {
    // Allow override via env var so CI / tests can pin a specific
    // build directory.
    if let Ok(p) = std::env::var("NOD_RUNTIME_LIB") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
        return Err(format!(
            "NOD_RUNTIME_LIB={} but file does not exist",
            p.display()
        ));
    }
    // The driver runs from anywhere; walk up from `current_exe` to the
    // workspace root. Cargo lays out test/CI/run binaries in the same
    // `target/<profile>/` directory, so `current_exe().parent()` is
    // where we expect to find `nod_runtime.lib` alongside the driver
    // itself.
    let exe = std::env::current_exe()
        .map_err(|e| format!("current_exe(): {e}"))?;
    let cargo_target = exe
        .parent()
        .ok_or_else(|| "current_exe has no parent".to_string())?;
    let direct = cargo_target.join("nod_runtime.lib");
    if direct.is_file() {
        return Ok(direct);
    }
    // Fall back to walking up: a manually-run `cargo run --bin nod` puts
    // the binary in `target/debug/`, the runtime artifact is right there;
    // `cargo test` puts the test binary in `target/debug/deps/` and the
    // runtime is one level up.
    let mut cursor = Some(cargo_target);
    while let Some(dir) = cursor {
        let candidate = dir.join("nod_runtime.lib");
        if candidate.is_file() {
            return Ok(candidate);
        }
        cursor = dir.parent();
    }
    Err(format!(
        "could not locate nod_runtime.lib (searched from {}). \
         Build it with: `cargo build -p nod-runtime` \
         (or set NOD_RUNTIME_LIB=/path/to/nod_runtime.lib).",
        cargo_target.display()
    ))
}

fn run_build_full(
    inputs: &[PathBuf],
    output: &std::path::Path,
    verbose: bool,
    entry_function: &str,
    library: bool,
) -> ExitCode {
    use nod_llvm::LlvmContext as Context;
    use nod_llvm::OptimizationLevel;
    let shape = if library {
        nod_llvm::AotShape::StaticLibrary
    } else {
        nod_llvm::AotShape::Executable
    };

    // Sprint 44 — multi-file front-end. For a single input the
    // pipeline is identical to the Sprint 39 single-file path (the
    // merge loop in `compile_files_for_aot` is a no-op for N=1);
    // for N>1 the function checks that every file declares the same
    // `Module:` header, lowers each in order, detects cross-file
    // duplicate definitions, then merges everything into one
    // `LoweredModule` before the stdlib is layered on.
    let path_refs: Vec<&std::path::Path> = inputs.iter().map(|p| p.as_path()).collect();
    // Sprint 51e — a `--library` build is a front-end shim: its own
    // `define class`es must be minted from the shim id band so they
    // don't shift `FIRST_USER..` ids in any host that statically links
    // the resulting `.obj`. `compile_files_for_aot_with_shape` flips the
    // band around the shim source's lowering. A normal EXE build passes
    // `library = false` and is unaffected.
    let lm = match nod_sema::compile_files_for_aot_with_shape(&path_refs, library) {
        Ok(lm) => lm,
        Err(e) => {
            eprintln!("nod build: {e}");
            return ExitCode::from(1);
        }
    };
    // Sprint 39b — `define c-function` (and bare-name Win32 calls
    // materialized via Sprint 31's hook) are supported. Each unique
    // `(dll, symbol)` reference becomes a manifest `StubEntry` row;
    // we collect the DLLs from the manifest after codegen and pass
    // the matching import libraries to `link.exe`.

    // Sprint 39a / 50d: the user's entry function (default `main`,
    // overridable via the project file's `start_function`) must be
    // present for `nod-llvm::aot::emit_aot_entry_stubs_full` to find
    // it. Surface a clear error before we kick off codegen if it's
    // missing.
    if !lm.functions.iter().any(|f| f.name == entry_function) {
        eprintln!(
            "nod build: input file does not define `{entry_function}` — Sprint 39a EXEs need \
             `define function {entry_function} () => () ... end` as the entry point."
        );
        return ExitCode::from(1);
    }

    // Step 2 — codegen. The LLVM module name is taken from the FIRST
    // input file's stem (matches the default output-EXE naming). For
    // a multi-file build this is purely a debug label — codegen
    // emits one merged LLVM module containing every function across
    // every input file.
    let ctx = Context::create();
    let module_name = inputs[0]
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("dylan-module");
    let out = match nod_llvm::codegen_module_for_surface(
        &ctx,
        &lm.functions,
        module_name,
        nod_llvm::CodeInstallSurface::Image,
    ) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("nod build: codegen: {e}");
            return ExitCode::from(1);
        }
    };
    let module = out.module;
    let manifest = out.manifest;
    let safepoint_installs = out.safepoint_installs;

    // Step 3 — AOT entry-stub injection + object-file emission.
    // We co-locate the `.obj` next to the output EXE so the file system
    // shows the compile pipeline's intermediate artifact for debugging.
    // (A future sprint can route this through a temp directory if the
    // intermediate becomes noise.)
    let obj_path = {
        let mut p = output.to_path_buf();
        p.set_extension("obj");
        p
    };

    // Sprint 39b — collect the unique DLLs referenced by `RelocKind::StubEntry`
    // entries BEFORE handing the manifest to `emit_aot_object`. The
    // returned set drives the extra `kernel32.lib` / `user32.lib` / etc.
    // import-library args we pass to `link.exe`. Manifest entries are
    // immutable across emission, so reading them here is order-independent.
    let user_dlls = collect_user_dlls(&manifest);

    // Sprint 39c — build the registration payload from the merged
    // (user + stdlib) lowered module. The AOT entry-stub injection
    // pass embeds one `nod_aot_register_method` / `nod_aot_register_block`
    // / `nod_aot_register_jit_function` call per entry inside the
    // codegen-emitted `nod_aot_resolve_relocs` function, which the
    // EXE's `main` calls before invoking the user's Dylan code. This
    // is what makes `size(<range>)` (and every other stdlib-defined
    // generic method) resolve at AOT runtime.
    let registrations = nod_sema::build_aot_registrations(&lm);

    // Sprint 51b — library mode picks `AotShape::StaticLibrary`, which
    // keeps every source-language symbol name intact and skips the
    // synthetic `i32 @main()` emission.
    if let Err(e) = nod_llvm::aot::emit_aot_object_full_with_mode(
        &module,
        &manifest,
        &registrations,
        &safepoint_installs,
        &obj_path,
        OptimizationLevel::Default,
        entry_function,
        shape,
    ) {
        eprintln!("nod build: {e}");
        return ExitCode::from(1);
    }

    if library {
        // Library mode — no linking. The `.obj` already sits at
        // `obj_path` from `emit_aot_object_full_with_mode`. If the
        // user asked for an output path that differs from
        // `default_exe_path`'s `.exe`-derived `.obj`, copy it across.
        if obj_path != output {
            if let Err(e) = std::fs::copy(&obj_path, output) {
                eprintln!(
                    "nod build: copy {} -> {}: {e}",
                    obj_path.display(),
                    output.display()
                );
                return ExitCode::from(1);
            }
        }
        println!("compiled (library): {}", output.display());
        return ExitCode::SUCCESS;
    }

    // Step 4 — locate the staticlib and `link.exe`.
    let runtime_lib = match locate_runtime_staticlib() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("nod build: {e}");
            return ExitCode::from(1);
        }
    };

    let mut link_cmd = match cc::windows_registry::find("x86_64-pc-windows-msvc", "link.exe") {
        Some(c) => c,
        None => {
            eprintln!(
                "nod build: could not locate MSVC link.exe. \
                 Run from a Developer Command Prompt or install VS Build Tools."
            );
            return ExitCode::from(1);
        }
    };

    if verbose {
        eprintln!(
            "nod build: triple    = {}",
            nod_llvm::aot::default_triple_string()
        );
        for (i, p) in inputs.iter().enumerate() {
            eprintln!("nod build: input[{i}] = {}", p.display());
        }
        eprintln!("nod build: object   = {}", obj_path.display());
        eprintln!("nod build: runtime  = {}", runtime_lib.display());
        eprintln!("nod build: output   = {}", output.display());
    }

    // Step 5 — invoke link.exe.
    //
    // Standard MSVC EXE link line:
    //   - User .obj (defines `nod_user_main` + `main`)
    //   - nod_runtime.lib (defines `nod_aot_main_wrapper` + the
    //     full Dylan runtime; transitively pulls in the user's
    //     `nod_user_main` reference from `aot.obj`)
    //   - CRT + system libs needed by Rust std I/O
    //   - /SUBSYSTEM:CONSOLE so format-out → stdout is visible
    //   - /ENTRY:mainCRTStartup the standard CRT entry; calls the
    //     `main()` stub we emitted in `emit_aot_entry_stubs`.
    //   - /NXCOMPAT /DYNAMICBASE /HIGHENTROPYVA — modern Windows
    //     security defaults; `link.exe` warns without these.
    link_cmd.arg(&obj_path);
    link_cmd.arg(&runtime_lib);
    link_cmd.arg(format!("/OUT:{}", output.display()));
    link_cmd.arg("/SUBSYSTEM:CONSOLE");
    link_cmd.arg("/ENTRY:mainCRTStartup");
    link_cmd.arg("/MACHINE:X64");
    link_cmd.arg("/NXCOMPAT");
    link_cmd.arg("/DYNAMICBASE");
    link_cmd.arg("/HIGHENTROPYVA");
    // NOTE (tasks #7/#8): we deliberately do NOT pass `/FORCE:MULTIPLE`.
    // `nod_runtime.lib` ships a default `nod_user_main` stub
    // (`aot_user_main_stub.rs`); the real AOT EXE supplies its own strong
    // `nod_user_main` (the renamed Dylan entry in `obj_path`, linked
    // FIRST), and MSVC's on-demand archive extraction drops the stub —
    // which works cleanly in DEBUG. It is FRAGILE in RELEASE, where
    // Cargo's CGU partitioner can colocate the stub with a hot std
    // monomorphization, forcing extraction → `LNK2005 nod_user_main`
    // (a pre-existing, *documented release-only* issue —
    // docs/manual/compiler/jit-and-aot.md:309). `/FORCE:MULTIPLE` would
    // mask that by silencing ALL duplicate-symbol errors, and still yields
    // a non-functional release EXE, so it is the wrong instrument. The
    // correct fix is the `codegen-units = 1` pin the manual already
    // promises but that is currently absent from every Cargo.toml
    // (task #8). Debug AOT links cleanly without any of this.
    // GAP-011 diagnostic: emit a linker map file alongside the EXE so a
    // crash-backtrace IP can be resolved back to the AOT Dylan function it
    // belongs to. Costs a few seconds of link time + a text file; no effect
    // on the EXE.
    link_cmd.arg(format!("/MAP:{}.map", output.display()));
    // Sprint 39b — pass an import lib for every DLL the user's program
    // references via `define c-function` / bare-name Win32 calls. The
    // Windows loader resolves these symbols from the named DLLs at EXE
    // load, populating the IAT before any user code runs. Duplicates
    // against the hard-coded list below are harmless — `link.exe`
    // dedupes by file name.
    for dll in &user_dlls {
        let Some(lib) = nod_winapi::import_lib_for_dll(dll) else {
            eprintln!(
                "nod build: WARN: cannot derive import lib for DLL `{dll}` \
                 (manifest entry skipped). The linker will likely surface \
                 an unresolved external for this DLL's exports."
            );
            continue;
        };
        link_cmd.arg(&lib);
    }
    // The libs Rust's MSVC std + windows-sys need at link time. cc-rs's
    // discovered link.exe Command already has %LIB% set so these
    // resolve from the SDK's lib directory.
    for lib in [
        "kernel32.lib",
        "advapi32.lib",
        "userenv.lib",
        "ws2_32.lib",
        "ntdll.lib",
        "msvcrt.lib",
        "ucrt.lib",
        "vcruntime.lib",
        "legacy_stdio_definitions.lib",
        // Sprint 35 / 36's COM types pull in these even when the user
        // program doesn't touch them — the unused-symbol DCE doesn't
        // strip them because the `windows` crate uses `#[link]` attrs
        // that the staticlib's metadata propagates. Cheap to include
        // unconditionally.
        "ole32.lib",
        "oleaut32.lib",
        "uuid.lib",
        "user32.lib",
        "gdi32.lib",
        "dxgi.lib",
        "d3d11.lib",
        "d2d1.lib",
        "dwrite.lib",
        "bcrypt.lib",
        "synchronization.lib",
        // Sprint 39a — the `windows` crate's PROPVARIANT/VARIANT
        // helpers pulled in via Sprint 35's COM types reference
        // `PropVariantTo*` / `VariantTo*` which live in propsys.lib.
        // Adding here unconditionally because we have no way to know
        // which symbols the staticlib's transitively-included COM
        // types will reference; the linker DCE drops unused entries.
        "propsys.lib",
        // Sprint 41e — `GetOpenFileNameW` (called from the IDE's
        // File → Open shim in nod-runtime's com_shim.rs) lives in
        // comdlg32.dll. Without this import lib, link.exe surfaces an
        // unresolved external when the staticlib pulls in the shim.
        "comdlg32.lib",
    ] {
        link_cmd.arg(lib);
    }
    if verbose {
        eprintln!("nod build: link.exe args: {:?}", link_cmd.get_args().collect::<Vec<_>>());
    }

    match link_cmd.output() {
        Ok(o) if o.status.success() => {
            println!("compiled: {}", output.display());
            ExitCode::SUCCESS
        }
        Ok(o) => {
            eprintln!("nod build: link.exe failed with status {}", o.status);
            if !o.stdout.is_empty() {
                eprintln!("link.exe stdout:");
                std::io::Write::write_all(&mut std::io::stderr(), &o.stdout).ok();
            }
            if !o.stderr.is_empty() {
                eprintln!("link.exe stderr:");
                std::io::Write::write_all(&mut std::io::stderr(), &o.stderr).ok();
            }
            ExitCode::from(1)
        }
        Err(e) => {
            eprintln!("nod build: failed to invoke link.exe: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_dump_dfm(inputs: &[PathBuf]) -> ExitCode {
    // N==1 routes through the original single-file path to keep its output
    // byte-identical (the in-tree dump-dfm gate compares it). N>1 merges the
    // files into one library unit so cross-file references resolve.
    let result = if inputs.len() == 1 {
        nod_sema::dump_dfm_for_file(&inputs[0])
    } else {
        let refs: Vec<&std::path::Path> = inputs.iter().map(|p| p.as_path()).collect();
        nod_sema::dump_dfm_for_files(&refs)
    };
    match result {
        Ok(dump) => {
            print!("{dump}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("nod-driver dump-dfm: {e}");
            ExitCode::from(2)
        }
    }
}

fn run_dump_sema(input: &std::path::Path) -> ExitCode {
    match nod_sema::dump_sema_for_file(input) {
        Ok(dump) => {
            print!("{dump}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("nod-driver dump-sema: {e}");
            ExitCode::from(2)
        }
    }
}

fn run_dump_llvm(input: &std::path::Path) -> ExitCode {
    match nod_sema::dump_llvm_for_file(input) {
        Ok(ir) => {
            print!("{ir}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("nod-driver dump-llvm: {e}");
            ExitCode::from(2)
        }
    }
}

fn run_eval(expr: &str) -> ExitCode {
    match nod_sema::eval_expr_to_string(expr) {
        Ok(s) => {
            println!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("nod-driver eval: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_dump_tokens(input: &std::path::Path) -> ExitCode {
    use nod_reader::{SourceMap, format_tokens, lex};
    let src = match std::fs::read_to_string(input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "nod-driver dump-tokens: failed to read {}: {e}",
                input.display()
            );
            return ExitCode::from(2);
        }
    };
    let mut sm = SourceMap::new();
    let id = match sm.add(input.to_path_buf(), src.clone()) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("nod-driver dump-tokens: {e}");
            return ExitCode::from(2);
        }
    };
    let tokens = lex(&src, id);
    let dump = format_tokens(&tokens, id, &sm);
    print!("{dump}");
    ExitCode::SUCCESS
}

fn run_dump_ast(input: &std::path::Path) -> ExitCode {
    use nod_reader::{
        SourceMap, format_ast_module, lex, parse_module_with_macros_rust, scan_preamble,
    };
    let src = match std::fs::read_to_string(input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("nod-driver dump-ast: failed to read {}: {e}", input.display());
            return ExitCode::from(2);
        }
    };
    // Sprint 51e — authoritative mode. Try the Dylan parser + the
    // `dylan_to_ast` translator first; print its `format_ast_module` and
    // return on success. On ANY Unsupported/Error, fall through to the
    // Rust parser below (the output is then identical to plain
    // `dump-ast`). The stderr note records which path each file took so
    // the translation-coverage harness can tally it.
    if std::env::var("NOD_PARSE_WITH_DYLAN").map(|v| v == "1").unwrap_or(false) {
        match dylan_lex_jit::init() {
            Err(e) => {
                eprintln!(
                    "parse-with-dylan: fell back (shim init: {e}) on {}",
                    input.display()
                );
            }
            Ok(()) => match dylan_parse_wire::parse_to_tree(&src) {
                Ok(tree) => match dylan_to_ast::to_ast_module(&tree, &src) {
                    Ok(m) => {
                        eprintln!("parse-with-dylan: translated {}", input.display());
                        print!("{}", format_ast_module(&m));
                        return ExitCode::SUCCESS;
                    }
                    Err(dylan_to_ast::Unsupported(why)) => {
                        eprintln!("parse-with-dylan: fell back ({why}) on {}", input.display());
                    }
                },
                Err(e) => {
                    eprintln!("parse-with-dylan: fell back (wire: {e}) on {}", input.display());
                }
            },
        }
    }

    let mut sm = SourceMap::new();
    let id = match sm.add(input.to_path_buf(), src.clone()) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("nod-driver dump-ast: {e}");
            return ExitCode::from(2);
        }
    };
    let tokens = lex(&src, id);
    let pre = scan_preamble(&src);
    // Seed the parser with the body-shaped macro names so the standalone
    // dump-ast path recognises stdlib macro calls (`when`, `with-cleanup`,
    // `repeat`, …) the same way the real sema pipeline does. The base set
    // covers the builtin statement forms; we then augment it with the
    // stdlib's ACTUAL macro names, derived from the macro source via
    // `stdlib_macro_names()` (a light parse+collect — no JIT, no class
    // registration, so it's safe even when the shim's AOT resolver has
    // claimed the class registry). New stdlib macros are picked up
    // automatically — no manual list to maintain. The static base remains a
    // fallback if the derivation returns nothing.
    let mut macros: std::collections::HashSet<String> = [
        "case", "cond", "for-each", "iterate", "select", "unless", "when", "while",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    macros.extend(nod_sema::stdlib::stdlib_macro_names());
    // Sprint 51e.5 — call the canonical Rust parser DIRECTLY here, not
    // the dispatcher. `run_dump_ast` already ran its own inline Dylan
    // path above (and `return`ed on success); reaching here means it
    // fell back, so this is the Rust fall-back. Using `_rust` keeps the
    // installed pipeline parse-override (set when `--parse-with-dylan`)
    // from double-firing the Dylan path here — the byte-identical
    // dump-ast gate stays measuring the translator, not a re-dispatch.
    let result = parse_module_with_macros_rust(&src, &tokens, pre.as_ref(), &macros);
    let rust_accepted = result.is_ok();
    // Sprint 51c — verify-parse check, when enabled. Runs the
    // Dylan-side parser on the same source and asserts both verdicts
    // agree. Silent on agreement; logs the divergence on disagreement
    // and demotes the exit code so the user sees it.
    if std::env::var("NOD_VERIFY_PARSE").map(|v| v == "1").unwrap_or(false) {
        match dylan_parse_check::verify(&src, id, rust_accepted) {
            Ok(()) => eprintln!("parse-verify: ok (rust+dylan agree on accept={rust_accepted})"),
            Err(e) => {
                eprintln!("parse-verify: DIVERGENCE on {}: {e}", input.display());
                return ExitCode::from(3);
            }
        }
    }
    match result {
        Ok(m) => {
            print!("{}", format_ast_module(&m));
            ExitCode::SUCCESS
        }
        Err(diags) => {
            for d in &diags {
                eprintln!("error: {}", d.message);
            }
            ExitCode::from(1)
        }
    }
}

// ─── Sprint 45a `dump-dylan-tokens` subcommand ────────────────────────────
//
// Embed the Dylan-in-Dylan lexer source into the driver via
// `include_str!`. On invocation:
//   1. Materialise the source to a temp file (path: tempdir/dylan-lexer.dylan).
//   2. Materialise it to an EXE at tempdir/dylan-lexer.exe via run_build.
//   3. Spawn the EXE with the user's input path as argv[1].
//   4. Forward the EXE's stdout to our stdout, byte-for-byte.
//
// The EXE is cached by a hash of the lexer source plus the driver's
// own version so re-runs reuse the same artifact. The cache lives in
// the OS tempdir as `nod-dylan-lexer-<hash>/`. This keeps the
// interactive experience sub-second on warm runs (the compile takes
// a few seconds; the lex step is a couple of milliseconds).
//
// Stub lex (Sprint 45a) → for any input the EXE prints exactly
// `1:1-1:1  EOF\n`. Sprint 45b's real lex fills out the dump; the
// driver path is unchanged.

/// Lexer library source (no main). Lives in the repo at
/// `tests/nod-tests/fixtures/dylan-lexer.dylan`; compiled together with
/// either `dylan-lexer-main.dylan` (for `dump-dylan-tokens`) or
/// `dylan-parser.dylan` (for `parse-dylan`).
const DYLAN_LEXER_SOURCE: &str =
    include_str!("../../../compiler/dylan-lexer.dylan");

/// Lexer entry-point source. Compiled with DYLAN_LEXER_SOURCE to produce
/// the `dump-dylan-tokens` EXE.
const DYLAN_LEXER_MAIN_SOURCE: &str =
    include_str!("../../../compiler/dylan-lexer-main.dylan");

/// Parser source. Compiled with DYLAN_LEXER_SOURCE to produce the
/// `parse-dylan` EXE. Contains its own main().
const DYLAN_PARSER_SOURCE: &str =
    include_str!("../../../compiler/dylan-parser.dylan");

fn dylan_lexer_cache_dir() -> Result<PathBuf, String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    DYLAN_LEXER_SOURCE.hash(&mut h);
    DYLAN_LEXER_MAIN_SOURCE.hash(&mut h);
    env!("CARGO_PKG_VERSION").hash(&mut h);
    let driver = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let meta = std::fs::metadata(&driver)
        .map_err(|e| format!("metadata {}: {e}", driver.display()))?;
    driver.hash(&mut h);
    meta.len().hash(&mut h);
    meta.modified()
        .map_err(|e| format!("modified {}: {e}", driver.display()))?
        .hash(&mut h);
    let digest = h.finish();
    let dir = std::env::temp_dir().join(format!("nod-dylan-lexer-{digest:016x}"));
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("create cache dir {}: {e}", dir.display()))?;
    Ok(dir)
}

fn ensure_dylan_lexer_exe() -> Result<PathBuf, String> {
    let dir = dylan_lexer_cache_dir()?;
    let src      = dir.join("dylan-lexer.dylan");
    let src_main = dir.join("dylan-lexer-main.dylan");
    let exe = dir.join("dylan-lexer.exe");
    // Always (re-)write the sources — cheap, ensures source-tree
    // consistency with the EXE if the hash collided or the source
    // file was deleted out from under us.
    std::fs::write(&src, DYLAN_LEXER_SOURCE)
        .map_err(|e| format!("write {}: {e}", src.display()))?;
    std::fs::write(&src_main, DYLAN_LEXER_MAIN_SOURCE)
        .map_err(|e| format!("write {}: {e}", src_main.display()))?;
    if exe.is_file() {
        return Ok(exe);
    }
    let driver = std::env::current_exe()
        .map_err(|e| format!("current_exe: {e}"))?;
    let out = std::process::Command::new(&driver)
        .arg("build")
        .arg(&src)
        .arg(&src_main)
        .arg("-o")
        .arg(&exe)
        .output()
        .map_err(|e| format!("spawn nod-driver build: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "dylan-lexer build failed: {}\nstdout:\n{}\nstderr:\n{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    if !exe.is_file() {
        return Err(format!(
            "dylan-lexer build claimed success but {} is missing",
            exe.display()
        ));
    }
    Ok(exe)
}

fn run_dump_dylan_ast(input: &std::path::Path) -> ExitCode {
    // Sprint 51d — exercise the AST wire format end-to-end. Fire the
    // shim's resolver if it hasn't already, then call
    // `dylan-parse-emit` and walk the records.
    if let Err(e) = dylan_lex_jit::init() {
        eprintln!("nod-driver dump-dylan-ast: shim init failed: {e}");
        return ExitCode::from(1);
    }
    let src = match std::fs::read_to_string(input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("nod-driver dump-dylan-ast: read {}: {e}", input.display());
            return ExitCode::from(2);
        }
    };
    match dylan_parse_wire::parse_to_tree(&src) {
        Ok(tree) => {
            print!("{}", dylan_parse_wire::format_tree(&tree, &src));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("nod-driver dump-dylan-ast: {e}");
            ExitCode::from(1)
        }
    }
}

/// Sprint 54c — the `--sema-with-dylan` model-dump provider installed into
/// `nod_sema::set_sema_dump_provider`. Given source text, fire the shared
/// resolver once (`init`) then run the in-process Dylan sema walk via the
/// `dylan-sema-emit` shim entry, returning its `dump-sema` model text;
/// `nod_sema` reconstructs a `SemaModel` from it for the back-end.
fn dylan_sema_dump_provider(src: &str) -> Result<String, String> {
    dylan_lex_jit::init()?;
    dylan_parse_wire::sema_emit_via_shim(src)
}

/// Sprint 55 — the `--lower-with-dylan` DFM-dump provider installed into
/// `nod_sema::set_dfm_dump_provider`. Given source text, fire the shared
/// resolver once (`init`) then run the in-process Dylan AST→DFM lowering via the
/// `dylan-lower-emit` shim entry, returning its `dump-dfm`-format text (or `""`
/// when the Dylan lowering bails on an unsupported form); `nod_sema`
/// reconstructs `Vec<Function>` from it and runs the back-end passes.
/// Sprint 56b — the Dylan front-end's expand→lower step (the shim resolver must
/// already be fired). Expand the source Dylan-side so macro-call forms
/// (`unless` / `when` / `cond` / `for-each`) become kernel AST, THEN run the
/// Dylan AST→DFM lowering. The Rust oracle (plain `dump-dfm`, no
/// `--lower-with-dylan`) always expands via `expand_with_stdlib_macros`, so the
/// Dylan lowering path MUST expand too to stay byte-identical. This is
/// INDEPENDENT of `NOD_EXPAND_WITH_DYLAN` (which gates the PARSE-path expander);
/// both call the same shim entry. On expansion error, fall back to the raw
/// source — a macro-free file expands to itself (idempotent after the
/// whitespace-insensitive re-lex); a genuine failure then bails the lowering.
/// Shared by the `--lower-with-dylan` provider and the standalone
/// `dump-dylan-dfm` command so both reflect the full front-end.
fn dylan_expand_then_lower_emit(src: &str) -> Result<String, String> {
    let expanded = match dylan_parse_wire::expand_source_via_shim(
        src,
        nod_sema::stdlib::stdlib_macro_source(),
    ) {
        Ok(exp) => exp,
        Err(_) => src.to_string(),
    };
    dylan_parse_wire::lower_emit_via_shim(&expanded)
}

fn dylan_dfm_dump_provider(src: &str) -> Result<String, String> {
    dylan_lex_jit::init()?;
    dylan_expand_then_lower_emit(src)
}

fn run_dump_dylan_dfm(input: &std::path::Path) -> ExitCode {
    // Sprint 55 Phase 0 — exercise the in-process Dylan lowering end-to-end.
    // Fire the shim resolver if needed, then call `dylan-lower-emit` and print
    // its DFM dump verbatim ("" for modules outside the Phase-0 subset).
    if let Err(e) = dylan_lex_jit::init() {
        eprintln!("nod-driver dump-dylan-dfm: shim init failed: {e}");
        return ExitCode::from(1);
    }
    let src = match std::fs::read_to_string(input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("nod-driver dump-dylan-dfm: read {}: {e}", input.display());
            return ExitCode::from(2);
        }
    };
    match dylan_expand_then_lower_emit(&src) {
        Ok(dfm) => {
            // Sprint 56c-T appended a `=== methods ===` section to the lowering
            // dump (consumed/verified host-side). Plain `dump-dfm` (the byte-match
            // oracle) does NOT print it, so for the standalone DFM view print only
            // the functions part — split off the methods section (and any later
            // `=== … ===` sections) so `dump-dylan-dfm` stays byte-comparable.
            let functions = dfm.split("\n=== ").next().unwrap_or(&dfm);
            print!("{functions}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("nod-driver dump-dylan-dfm: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_dump_dylan_sema(input: &std::path::Path) -> ExitCode {
    // Sprint 54b — exercise the in-process Dylan sema walk end-to-end. Fire
    // the shim's resolver if it hasn't already, then call `dylan-sema-emit`
    // and print its model dump verbatim.
    if let Err(e) = dylan_lex_jit::init() {
        eprintln!("nod-driver dump-dylan-sema: shim init failed: {e}");
        return ExitCode::from(1);
    }
    let src = match std::fs::read_to_string(input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("nod-driver dump-dylan-sema: read {}: {e}", input.display());
            return ExitCode::from(2);
        }
    };
    match dylan_parse_wire::sema_emit_via_shim(&src) {
        Ok(model) => {
            print!("{model}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("nod-driver dump-dylan-sema: {e}");
            ExitCode::from(1)
        }
    }
}

fn run_dump_dylan_tokens(input: &std::path::Path, gc_stats: bool) -> ExitCode {
    let exe = match ensure_dylan_lexer_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("nod-driver dump-dylan-tokens: {e}");
            return ExitCode::from(1);
        }
    };
    // Pass the absolute input path so the EXE's %read-file resolves
    // it independent of the working directory the EXE inherits.
    let input_abs = match std::fs::canonicalize(input) {
        Ok(p) => p,
        Err(_) => input.to_path_buf(),
    };
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(&input_abs);
    if gc_stats {
        cmd.arg("--gc-stats");
    }
    let out = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("nod-driver dump-dylan-tokens: spawn {}: {e}", exe.display());
            return ExitCode::from(1);
        }
    };
    // Forward stdout byte-for-byte so the canonical dump (sprint 45d
    // oracle contract) survives any console transcoding.
    use std::io::Write;
    std::io::stdout().write_all(&out.stdout).ok();
    std::io::stderr().write_all(&out.stderr).ok();
    if out.status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(out.status.code().unwrap_or(1) as u8)
    }
}

// ─── `parse-dylan` subcommand ─────────────────────────────────────────────
//
// Builds [dylan-lexer.dylan, dylan-parser.dylan] into a cached EXE using
// the same strategy as `dump-dylan-tokens`.  The parser's main() reads
// argv[1] as a source path, lexes + parses, then dumps the AST to stdout.

fn dylan_parser_cache_dir() -> Result<PathBuf, String> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    DYLAN_LEXER_SOURCE.hash(&mut h);
    DYLAN_PARSER_SOURCE.hash(&mut h);
    env!("CARGO_PKG_VERSION").hash(&mut h);
    let driver = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let meta = std::fs::metadata(&driver)
        .map_err(|e| format!("metadata {}: {e}", driver.display()))?;
    driver.hash(&mut h);
    meta.len().hash(&mut h);
    meta.modified()
        .map_err(|e| format!("modified {}: {e}", driver.display()))?
        .hash(&mut h);
    let digest = h.finish();
    let dir = std::env::temp_dir().join(format!("nod-dylan-parser-{digest:016x}"));
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("create cache dir {}: {e}", dir.display()))?;
    Ok(dir)
}

fn ensure_dylan_parser_exe() -> Result<PathBuf, String> {
    let dir = dylan_parser_cache_dir()?;
    let src_lexer  = dir.join("dylan-lexer.dylan");
    let src_parser = dir.join("dylan-parser.dylan");
    let exe = dir.join("dylan-parser.exe");
    std::fs::write(&src_lexer, DYLAN_LEXER_SOURCE)
        .map_err(|e| format!("write {}: {e}", src_lexer.display()))?;
    std::fs::write(&src_parser, DYLAN_PARSER_SOURCE)
        .map_err(|e| format!("write {}: {e}", src_parser.display()))?;
    if exe.is_file() {
        return Ok(exe);
    }
    let driver = std::env::current_exe()
        .map_err(|e| format!("current_exe: {e}"))?;
    let out = std::process::Command::new(&driver)
        .arg("build")
        .arg(&src_lexer)
        .arg(&src_parser)
        .arg("-o")
        .arg(&exe)
        .output()
        .map_err(|e| format!("spawn nod-driver build: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "dylan-parser build failed: {}\nstdout:\n{}\nstderr:\n{}",
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    if !exe.is_file() {
        return Err(format!(
            "dylan-parser build claimed success but {} is missing",
            exe.display()
        ));
    }
    Ok(exe)
}

fn run_parse_dylan(input: &std::path::Path) -> ExitCode {
    let exe = match ensure_dylan_parser_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("nod-driver parse-dylan: {e}");
            return ExitCode::from(1);
        }
    };
    let input_abs = match std::fs::canonicalize(input) {
        Ok(p) => p,
        Err(_) => input.to_path_buf(),
    };
    let out = match std::process::Command::new(&exe).arg(&input_abs).output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("nod-driver parse-dylan: spawn {}: {e}", exe.display());
            return ExitCode::from(1);
        }
    };
    use std::io::Write;
    std::io::stdout().write_all(&out.stdout).ok();
    std::io::stderr().write_all(&out.stderr).ok();
    if out.status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(out.status.code().unwrap_or(1) as u8)
    }
}

fn run_dump_graph(input: &std::path::Path) -> ExitCode {
    use nod_namespace::{Graph, dump_graph, load_lid_chain};
    let lid = match load_lid_chain(input) {
        Ok(lid) => lid,
        Err(e) => {
            eprintln!("nod-driver dump-graph: failed to load {}: {e}", input.display());
            return ExitCode::from(2);
        }
    };
    let mut g = Graph::new();
    g.add_library_from_lid(&lid);
    print!("{}", dump_graph(&g));
    ExitCode::SUCCESS
}

// ─── `symbolicate` subcommand ─────────────────────────────────────────
//
// Lives in nod-driver (not nod-runtime) so adding it doesn't shift the
// CGU layout of nod-runtime — the production `.lib` that AOT EXEs link
// against has a fragile archive-extraction rule (see
// `aot_user_main_stub.rs`) that breaks whenever Cargo rearranges
// CGUs. Keeping crash-time helpers OUT of nod-runtime is the rule;
// post-mortem helpers like this one belong here.

fn run_symbolicate(
    map_path: &std::path::Path,
    input: Option<&std::path::Path>,
    output: Option<&std::path::Path>,
    runtime_base_override: Option<&str>,
) -> ExitCode {
    use std::io::{Read, Write};

    let map_raw = match std::fs::read_to_string(map_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("nod-driver symbolicate: read {}: {e}", map_path.display());
            return ExitCode::from(2);
        }
    };
    let (preferred_base, syms) = match parse_link_map(&map_raw) {
        Some(t) => t,
        None => {
            eprintln!(
                "nod-driver symbolicate: failed to parse {} (no `Preferred load address` or no symbol rows)",
                map_path.display()
            );
            return ExitCode::from(2);
        }
    };
    let runtime_base = match runtime_base_override {
        Some(s) => {
            let trimmed = s.trim_start_matches("0x");
            match u64::from_str_radix(trimmed, 16) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!(
                        "nod-driver symbolicate: --runtime-base `{s}` not hex: {e}"
                    );
                    return ExitCode::from(2);
                }
            }
        }
        None => preferred_base,
    };
    let slide = runtime_base as i64 - preferred_base as i64;

    // Read input.
    let text = match input {
        None => {
            let mut s = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut s) {
                eprintln!("nod-driver symbolicate: stdin: {e}");
                return ExitCode::from(2);
            }
            s
        }
        Some(p) => match std::fs::read_to_string(p) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("nod-driver symbolicate: read {}: {e}", p.display());
                return ExitCode::from(2);
            }
        },
    };

    let rewritten = rewrite_hex_ips(&text, &syms, slide);

    // Write output.
    match output {
        None => {
            if let Err(e) = std::io::stdout().write_all(rewritten.as_bytes()) {
                eprintln!("nod-driver symbolicate: stdout: {e}");
                return ExitCode::from(2);
            }
        }
        Some(p) => {
            if let Err(e) = std::fs::write(p, rewritten) {
                eprintln!("nod-driver symbolicate: write {}: {e}", p.display());
                return ExitCode::from(2);
            }
        }
    }
    ExitCode::SUCCESS
}

/// One parsed symbol from a `.map`. Sorted by `rva_plus_base` after
/// parsing.
#[derive(Debug)]
struct LinkMapSym {
    rva_plus_base: u64,
    name: String,
}

/// Parse the MSVC `.map` text format. Returns
/// `(preferred_base, sorted_symbols)`. Tolerates malformed lines —
/// only rows whose first token is `NNNN:NNNN` are taken as symbol
/// definitions.
fn parse_link_map(raw: &str) -> Option<(u64, Vec<LinkMapSym>)> {
    let mut preferred_base: Option<u64> = None;
    let mut syms: Vec<LinkMapSym> = Vec::with_capacity(16384);
    let mut past_header = false;
    for line in raw.lines() {
        if preferred_base.is_none() {
            if let Some(rest) = line.trim_start().strip_prefix("Preferred load address is ") {
                preferred_base = u64::from_str_radix(rest.trim(), 16).ok();
                continue;
            }
        }
        if !past_header {
            if line.trim_start().starts_with("Address ") && line.contains("Rva+Base") {
                past_header = true;
            }
            continue;
        }
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let first = trimmed.split_whitespace().next().unwrap_or("");
        if !is_section_offset(first) {
            continue;
        }
        let tokens: Vec<&str> = trimmed.split_whitespace().collect();
        let rva_plus_base = tokens
            .iter()
            .skip(2)
            .rev()
            .find_map(|t| {
                if t.len() == 16 && t.chars().all(|c| c.is_ascii_hexdigit()) {
                    u64::from_str_radix(t, 16).ok()
                } else {
                    None
                }
            })?;
        let name = tokens.get(1)?.to_string();
        syms.push(LinkMapSym { rva_plus_base, name });
    }
    let base = preferred_base?;
    if syms.is_empty() {
        return None;
    }
    syms.sort_by_key(|s| s.rva_plus_base);
    syms.dedup_by(|a, b| a.rva_plus_base == b.rva_plus_base);
    Some((base, syms))
}

fn is_section_offset(s: &str) -> bool {
    let mut parts = s.split(':');
    let (Some(a), Some(b), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !a.is_empty()
        && a.len() <= 8
        && a.chars().all(|c| c.is_ascii_hexdigit())
        && !b.is_empty()
        && b.len() <= 8
        && b.chars().all(|c| c.is_ascii_hexdigit())
}

/// Find every `0x` followed by 16 hex digits in `text` and rewrite
/// each as `name+0xNN (0x...)`. Anything that doesn't resolve to a
/// symbol stays as-is.
fn rewrite_hex_ips(text: &str, syms: &[LinkMapSym], slide: i64) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Find `0x`.
        if i + 18 <= bytes.len() && &bytes[i..i + 2] == b"0x" {
            let hex = &bytes[i + 2..i + 18];
            if hex.iter().all(|b| b.is_ascii_hexdigit()) {
                let s = std::str::from_utf8(hex).unwrap();
                if let Ok(ip) = u64::from_str_radix(s, 16) {
                    if let Some((name, off)) = lookup_symbol(syms, ip, slide) {
                        // Only emit symbolicated form if the offset is small
                        // (heuristic: < 4MB) — otherwise the IP is more
                        // likely an unrelated random hex value (e.g. a tag
                        // bit pattern from the log).
                        if off < 4 * 1024 * 1024 {
                            out.push_str(&format!("{name}+0x{off:x} (0x{ip:016x})"));
                            i += 18;
                            continue;
                        }
                    }
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn lookup_symbol(syms: &[LinkMapSym], ip: u64, slide: i64) -> Option<(String, usize)> {
    let lookup = (ip as i64).checked_sub(slide)? as u64;
    let idx = match syms.binary_search_by_key(&lookup, |s| s.rva_plus_base) {
        Ok(i) => i,
        Err(0) => return None,
        Err(i) => i - 1,
    };
    Some((syms[idx].name.clone(), (lookup - syms[idx].rva_plus_base) as usize))
}

#[cfg(test)]
mod symbolicate_tests {
    use super::*;

    const SAMPLE: &str = "\
 my-exe

 Preferred load address is 0000000140000000

  Address         Publics by Value              Rva+Base               Lib:Object

 0001:00066ae0       nod_stretchy_vector_push   0000000140067ae0 f   nod_runtime:foo.o
 0001:00067000       another_function           0000000140068000 f   nod_runtime:foo.o
";

    #[test]
    fn parses_map() {
        let (base, syms) = parse_link_map(SAMPLE).expect("parse");
        assert_eq!(base, 0x0000000140000000);
        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].name, "nod_stretchy_vector_push");
    }

    #[test]
    fn rewrites_known_ip() {
        let (base, syms) = parse_link_map(SAMPLE).expect("parse");
        // 0x140067b00 is +0x20 into nod_stretchy_vector_push.
        let inp = "  frame  0: 0x0000000140067b00";
        let out = rewrite_hex_ips(inp, &syms, 0_i64 - base as i64 + base as i64);
        assert!(out.contains("nod_stretchy_vector_push+0x20"));
        assert!(out.contains("0x0000000140067b00"));
    }

    #[test]
    fn leaves_unknown_ip_alone() {
        let (_base, syms) = parse_link_map(SAMPLE).expect("parse");
        // Way past any symbol → unchanged.
        let inp = "0xdeadbeefdeadbeef";
        let out = rewrite_hex_ips(inp, &syms, 0);
        assert_eq!(out, "0xdeadbeefdeadbeef");
    }
}

