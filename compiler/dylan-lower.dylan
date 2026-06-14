Module: dylan-lexer
Precedence: c

// Sprint 55 Phase-0 — Dylan-side AST->DFM lowering (straight-line subset).
//
// Ports the SIMPLEST slice of the Rust lowering (src/nod-sema/src/lower.rs) to
// Dylan and reproduces the `dump-dfm` text byte-for-byte
// (src/nod-dfm/src/format.rs). Phase 0 handles ONLY:
//   * integer / boolean / string literals
//   * binary ops (arith + comparisons), integer/Top operands
//   * direct calls to known top-level names
//   * functions whose body is one straight-line expression ending in Return
//
// It emits ONLY Const / PrimOp / DirectCall computations and a Return
// terminator, always with empty safepoint roots and never `is_no_alloc` — the
// Phase-0 surface from docs/journal/2026-06-07-sprint-55-lowering-plan.md.
//
// The whole game is the byte-match, and the byte-match is the EMISSION ORDER:
// temp ids and block ids are monotonic counters, so reproducing the exact
// order fresh-temp / new-block fire reproduces the dump. Mirrored Rust line
// refs are cited inline (lower.rs / format.rs / ir.rs).
//
// Bundled in `Module: dylan-lexer` alongside dylan-lexer/parser/sema, so it
// freely calls lex, parse-dylan-with-precedence, the <ast-*> accessors,
// token-source-text, integer-to-string, etc.
//
// Per the slot-default GAP (see dylan-parser.dylan): every slot carries an
// explicit init-keyword and is supplied at `make` time; flags are <object>.

// ─── DFM IR — Dylan mirrors of nod-dfm/src/ir.rs (Phase-0 subset) ──────────

// A temporary (ir.rs Temporary). We store the rendered type label directly
// ("<integer>" etc.), since the dump only needs TypeEstimate::name().
define class <dfm-temp> (<object>)
  slot temp-id   :: <integer>,     init-keyword: id:;
  slot temp-type :: <byte-string>, init-keyword: type:;
end class;

// One computation (ir.rs Computation). Phase 0 builds only const / primop /
// directcall, so a single tagged record is simpler than a class hierarchy.
define class <dfm-comp> (<object>)
  slot comp-kind   :: <byte-string>,     init-keyword: kind:;
  slot comp-dst    :: <integer>,         init-keyword: dst:;
  slot comp-cval   :: <object>,          init-keyword: cval:;    // "Integer(5)" or #f
  slot comp-op     :: <object>,          init-keyword: op:;      // "AddInt" or #f
  slot comp-args   :: <stretchy-vector>, init-keyword: args:;    // <integer> temp ids
  slot comp-callee :: <object>,          init-keyword: callee:;  // name or #f
end class;

// A block (ir.rs Block). Phase 0 makes exactly the entry block. Terminator is
// inlined: block-term-kind is "return"; block-term-value is the <integer> temp
// id, or #f for a bare `Return`.
// A terminator (ir.rs Terminator). kind ∈ {"return","if","jump"}:
//   return: value = <integer> temp or #f.
//   if:     value = cond temp; a = then-label; b = else-label.
//   jump:   a = target-label; args = <stretchy-vector> of temp ids.
// Held as a separate object so <dfm-block>'s `make` stays within the 8-keyword
// limit and every slot is supplied (avoiding the slot-default GAP).
define class <dfm-term> (<object>)
  slot term-kind  :: <byte-string>, init-keyword: kind:;
  slot term-value :: <object>,      init-keyword: value:;
  slot term-a     :: <object>,      init-keyword: a:;
  slot term-b     :: <object>,      init-keyword: b:;
  slot term-args  :: <object>,      init-keyword: args:;
end class;

define function make-return-term (value :: <object>) => (t :: <dfm-term>)
  make(<dfm-term>, kind: "return", value: value, a: #f, b: #f, args: #f)
end function;

define class <dfm-block> (<object>)
  slot block-id     :: <integer>,         init-keyword: id:;
  slot block-label  :: <byte-string>,     init-keyword: label:;
  slot block-params :: <stretchy-vector>, init-keyword: params:;
  slot block-comps  :: <stretchy-vector>, init-keyword: comps:;
  slot block-term   :: <dfm-term>,        init-keyword: term:;
end class;

// A function (ir.rs Function). func-temps is the master temp list (so we can
// answer "type of temp N" cheaply, mirroring Function::temp_type).
define class <dfm-func> (<object>)
  slot func-name        :: <byte-string>,     init-keyword: name:;
  slot func-params      :: <stretchy-vector>, init-keyword: params:;
  slot func-blocks      :: <stretchy-vector>, init-keyword: blocks:;
  slot func-temps       :: <stretchy-vector>, init-keyword: temps:;
  slot func-return-type :: <byte-string>,     init-keyword: return-type:;
end class;

// ─── FunctionBuilder — mirrors lower.rs FunctionBuilder ────────────────────

define class <fn-builder> (<object>)
  slot fb-func       :: <dfm-func>, init-keyword: func:;
  slot fb-current    :: <integer>,  init-keyword: current:;
  slot fb-next-temp  :: <integer>,  init-keyword: next-temp:;
  slot fb-next-block :: <integer>,  init-keyword: next-block:;
  slot fb-last-temp  :: <object>,   init-keyword: last-temp:;
  // LocalEnv (lower.rs LocalEnv = name -> TempId), as parallel vectors: the
  // bindings visible in the current scope (Phase 0: just the params).
  slot fb-env-names  :: <stretchy-vector>, init-keyword: env-names:;
  slot fb-env-temps  :: <stretchy-vector>, init-keyword: env-temps:;
  // Names that are GENERICS in this module (slot getters/setters, define
  // generic, define method). A call to one of these is a Dispatch in Rust
  // (lower.rs 6060), not a DirectCall — until Dispatch lowering lands, such a
  // call bails the whole function to the Rust path (so we never emit a wrong
  // dump). Set by `lower-function`; empty in the synthesized accessor bodies.
  slot fb-generics   :: <stretchy-vector>, init-keyword: generics:;
end class;

// FunctionBuilder::new — entry = BlockId(0) "entry", Return{None}, next_temp=0,
// next_block=1, current=entry.
define function make-fn-builder (name :: <byte-string>) => (b :: <fn-builder>)
  let entry = make(<dfm-block>,
                   id: 0, label: "entry",
                   params: make(<stretchy-vector>),
                   comps:  make(<stretchy-vector>),
                   term: make-return-term(#f));
  let blocks = make(<stretchy-vector>);
  add!(blocks, entry);
  let func = make(<dfm-func>,
                  name: name,
                  params: make(<stretchy-vector>),
                  blocks: blocks,
                  temps:  make(<stretchy-vector>),
                  return-type: "<unit>");
  make(<fn-builder>,
       func: func, current: 0, next-temp: 0, next-block: 1, last-temp: #f,
       env-names: make(<stretchy-vector>), env-temps: make(<stretchy-vector>),
       generics: make(<stretchy-vector>))
end function;

// LocalEnv bind / lookup. `fb-lookup` returns the bound temp id (most-recent
// binding wins, scanning back to front) or #f if the name isn't bound.
define function fb-bind (b :: <fn-builder>, name :: <byte-string>, temp :: <integer>) => ()
  add!(fb-env-names(b), name);
  add!(fb-env-temps(b), temp);
end function;

define function fb-lookup (b :: <fn-builder>, name :: <byte-string>)
 => (temp :: <object>)
  let names = fb-env-names(b);
  let temps = fb-env-temps(b);
  let i = size(names) - 1;
  let found = #f;
  until (i < 0 | found)
    if (names[i] = name) found := temps[i]; end;
    i := i - 1;
  end;
  found
end function;

// fresh_temp — allocate the next temp id, record its type, return id.
define function fb-fresh-temp (b :: <fn-builder>, ty :: <byte-string>)
 => (id :: <integer>)
  let id = fb-next-temp(b);
  fb-next-temp(b) := id + 1;
  add!(func-temps(fb-func(b)), make(<dfm-temp>, id: id, type: ty));
  id
end function;

// push — append a computation to the current block.
define function fb-push (b :: <fn-builder>, c :: <dfm-comp>) => ()
  let blk = func-blocks(fb-func(b))[fb-current(b)];
  add!(block-comps(blk), c);
end function;

// terminate_current — set the current block's Return terminator (value is an
// <integer> temp id, or #f for bare Return).
define function fb-terminate-return (b :: <fn-builder>, value :: <object>) => ()
  let blk = func-blocks(fb-func(b))[fb-current(b)];
  block-term(blk) := make-return-term(value);
end function;

// Function::temp_type — rendered type label of a temp id (Top fallback).
define function fb-temp-type (b :: <fn-builder>, id :: <integer>)
 => (ty :: <byte-string>)
  temp-type-of(func-temps(fb-func(b)), id)
end function;

// new_block — allocate the next block id, append a block labelled
// `<prefix><id>` (matching the Rust new_block labels: "then1", "else2",
// "join3"), default Return{None} terminator. Returns the block's index in
// func-blocks (== its id, since blocks are appended in id order).
define function fb-new-block (b :: <fn-builder>, prefix :: <byte-string>)
 => (index :: <integer>)
  let id = fb-next-block(b);
  fb-next-block(b) := id + 1;
  let blk = make(<dfm-block>,
                 id: id, label: concatenate(prefix, integer-to-string(id)),
                 params: make(<stretchy-vector>),
                 comps:  make(<stretchy-vector>),
                 term: make-return-term(#f));
  let blocks = func-blocks(fb-func(b));
  let index = size(blocks);
  add!(blocks, blk);
  index
end function;

// switch_to — make `index` the current block.
define function fb-switch-to (b :: <fn-builder>, index :: <integer>) => ()
  fb-current(b) := index;
end function;

// Block label by index.
define function fb-block-label (b :: <fn-builder>, index :: <integer>)
 => (label :: <byte-string>)
  block-label(func-blocks(fb-func(b))[index])
end function;

// add_block_param — append a fresh temp (typed `ty`) as a parameter of block
// `index`; returns the temp id (the merged value at a join).
define function fb-add-block-param (b :: <fn-builder>, index :: <integer>,
                                    ty :: <byte-string>) => (temp :: <integer>)
  let t = fb-fresh-temp(b, ty);
  add!(block-params(func-blocks(fb-func(b))[index]), t);
  t
end function;

// terminate the current block with `If <cnd> then-label else-label`.
define function fb-terminate-if (b :: <fn-builder>, cnd :: <integer>,
                                 then-lbl :: <byte-string>, else-lbl :: <byte-string>) => ()
  let blk = func-blocks(fb-func(b))[fb-current(b)];
  block-term(blk) := make(<dfm-term>, kind: "if", value: cnd,
                          a: then-lbl, b: else-lbl, args: #f);
end function;

// terminate the current block with `Jump target(args…)`.
define function fb-terminate-jump (b :: <fn-builder>, target :: <byte-string>,
                                   args :: <stretchy-vector>) => ()
  let blk = func-blocks(fb-func(b))[fb-current(b)];
  block-term(blk) := make(<dfm-term>, kind: "jump", value: #f,
                          a: target, b: #f, args: args);
end function;

// Shared temp-type lookup over a temp list.
define function temp-type-of (temps :: <stretchy-vector>, id :: <integer>)
 => (ty :: <byte-string>)
  let n = size(temps);
  let i = 0;
  let found = #f;
  until (i >= n | found)
    if (temp-id(temps[i]) = id) found := temp-type(temps[i]); end;
    i := i + 1;
  end;
  if (found) found else "<top>" end
end function;

// ── small helpers ──

define function pair-args (a :: <integer>, b :: <integer>)
 => (v :: <stretchy-vector>)
  let v = make(<stretchy-vector>);
  add!(v, a);
  add!(v, b);
  v
end function;

define function singleton-vec (a :: <integer>) => (v :: <stretchy-vector>)
  let v = make(<stretchy-vector>);
  add!(v, a);
  v
end function;

// Shallow copy of a stretchy-vector (for env snapshot / restore at a merge).
define function copy-vec (v :: <stretchy-vector>) => (c :: <stretchy-vector>)
  let c = make(<stretchy-vector>);
  let n = size(v);
  let i = 0;
  until (i >= n)
    add!(c, v[i]);
    i := i + 1;
  end;
  c
end function;

// Is a type label GC-typed (needs a GC root / block-param threading across a
// merge)? Immediate-scalar values (fixnum / boolean / character) are NOT;
// everything else (strings, classes, floats(boxed), Top) conservatively is.
// Used to gate `if`: env-merge threading of GC-typed bindings is a later 55a
// step, so an `if` whose enclosing env holds a GC-typed binding bails to Rust.
define function gc-typed-label? (label :: <byte-string>) => (yes? :: <boolean>)
  ~ (label = "<integer>" | label = "<boolean>" | label = "<character>")
end function;

// Lattice join of two type labels for a merge param (TypeEstimate::join):
// equal → that type; otherwise → Top. (Two distinct user classes both render
// "<class>" via name(), so this is approximate for classes — a 55b concern;
// no class values flow through `if` yet.)
define function join-type-label (a :: <byte-string>, b :: <byte-string>)
 => (label :: <byte-string>)
  if (a = b) a else "<top>" end
end function;

// Const Bool(false) — the value of an `if` with no `else` arm.
define function emit-false-const (b :: <fn-builder>) => (temp :: <integer>)
  let t = fb-fresh-temp(b, "<boolean>");
  fb-push(b, make(<dfm-comp>, kind: "const", dst: t, cval: "Bool(false)",
                  op: #f, args: make(<stretchy-vector>), callee: #f));
  t
end function;

// Const Bool(false) typed `<unit>` — the materialised void value
// FunctionBuilder::unit_temp emits whenever a loop (or any void `Expr::Stmt`)
// is lowered in EXPRESSION position (e.g. as a constituent of a `begin`). Same
// `Const Bool(false)` comp as emit-false-const, but the temp's type is `<unit>`
// (TypeEstimate::Unit) so the surrounding context knows the value is void.
define function emit-unit-const (b :: <fn-builder>) => (temp :: <integer>)
  let t = fb-fresh-temp(b, "<unit>");
  fb-push(b, make(<dfm-comp>, kind: "const", dst: t, cval: "Bool(false)",
                  op: #f, args: make(<stretchy-vector>), callee: #f));
  t
end function;

// ─── Type mapping — mirrors type_from_expr (lower.rs) for scalar cases ─────

define function type-name-to-label (type-name :: <byte-string>)
 => (label :: <byte-string>)
  if (type-name = "<integer>")            "<integer>"
  elseif (type-name = "<single-float>")   "<single-float>"
  elseif (type-name = "<double-float>")   "<double-float>"
  elseif (type-name = "<float>")          "<double-float>"
  elseif (type-name = "<boolean>")        "<boolean>"
  elseif (type-name = "<character>")      "<character>"
  elseif (type-name = "<string>")         "<string>"
  elseif (type-name = "<byte-string>")    "<string>"
  else                                    "<top>"
  end
end function;

// ─── Top-name return-type map (mirrors TopNames::return_type) ───────────────

define class <name-ret-map> (<object>)
  slot nrm-names  :: <stretchy-vector>, init-keyword: names:;
  slot nrm-labels :: <stretchy-vector>, init-keyword: labels:;
end class;

define function nrm-lookup (m :: <name-ret-map>, name :: <byte-string>)
 => (label :: <byte-string>)
  let names = nrm-names(m);
  let n = size(names);
  let i = 0;
  let found = #f;
  until (i >= n | found)
    if (names[i] = name) found := nrm-labels(m)[i]; end;
    i := i + 1;
  end;
  if (found) found else "<top>" end
end function;

// Is `name` a known top-level `define function`? Only these are safe plain
// DirectCalls; a call to any other name (a stdlib function, a generic, or a
// `%`-primitive) needs classification the Dylan side can't do yet, so it bails.
define function nrm-contains? (m :: <name-ret-map>, name :: <byte-string>)
 => (yes? :: <boolean>)
  name-in-vec?(nrm-names(m), name)
end function;

// Declared return label of a `define function`, or #f if none.
define function defn-declared-return-label (defn :: <ast-body-definition>,
                                            user-classes :: <stretchy-vector>,
                                            source :: <byte-string>)
 => (label :: <object>)
  let rspec = defn-return(defn);
  if (~ rspec)
    #f
  else
    let vals = ret-values(rspec);
    if (size(vals) = 0)
      #f
    else
      let tn = vals[0];
      let ty = typed-name-type(tn);
      let type-name =
        if (ty)
          if (instance?(ty, <ast-variable-ref>))
            token-source-text(varref-tok(ty), source)
          else
            ""
          end
        else
          token-source-text(typed-name-tok(tn), source)
        end;
      label-for-type-name(type-name, user-classes)
    end
  end
end function;

// Build name -> declared-return-label map over top-level `define function`s.
define function build-name-ret-map (items :: <stretchy-vector>,
                                    user-classes :: <stretchy-vector>,
                                    source :: <byte-string>)
 => (m :: <name-ret-map>)
  let names  = make(<stretchy-vector>);
  let labels = make(<stretchy-vector>);
  let n = size(items);
  let i = 0;
  until (i >= n)
    let item = items[i];
    if (instance?(item, <ast-body-definition>))
      let word = token-source-text(defn-word(item), source);
      if (word = "function")
        let name-tok = defn-method-name(item);
        if (name-tok)
          let name = token-source-text(name-tok, source);
          let lbl  = defn-declared-return-label(item, user-classes, source);
          add!(names, name);
          add!(labels, if (lbl) lbl else "<top>" end);
        end;
      end;
    end;
    i := i + 1;
  end;
  make(<name-ret-map>, names: names, labels: labels)
end function;

// ─── select_binop — mirrors lower.rs select_binop (Phase-0 int / Top) ───────

define function select-binop (op-text :: <byte-string>,
                              lt :: <byte-string>, rt :: <byte-string>)
 => (prim :: <object>)
  let int-ok? = (lt = "<integer>" | lt = "<top>") & (rt = "<integer>" | rt = "<top>");
  if (~ int-ok?)            #f
  elseif (op-text = "+")    "AddInt"
  elseif (op-text = "-")    "SubInt"
  elseif (op-text = "*")    "MulInt"
  elseif (op-text = "/")    "DivInt"
  elseif (op-text = "mod")  "ModInt"
  elseif (op-text = "rem")  "RemInt"
  elseif (op-text = "=")    "EqInt"
  elseif (op-text = "==")   "EqInt"
  elseif (op-text = "~=")   "NeInt"
  elseif (op-text = "~==")  "NeInt"
  elseif (op-text = "<")    "LtInt"
  elseif (op-text = ">")    "GtInt"
  elseif (op-text = "<=")   "LeInt"
  elseif (op-text = ">=")   "GeInt"
  else                      #f
  end
end function;

// PrimOp::result_type label: arith -> <integer>, comparison -> <boolean>.
define function primop-result-label (prim :: <byte-string>)
 => (label :: <byte-string>)
  if (prim = "AddInt" | prim = "SubInt" | prim = "MulInt"
        | prim = "DivInt" | prim = "ModInt" | prim = "RemInt" | prim = "NegInt")
    "<integer>"
  elseif (prim = "AddFloat" | prim = "SubFloat" | prim = "MulFloat"
            | prim = "DivFloat" | prim = "NegFloat")
    "<double-float>"
  else
    "<boolean>"             // comparisons + boolean ops -> <boolean>
  end
end function;

// ─── lower-expr — mirrors lower.rs lower_expr (Phase-0 subset) ──────────────
//
// Lowers one expression node into computations on `b`, returning its result
// temp id (an <integer>), or #f if the node is outside Phase-0 scope (the
// caller treats #f as "fixture not yet Dylan-lowerable").

define function lower-expr (b :: <fn-builder>, node :: <object>,
                            ret-map :: <name-ret-map>, source :: <byte-string>)
 => (temp :: <object>)
  if (instance?(node, <ast-variable-ref>))
    // A bare name: Phase 0 only resolves params / locals in the env (lower.rs
    // lower_expr Ident → local-env read). A name not in the env (stdlib
    // constant, class ref, bare function-ref) is outside Phase 0 → #f.
    fb-lookup(b, token-source-text(varref-tok(node), source))
  elseif (instance?(node, <ast-integer-lit>))
    let v = lit-value(node);
    let t = fb-fresh-temp(b, "<integer>");
    let cval = concatenate("Integer(", concatenate(integer-to-string(v), ")"));
    fb-push(b, make(<dfm-comp>, kind: "const", dst: t, cval: cval,
                    op: #f, args: make(<stretchy-vector>), callee: #f));
    t
  elseif (instance?(node, <ast-boolean-lit>))
    let t = fb-fresh-temp(b, "<boolean>");
    let cval = if (lit-value(node)) "Bool(true)" else "Bool(false)" end;
    fb-push(b, make(<dfm-comp>, kind: "const", dst: t, cval: cval,
                    op: #f, args: make(<stretchy-vector>), callee: #f));
    t
  elseif (instance?(node, <ast-string-lit>))
    let t = fb-fresh-temp(b, "<string>");
    let raw = lit-value(node);
    let cval = concatenate("String(\"", concatenate(escape-string-debug(raw), "\")"));
    fb-push(b, make(<dfm-comp>, kind: "const", dst: t, cval: cval,
                    op: #f, args: make(<stretchy-vector>), callee: #f));
    t
  elseif (instance?(node, <ast-list-lit>))
    // `#(a, b, c)` literal list (lower.rs 5994-6042). The parser builds an
    // <ast-list-lit>; lower each element left-to-right, then the cons chain
    // RIGHT-to-left: `tail = %nil()`, then `%pair-alloc(elt, tail)` per element
    // in reverse. dst type `<class>` for both `%nil` (Class(EMPTY_LIST)) and
    // `%pair-alloc` (Class(PAIR)). Empty `#()` -> just `%nil()`. Temp order:
    // elements (source), nil, pairs (reverse) — matches Rust's fresh_temp
    // sequence exactly. An improper list (`#(a . b)`, lit-tail set) bails —
    // Rust represents it differently.
    if (lit-tail(node))
      #f
    else
      let elems = lit-elems(node);
      let n = size(elems);
      let elem-temps = make(<stretchy-vector>);
      let i = 0;
      let ok? = #t;
      until (i >= n | ~ ok?)
        let et = lower-expr(b, elems[i], ret-map, source);
        if (~ et) ok? := #f; else add!(elem-temps, et); end;
        i := i + 1;
      end;
      if (~ ok?)
        #f
      else
        let tail = fb-fresh-temp(b, "<class>");
        fb-push(b, make(<dfm-comp>, kind: "directcall", dst: tail, cval: #f,
                        op: #f, args: make(<stretchy-vector>), callee: "%nil"));
        let j = size(elem-temps) - 1;
        until (j < 0)
          let pair-dst = fb-fresh-temp(b, "<class>");
          let pargs = make(<stretchy-vector>);
          add!(pargs, elem-temps[j]);
          add!(pargs, tail);
          fb-push(b, make(<dfm-comp>, kind: "directcall", dst: pair-dst, cval: #f,
                          op: #f, args: pargs, callee: "%pair-alloc"));
          tail := pair-dst;
          j := j - 1;
        end;
        tail
      end
    end
  elseif (instance?(node, <ast-binary-op>))
    let op-text = token-source-text(binop-operator(node), source);
    if (op-text = ":=")
      // `lhs := rhs` — plain-local SSA rebind (lower_assign). Lower the RHS and,
      // if the LHS is a simple env-bound name, rebind name->rhs-temp; the
      // assignment value IS the rhs temp and NO computation is emitted for the
      // assignment itself. A non-simple / unbound LHS is outside scope -> #f.
      lower-assign(b, node, ret-map, source)
    elseif (op-text = "|" | op-text = "&")
      // `|` / `&` short-circuit — a diamond, NOT a PrimOp (lower_short_circuit).
      lower-short-circuit(b, node, op-text, ret-map, source)
    else
      // Strict binop. Operands lower left-then-right — this ORDER fixes the
      // operand temp ids.
      let l = lower-expr(b, binop-left(node), ret-map, source);
      let r = lower-expr(b, binop-right(node), ret-map, source);
      if (~ l | ~ r)
        #f
      else
        let lt = fb-temp-type(b, l);
        let rt = fb-temp-type(b, r);
        let prim = select-binop(op-text, lt, rt);
        if (~ prim)
          #f
        else
          let dst = fb-fresh-temp(b, primop-result-label(prim));
          fb-push(b, make(<dfm-comp>, kind: "primop", dst: dst, cval: #f,
                          op: prim, args: pair-args(l, r), callee: #f));
          dst
        end
      end
    end
  elseif (instance?(node, <ast-unary-op>))
    // Prefix `- operand` -> PrimOp NegInt (integer) / NegFloat (float),
    // mirroring lower.rs. `~` (not) and other prefixes are later -> #f.
    let op = token-source-text(unary-op(node), source);
    if (op = "-")
      let operand = lower-expr(b, unary-operand(node), ret-map, source);
      if (~ operand)
        #f
      else
        let prim = if (fb-temp-type(b, operand) = "<double-float>") "NegFloat"
                   else "NegInt" end;
        let dst = fb-fresh-temp(b, primop-result-label(prim));
        fb-push(b, make(<dfm-comp>, kind: "primop", dst: dst, cval: #f,
                        op: prim, args: singleton-vec(operand), callee: #f));
        dst
      end
    elseif (op = "~")
      // `~ operand` -> PrimOp BoolNot, dst <boolean> (lower.rs). Mirrors the
      // `-` branch; primop-result-label("BoolNot") -> <boolean> (else branch).
      let operand = lower-expr(b, unary-operand(node), ret-map, source);
      if (~ operand)
        #f
      else
        let dst = fb-fresh-temp(b, primop-result-label("BoolNot"));
        fb-push(b, make(<dfm-comp>, kind: "primop", dst: dst, cval: #f,
                        op: "BoolNot", args: singleton-vec(operand), callee: #f));
        dst
      end
    else
      #f
    end
  elseif (instance?(node, <ast-call>))
    let callee-node = call-fn(node);
    if (~ instance?(callee-node, <ast-variable-ref>))
      #f
    else
      let name = token-source-text(varref-tok(callee-node), source);
      // `instance?(value, <class>)` -> `TypeCheck value <label>` dst <boolean>
      // (lower_instance_check, lower.rs 6467): lower the value, mint the dst
      // last. The class arg must be a bare class name (a variable-ref); a
      // complex type expression bails.
      if (name = "instance?")
        let arg-nodes = call-args(node);
        if (size(arg-nodes) ~= 2)
          #f
        else
          let cls-node = unwrap-arg(arg-nodes[1]);
          if (~ instance?(cls-node, <ast-variable-ref>))
            #f
          else
            let v = lower-expr(b, unwrap-arg(arg-nodes[0]), ret-map, source);
            if (~ v)
              #f
            else
              let label = instance-check-label(token-source-text(varref-tok(cls-node), source));
              let dst = fb-fresh-temp(b, "<boolean>");
              fb-push(b, make(<dfm-comp>, kind: "typecheck", dst: dst, cval: label,
                              op: #f, args: singleton-vec(v), callee: #f));
              dst
            end
          end
        end
      // `make(<C>, kw: v, …)` -> a ClassMetadataPtr Const (class emitted BY
      // NAME — the host reconstruction resolves it against the registered class
      // table), then interleaved (SymbolLiteralRef(kw), value) consts, then
      // `DirectCall %make(…)` dst <top> (lower_make, lower.rs 6095). A
      // positional (non-keyword) supplied arg bails (make-from shapes — later).
      elseif (name = "make")
        let arg-nodes = call-args(node);
        if (size(arg-nodes) < 1)
          #f
        else
          let cls-node = unwrap-arg(arg-nodes[0]);
          if (~ instance?(cls-node, <ast-variable-ref>))
            #f
          else
            let make-args = make(<stretchy-vector>);
            let cptr = fb-fresh-temp(b, "<top>");
            fb-push(b, make(<dfm-comp>, kind: "const", dst: cptr,
                            cval: concatenate("ClassMetadataPtr(",
                                    concatenate(token-source-text(varref-tok(cls-node), source),
                                                ", tagged=false)")),
                            op: #f, args: make(<stretchy-vector>), callee: #f));
            add!(make-args, cptr);
            let ok? = #t;
            let i = 1;
            let n = size(arg-nodes);
            until (i >= n | ~ ok?)
              let a = arg-nodes[i];
              if (~ instance?(a, <ast-kw-arg>))
                ok? := #f;
              else
                let kw = keyword-name-token-name(kw-arg-key(a));
                let symt = fb-fresh-temp(b, "<top>");
                fb-push(b, make(<dfm-comp>, kind: "const", dst: symt,
                                cval: concatenate("SymbolLiteralRef(\"",
                                        concatenate(escape-string-debug(kw), "\")")),
                                op: #f, args: make(<stretchy-vector>), callee: #f));
                add!(make-args, symt);
                let v = lower-expr(b, kw-arg-value(a), ret-map, source);
                if (~ v) ok? := #f; else add!(make-args, v); end;
              end;
              i := i + 1;
            end;
            if (~ ok?)
              #f
            else
              let dst = fb-fresh-temp(b, "<top>");
              fb-push(b, make(<dfm-comp>, kind: "directcall", dst: dst, cval: #f,
                              op: #f, args: make-args, callee: "%make"));
              dst
            end
          end
        end
      // `pair`/`head`/`tail`/`empty?`/`nil` -> a `%pair*`/`%nil`/`%empty?`
      // DirectCall (lower.rs lower_list_builtin, checked BEFORE dispatch and not
      // shadowed by a user name). Args lower left-to-right, dst minted last; dst
      // type per the builtin. Wrong arity is an invalid program (Rust errors) —
      // bail so we never emit a malformed call.
      elseif (list-builtin-callee(name))
        let arg-nodes = call-args(node);
        let n = size(arg-nodes);
        if (n ~= list-builtin-arity(name))
          #f
        else
          let arg-temps = make(<stretchy-vector>);
          let i = 0;
          let ok? = #t;
          until (i >= n | ~ ok?)
            let at = lower-expr(b, unwrap-arg(arg-nodes[i]), ret-map, source);
            if (~ at) ok? := #f; else add!(arg-temps, at); end;
            i := i + 1;
          end;
          if (~ ok?)
            #f
          else
            let dst = fb-fresh-temp(b, list-builtin-result-label(name));
            fb-push(b, make(<dfm-comp>, kind: "directcall", dst: dst, cval: #f,
                            op: #f, args: arg-temps, callee: list-builtin-callee(name)));
            dst
          end
        end
      // A call to a generic -> `Dispatch g(args)` dst <top>, EMPTY safepoint set
      // (the host liveness pass populates it; the resolver may later rewrite to
      // Direct/SealedDirectCall). A name is a generic if it's a module slot
      // getter (fb-generics) OR a registered generic (`%is-generic?`, which sees
      // stdlib generics like `size`/`add!` — they're registered before lowering
      // runs). But a name that is ALSO a known top-level function is a
      // DirectCall, not a Dispatch (Rust checks top_names first), hence the
      // `~ nrm-contains?` guard. lower.rs 6045-6068.
      elseif (~ nrm-contains?(ret-map, name)
                & (name-in-vec?(fb-generics(b), name) | %is-generic?(name)))
        let arg-nodes = call-args(node);
        let n = size(arg-nodes);
        let arg-temps = make(<stretchy-vector>);
        let i = 0;
        let ok? = #t;
        until (i >= n | ~ ok?)
          let at = lower-expr(b, unwrap-arg(arg-nodes[i]), ret-map, source);
          if (~ at) ok? := #f; else add!(arg-temps, at); end;
          i := i + 1;
        end;
        if (~ ok?)
          #f
        else
          let dst = fb-fresh-temp(b, "<top>");
          fb-push(b, make(<dfm-comp>, kind: "dispatch", dst: dst, cval: #f,
                          op: #f, args: arg-temps, callee: name));
          dst
        end
      elseif (starts-with-percent?(name))
        // A `%`-primitive call -> DirectCall against the `nod_…` runtime symbol
        // (`prim-callee`/`prim-arity`/`prim-result-label` mirror lower.rs
        // LOWER_PRIMITIVE_TABLE). An UNKNOWN `%`-prim (not in the table) BAILS
        // rather than fall through to the plain-DirectCall else below — which
        // would emit the raw `%foo` callee (wrong). Args lower left-to-right,
        // dst minted last (matches fresh_temp(ret) ordering); empty safepoint
        // set (the host liveness pass populates it post-flip), so flip-only.
        let pcallee = prim-callee(name);
        if (~ pcallee)
          #f
        else
          let parity = prim-arity(name);
          let plabel = prim-result-label(name);
          let arg-nodes = call-args(node);
          let n = size(arg-nodes);
          if (n ~= parity)
            #f
          else
            let arg-temps = make(<stretchy-vector>);
            let i = 0;
            let ok? = #t;
            until (i >= n | ~ ok?)
              let at = lower-expr(b, unwrap-arg(arg-nodes[i]), ret-map, source);
              if (~ at) ok? := #f; else add!(arg-temps, at); end;
              i := i + 1;
            end;
            if (~ ok?)
              #f
            else
              let dst = fb-fresh-temp(b, plabel);
              fb-push(b, make(<dfm-comp>, kind: "directcall", dst: dst, cval: #f,
                              op: #f, args: arg-temps, callee: pcallee));
              dst
            end
          end
        end
      else
        // Either a known top-level `define function` (declared return) or a
        // non-generic stdlib function / unknown ident (dst falls back to
        // <top>) — both are plain DirectCalls (`nrm-lookup` gives the right
        // dst either way). Args lower left-to-right BEFORE the dst is minted
        // (dst id comes after all arg ids, matching lower.rs fresh_temp(ret)).
        let arg-nodes = call-args(node);
        let n = size(arg-nodes);
        let arg-temps = make(<stretchy-vector>);
        let i = 0;
        let ok? = #t;
        until (i >= n | ~ ok?)
          let an = arg-nodes[i];
          let av = if (instance?(an, <ast-pos-arg>)) pos-arg-value(an) else an end;
          let at = lower-expr(b, av, ret-map, source);
          if (~ at) ok? := #f; else add!(arg-temps, at); end;
          i := i + 1;
        end;
        if (~ ok?)
          #f
        else
          let ret-label = nrm-lookup(ret-map, name);
          let dst = fb-fresh-temp(b, ret-label);
          fb-push(b, make(<dfm-comp>, kind: "directcall", dst: dst, cval: #f,
                          op: #f, args: arg-temps, callee: name));
          dst
        end
      end
    end
  elseif (instance?(node, <ast-statement>))
    // Control-flow statements in expression position. 55a: `if`, `while`,
    // `until`. 56: `begin`. Others (case / block / method-literal) are later → #f.
    let word = token-source-text(stmt-word(node), source);
    if (word = "if")
      lower-if-expr(b, node, ret-map, source)
    elseif (word = "while")
      lower-loop(b, node, #f, ret-map, source)
    elseif (word = "until")
      lower-loop(b, node, #t, ret-map, source)
    elseif (word = "begin")
      // `begin S1; … Sn end` is a TRANSPARENT body sequence (lower.rs Expr::Begin,
      // no block/scope wrapper in the DFM); value = last statement's value. Its
      // stmt-body carries NO leading condition, so lower all constituents.
      lower-block-value(b, body-constituents(stmt-body(node)), ret-map, source)
    else
      #f
    end
  else
    // Outside the current subset (unary, floats, chars, symbols,
    // make/instance?/%-prims, begin/loops, …): later → #f.
    #f
  end
end function;

// ─── lower-let — mirrors a Statement::Let with a single binder ─────────────
//
// `let binder = init` (an <ast-local-decl> whose ldecl-list is the binder
// binop). Lowers the init expression and binds the binder name to its temp
// (a non-captured let in Rust lowering is just a name->value-temp binding — no
// extra computation; cell promotion for captured lets is 55c). Returns the
// init temp, or #f if outside the Phase-0/55a subset (multi-binder destructure,
// `let x` with no init, or an unsupported init).
define function lower-let (b :: <fn-builder>, decl :: <ast-local-decl>,
                           ret-map :: <name-ret-map>, source :: <byte-string>)
 => (temp :: <object>)
  let list = ldecl-list(decl);
  let cs = body-constituents(list);
  if (size(cs) ~= 1)
    #f                                  // `let (a, b) = …` multi-binder — 55a+
  else
    let node = cs[0];
    if (~ instance?(node, <ast-binary-op>))
      #f                                // `let x` with no initialiser — bail
    else
      let lhs = binop-left(node);
      let name =
        if (instance?(lhs, <ast-variable-ref>))
          token-source-text(varref-tok(lhs), source)
        elseif (instance?(lhs, <ast-typed-name>))
          token-source-text(typed-name-tok(lhs), source)
        else
          #f
        end;
      if (~ name)
        #f
      else
        let t = lower-expr(b, binop-right(node), ret-map, source);
        if (~ t)
          #f
        else
          fb-bind(b, name, t);
          t
        end
      end
    end
  end
end function;

// Lower one body constituent (a `let` decl or an expression). Returns its
// value temp, or #f if unsupported.
define function lower-body-stmt (b :: <fn-builder>, item :: <object>,
                                 ret-map :: <name-ret-map>, source :: <byte-string>)
 => (temp :: <object>)
  if (instance?(item, <ast-local-decl>))
    lower-let(b, item, ret-map, source)
  else
    lower-expr(b, item, ret-map, source)
  end
end function;

// Lower a range of body constituents [start, end) in order; the last *value* is
// returned. #f if any is unsupported, or the range is empty.
//
// A body statement's result is classified (lower_function_inner's last_temp
// update): #f → bail; an <integer> temp → a value (updates `last`); anything
// else truthy (the void marker `#t` from a loop, which produces no value) is a
// void statement — it is lowered for effect but does NOT update `last`.
define function lower-stmt-range (b :: <fn-builder>, cs :: <stretchy-vector>,
                                  start :: <integer>, ret-map :: <name-ret-map>,
                                  source :: <byte-string>)
 => (temp :: <object>)
  let n = size(cs);
  let i = start;
  let last = #f;
  let ok? = #t;
  until (i >= n | ~ ok?)
    let t = lower-body-stmt(b, cs[i], ret-map, source);
    if (~ t)
      ok? := #f;
    elseif (instance?(t, <integer>))
      last := t;
    end;
    i := i + 1;
  end;
  if (~ ok?) #f else last end
end function;

// ─── lower-block-value — mirrors `Expr::Begin` (lower_expr) ────────────────
// A `begin … end` is a block EXPRESSION whose value is the LAST statement's
// value. Unlike lower-stmt-range (a function-body helper that lowers a loop for
// effect and tracks the last INTEGER value), a `begin` constituent in
// expression position materialises a loop's void value: Rust's `unit_temp`
// emits a `<unit>` Const Bool(false) at the loop_exit (per loop, interior ones
// too), and that const IS the constituent's value. So a trailing void loop
// yields a real `<unit>` temp, not #f.
define function lower-block-value (b :: <fn-builder>, cs :: <stretchy-vector>,
                                   ret-map :: <name-ret-map>, source :: <byte-string>)
 => (temp :: <object>)
  let n = size(cs);
  if (n = 0)
    #f                                   // empty `begin` — Rust raises; bail.
  else
    let i = 0;
    let last = #f;
    let ok? = #t;
    until (i >= n | ~ ok?)
      let t = lower-body-stmt(b, cs[i], ret-map, source);
      if (~ t)
        ok? := #f;
      elseif (instance?(t, <integer>))
        last := t;                       // a value constituent.
      else
        // Void marker (a loop): in expression position the materialised value
        // is a `<unit>` Const Bool(false) after the loop (lands in loop_exit).
        last := emit-unit-const(b);
      end;
      i := i + 1;
    end;
    if (~ ok?) #f else last end
  end
end function;

// ─── lower-short-circuit — mirrors lower_short_circuit (`|` / `&`) ──────────
//
// `a | b` / `a & b` lower to an sc_edge / sc_rhs / sc_join diamond. The LHS is
// evaluated in the current block; on the short-circuit outcome control jumps to
// sc_edge carrying the LHS value, otherwise to sc_rhs which evaluates the RHS
// and jumps with its value; sc_join's block-param is the result.
//   `|`: LHS true  → sc_edge (value = LHS); false → sc_rhs.  (If lhs edge rhs)
//   `&`: LHS true  → sc_rhs;  false → sc_edge (value = LHS). (If lhs rhs edge)
// Like `if`, the join carries the result (first param) plus any var assigned in
// the RHS (or GC-typed in env) — sorted. The sc_edge arm (short-circuit, RHS not
// run) carries the PRE-rhs values; sc_rhs carries the post-rhs values. The join
// is created AFTER the RHS (so a nested-control-flow RHS orders right). RHS is an
// expression (no lets to evict), so no env snapshot/restore is needed.
define function lower-short-circuit (b :: <fn-builder>, node :: <ast-binary-op>,
                                     op :: <byte-string>, ret-map :: <name-ret-map>,
                                     source :: <byte-string>)
 => (temp :: <object>)
  let lhs = lower-expr(b, binop-left(node), ret-map, source);
  if (~ lhs)
    #f
  else
    let lhs-ty = fb-temp-type(b, lhs);
    // Merge set = vars assigned in the RHS ∪ GC-typed env names, sorted.
    let merge = make(<stretchy-vector>);
    collect-assigned(b, binop-right(node), source, merge);
    let enames = fb-env-names(b);
    let etemps = fb-env-temps(b);
    let ne = size(enames);
    let gi = 0;
    until (gi >= ne)
      if (gc-typed-label?(fb-temp-type(b, etemps[gi]))) set-add!(merge, enames[gi]); end;
      gi := gi + 1;
    end;
    lower-sort-strings!(merge);
    let nm = size(merge);
    // Capture the PRE-rhs merge temps (for the short-circuit edge).
    let edge-merge = make(<stretchy-vector>);
    let ci = 0;
    until (ci >= nm) add!(edge-merge, fb-lookup(b, merge[ci])); ci := ci + 1; end;
    let edge-idx = fb-new-block(b, "sc_edge");
    let rhs-idx = fb-new-block(b, "sc_rhs");
    if (op = "|")
      fb-terminate-if(b, lhs, fb-block-label(b, edge-idx), fb-block-label(b, rhs-idx));
    else
      fb-terminate-if(b, lhs, fb-block-label(b, rhs-idx), fb-block-label(b, edge-idx));
    end;
    // sc_rhs: evaluate the RHS.
    fb-switch-to(b, rhs-idx);
    let rhs = lower-expr(b, binop-right(node), ret-map, source);
    if (~ rhs)
      #f
    else
      let rhs-ty = fb-temp-type(b, rhs);
      let rhs-end = fb-current(b);
      let rhs-merge = make(<stretchy-vector>);
      let mj = 0;
      until (mj >= nm) add!(rhs-merge, fb-lookup(b, merge[mj])); mj := mj + 1; end;
      // Join after the RHS.
      let join-idx = fb-new-block(b, "sc_join");
      let join-lbl = fb-block-label(b, join-idx);
      // sc_edge → join([lhs] + pre-rhs merge…)
      let edge-args = make(<stretchy-vector>);
      add!(edge-args, lhs);
      let ei = 0;
      until (ei >= nm) add!(edge-args, edge-merge[ei]); ei := ei + 1; end;
      fb-switch-to(b, edge-idx);
      fb-terminate-jump(b, join-lbl, edge-args);
      // sc_rhs → join([rhs] + post-rhs merge…)
      let rhs-args = make(<stretchy-vector>);
      add!(rhs-args, rhs);
      let ri = 0;
      until (ri >= nm) add!(rhs-args, rhs-merge[ri]); ri := ri + 1; end;
      fb-switch-to(b, rhs-end);
      fb-terminate-jump(b, join-lbl, rhs-args);
      // Join params: value first, then merge vars; rebind env to the params.
      fb-switch-to(b, join-idx);
      let value-param = fb-add-block-param(b, join-idx, join-type-label(lhs-ty, rhs-ty));
      let pk = 0;
      until (pk >= nm)
        let pty = join-type-label(fb-temp-type(b, edge-merge[pk]),
                                  fb-temp-type(b, rhs-merge[pk]));
        fb-bind(b, merge[pk], fb-add-block-param(b, join-idx, pty));
        pk := pk + 1;
      end;
      value-param
    end
  end
end function;

// ─── lower-if-expr — mirrors lower_if (the value-merge, non-mutating case) ──
//
// `if (cond) then-body [else else-body] end` → a 3-block diamond
// (then/else/join) with the merged value as the single join block-param — the
// shape Rust's lower_if produces when no arm assigns a variable and the
// enclosing env holds no GC-typed binding (so nothing else threads through the
// join). Block ids/labels and temp ids reproduce the Rust emission order:
// cond temps (entry) → then-body temps → else-body temps → join param.
//
// Bails (#f, → Rust path) on: any GC-typed env binding (env-merge threading is
// a later 55a step), `elseif` chains, or any unsupported arm expression
// (e.g. an arm that assigns — `:=` isn't lowered yet, so it bails naturally).
define function lower-if-expr (b :: <fn-builder>, stmt :: <ast-statement>,
                               ret-map :: <name-ret-map>, source :: <byte-string>)
 => (temp :: <object>)
  let scs = body-constituents(stmt-body(stmt));
  if (size(scs) < 1)
    #f
  else
    // Resolve the else arm. `if (c) … end` -> no else; `… else B end` -> B;
    // `… elseif (c2) B2 <rest…> end` -> desugar to a NESTED `if` statement
    // (mirrors Rust's nested-if lowering of if/elseif/else): the else arm
    // becomes the single synthetic statement `if (c2) B2 <rest…> end`, lowered
    // recursively by the SAME machinery — so block ids / temps / merge sets
    // nest exactly as Rust's. An elseif clause's body already carries its cond
    // as the first constituent (the same shape as a leading `if` body), so it
    // transplants directly; we reuse the original `if` word token (lower-expr
    // routes the synthetic node on its "if" text).
    let clauses = stmt-clauses(stmt);
    let else-cs = #f;
    let bail? = #f;
    if (instance?(clauses, <stretchy-vector>))
      let nc = size(clauses);
      if (nc >= 1)
        let cl0 = clauses[0];
        let w0 = token-source-text(clause-word(cl0), source);
        if (w0 = "else")
          if (nc = 1)
            else-cs := body-constituents(clause-body(cl0));
          else
            bail? := #t;            // `else` with trailing clauses — malformed
          end;
        elseif (w0 = "elseif")
          // Build the nested `if` via a factory in dylan-parser (where
          // <ast-statement> is defined) — calling `make(<ast-statement>)` here
          // would make dylan-lower.dylan reference a class out of its own
          // standalone scope. `rest` (#f or the remaining clauses) becomes the
          // nested if's clauses.
          let rest = #f;
          if (nc > 1)
            let r = make(<stretchy-vector>);
            let ri = 1;
            until (ri >= nc) add!(r, clauses[ri]); ri := ri + 1; end;
            rest := r;
          end;
          let nested = make-if-statement(stmt-word(stmt), clause-body(cl0), rest);
          else-cs := make(<stretchy-vector>);
          add!(else-cs, nested);
        else
          bail? := #t;              // case / exception / … — later
        end;
      end;
    end;
    if (bail?)
      #f
    else
      // Merge set = vars assigned in either arm ∪ GC-typed env names, sorted.
      // That order = join-param order = jump-arg order, value param FIRST.
      let merge = make(<stretchy-vector>);
      let ti = 1;
      let tn = size(scs);
      until (ti >= tn)
        collect-assigned(b, scs[ti], source, merge);
        ti := ti + 1;
      end;
      if (instance?(else-cs, <stretchy-vector>))
        let ei = 0;
        let en = size(else-cs);
        until (ei >= en)
          collect-assigned(b, else-cs[ei], source, merge);
          ei := ei + 1;
        end;
      end;
      let enames = fb-env-names(b);
      let etemps = fb-env-temps(b);
      let ne = size(enames);
      let gi = 0;
      until (gi >= ne)
        if (gc-typed-label?(fb-temp-type(b, etemps[gi])))
          set-add!(merge, enames[gi]);
        end;
        gi := gi + 1;
      end;
      lower-sort-strings!(merge);
      let nm = size(merge);
      // Condition (lowered in the current block).
      let cnd = lower-expr(b, scs[0], ret-map, source);
      if (~ cnd)
        #f
      else
        let then-idx = fb-new-block(b, "then");
        let else-idx = fb-new-block(b, "else");
        fb-terminate-if(b, cnd, fb-block-label(b, then-idx), fb-block-label(b, else-idx));
        // Snapshot env so the else arm starts from the pre-if bindings.
        let snap-names = copy-vec(fb-env-names(b));
        let snap-temps = copy-vec(fb-env-temps(b));
        // then arm
        fb-switch-to(b, then-idx);
        let then-val = lower-stmt-range(b, scs, 1, ret-map, source);
        if (~ then-val)
          #f
        else
          let then-ty = fb-temp-type(b, then-val);
          let then-end = fb-current(b);            // arm may have branched
          let then-merge = make(<stretchy-vector>);
          let mi = 0;
          until (mi >= nm) add!(then-merge, fb-lookup(b, merge[mi])); mi := mi + 1; end;
          // Restore env for the else arm.
          fb-env-names(b) := copy-vec(snap-names);
          fb-env-temps(b) := copy-vec(snap-temps);
          fb-switch-to(b, else-idx);
          let else-val =
            if (instance?(else-cs, <stretchy-vector>))
              lower-stmt-range(b, else-cs, 0, ret-map, source)
            else
              emit-false-const(b)
            end;
          if (~ else-val)
            #f
          else
            let else-ty = fb-temp-type(b, else-val);
            let else-end = fb-current(b);
            let else-merge = make(<stretchy-vector>);
            let mj = 0;
            until (mj >= nm) add!(else-merge, fb-lookup(b, merge[mj])); mj := mj + 1; end;
            // Join created AFTER both arms (GAP-010): its id follows any blocks
            // a nested-control-flow arm created.
            let join-idx = fb-new-block(b, "join");
            let join-lbl = fb-block-label(b, join-idx);
            // then-end → join(then-val, then-merge…)
            let then-args = make(<stretchy-vector>);
            add!(then-args, then-val);
            let ai = 0;
            until (ai >= nm) add!(then-args, then-merge[ai]); ai := ai + 1; end;
            fb-switch-to(b, then-end);
            fb-terminate-jump(b, join-lbl, then-args);
            // else-end → join(else-val, else-merge…)
            let else-args = make(<stretchy-vector>);
            add!(else-args, else-val);
            let aj = 0;
            until (aj >= nm) add!(else-args, else-merge[aj]); aj := aj + 1; end;
            fb-switch-to(b, else-end);
            fb-terminate-jump(b, join-lbl, else-args);
            // Join params: VALUE first, then merge vars (sorted). Then rebind
            // env to pre-if + the merge vars' new join params.
            fb-switch-to(b, join-idx);
            let value-param =
              fb-add-block-param(b, join-idx, join-type-label(then-ty, else-ty));
            fb-env-names(b) := copy-vec(snap-names);
            fb-env-temps(b) := copy-vec(snap-temps);
            let pk = 0;
            until (pk >= nm)
              let pty = join-type-label(fb-temp-type(b, then-merge[pk]),
                                        fb-temp-type(b, else-merge[pk]));
              fb-bind(b, merge[pk], fb-add-block-param(b, join-idx, pty));
              pk := pk + 1;
            end;
            value-param
          end
        end
      end
    end
  end
end function;

// ─── lower-assign — mirrors lower_assign (plain-local SSA-rebind case) ──────
//
// `lhs := rhs`. Lower the RHS to a temp; if the LHS is a bare name currently
// bound in env (a plain local / param), REBIND name->rhs-temp and return the
// rhs temp — emitting NO computation for the assignment itself (an SSA rebind
// is just an env update; the value of `:=` is the RHS). A non-simple LHS, or a
// name not in env (module variable / cell-promoted local — later sprints),
// bails to the Rust path (#f). The 55a subset has no GC-typed locals, so the
// cell/closure/module-variable branches of lower_assign never apply here.
define function lower-assign (b :: <fn-builder>, node :: <ast-binary-op>,
                              ret-map :: <name-ret-map>, source :: <byte-string>)
 => (temp :: <object>)
  let lhs = binop-left(node);
  if (instance?(lhs, <ast-variable-ref>))
    let name = token-source-text(varref-tok(lhs), source);
    if (~ fb-lookup(b, name))
      #f                                  // unbound name — module var / later
    else
      let t = lower-expr(b, binop-right(node), ret-map, source);
      if (~ t)
        #f
      else
        fb-bind(b, name, t);              // SSA rebind; most-recent wins
        t
      end
    end
  elseif (instance?(lhs, <ast-call>))
    // `slot(obj) := v` -> `Dispatch <slot>-setter(obj, value)`. lower.rs's
    // try_resolve_slot_offset always returns None, so a slot assignment is a
    // setter Dispatch, never a StoreSlot. Obj args lower first, then the value;
    // dst minted last (lower_assign unary case, args [obj, value]). Unary
    // slot-setter only; an n-ary setter (value-first order) bails.
    let callee-node = call-fn(lhs);
    let arg-nodes = call-args(lhs);
    if (~ instance?(callee-node, <ast-variable-ref>) | size(arg-nodes) ~= 1)
      #f
    else
      let setter = concatenate(token-source-text(varref-tok(callee-node), source), "-setter");
      let obj = lower-expr(b, unwrap-arg(arg-nodes[0]), ret-map, source);
      if (~ obj)
        #f
      else
        let val = lower-expr(b, binop-right(node), ret-map, source);
        if (~ val)
          #f
        else
          let dst = fb-fresh-temp(b, "<top>");
          fb-push(b, make(<dfm-comp>, kind: "dispatch", dst: dst, cval: #f,
                          op: #f, args: pair-args(obj, val), callee: setter));
          dst
        end
      end
    end
  else
    #f
  end
end function;

// ── string sort (local; mirrors bs-le? / sort-strings! in dylan-sema.dylan) ──
// Byte-wise lexical compare a <= b. Shorter-but-equal-prefix sorts first.
define function lower-bs-le? (a :: <byte-string>, b :: <byte-string>)
 => (yes? :: <boolean>)
  let na = size(a);
  let nb = size(b);
  let n = if (na < nb) na else nb end;
  let i = 0;
  let result = #f;
  let decided = #f;
  until (i >= n | decided)
    let ca = %byte-string-element(a, i);
    let cb = %byte-string-element(b, i);
    if (ca < cb)      result := #t; decided := #t;
    elseif (ca > cb)  result := #f; decided := #t;
    end;
    i := i + 1;
  end;
  if (decided) result else na <= nb end
end function;

// In-place insertion sort of a stretchy-vector of <byte-string> (ascending).
define function lower-sort-strings! (v :: <stretchy-vector>) => ()
  let n = size(v);
  let i = 1;
  until (i >= n)
    let x = v[i];
    let j = i;
    until (j = 0 | lower-bs-le?(v[j - 1], x))
      v[j] := v[j - 1];
      j := j - 1;
    end;
    v[j] := x;
    i := i + 1;
  end;
end function;

// Add `name` to set-vector `v` if not already present (HashSet::insert).
define function set-add! (v :: <stretchy-vector>, name :: <byte-string>) => ()
  let n = size(v);
  let i = 0;
  let found = #f;
  until (i >= n | found)
    if (v[i] = name) found := #t; end;
    i := i + 1;
  end;
  if (~ found) add!(v, name); end;
end function;

// ── carried-set walks — mirror collect_used_bound_names_* / collect_assigned_*
//
// Both walk the loop's cond + body collecting env-bound names into a set-vector
// `out`. They recurse over binops, calls (callee + positional-arg values),
// control statements (if/while/until: stmt-body constituents + clause bodies),
// and nested `let` initialisers — the node shapes that appear in the 55a subset.

// Names that are READ (and currently bound in env). An <ast-variable-ref> whose
// name is in env is a use. (`x := …` also reaches here via the binop LHS, which
// is harmless: the assigned name is carried regardless.)
define function collect-used (b :: <fn-builder>, node :: <object>,
                              source :: <byte-string>, out :: <stretchy-vector>) => ()
  if (instance?(node, <ast-variable-ref>))
    let name = token-source-text(varref-tok(node), source);
    if (fb-lookup(b, name)) set-add!(out, name); end;
  elseif (instance?(node, <ast-binary-op>))
    collect-used(b, binop-left(node), source, out);
    collect-used(b, binop-right(node), source, out);
  elseif (instance?(node, <ast-call>))
    collect-used(b, call-fn(node), source, out);
    let args = call-args(node);
    let n = size(args);
    let i = 0;
    until (i >= n)
      let an = args[i];
      let av = if (instance?(an, <ast-pos-arg>)) pos-arg-value(an) else an end;
      collect-used(b, av, source, out);
      i := i + 1;
    end;
  elseif (instance?(node, <ast-local-decl>))
    collect-used-in-body(b, ldecl-list(node), source, out);
  elseif (instance?(node, <ast-statement>))
    collect-used-in-body(b, stmt-body(node), source, out);
    let clauses = stmt-clauses(node);
    if (instance?(clauses, <stretchy-vector>))
      let n = size(clauses);
      let i = 0;
      until (i >= n)
        collect-used-in-body(b, clause-body(clauses[i]), source, out);
        i := i + 1;
      end;
    end;
  end;
end function;

define function collect-used-in-body (b :: <fn-builder>, body :: <object>,
                                      source :: <byte-string>, out :: <stretchy-vector>) => ()
  let cs = body-constituents(body);
  let n = size(cs);
  let i = 0;
  until (i >= n)
    collect-used(b, cs[i], source, out);
    i := i + 1;
  end;
end function;

// Names ASSIGNED via `:=` to a bound env name (collect_assigned_in_*). For an
// assignment binop the LHS env-name is added and only the RHS is recursed;
// other binops recurse both sides. `let` shadowing a bound outer name marks
// that name assigned (Sprint 18 rule), matching the Rust binder-shadow arm.
define function collect-assigned (b :: <fn-builder>, node :: <object>,
                                  source :: <byte-string>, out :: <stretchy-vector>) => ()
  if (instance?(node, <ast-binary-op>))
    let op-text = token-source-text(binop-operator(node), source);
    if (op-text = ":=")
      let lhs = binop-left(node);
      if (instance?(lhs, <ast-variable-ref>))
        let name = token-source-text(varref-tok(lhs), source);
        if (fb-lookup(b, name)) set-add!(out, name); end;
      end;
      collect-assigned(b, binop-right(node), source, out);
    else
      collect-assigned(b, binop-left(node), source, out);
      collect-assigned(b, binop-right(node), source, out);
    end;
  elseif (instance?(node, <ast-call>))
    collect-assigned(b, call-fn(node), source, out);
    let args = call-args(node);
    let n = size(args);
    let i = 0;
    until (i >= n)
      let an = args[i];
      let av = if (instance?(an, <ast-pos-arg>)) pos-arg-value(an) else an end;
      collect-assigned(b, av, source, out);
      i := i + 1;
    end;
  elseif (instance?(node, <ast-local-decl>))
    collect-assigned-in-body(b, ldecl-list(node), source, out);
  elseif (instance?(node, <ast-statement>))
    collect-assigned-in-body(b, stmt-body(node), source, out);
    let clauses = stmt-clauses(node);
    if (instance?(clauses, <stretchy-vector>))
      let n = size(clauses);
      let i = 0;
      until (i >= n)
        collect-assigned-in-body(b, clause-body(clauses[i]), source, out);
        i := i + 1;
      end;
    end;
  end;
end function;

define function collect-assigned-in-body (b :: <fn-builder>, body :: <object>,
                                          source :: <byte-string>, out :: <stretchy-vector>) => ()
  let cs = body-constituents(body);
  let n = size(cs);
  let i = 0;
  until (i >= n)
    collect-assigned(b, cs[i], source, out);
    i := i + 1;
  end;
end function;

// ─── lower-loop — mirrors lower_while_like (while + until) ──────────────────
//
// `while (cond) body… end` / `until (cond) body… end`. stmt-body = [cond,
// body…]. Builds the loop_header / loop_body / loop_exit CFG with the carried
// (phi) set threaded through the header block-params.
//
//   loop_header(phi…):  cond_t = <cond>;  If cond_t <then> <else>
//      while: then=body  else=exit ;  until: then=exit  else=body
//      (ONLY the branch labels swap — the cond primop is NOT negated.)
//   loop_body:          <body stmts>;  Jump loop_header(carried env temps…)
//   loop_exit:          continue (the loop's value is void).
//
// Carried set = names assigned via `:=` in the body, OR used in cond/body, OR
// GC-typed in env — sorted lexically. That single order governs header-param
// order, the entry-jump args, and the back-edge args. Returns the void marker
// (#t) on success, or #f if any sub-lowering bails (-> Rust path).
//
// Block creation order is load-bearing for the byte-match: header FIRST (id H);
// header params consume temp ids BEFORE the cond is lowered; body/exit are
// created AFTER lowering the cond (so any sc_* blocks from a short-circuit cond
// precede them — GAP-009).
define function lower-loop (b :: <fn-builder>, stmt :: <ast-statement>,
                            invert? :: <object>, ret-map :: <name-ret-map>,
                            source :: <byte-string>)
 => (temp :: <object>)
  let scs = body-constituents(stmt-body(stmt));
  if (size(scs) < 1)
    #f                                   // no condition — malformed
  else
    let cond-node = scs[0];
    // (1) loop_header FIRST (id H).
    let header-idx = fb-new-block(b, "loop_header");
    // (2) Carried set: assigned ∪ used ∪ GC-typed env names, then sort.
    let carried = make(<stretchy-vector>);
    collect-assigned-in-body(b, stmt-body(stmt), source, carried);  // cond + body
    let used = make(<stretchy-vector>);
    collect-used(b, cond-node, source, used);
    let bi = 1;
    let nb = size(scs);
    until (bi >= nb)
      collect-used(b, scs[bi], source, used);
      bi := bi + 1;
    end;
    // Add GC-typed env names + used names to the carried set (assigned already
    // in). (No 55a fixture has GC-typed locals, but mirror the Rust rule.)
    let enames = fb-env-names(b);
    let etemps = fb-env-temps(b);
    let ne = size(enames);
    let ei = 0;
    until (ei >= ne)
      if (gc-typed-label?(fb-temp-type(b, etemps[ei])))
        set-add!(carried, enames[ei]);
      end;
      ei := ei + 1;
    end;
    let nu = size(used);
    let ui = 0;
    until (ui >= nu)
      // Only carry names still bound in env (used is already env-filtered).
      set-add!(carried, used[ui]);
      ui := ui + 1;
    end;
    lower-sort-strings!(carried);
    // (3) Capture each carried name's CURRENT (pre-loop) env temp in sorted
    // order, and add a header block-param per carried name (consuming temp ids
    // BEFORE the cond is lowered). Bail if any carried name is somehow unbound.
    let nc = size(carried);
    let pre-temps = make(<stretchy-vector>);
    let phis = make(<stretchy-vector>);
    let ok? = #t;
    let ci = 0;
    until (ci >= nc | ~ ok?)
      let outer = fb-lookup(b, carried[ci]);
      if (~ outer)
        ok? := #f;
      else
        add!(pre-temps, outer);
        let phi = fb-add-block-param(b, header-idx, fb-temp-type(b, outer));
        add!(phis, phi);
      end;
      ci := ci + 1;
    end;
    if (~ ok?)
      #f
    else
      let header-lbl = fb-block-label(b, header-idx);
      // (4) Entry-side jump → header with pre-loop temps (sorted order).
      fb-terminate-jump(b, header-lbl, pre-temps);
      // (5) Rebind env name->phi so header/body read the loop phis.
      let ri = 0;
      until (ri >= nc)
        fb-bind(b, carried[ri], phis[ri]);
        ri := ri + 1;
      end;
      // (6) header: lower cond, then create body/exit, then branch.
      fb-switch-to(b, header-idx);
      let cond-t = lower-expr(b, cond-node, ret-map, source);
      if (~ cond-t)
        #f
      else
        let body-idx = fb-new-block(b, "loop_body");
        let exit-idx = fb-new-block(b, "loop_exit");
        let body-lbl = fb-block-label(b, body-idx);
        let exit-lbl = fb-block-label(b, exit-idx);
        if (invert?)
          fb-terminate-if(b, cond-t, exit-lbl, body-lbl);   // until polarity
        else
          fb-terminate-if(b, cond-t, body-lbl, exit-lbl);   // while polarity
        end;
        // (7) loop_body: lower body stmts (`:=` rebinds env), then back-edge.
        fb-switch-to(b, body-idx);
        let body-ok? = #t;
        let si = 1;
        until (si >= nb | ~ body-ok?)
          let t = lower-body-stmt(b, scs[si], ret-map, source);
          if (~ t) body-ok? := #f; end;   // <integer>/void both fine (discarded)
          si := si + 1;
        end;
        if (~ body-ok?)
          #f
        else
          // Back-edge args: env[name] for each carried name, in sorted order.
          let back-args = make(<stretchy-vector>);
          let qi = 0;
          until (qi >= nc)
            add!(back-args, fb-lookup(b, carried[qi]));
            qi := qi + 1;
          end;
          fb-terminate-jump(b, header-lbl, back-args);
          // (8) Restore env name->phi (post-loop reads see the header phi),
          // then continue at exit. The loop's own value is void.
          let xi = 0;
          until (xi >= nc)
            fb-bind(b, carried[xi], phis[xi]);
            xi := xi + 1;
          end;
          fb-switch-to(b, exit-idx);
          #t                              // void marker — loop produces no value
        end
      end
    end
  end
end function;

// ─── 55b: slot-accessor emission (Phase 3) + generic-name table ────────────
//
// For each `define class`, the Rust lowering synthesizes a getter (and, unless
// the slot is `constant`, a setter) per OWN slot, emitted BEFORE all user
// functions (lower.rs Phase 3, builders build_slot_getter/build_slot_setter at
// lower.rs 3371/3420). These bodies are the ONLY place `LoadSlot`/`StoreSlot`
// appear — a user `slot(obj) := v` lowers to a `<slot>-setter` Dispatch, never
// a StoreSlot (lower.rs try_resolve_slot_offset always returns None).
//
// Offsets are deterministic: own slot i sits at `8 + i*8` (runtime classes.rs;
// the Dylan sema walk computes the same at dylan-sema.dylan). We only handle
// classes whose sole super is `<object>` (no inherited slots → own slots start
// at @8); anything else bails the module (offsets would shift).

// String membership in a vector (for the generics table).
define function name-in-vec? (v :: <stretchy-vector>, s :: <byte-string>)
 => (yes? :: <boolean>)
  let n = size(v);
  let i = 0;
  let found = #f;
  until (i >= n | found)
    if (v[i] = s) found := #t; end;
    i := i + 1;
  end;
  found
end function;

// Does `name` start with `%` (a primitive call, e.g. `%make-stretchy-vector`)?
// ('%' is byte 37.)
define function starts-with-percent? (name :: <byte-string>) => (yes? :: <boolean>)
  size(name) > 0 & %byte-string-element(name, 0) = 37
end function;

// ClassCheck::name() — the label printed for a `TypeCheck`. Most class names
// pass through verbatim (builtins like <integer>/<boolean>/<character>/<symbol>,
// <object>, and user classes by source name), but two builtins normalize to
// their canonical class: <string> -> <byte-string>, <vector> ->
// <simple-object-vector> (ir.rs ClassCheck variants String / Vector).
define function instance-check-label (name :: <byte-string>)
 => (label :: <byte-string>)
  if (name = "<string>")     "<byte-string>"
  elseif (name = "<vector>") "<simple-object-vector>"
  else                       name
  end
end function;

// Unwrap a call argument node (positional args are wrapped in <ast-pos-arg>).
define function unwrap-arg (an :: <object>) => (v :: <object>)
  if (instance?(an, <ast-pos-arg>)) pos-arg-value(an) else an end
end function;

// ── <pair> / <list> builtins (lower.rs Sprint 16, ListBuiltin) ─────────────
// `pair` / `head` / `tail` / `empty?` / `nil` lower to a DirectCall against a
// synthetic `%pair*` / `%nil` / `%empty?` runtime-shim callee (codegen turns
// these into the matching `nod_runtime` shim). The Rust lowering checks these
// BEFORE the generic/top-name dispatch decision and is NOT shadowed by a
// user-defined name, so we mirror that precedence. `list-builtin-callee`
// returns the `%…` callee for one of these names, else #f.
define function list-builtin-callee (name :: <byte-string>) => (callee :: <object>)
  if (name = "pair")        "%pair-alloc"
  elseif (name = "head")    "%pair-head"
  elseif (name = "tail")    "%pair-tail"
  elseif (name = "empty?")  "%empty?"
  elseif (name = "nil")     "%nil"
  else                      #f
  end
end function;

// Required arity of a list builtin (ListBuiltin::arity): pair=2, nil=0, else 1.
define function list-builtin-arity (name :: <byte-string>) => (n :: <integer>)
  if (name = "pair")     2
  elseif (name = "nil")  0
  else                   1
  end
end function;

// Result-type label of a list builtin (lower.rs lower_list_builtin result_ty):
// pair -> Class(<pair>) (`<class>`), nil -> Class(<empty-list>) (`<class>`),
// empty? -> Boolean (`<boolean>`), head/tail -> Top (`<top>`).
define function list-builtin-result-label (name :: <byte-string>) => (label :: <byte-string>)
  if (name = "pair")        "<class>"
  elseif (name = "nil")     "<class>"
  elseif (name = "empty?")  "<boolean>"
  else                      "<top>"
  end
end function;

// ── `%`-primitive table (lower.rs LOWER_PRIMITIVE_TABLE) ───────────────────
// A `%`-prefixed primitive call lowers to a DirectCall against the `nod_…`
// runtime symbol, with the table's arity + return-type label. The name map is
// LITERAL (no mechanical `%foo`->`nod_foo` transform — e.g. `%vector-size`->
// `nod_sov_size`), so this mirrors the Rust table verbatim. Generated from
// `src/nod-sema/src/lower.rs` (see docs/journal/2026-06-10). Returns
// `vector(callee :: <byte-string>, arity :: <integer>, label :: <byte-string>)`
// or #f. The host liveness pass adds safepoint roots post-flip (lowering emits
// an empty set), and NO primitive carries `[no_alloc]` on the live path, so
// these are FLIP-ONLY fixtures.
define function prim-callee (name :: <byte-string>) => (callee :: <object>)
  if (name = "%error") "nod_error"
  elseif (name = "%values-clear") "nod_values_clear"
  elseif (name = "%values-set!") "nod_values_set"
  elseif (name = "%values-get") "nod_values_get"
  elseif (name = "%values-count") "nod_values_count"
  elseif (name = "%collection-size") "nod_collection_size"
  elseif (name = "%collection-concatenate") "nod_collection_concatenate"
  elseif (name = "%range-from") "nod_range_from"
  elseif (name = "%range-to") "nod_range_to"
  elseif (name = "%range-by") "nod_range_by"
  elseif (name = "%vector-size") "nod_sov_size"
  elseif (name = "%vector-element") "nod_sov_element"
  elseif (name = "%vector-element-setter") "nod_sov_element_setter"
  elseif (name = "%stretchy-vector-size") "nod_stretchy_vector_size"
  elseif (name = "%stretchy-vector-element") "nod_stretchy_vector_element"
  elseif (name = "%stretchy-vector-push") "nod_stretchy_vector_push"
  elseif (name = "%fip-init") "nod_fip_init"
  elseif (name = "%fip-finished?") "nod_fip_finished_p"
  elseif (name = "%fip-current-element") "nod_fip_current_element"
  elseif (name = "%fip-advance!") "nod_fip_advance"
  elseif (name = "%make-range") "nod_make_range"
  elseif (name = "%make-stretchy-vector") "nod_make_stretchy_vector"
  elseif (name = "%funcall0") "nod_funcall0"
  elseif (name = "%funcall1") "nod_funcall1"
  elseif (name = "%funcall2") "nod_funcall2"
  elseif (name = "%funcall3") "nod_funcall3"
  elseif (name = "%funcall4") "nod_funcall4"
  elseif (name = "%funcall5") "nod_funcall5"
  elseif (name = "%apply") "nod_apply"
  elseif (name = "%make-sov") "nod_make_sov_len"
  elseif (name = "%make-cell") "nod_make_cell"
  elseif (name = "%cell-get") "nod_cell_get"
  elseif (name = "%cell-set!") "nod_cell_set"
  elseif (name = "%env-cell") "nod_env_cell"
  elseif (name = "%make-environment") "nod_make_environment"
  elseif (name = "%make-closure") "nod_make_closure"
  elseif (name = "%byte-string-allocate") "nod_byte_string_allocate"
  elseif (name = "%byte-string-size") "nod_byte_string_size"
  elseif (name = "%byte-string-element") "nod_byte_string_element"
  elseif (name = "%byte-string-element-setter") "nod_byte_string_element_setter"
  elseif (name = "%byte-string-copy!") "nod_byte_string_copy_bytes"
  elseif (name = "%is-generic?") "nod_is_generic_defined"
  elseif (name = "%is-class?") "nod_is_class_defined"
  elseif (name = "%make-table") "nod_make_table"
  elseif (name = "%table-size") "nod_table_size"
  elseif (name = "%table-element") "nod_table_element"
  elseif (name = "%table-element-or-default") "nod_table_element_or_default"
  elseif (name = "%table-element-setter") "nod_table_element_setter"
  elseif (name = "%table-remove-key") "nod_table_remove_key"
  elseif (name = "%table-keys") "nod_table_keys"
  elseif (name = "%table-values") "nod_table_values"
  elseif (name = "%object-hash") "nod_object_hash"
  elseif (name = "%object-equal?") "nod_object_equal_p"
  elseif (name = "%register-wndproc") "nod_register_wndproc"
  elseif (name = "%register-wndenumproc") "nod_register_wndenumproc"
  elseif (name = "%struct-get-i32") "nod_struct_get_i32"
  elseif (name = "%struct-set-i32") "nod_struct_set_i32"
  elseif (name = "%struct-get-i64") "nod_struct_get_i64"
  elseif (name = "%struct-set-i64") "nod_struct_set_i64"
  elseif (name = "%struct-get-u16") "nod_struct_get_u16"
  elseif (name = "%struct-set-u16") "nod_struct_set_u16"
  elseif (name = "%struct-get-u32") "nod_struct_get_u32"
  elseif (name = "%struct-set-u32") "nod_struct_set_u32"
  elseif (name = "%struct-get-u64") "nod_struct_get_u64"
  elseif (name = "%struct-set-u64") "nod_struct_set_u64"
  elseif (name = "%struct-get-pointer") "nod_struct_get_pointer"
  elseif (name = "%struct-set-pointer") "nod_struct_set_pointer"
  elseif (name = "%com-release") "nod_com_release"
  elseif (name = "%com-registry-len") "nod_com_registry_len"
  elseif (name = "%com-last-hresult") "nod_com_last_hresult"
  elseif (name = "%com-clear-last-hresult") "nod_com_clear_last_hresult"
  elseif (name = "%dxgi-create-factory") "nod_dxgi_create_factory"
  elseif (name = "%dxgi-device-from-d3d-device") "nod_dxgi_device_from_d3d_device"
  elseif (name = "%dxgi-create-surface-from-texture") "nod_dxgi_create_surface_from_texture"
  elseif (name = "%d3d11-create-device") "nod_d3d11_create_device"
  elseif (name = "%d3d11-get-immediate-context") "nod_d3d11_get_immediate_context"
  elseif (name = "%d3d11-create-texture-2d") "nod_d3d11_create_texture_2d"
  elseif (name = "%d3d11-copy-to-staging-and-map") "nod_d3d11_copy_to_staging_and_map"
  elseif (name = "%d3d11-last-staging-handle") "nod_d3d11_last_staging_handle"
  elseif (name = "%d3d11-last-mapped-row-pitch") "nod_d3d11_last_mapped_row_pitch"
  elseif (name = "%d3d11-unmap") "nod_d3d11_unmap"
  elseif (name = "%d2d-create-factory") "nod_d2d_create_factory"
  elseif (name = "%d2d-create-device") "nod_d2d_create_device"
  elseif (name = "%d2d-create-device-context") "nod_d2d_create_device_context"
  elseif (name = "%d2d-create-bitmap-for-target") "nod_d2d_create_bitmap_for_target"
  elseif (name = "%d2d-set-target") "nod_d2d_set_target"
  elseif (name = "%d2d-begin-draw") "nod_d2d_begin_draw"
  elseif (name = "%d2d-end-draw") "nod_d2d_end_draw"
  elseif (name = "%d2d-clear") "nod_d2d_clear"
  elseif (name = "%d2d-set-transform-identity") "nod_d2d_set_transform_identity"
  elseif (name = "%d2d-create-solid-color-brush") "nod_d2d_create_solid_color_brush"
  elseif (name = "%d2d-draw-text-layout") "nod_d2d_draw_text_layout"
  elseif (name = "%d2d-draw-rectangle") "nod_d2d_draw_rectangle"
  elseif (name = "%d2d-fill-rectangle") "nod_d2d_fill_rectangle"
  elseif (name = "%dwrite-create-factory") "nod_dwrite_create_factory"
  elseif (name = "%dwrite-create-text-format") "nod_dwrite_create_text_format"
  elseif (name = "%dwrite-create-text-layout") "nod_dwrite_create_text_layout"
  elseif (name = "%dwrite-get-layout-metrics") "nod_dwrite_get_layout_metrics"
  elseif (name = "%dwrite-hit-test-position") "nod_dwrite_hit_test_text_position"
  elseif (name = "%dwrite-hit-test-point") "nod_dwrite_hit_test_point"
  elseif (name = "%dwrite-set-drawing-effect") "nod_dwrite_set_drawing_effect"
  elseif (name = "%dwrite-set-line-spacing") "nod_dwrite_set_line_spacing"
  elseif (name = "%count-non-zero-red") "nod_count_non_zero_red"
  elseif (name = "%dxgi-factory-from-d3d-device") "nod_dxgi_factory_from_d3d_device"
  elseif (name = "%dxgi-create-swap-chain-for-hwnd") "nod_dxgi_create_swap_chain_for_hwnd"
  elseif (name = "%d2d-create-bitmap-from-swap-chain") "nod_d2d_create_bitmap_from_swap_chain"
  elseif (name = "%dxgi-swap-chain-present") "nod_dxgi_swap_chain_present"
  elseif (name = "%dxgi-swap-chain-resize-buffers") "nod_dxgi_swap_chain_resize_buffers"
  elseif (name = "%register-window-class") "nod_register_window_class"
  elseif (name = "%create-message-only-window") "nod_create_message_only_window"
  elseif (name = "%create-hidden-window") "nod_create_hidden_window"
  elseif (name = "%destroy-window") "nod_destroy_window"
  elseif (name = "%post-message") "nod_post_message"
  elseif (name = "%pump-one-message") "nod_pump_one_message"
  elseif (name = "%run-message-loop") "nod_run_message_loop"
  elseif (name = "%def-window-proc") "nod_def_window_proc"
  elseif (name = "%read-file") "nod_read_file_to_string"
  elseif (name = "%argv1") "nod_get_argv1"
  elseif (name = "%argv2") "nod_get_argv2"
  elseif (name = "%print-gc-stats") "nod_print_gc_stats"
  elseif (name = "%lo-word") "nod_lo_word"
  elseif (name = "%hi-word") "nod_hi_word"
  elseif (name = "%set-scroll-info") "nod_set_scroll_info"
  elseif (name = "%get-scroll-pos") "nod_get_scroll_pos"
  elseif (name = "%show-open-file-dialog") "nod_show_open_file_dialog"
  elseif (name = "%write-file") "nod_write_file_from_string"
  elseif (name = "%show-save-file-dialog") "nod_show_save_file_dialog"
  else #f
  end
end function;

define function prim-arity (name :: <byte-string>) => (n :: <integer>)
  if (name = "%error") 1
  elseif (name = "%values-clear") 0
  elseif (name = "%values-set!") 2
  elseif (name = "%values-get") 1
  elseif (name = "%values-count") 0
  elseif (name = "%collection-size") 1
  elseif (name = "%collection-concatenate") 2
  elseif (name = "%range-from") 1
  elseif (name = "%range-to") 1
  elseif (name = "%range-by") 1
  elseif (name = "%vector-size") 1
  elseif (name = "%vector-element") 2
  elseif (name = "%vector-element-setter") 3
  elseif (name = "%stretchy-vector-size") 1
  elseif (name = "%stretchy-vector-element") 2
  elseif (name = "%stretchy-vector-push") 2
  elseif (name = "%fip-init") 1
  elseif (name = "%fip-finished?") 1
  elseif (name = "%fip-current-element") 1
  elseif (name = "%fip-advance!") 1
  elseif (name = "%make-range") 3
  elseif (name = "%make-stretchy-vector") 1
  elseif (name = "%funcall0") 1
  elseif (name = "%funcall1") 2
  elseif (name = "%funcall2") 3
  elseif (name = "%funcall3") 4
  elseif (name = "%funcall4") 5
  elseif (name = "%funcall5") 6
  elseif (name = "%apply") 2
  elseif (name = "%make-sov") 1
  elseif (name = "%make-cell") 1
  elseif (name = "%cell-get") 1
  elseif (name = "%cell-set!") 2
  elseif (name = "%env-cell") 2
  elseif (name = "%make-environment") 1
  elseif (name = "%make-closure") 3
  elseif (name = "%byte-string-allocate") 1
  elseif (name = "%byte-string-size") 1
  elseif (name = "%byte-string-element") 2
  elseif (name = "%byte-string-element-setter") 3
  elseif (name = "%byte-string-copy!") 5
  elseif (name = "%is-generic?") 1
  elseif (name = "%is-class?") 1
  elseif (name = "%make-table") 1
  elseif (name = "%table-size") 1
  elseif (name = "%table-element") 2
  elseif (name = "%table-element-or-default") 3
  elseif (name = "%table-element-setter") 3
  elseif (name = "%table-remove-key") 2
  elseif (name = "%table-keys") 1
  elseif (name = "%table-values") 1
  elseif (name = "%object-hash") 1
  elseif (name = "%object-equal?") 2
  elseif (name = "%register-wndproc") 1
  elseif (name = "%register-wndenumproc") 1
  elseif (name = "%struct-get-i32") 2
  elseif (name = "%struct-set-i32") 3
  elseif (name = "%struct-get-i64") 2
  elseif (name = "%struct-set-i64") 3
  elseif (name = "%struct-get-u16") 2
  elseif (name = "%struct-set-u16") 3
  elseif (name = "%struct-get-u32") 2
  elseif (name = "%struct-set-u32") 3
  elseif (name = "%struct-get-u64") 2
  elseif (name = "%struct-set-u64") 3
  elseif (name = "%struct-get-pointer") 2
  elseif (name = "%struct-set-pointer") 3
  elseif (name = "%com-release") 1
  elseif (name = "%com-registry-len") 0
  elseif (name = "%com-last-hresult") 0
  elseif (name = "%com-clear-last-hresult") 0
  elseif (name = "%dxgi-create-factory") 0
  elseif (name = "%dxgi-device-from-d3d-device") 1
  elseif (name = "%dxgi-create-surface-from-texture") 1
  elseif (name = "%d3d11-create-device") 0
  elseif (name = "%d3d11-get-immediate-context") 1
  elseif (name = "%d3d11-create-texture-2d") 4
  elseif (name = "%d3d11-copy-to-staging-and-map") 5
  elseif (name = "%d3d11-last-staging-handle") 0
  elseif (name = "%d3d11-last-mapped-row-pitch") 0
  elseif (name = "%d3d11-unmap") 2
  elseif (name = "%d2d-create-factory") 0
  elseif (name = "%d2d-create-device") 2
  elseif (name = "%d2d-create-device-context") 1
  elseif (name = "%d2d-create-bitmap-for-target") 2
  elseif (name = "%d2d-set-target") 2
  elseif (name = "%d2d-begin-draw") 1
  elseif (name = "%d2d-end-draw") 1
  elseif (name = "%d2d-clear") 5
  elseif (name = "%d2d-set-transform-identity") 1
  elseif (name = "%d2d-create-solid-color-brush") 5
  elseif (name = "%d2d-draw-text-layout") 5
  elseif (name = "%d2d-draw-rectangle") 7
  elseif (name = "%d2d-fill-rectangle") 6
  elseif (name = "%dwrite-create-factory") 0
  elseif (name = "%dwrite-create-text-format") 4
  elseif (name = "%dwrite-create-text-layout") 5
  elseif (name = "%dwrite-get-layout-metrics") 1
  elseif (name = "%dwrite-hit-test-position") 3
  elseif (name = "%dwrite-hit-test-point") 3
  elseif (name = "%dwrite-set-drawing-effect") 4
  elseif (name = "%dwrite-set-line-spacing") 3
  elseif (name = "%count-non-zero-red") 4
  elseif (name = "%dxgi-factory-from-d3d-device") 1
  elseif (name = "%dxgi-create-swap-chain-for-hwnd") 5
  elseif (name = "%d2d-create-bitmap-from-swap-chain") 2
  elseif (name = "%dxgi-swap-chain-present") 1
  elseif (name = "%dxgi-swap-chain-resize-buffers") 3
  elseif (name = "%register-window-class") 2
  elseif (name = "%create-message-only-window") 1
  elseif (name = "%create-hidden-window") 1
  elseif (name = "%destroy-window") 1
  elseif (name = "%post-message") 4
  elseif (name = "%pump-one-message") 1
  elseif (name = "%run-message-loop") 0
  elseif (name = "%def-window-proc") 4
  elseif (name = "%read-file") 1
  elseif (name = "%argv1") 0
  elseif (name = "%argv2") 0
  elseif (name = "%print-gc-stats") 0
  elseif (name = "%lo-word") 1
  elseif (name = "%hi-word") 1
  elseif (name = "%set-scroll-info") 7
  elseif (name = "%get-scroll-pos") 2
  elseif (name = "%show-open-file-dialog") 1
  elseif (name = "%write-file") 2
  elseif (name = "%show-save-file-dialog") 1
  else -1
  end
end function;

define function prim-result-label (name :: <byte-string>) => (label :: <byte-string>)
  if (name = "%error") "<top>"
  elseif (name = "%values-clear") "<top>"
  elseif (name = "%values-set!") "<top>"
  elseif (name = "%values-get") "<top>"
  elseif (name = "%values-count") "<integer>"
  elseif (name = "%collection-size") "<integer>"
  elseif (name = "%collection-concatenate") "<top>"
  elseif (name = "%range-from") "<integer>"
  elseif (name = "%range-to") "<integer>"
  elseif (name = "%range-by") "<integer>"
  elseif (name = "%vector-size") "<integer>"
  elseif (name = "%vector-element") "<top>"
  elseif (name = "%vector-element-setter") "<top>"
  elseif (name = "%stretchy-vector-size") "<integer>"
  elseif (name = "%stretchy-vector-element") "<top>"
  elseif (name = "%stretchy-vector-push") "<top>"
  elseif (name = "%fip-init") "<top>"
  elseif (name = "%fip-finished?") "<boolean>"
  elseif (name = "%fip-current-element") "<top>"
  elseif (name = "%fip-advance!") "<top>"
  elseif (name = "%make-range") "<top>"
  elseif (name = "%make-stretchy-vector") "<top>"
  elseif (name = "%funcall0") "<top>"
  elseif (name = "%funcall1") "<top>"
  elseif (name = "%funcall2") "<top>"
  elseif (name = "%funcall3") "<top>"
  elseif (name = "%funcall4") "<top>"
  elseif (name = "%funcall5") "<top>"
  elseif (name = "%apply") "<top>"
  elseif (name = "%make-sov") "<top>"
  elseif (name = "%make-cell") "<top>"
  elseif (name = "%cell-get") "<top>"
  elseif (name = "%cell-set!") "<top>"
  elseif (name = "%env-cell") "<top>"
  elseif (name = "%make-environment") "<top>"
  elseif (name = "%make-closure") "<top>"
  elseif (name = "%byte-string-allocate") "<top>"
  elseif (name = "%byte-string-size") "<integer>"
  elseif (name = "%byte-string-element") "<integer>"
  elseif (name = "%byte-string-element-setter") "<integer>"
  elseif (name = "%byte-string-copy!") "<integer>"
  elseif (name = "%is-generic?") "<boolean>"
  elseif (name = "%is-class?") "<boolean>"
  elseif (name = "%make-table") "<top>"
  elseif (name = "%table-size") "<integer>"
  elseif (name = "%table-element") "<top>"
  elseif (name = "%table-element-or-default") "<top>"
  elseif (name = "%table-element-setter") "<top>"
  elseif (name = "%table-remove-key") "<top>"
  elseif (name = "%table-keys") "<top>"
  elseif (name = "%table-values") "<top>"
  elseif (name = "%object-hash") "<integer>"
  elseif (name = "%object-equal?") "<boolean>"
  elseif (name = "%register-wndproc") "<top>"
  elseif (name = "%register-wndenumproc") "<top>"
  elseif (name = "%struct-get-i32") "<integer>"
  elseif (name = "%struct-set-i32") "<integer>"
  elseif (name = "%struct-get-i64") "<integer>"
  elseif (name = "%struct-set-i64") "<integer>"
  elseif (name = "%struct-get-u16") "<integer>"
  elseif (name = "%struct-set-u16") "<integer>"
  elseif (name = "%struct-get-u32") "<integer>"
  elseif (name = "%struct-set-u32") "<integer>"
  elseif (name = "%struct-get-u64") "<integer>"
  elseif (name = "%struct-set-u64") "<integer>"
  elseif (name = "%struct-get-pointer") "<integer>"
  elseif (name = "%struct-set-pointer") "<integer>"
  elseif (name = "%com-release") "<integer>"
  elseif (name = "%com-registry-len") "<integer>"
  elseif (name = "%com-last-hresult") "<integer>"
  elseif (name = "%com-clear-last-hresult") "<integer>"
  elseif (name = "%dxgi-create-factory") "<integer>"
  elseif (name = "%dxgi-device-from-d3d-device") "<integer>"
  elseif (name = "%dxgi-create-surface-from-texture") "<integer>"
  elseif (name = "%d3d11-create-device") "<integer>"
  elseif (name = "%d3d11-get-immediate-context") "<integer>"
  elseif (name = "%d3d11-create-texture-2d") "<integer>"
  elseif (name = "%d3d11-copy-to-staging-and-map") "<integer>"
  elseif (name = "%d3d11-last-staging-handle") "<integer>"
  elseif (name = "%d3d11-last-mapped-row-pitch") "<integer>"
  elseif (name = "%d3d11-unmap") "<integer>"
  elseif (name = "%d2d-create-factory") "<integer>"
  elseif (name = "%d2d-create-device") "<integer>"
  elseif (name = "%d2d-create-device-context") "<integer>"
  elseif (name = "%d2d-create-bitmap-for-target") "<integer>"
  elseif (name = "%d2d-set-target") "<integer>"
  elseif (name = "%d2d-begin-draw") "<integer>"
  elseif (name = "%d2d-end-draw") "<integer>"
  elseif (name = "%d2d-clear") "<integer>"
  elseif (name = "%d2d-set-transform-identity") "<integer>"
  elseif (name = "%d2d-create-solid-color-brush") "<integer>"
  elseif (name = "%d2d-draw-text-layout") "<integer>"
  elseif (name = "%d2d-draw-rectangle") "<integer>"
  elseif (name = "%d2d-fill-rectangle") "<integer>"
  elseif (name = "%dwrite-create-factory") "<integer>"
  elseif (name = "%dwrite-create-text-format") "<integer>"
  elseif (name = "%dwrite-create-text-layout") "<integer>"
  elseif (name = "%dwrite-get-layout-metrics") "<integer>"
  elseif (name = "%dwrite-hit-test-position") "<integer>"
  elseif (name = "%dwrite-hit-test-point") "<integer>"
  elseif (name = "%dwrite-set-drawing-effect") "<integer>"
  elseif (name = "%dwrite-set-line-spacing") "<integer>"
  elseif (name = "%count-non-zero-red") "<integer>"
  elseif (name = "%dxgi-factory-from-d3d-device") "<integer>"
  elseif (name = "%dxgi-create-swap-chain-for-hwnd") "<integer>"
  elseif (name = "%d2d-create-bitmap-from-swap-chain") "<integer>"
  elseif (name = "%dxgi-swap-chain-present") "<integer>"
  elseif (name = "%dxgi-swap-chain-resize-buffers") "<integer>"
  elseif (name = "%register-window-class") "<integer>"
  elseif (name = "%create-message-only-window") "<integer>"
  elseif (name = "%create-hidden-window") "<integer>"
  elseif (name = "%destroy-window") "<integer>"
  elseif (name = "%post-message") "<integer>"
  elseif (name = "%pump-one-message") "<integer>"
  elseif (name = "%run-message-loop") "<integer>"
  elseif (name = "%def-window-proc") "<integer>"
  elseif (name = "%read-file") "<top>"
  elseif (name = "%argv1") "<top>"
  elseif (name = "%argv2") "<top>"
  elseif (name = "%print-gc-stats") "<top>"
  elseif (name = "%lo-word") "<integer>"
  elseif (name = "%hi-word") "<integer>"
  elseif (name = "%set-scroll-info") "<integer>"
  elseif (name = "%get-scroll-pos") "<integer>"
  elseif (name = "%show-open-file-dialog") "<top>"
  elseif (name = "%write-file") "<integer>"
  elseif (name = "%show-save-file-dialog") "<top>"
  else "<top>"
  end
end function;

// SlotTypeKind label for the `[..]` annotation (lower.rs slot_type_to_dfm_kind:
// Integer|Character -> Integer, else Object).
define function slot-kind-label (type-name :: <byte-string>) => (k :: <byte-string>)
  if (type-name = "<integer>" | type-name = "<character>") "Integer" else "Object" end
end function;

// Getter return-type label (lower.rs slot_type_to_estimate: Integer->Integer,
// DoubleFloat->DoubleFloat, Boolean->Boolean, Character->Character,
// String->String, else Top).
define function slot-return-label (type-name :: <byte-string>)
 => (label :: <byte-string>)
  if (type-name = "<integer>")          "<integer>"
  elseif (type-name = "<boolean>")      "<boolean>"
  elseif (type-name = "<character>")    "<character>"
  elseif (type-name = "<byte-string>")  "<string>"
  elseif (type-name = "<string>")       "<string>"
  elseif (type-name = "<double-float>") "<double-float>"
  else                                  "<top>"
  end
end function;

// A slot's declared type name ("<integer>" etc.), or "" when untyped (-> Top).
define function slot-type-name (s :: <ast-slot-spec>, source :: <byte-string>)
 => (tn :: <byte-string>)
  if (instance?(slot-type(s), <ast-node>))
    type-node-name(slot-type(s), source)
  else
    ""
  end
end function;

// Count the OWN slots of a class definition (slots with a real name token).
define function class-own-slot-count (cd :: <ast-class-definition>) => (n :: <integer>)
  let slots = class-slots(cd);
  let ns = size(slots);
  let i = 0;
  let count = 0;
  until (i >= ns)
    if (instance?(slot-name-tok(slots[i]), <token>)) count := count + 1; end;
    i := i + 1;
  end;
  count
end function;

// Find the module's `define class` whose name is `name`, or #f if none (i.e.
// the name is a builtin / out-of-module class).
define function module-class-by-name (items :: <stretchy-vector>, name :: <byte-string>,
                                      source :: <byte-string>) => (cd :: <object>)
  let n = size(items);
  let i = 0;
  let found = #f;
  until (i >= n | found)
    let item = items[i];
    if (instance?(item, <ast-class-definition>)
          & instance?(class-name(item), <token>)
          & token-source-text(class-name(item), source) = name)
      found := item;
    end;
    i := i + 1;
  end;
  found
end function;

// Total inherited slot count for a class: walk its SINGLE-inheritance super
// chain through the module's user classes, summing each ancestor's own slots.
// Returns the count, or #f if the layout can't be determined safely here:
//   * multiple supers (MI — slot merge order is most-specific-first; not
//     reimplemented), or
//   * a super that is neither `<object>` nor a module user class (a
//     slot-bearing builtin would shift offsets we can't see).
// `<object>` terminates the chain with 0 inherited slots.
define function class-inherited-slot-count (cd :: <ast-class-definition>,
                                            items :: <stretchy-vector>,
                                            source :: <byte-string>)
 => (count :: <object>)
  let supers = class-supers(cd);
  if (size(supers) ~= 1)
    #f                                  // MI (or no supers) — out of scope
  else
    let sname = super-name(supers[0], source);
    if (sname = "<object>")
      0
    else
      let scd = module-class-by-name(items, sname, source);
      if (~ scd)
        #f                              // super is a builtin/unknown class
      else
        let rest = class-inherited-slot-count(scd, items, source);
        if (~ rest) #f else rest + class-own-slot-count(scd) end
      end
    end
  end
end function;

// A class is handleable here iff (a) its super chain contributes ZERO inherited
// slots — its sole super is `<object>`, or a chain of module user classes that
// all have no slots — so own slots still start at @8 (we don't reimplement the
// runtime's most-specific-first inherited-slot layout) AND (b) it has no
// `constant` slot. Rust lowering supports only `instance:` allocation and ERRORS
// on a `Constant` slot (lower.rs), so a constant slot would make the Rust oracle
// fail while we'd emit a getter — bail to keep the two sides aligned.
define function class-is-simple? (cd :: <ast-class-definition>,
                                  items :: <stretchy-vector>,
                                  source :: <byte-string>)
 => (yes? :: <boolean>)
  let inherited = class-inherited-slot-count(cd, items, source);
  if (~ inherited | inherited ~= 0)
    #f                                  // undeterminable layout or shifted offsets
  else
    let slots = class-slots(cd);
    let ns = size(slots);
    let i = 0;
    let ok? = #t;
    until (i >= ns | ~ ok?)
      let s = slots[i];
      // slot-has-setter? is #f exactly for `constant` slots (the only
      // unsupported allocation the parsed AST surfaces).
      if (instance?(slot-name-tok(s), <token>) & ~ slot-has-setter?(s, source))
        ok? := #f;
      end;
      i := i + 1;
    end;
    ok?
  end
end function;

// The set of generic names in the module: every class slot's getter (its name)
// plus, when the slot has a setter, `<slot>-setter`, AND every `define generic`
// name and `define method` name (mirrors Rust `collect_generic_names`, lower.rs
// ~4733-4742). A call to one of these is a Dispatch, not a DirectCall (lower.rs
// 6108). The generic/method names are LOAD-BEARING here: when this lowering
// runs, the host hasn't registered the module's own generics yet, so
// `%is-generic?` returns #f for `run-task` etc.; without seeding them a call to
// a same-module method body would wrongly emit a DirectCall.
define function build-generic-names (items :: <stretchy-vector>, source :: <byte-string>)
 => (names :: <stretchy-vector>)
  let names = make(<stretchy-vector>);
  let n = size(items);
  let i = 0;
  until (i >= n)
    let item = items[i];
    if (instance?(item, <ast-class-definition>))
      let slots = class-slots(item);
      let ns = size(slots);
      let si = 0;
      until (si >= ns)
        let s = slots[si];
        if (instance?(slot-name-tok(s), <token>))
          let sn = token-source-text(slot-name-tok(s), source);
          add!(names, sn);
          if (slot-has-setter?(s, source))
            add!(names, concatenate(sn, "-setter"));
          end;
        end;
        si := si + 1;
      end;
    elseif (instance?(item, <ast-generic-definition>))
      // `define generic g …` — `g` is a generic.
      if (instance?(gen-name(item), <token>))
        set-add!(names, token-source-text(gen-name(item), source));
      end;
    elseif (instance?(item, <ast-body-definition>))
      // `define method g …` — `g` is a generic (its body is one method).
      if (token-source-text(defn-word(item), source) = "method"
            & instance?(defn-method-name(item), <token>))
        set-add!(names, token-source-text(defn-method-name(item), source));
      end;
    end;
    i := i + 1;
  end;
  names
end function;

// build_slot_getter (lower.rs 3371): `fn <C>-getter-<slot> (t0: <top>)
// -> <ret>: entry: t1 = LoadSlot t0 @<off> [<kind>]; Return t1`.
define function make-getter-fn (class-name :: <byte-string>, slot-name :: <byte-string>,
                                offset :: <integer>, slot-kind :: <byte-string>,
                                ret-label :: <byte-string>) => (f :: <dfm-func>)
  let b = make-fn-builder(concatenate(class-name, concatenate("-getter-", slot-name)));
  let t0 = fb-fresh-temp(b, "<top>");            // self
  add!(func-params(fb-func(b)), t0);
  let t1 = fb-fresh-temp(b, ret-label);          // loaded value
  fb-push(b, make(<dfm-comp>, kind: "loadslot", dst: t1, cval: offset,
                  op: slot-kind, args: singleton-vec(t0), callee: #f));
  func-return-type(fb-func(b)) := ret-label;
  fb-terminate-return(b, t1);
  fb-func(b)
end function;

// build_slot_setter (lower.rs 3420): `fn <C>-setter-<slot> (t0: <top>, t1: <top>)
// -> <top>: entry: t2 = StoreSlot t0 @<off> := t1 [<kind>]; Return t2`.
define function make-setter-fn (class-name :: <byte-string>, slot-name :: <byte-string>,
                                offset :: <integer>, slot-kind :: <byte-string>)
 => (f :: <dfm-func>)
  let b = make-fn-builder(concatenate(class-name, concatenate("-setter-", slot-name)));
  let t0 = fb-fresh-temp(b, "<top>");   // self
  add!(func-params(fb-func(b)), t0);
  let t1 = fb-fresh-temp(b, "<top>");   // value
  add!(func-params(fb-func(b)), t1);
  let t2 = fb-fresh-temp(b, "<top>");   // store result
  fb-push(b, make(<dfm-comp>, kind: "storeslot", dst: t2, cval: offset,
                  op: slot-kind, args: pair-args(t0, t1), callee: #f));
  func-return-type(fb-func(b)) := "<top>";
  fb-terminate-return(b, t2);
  fb-func(b)
end function;

// Append the getter/setter accessor functions for one class (own slots, source
// order, getter-then-setter). Mirrors Phase 3 ordering. Also records the
// matching method registrations into `methods` (Sprint 56c-T), in the SAME walk
// order as Rust's accessor pass (lower.rs ~1779/1795): per own slot, a getter
// `MethodRegistration{ generic=<slot>, specialisers=[<C>], body=<C>-getter-<slot>,
// param_count=1 }`, then (iff the slot has a setter) a setter `{ generic=
// <slot>-setter, specialisers=[<C>, <object>], body=<C>-setter-<slot>,
// param_count=2 }`.
define function emit-class-accessors (cd :: <ast-class-definition>,
                                      source :: <byte-string>,
                                      funcs :: <stretchy-vector>,
                                      methods :: <stretchy-vector>) => ()
  let cname = token-source-text(class-name(cd), source);
  let slots = class-slots(cd);
  let nsl = size(slots);
  let sli = 0;
  let idx = 0;                          // own-slot index -> offset 8 + idx*8
  until (sli >= nsl)
    let s = slots[sli];
    if (instance?(slot-name-tok(s), <token>))
      let sn = token-source-text(slot-name-tok(s), source);
      let tn = slot-type-name(s, source);
      let offset = 8 + idx * 8;
      let kind = slot-kind-label(tn);
      add!(funcs, make-getter-fn(cname, sn, offset, kind, slot-return-label(tn)));
      // Getter registration: generic = slot name, sole specialiser = <C>.
      let g-specs = make(<stretchy-vector>);
      add!(g-specs, cname);
      add!(methods, make-method-reg(sn, concatenate(cname, concatenate("-getter-", sn)),
                                    1, g-specs));
      if (slot-has-setter?(s, source))
        add!(funcs, make-setter-fn(cname, sn, offset, kind));
        // Setter registration: generic = <slot>-setter, specialisers = [<C>, <object>].
        let s-specs = make(<stretchy-vector>);
        add!(s-specs, cname);
        add!(s-specs, "<object>");
        add!(methods, make-method-reg(concatenate(sn, "-setter"),
                                      concatenate(cname, concatenate("-setter-", sn)),
                                      2, s-specs));
      end;
      idx := idx + 1;
    end;
    sli := sli + 1;
  end;
end function;

// Names of all user `define class`es in the module (so a param/return/slot of a
// user-class type can be typed `<class>` rather than `<top>`).
define function build-user-class-names (items :: <stretchy-vector>, source :: <byte-string>)
 => (names :: <stretchy-vector>)
  let names = make(<stretchy-vector>);
  let n = size(items);
  let i = 0;
  until (i >= n)
    let item = items[i];
    if (instance?(item, <ast-class-definition>))
      let nt = class-name(item);
      if (instance?(nt, <token>))
        add!(names, token-source-text(nt, source));
      end;
    end;
    i := i + 1;
  end;
  names
end function;

// Map a declared type name to its DFM label. A class — user (from the AST set,
// since user classes aren't registered yet when this runs) or a registered
// builtin (`<stretchy-vector>`, … via `%is-class?`) — is `TypeEstimate::Class`.
//
// B-i: params/returns/block-params now dump `<class:N>` (id present) via
// `type_label`, so the lowering must emit class types BY NAME — it can't know
// the host-assigned ids at lowering time. We emit `<class:<NAME>>` (e.g.
// `<class:<idler>>`); `parse_type` resolves the inner class name through the
// live registry at the `--lower-with-dylan` seam, yielding `Class(id)` which
// reformats to the numeric `<class:N>` = byte-identical to the Rust dump. This
// is the load-bearing flip that lets SEALED dispatch on a class-typed param
// resolve identically on both sides (the crux that kept `richards-shape`
// bailing).
//
// The universal `<object>` is `Top` (-> `<top>`). Scalars
// (`<integer>`/`<boolean>`/…) keep their estimate; anything else genuinely
// unknown is `<top>`.
define function label-for-type-name (type-name :: <byte-string>,
                                     user-classes :: <stretchy-vector>)
 => (label :: <byte-string>)
  if (name-in-vec?(user-classes, type-name))
    concatenate("<class:", concatenate(type-name, ">"))   // user class, BY NAME
  else
    let scalar = type-name-to-label(type-name);
    if (scalar ~= "<top>")
      scalar                              // known scalar (<integer>, <string>, …)
    elseif (type-name = "<object>")
      "<top>"                            // the universal class -> Top
    elseif (%is-class?(type-name))
      concatenate("<class:", concatenate(type-name, ">"))  // registered (builtin) class, BY NAME
    else
      "<top>"                            // genuinely unknown type
    end
  end
end function;

// Declared return label of a constant's binder: `*x* :: <integer> = …` gives
// the typed-name's type label; a bare `*x* = …` gives #f (use the init type).
define function constant-declared-label (lhs :: <object>, user-classes :: <stretchy-vector>,
                                         source :: <byte-string>)
 => (label :: <object>)
  if (instance?(lhs, <ast-typed-name>))
    let ty = typed-name-type(lhs);
    if (ty & instance?(ty, <ast-variable-ref>))
      label-for-type-name(token-source-text(varref-tok(ty), source), user-classes)
    else
      #f
    end
  else
    #f
  end
end function;

// `define constant NAME [:: <type>] = INIT` lowers to a 0-arg initializer
// function `fn NAME () -> <ret>: <init>; Return t` (one thunk per constant, in
// source order with the user functions). Single binder only; multi-binder or an
// unsupported init returns #f (whole module bails).
define function lower-constant-defn (ld :: <ast-list-definition>,
                                     ret-map :: <name-ret-map>,
                                     gnames :: <stretchy-vector>,
                                     user-classes :: <stretchy-vector>,
                                     source :: <byte-string>) => (func :: <object>)
  let cs = body-constituents(defn-list(ld));
  if (size(cs) ~= 1)
    #f                                  // `define constant a = 1, b = 2` — later
  else
    let node = cs[0];
    if (~ instance?(node, <ast-binary-op>))
      #f
    else
      let lhs = binop-left(node);
      let name =
        if (instance?(lhs, <ast-variable-ref>))
          token-source-text(varref-tok(lhs), source)
        elseif (instance?(lhs, <ast-typed-name>))
          token-source-text(typed-name-tok(lhs), source)
        else
          #f
        end;
      if (~ name)
        #f
      else
        let b = make-fn-builder(name);
        fb-generics(b) := gnames;
        let t = lower-expr(b, binop-right(node), ret-map, source);
        if (~ t)
          #f
        else
          let declared = constant-declared-label(lhs, user-classes, source);
          let ret-label = if (declared) declared else fb-temp-type(b, t) end;
          func-return-type(fb-func(b)) := ret-label;
          fb-terminate-return(b, t);
          fb-func(b)
        end
      end
    end
  end
end function;

// ─── lower-function / lower-method — mirror lower_function_inner ────────────
//
// Builds a <dfm-func> for one `define function` / `define method` whose body is
// a straight-line / 55a-control-flow statement sequence. Returns the
// <dfm-func>, or #f if outside scope. Order mirrored from lower.rs: params get
// fresh temps in declaration order (t0,t1,…) BEFORE the body; the body's last
// statement's value is the Return value; return_type = declared label if
// present, else the final temp's type. The ONLY difference between a function
// and a method is the function NAME (a method body is named `g$spec_spec…`,
// computed by the caller); the body lowering is identical, so both delegate to
// `lower-defn-body-into` with a builder whose name is already set.

// Lower `defn`'s params + body + return into the already-named builder `b`.
// `defn` is an <ast-body-definition> (function or method). Returns the
// <dfm-func>, or #f on any unsupported form.
define function lower-defn-body-into (b :: <fn-builder>,
                                      defn :: <ast-body-definition>,
                                      ret-map :: <name-ret-map>,
                                      user-classes :: <stretchy-vector>,
                                      source :: <byte-string>)
 => (func :: <object>)
  // (1) Parameters -> entry temps, declaration order.
  let params = defn-params(defn);
  if (params)
    let reqs = params-required(params);
    let np = size(reqs);
    let pi = 0;
    until (pi >= np)
      let tn = reqs[pi];
      let ty = typed-name-type(tn);
      let type-name =
        if (ty & instance?(ty, <ast-variable-ref>))
          token-source-text(varref-tok(ty), source)
        else
          ""
        end;
      let t = fb-fresh-temp(b, label-for-type-name(type-name, user-classes));
      add!(func-params(fb-func(b)), t);
      // Bind the param name so body var-refs resolve to its temp.
      fb-bind(b, token-source-text(typed-name-tok(tn), source), t);
      pi := pi + 1;
    end;
  end;
  // (2) Body — a sequence of straight-line statements (let bindings +
  // expressions). Each lowers in order; the LAST statement's value is the
  // return value (lower_function_inner's last_temp). Any unsupported
  // statement bails the whole function (-> Rust path).
  let body = defn-body(defn);
  let cs = body-constituents(body);
  let nc = size(cs);
  let ci = 0;
  let last-temp = #f;
  let ok? = #t;
  until (ci >= nc | ~ ok?)
    let t = lower-body-stmt(b, cs[ci], ret-map, source);
    // #f → bail; <integer> temp → the running return value; void marker (a
    // loop) → lowered for effect, does NOT become the return value (so
    // `until(...)...end; result` returns `result`, not the loop).
    if (~ t)
      ok? := #f;
    elseif (instance?(t, <integer>))
      last-temp := t;
    else
      // Void statement (a loop's void marker): the function's value is THIS
      // last statement, so reset — a trailing loop makes the function void
      // even if an earlier `let` produced a temp. (Rust returns the value of
      // the LAST statement; `=> ()` is NOT what makes it void — a `=> ()`
      // function whose body is an expression still returns that expression's
      // value, e.g. `hello`'s `format-out(...)`.)
      last-temp := #f;
    end;
    ci := ci + 1;
  end;
  if (~ ok?)
    #f
  elseif (~ last-temp)
    // Void function: the last statement produced no value (a trailing loop).
    // Rust types these `<unit>` with a bare `Return` (Return{None}).
    func-return-type(fb-func(b)) := "<unit>";
    fb-terminate-return(b, #f);
    fb-func(b)
  else
    // (3) return_type: declared wins, else the final temp's type.
    let declared = defn-declared-return-label(defn, user-classes, source);
    let ret-label = if (declared) declared else fb-temp-type(b, last-temp) end;
    func-return-type(fb-func(b)) := ret-label;
    // (4) Return{value}.
    fb-terminate-return(b, last-temp);
    fb-func(b)
  end
end function;

define function lower-function (defn :: <ast-body-definition>,
                                ret-map :: <name-ret-map>,
                                gnames :: <stretchy-vector>,
                                user-classes :: <stretchy-vector>,
                                source :: <byte-string>)
 => (func :: <object>)
  let name-tok = defn-method-name(defn);
  if (~ name-tok)
    #f
  else
    let b = make-fn-builder(token-source-text(name-tok, source));
    fb-generics(b) := gnames;
    lower-defn-body-into(b, defn, ret-map, user-classes, source)
  end
end function;

// Build a method body's function name: `{generic}$` + the specialiser ids of
// the REQUIRED params joined by `_`, mirroring lower.rs `lower_method_item`
// (~3567): `body_fn_name = format!("{name}${suffix}")`, suffix = specialiser
// ids joined by "_". Each specialiser is emitted BY NAME (the class's source
// text, e.g. `<idler>`) — classes aren't registered when this runs, so
// `parse_function_header` resolves the `$<class>` suffix to the numeric id at
// the reconstruction seam. An UNANNOTATED required param contributes `<object>`
// (ClassId::OBJECT == 0). Returns #f (bail) if a required specialiser is not a
// bare class-name type ref (a singleton/union/expression specialiser) — we
// never emit a dubious header.
define function method-body-name (generic :: <byte-string>,
                                  params :: <object>,
                                  source :: <byte-string>)
 => (name :: <object>)
  if (~ params)
    #f                                  // a method needs >= 1 required param
  else
    let reqs = params-required(params);
    let np = size(reqs);
    if (np = 0)
      #f
    else
      let suffix = "";
      let pi = 0;
      let ok? = #t;
      until (pi >= np | ~ ok?)
        let tn = reqs[pi];
        let ty = typed-name-type(tn);
        let spec =
          if (~ ty)
            "<object>"                  // unannotated required param
          elseif (instance?(ty, <ast-variable-ref>))
            token-source-text(varref-tok(ty), source)   // bare class name
          else
            #f                          // singleton / union / expr — bail
          end;
        if (~ spec)
          ok? := #f;
        else
          suffix := if (pi = 0) spec else concatenate(suffix, concatenate("_", spec)) end;
        end;
        pi := pi + 1;
      end;
      if (~ ok?) #f else concatenate(generic, concatenate("$", suffix)) end
    end
  end
end function;

// Lower a `define method` to its method-body function (named `g$spec_spec…`).
// The body lowering is identical to a function's (lower-defn-body-into); only
// the name differs. Returns the <dfm-func>, or #f (bail) if the method name is
// missing or a specialiser isn't a bare class name (method-body-name -> #f).
define function lower-method (defn :: <ast-body-definition>,
                              ret-map :: <name-ret-map>,
                              gnames :: <stretchy-vector>,
                              user-classes :: <stretchy-vector>,
                              source :: <byte-string>)
 => (func :: <object>)
  let name-tok = defn-method-name(defn);
  if (~ instance?(name-tok, <token>))
    #f
  else
    let generic = token-source-text(name-tok, source);
    let fname = method-body-name(generic, defn-params(defn), source);
    if (~ fname)
      #f
    else
      let b = make-fn-builder(fname);
      fb-generics(b) := gnames;
      lower-defn-body-into(b, defn, ret-map, user-classes, source)
    end
  end
end function;

// ─── format-dfm — mirrors nod-dfm/src/format.rs EXACTLY ────────────────────

// Render one byte as a 1-char <byte-string>.
define function byte-to-string-1 (c :: <integer>) => (s :: <byte-string>)
  let s = %byte-string-allocate(1);
  %byte-string-element-setter(c, s, 0);
  s
end function;

// Lowercase hex of a byte value (no leading zero), for `\u{..}` escapes.
define function byte-hex (c :: <integer>) => (s :: <byte-string>)
  let digits = "0123456789abcdef";
  let hi = c - (c / 16) * 16;        // low nibble
  let lo-s = byte-to-string-1(%byte-string-element(digits, hi));
  let high = c / 16;
  if (high = 0)
    lo-s
  else
    concatenate(byte-to-string-1(%byte-string-element(digits, high)), lo-s)
  end
end function;

// Escape a string the way Rust's `{:?}` (str Debug / escape_debug) does, so
// `String(<...>)` in the DFM dump matches `format.rs` byte-for-byte: `"` and
// `\` are backslash-escaped, `\n` / `\t` / `\r` use their letter escapes,
// printable ASCII passes through, and any other byte becomes `\u{<hex>}`.
define function escape-string-debug (s :: <byte-string>) => (out :: <byte-string>)
  let out = "";
  let n = size(s);
  let i = 0;
  until (i >= n)
    let c = %byte-string-element(s, i);
    let piece =
      if (c = 34)                   "\\\""        // "
      elseif (c = 92)               "\\\\"        // backslash
      elseif (c = 10)               "\\n"
      elseif (c = 9)                "\\t"
      elseif (c = 13)               "\\r"
      elseif (c >= 32 & c <= 126)   byte-to-string-1(c)
      else                          concatenate("\\u{", concatenate(byte-hex(c), "}"))
      end;
    out := concatenate(out, piece);
    i := i + 1;
  end;
  out
end function;

// fmt_computation (format.rs), Phase-0 kinds. 4-space indent, newline-end.
define function fmt-computation (c :: <dfm-comp>, temps :: <stretchy-vector>)
 => (s :: <byte-string>)
  let kind = comp-kind(c);
  let dst-ty = temp-type-of(temps, comp-dst(c));
  let head = concatenate("    t",
               concatenate(integer-to-string(comp-dst(c)),
                 concatenate(": ", dst-ty)));
  if (kind = "const")
    concatenate(head, concatenate(" = Const ", concatenate(comp-cval(c), "\n")))
  elseif (kind = "primop")
    let line = concatenate(head, concatenate(" = PrimOp ", comp-op(c)));
    let args = comp-args(c);
    let n = size(args);
    let i = 0;
    until (i >= n)
      line := concatenate(line, concatenate(" t", integer-to-string(args[i])));
      i := i + 1;
    end;
    concatenate(line, "\n")
  elseif (kind = "loadslot")
    // `= LoadSlot t<inst> @<offset> [<kind>]` (format.rs 166-176). offset is in
    // comp-cval, the SlotTypeKind label ("Integer"/"Object") in comp-op, the
    // instance temp in args[0].
    let inst = comp-args(c)[0];
    concatenate(head,
      concatenate(" = LoadSlot t", concatenate(integer-to-string(inst),
        concatenate(" @", concatenate(integer-to-string(comp-cval(c)),
          concatenate(" [", concatenate(comp-op(c), "]\n")))))))
  elseif (kind = "storeslot")
    // `= StoreSlot t<inst> @<offset> := t<value> [<kind>]` (format.rs 177-188).
    // args[0] = instance, args[1] = value.
    let inst = comp-args(c)[0];
    let val  = comp-args(c)[1];
    concatenate(head,
      concatenate(" = StoreSlot t", concatenate(integer-to-string(inst),
        concatenate(" @", concatenate(integer-to-string(comp-cval(c)),
          concatenate(" := t", concatenate(integer-to-string(val),
            concatenate(" [", concatenate(comp-op(c), "]\n")))))))))
  elseif (kind = "typecheck")
    // `= TypeCheck t<value> <class-label>` (format.rs 146-155). The class label
    // (ClassCheck::name()) is in comp-cval; the value temp in args[0].
    concatenate(head,
      concatenate(" = TypeCheck t", concatenate(integer-to-string(comp-args(c)[0]),
        concatenate(" ", concatenate(comp-cval(c), "\n")))))
  elseif (kind = "dispatch")
    // `= Dispatch generic(t0, t1)` (format.rs 189-206). Lowering always emits an
    // EMPTY safepoint set (the host liveness pass populates it), and the dst is
    // always <top> here, so `head` (which uses the dst's label) is correct.
    let line = concatenate(head,
                 concatenate(" = Dispatch ", concatenate(comp-callee(c), "(")));
    let args = comp-args(c);
    let n = size(args);
    let i = 0;
    until (i >= n)
      if (i > 0) line := concatenate(line, ", "); end;
      line := concatenate(line, concatenate("t", integer-to-string(args[i])));
      i := i + 1;
    end;
    concatenate(line, ")\n")
  else
    // directcall: ` = DirectCall callee(t0, t1)`; empty safepoint + not
    // no_alloc -> nothing appended.
    let line = concatenate(head,
                 concatenate(" = DirectCall ", concatenate(comp-callee(c), "(")));
    let args = comp-args(c);
    let n = size(args);
    let i = 0;
    until (i >= n)
      if (i > 0) line := concatenate(line, ", "); end;
      line := concatenate(line, concatenate("t", integer-to-string(args[i])));
      i := i + 1;
    end;
    concatenate(line, ")\n")
  end
end function;

// fmt_terminator (format.rs): Return / If / Jump.
define function fmt-terminator (blk :: <dfm-block>) => (s :: <byte-string>)
  let tm = block-term(blk);
  let kind = term-kind(tm);
  if (kind = "return")
    let v = term-value(tm);
    if (v)
      concatenate("    Return t", concatenate(integer-to-string(v), "\n"))
    else
      "    Return\n"
    end
  elseif (kind = "if")
    // `    If t<cond> <then-label> <else-label>`
    concatenate("    If t",
      concatenate(integer-to-string(term-value(tm)),
        concatenate(" ", concatenate(term-a(tm),
          concatenate(" ", concatenate(term-b(tm), "\n"))))))
  else
    // `    Jump <target>(t.., t..)`
    let line = concatenate("    Jump ", concatenate(term-a(tm), "("));
    let args = term-args(tm);
    let m = size(args);
    let j = 0;
    until (j >= m)
      if (j > 0) line := concatenate(line, ", "); end;
      line := concatenate(line, concatenate("t", integer-to-string(args[j])));
      j := j + 1;
    end;
    concatenate(line, ")\n")
  end
end function;

// fmt_function (format.rs).
define function fmt-function (f :: <dfm-func>) => (s :: <byte-string>)
  let temps  = func-temps(f);
  // Header: `fn <name> (t0: <type>, …) -> <ret>:`
  let out = concatenate("fn ", concatenate(func-name(f), " ("));
  let params = func-params(f);
  let np = size(params);
  let pi = 0;
  until (pi >= np)
    if (pi > 0) out := concatenate(out, ", "); end;
    let pid = params[pi];
    out := concatenate(out,
             concatenate("t", concatenate(integer-to-string(pid),
               concatenate(": ", temp-type-of(temps, pid)))));
    pi := pi + 1;
  end;
  out := concatenate(out,
           concatenate(") -> ", concatenate(func-return-type(f), ":\n")));
  // Blocks.
  let blocks = func-blocks(f);
  let nb = size(blocks);
  let bi = 0;
  until (bi >= nb)
    let blk = blocks[bi];
    out := concatenate(out, concatenate("  ", block-label(blk)));
    let bparams = block-params(blk);
    let nbp = size(bparams);
    if (nbp > 0)
      out := concatenate(out, "(");
      let bpi = 0;
      until (bpi >= nbp)
        if (bpi > 0) out := concatenate(out, ", "); end;
        let bpid = bparams[bpi];
        out := concatenate(out,
                 concatenate("t", concatenate(integer-to-string(bpid),
                   concatenate(": ", temp-type-of(temps, bpid)))));
        bpi := bpi + 1;
      end;
      out := concatenate(out, ")");
    end;
    out := concatenate(out, ":\n");
    let comps = block-comps(blk);
    let nc = size(comps);
    let ci = 0;
    until (ci >= nc)
      out := concatenate(out, fmt-computation(comps[ci], temps));
      ci := ci + 1;
    end;
    out := concatenate(out, fmt-terminator(blk));
    bi := bi + 1;
  end;
  out
end function;

// format_dfm_module (format.rs): functions joined by a '\n' separator (each
// function block already ends with '\n', so this yields a blank line between).
define function format-dfm-module (funcs :: <stretchy-vector>)
 => (s :: <byte-string>)
  let out = "";
  let n = size(funcs);
  let i = 0;
  until (i >= n)
    if (i > 0) out := concatenate(out, "\n"); end;
    out := concatenate(out, fmt-function(funcs[i]));
    i := i + 1;
  end;
  out
end function;

// ─── methods table (Sprint 56c-T) — shadow-emit, verified host-side ────────
//
// Mirrors the Rust `methods: Vec<MethodRegistration>` built in
// `lower_module_full_inner` (lower.rs) in WALK ORDER: pass-1 records slot
// accessor getter/setter registrations (per class, per own slot, getter then
// setter), pass-2 records user `define method` registrations. Each entry is a
// 4-element `<stretchy-vector>`: #[generic-name, body-fn-name, param-count,
// specialiser-names], where specialiser-names is itself a `<stretchy-vector>`
// of class-name strings. The host splits the dump at `\n=== methods ===\n`,
// parses these lines into `ParsedMethod`, and verifies them against the Rust
// `MethodRegistration` table (by generic-name / body-fn-name / param-count and
// specialisers BY NAME). It is NOT consumed — the dump-dfm output is unchanged.

// Build one method-registration record. `specialisers` is a <stretchy-vector>
// of class-name strings.
define function make-method-reg (generic :: <byte-string>, body :: <byte-string>,
                                 param-count :: <integer>,
                                 specialisers :: <stretchy-vector>)
 => (reg :: <stretchy-vector>)
  let v = make(<stretchy-vector>);
  add!(v, generic);
  add!(v, body);
  add!(v, param-count);
  add!(v, specialisers);
  v
end function;

// The specialiser class NAMES of a method's REQUIRED params, mirroring the
// names that `method-body-name` already derives for the body-fn suffix: an
// unannotated required param contributes `<object>`; a bare class-name type ref
// contributes the source text. (method-body-name has already returned #f for
// singleton/union/expr specialisers, so this only runs on accepted methods.)
define function method-specialiser-names (params :: <object>, source :: <byte-string>)
 => (names :: <stretchy-vector>)
  let names = make(<stretchy-vector>);
  if (params)
    let reqs = params-required(params);
    let np = size(reqs);
    let pi = 0;
    until (pi >= np)
      let tn = reqs[pi];
      let ty = typed-name-type(tn);
      let spec =
        if (~ ty)
          "<object>"
        elseif (instance?(ty, <ast-variable-ref>))
          token-source-text(varref-tok(ty), source)
        else
          "<object>"                       // unreachable: method already accepted
        end;
      add!(names, spec);
      pi := pi + 1;
    end;
  end;
  names
end function;

// Format one method line:
//   `method <generic> body=<body> params=<N> specialisers=[<a>, <b>, ...]`
// (same comma-space join as the classes section in format_sema_model).
define function fmt-method-reg (reg :: <stretchy-vector>) => (s :: <byte-string>)
  let generic = reg[0];
  let body    = reg[1];
  let pcount  = reg[2];
  let specs   = reg[3];
  let line = concatenate("method ", generic);
  line := concatenate(line, concatenate(" body=", body));
  line := concatenate(line, concatenate(" params=", integer-to-string(pcount)));
  line := concatenate(line, " specialisers=[");
  let ns = size(specs);
  let i = 0;
  until (i >= ns)
    if (i > 0) line := concatenate(line, ", "); end;
    line := concatenate(line, specs[i]);
    i := i + 1;
  end;
  concatenate(line, "]")
end function;

// Render the `=== methods ===` section (one line per method, '\n'-terminated).
define function format-methods-section (methods :: <stretchy-vector>)
 => (s :: <byte-string>)
  let out = "=== methods ===\n";
  let n = size(methods);
  let i = 0;
  until (i >= n)
    out := concatenate(out, concatenate(fmt-method-reg(methods[i]), "\n"));
    i := i + 1;
  end;
  out
end function;

// ─── Top-level entry — lex -> parse -> lower -> format ─────────────────────
//
// Returns the dump-dfm text, or "" if ANY top-level item is outside Phase-0
// scope (so the gate keeps that fixture on the Rust path — Phase 0 must never
// emit a WRONG dump).

define function dylan-lower-emit (source :: <byte-string>)
 => (dfm-text :: <byte-string>)
  let tokens = lex(source);
  let ast    = parse-dylan-with-precedence(tokens, precedence-c-header?(source));
  let items  = body-constituents(ast);
  let user-classes = build-user-class-names(items, source);
  let ret-map = build-name-ret-map(items, user-classes, source);
  let gnames  = build-generic-names(items, source);
  let funcs  = make(<stretchy-vector>);
  // Sprint 56c-T: shadow methods table, in Rust WALK ORDER (accessors pass-1,
  // then user methods pass-2). Verified host-side, not consumed.
  let methods = make(<stretchy-vector>);
  let n = size(items);
  let all-ok? = #t;
  // Pass 1 (Phase 3): slot accessors for every class, in source order. All
  // accessors precede all user functions in the dump, regardless of where the
  // classes appear in source. Only simple (sole-super-<object>) classes are
  // handled; anything else bails (inherited slots would shift offsets).
  let i = 0;
  until (i >= n | ~ all-ok?)
    let item = items[i];
    if (instance?(item, <ast-class-definition>))
      // B-i: sealed classes are now IN SCOPE. With class-typed params dumped as
      // `<class:N>` (lossless), SEALED dispatch on a class-typed param receiver
      // resolves identically on both sides of the flip, so we no longer bail on
      // the `sealed` modifier. Other shape limits still apply (simple class
      // only — `class-is-simple?` guards MI / slot-bearing supers / etc.).
      if (class-is-simple?(item, items, source))
        emit-class-accessors(item, source, funcs, methods);
      else
        all-ok? := #f;
      end;
    end;
    i := i + 1;
  end;
  // Pass 2 (Phase 4): user functions, source order. Any unsupported item bails.
  i := 0;
  until (i >= n | ~ all-ok?)
    let item = items[i];
    if (instance?(item, <ast-body-definition>))
      let word = token-source-text(defn-word(item), source);
      if (word = "function")
        let f = lower-function(item, ret-map, gnames, user-classes, source);
        if (f) add!(funcs, f); else all-ok? := #f; end;
      elseif (word = "method")
        // `define method g (…) … end` -> a method-body function named
        // `g$spec_spec…`. B-i: sealed methods are now IN SCOPE — class-typed
        // params dump as `<class:N>` (lossless), so SEALED dispatch on a param
        // receiver resolves identically on both sides of the flip.
        let f = lower-method(item, ret-map, gnames, user-classes, source);
        if (f)
          add!(funcs, f);
          // Sprint 56c-T: record the user-method registration in WALK ORDER
          // (mirrors Rust `methods.push(method.registration)`, lower.rs ~2190).
          // generic = method name; body = method-body-name; specialisers + count
          // from the REQUIRED params (method-body-name already vetted the shapes,
          // so all-ok? here implies a clean specialiser list).
          let gname = token-source-text(defn-method-name(item), source);
          let bname = method-body-name(gname, defn-params(item), source);
          let specs = method-specialiser-names(defn-params(item), source);
          add!(methods, make-method-reg(gname, bname, size(specs), specs));
        else
          all-ok? := #f;
        end;
      else
        all-ok? := #f;     // other body-definition words — later
      end;
    elseif (instance?(item, <ast-class-definition>))
      #f;                  // handled in pass 1
    elseif (instance?(item, <ast-list-definition>))
      // `define constant` emits a 0-arg initializer function; `define variable`
      // and anything else still bails.
      if (token-source-text(defn-word(item), source) = "constant")
        let f = lower-constant-defn(item, ret-map, gnames, user-classes, source);
        if (f) add!(funcs, f); else all-ok? := #f; end;
      else
        all-ok? := #f;
      end;
    elseif (instance?(item, <ast-generic-definition>))
      // `define generic g (…) => (…);` — the host registers the generic from the
      // AST; the lowering emits NO function for it (it has no body). A NO-OP, not
      // a bail. B-i: sealed generics are now IN SCOPE too — the `<class:N>`
      // param format makes SEALED dispatch resolve identically on both sides of
      // the flip, so we no longer bail on the `sealed` modifier.
      #f;                  // no-op: no function emitted for the generic
    else
      // Preamble (`Module:` / `Precedence:` lexed as ordinary forms) or a bare
      // top-level expression. The Dylan parser keeps the preamble as items
      // (the host translator strips it via scan_preamble); skip such items,
      // mirroring `collect-top-names`. No Phase-0 fixture has a bare top-level
      // expression, so skipping is safe here.
      #f;
    end;
    i := i + 1;
  end;
  // On success, append the shadow `=== methods ===` section after the function
  // dump, separated by a single '\n' so the host can SPLIT the dump at the
  // literal `\n=== methods ===\n` boundary: the left part is the unchanged DFM
  // funcs dump (fed to parse_dfm_module), the right part the methods table
  // (parsed + verified against the Rust MethodRegistration table, not printed).
  // A bail still returns "".
  if (all-ok?)
    concatenate(format-dfm-module(funcs),
                concatenate("\n", format-methods-section(methods)))
  else
    ""
  end
end function;
