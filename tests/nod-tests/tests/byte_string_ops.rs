//! Sprint 42a — `<byte-string>` operations end-to-end tests.
//!
//! Verifies the stdlib methods built on the five `%byte-string-*`
//! primitives produce correct results when called from user code.
//! Also exercises the generic `=` dispatch (Phase B) so two
//! distinct-allocation byte-strings with the same bytes compare
//! equal, and `<byte-string>` as a `<table>` key (covers the
//! object-hash ↔ \= invariant — Sprint 22 already wired the hash,
//! Sprint 42a adds the working `=` method).
//!
//! All tests `#[serial]` because the runtime's class registry,
//! literal pool, dispatch table, and table-hash machinery are
//! process-global.

use serial_test::serial;

fn setup() {
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::ensure_collections_registered();
    nod_runtime::ensure_tables_registered();
}

// ─── size + element + empty? ──────────────────────────────────────────────

#[test]
#[serial]
fn size_of_byte_string_literal_is_byte_count() {
    setup();
    let s = nod_sema::eval_expr_to_string("size(\"hello\")").expect("eval");
    assert_eq!(s, "5");
}

#[test]
#[serial]
fn size_of_empty_byte_string_is_zero() {
    setup();
    let s = nod_sema::eval_expr_to_string("size(\"\")").expect("eval");
    assert_eq!(s, "0");
}

// NB: `empty?(byte-string)` would be a natural test here, but the
// Sprint 16 lower-time shortcut in `nod-sema/src/lower.rs` intercepts
// `empty?(x)` and lowers it to a list-specific primitive without
// going through generic dispatch. Until that shortcut is retired
// (DEFERRED.md → Sprint 16 list-builtins → stdlib generics), no
// stdlib method on `empty?` is reachable. Use `size(s) = 0` instead.
#[test]
#[serial]
fn size_zero_idiom_replaces_empty_predicate_on_byte_strings() {
    setup();
    let yes = nod_sema::eval_expr_to_string("size(\"\") = 0").expect("eval");
    assert_eq!(yes, "#t");
    let no = nod_sema::eval_expr_to_string("size(\"x\") = 0").expect("eval");
    assert_eq!(no, "#f");
}

#[test]
#[serial]
fn element_returns_byte_as_integer() {
    setup();
    // 'h' = 104, 'e' = 101, 'l' = 108, 'o' = 111
    let s = nod_sema::eval_expr_to_string("element(\"hello\", 0)").expect("eval");
    assert_eq!(s, "104");
    let s = nod_sema::eval_expr_to_string("element(\"hello\", 4)").expect("eval");
    assert_eq!(s, "111");
}

// ─── concatenate ──────────────────────────────────────────────────────────

#[test]
#[serial]
fn concatenate_two_byte_strings_returns_combined_bytes() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "size(concatenate(\"hello, \", \"world\"))"
    ).expect("eval");
    assert_eq!(s, "12");
    // Confirm bytes are correct via element access on the result.
    let h = nod_sema::eval_expr_to_string(
        "element(concatenate(\"a\", \"bc\"), 1)"  // 'b' = 98
    ).expect("eval");
    assert_eq!(h, "98");
}

// ─── = (Sprint 42a Phase B universal dispatch) ────────────────────────────

#[test]
#[serial]
fn equal_byte_strings_compare_equal_even_across_allocations() {
    setup();
    // Concatenation forces a fresh heap allocation distinct from the
    // literal-pool-interned "ab". If `=` were pointer-only this would
    // return #f.
    let s = nod_sema::eval_expr_to_string(
        "concatenate(\"a\", \"b\") = \"ab\""
    ).expect("eval");
    assert_eq!(s, "#t");
}

#[test]
#[serial]
fn unequal_byte_strings_compare_unequal() {
    setup();
    let s = nod_sema::eval_expr_to_string("\"abc\" = \"abd\"").expect("eval");
    assert_eq!(s, "#f");
}

#[test]
#[serial]
fn empty_byte_strings_compare_equal() {
    setup();
    let s = nod_sema::eval_expr_to_string("\"\" = \"\"").expect("eval");
    assert_eq!(s, "#t");
}

// ─── starts-with? / ends-with? ────────────────────────────────────────────

#[test]
#[serial]
fn starts_with_matches_prefix() {
    setup();
    let yes = nod_sema::eval_expr_to_string(
        "starts-with?(\"hello, world\", \"hello\")"
    ).expect("eval");
    assert_eq!(yes, "#t");
    let no = nod_sema::eval_expr_to_string(
        "starts-with?(\"hello, world\", \"world\")"
    ).expect("eval");
    assert_eq!(no, "#f");
}

#[test]
#[serial]
fn starts_with_empty_prefix_always_matches() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "starts-with?(\"anything\", \"\")"
    ).expect("eval");
    assert_eq!(s, "#t");
}

#[test]
#[serial]
fn ends_with_matches_suffix() {
    setup();
    let yes = nod_sema::eval_expr_to_string(
        "ends-with?(\"hello, world\", \"world\")"
    ).expect("eval");
    assert_eq!(yes, "#t");
    let no = nod_sema::eval_expr_to_string(
        "ends-with?(\"hello, world\", \"hello\")"
    ).expect("eval");
    assert_eq!(no, "#f");
}

// ─── find-substring ───────────────────────────────────────────────────────

#[test]
#[serial]
fn find_substring_hit_returns_position() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "find-substring(\"hello, world\", \"world\")"
    ).expect("eval");
    assert_eq!(s, "7");
}

#[test]
#[serial]
fn find_substring_miss_returns_false() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "find-substring(\"hello\", \"xyz\")"
    ).expect("eval");
    assert_eq!(s, "#f");
}

// ─── copy-sequence / subsequence ──────────────────────────────────────────

#[test]
#[serial]
fn copy_sequence_full_returns_byte_equal_copy() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "copy-sequence(\"hello\") = \"hello\""
    ).expect("eval");
    assert_eq!(s, "#t");
}

#[test]
#[serial]
fn copy_sequence_with_bounds_returns_substring() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "copy-sequence(\"hello, world\", 7, 12) = \"world\""
    ).expect("eval");
    assert_eq!(s, "#t");
}

// ─── as-uppercase / as-lowercase (ASCII) ──────────────────────────────────

#[test]
#[serial]
fn as_uppercase_ascii_uppercases_letters() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "as-uppercase(\"hello\") = \"HELLO\""
    ).expect("eval");
    assert_eq!(s, "#t");
}

#[test]
#[serial]
fn as_uppercase_leaves_non_letters_unchanged() {
    setup();
    let s = nod_sema::eval_expr_to_string(
        "as-uppercase(\"Hello, World!\") = \"HELLO, WORLD!\""
    ).expect("eval");
    assert_eq!(s, "#t");
}

// ─── byte-string as table key (= ↔ object-hash invariant) ─────────────────

#[test]
#[serial]
fn byte_string_works_as_table_key_across_allocations() {
    setup();
    // Insert under "hello" (literal), look up under a freshly-allocated
    // byte-string with the same bytes — must find the value because
    // object-hash is content-based (Sprint 22) and `=` is content-based
    // (Sprint 42a).
    let s = nod_sema::eval_expr_to_string(
        "let t = make(<table>); \
         t[\"hello\"] := 42; \
         t[concatenate(\"hel\", \"lo\")]"
    ).expect("eval");
    assert_eq!(s, "42");
}
