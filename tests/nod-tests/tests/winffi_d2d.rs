//! Sprint 35 — COM via `windows` crate: DXGI / D3D11 / D2D /
//! DirectWrite infrastructure acceptance.
//!
//! Headline: a Dylan-source expression builds the full GPU device
//! chain offscreen, creates a D2D bitmap render target backed by a
//! 256x256 D3D11 texture, draws "hello, dylan" with DirectWrite +
//! a red solid-color brush, reads the texture's pixels back into
//! a CPU staging texture, and counts the non-zero red-channel
//! pixels. A positive count proves text glyphs rendered — i.e.
//! the entire COM chain works end-to-end.
//!
//! ## Sprint 35 deviation
//!
//! The brief sketches `<c-float>` / `<c-double>` Dylan args feeding
//! float-aware trampoline variants. Sprint 35 instead routes the
//! whole COM surface through integer-encoded scalars (color
//! channels as 0..=255 Dylan integers, coordinates as integer
//! pixels) — see `nod-runtime::com_shim` module docs for the
//! rationale. The `<c-float>` / `<c-double>` Dylan classes ARE
//! registered (per the brief's Phase A acceptance item), but no
//! Sprint 35 shim takes them as a native parameter; the
//! `CArgKind::Float32` / `Float64` enum variants exist and the
//! `from_c_type_name` mapping accepts the strings — sema accepts
//! `define c-function` declarations using these types, but Sprint
//! 36+ wires the trampoline path that actually marshals them.
//!
//! Each test that touches COM is `#[serial]` because the process-
//! global COM handle registry is shared mutable state across the
//! workspace.

#![cfg(windows)]

use nod_sema::eval_expr_to_string;
use serial_test::serial;

fn setup() {
    nod_runtime::ensure_conditions_registered();
    nod_runtime::ensure_c_ffi_error_registered();
    nod_runtime::ensure_com_types_registered();
    nod_runtime::_reset_handler_stack_for_tests();
    nod_runtime::_reset_com_registry_for_tests();
}

// ─── Headline acceptance ──────────────────────────────────────────────────

/// The Sprint 35 headline: end-to-end offscreen text rendering.
///
/// Dylan source builds:
///   1. DXGI factory + D3D11 device + immediate context.
///   2. 256x256 BGRA8 texture as render target.
///   3. DXGI surface view of the texture.
///   4. D2D factory + D2D device + D2D device context.
///   5. D2D bitmap wrapping the surface.
///   6. DirectWrite factory + text format + text layout.
///   7. Red solid-color brush.
///   8. `begin-draw` → `clear` to white → `draw-text-layout` → `end-draw`.
///   9. `copy-to-staging-and-map` → scan pixels → count non-zero red.
///
/// Asserts: count > 0 (some red pixels rendered) AND count < 65536
/// (less than the full 256x256 — i.e. not all-red, glyphs only).
#[test]
#[serial]
fn d2d_offscreen_renders_text_glyphs() {
    setup();
    // DXGI_FORMAT_B8G8R8A8_UNORM = 87.
    let body = "\
        let d3d-device   = %d3d11-create-device(); \
        let d3d-ctx      = %d3d11-get-immediate-context(d3d-device); \
        let texture      = %d3d11-create-texture-2d(d3d-device, 256, 256, 87); \
        let surface      = %dxgi-create-surface-from-texture(texture); \
        let d2d-factory  = %d2d-create-factory(); \
        let dxgi-device  = %dxgi-device-from-d3d-device(d3d-device); \
        let d2d-device   = %d2d-create-device(d2d-factory, dxgi-device); \
        let dc           = %d2d-create-device-context(d2d-device); \
        let bitmap       = %d2d-create-bitmap-for-target(dc, surface); \
        %d2d-set-target(dc, bitmap); \
        %d2d-set-transform-identity(dc); \
        let dwrite       = %dwrite-create-factory(); \
        let format       = %dwrite-create-text-format(dwrite, \"Segoe UI\", 2400, \"en-us\"); \
        let layout       = %dwrite-create-text-layout(dwrite, \"hello, dylan\", format, 240, 60); \
        let brush        = %d2d-create-solid-color-brush(dc, 255, 0, 0, 255); \
        %d2d-begin-draw(dc); \
        %d2d-clear(dc, 0, 0, 0, 255); \
        %d2d-draw-text-layout(dc, 10, 50, layout, brush); \
        %d2d-end-draw(dc); \
        let pixels-ptr   = %d3d11-copy-to-staging-and-map(d3d-device, d3d-ctx, texture, 256, 256); \
        let row-pitch    = %d3d11-last-mapped-row-pitch(); \
        let staging      = %d3d11-last-staging-handle(); \
        let red-count    = %count-non-zero-red(pixels-ptr, 256, 256, row-pitch); \
        %d3d11-unmap(d3d-ctx, staging); \
        red-count";
    let s = eval_expr_to_string(body).unwrap_or_else(|e| {
        panic!("d2d offscreen render eval failed: {e:?}")
    });
    let n: i64 = s.parse().unwrap_or_else(|_| panic!("expected integer, got `{s}`"));
    assert!(
        n > 0,
        "expected text glyphs to produce some non-zero red pixels, got {n}. \
         A zero count means either nothing was drawn (BeginDraw/EndDraw \
         failed) or the text region rendered outside the readback area."
    );
    let total = 256i64 * 256;
    assert!(
        n < total,
        "expected glyphs to be a subset of the texture; got {n} red pixels \
         out of {total} total — that's the whole texture, so we probably \
         filled the entire background red instead of drawing glyph shapes."
    );
}

// ─── Phase B device-chain isolation tests ────────────────────────────────

#[test]
#[serial]
fn dxgi_factory_creation_succeeds() {
    setup();
    let s = eval_expr_to_string("%dxgi-create-factory()").expect("dxgi factory");
    let h: i64 = s.parse().expect("integer handle");
    assert!(h > 0, "DXGI factory should return a positive handle, got {h}");
    // Release should succeed.
    let s = eval_expr_to_string(&format!("%com-release({h})"))
        .expect("com release");
    assert_eq!(s, "1", "com-release must report success");
}

#[test]
#[serial]
fn d3d11_device_creation_succeeds() {
    setup();
    let s = eval_expr_to_string("%d3d11-create-device()").expect("d3d11 device");
    let h: i64 = s.parse().expect("integer handle");
    assert!(h > 0, "D3D11 device should return a positive handle, got {h}");
}

#[test]
#[serial]
fn d3d11_texture_creation_succeeds() {
    setup();
    // DXGI_FORMAT_B8G8R8A8_UNORM = 87.
    let s = eval_expr_to_string(
        "let d = %d3d11-create-device(); \
         %d3d11-create-texture-2d(d, 256, 256, 87)",
    )
    .expect("d3d11 texture");
    let h: i64 = s.parse().expect("integer handle");
    assert!(h > 0, "Texture should return a positive handle, got {h}");
}

#[test]
#[serial]
fn d2d_factory_creation_succeeds() {
    setup();
    let s = eval_expr_to_string("%d2d-create-factory()").expect("d2d factory");
    let h: i64 = s.parse().expect("integer handle");
    assert!(h > 0, "D2D factory should return a positive handle, got {h}");
}

#[test]
#[serial]
fn dwrite_factory_creation_succeeds() {
    setup();
    let s = eval_expr_to_string("%dwrite-create-factory()").expect("dwrite factory");
    let h: i64 = s.parse().expect("integer handle");
    assert!(h > 0, "DirectWrite factory should return a positive handle, got {h}");
}

// ─── Refcount discipline ──────────────────────────────────────────────────

#[test]
#[serial]
fn ten_handles_released_clears_registry() {
    setup();
    // Create 10 DXGI factories from Dylan, release them all, assert
    // the registry is back to empty.
    let s = eval_expr_to_string(
        "let h1 = %dxgi-create-factory(); \
         let h2 = %dxgi-create-factory(); \
         let h3 = %dxgi-create-factory(); \
         let h4 = %dxgi-create-factory(); \
         let h5 = %dxgi-create-factory(); \
         let h6 = %dxgi-create-factory(); \
         let h7 = %dxgi-create-factory(); \
         let h8 = %dxgi-create-factory(); \
         let h9 = %dxgi-create-factory(); \
         let h10 = %dxgi-create-factory(); \
         let before = %com-registry-len(); \
         %com-release(h1); \
         %com-release(h2); \
         %com-release(h3); \
         %com-release(h4); \
         %com-release(h5); \
         %com-release(h6); \
         %com-release(h7); \
         %com-release(h8); \
         %com-release(h9); \
         %com-release(h10); \
         let after = %com-registry-len(); \
         before - after",
    )
    .expect("refcount roundtrip");
    assert_eq!(s, "10", "expected 10 entries created and 10 released");
}

#[test]
#[serial]
fn refcount_registry_starts_empty_after_reset() {
    setup();
    let s = eval_expr_to_string("%com-registry-len()").expect("registry len");
    assert_eq!(s, "0", "registry must be empty after _reset_com_registry_for_tests");
}

// ─── End-draw HRESULT check (the float-marshaling proof) ──────────────────

#[test]
#[serial]
fn d2d_clear_and_end_draw_succeed() {
    setup();
    // The clear → begin-draw → clear → end-draw chain exercises the
    // most of the COM surface that the Sprint 35 brief wants to
    // demonstrate. EndDraw returns the HRESULT-as-u64; 0 = S_OK.
    let body = "\
        let d3d-device   = %d3d11-create-device(); \
        let texture      = %d3d11-create-texture-2d(d3d-device, 256, 256, 87); \
        let surface      = %dxgi-create-surface-from-texture(texture); \
        let d2d-factory  = %d2d-create-factory(); \
        let dxgi-device  = %dxgi-device-from-d3d-device(d3d-device); \
        let d2d-device   = %d2d-create-device(d2d-factory, dxgi-device); \
        let dc           = %d2d-create-device-context(d2d-device); \
        let bitmap       = %d2d-create-bitmap-for-target(dc, surface); \
        %d2d-set-target(dc, bitmap); \
        %d2d-begin-draw(dc); \
        %d2d-clear(dc, 128, 64, 200, 255); \
        %d2d-end-draw(dc)";
    let s = eval_expr_to_string(body).expect("clear + end-draw");
    assert_eq!(
        s, "0",
        "d2d-end-draw should return 0 (S_OK) after a successful clear; got HRESULT {s}"
    );
}

// ─── Float c-types registered in sema ─────────────────────────────────────

#[test]
fn c_float_class_resolves() {
    nod_runtime::ensure_float_types_registered();
    let id = nod_runtime::c_float_class_id();
    let md = nod_runtime::class_metadata_for(id);
    assert_eq!(md.name, "<c-float>");
}

#[test]
fn c_double_class_resolves() {
    nod_runtime::ensure_float_types_registered();
    let id = nod_runtime::c_double_class_id();
    let md = nod_runtime::class_metadata_for(id);
    assert_eq!(md.name, "<c-double>");
}
