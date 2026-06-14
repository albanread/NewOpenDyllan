# `compiler/` — the Dylan-in-Dylan front-end

The self-hosted compiler front-end, written in Dylan. These files were
previously mixed into `tests/nod-tests/fixtures/`; they are not test
fixtures — they are compiler source. Relocated here (Sprint 56 reorg) so
the front-end has its own home, distinct from the test corpus.

## Sources

| File | Role |
|------|------|
| `dylan-lexer.dylan` | Lexer — token class hierarchy, `lex`, `non-trivia-tokens`. `include_str!`'d into `nod-driver`. |
| `dylan-lexer-main.dylan` | Lexer EXE entry (`dump-dylan-tokens`). `include_str!`'d into `nod-driver`. |
| `dylan-parser.dylan` | Parser — emits the AST wire format. `include_str!`'d into `nod-driver`. |
| `dylan-macro.dylan` | Macro engine — collect / match / substitute / module-walk. |
| `dylan-c3.dylan` | C3 linearisation (sema support; the sema walk calls `c3-linearise`). |
| `dylan-sema.dylan` | Sema recording walk — name table, class derivation, `SemaModel`. |
| `dylan-lower.dylan` | AST → DFM lowering. |
| `dylan-lex-shim.dylan` | Host seam — `dylan-lex-collect`, `dylan-parse-emit`, `dylan-parse-collect`, `dylan-sema-emit`, `dylan-lower-emit`. |
| `dylan-lex-shim.prj` | Bundle project: compiles all of the above into one `.obj`. |
| `rope.dylan` | Immutable read-only rope buffer (`Module: rope`). |

## Build / bootstrap

`nod-driver` `include_str!`'s the lexer/parser/lexer-main directly. The
rest reach the driver through the **shim static library**, built via the
no-shim bootstrap:

```
cargo build -p nod-driver                       # no shim yet (Rust front-end)
./target/debug/nod-driver.exe build --library \
    --project compiler/dylan-lex-shim.prj \
    -o compiler/dylan-lex-shim.lib.obj
cargo build -p nod-driver                       # build.rs links the .obj, sets cfg
```

`compiler/dylan-lex-shim.lib.obj` is a build artifact (git-ignored). On a
fresh checkout without it, `--lex-with-dylan` / `--sema-with-dylan` /
`--lower-with-dylan` fall back to Rust with a clear message.

The compiler's own unit/smoke drivers (`dylan-macro-*`, `dylan-c3-smoke`,
`dylan-sema.prj`, `expand-pipeline-smoke`) remain in
`tests/nod-tests/fixtures/` — they are tests, and their `.prj` files
reference these sources via `../../../compiler/`.
