Module: dylan-lexer
Precedence: c

// Sprint 52.2 — Dylan-side macro engine (production home).
//
// Promoted out of `dylan-macro-smoke.dylan` (the Sprint 50a/b/c seed):
// this file is the production home of the Dylan-side macro engine, the
// Dylan port of the Rust `nod-macro` crate (~1900 lines). It is
// `Module: dylan-lexer` so it bundles with `dylan-lexer.dylan` (the
// Dylan-side lexer) via a `.prj`, calling the real lexer's `<token>`
// machinery, `lex()`, and `token-source-text`.
//
// Under Sprint 52's locus decision (B) — expand entirely Dylan-side,
// before the AST-wire emit (see docs/DYLAN_AST_WIRE.md §7) — this engine
// is bundled into the parse+macro shim so expansion runs inside the
// Dylan front-end before serialising the AST. The Rust `nod-macro`
// becomes the verify oracle + fall-back.
//
// Contents:
//   * Data model: <tok>, <fragment> family, <pattern-elem> family,
//     <template-elem> family, <binding>, <macro-rule>, <macro-def>.
//   * Group-balancer (flat token stream → fragment tree).
//   * Real-lexer adapter (<token> → <tok>).
//   * Pattern matching (greedy, depth-aware ?x:body).
//   * Template substitution → source text.
//   * `define macro` body parse → <macro-def> (multi-rule).
//   * 52.2: `collect-macro-defs(source)` — top-level `define macro …
//     end macro` extraction across a whole module's source.
//
// What this file does NOT yet have (later 52 sub-tasks):
//   * Hygiene rename (52.4).
//   * Multi-rule SELECTION at a call site + module-walk driver (52.5).
//   * Span rewriting after re-parse (52.4).
//   * Wiring into the parse shim before wire-emit (52.6).

// ─── Minimal token + fragment shape ───────────────────────────────────────

// A token is (kind, text). Real `<token>` from dylan-lexer.dylan has
// the same shape plus spans; we omit spans for the smoke. Token kinds
// used in this smoke: #"ident", #"kw-end", #"punct".

define class <tok> (<object>)
  slot tok-kind :: <symbol>,      init-keyword: kind:;
  slot tok-text :: <byte-string>, init-keyword: text:;
end class;

define function make-tok (k :: <symbol>, t :: <byte-string>) => (x :: <tok>)
  make(<tok>, kind: k, text: t)
end function;

// A Fragment is either a single token or a grouped sequence
// `( … )`, `[ … ]`, `{ … }`, etc. The macro engine matches at this
// level — call-site fragments against pattern elements.

define class <fragment> (<object>)
end class;

define class <token-fragment> (<fragment>)
  slot tfrag-tok :: <tok>, init-keyword: tok:;
end class;

define class <group-fragment> (<fragment>)
  slot gfrag-kind :: <symbol>,           init-keyword: kind:;   // #"paren", #"bracket", #"brace"
  slot gfrag-body :: <stretchy-vector>,  init-keyword: body:;
end class;

define function make-token-frag (t :: <tok>) => (f :: <token-fragment>)
  make(<token-fragment>, tok: t)
end function;

define function make-group-frag (kind :: <symbol>, body :: <stretchy-vector>)
 => (f :: <group-fragment>)
  make(<group-fragment>, kind: kind, body: body)
end function;

// ─── Pattern + template elements ──────────────────────────────────────────
//
// PatternElem variants (matching Rust nod-macro):
//   <pat-literal>  — a fixed token the call must reproduce
//   <pat-variable> — `?name:kind`, binds one or more call fragments
//   <pat-group>    — `( … )` etc, recursively patterned

define class <pattern-elem> (<object>)
end class;

define class <pat-literal> (<pattern-elem>)
  slot pat-lit-tok :: <tok>, init-keyword: tok:;
end class;

define class <pat-variable> (<pattern-elem>)
  slot pat-var-name :: <byte-string>, init-keyword: name:;
  slot pat-var-kind :: <symbol>,       init-keyword: kind:;
    // #"expression" | #"body" — Sprint 50a subset.
end class;

define class <pat-group> (<pattern-elem>)
  slot pat-grp-kind :: <symbol>,          init-keyword: kind:;
  slot pat-grp-body :: <stretchy-vector>, init-keyword: body:;
end class;

// TemplateElem variants. `<tpl-substitution>` carries the binding
// name to splice; everything else is emitted verbatim.

define class <template-elem> (<object>)
end class;

define class <tpl-literal> (<template-elem>)
  slot tpl-lit-tok :: <tok>, init-keyword: tok:;
end class;

define class <tpl-substitution> (<template-elem>)
  slot tpl-sub-name :: <byte-string>, init-keyword: name:;
end class;

define class <tpl-group> (<template-elem>)
  slot tpl-grp-kind :: <symbol>,          init-keyword: kind:;
  slot tpl-grp-body :: <stretchy-vector>, init-keyword: body:;
end class;

// ─── Bindings (linear list-of-pairs for now) ──────────────────────────────
//
// A bindings table maps a pattern-variable name (<byte-string>) to a
// captured sequence of fragments (<stretchy-vector>). The Rust
// implementation uses a HashMap; for Sprint 50a's tiny tables (≤4
// entries) a linear scan is faster than the hash overhead.

define class <binding> (<object>)
  slot binding-name  :: <byte-string>,    init-keyword: name:;
  slot binding-frags :: <stretchy-vector>, init-keyword: frags:;
end class;

define function make-bindings () => (b :: <stretchy-vector>)
  make(<stretchy-vector>)
end function;

define function bindings-add! (b :: <stretchy-vector>, name :: <byte-string>,
                               frags :: <stretchy-vector>) => ()
  add!(b, make(<binding>, name: name, frags: frags));
end function;

define function bindings-get (b :: <stretchy-vector>, name :: <byte-string>)
 => (frags :: <object>)
  // Returns the <stretchy-vector> of captured fragments, or #f on miss.
  let n = size(b);
  let i = 0;
  let found = #f;
  until (i = n | found)
    let entry = b[i];
    if (binding-name(entry) = name)
      found := binding-frags(entry);
    else
      i := i + 1;
    end;
  end;
  found
end function;

// ─── Pattern matching ─────────────────────────────────────────────────────
//
// match-pattern takes a pattern (stretchy-vector of <pattern-elem>)
// and a call site's fragments (stretchy-vector of <fragment>) and
// returns either a bindings table or #f on mismatch.
//
// Sprint 50a supports:
//   * <pat-literal>  — token-kind + text equality
//   * <pat-variable> with kind #"expression" — binds exactly one frag
//   * <pat-variable> with kind #"body"       — binds 0+ frags up to
//                                              the first match of the
//                                              NEXT literal in pattern,
//                                              or to end-of-call if
//                                              pattern has no trailer.
//                                              Depth-aware on `end`.
//   * <pat-group>    — recursive match on body
//
// Greedy, left-to-right, no backtracking. Same approach as Rust
// nod-macro::match_pattern at Sprint-17 level.

define function tok-frag? (f :: <fragment>) => (yes? :: <boolean>)
  instance?(f, <token-fragment>)
end function;

define function group-frag? (f :: <fragment>) => (yes? :: <boolean>)
  instance?(f, <group-fragment>)
end function;

// Predicate: does this call-site fragment match a literal-pattern's
// (kind, text)? Only token fragments can match literals.
define function frag-matches-literal? (f :: <fragment>, lit :: <tok>)
 => (yes? :: <boolean>)
  if (tok-frag?(f))
    let tf = f;
    let t = tfrag-tok(tf);
    tok-kind(t) = tok-kind(lit) & tok-text(t) = tok-text(lit)
  else
    #f
  end
end function;

// Recognise call-site idents that open an end-terminated body form.
// Used by the body-matcher's depth-aware scan. List mirrors the Rust
// engine's tok_text_eq cluster.
define function opens-end-form? (text :: <byte-string>) => (yes? :: <boolean>)
  text = "if" | text = "unless" | text = "while" | text = "until"
    | text = "for" | text = "block" | text = "select" | text = "case"
    | text = "cond" | text = "begin" | text = "method" | text = "when"
    | text = "with-cleanup"
end function;

// Scan `call[ci..]` for the first position whose fragment matches
// `lit`, tracking nesting so a nested `if … end` doesn't claim the
// outer `unless`'s terminator. Returns the absolute index or #f.
define function find-body-end (call :: <stretchy-vector>, ci :: <integer>,
                               lit :: <tok>) => (pos :: <object>)
  let n = size(call);
  let depth = 0;
  let i = ci;
  let found = #f;
  let kw-end-lit = tok-kind(lit) = #"kw-end";
  until (i = n | found)
    let f = call[i];
    if (tok-frag?(f))
      let t = tfrag-tok(f);
      if (kw-end-lit & tok-kind(t) = #"ident" & opens-end-form?(tok-text(t)))
        depth := depth + 1;
      elseif (frag-matches-literal?(f, lit))
        if (depth = 0)
          found := i;
        else
          depth := depth - 1;
        end;
      end;
    end;
    if (~ found) i := i + 1; end;
  end;
  found
end function;

// Count trailing literal/group pattern elements — used as the body's
// stop-point when the next pattern element isn't a literal.
define function count-trailing-literals (pattern :: <stretchy-vector>,
                                         start :: <integer>) => (n :: <integer>)
  let m = size(pattern);
  let n = 0;
  let i = m - 1;
  let stop? = #f;
  until (i < start | stop?)
    let p = pattern[i];
    if (instance?(p, <pat-literal>) | instance?(p, <pat-group>))
      n := n + 1;
      i := i - 1;
    else
      stop? := #t;
    end;
  end;
  n
end function;

define function match-pattern (pattern :: <stretchy-vector>,
                               call    :: <stretchy-vector>)
 => (b :: <object>)
  let b      = make-bindings();
  let pi     = 0;
  let ci     = 0;
  let pn     = size(pattern);
  let cn     = size(call);
  let fail?  = #f;
  until (pi = pn | fail?)
    let p = pattern[pi];
    if (instance?(p, <pat-literal>))
      if (ci >= cn)
        fail? := #t;
      else
        let f = call[ci];
        if (frag-matches-literal?(f, pat-lit-tok(p)))
          ci := ci + 1;
          pi := pi + 1;
        else
          fail? := #t;
        end;
      end;
    elseif (instance?(p, <pat-variable>))
      let kind = pat-var-kind(p);
      // Sprint 52.3 — full nod-macro PatternKind parity. Expression,
      // MacroArg, and Constraint all bind exactly one fragment (the Rust
      // engine aliases the latter two to Expression today). Name binds
      // one fragment but only if it is an identifier token. Variable
      // binds an identifier plus an optional `:: <type>`. ParameterList
      // binds a single paren group. Body is the depth-aware greedy match.
      if (kind = #"expression" | kind = #"macro-arg" | kind = #"constraint")
        if (ci >= cn)
          fail? := #t;
        else
          let frags = make(<stretchy-vector>);
          add!(frags, call[ci]);
          bindings-add!(b, pat-var-name(p), frags);
          ci := ci + 1;
          pi := pi + 1;
        end;
      elseif (kind = #"name")
        // Bind one fragment, but only an identifier token.
        if (ci >= cn)
          fail? := #t;
        else
          let f = call[ci];
          if (tok-frag?(f) & tok-kind(tfrag-tok(f)) = #"ident")
            let frags = make(<stretchy-vector>);
            add!(frags, f);
            bindings-add!(b, pat-var-name(p), frags);
            ci := ci + 1;
            pi := pi + 1;
          else
            fail? := #t;
          end;
        end;
      elseif (kind = #"parameter-list")
        // Bind a single paren group, contents unvalidated (the template
        // re-emits verbatim; the expansion-site parser rejects ill-formed
        // lists). Mirrors nod-macro's ParameterList arm.
        if (ci >= cn)
          fail? := #t;
        else
          let f = call[ci];
          if (group-frag?(f) & gfrag-kind(f) = #"paren")
            let frags = make(<stretchy-vector>);
            add!(frags, f);
            bindings-add!(b, pat-var-name(p), frags);
            ci := ci + 1;
            pi := pi + 1;
          else
            fail? := #t;
          end;
        end;
      elseif (kind = #"variable")
        // `?x:variable` — an identifier, optionally `:: <type>`. The
        // type annotation may lex as a single `::` punct or two adjacent
        // `:` puncts (the Dylan lexer has no dedicated `::`); accept both.
        if (ci >= cn)
          fail? := #t;
        else
          let head = call[ci];
          if (~ (tok-frag?(head) & tok-kind(tfrag-tok(head)) = #"ident"))
            fail? := #t;
          else
            let consumed = 1;
            if (ci + 2 < cn
                  & tok-is?(call[ci + 1], #"punct", "::")
                  & tok-frag?(call[ci + 2])
                  & tok-kind(tfrag-tok(call[ci + 2])) = #"ident")
              consumed := 3;
            elseif (ci + 3 < cn
                      & tok-is?(call[ci + 1], #"punct", ":")
                      & tok-is?(call[ci + 2], #"punct", ":")
                      & tok-frag?(call[ci + 3])
                      & tok-kind(tfrag-tok(call[ci + 3])) = #"ident")
              consumed := 4;
            end;
            let frags = make(<stretchy-vector>);
            let j = ci;
            until (j = ci + consumed)
              add!(frags, call[j]);
              j := j + 1;
            end;
            bindings-add!(b, pat-var-name(p), frags);
            ci := ci + consumed;
            pi := pi + 1;
          end;
        end;
      elseif (kind = #"body")
        // Determine body's end position: scan to the next literal in
        // pattern, or fall back to len(call) - count_trailing_literals.
        // Statement-form (not let-binding an if-expression) to dodge
        // the GAP-011-family LLVM SSA-dominance issue on heap-typed
        // join values.
        let body-end = cn - count-trailing-literals(pattern, pi + 1);
        if (pi + 1 < pn & instance?(pattern[pi + 1], <pat-literal>))
          let next-lit = pat-lit-tok(pattern[pi + 1]);
          let scanned  = find-body-end(call, ci, next-lit);
          if (scanned) body-end := scanned; end;
        end;
        let frags = make(<stretchy-vector>);
        let j = ci;
        until (j = body-end)
          add!(frags, call[j]);
          j := j + 1;
        end;
        bindings-add!(b, pat-var-name(p), frags);
        ci := body-end;
        pi := pi + 1;
      else
        // Unsupported pattern kind for Sprint 50a.
        fail? := #t;
      end;
    elseif (instance?(p, <pat-group>))
      if (ci >= cn)
        fail? := #t;
      else
        let f = call[ci];
        if (~ group-frag?(f))
          fail? := #t;
        else
          let g = f;
          if (gfrag-kind(g) ~= pat-grp-kind(p))
            fail? := #t;
          else
            let sub = match-pattern(pat-grp-body(p), gfrag-body(g));
            if (~ sub)
              fail? := #t;
            else
              // Merge sub-bindings into b.
              let m = size(sub);
              let k = 0;
              until (k = m)
                let e = sub[k];
                add!(b, e);
                k := k + 1;
              end;
              ci := ci + 1;
              pi := pi + 1;
            end;
          end;
        end;
      end;
    else
      fail? := #t;
    end;
  end;
  if (fail? | ci ~= cn)
    #f
  else
    b
  end
end function;

// ─── Template substitution → text ─────────────────────────────────────────
//
// The Rust `substitute` emits a text buffer; the caller re-lexes and
// re-parses. We mirror that: walk the template, accumulating into a
// <stretchy-vector> of <byte-string> chunks, then concatenate via the
// stdlib's reduce + concatenate.
//
// Spacing policy: insert a single space between any two adjacent
// chunks unless the surroundings are tight (open paren before, close
// paren / comma / semicolon after). Same heuristic the Rust engine
// uses to keep emitted text readable.

// Render a <tok>'s surface text. Keyword-name tokens (`Module:`, `x:`)
// are stored colon-stripped by the lexer adapter; re-append the colon so
// rendered text re-lexes back to a keyword-name (not a bare ident) — this
// keeps the `Module:` preamble and body keyword-args faithful through the
// expand → render → re-lex round-trip.
define function tok-render-text (t :: <tok>) => (s :: <byte-string>)
  if (tok-kind(t) = #"keyword-name")
    concatenate(tok-text(t), ":")
  else
    tok-text(t)
  end
end function;

define function emit-tok (out :: <stretchy-vector>, t :: <tok>) => ()
  add!(out, tok-render-text(t));
end function;

define function emit-frag (out :: <stretchy-vector>, f :: <fragment>) => ()
  if (tok-frag?(f))
    emit-tok(out, tfrag-tok(f));
  else
    let g = f;
    let k = gfrag-kind(g);
    // Statement-form open/close pick: heap-typed `let X = if ... end`
    // hits the GAP-011-family LLVM SSA-dominance issue (deferred fix,
    // see Sprint 49d retro). Statement-form sidesteps it.
    let open  = "{";
    let close = "}";
    if (k = #"paren")
      open := "("; close := ")";
    elseif (k = #"bracket")
      open := "["; close := "]";
    elseif (k = #"hash-paren")
      open := "#("; close := ")";
    elseif (k = #"hash-bracket")
      open := "#["; close := "]";
    elseif (k = #"hash-brace")
      open := "#{"; close := "}";
    end;
    add!(out, open);
    let body = gfrag-body(g);
    let n = size(body);
    let i = 0;
    until (i = n)
      emit-frag(out, body[i]);
      i := i + 1;
    end;
    add!(out, close);
  end;
end function;

define function emit-template (template :: <stretchy-vector>,
                               bindings :: <stretchy-vector>,
                               out      :: <stretchy-vector>) => ()
  let n = size(template);
  let i = 0;
  until (i = n)
    let e = template[i];
    if (instance?(e, <tpl-literal>))
      emit-tok(out, tpl-lit-tok(e));
    elseif (instance?(e, <tpl-substitution>))
      let frags = bindings-get(bindings, tpl-sub-name(e));
      if (frags)
        let m = size(frags);
        let j = 0;
        until (j = m)
          emit-frag(out, frags[j]);
          j := j + 1;
        end;
      end;
    elseif (instance?(e, <tpl-group>))
      let k = tpl-grp-kind(e);
      let open  = "{";
      let close = "}";
      if (k = #"paren")
        open := "("; close := ")";
      elseif (k = #"bracket")
        open := "["; close := "]";
      elseif (k = #"hash-paren")
        open := "#("; close := ")";
      elseif (k = #"hash-bracket")
        open := "#["; close := "]";
      elseif (k = #"hash-brace")
        open := "#{"; close := "}";
      end;
      add!(out, open);
      emit-template(tpl-grp-body(e), bindings, out);
      add!(out, close);
    end;
    i := i + 1;
  end;
end function;

// Join chunks with single spaces. A more sophisticated pass would
// respect cluster boundaries (no space between an ident and its
// opening paren); Sprint 50b will refine this.
define function join-chunks (chunks :: <stretchy-vector>) => (s :: <byte-string>)
  let n = size(chunks);
  let result = "";
  if (n > 0)
    result := chunks[0];
    let i = 1;
    until (i = n)
      result := concatenate(result, " ");
      result := concatenate(result, chunks[i]);
      i := i + 1;
    end;
  end;
  result
end function;

define function substitute (template :: <stretchy-vector>,
                            bindings :: <stretchy-vector>)
 => (s :: <byte-string>)
  let out = make(<stretchy-vector>);
  emit-template(template, bindings, out);
  join-chunks(out)
end function;

// Sprint 52.3 — render a raw fragment sequence (e.g. a binding's
// captured fragments) to the same canonical, single-space-joined text
// the substitution emitter produces. Used by the match driver to print
// each binding's value for the Rust-vs-Dylan parity gate.
define function render-frags (frags :: <stretchy-vector>)
 => (s :: <byte-string>)
  let out = make(<stretchy-vector>);
  let n = size(frags);
  let i = 0;
  until (i = n)
    emit-frag(out, frags[i]);
    i := i + 1;
  end;
  join-chunks(out)
end function;

// ─── Sprint 52.4 — template substitution with hygiene ────────────────────
//
// Hygiene policy (binder-only rename), mirroring nod-macro:
//   * collect-template-binders walks the template for identifiers in
//     BINDING position: the ident after `let`, and the param idents of a
//     `method (…)` / `function (…)` head.
//   * emit-template-hyg renames every template-literal occurrence of a
//     binder name to `<name>__nod_hyg_<nonce>`, UNLESS the name is a
//     pattern variable or is in the conservative no-rename keyword/type
//     list. Reference-position identifiers (not binders) flow through
//     verbatim so they resolve against the surrounding scope.
//   * Substituted (`?x`) call-site fragments are spliced verbatim and
//     never renamed.
// The nonce is passed as a string so the gate can pin it deterministically
// for the byte-identical cross-check against the Rust expander.

// Small string-set on a <stretchy-vector> (linear; sets are tiny).
define function string-in? (set :: <stretchy-vector>, s :: <byte-string>)
 => (yes? :: <boolean>)
  let n = size(set);
  let i = 0;
  let found = #f;
  until (i = n | found)
    if (set[i] = s) found := #t; else i := i + 1; end;
  end;
  found
end function;

define function string-set-add! (set :: <stretchy-vector>, s :: <byte-string>) => ()
  if (~ string-in?(set, s)) add!(set, s); end;
end function;

// Pattern-variable names (recursively, including group sub-patterns).
define function collect-pattern-var-names (pattern :: <stretchy-vector>)
 => (out :: <stretchy-vector>)
  let out = make(<stretchy-vector>);
  collect-pv(pattern, out);
  out
end function;

define function collect-pv (pattern :: <stretchy-vector>, out :: <stretchy-vector>) => ()
  let n = size(pattern);
  let i = 0;
  until (i = n)
    let p = pattern[i];
    if (instance?(p, <pat-variable>))
      string-set-add!(out, pat-var-name(p));
    elseif (instance?(p, <pat-group>))
      collect-pv(pat-grp-body(p), out);
    end;
    i := i + 1;
  end;
end function;

// Is this template element a literal identifier token?
define function tpl-lit-ident? (e :: <template-elem>) => (yes? :: <boolean>)
  if (instance?(e, <tpl-literal>))
    tok-kind(tpl-lit-tok(e)) = #"ident"
  else
    #f
  end
end function;

// Is this template element a paren group?
define function tpl-paren-group? (e :: <template-elem>) => (yes? :: <boolean>)
  if (instance?(e, <tpl-group>))
    tpl-grp-kind(e) = #"paren"
  else
    #f
  end
end function;

// Record every Ident at a binding position in a `method`/`function`
// param list: the head ident and each ident immediately after a comma.
define function record-param-idents (body :: <stretchy-vector>,
                                     out :: <stretchy-vector>) => ()
  let n = size(body);
  let i = 0;
  let expect-name = #t;
  until (i = n)
    let el = body[i];
    if (instance?(el, <tpl-literal>))
      let t = tpl-lit-tok(el);
      if (tok-kind(t) = #"ident" & expect-name)
        string-set-add!(out, tok-text(t));
        expect-name := #f;
      elseif (tok-kind(t) = #"punct" & tok-text(t) = ",")
        expect-name := #t;
      end;
    else
      expect-name := #t;
    end;
    i := i + 1;
  end;
end function;

define function walk-template-for-binders (template :: <stretchy-vector>,
                                           out :: <stretchy-vector>) => ()
  let n = size(template);
  let i = 0;
  until (i >= n)
    let e = template[i];
    if (tpl-lit-ident?(e))
      let text = tok-text(tpl-lit-tok(e));
      if (text = "let" & i + 1 < n & tpl-lit-ident?(template[i + 1]))
        string-set-add!(out, tok-text(tpl-lit-tok(template[i + 1])));
        i := i + 2;
      elseif ((text = "method" | text = "function")
                & i + 1 < n & tpl-paren-group?(template[i + 1]))
        record-param-idents(tpl-grp-body(template[i + 1]), out);
        i := i + 2;
      else
        i := i + 1;
      end;
    elseif (instance?(e, <tpl-group>))
      walk-template-for-binders(tpl-grp-body(e), out);
      i := i + 1;
    else
      i := i + 1;
    end;
  end;
end function;

define function collect-template-binders (template :: <stretchy-vector>)
 => (out :: <stretchy-vector>)
  let out = make(<stretchy-vector>);
  walk-template-for-binders(template, out);
  out
end function;

// Conservative no-rename set: Dylan special-form keywords + core type
// names + helpers. Mirrors nod-macro::is_template_no_rename exactly.
define function is-template-no-rename? (name :: <byte-string>) => (yes? :: <boolean>)
  name = "if" | name = "else" | name = "elseif" | name = "unless"
    | name = "begin" | name = "let" | name = "local" | name = "method"
    | name = "case" | name = "select" | name = "for" | name = "while"
    | name = "until" | name = "block" | name = "exception" | name = "cleanup"
    | name = "finally" | name = "afterwards" | name = "from" | name = "to"
    | name = "by" | name = "below" | name = "above" | name = "in"
    | name = "values" | name = "next-method" | name = "make" | name = "as"
    | name = "instance?" | name = "subtype?" | name = "element"
    | name = "<integer>" | name = "<single-float>" | name = "<double-float>"
    | name = "<boolean>" | name = "<character>" | name = "<string>"
    | name = "<byte-string>" | name = "<object>" | name = "<class>"
    | name = "<symbol>" | name = "<pair>" | name = "<list>"
    | name = "<empty-list>" | name = "<vector>" | name = "<sequence>"
    | name = "<collection>"
end function;

// Emit a template with hygiene: rename binder identifiers, splice
// substitutions verbatim, recurse into groups. `nonce-str` is the
// hygiene nonce as a string; `binders` and `pvars` are string-sets.
define function emit-template-hyg (template :: <stretchy-vector>,
                                   bindings :: <stretchy-vector>,
                                   out :: <stretchy-vector>,
                                   nonce-str :: <byte-string>,
                                   binders :: <stretchy-vector>,
                                   pvars :: <stretchy-vector>) => ()
  let n = size(template);
  let i = 0;
  until (i = n)
    let e = template[i];
    if (instance?(e, <tpl-literal>))
      let t = tpl-lit-tok(e);
      let text = tok-text(t);
      // Base surface text (keyword-name colon re-appended); a renameable
      // binder ident is an ident, never a keyword-name, so the rename
      // branch and the colon handling don't overlap.
      let emit-text = tok-render-text(t);
      if (tok-kind(t) = #"ident" & string-in?(binders, text)
            & ~ string-in?(pvars, text) & ~ is-template-no-rename?(text))
        emit-text := concatenate(text, concatenate("__nod_hyg_", nonce-str));
      end;
      add!(out, emit-text);
    elseif (instance?(e, <tpl-substitution>))
      let frags = bindings-get(bindings, tpl-sub-name(e));
      if (frags)
        let m = size(frags);
        let j = 0;
        until (j = m)
          emit-frag(out, frags[j]);
          j := j + 1;
        end;
      end;
    elseif (instance?(e, <tpl-group>))
      let k = tpl-grp-kind(e);
      let open  = "{";
      let close = "}";
      if (k = #"paren")
        open := "("; close := ")";
      elseif (k = #"bracket")
        open := "["; close := "]";
      elseif (k = #"hash-paren")
        open := "#("; close := ")";
      elseif (k = #"hash-bracket")
        open := "#["; close := "]";
      elseif (k = #"hash-brace")
        open := "#{"; close := "}";
      end;
      add!(out, open);
      emit-template-hyg(tpl-grp-body(e), bindings, out, nonce-str, binders, pvars);
      add!(out, close);
    end;
    i := i + 1;
  end;
end function;

// Full hygienic substitution: collect binders, emit, join.
define function substitute-hyg (template :: <stretchy-vector>,
                                bindings :: <stretchy-vector>,
                                pvars :: <stretchy-vector>,
                                nonce-str :: <byte-string>)
 => (s :: <byte-string>)
  let binders = collect-template-binders(template);
  let out = make(<stretchy-vector>);
  emit-template-hyg(template, bindings, out, nonce-str, binders, pvars);
  join-chunks(out)
end function;

// ─── Sprint 52.5 — multi-rule selection + module-walk expansion ──────────
//
// expand-call mirrors nod-macro::expand_one's rule loop: try each rule's
// pattern in definition order, first match wins, substitute that rule's
// template. expand-fragments is the module-walk driver: walk a fragment
// stream, expand every macro call site (multi-rule), re-lex the result,
// and recurse to fixpoint. Under locus (B) expansion operates on the
// Dylan-side token/fragment representation (the seed's pipeline), so the
// walk is fragment-level — the same shape nod-macro::expand_module has
// over its AST.

// Linear lookup of a <macro-def> by name in a defs vector, or #f.
define function macro-table-lookup (table :: <stretchy-vector>, name :: <byte-string>)
 => (def :: <object>)
  let n = size(table);
  let i = 0;
  let found = #f;
  until (i = n | found)
    if (macro-def-name(table[i]) = name)
      found := table[i];
    else
      i := i + 1;
    end;
  end;
  found
end function;

// Multi-rule selection: try each rule's pattern against call-frags in
// definition order; on the first match, substitute that rule's template
// (with hygiene). Returns the expansion text, or #f if no rule matched.
define function expand-call (def :: <macro-def>,
                             call-frags :: <stretchy-vector>,
                             nonce-str :: <byte-string>)
 => (text :: <object>)
  let rules = macro-def-rules(def);
  let n = size(rules);
  let i = 0;
  let result = #f;
  until (i = n | result)
    let rule    = rules[i];
    let pattern = macro-rule-pattern(rule);
    let b = match-pattern(pattern, call-frags);
    if (b)
      let pvars = collect-pattern-var-names(pattern);
      result := substitute-hyg(macro-rule-template(rule), b, pvars, nonce-str);
    else
      i := i + 1;
    end;
  end;
  result
end function;

// A macro is call-shaped (`name(args)`) rather than body-shaped
// (`name args… end`) when its first rule's pattern is exactly
// [literal name, paren-group]. Mirrors nod-macro's call-vs-statement
// macro distinction; at the fragment level the shape is read off the
// rule pattern.
define function macro-call-shaped? (def :: <macro-def>) => (yes? :: <boolean>)
  let rules = macro-def-rules(def);
  let result = #f;
  if (size(rules) > 0)
    let pat = macro-rule-pattern(rules[0]);
    if (size(pat) = 2 & instance?(pat[1], <pat-group>))
      result := pat-grp-kind(pat[1]) = #"paren";
    end;
  end;
  result
end function;

// Locate the `end` that closes a body-shaped macro call beginning at the
// macro-name fragment `i`. Depth-aware: nested body-opening forms (the
// `opens-end-form?` keywords plus any BODY-shaped macro name in the
// table) bump the nesting depth, so only the macro's own terminator is
// returned. Call-shaped macros do NOT open a body and so do not bump
// depth. Returns the absolute index of the closing `end`, or #f.
define function find-macro-call-end (frags :: <stretchy-vector>, i :: <integer>,
                                     table :: <stretchy-vector>) => (pos :: <object>)
  let n = size(frags);
  let depth = 1;
  let j = i + 1;
  let found = #f;
  until (j = n | found)
    let f = frags[j];
    if (tok-frag?(f))
      let t = tfrag-tok(f);
      if (tok-kind(t) = #"kw-end")
        depth := depth - 1;
        if (depth = 0) found := j; end;
      elseif (tok-kind(t) = #"ident")
        let txt = tok-text(t);
        let mdef = macro-table-lookup(table, txt);
        if (opens-end-form?(txt) | (mdef ~= #f & ~ macro-call-shaped?(mdef)))
          depth := depth + 1;
        end;
      end;
    end;
    if (~ found) j := j + 1; end;
  end;
  found
end function;

// Index just past a `define macro … end macro` form beginning at `i`
// (the `define` fragment). Mirrors collect-macro-defs' tail skip: scan to
// the first top-level kw-end after the name, then step over an optional
// `macro` word and `;`.
define function define-macro-end-index (frags :: <stretchy-vector>, i :: <integer>)
 => (idx :: <integer>)
  let n = size(frags);
  let j = i + 3;
  let done? = #f;
  until (j >= n | done?)
    if (frag-kw-end?(frags[j])) done? := #t; else j := j + 1; end;
  end;
  let k = j;
  if (k < n & frag-kw-end?(frags[k]))               k := k + 1; end;
  if (k < n & tok-is?(frags[k], #"ident", "macro"))  k := k + 1; end;
  if (k < n & tok-is?(frags[k], #"punct", ";"))      k := k + 1; end;
  k
end function;

// Module-walk: walk `frags`, expand every macro call to fixpoint, and
// return the expanded fragment sequence. Non-macro fragments pass through
// unchanged; group bodies are walked recursively. `define macro … end
// macro` forms are STRIPPED — they are compile-time only (lowering
// ignores them) and cannot be losslessly re-rendered (keyword-name tokens
// like `?c:expression` drop their colon), so the expanded output omits
// them; the macro name in the definition header and the pattern literals
// in its rule bodies are correctly never treated as call sites. `depth`
// bounds runaway expansion (a buggy macro that expands to a call of
// itself).
define function expand-fragments (frags :: <stretchy-vector>,
                                  table :: <stretchy-vector>,
                                  nonce-str :: <byte-string>,
                                  depth :: <integer>) => (out :: <stretchy-vector>)
  let out = make(<stretchy-vector>);
  let n = size(frags);
  let i = 0;
  until (i = n)
    let f = frags[i];
    let handled = #f;
    // Strip `define macro … end macro` forms from the expanded output.
    if (define-macro-head?(frags, i))
      i := define-macro-end-index(frags, i);
      handled := #t;
    end;
    if (~ handled & tok-frag?(f) & tok-kind(tfrag-tok(f)) = #"ident" & depth < 50)
      let name = tok-text(tfrag-tok(f));
      let def = macro-table-lookup(table, name);
      if (def ~= #f)
        // Call-shaped macros (`name(args)`) span the name plus the
        // immediately-following paren group; body-shaped macros span to
        // their matching `end`.
        let call-end = #f;
        if (macro-call-shaped?(def))
          if (i + 1 < n & group-frag?(frags[i + 1])
                & gfrag-kind(frags[i + 1]) = #"paren")
            call-end := i + 1;
          end;
        else
          call-end := find-macro-call-end(frags, i, table);
        end;
        if (call-end)
          let call-frags = make(<stretchy-vector>);
          let k = i;
          until (k > call-end)
            add!(call-frags, frags[k]);
            k := k + 1;
          end;
          let text = expand-call(def, call-frags, nonce-str);
          if (text)
            let sub-frags = tokens-to-fragments(lex-source-to-toks(text));
            let expanded  = expand-fragments(sub-frags, table, nonce-str, depth + 1);
            let m = size(expanded);
            let q = 0;
            until (q = m)
              add!(out, expanded[q]);
              q := q + 1;
            end;
            i := call-end + 1;
            handled := #t;
          end;
        end;
      end;
    end;
    if (~ handled)
      if (group-frag?(f))
        let inner = expand-fragments(gfrag-body(f), table, nonce-str, depth);
        add!(out, make-group-frag(gfrag-kind(f), inner));
      else
        add!(out, f);
      end;
      i := i + 1;
    end;
  end;
  out
end function;

// Expand a whole module's source to fixpoint: collect its macro defs,
// lex+fragment the source, expand all call sites, and render the result.
// Is the source's first line a `Word: …` header line? (A single word,
// then `:`, before any whitespace or newline.) Used to decide whether a
// `Module:`-style preamble is present.
define function header-present? (source :: <byte-string>) => (yes? :: <boolean>)
  let n = size(source);
  let i = 0;
  let saw-space = #f;
  let done = #f;
  let result = #f;
  until (i >= n | done)
    let c = %byte-string-element(source, i);
    if (c = 10 | c = 13)        // newline before a colon → not a header
      done := #t;
    elseif (c = 58)             // ':' with a non-empty, space-free key
      if (~ saw-space & i > 0) result := #t; end;
      done := #t;
    elseif (c = 32 | c = 9)
      saw-space := #t;
    end;
    i := i + 1;
  end;
  result
end function;

// Byte offset where the module BODY starts: past the header block (up to
// and including the first blank line) when a header is present, else 0.
// The preamble is host-side metadata; the expanded output is body-only,
// so the single-line render doesn't break preamble detection on re-parse.
define function body-start-offset (source :: <byte-string>) => (off :: <integer>)
  if (~ header-present?(source))
    0
  else
    let n = size(source);
    let i = 0;
    let off = 0;
    let found = #f;
    until (i >= n | found)
      if (%byte-string-element(source, i) = 10)    // LF
        let j = i + 1;
        if (j < n & %byte-string-element(source, j) = 13) j := j + 1; end;
        if (j < n & %byte-string-element(source, j) = 10)  // blank line
          off := j + 1;
          found := #t;
        end;
      end;
      i := i + 1;
    end;
    if (found) off else 0 end
  end
end function;

define function expand-module-source (source :: <byte-string>,
                                      table :: <stretchy-vector>,
                                      nonce-str :: <byte-string>)
 => (text :: <byte-string>)
  // Keep the `Module:` preamble VERBATIM (with its newlines) and prepend
  // it to the single-line expanded body. This preserves the module name
  // (the host needs it for namespace resolution) and re-lexes as a normal
  // file — preamble detection works because the header still has its
  // newline structure, even though the body that follows is single-line
  // (the parser is whitespace-insensitive).
  let off      = body-start-offset(source);
  let preamble = copy-sequence(source, 0, off);
  let body     = copy-sequence(source, off, size(source));
  let frags    = tokens-to-fragments(lex-source-to-toks(body));
  let expanded = expand-fragments(frags, table, nonce-str, 0);
  concatenate(preamble, render-frags(expanded))
end function;

// ─── Sprint 50b — parse `define macro` body fragments → <macro-def> ──────
//
// The Rust nod-macro grammar for a definition body is:
//   macro-body : rule (';' rule)*
//   rule       : '{' pattern '}' '=>' '{' template '}'
//   pattern    : pattern-elem*
//   template   : template-elem*
//   pat-elem   : literal | '?' name ':' kind | group   (group recursive)
//   tpl-elem   : literal | '?' name             | group   (group recursive)
//
// In tokenised form the lexer glues `name:` into a single
// `#"keyword-name"` token. So the common physical shape for
// `?cond:expression` is three tokens: `?`, `cond:`, `expression`.
// Sprint 50b accepts that form (mirrors nod-macro's parse_pattern_var_head
// common arm). The explicit-spaces form `? cond : expression` is rare
// and deferred to 50c when we plug the real lexer in.

// Sprint 50b: a rule wraps one (pattern, template) pair so a single
// def can carry multiple. Sprint 50a's match/substitute happily took
// the two halves separately; the wrapper is just an organisational
// convenience for the def-level parser.
define class <macro-rule> (<object>)
  slot macro-rule-pattern  :: <stretchy-vector>, init-keyword: pattern:;
  slot macro-rule-template :: <stretchy-vector>, init-keyword: template:;
end class;

define class <macro-def> (<object>)
  slot macro-def-name  :: <byte-string>,    init-keyword: name:;
  slot macro-def-rules :: <stretchy-vector>, init-keyword: rules:;
end class;

// Predicate: is `f` a single-token fragment whose token has `kind` and `text`?
define function tok-is? (f :: <fragment>, kind :: <symbol>, text :: <byte-string>)
 => (yes? :: <boolean>)
  if (tok-frag?(f))
    let t = tfrag-tok(f);
    tok-kind(t) = kind & tok-text(t) = text
  else
    #f
  end
end function;

// Strip a trailing `:` from `s` (used to unglue the keyword-name's name).
define function strip-trailing-colon (s :: <byte-string>) => (r :: <byte-string>)
  let n = size(s);
  if (n > 0 & %byte-string-element(s, n - 1) = 58)
    copy-sequence(s, 0, n - 1)
  else
    s
  end
end function;

// Parse one pattern-elem from `body[i]`, return (elem, consumed-count).
define function parse-pattern-elem (body :: <stretchy-vector>, i :: <integer>)
 => (elem :: <pattern-elem>, consumed :: <integer>)
  let f = body[i];
  let result :: <pattern-elem> = make(<pat-literal>, tok: make-tok(#"ident", "?"));
  let consumed = 1;
  if (group-frag?(f))
    let g = f;
    let inner-pattern = parse-pattern-body(gfrag-body(g));
    result := make(<pat-group>, kind: gfrag-kind(g), body: inner-pattern);
  elseif (tok-is?(f, #"punct", "?"))
    // Expect: ?  keyword-name(name:)  ident(kind)
    let name-frag = body[i + 1];
    let kind-frag = body[i + 2];
    let name-tok  = tfrag-tok(name-frag);
    let kind-tok  = tfrag-tok(kind-frag);
    let name      = strip-trailing-colon(tok-text(name-tok));
    let kind-text = tok-text(kind-tok);
    // Sprint 52.3 — recognise all seven nod-macro PatternKind words.
    // `case-body`/`type`/`case-expression`/`definition` are recognised
    // names that alias to expression (mirrors parse_kind_word); any
    // other word also falls through to expression (the corpus never
    // uses one, and a totalising default keeps the matcher panic-free).
    let kind-sym  = #"expression";
    if (kind-text = "body")                 kind-sym := #"body";
    elseif (kind-text = "name")             kind-sym := #"name";
    elseif (kind-text = "variable")         kind-sym := #"variable";
    elseif (kind-text = "macro-arg")        kind-sym := #"macro-arg";
    elseif (kind-text = "parameter-list")   kind-sym := #"parameter-list";
    elseif (kind-text = "constraint")       kind-sym := #"constraint";
    end;
    result := make(<pat-variable>, name: name, kind: kind-sym);
    consumed := 3;
  else
    result := make(<pat-literal>, tok: tfrag-tok(f));
  end;
  values(result, consumed)
end function;

define function parse-pattern-body (body :: <stretchy-vector>)
 => (pat :: <stretchy-vector>)
  let out = make(<stretchy-vector>);
  let n = size(body);
  let i = 0;
  until (i = n)
    let (elem, consumed) = parse-pattern-elem(body, i);
    add!(out, elem);
    i := i + consumed;
  end;
  out
end function;

// Parse one template-elem. Templates only have `?name` (no kind).
define function parse-template-elem (body :: <stretchy-vector>, i :: <integer>)
 => (elem :: <template-elem>, consumed :: <integer>)
  let f = body[i];
  let result :: <template-elem> = make(<tpl-literal>, tok: make-tok(#"ident", "?"));
  let consumed = 1;
  if (group-frag?(f))
    let g = f;
    let inner-tpl = parse-template-body(gfrag-body(g));
    result := make(<tpl-group>, kind: gfrag-kind(g), body: inner-tpl);
  elseif (tok-is?(f, #"punct", "?"))
    let name-frag = body[i + 1];
    let name-tok  = tfrag-tok(name-frag);
    result := make(<tpl-substitution>, name: tok-text(name-tok));
    consumed := 2;
  else
    result := make(<tpl-literal>, tok: tfrag-tok(f));
  end;
  values(result, consumed)
end function;

define function parse-template-body (body :: <stretchy-vector>)
 => (tpl :: <stretchy-vector>)
  let out = make(<stretchy-vector>);
  let n = size(body);
  let i = 0;
  until (i = n)
    let (elem, consumed) = parse-template-elem(body, i);
    add!(out, elem);
    i := i + consumed;
  end;
  out
end function;

// Parse one rule starting at `frags[i]`: expects `{ pattern } => { template }`.
// Returns (rule, next-i).
define function parse-rule (frags :: <stretchy-vector>, start :: <integer>)
 => (rule :: <macro-rule>, next :: <integer>)
  let pat-group  = frags[start];
  let arrow-frag = frags[start + 1];
  let tpl-group  = frags[start + 2];
  let pattern  = parse-pattern-body(gfrag-body(pat-group));
  let template = parse-template-body(gfrag-body(tpl-group));
  let rule = make(<macro-rule>, pattern: pattern, template: template);
  values(rule, start + 3)
end function;

// Parse a complete `define macro NAME` body: 1+ rules separated by `;`.
define function parse-macro-def (name :: <byte-string>, body :: <stretchy-vector>)
 => (def :: <macro-def>)
  let rules = make(<stretchy-vector>);
  let n = size(body);
  let i = 0;
  until (i >= n)
    // Skip a leading `;` between rules.
    if (i < n & tok-is?(body[i], #"punct", ";"))
      i := i + 1;
    else
      let (rule, next) = parse-rule(body, i);
      add!(rules, rule);
      i := next;
    end;
  end;
  make(<macro-def>, name: name, rules: rules)
end function;

// ─── Sprint 50c-1 — token-stream → fragment-tree group-balancer ──────────
//
// A real lexer emits a FLAT stream of tokens; the macro engine wants
// fragments — tokens plus recursive `<group-fragment>` nesting for
// `( … )`, `[ … ]`, `{ … }`. This pass walks tokens left-to-right and
// builds the tree.
//
// Mirrors `nod-reader::fragments::Fragmenter`. Sprint 50c-1 supports
// the three basic group kinds (paren/bracket/brace); the `#( #[ #{`
// hash-prefixed groups land alongside the real lexer integration in
// 50c-2.
//
// Returns (frags, next-index). When called at the top level, the
// caller passes `closer = ""` so the walk runs to end-of-token-stream;
// recursive calls pass the expected close-text and stop when they see
// it.

define function group-open-kind (text :: <byte-string>) => (kind :: <object>)
  // Returns the <symbol> for an opener token, or #f if not an opener.
  // Sprint 50c-3 — added hash-prefixed openers `#(`, `#[`, `#{`.
  let result = #f;
  if (text = "(")        result := #"paren";
  elseif (text = "[")    result := #"bracket";
  elseif (text = "{")    result := #"brace";
  elseif (text = "#(")   result := #"hash-paren";
  elseif (text = "#[")   result := #"hash-bracket";
  elseif (text = "#{")   result := #"hash-brace";
  end;
  result
end function;

define function group-close-text (kind :: <symbol>) => (text :: <byte-string>)
  // Hash-prefixed groups close with the bare close-bracket — the
  // lexer doesn't emit `#)` / `#]` / `#}`.
  let result = "}";
  if (kind = #"paren")             result := ")";
  elseif (kind = #"bracket")       result := "]";
  elseif (kind = #"hash-paren")    result := ")";
  elseif (kind = #"hash-bracket")  result := "]";
  elseif (kind = #"hash-brace")    result := "}";
  end;
  result
end function;

// Walk `tokens` from index `start`. Build a stretchy-vector of
// fragments. If `closer` is non-empty, stop when a punct token with
// that text is seen (and consume it). Returns (frags, next-i).
define function tokens-to-fragments-from (tokens :: <stretchy-vector>,
                                          start  :: <integer>,
                                          closer :: <byte-string>)
 => (frags :: <stretchy-vector>, next :: <integer>)
  let frags = make(<stretchy-vector>);
  let n = size(tokens);
  let i = start;
  let done? = #f;
  until (i = n | done?)
    let t = tokens[i];
    let text = tok-text(t);
    let is-punct? = tok-kind(t) = #"punct";
    if (is-punct? & size(closer) > 0 & text = closer)
      // Consume the closer and stop.
      i := i + 1;
      done? := #t;
    else
      let open-kind = #f;
      if (is-punct?) open-kind := group-open-kind(text); end;
      if (open-kind)
        let close-text = group-close-text(open-kind);
        let (body, after) =
          tokens-to-fragments-from(tokens, i + 1, close-text);
        add!(frags, make-group-frag(open-kind, body));
        i := after;
      else
        add!(frags, make-token-frag(t));
        i := i + 1;
      end;
    end;
  end;
  values(frags, i)
end function;

define function tokens-to-fragments (tokens :: <stretchy-vector>)
 => (frags :: <stretchy-vector>)
  let (frags, _next) = tokens-to-fragments-from(tokens, 0, "");
  frags
end function;

// ─── Sprint 50c-2/3 — adapt the REAL dylan-lexer's <token> → <tok> ───────
//
// The smoke is bundled with `dylan-lexer.dylan` via the project file
// `dylan-macro-smoke.prj`, so the lexer's `lex(<byte-string>)`,
// `<token>` hierarchy, and `token-source-text` are in scope.
//
// Sprint 50c-3 — replaced the 50c-2 hand-enumerated keyword + punct
// inverse tables with `token-source-text(t, source)`. The lexer
// already keeps a span on every token; slicing the source via that
// span recovers the original text directly. No more enumeration to
// keep in sync — every keyword the lexer knows now round-trips for
// free.

// Convert one lexer token to the engine's <tok> form, or #f if it
// should be skipped (trivia / unsupported). Pass `source` so
// `token-source-text` can slice it for keyword/punct/etc text.
define function lex-token-to-tok (t :: <token>, source :: <byte-string>)
 => (r :: <object>)
  let result = #f;
  if (instance?(t, <whitespace-token>) | instance?(t, <comment-token>))
    result := #f;
  elseif (instance?(t, <keyword-token>))
    let kw   = keyword-token-keyword(t);
    let text = token-source-text(t, source);
    if (kw = #"end")
      result := make-tok(#"kw-end", text);
    else
      result := make-tok(#"ident", text);
    end;
  elseif (instance?(t, <identifier-token>))
    result := make-tok(#"ident", identifier-token-name(t));
  elseif (instance?(t, <keyword-name-token>))
    // Lexer already strips the trailing ":"; my parser tolerates that.
    result := make-tok(#"keyword-name", keyword-name-token-name(t));
  elseif (instance?(t, <punctuation-token>))
    result := make-tok(#"punct", token-source-text(t, source));
  elseif (instance?(t, <literal-vector-open>))
    // `#(` opens a literal-vector group. Surfaces as a punct token
    // with text "#(" so the group-balancer can recognise + match.
    result := make-tok(#"punct", "#(");
  elseif (instance?(t, <literal-sequence-open>))
    // `#[` opens a literal-sequence group.
    result := make-tok(#"punct", "#[");
  elseif (instance?(t, <boolean-literal-token>))
    let v = boolean-literal-token-value(t);
    let text = "#t";
    if (~ v) text := "#f"; end;
    result := make-tok(#"ident", text);
  // Sprint 52.5 — round-trip the remaining literal token kinds as opaque
  // #"literal" tokens (text recovered from the span). Without this they
  // were silently dropped, which is harmless when collecting/matching the
  // corpus (no literals in load-bearing positions) but corrupts re-lexed
  // expansions that contain literals (e.g. `unless ?x (1) end` would lose
  // the `1`). <number-token> covers integer/float/ratio via inheritance.
  elseif (instance?(t, <number-token>))
    result := make-tok(#"literal", token-source-text(t, source));
  elseif (instance?(t, <string-literal-token>))
    result := make-tok(#"literal", token-source-text(t, source));
  elseif (instance?(t, <character-literal-token>))
    result := make-tok(#"literal", token-source-text(t, source));
  elseif (instance?(t, <symbol-literal-token>))
    result := make-tok(#"literal", token-source-text(t, source));
  elseif (instance?(t, <nil-literal-token>))
    result := make-tok(#"literal", token-source-text(t, source));
  elseif (instance?(t, <escaped-ident-token>))
    // `\+` and friends — an operator used as an identifier.
    result := make-tok(#"ident", token-source-text(t, source));
  end;
  result
end function;

// Lex `source`, filter trivia / unsupported tokens, return a flat
// <stretchy-vector> of <tok>. Designed to drive `tokens-to-fragments`
// directly.
define function lex-source-to-toks (source :: <byte-string>)
 => (toks :: <stretchy-vector>)
  let raw = lex(source);
  let out = make(<stretchy-vector>);
  let n = size(raw);
  let i = 0;
  until (i = n)
    let t = raw[i];
    let mine = lex-token-to-tok(t, source);
    if (mine) add!(out, mine); end;
    i := i + 1;
  end;
  out
end function;


// ─── Sprint 52.2 — top-level `define macro … end macro` extraction ───────
//
// The Rust side recognises `define macro NAME … end macro` in the
// PARSER (nod-reader), producing `Item::DefineMacro { name,
// body_fragments }`; `nod-macro::collect_macros` then walks those items.
// Under locus (B) the Dylan front-end does both jobs Dylan-side. This
// function is the collector: lex a whole module's source, group-balance
// it, and pull out every top-level `define macro` form's body fragments,
// parsing each into a <macro-def>.
//
// Extraction shape. After `tokens-to-fragments`, a module's top-level
// fragment list is flat-with-nested-groups: every `{ … }` rule body is a
// <group-fragment>, so the `end` tokens INSIDE rule templates are nested
// and never appear at the top level. A `define macro` form therefore
// reads at top level as:
//
//   ident"define" ident"macro" ident NAME
//     { rule } => { rule }  [ ; ]  { rule } => { rule } …
//   kw-end"end" [ ident"macro" ] [ punct";" ]
//
// We scan for the `define macro NAME` head, collect body fragments up to
// the first top-level kw-end, hand them to `parse-macro-def`, then skip
// the `end [macro] [;]` tail. Non-macro top-level forms (`define
// function …`, etc.) are stepped over one fragment at a time; their own
// `end`s sit at top level but we only special-case `define macro`, so
// they are harmlessly skipped.

// Is `frags[i]` the head of a `define macro NAME` form?
define function define-macro-head? (frags :: <stretchy-vector>, i :: <integer>)
 => (yes? :: <boolean>)
  let n = size(frags);
  let result = #f;
  if (i + 2 < n)
    if (tok-is?(frags[i], #"ident", "define")
          & tok-is?(frags[i + 1], #"ident", "macro"))
      let third = frags[i + 2];
      if (tok-frag?(third))
        result := tok-kind(tfrag-tok(third)) = #"ident";
      end;
    end;
  end;
  result
end function;

// True iff `f` is a single-token fragment whose token kind is kw-end.
define function frag-kw-end? (f :: <fragment>) => (yes? :: <boolean>)
  if (tok-frag?(f))
    tok-kind(tfrag-tok(f)) = #"kw-end"
  else
    #f
  end
end function;

// Lex `source`, group-balance it, and return a <stretchy-vector> of
// every top-level `define macro` form parsed into a <macro-def>.
define function collect-macro-defs (source :: <byte-string>)
 => (defs :: <stretchy-vector>)
  let toks  = lex-source-to-toks(source);
  let frags = tokens-to-fragments(toks);
  let defs  = make(<stretchy-vector>);
  let n = size(frags);
  let i = 0;
  until (i >= n)
    if (define-macro-head?(frags, i))
      let name = tok-text(tfrag-tok(frags[i + 2]));
      // Collect the body: fragments after NAME up to (not including)
      // the first top-level kw-end.
      let body  = make(<stretchy-vector>);
      let j     = i + 3;
      let done? = #f;
      until (j >= n | done?)
        let f = frags[j];
        if (frag-kw-end?(f))
          done? := #t;
        else
          add!(body, f);
          j := j + 1;
        end;
      end;
      let def = parse-macro-def(name, body);
      add!(defs, def);
      // Skip the `end [macro] [;]` tail.
      let k = j;
      if (k < n & frag-kw-end?(frags[k]))         k := k + 1; end;
      if (k < n & tok-is?(frags[k], #"ident", "macro")) k := k + 1; end;
      if (k < n & tok-is?(frags[k], #"punct", ";"))     k := k + 1; end;
      i := k;
    else
      i := i + 1;
    end;
  end;
  defs
end function;
