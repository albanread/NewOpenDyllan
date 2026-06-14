//! **Platform-specific module — Windows-only.** See
//! `docs/PLATFORMS.md`. This is one of the named modules that hosts
//! platform-specific code so the rest of `nod-runtime` stays portable.
//! The macOS variant will ship its own `cocoa_shim.rs` analogue
//! (Cocoa/UIKit + Metal + Core Text) covering the same role.
//!
//! Sprint 35 — COM shim for DXGI / D3D11 / D2D / DirectWrite via the
//! `windows` crate.
//!
//! ## Architecture
//!
//! Dylan sees a process-global registry that hands out opaque `u64`
//! handle tokens (treated as `<c-handle>` on the Dylan side). Each
//! token maps to a [`ComObject`] enum variant carrying a typed
//! `windows`-crate COM interface. Cloning a `windows` crate COM type
//! bumps the underlying object's refcount via `AddRef`; dropping it
//! calls `Release`. The registry's `HashMap<u64, ComObject>` owns
//! exactly one reference per Dylan-held handle; removing the entry
//! drops the inner enum, which calls `Release`.
//!
//! Calls like `nod_d2d_draw_text_layout(dc_handle, x, y, layout_handle,
//! brush_handle)` are regular `extern "C-unwind"` Rust functions:
//! they look up each handle via the registry, clone the typed
//! interface (cheap — atomic refcount increment), and call the
//! `windows` crate methods directly. Errors are surfaced as
//! `<c-ffi-error>` Dylan conditions whose `os-error-code` slot
//! carries the underlying HRESULT.
//!
//! ## Sprint 35 scope
//!
//! Offscreen only — no HWND, no swap chain, no actual presentation.
//! The acceptance test creates an `ID3D11Texture2D` of 256×256 ARGB
//! as the render target, sets a D2D bitmap on it, draws "hello, dylan"
//! with DirectWrite, then maps a CPU-readable staging texture to read
//! the pixels back and assert text glyphs rendered (count of non-zero
//! red pixels in the expected text region).
//!
//! ## Refcount discipline
//!
//! The `windows` crate's COM types are `Clone + Drop`. The registry
//! takes ownership of each created object; calling `nod_com_release`
//! removes the entry, which drops the typed wrapper, which calls
//! `Release` on the underlying COM object. We do not manually call
//! `AddRef` or `Release`. This is the entire lifetime story.
//!
//! ## Float marshaling caveat (Sprint 35 deviation)
//!
//! The brief sketches `<c-float>` / `<c-double>` Dylan types feeding
//! float-aware trampoline variants. To avoid restructuring the
//! Sprint 28 trampolines (which use `extern "system" fn(u64, u64, …)`
//! integer-only signatures — adding f32 args in mid-call-frame
//! positions changes Win64 register classes), Sprint 35 instead
//! accepts integer-encoded scalars for the shim layer:
//!
//! - Color channels: 0..=255 Dylan integers, mapped to 0.0..=1.0 f32.
//! - Pixel coordinates: integer Dylan values, cast to f32 inside the
//!   shim.
//! - DirectWrite font size: integer Dylan value, cast to f32.
//!
//! The `<c-float>` / `<c-double>` Dylan classes are still registered
//! (Sprint 35 Phase A acceptance item) and the `Float32` / `Float64`
//! variants exist in `CArgKind` / `CReturnKind`, but no Sprint 35
//! shim function currently takes them as parameters. Sprint 36+ wires
//! float-marshaling trampolines once a use case requires native
//! Dylan-source float literals to reach a D2D API directly.
//!
//! This is a documented deviation; the IDE pixels-on-screen story
//! doesn't need fractional pixel positions in Sprint 35.
//!
//! ## Process-global state
//!
//! The handle registry is one `Mutex<HashMap<u64, ComObject>>`. Every
//! test that touches COM must run with `#[serial]` because handles
//! leak across tests if not explicitly released, and the monotonic
//! counter is shared.

#![cfg(windows)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_POINT_2F, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_TARGET,
    D2D1_BITMAP_PROPERTIES1, D2D1_BRUSH_PROPERTIES, D2D1_DEVICE_CONTEXT_OPTIONS_NONE,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED, ID2D1Bitmap1, ID2D1Device,
    ID2D1DeviceContext, ID2D1Factory1, ID2D1RenderTarget, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
    IDWriteTextLayout,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, DXGI_CREATE_FACTORY_FLAGS, DXGI_SCALING_NONE, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
    IDXGIDevice, IDXGIFactory2, IDXGISurface, IDXGISwapChain1,
};
use windows::core::Interface;

// ─── Fixnum-tag helper ────────────────────────────────────────────────────

/// Tag a raw u64 (a COM handle, a count, a pointer-as-integer, an
/// HRESULT, …) as a Dylan fixnum Word so the value round-trips
/// through the JIT-emitted primitive call → Dylan `<integer>` →
/// callsite path. JIT-emitted primitive calls return the shim's raw
/// u64 directly as the result Word; for that to mean
/// "Dylan integer N", the bits must be `N << 1`.
///
/// We use the low 63 bits as the integer value (matching
/// `Word::fixnum_unchecked`'s 63-bit range), which is sufficient for
/// every Sprint 35 use case: handle IDs are monotonically allocated
/// counters that won't approach 2^62, HRESULTs fit in 32 bits, and
/// pixel counts cap at 256*256 = 65536.
#[inline]
fn tag(n: u64) -> u64 {
    crate::word::Word::fixnum_unchecked(n as i64).raw()
}

/// Inverse of [`tag`] — strip the fixnum tag bit. Used by shims that
/// receive Dylan-side `<integer>` args (i.e. every COM-shim arg under
/// Sprint 35's integer-encoded convention).
#[inline]
fn untag(w: u64) -> u64 {
    crate::word::Word::from_raw(w).as_fixnum().unwrap_or(0) as u64
}

/// Inverse for signed coordinate args (negative integers).
#[inline]
fn untag_i64(w: u64) -> i64 {
    crate::word::Word::from_raw(w).as_fixnum().unwrap_or(0)
}

// ─── Registry ──────────────────────────────────────────────────────────────

/// One entry in the COM handle registry — owns exactly one reference
/// to the underlying COM object via the `windows` crate's typed
/// wrapper. Dropping the variant calls `Release` on the inner object.
#[allow(clippy::large_enum_variant)]
pub enum ComObject {
    DxgiFactory(IDXGIFactory2),
    DxgiDevice(IDXGIDevice),
    DxgiSurface(IDXGISurface),
    /// Sprint 36 — `IDXGISwapChain1` for HWND-bound presentation.
    DxgiSwapChain(IDXGISwapChain1),
    D3D11Device(ID3D11Device),
    D3D11DeviceContext(ID3D11DeviceContext),
    D3D11Texture2D(ID3D11Texture2D),
    D2DFactory(ID2D1Factory1),
    D2DDevice(ID2D1Device),
    D2DDeviceContext(ID2D1DeviceContext),
    D2DBitmap(ID2D1Bitmap1),
    D2DSolidColorBrush(ID2D1SolidColorBrush),
    DWriteFactory(IDWriteFactory),
    DWriteTextFormat(IDWriteTextFormat),
    DWriteTextLayout(IDWriteTextLayout),
}

static REGISTRY: OnceLock<Mutex<HashMap<u64, ComObject>>> = OnceLock::new();
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

fn registry() -> &'static Mutex<HashMap<u64, ComObject>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Allocate a fresh handle and insert `obj` into the process-global
/// registry. Returns the handle u64 — Dylan treats this as a
/// `<c-handle>` opaque token.
pub fn register(obj: ComObject) -> u64 {
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    let mut g = registry().lock().expect("com_shim registry poisoned");
    g.insert(handle, obj);
    handle
}

/// Remove the entry for `handle` from the registry; the inner COM
/// object's `Release` runs on drop. Returns `true` if the handle was
/// present, `false` if already released / never registered.
pub fn release(handle: u64) -> bool {
    let mut g = registry().lock().expect("com_shim registry poisoned");
    g.remove(&handle).is_some()
}

/// Diagnostic: how many entries are live in the registry. Used by
/// the refcount-discipline test.
pub fn registry_len() -> usize {
    let g = registry().lock().expect("com_shim registry poisoned");
    g.len()
}

/// Doc-hidden test reset — drop every registered COM object. Used by
/// the refcount-discipline test to start from a known-empty state.
#[doc(hidden)]
pub fn _reset_registry_for_tests() {
    let mut g = registry().lock().expect("com_shim registry poisoned");
    g.clear();
}

// ─── Typed accessors ──────────────────────────────────────────────────────
//
// Each accessor takes a u64 handle, looks up the entry, and returns
// a clone (refcount bump) of the typed interface. `None` if the
// handle isn't of the expected variant. The clone gives the caller
// an owned reference whose Drop calls Release independently of the
// registry's reference.

macro_rules! typed_accessor {
    ($name:ident, $variant:ident, $ty:ty) => {
        /// Look up a registered COM object by Dylan-side `<c-handle>`
        /// (a fixnum-tagged Word). The lookup untags the input and
        /// returns `None` if the handle is unknown or of the wrong
        /// variant. The returned reference is a fresh clone (refcount
        /// bump) so the caller has independent lifetime from the
        /// registry's reference.
        pub fn $name(handle: u64) -> Option<$ty> {
            let h = untag(handle);
            if h == 0 {
                return None;
            }
            let g = registry().lock().expect("com_shim registry poisoned");
            match g.get(&h)? {
                ComObject::$variant(x) => Some(x.clone()),
                _ => None,
            }
        }
    };
}

typed_accessor!(get_dxgi_factory, DxgiFactory, IDXGIFactory2);
typed_accessor!(get_dxgi_device, DxgiDevice, IDXGIDevice);
typed_accessor!(get_dxgi_surface, DxgiSurface, IDXGISurface);
typed_accessor!(get_dxgi_swap_chain, DxgiSwapChain, IDXGISwapChain1);
typed_accessor!(get_d3d11_device, D3D11Device, ID3D11Device);
typed_accessor!(get_d3d11_device_context, D3D11DeviceContext, ID3D11DeviceContext);
typed_accessor!(get_d3d11_texture_2d, D3D11Texture2D, ID3D11Texture2D);
typed_accessor!(get_d2d_factory, D2DFactory, ID2D1Factory1);
typed_accessor!(get_d2d_device, D2DDevice, ID2D1Device);
typed_accessor!(get_d2d_device_context, D2DDeviceContext, ID2D1DeviceContext);
typed_accessor!(get_d2d_bitmap, D2DBitmap, ID2D1Bitmap1);
typed_accessor!(get_d2d_solid_brush, D2DSolidColorBrush, ID2D1SolidColorBrush);
typed_accessor!(get_dwrite_factory, DWriteFactory, IDWriteFactory);
typed_accessor!(get_dwrite_text_format, DWriteTextFormat, IDWriteTextFormat);
typed_accessor!(get_dwrite_text_layout, DWriteTextLayout, IDWriteTextLayout);

// ─── HRESULT → Dylan error helper ─────────────────────────────────────────

/// Surface a `windows::core::Error` as the integer HRESULT for the
/// shim's u64 return channel. Sprint 35 uses a sentinel return value
/// pattern: shim functions return 0 on success-with-handle-encoded
/// or actual_handle, and on error return 0 with the HRESULT
/// surfaced via thread-local context (Sprint 36 will integrate this
/// with the `<c-ffi-error>` raise path). For Sprint 35's acceptance
/// test, success returns a positive handle; failure returns 0.
fn hresult_to_zero<T>(r: windows::core::Result<T>) -> Option<T> {
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            // SAFETY: side-effect-free — store the last HRESULT for
            // diagnostics. Sprint 35 keeps this minimal; Sprint 36
            // raises a `<c-ffi-error>` Dylan condition via the
            // Sprint 19 signal path.
            store_last_hresult(e.code().0);
            None
        }
    }
}

static LAST_HRESULT: AtomicU64 = AtomicU64::new(0);

fn store_last_hresult(code: i32) {
    LAST_HRESULT.store(code as u32 as u64, Ordering::Relaxed);
}

/// JIT-callable: read the most recent HRESULT seen by a shim
/// function. Returns 0 (S_OK) if no error has occurred since the
/// last call to this function or `nod_com_clear_last_hresult`.
///
/// # Safety
/// No unsafe operations; declared `unsafe extern "C-unwind"` to
/// match the rest of the JIT-callable surface.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_com_last_hresult() -> u64 {
    tag(LAST_HRESULT.load(Ordering::Relaxed))
}

/// JIT-callable: clear the last-HRESULT slot back to 0.
///
/// # Safety
/// No unsafe operations.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_com_clear_last_hresult() -> u64 {
    LAST_HRESULT.store(0, Ordering::Relaxed);
    0
}

// ─── Registry-level JIT entries ───────────────────────────────────────────

/// JIT-callable: drop a Dylan-held COM handle. Returns 1 if the
/// handle was present (and its underlying COM object's `Release`
/// ran), 0 if the handle was unknown.
///
/// # Safety
/// No unsafe operations beyond the FFI boundary; the underlying
/// HashMap is mutex-protected.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_com_release(handle: u64) -> u64 {
    let h = untag(handle);
    if h != 0 && release(h) { tag(1) } else { 0 }
}

/// JIT-callable: number of live registry entries. Diagnostics.
///
/// # Safety
/// No unsafe operations.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_com_registry_len() -> u64 {
    tag(registry_len() as u64)
}

// ─── Phase B — DXGI + D3D11 device chain ─────────────────────────────────

/// JIT-callable: create the process's DXGI factory. Returns the
/// registry handle or 0 on failure (HRESULT stored in
/// `LAST_HRESULT`).
///
/// # Safety
/// Calls `CreateDXGIFactory2`, which has no Rust-side preconditions
/// beyond a sane process state.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_dxgi_create_factory() -> u64 {
    // SAFETY: CreateDXGIFactory2 is a stateless Win32 API. The
    // returned factory is fully owned by the typed wrapper.
    let r: windows::core::Result<IDXGIFactory2> =
        unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)) };
    match hresult_to_zero(r) {
        Some(f) => tag(register(ComObject::DxgiFactory(f))),
        None => 0,
    }
}

/// JIT-callable: create a D3D11 device with BGRA support (required
/// for D2D interop). Tries hardware first, falls back to WARP
/// (software). Returns the device handle or 0 on failure.
///
/// # Safety
/// Calls `D3D11CreateDevice`, whose contract requires aligned out-
/// pointers (which the `windows` crate handles).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d3d11_create_device() -> u64 {
    // Try the hardware driver first; fall back to WARP for CI hosts
    // or VMs without a GPU. Either path produces a device that D2D
    // can use as long as BGRA_SUPPORT is enabled.
    for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        // SAFETY: D3D11CreateDevice fills the out-params on success;
        // on failure the Rust wrapper preserves the Option as None.
        let r = unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        };
        if r.is_ok()
            && let Some(d) = device
        {
            // Stash the immediate context alongside if we got one,
            // but don't return it from here — callers ask via
            // `nod_d3d11_get_immediate_context`. Drop the local
            // Option<ID3D11DeviceContext> here; the device retains
            // its own reference to the immediate context.
            drop(context);
            return tag(register(ComObject::D3D11Device(d)));
        }
        if let Err(e) = r {
            store_last_hresult(e.code().0);
        }
    }
    0
}

/// JIT-callable: fetch the device's immediate context as a fresh
/// handle. The device retains its own reference; this clones into
/// the registry.
///
/// # Safety
/// `device_handle` must be a previously-registered D3D11Device handle
/// (or 0, which returns 0).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d3d11_get_immediate_context(device_handle: u64) -> u64 {
    let Some(device) = get_d3d11_device(device_handle) else {
        return 0;
    };
    // SAFETY: device is a valid ID3D11Device.
    let r: windows::core::Result<ID3D11DeviceContext> = unsafe { device.GetImmediateContext() };
    match hresult_to_zero(r) {
        Some(c) => tag(register(ComObject::D3D11DeviceContext(c))),
        None => 0,
    }
}

/// JIT-callable: create a 2D texture suitable as a D2D render
/// target. `format` is a DXGI_FORMAT enum value (e.g. 87 =
/// DXGI_FORMAT_B8G8R8A8_UNORM, which D2D requires). The texture is
/// USAGE_DEFAULT with BIND_RENDER_TARGET | BIND_SHADER_RESOURCE.
///
/// # Safety
/// `device_handle` must be a valid D3D11Device handle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d3d11_create_texture_2d(
    device_handle: u64,
    width: u64,
    height: u64,
    format: u64,
) -> u64 {
    let Some(device) = get_d3d11_device(device_handle) else {
        return 0;
    };
    let width = untag(width);
    let height = untag(height);
    let format = untag(format);
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width as u32,
        Height: height as u32,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT(format as i32),
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut tex: Option<ID3D11Texture2D> = None;
    // SAFETY: desc fully populated; out-param filled by D3D11.
    let r = unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) };
    if let Err(e) = r {
        store_last_hresult(e.code().0);
        return 0;
    }
    match tex {
        Some(t) => tag(register(ComObject::D3D11Texture2D(t))),
        None => 0,
    }
}

/// JIT-callable: query a registered D3D11 texture for its
/// IDXGISurface interface (the D2D-friendly view). Returns the
/// surface handle or 0 on failure.
///
/// # Safety
/// `texture_handle` must be a valid D3D11Texture2D handle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_dxgi_create_surface_from_texture(texture_handle: u64) -> u64 {
    let Some(tex) = get_d3d11_texture_2d(texture_handle) else {
        return 0;
    };
    // Cast the texture into IDXGISurface — same underlying COM
    // object, different interface vtable.
    let r: windows::core::Result<IDXGISurface> = tex.cast();
    match hresult_to_zero(r) {
        Some(s) => tag(register(ComObject::DxgiSurface(s))),
        None => 0,
    }
}

/// JIT-callable: query a registered D3D11 device for its
/// IDXGIDevice interface. Required for the D2D device-creation step
/// (`ID2D1Factory1::CreateDevice` takes an IDXGIDevice).
///
/// # Safety
/// `device_handle` must be a valid D3D11Device handle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_dxgi_device_from_d3d_device(device_handle: u64) -> u64 {
    let Some(device) = get_d3d11_device(device_handle) else {
        return 0;
    };
    let r: windows::core::Result<IDXGIDevice> = device.cast();
    match hresult_to_zero(r) {
        Some(d) => tag(register(ComObject::DxgiDevice(d))),
        None => 0,
    }
}

// ─── Sprint 36 — HWND-bound swap chain (IDXGISwapChain1) ─────────────────
//
// The Sprint 35 acceptance proved we can render offscreen into a D3D11
// texture. Sprint 36 adds the missing presentation pieces:
//   * `nod_dxgi_factory_from_d3d_device` — fetches the DXGI factory
//     that the existing D3D11 device was created against. We can't
//     just call `CreateDXGIFactory2` a second time because the swap
//     chain must be created via the same factory the device's adapter
//     came from, or Present silently no-ops.
//   * `nod_dxgi_create_swap_chain_for_hwnd` — binds a swap chain to a
//     window handle so DXGI presents into the window's client area.
//   * `nod_d2d_create_bitmap_from_swap_chain` — wraps the swap chain's
//     back-buffer surface as a D2D bitmap so the existing D2D draw
//     pipeline can target it.
//   * `nod_dxgi_swap_chain_present` — pushes the back buffer to the
//     screen. Called from WM_PAINT.
//   * `nod_dxgi_swap_chain_resize_buffers` — handles WM_SIZE. Caller
//     must release the bitmap targeting the back buffer first
//     (D2D::SetTarget(None)).

/// JIT-callable: derive the DXGI factory associated with an existing
/// D3D11 device. Walks D3D11Device → IDXGIDevice → IDXGIAdapter →
/// IDXGIFactory2 (the parent of the adapter). Registers the factory
/// in the COM registry; the caller can release with `nod_com_release`
/// once swap-chain creation is done.
///
/// # Safety
/// `device_handle` must be a valid D3D11Device handle (or 0, which
/// returns 0).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_dxgi_factory_from_d3d_device(device_handle: u64) -> u64 {
    let Some(device) = get_d3d11_device(device_handle) else {
        return 0;
    };
    // D3D11Device → IDXGIDevice (cast/QueryInterface).
    let dxgi_dev: windows::core::Result<IDXGIDevice> = device.cast();
    let Some(dxgi_dev) = hresult_to_zero(dxgi_dev) else {
        return 0;
    };
    // IDXGIDevice → IDXGIAdapter → IDXGIFactory2. Both calls are
    // `unsafe` because they fetch raw COM out-pointers via the
    // `windows` crate's reified-pointer API.
    // SAFETY: dxgi_dev is a valid IDXGIDevice; GetAdapter writes the
    // out-pointer on success.
    let adapter = match unsafe { dxgi_dev.GetAdapter() } {
        Ok(a) => a,
        Err(e) => {
            store_last_hresult(e.code().0);
            return 0;
        }
    };
    // SAFETY: adapter valid; GetParent for IDXGIFactory2 returns the
    // factory that produced this adapter.
    let factory: windows::core::Result<IDXGIFactory2> = unsafe { adapter.GetParent() };
    match hresult_to_zero(factory) {
        Some(f) => tag(register(ComObject::DxgiFactory(f))),
        None => 0,
    }
}

/// JIT-callable: create an HWND-bound swap chain (`IDXGISwapChain1`)
/// using the given DXGI factory and D3D11 device, at the given
/// dimensions. Sprint 36 uses BGRA8_UNORM with 2 buffers and
/// FLIP_SEQUENTIAL — the modern (DXGI 1.2+) presentation model.
///
/// `hwnd_word` is a fixnum-tagged Word carrying the HWND value the
/// `CreateWindowExW` shim returned (Sprint 28's `<c-handle>` ABI). On
/// failure returns 0; HRESULT in `LAST_HRESULT`.
///
/// # Safety
/// All handles must be of matching variants. `hwnd_word` must encode
/// a real HWND (Win32 validates this).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_dxgi_create_swap_chain_for_hwnd(
    factory_handle: u64,
    device_handle: u64,
    hwnd_word: u64,
    width: u64,
    height: u64,
) -> u64 {
    let Some(factory) = get_dxgi_factory(factory_handle) else {
        return 0;
    };
    let Some(device) = get_d3d11_device(device_handle) else {
        return 0;
    };
    // HWND is a sign-extended fixnum; treat the bit pattern as a
    // pointer-width word.
    let hwnd_raw = untag_i64(hwnd_word);
    if hwnd_raw == 0 {
        return 0;
    }
    let hwnd_h = HWND(hwnd_raw as *mut std::ffi::c_void);
    let w = untag(width) as u32;
    let h = untag(height) as u32;
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: w,
        Height: h,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        Stereo: false.into(),
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        // Sprint 41c — Scaling: NONE (not STRETCH). With STRETCH, when the
        // user drags the window edge, the OS scales the back buffer to
        // fill the new client area until ResizeBuffers updates the
        // dimensions — text gets visually stretched mid-drag. With NONE,
        // the back buffer stays at its native size, top-left anchored;
        // the OS fills the newly-exposed region with the window's
        // background brush. NONE requires a flip-model swap chain
        // (FLIP_SEQUENTIAL or FLIP_DISCARD), which we're already using.
        Scaling: DXGI_SCALING_NONE,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
        Flags: 0,
    };
    // SAFETY: factory + device + hwnd valid; desc on stack.
    let r: windows::core::Result<IDXGISwapChain1> = unsafe {
        factory.CreateSwapChainForHwnd(&device, hwnd_h, &desc, None, None)
    };
    match hresult_to_zero(r) {
        Some(sc) => tag(register(ComObject::DxgiSwapChain(sc))),
        None => 0,
    }
}

/// JIT-callable: wrap the swap chain's back-buffer surface as a D2D
/// bitmap, suitable as the target for a D2D device context. Returns
/// the bitmap handle or 0. This is the Sprint 36 equivalent of
/// Sprint 35's `nod_d2d_create_bitmap_for_target` (which took an
/// offscreen `IDXGISurface`); the bitmap shares memory with the swap
/// chain back buffer, so drawing into it puts pixels in front of
/// `Present`.
///
/// # Safety
/// Both handles must be of matching variants.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d2d_create_bitmap_from_swap_chain(
    dc_handle: u64,
    sc_handle: u64,
) -> u64 {
    let Some(dc) = get_d2d_device_context(dc_handle) else {
        return 0;
    };
    let Some(sc) = get_dxgi_swap_chain(sc_handle) else {
        return 0;
    };
    // SAFETY: sc valid; GetBuffer fetches buffer 0 (the back buffer).
    let surface: windows::core::Result<IDXGISurface> = unsafe { sc.GetBuffer(0) };
    let Some(surface) = hresult_to_zero(surface) else {
        return 0;
    };
    // Same bitmap-properties shape Sprint 35's `create_bitmap_for_target`
    // uses: target-capable, can't-draw-from (it's a render target
    // only), DPI 96. For HWND presentation we use ALPHA_MODE_IGNORE
    // to match the swap-chain alpha mode set in
    // `_create_swap_chain_for_hwnd`.
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: windows::Win32::Graphics::Direct2D::Common::D2D1_ALPHA_MODE_IGNORE,
        },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
        colorContext: std::mem::ManuallyDrop::new(None),
    };
    // SAFETY: surface + props valid for the call duration.
    let r: windows::core::Result<ID2D1Bitmap1> =
        unsafe { dc.CreateBitmapFromDxgiSurface(&surface, Some(&props)) };
    match hresult_to_zero(r) {
        Some(b) => tag(register(ComObject::D2DBitmap(b))),
        None => 0,
    }
}

/// JIT-callable: present the swap chain's back buffer. SyncInterval=1
/// (VSync), Flags=0. Returns 0 on success; HRESULT-as-u64 on failure.
///
/// # Safety
/// `sc_handle` must be a valid DxgiSwapChain.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_dxgi_swap_chain_present(sc_handle: u64) -> u64 {
    use windows::Win32::Graphics::Dxgi::DXGI_PRESENT;
    let Some(sc) = get_dxgi_swap_chain(sc_handle) else {
        return tag(0xFFFF_FFFF);
    };
    // SAFETY: sc valid. Present is the OS hand-off; the surface
    // pointer doesn't escape across the call. SyncInterval=1 → VSync.
    // `IDXGISwapChain1::Present` returns raw `HRESULT` (`windows` 0.58);
    // `.ok()` converts to `Result<(), Error>` for ergonomic matching.
    let r = unsafe { sc.Present(1, DXGI_PRESENT(0)) }.ok();
    match r {
        Ok(()) => 0,
        Err(e) => {
            store_last_hresult(e.code().0);
            tag(e.code().0 as u32 as u64)
        }
    }
}

/// JIT-callable: resize the swap chain to the new dimensions, keeping
/// the same buffer count + format. **Caller MUST drop any references
/// to the previous back-buffer bitmap** before calling — DXGI fails
/// `ResizeBuffers` with `DXGI_ERROR_INVALID_CALL` otherwise. The
/// Sprint 36 IDE-shell handles this in its WM_SIZE branch by
/// releasing the old D2D bitmap before resizing.
///
/// # Safety
/// `sc_handle` must be a valid DxgiSwapChain.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_dxgi_swap_chain_resize_buffers(
    sc_handle: u64,
    width: u64,
    height: u64,
) -> u64 {
    let Some(sc) = get_dxgi_swap_chain(sc_handle) else {
        return tag(0xFFFF_FFFF);
    };
    let w = untag(width) as u32;
    let h = untag(height) as u32;
    // SAFETY: sc valid; ResizeBuffers takes width/height by value.
    // The `windows` 0.58 wrapper for this entry point produces
    // `Result<(), Error>` directly (no `.ok()` needed).
    let r = unsafe {
        sc.ResizeBuffers(2, w, h, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SWAP_CHAIN_FLAG(0))
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            store_last_hresult(e.code().0);
            tag(e.code().0 as u32 as u64)
        }
    }
}

// ─── Phase C — D2D factory / device / device-context / bitmap ─────────────

/// JIT-callable: create the D2D factory (`ID2D1Factory1` for D2D
/// device interop).
///
/// # Safety
/// `D2D1CreateFactory` writes its out-param via the `windows`
/// crate's typed wrapper.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d2d_create_factory() -> u64 {
    // SAFETY: D2D1CreateFactory has no Rust-side preconditions; the
    // typed result is owned by the IDl2D1Factory1 wrapper.
    let r: windows::core::Result<ID2D1Factory1> = unsafe {
        D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)
    };
    match hresult_to_zero(r) {
        Some(f) => tag(register(ComObject::D2DFactory(f))),
        None => 0,
    }
}

/// JIT-callable: create a D2D device from the D2D factory + DXGI
/// device. The result is the D2D-side device — its device contexts
/// are what we actually draw with.
///
/// # Safety
/// Both handles must be of the matching variants.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d2d_create_device(
    d2d_factory_handle: u64,
    dxgi_device_handle: u64,
) -> u64 {
    let Some(factory) = get_d2d_factory(d2d_factory_handle) else {
        return 0;
    };
    let Some(dxgi_dev) = get_dxgi_device(dxgi_device_handle) else {
        return 0;
    };
    // SAFETY: CreateDevice takes a non-null IDXGIDevice.
    let r: windows::core::Result<ID2D1Device> = unsafe { factory.CreateDevice(&dxgi_dev) };
    match hresult_to_zero(r) {
        Some(d) => tag(register(ComObject::D2DDevice(d))),
        None => 0,
    }
}

/// JIT-callable: create a device context from the D2D device. The
/// context is where drawing operations happen (begin-draw,
/// draw-text-layout, end-draw, etc.).
///
/// # Safety
/// `d2d_device_handle` must be a valid D2DDevice handle.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d2d_create_device_context(d2d_device_handle: u64) -> u64 {
    let Some(d2d_dev) = get_d2d_device(d2d_device_handle) else {
        return 0;
    };
    let r: windows::core::Result<ID2D1DeviceContext> = unsafe {
        d2d_dev.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)
    };
    match hresult_to_zero(r) {
        Some(c) => tag(register(ComObject::D2DDeviceContext(c))),
        None => 0,
    }
}

/// JIT-callable: wrap a DXGI surface as a D2D bitmap suitable as a
/// render target. The bitmap shares pixels with the underlying
/// texture; drawing into the bitmap mutates the texture.
///
/// # Safety
/// Both handles must be of the matching variants.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d2d_create_bitmap_for_target(
    dc_handle: u64,
    surface_handle: u64,
) -> u64 {
    let Some(dc) = get_d2d_device_context(dc_handle) else {
        return 0;
    };
    let Some(surface) = get_dxgi_surface(surface_handle) else {
        return 0;
    };
    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
        bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
        colorContext: std::mem::ManuallyDrop::new(None),
    };
    // SAFETY: surface is valid IDXGISurface; props references it.
    let r: windows::core::Result<ID2D1Bitmap1> =
        unsafe { dc.CreateBitmapFromDxgiSurface(&surface, Some(&props)) };
    match hresult_to_zero(r) {
        Some(b) => tag(register(ComObject::D2DBitmap(b))),
        None => 0,
    }
}

/// JIT-callable: set the device context's target to the given
/// bitmap. Sprint 35 always passes a `<c-handle>` here — the bitmap
/// the test owns.
///
/// # Safety
/// Both handles must be of the matching variants. Returns 1 on
/// success (target set), 0 on a handle-lookup miss.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d2d_set_target(dc_handle: u64, bitmap_handle: u64) -> u64 {
    let Some(dc) = get_d2d_device_context(dc_handle) else {
        return 0;
    };
    let Some(bitmap) = get_d2d_bitmap(bitmap_handle) else {
        return 0;
    };
    // SAFETY: bitmap is a registered ID2D1Bitmap1; SetTarget accepts
    // any ID2D1Image (the param trait handles upcasting at the
    // vtable level).
    unsafe { dc.SetTarget(&bitmap) };
    tag(1)
}

/// Helper: cast a D2D device context handle to its render-target
/// view (the parent interface that owns `BeginDraw` / `EndDraw` /
/// `Clear` / brush + primitive APIs). The `windows` crate doesn't
/// auto-deref subinterfaces, so we cast explicitly via QueryInterface.
///
/// Returns `None` if the handle is unknown or the cast fails (should
/// never happen — every device context IS a render target).
fn dc_as_render_target(dc_handle: u64) -> Option<ID2D1RenderTarget> {
    let dc = get_d2d_device_context(dc_handle)?;
    let rt: windows::core::Result<ID2D1RenderTarget> = dc.cast();
    hresult_to_zero(rt)
}

/// JIT-callable: begin a drawing batch on the device context.
/// Returns 1 always (the call doesn't fail; errors surface on
/// `EndDraw`).
///
/// # Safety
/// `dc_handle` must be a valid D2DDeviceContext.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d2d_begin_draw(dc_handle: u64) -> u64 {
    let Some(rt) = dc_as_render_target(dc_handle) else {
        return 0;
    };
    // SAFETY: rt is the render-target view of a valid DC.
    unsafe { rt.BeginDraw() };
    tag(1)
}

/// JIT-callable: end the drawing batch. Returns the HRESULT-as-u64
/// (0 = S_OK; non-zero on failure). Floats-free — see module-level
/// caveat.
///
/// # Safety
/// `dc_handle` must be a valid D2DDeviceContext.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d2d_end_draw(dc_handle: u64) -> u64 {
    let Some(rt) = dc_as_render_target(dc_handle) else {
        return tag(0xFFFF_FFFF);
    };
    // SAFETY: rt valid; EndDraw on ID2D1RenderTarget takes no tag
    // out-params and returns HRESULT.
    match unsafe { rt.EndDraw(None, None) } {
        Ok(()) => 0,
        Err(e) => {
            store_last_hresult(e.code().0);
            tag((e.code().0 as u32) as u64)
        }
    }
}

/// JIT-callable: clear the target with an RGBA color. Each channel
/// is an integer 0..=255; the shim converts to f32 (channel /
/// 255.0). This is the Sprint 35 deviation from native float
/// marshaling (see module docs).
///
/// # Safety
/// `dc_handle` must be a valid D2DDeviceContext.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d2d_clear(
    dc_handle: u64,
    r: u64,
    g: u64,
    b: u64,
    a: u64,
) -> u64 {
    let Some(rt) = dc_as_render_target(dc_handle) else {
        return 0;
    };
    let r = untag(r) & 0xff;
    let g = untag(g) & 0xff;
    let b = untag(b) & 0xff;
    let a = untag(a) & 0xff;
    let color = D2D1_COLOR_F {
        r: (r as f32) / 255.0,
        g: (g as f32) / 255.0,
        b: (b as f32) / 255.0,
        a: (a as f32) / 255.0,
    };
    // SAFETY: rt valid; color on stack.
    unsafe { rt.Clear(Some(&color)) };
    tag(1)
}

/// JIT-callable: set the device context's transform to identity.
/// Eliminates accidental DPI scaling from polluting the pixel
/// assertions in the acceptance test.
///
/// # Safety
/// `dc_handle` must be a valid D2DDeviceContext.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d2d_set_transform_identity(dc_handle: u64) -> u64 {
    let Some(rt) = dc_as_render_target(dc_handle) else {
        return 0;
    };
    let m = Matrix3x2 {
        M11: 1.0,
        M12: 0.0,
        M21: 0.0,
        M22: 1.0,
        M31: 0.0,
        M32: 0.0,
    };
    // SAFETY: rt valid; identity matrix on stack.
    unsafe { rt.SetTransform(&m) };
    tag(1)
}

// ─── Phase D — DirectWrite ────────────────────────────────────────────────

/// JIT-callable: create the shared DirectWrite factory.
///
/// # Safety
/// `DWriteCreateFactory` is stateless.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_dwrite_create_factory() -> u64 {
    // SAFETY: stateless API.
    let r: windows::core::Result<IDWriteFactory> =
        unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED) };
    match hresult_to_zero(r) {
        Some(f) => tag(register(ComObject::DWriteFactory(f))),
        None => 0,
    }
}

/// JIT-callable: create a text format (font family + size).
/// `family_word` and `locale_word` are Dylan `<byte-string>` Words
/// (pointer-tagged) — the shim reads the UTF-8 payload via the
/// runtime's byte-string accessor and converts to UTF-16LE on the
/// stack for the DirectWrite call.
///
/// `size_x100` is the font size in *hundredths of a DIP*: pass `2400`
/// for 24.0 DIPs. (Sprint 35 deviation — see module docs for the
/// float-marshaling caveat.)
///
/// # Safety
/// `factory_handle` must be a valid DWriteFactory; `family_word` and
/// `locale_word` must be Dylan `<byte-string>` Words.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_dwrite_create_text_format(
    factory_handle: u64,
    family_word: u64,
    size_x100: u64,
    locale_word: u64,
) -> u64 {
    let Some(factory) = get_dwrite_factory(factory_handle) else {
        return 0;
    };
    let family = utf16_from_dylan_byte_string(family_word);
    let locale = utf16_from_dylan_byte_string(locale_word);
    let size_dips = (untag(size_x100) as f32) / 100.0;
    // SAFETY: family/locale slices are valid for the call duration
    // (owned Vec<u16>s on this stack frame).
    let r: windows::core::Result<IDWriteTextFormat> = unsafe {
        factory.CreateTextFormat(
            windows::core::PCWSTR(family.as_ptr()),
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size_dips,
            windows::core::PCWSTR(locale.as_ptr()),
        )
    };
    drop(family);
    drop(locale);
    match hresult_to_zero(r) {
        Some(f) => tag(register(ComObject::DWriteTextFormat(f))),
        None => 0,
    }
}

/// JIT-callable: create a text layout from a Dylan `<byte-string>`
/// source + a previously-created text format. `max_width_pixels` and
/// `max_height_pixels` are integer pixel sizes (Sprint 35 deviation).
///
/// # Safety
/// `factory_handle` and `format_handle` must be of matching
/// variants; `text_word` must be a Dylan `<byte-string>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_dwrite_create_text_layout(
    factory_handle: u64,
    text_word: u64,
    format_handle: u64,
    max_width_pixels: u64,
    max_height_pixels: u64,
) -> u64 {
    let Some(factory) = get_dwrite_factory(factory_handle) else {
        return 0;
    };
    let Some(format) = get_dwrite_text_format(format_handle) else {
        return 0;
    };
    // UTF-16 buffer WITHOUT trailing null — DirectWrite takes a
    // `&[u16]` slice (length-prefixed at the C ABI level), not a
    // null-terminated string.
    let text = utf16_from_dylan_byte_string_no_null(text_word);
    // SAFETY: text slice is valid for the call duration.
    let r: windows::core::Result<IDWriteTextLayout> = unsafe {
        factory.CreateTextLayout(
            &text,
            &format,
            untag(max_width_pixels) as f32,
            untag(max_height_pixels) as f32,
        )
    };
    drop(text);
    match hresult_to_zero(r) {
        Some(l) => tag(register(ComObject::DWriteTextLayout(l))),
        None => 0,
    }
}

/// JIT-callable: read metrics off a text layout. Returns a packed
/// u64 with the layout width (low 32 bits) and height (high 32 bits),
/// each rounded to integer pixels. Returns 0 if `layout_handle` is
/// invalid.
///
/// # Safety
/// `layout_handle` must be a valid DWriteTextLayout.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_dwrite_get_layout_metrics(layout_handle: u64) -> u64 {
    let Some(layout) = get_dwrite_text_layout(layout_handle) else {
        return 0;
    };
    let mut metrics =
        windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_METRICS::default();
    // SAFETY: layout is valid; metrics out-param.
    if unsafe { layout.GetMetrics(&mut metrics) }.is_err() {
        return 0;
    }
    let w = metrics.width as u32 as u64;
    let h = metrics.height as u32 as u64;
    tag((h << 32) | (w & 0xFFFF_FFFF))
}

/// JIT-callable: convert a text-position offset into pixel coordinates
/// inside the layout box. Wraps `IDWriteTextLayout::HitTestTextPosition`.
///
/// Inputs:
///   * `layout_handle` — a DWriteTextLayout handle.
///   * `text_position` — UTF-16 code-unit offset into the layout's text.
///     For ASCII-only buffers this is also the byte offset.
///   * `is_trailing_x10` — 0 for the leading edge of the character at
///     `text_position` (i.e. cursor BEFORE that character), non-zero
///     for the trailing edge (cursor AFTER). The naming `_x10` is a
///     parity tag — we untag with `untag()` like any other small int.
///
/// Returns: a packed u64 with `y_pixels` in the high 32 bits and
/// `x_pixels` in the low 32 bits, each rounded to integer pixels.
/// Coordinates are relative to the layout origin (0,0 = top-left of
/// the box passed to `nod_d2d_draw_text_layout`). Returns 0 if the
/// layout handle is invalid.
///
/// This is the canonical "text offset → pixel position" primitive for
/// the IDE — cursor draw uses it, and the click-positioning sprint
/// (43e-5) will use its inverse, `HitTestPoint`.
///
/// # Safety
/// `layout_handle` must be a valid DWriteTextLayout.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_dwrite_hit_test_text_position(
    layout_handle: u64,
    text_position: u64,
    is_trailing_x10: u64,
) -> u64 {
    let Some(layout) = get_dwrite_text_layout(layout_handle) else {
        return 0;
    };
    let pos = untag(text_position) as u32;
    let is_trailing = windows::Win32::Foundation::BOOL::from(untag(is_trailing_x10) != 0);
    let mut point_x: f32 = 0.0;
    let mut point_y: f32 = 0.0;
    let mut metrics =
        windows::Win32::Graphics::DirectWrite::DWRITE_HIT_TEST_METRICS::default();
    // SAFETY: layout valid; out-params on the stack.
    if unsafe {
        layout.HitTestTextPosition(pos, is_trailing, &mut point_x, &mut point_y, &mut metrics)
    }
    .is_err()
    {
        return 0;
    }
    let x = point_x.round() as i32 as u32 as u64;
    let y = point_y.round() as i32 as u32 as u64;
    tag((y << 32) | (x & 0xFFFF_FFFF))
}

/// JIT-callable: convert layout-relative pixel coordinates into a text-
/// position offset. Wraps `IDWriteTextLayout::HitTestPoint`. Inverse
/// of `nod_dwrite_hit_test_text_position`.
///
/// Used by the IDE's mouse-click handler: given a (layout-relative)
/// click point, return the UTF-16 code-unit offset where the cursor
/// should land. For ASCII-only buffers this equals the byte offset.
///
/// Inputs:
///   * `layout_handle` — a DWriteTextLayout handle.
///   * `point_x` — x in DIPs, relative to layout origin.
///   * `point_y` — y in DIPs, relative to layout origin.
///
/// Returns the text-position offset as a Dylan-tagged fixnum. If the
/// click was on the trailing edge of a character (closer to its right
/// edge than its left), the returned offset is one PAST the character
/// — i.e. the cursor sits *after* the character. This matches what
/// every text editor does for mid-character clicks. Returns 0 if the
/// layout handle is invalid.
///
/// # Safety
/// `layout_handle` must be a valid DWriteTextLayout.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_dwrite_hit_test_point(
    layout_handle: u64,
    point_x: u64,
    point_y: u64,
) -> u64 {
    let Some(layout) = get_dwrite_text_layout(layout_handle) else {
        return tag(0);
    };
    let px = untag_i64(point_x) as f32;
    let py = untag_i64(point_y) as f32;
    let mut is_trailing = windows::Win32::Foundation::BOOL::from(false);
    let mut is_inside = windows::Win32::Foundation::BOOL::from(false);
    let mut metrics =
        windows::Win32::Graphics::DirectWrite::DWRITE_HIT_TEST_METRICS::default();
    // SAFETY: layout valid; out-params on the stack.
    if unsafe {
        layout.HitTestPoint(px, py, &mut is_trailing, &mut is_inside, &mut metrics)
    }
    .is_err()
    {
        return tag(0);
    }
    let mut pos = metrics.textPosition as u64;
    if is_trailing.as_bool() {
        pos += metrics.length as u64;
    }
    tag(pos)
}

/// JIT-callable: force a uniform line height on a text format, so
/// every text layout created from it lays out lines exactly
/// `line_spacing_x10 / 10` DIPs apart with the baseline at
/// `baseline_x10 / 10` DIPs from the top of each line. Wraps
/// `IDWriteTextFormat::SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM,
/// lineSpacing, baseline)`.
///
/// The IDE uses this to make DirectWrite's per-line pixel count
/// match the Dylan-side `line-height` constant — without this,
/// the gutter's line numbers drift relative to the text as the
/// user scrolls (cumulative ~1px/line offset because Consolas's
/// natural line height isn't exactly the constant we picked).
///
/// `_x10` is a parity tag — values are untagged like any small
/// integer, then divided by 10. Lets the IDE specify
/// "18.0 DIPs" as the integer 180 without needing a float shim.
///
/// # Safety
/// `format_handle` must be a valid DWriteTextFormat.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_dwrite_set_line_spacing(
    format_handle: u64,
    line_spacing_x10: u64,
    baseline_x10: u64,
) -> u64 {
    let Some(format) = get_dwrite_text_format(format_handle) else {
        return tag(0);
    };
    let spacing = (untag(line_spacing_x10) as f32) / 10.0;
    let baseline = (untag(baseline_x10) as f32) / 10.0;
    // SAFETY: format is valid; method is a constant enum.
    if unsafe {
        format.SetLineSpacing(
            windows::Win32::Graphics::DirectWrite::DWRITE_LINE_SPACING_METHOD_UNIFORM,
            spacing,
            baseline,
        )
    }
    .is_err()
    {
        return tag(0);
    }
    tag(1)
}

/// JIT-callable: apply a drawing effect (typically a colored brush)
/// to a range of text in a layout. Wraps
/// `IDWriteTextLayout::SetDrawingEffect`. The brush becomes the
/// foreground colour for that range when `DrawTextLayout` later
/// renders the layout.
///
/// Inputs:
///   * `layout_handle` — a DWriteTextLayout.
///   * `brush_handle` — an ID2D1SolidColorBrush from
///     `nod_d2d_create_solid_color_brush`.
///   * `start` — UTF-16 code-unit offset of the range start.
///   * `length` — UTF-16 code-unit count.
///
/// Returns 1 on success, 0 on error or bad handle. Used by the IDE's
/// syntax-colouring pass (Sprint 43f-1) to paint Dylan keywords,
/// comments, strings, etc. in distinct colours.
///
/// # Safety
/// Both handles must be of the matching ComObject variants.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_dwrite_set_drawing_effect(
    layout_handle: u64,
    brush_handle: u64,
    start: u64,
    length: u64,
) -> u64 {
    let Some(layout) = get_dwrite_text_layout(layout_handle) else {
        return tag(0);
    };
    let Some(brush) = get_d2d_solid_brush(brush_handle) else {
        return tag(0);
    };
    let range = windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_RANGE {
        startPosition: untag(start) as u32,
        length: untag(length) as u32,
    };
    // SAFETY: layout and brush are valid for the call. The brush is
    // ref-counted; SetDrawingEffect AddRefs it internally to keep it
    // alive until the next SetDrawingEffect / layout drop.
    let effect: windows::core::IUnknown = brush.clone().into();
    if unsafe { layout.SetDrawingEffect(&effect, range) }.is_err() {
        return tag(0);
    }
    tag(1)
}

// ─── Phase E — drawing primitives ────────────────────────────────────────

/// JIT-callable: create a solid-color brush. RGBA channels are
/// 0..=255 integers; the shim converts to 0.0..=1.0 f32. (Sprint 35
/// deviation.)
///
/// # Safety
/// `dc_handle` must be a valid D2DDeviceContext.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d2d_create_solid_color_brush(
    dc_handle: u64,
    r: u64,
    g: u64,
    b: u64,
    a: u64,
) -> u64 {
    let Some(rt) = dc_as_render_target(dc_handle) else {
        return 0;
    };
    let r = untag(r) & 0xff;
    let g = untag(g) & 0xff;
    let b = untag(b) & 0xff;
    let a = untag(a) & 0xff;
    let color = D2D1_COLOR_F {
        r: (r as f32) / 255.0,
        g: (g as f32) / 255.0,
        b: (b as f32) / 255.0,
        a: (a as f32) / 255.0,
    };
    let props = D2D1_BRUSH_PROPERTIES {
        opacity: 1.0,
        transform: Matrix3x2 {
            M11: 1.0,
            M12: 0.0,
            M21: 0.0,
            M22: 1.0,
            M31: 0.0,
            M32: 0.0,
        },
    };
    // SAFETY: rt valid; color + props on stack.
    let r: windows::core::Result<ID2D1SolidColorBrush> =
        unsafe { rt.CreateSolidColorBrush(&color, Some(&props)) };
    match hresult_to_zero(r) {
        Some(b) => tag(register(ComObject::D2DSolidColorBrush(b))),
        None => 0,
    }
}

/// JIT-callable: draw a text layout at the given integer pixel
/// origin. (Sprint 35 deviation — coordinates as ints.)
///
/// # Safety
/// All three handles must be of matching variants.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d2d_draw_text_layout(
    dc_handle: u64,
    origin_x: u64,
    origin_y: u64,
    layout_handle: u64,
    brush_handle: u64,
) -> u64 {
    // Use the ID2D1RenderTarget::DrawTextLayout signature (4 args:
    // origin, layout, brush, options) — the ID2D1DeviceContext4
    // variant requires SVG glyph style + color palette index, both
    // of which we don't need for Sprint 35.
    let Some(rt) = dc_as_render_target(dc_handle) else {
        return 0;
    };
    let Some(layout) = get_dwrite_text_layout(layout_handle) else {
        return 0;
    };
    let Some(brush) = get_d2d_solid_brush(brush_handle) else {
        return 0;
    };
    let origin = D2D_POINT_2F {
        x: untag_i64(origin_x) as f32,
        y: untag_i64(origin_y) as f32,
    };
    // SAFETY: rt/layout/brush valid; origin on stack.
    unsafe {
        rt.DrawTextLayout(origin, &layout, &brush, D2D1_DRAW_TEXT_OPTIONS_NONE);
    }
    tag(1)
}

/// JIT-callable: stroke a rectangle outline with the given brush.
/// `stroke_width_x10` is in tenths of a pixel (Sprint 35 deviation).
///
/// # Safety
/// `dc_handle` and `brush_handle` must be of matching variants.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d2d_draw_rectangle(
    dc_handle: u64,
    left: u64,
    top: u64,
    right: u64,
    bottom: u64,
    brush_handle: u64,
    stroke_width_x10: u64,
) -> u64 {
    let Some(rt) = dc_as_render_target(dc_handle) else {
        return 0;
    };
    let Some(brush) = get_d2d_solid_brush(brush_handle) else {
        return 0;
    };
    let rect = D2D_RECT_F {
        left: untag_i64(left) as f32,
        top: untag_i64(top) as f32,
        right: untag_i64(right) as f32,
        bottom: untag_i64(bottom) as f32,
    };
    let stroke = (untag(stroke_width_x10) as f32) / 10.0;
    // SAFETY: rt/brush valid; rect on stack.
    unsafe { rt.DrawRectangle(&rect, &brush, stroke, None) };
    tag(1)
}

/// JIT-callable: fill a rectangle with the given brush.
///
/// # Safety
/// `dc_handle` and `brush_handle` must be of matching variants.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d2d_fill_rectangle(
    dc_handle: u64,
    left: u64,
    top: u64,
    right: u64,
    bottom: u64,
    brush_handle: u64,
) -> u64 {
    let Some(rt) = dc_as_render_target(dc_handle) else {
        return 0;
    };
    let Some(brush) = get_d2d_solid_brush(brush_handle) else {
        return 0;
    };
    let rect = D2D_RECT_F {
        left: untag_i64(left) as f32,
        top: untag_i64(top) as f32,
        right: untag_i64(right) as f32,
        bottom: untag_i64(bottom) as f32,
    };
    // SAFETY: rt/brush valid; rect on stack.
    unsafe { rt.FillRectangle(&rect, &brush) };
    tag(1)
}

// ─── Phase F — pixel readback ─────────────────────────────────────────────

/// Staging texture cached per (device, dimensions). For Sprint 35
/// we create + cache a single staging texture inside the device's
/// "scratch" registry slot (a separate handle); the test creates +
/// caches it explicitly.
///
/// To keep the API surface flat for Sprint 35, the
/// `nod_d3d11_copy_to_staging_and_map` shim creates a fresh
/// staging texture on each call, copies the GPU texture into it,
/// maps the staging, and returns (a) the raw pointer to the mapped
/// bytes and (b) a handle to the staging texture (used by the
/// matching unmap call). To avoid multi-return-value plumbing, we
/// register the staging texture in the COM registry and surface its
/// handle separately via `nod_d3d11_last_staging_handle()`.
static LAST_STAGING_HANDLE: AtomicU64 = AtomicU64::new(0);
static LAST_MAPPED_ROW_PITCH: AtomicU64 = AtomicU64::new(0);

/// JIT-callable: copy the GPU texture to a CPU-readable staging
/// texture, map it for reading, and return a raw pointer to the
/// mapped bytes (as a u64 — Dylan side treats it as a
/// `<c-pointer>`). The staging texture is registered separately;
/// retrieve its handle via `nod_d3d11_last_staging_handle()`.
///
/// `width`/`height` MUST match the original texture's dimensions.
/// On error returns 0; the matching `_last_staging_handle()` also
/// returns 0 in that case.
///
/// The mapped bytes layout is BGRA8 (4 bytes per pixel). The pitch
/// (bytes per row, including any padding) may be > width * 4 — read
/// it via `nod_d3d11_last_mapped_row_pitch()`.
///
/// # Safety
/// All handles must be valid; width/height must match the texture.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d3d11_copy_to_staging_and_map(
    device_handle: u64,
    ctx_handle: u64,
    tex_handle: u64,
    width: u64,
    height: u64,
) -> u64 {
    LAST_STAGING_HANDLE.store(0, Ordering::Relaxed);
    LAST_MAPPED_ROW_PITCH.store(0, Ordering::Relaxed);

    let Some(device) = get_d3d11_device(device_handle) else {
        return 0;
    };
    let Some(ctx) = get_d3d11_device_context(ctx_handle) else {
        return 0;
    };
    let Some(tex) = get_d3d11_texture_2d(tex_handle) else {
        return 0;
    };
    let width = untag(width);
    let height = untag(height);

    // Build the staging texture (CPU-readable, no bind flags).
    let staging_desc = D3D11_TEXTURE2D_DESC {
        Width: width as u32,
        Height: height as u32,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut staging: Option<ID3D11Texture2D> = None;
    // SAFETY: desc fully populated.
    let r = unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut staging)) };
    if let Err(e) = r {
        store_last_hresult(e.code().0);
        return 0;
    }
    let Some(staging) = staging else { return 0 };

    // Copy GPU → staging, then explicit Flush (recommended for
    // read-back to avoid stale GPU work).
    // SAFETY: staging and tex are both valid ID3D11Texture2D.
    unsafe { ctx.CopyResource(&staging, &tex) };
    // SAFETY: ctx is valid.
    unsafe { ctx.Flush() };

    // Map the staging texture for reading.
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: staging valid; mapped is out-param.
    let r = unsafe { ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) };
    if let Err(e) = r {
        store_last_hresult(e.code().0);
        return 0;
    }
    // Stash staging in the registry so the caller can unmap + drop
    // it explicitly. The mapped pointer is borrowed for the
    // duration of the unmap call — Dylan reads pixels via raw
    // pointer arithmetic before unmapping.
    let staging_handle = register(ComObject::D3D11Texture2D(staging));
    LAST_STAGING_HANDLE.store(tag(staging_handle), Ordering::Relaxed);
    LAST_MAPPED_ROW_PITCH.store(tag(mapped.RowPitch as u64), Ordering::Relaxed);
    // Return the raw pixel pointer as a Dylan integer-encoded value
    // — the receiver treats this as a `<c-pointer>`. Fixnums in our
    // 63-bit range cover any user-mode address (high bits clear).
    tag(mapped.pData as u64)
}

/// JIT-callable: report the staging-texture handle from the most
/// recent `_copy_to_staging_and_map` call. 0 if the last call
/// failed.
///
/// # Safety
/// No unsafe operations.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d3d11_last_staging_handle() -> u64 {
    LAST_STAGING_HANDLE.load(Ordering::Relaxed)
}

/// JIT-callable: report the mapped row-pitch (bytes per row) from
/// the most recent `_copy_to_staging_and_map` call.
///
/// # Safety
/// No unsafe operations.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d3d11_last_mapped_row_pitch() -> u64 {
    LAST_MAPPED_ROW_PITCH.load(Ordering::Relaxed)
}

/// JIT-callable: unmap a previously-mapped staging texture.
///
/// # Safety
/// `ctx_handle` and `staging_handle` must be of matching variants
/// from `_copy_to_staging_and_map`.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_d3d11_unmap(ctx_handle: u64, staging_handle: u64) -> u64 {
    let Some(ctx) = get_d3d11_device_context(ctx_handle) else {
        return 0;
    };
    let Some(staging) = get_d3d11_texture_2d(staging_handle) else {
        return 0;
    };
    // SAFETY: ctx + staging valid.
    unsafe { ctx.Unmap(&staging, 0) };
    tag(1)
}

/// JIT-callable: scan a mapped pixel buffer for non-zero red
/// channel pixels. BGRA8 layout — red is byte offset 2 in each
/// 4-byte pixel. Returns the count. This is the acceptance-test
/// assertion primitive: "did text glyphs render any red pixels?"
///
/// `pixels_ptr` comes from `_copy_to_staging_and_map`. `width` and
/// `height` are the texture dimensions. `row_pitch` is the bytes-
/// per-row from the mapped subresource (may be > width*4).
///
/// # Safety
/// `pixels_ptr` must point at a readable region of at least
/// `row_pitch * height` bytes (i.e. the mapped staging texture).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_count_non_zero_red(
    pixels_ptr: u64,
    width: u64,
    height: u64,
    row_pitch: u64,
) -> u64 {
    let pixels_ptr = untag(pixels_ptr);
    let width = untag(width);
    let height = untag(height);
    let row_pitch = untag(row_pitch);
    if pixels_ptr == 0 {
        return 0;
    }
    let base = pixels_ptr as *const u8;
    let w = width as usize;
    let h = height as usize;
    let pitch = row_pitch as usize;
    let mut count: u64 = 0;
    for y in 0..h {
        for x in 0..w {
            let offset = y * pitch + x * 4;
            // SAFETY: caller's invariant — bytes within the mapped
            // region. BGRA8 means red is at byte 2 of each pixel.
            let red = unsafe { *base.add(offset + 2) };
            if red != 0 {
                count += 1;
            }
        }
    }
    tag(count)
}

// ─── Sprint 36 — Window class registration (Rust helper) ──────────────────
//
// Building a WNDCLASSEXW in Dylan source is awkward because the
// `lpszClassName` field needs to point at a wide-string buffer with
// process-lifetime semantics. We could express that in pure Dylan by
// pinning a fresh `<c-wide-string>` in the static area, but Sprint 30
// didn't ship a "leak this into the static area for ever" helper.
//
// Instead we offer a Rust extern that takes a WNDPROC pointer and a
// Dylan byte-string class name, leaks a fresh UTF-16 buffer into a
// process-global map keyed by class name (so repeated calls with the
// same name reuse the same buffer — important if the caller hits the
// helper twice), constructs WNDCLASSEXW on the stack, and calls
// `RegisterClassExW`. Returns the registered atom (non-zero) or 0 on
// error (HRESULT-equivalent goes into `LAST_HRESULT` as the Win32
// `GetLastError()` value cast through u32).
//
// Sprint 36 also exposes `nod_create_message_only_window` for the
// non-interactive infrastructure tests. A message-only window is
// created by passing `HWND_MESSAGE` as the parent HWND; the window
// never displays, so it's perfect for proving the message-loop
// plumbing without inflicting a UI on the user's screen during
// routine `cargo test`.

use std::sync::RwLock;

static CLASS_NAME_BUFFERS: OnceLock<RwLock<HashMap<String, &'static [u16]>>> =
    OnceLock::new();

fn class_name_buffers() -> &'static RwLock<HashMap<String, &'static [u16]>> {
    CLASS_NAME_BUFFERS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Leak a UTF-16 null-terminated buffer for `name` into the process-
/// global cache, returning a `&'static [u16]` whose pointer survives
/// the process lifetime. Idempotent — repeated calls with the same
/// name return the same buffer.
fn leak_class_name(name: &str) -> &'static [u16] {
    // Fast path: already cached.
    {
        let g = class_name_buffers().read().expect("class name cache poisoned");
        if let Some(b) = g.get(name) {
            return b;
        }
    }
    // Slow path: insert under a write lock. Recheck after acquiring
    // the write lock to avoid double-leaking under a race.
    let mut g = class_name_buffers().write().expect("class name cache poisoned");
    if let Some(b) = g.get(name) {
        return b;
    }
    let mut units: Vec<u16> = name.encode_utf16().collect();
    units.push(0);
    let leaked: &'static [u16] = Box::leak(units.into_boxed_slice());
    g.insert(name.to_string(), leaked);
    leaked
}

/// JIT-callable: register a Win32 window class given a WNDPROC
/// callback pointer (from `as-wndproc-callback`) and a class name as
/// a Dylan byte-string. The class name is leaked into a process-
/// global cache so the pointer stored in `lpszClassName` survives for
/// the process lifetime — Win32 expects that pointer to be valid for
/// the entire interval between `RegisterClassExW` and (much later)
/// `UnregisterClass`. We don't unregister in Sprint 36; the leak is
/// intentional and bounded (one Vec<u16> per distinct class name).
///
/// `wndproc_word` is a fixnum-tagged pointer (`<c-pointer>` ABI). The
/// trampoline-pool API returns these.
///
/// Returns the atom (non-zero on success), 0 on failure. The Win32
/// `GetLastError()` is stashed in `LAST_HRESULT` on failure.
///
/// # Safety
/// `wndproc_word` must encode the address of a Sprint 32 trampoline
/// slot. `class_name_word` must be a Dylan `<byte-string>` Word.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_register_window_class(
    wndproc_word: u64,
    class_name_word: u64,
) -> u64 {
    use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CS_HREDRAW, CS_VREDRAW, HCURSOR, HICON, RegisterClassExW, WNDCLASSEXW, WNDPROC,
    };
    use windows::Win32::Graphics::Gdi::HBRUSH;
    use windows::core::PCWSTR;

    // Decode the WNDPROC pointer (fixnum-tagged raw address).
    let wndproc_addr = untag_i64(wndproc_word);
    if wndproc_addr == 0 {
        return 0;
    }
    // SAFETY: the address came from Sprint 32's callback trampoline
    // pool which guarantees a `extern "system" fn(HWND, UINT, WPARAM,
    // LPARAM) -> LRESULT`. The transmute reifies that shape (the
    // `windows` crate uses its own newtypes — WPARAM/LPARAM/LRESULT —
    // which are repr-transparent over usize/isize, so the actual ABI
    // matches the integer-shaped `extern "system" fn(u64, u64) -> i32`
    // the Sprint 32 trampolines were emitted as).
    let wndproc: WNDPROC = Some(unsafe {
        std::mem::transmute::<*const (), unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>(
            wndproc_addr as *const (),
        )
    });

    // Read the class-name byte string and leak a wide buffer for it.
    let name_word = crate::word::Word::from_raw(class_name_word);
    let name = crate::winffi::read_dylan_byte_string(name_word);
    let wide = leak_class_name(&name);

    // Fetch HINSTANCE — pass None so `GetModuleHandleW(NULL)` returns
    // the EXE's instance, which is what `RegisterClassExW` wants for
    // a non-DLL caller.
    // SAFETY: GetModuleHandleW with a null lpModuleName is documented
    // to return the calling process's module.
    let hinstance: HINSTANCE = match unsafe { GetModuleHandleW(None) } {
        Ok(h) => h.into(),
        Err(e) => {
            store_last_hresult(e.code().0);
            return 0;
        }
    };

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: wndproc,
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        hIcon: HICON::default(),
        hCursor: HCURSOR::default(),
        hbrBackground: HBRUSH::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: PCWSTR(wide.as_ptr()),
        hIconSm: HICON::default(),
    };

    // SAFETY: wc fully populated; the wide-name pointer outlives the
    // call (Box::leak).
    let atom = unsafe { RegisterClassExW(&wc) };
    if atom == 0 {
        // Map GetLastError into the HRESULT slot. RegisterClassExW
        // doesn't return an HRESULT, but the convention is what
        // downstream callers check.
        // SAFETY: GetLastError has no Rust preconditions.
        let err = unsafe { windows::Win32::Foundation::GetLastError() };
        store_last_hresult(err.0 as i32);
        return 0;
    }
    tag(atom as u64)
}

/// JIT-callable: create a *hidden* normal window. Unlike a
/// message-only window, this one is a full-fledged overlapped
/// window that can host an HWND-bound swap chain (DXGI rejects
/// `HWND_MESSAGE`). Because we never call `ShowWindow`, it stays
/// invisible — perfect for the infrastructure test that exercises
/// swap-chain creation without popping a UI.
///
/// # Safety
/// `class_atom_word` must be a previously-registered class atom.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_create_hidden_window(class_atom_word: u64) -> u64 {
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, CW_USEDEFAULT, HMENU, WINDOW_EX_STYLE, WS_OVERLAPPEDWINDOW,
    };
    use windows::core::PCWSTR;

    let atom = untag(class_atom_word) as u16;
    if atom == 0 {
        return 0;
    }
    let class_name = PCWSTR(atom as usize as *const u16);
    // SAFETY: window is created hidden — we never call ShowWindow on
    // it. Standard overlapped-window style; default position; 200x150
    // dimensions for the swap chain to have something to bind to.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            PCWSTR::null(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            200,
            150,
            HWND::default(),
            HMENU::default(),
            None,
            None,
        )
    };
    match hwnd {
        Ok(h) if !h.0.is_null() => tag(h.0 as u64),
        Ok(_) => 0,
        Err(e) => {
            store_last_hresult(e.code().0);
            0
        }
    }
}

/// JIT-callable: create a Win32 "message-only" window. Message-only
/// windows never display anywhere — Win32 uses them as message-loop
/// endpoints for services and tests. They support `PostMessage` /
/// `GetMessage` / `DispatchMessage` and routed WNDPROC dispatch
/// exactly like a normal window, so they're the right tool for
/// proving the message-pump plumbing in the non-interactive
/// infrastructure tests. (DXGI swap chains REQUIRE a non-message
/// window — for that test, use `nod_create_hidden_window`.)
///
/// `class_atom` is the atom returned from `nod_register_window_class`.
/// Returns the HWND-as-fixnum or 0 on failure (`GetLastError` stashed
/// in `LAST_HRESULT`).
///
/// # Safety
/// `class_atom_word` must be a valid registered class atom (call
/// `nod_register_window_class` first).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_create_message_only_window(class_atom_word: u64) -> u64 {
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, HMENU, HWND_MESSAGE, WINDOW_EX_STYLE, WINDOW_STYLE,
    };
    use windows::core::PCWSTR;

    let atom = untag(class_atom_word) as u16;
    if atom == 0 {
        return 0;
    }
    // Win32 lets you pass MAKEINTATOM(atom) (a u16 cast to LPCWSTR)
    // as `lpClassName` to identify a previously-registered class
    // without needing the name string. The low 16 bits are the atom;
    // the upper bits MUST be zero so Win32 recognises it as an atom
    // rather than a string pointer.
    let class_name = PCWSTR(atom as usize as *const u16);
    // SAFETY: hwndParent = HWND_MESSAGE → window is created in the
    // message-only space (never displayed).
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            HMENU::default(),
            None,
            None,
        )
    };
    match hwnd {
        Ok(h) if !h.0.is_null() => tag(h.0 as u64),
        Ok(_) => 0,
        Err(e) => {
            store_last_hresult(e.code().0);
            0
        }
    }
}

/// JIT-callable: destroy a window. Used by tests to clean up
/// message-only windows.
///
/// # Safety
/// `hwnd_word` must encode a valid HWND obtained from
/// `CreateWindowExW` (or `nod_create_message_only_window`).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_destroy_window(hwnd_word: u64) -> u64 {
    use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;
    let h = untag_i64(hwnd_word);
    if h == 0 {
        return 0;
    }
    let hwnd_h = HWND(h as *mut std::ffi::c_void);
    // SAFETY: hwnd_h valid; DestroyWindow just calls into user32.
    match unsafe { DestroyWindow(hwnd_h) } {
        Ok(()) => tag(1),
        Err(e) => {
            store_last_hresult(e.code().0);
            0
        }
    }
}

/// JIT-callable: forward an unhandled WNDPROC message to
/// `DefWindowProcW`. Tests use this as the WNDPROC's default-return
/// path — returning 0 from WM_NCCREATE / WM_CREATE causes
/// `CreateWindowExW` to fail, so a stub WNDPROC needs a working
/// default.
///
/// # Safety
/// All args must be Win32-ABI-compatible (fixnum-tagged integers).
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_def_window_proc(
    hwnd_word: u64,
    msg: u64,
    wparam: u64,
    lparam: u64,
) -> u64 {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::DefWindowProcW;
    let h = untag_i64(hwnd_word);
    let hwnd_h = HWND(h as *mut std::ffi::c_void);
    let m = untag(msg) as u32;
    let wp = WPARAM(untag(wparam) as usize);
    let lp = LPARAM(untag_i64(lparam) as isize);
    // SAFETY: all args are Win32-ABI compatible.
    let result = unsafe { DefWindowProcW(hwnd_h, m, wp, lp) };
    // result.0 is `isize`. Fixnum-encode it.
    let val = result.0 as i64;
    // If the value exceeds the fixnum range, truncate via tag's
    // shift. Practical LRESULT values from DefWindowProcW fit
    // comfortably in 62 bits.
    crate::word::Word::fixnum_unchecked(val).raw()
}

/// JIT-callable: post a message to a window's queue. Used by tests
/// to drive the message pump without UI events.
///
/// # Safety
/// `hwnd_word` must be a valid HWND.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_post_message(
    hwnd_word: u64,
    msg: u64,
    wparam: u64,
    lparam: u64,
) -> u64 {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
    let h = untag_i64(hwnd_word);
    if h == 0 {
        return 0;
    }
    let hwnd_h = HWND(h as *mut std::ffi::c_void);
    let m = untag(msg) as u32;
    let wp = WPARAM(untag(wparam) as usize);
    let lp = LPARAM(untag_i64(lparam) as isize);
    // SAFETY: hwnd valid; PostMessageW writes to the OS message
    // queue and returns immediately.
    match unsafe { PostMessageW(hwnd_h, m, wp, lp) } {
        Ok(()) => tag(1),
        Err(e) => {
            store_last_hresult(e.code().0);
            0
        }
    }
}

/// JIT-callable: dispatch a single waiting message if there is one,
/// using a peek-and-dispatch loop. Used by infrastructure tests to
/// drain pending messages without blocking on `GetMessage`. Returns
/// the number of messages dispatched (0 if the queue was empty).
///
/// `hwnd_word` is currently unused — we PeekMessage on the whole
/// thread queue (passing `None` as the HWND filter), which is what
/// the infrastructure tests want: they expect to drain any messages
/// the test's `PostMessage` calls posted plus any framework-level
/// messages WIN32 inserts. The arg is kept in the API for forward-
/// compatibility (Sprint 36+1 will let callers filter to a specific
/// HWND).
///
/// # Safety
/// No unsafe operations beyond the FFI boundary.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_pump_one_message(_hwnd_word: u64) -> u64 {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    };
    let mut count: u64 = 0;
    // Peek up to a small bound to drain the queue without spinning.
    for _ in 0..32 {
        let mut msg = MSG::default();
        // SAFETY: msg is a stack out-param; PeekMessageW writes to it.
        // `None` HWND drains the whole thread queue.
        let has = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() };
        if !has {
            break;
        }
        // SAFETY: msg fully populated by PeekMessageW.
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        count += 1;
    }
    tag(count)
}

/// JIT-callable: the canonical Win32 blocking message loop. Calls
/// `GetMessageW` in a loop until it returns 0 (signalling `WM_QUIT`,
/// which a WNDPROC posts via `PostQuitMessage(N)` typically inside a
/// `WM_DESTROY` handler). Each non-quit message is translated +
/// dispatched. Returns the fixnum-tagged `WPARAM` of the WM_QUIT
/// message (which `PostQuitMessage(N)` set to `N`) so the caller can
/// surface a process exit code.
///
/// This is the Sprint 41a primitive: a Dylan-source `%run-message-loop()`
/// call lowers to this shim and the test/EXE blocks here until the
/// user closes the window. Behaves identically to the canonical
/// `while (GetMessage(...)) { Translate; Dispatch; }` C idiom every
/// Win32 SDK sample carries.
///
/// `GetMessageW`'s return contract is `BOOL`-but-tri-valued: positive
/// for "got a message", zero for "WM_QUIT", negative for an error
/// (typically because the caller passed garbage). We treat any
/// non-positive return as "stop pumping" — for a negative error
/// `msg.wParam` is undefined, so the caller observes 0 as the exit
/// code, which is the same outcome as a clean WM_QUIT(0). A future
/// sprint can surface the error as a `<c-ffi-error>` if a use case
/// emerges; Sprint 41a chose simplicity over a path that doesn't
/// fire in practice (every well-formed program reaches WM_QUIT,
/// never the error branch).
///
/// # Safety
/// No unsafe operations beyond the FFI boundary; `GetMessageW` is a
/// stateless Win32 API that fills `MSG` via the out-pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_run_message_loop() -> u64 {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, MSG, TranslateMessage,
    };
    let mut msg = MSG::default();
    let exit_code: i32;
    // SAFETY: msg is a stack out-param; GetMessageW writes to it on
    // every iteration. `None` HWND filter blocks on the whole thread
    // queue, exactly matching the C idiom.
    unsafe {
        loop {
            // GetMessageW returns:
            //   > 0 : a message was retrieved (translate + dispatch).
            //   = 0 : WM_QUIT — exit loop, return msg.wParam as code.
            //   < 0 : error (invalid hwnd, etc.) — exit loop, code 0.
            let ret = GetMessageW(&mut msg, None, 0, 0).0;
            if ret <= 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // msg.wParam carries the value `PostQuitMessage(N)` passed.
        // For the negative-return error path, msg is undefined but
        // wParam is just a u/i value — clamp the fixnum-encoded
        // exit code to a 32-bit integer the caller can compare.
        exit_code = msg.wParam.0 as i32;
    }
    tag(exit_code as u32 as u64)
}

// ─── Sprint 41b — bit-extraction helpers for WM_SIZE lparam unpack ────────

/// JIT-callable: return the low 16 bits of an integer. Used by the
/// Dylan-side WM_SIZE handler to extract `LOWORD(lparam)` = new client
/// width. Dylan currently lacks `logand` / `bit-and` / `ash`
/// primitives, so a dedicated shim is the path of least resistance for
/// Sprint 41b — alternatives (adding three full bitwise primitives, or
/// exposing `GetClientRect` and a `<rect>` struct) are much larger
/// commitments. Future sprints can promote this to a general
/// `%logand(value, mask)` shim when more callers materialise.
///
/// # Safety
/// No unsafe operations beyond the FFI boundary.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_lo_word(value: u64) -> u64 {
    let v = untag(value);
    tag(v & 0xFFFF)
}

/// JIT-callable: return bits 16-31 of an integer as a u16. Used by the
/// Dylan-side WM_SIZE handler to extract `HIWORD(lparam)` = new client
/// height. See `nod_lo_word` for the rationale.
///
/// # Safety
/// No unsafe operations beyond the FFI boundary.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_hi_word(value: u64) -> u64 {
    let v = untag(value);
    tag((v >> 16) & 0xFFFF)
}

// ─── Sprint 41b — file I/O + argv helpers for the IDE ───────────────────────

/// JIT-callable: read the entire contents of a file whose UTF-8 path is
/// carried by the Dylan `<byte-string>` Word `path_word`, intern the
/// result as a fresh Dylan `<byte-string>` Word, and return its raw
/// bits. On any error (missing file, permission denied, OS quirk),
/// return the `nil` immediate Word — the Dylan caller's `result = nil`
/// check is the documented signal.
///
/// The file is decoded as UTF-8 with `String::from_utf8_lossy` so
/// non-UTF-8 sequences are replaced with U+FFFD instead of propagating
/// an error. For a source-viewer use case that's the right default:
/// a Dylan source file with a stray non-UTF-8 byte should still render,
/// just with a visible replacement glyph at the bad byte.
///
/// Allocation goes through `intern_string_literal`, which puts the
/// content in the process-global static-area literal pool. That's
/// long-lived (no GC) but the IDE reads each source file exactly
/// once at open-time, so the pool grows O(file-size) per opened
/// source, not per-paint. A future sprint can route the allocation
/// through the moveable heap when an editor wants per-buffer
/// lifetime.
///
/// # Safety
/// `path_word` must be a valid Dylan Word. If sema's type-checking
/// passed a non-`<byte-string>` Word, `read_dylan_byte_string` panics.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_read_file_to_string(path_word: u64) -> u64 {
    let path_string = crate::winffi::read_dylan_byte_string(crate::word::Word::from_raw(path_word));
    match std::fs::read(&path_string) {
        Ok(bytes) => {
            // Lossy decode preserves the byte count for ASCII and
            // mostly-ASCII files (every byte either round-trips or
            // becomes U+FFFD, which is 3 UTF-8 bytes). Most Dylan
            // source files are pure ASCII.
            let s = String::from_utf8_lossy(&bytes).into_owned();
            crate::intern_string_literal(&s).raw()
        }
        Err(_) => crate::literal_pool_immediates().nil.raw(),
    }
}

/// JIT-callable: return the first user-supplied command-line argument
/// (the second `std::env::args()` element, i.e. `argv[1]`) as a fresh
/// Dylan `<byte-string>` Word. Returns `nil` if the process was
/// launched with no extra args.
///
/// On Windows `std::env::args()` decodes the OS's UTF-16 command line
/// to UTF-8 via the Rust std runtime — which is exactly what every
/// well-behaved CLI tool wants. A future general `%argv()` shim can
/// expose the full vector; Sprint 41b only needs the one filename arg
/// the IDE EXE is invoked with.
///
/// # Safety
/// No unsafe operations beyond the FFI boundary.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_get_argv1() -> u64 {
    match std::env::args().nth(1) {
        Some(s) => crate::intern_string_literal(&s).raw(),
        None => crate::literal_pool_immediates().nil.raw(),
    }
}

/// JIT-callable: return `argv[2]` as a Dylan `<byte-string>` Word.
/// Returns `nil` if the process was launched without a second user
/// argument.
///
/// # Safety
/// No unsafe operations beyond the FFI boundary.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_get_argv2() -> u64 {
    match std::env::args().nth(2) {
        Some(s) => crate::intern_string_literal(&s).raw(),
        None => crate::literal_pool_immediates().nil.raw(),
    }
}

/// JIT-callable: print the process-global GC stats report to stderr.
/// Returns Dylan `#f` so callers can use it in statement position.
///
/// # Safety
/// No unsafe operations beyond the FFI boundary.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_print_gc_stats() -> u64 {
    eprint!("{}", crate::gc_stats_report());
    crate::literal_pool_immediates().false_.raw()
}

// Sprint 41c's `nod_count_newlines` and Sprint 41d's `nod_max_line_chars`
// shims were retired in Sprint 42a Phase E — both are now pure Dylan in
// `tests/nod-tests/fixtures/nod-ide.dylan` (`nod-count-newlines` /
// `nod-max-line-chars`) using the byte-string `size` and `element` ops.

// ─── Sprint 41c — scrollbar primitives ─────────────────────────────────────

/// JIT-callable: configure the vertical or horizontal scrollbar on the
/// given window. Flat-args shim around `SetScrollInfo` — passing the
/// SCROLLINFO struct from Dylan would require plumbing a 7-field
/// `<c-struct>` shape, which is a much bigger lift than a 7-u64-args
/// shim that builds the struct on the Rust side.
///
/// * `hwnd_word` — fixnum-tagged HWND (from `CreateWindowExW`).
/// * `nbar` — 0 = `SB_HORZ`, 1 = `SB_VERT`. Matches the Win32
///   `SCROLLBAR_CONSTANTS` integer encoding so Dylan callers can use
///   the win32-constants names directly if those land in stdlib in a
///   later sprint.
/// * `n_min` — scroll range minimum (typically 0).
/// * `n_max` — scroll range maximum (e.g. line-count).
/// * `n_page` — visible-window size in the same units as the range;
///   drives proportional thumb sizing.
/// * `n_pos` — desired current scroll position.
/// * `redraw` — 1 to repaint the scrollbar immediately, 0 to defer.
///
/// Returns the new scroll position as a fixnum-tagged Word (Win32's
/// `SetScrollInfo` returns the actual position, which may differ from
/// the requested `n_pos` if it was out of range). On a null/invalid
/// HWND returns 0.
///
/// # Safety
/// `hwnd_word` must encode a real HWND. Win32 validates and returns 0
/// for garbage handles, so a stale handle is safe but useless.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_set_scroll_info(
    hwnd_word: u64,
    nbar: u64,
    n_min: u64,
    n_max: u64,
    n_page: u64,
    n_pos: u64,
    redraw: u64,
) -> u64 {
    use windows::Win32::UI::Controls::SetScrollInfo;
    use windows::Win32::UI::WindowsAndMessaging::{SCROLLBAR_CONSTANTS, SCROLLINFO, SIF_ALL};
    let hwnd_raw = untag_i64(hwnd_word);
    if hwnd_raw == 0 {
        return 0;
    }
    let hwnd_h = HWND(hwnd_raw as *mut std::ffi::c_void);
    let bar = SCROLLBAR_CONSTANTS(untag(nbar) as i32);
    let info = SCROLLINFO {
        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
        fMask: SIF_ALL,
        nMin: untag_i64(n_min) as i32,
        nMax: untag_i64(n_max) as i32,
        nPage: untag(n_page) as u32,
        nPos: untag_i64(n_pos) as i32,
        nTrackPos: 0,
    };
    let redraw_b = untag(redraw) != 0;
    // SAFETY: hwnd valid (checked above), &info valid on stack.
    let new_pos = unsafe { SetScrollInfo(hwnd_h, bar, &info, redraw_b) };
    tag(new_pos as u64)
}

/// JIT-callable: read the current scroll position of the given
/// scrollbar (`nbar` = 0 = SB_HORZ, 1 = SB_VERT). Returns the position
/// as a fixnum-tagged Word. On invalid HWND returns 0.
///
/// # Safety
/// `hwnd_word` must encode a real HWND.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_get_scroll_pos(hwnd_word: u64, nbar: u64) -> u64 {
    use windows::Win32::UI::WindowsAndMessaging::{GetScrollPos, SCROLLBAR_CONSTANTS};
    let hwnd_raw = untag_i64(hwnd_word);
    if hwnd_raw == 0 {
        return 0;
    }
    let hwnd_h = HWND(hwnd_raw as *mut std::ffi::c_void);
    let bar = SCROLLBAR_CONSTANTS(untag(nbar) as i32);
    // SAFETY: hwnd valid (checked above). GetScrollPos returns 0 on
    // failure as well; that's indistinguishable from a real 0
    // position, which is acceptable for IDE viewport tracking.
    let pos = unsafe { GetScrollPos(hwnd_h, bar) };
    tag(pos as u64)
}

// ─── Sprint 41e — Win32 file-open common dialog ──────────────────────────

/// JIT-callable: show the Win32 common file-open dialog and return the
/// chosen path as a fresh Dylan `<byte-string>` Word, or the `nil`
/// immediate if the user cancelled (or any system error). The dialog is
/// owned by `hwnd_word` (so it modals against the IDE window).
///
/// Why a shim: `GetOpenFileNameW` takes an `OPENFILENAMEW` struct with
/// 18+ fields and a hard `cbSize = sizeof(OPENFILENAMEW)` invariant.
/// Plumbing that as a Dylan-side `<c-struct>` would be far more code
/// than the rest of the Sprint 41e brief combined. A single shim sets
/// sane defaults (filter = Dylan/Text/All, OFN_FILEMUSTEXIST |
/// OFN_PATHMUSTEXIST), allocates a 260-wchar buffer for the returned
/// path, runs the dialog, and surfaces the chosen path back to Dylan as
/// a freshly-interned `<byte-string>`.
///
/// # Safety
/// `hwnd_word` must be a Dylan fixnum encoding a valid (or 0 = no
/// owner) HWND. The dialog blocks the calling thread until the user
/// confirms or cancels.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_show_open_file_dialog(hwnd_word: u64) -> u64 {
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    use windows::core::{PCWSTR, PWSTR};

    let hwnd_raw = untag_i64(hwnd_word);
    let hwnd_h = HWND(hwnd_raw as *mut std::ffi::c_void);

    // Filter pairs: "<display>\0<pattern>\0", terminated by a double
    // null. The DirectWrite docs call this format a "double-null
    // terminated wide-character string".
    let filter_utf8 =
        "Dylan (*.dylan)\0*.dylan\0Text (*.txt)\0*.txt\0All (*.*)\0*.*\0\0";
    let filter: Vec<u16> = filter_utf8.encode_utf16().collect();

    // 260 = MAX_PATH on classic Win32. Long-path support requires a
    // larger buffer + OFN_EXPLORER + a manifest entry; deferred until
    // someone files a real bug against an IDE-opened path > 260 chars.
    let mut path_buf: [u16; 260] = [0u16; 260];

    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd_h,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(path_buf.as_mut_ptr()),
        nMaxFile: path_buf.len() as u32,
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST,
        ..unsafe { std::mem::zeroed() }
    };

    // SAFETY: ofn is fully populated; lpstrFile points to path_buf
    // which lives for the duration of this call; lpstrFilter points to
    // the filter Vec which also lives for the duration.
    let ok = unsafe { GetOpenFileNameW(&mut ofn) };
    drop(filter);

    if !ok.as_bool() {
        // Either the user cancelled or `CommDlgExtendedError()` would
        // surface a real problem. Either way, Dylan-side semantics is
        // "no file picked"; return nil for the caller to test.
        return crate::literal_pool_immediates().nil.raw();
    }

    // Find the null terminator. GetOpenFileNameW guarantees one within
    // [0, nMaxFile-1] on success.
    let len = path_buf.iter().position(|&c| c == 0).unwrap_or(path_buf.len());
    let s = String::from_utf16_lossy(&path_buf[..len]);
    crate::intern_string_literal(&s).raw()
}

// ─── Sprint 41g — write-file + save-file dialog ──────────────────────────

/// JIT-callable: write a Dylan `<byte-string>` payload to the file
/// whose UTF-8 path is also a Dylan `<byte-string>` Word. Returns
/// fixnum-tagged 1 on success, fixnum-tagged 0 on any I/O error.
///
/// Mirrors `nod_read_file_to_string` exactly: decode both Words via
/// `read_dylan_byte_string`, then call `std::fs::write` which creates
/// the file if absent or truncates and overwrites if present. The
/// write is binary — bytes go to disk verbatim, no CRLF translation,
/// no encoding mangling, no trailing-newline normalization. Round-
/// tripping a file with `%read-file` followed by `%write-file` to a
/// fresh path produces byte-identical content.
///
/// Sprint 41g uses this to support File → Save / Save As; the editor
/// is still read-only so the typical usage is "rewrite the file with
/// its own current contents", but the plumbing is ready for when
/// editing arrives (Sprint 41h+).
///
/// # Safety
/// `path_word` and `content_word` must be valid Dylan
/// `<byte-string>` Words. `read_dylan_byte_string` panics if either
/// is the wrong class — sema's type-checking is responsible for
/// catching that at the Dylan-source level.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_write_file_from_string(
    path_word: u64,
    content_word: u64,
) -> u64 {
    let path = crate::winffi::read_dylan_byte_string(crate::word::Word::from_raw(path_word));
    let content = crate::winffi::read_dylan_byte_string(crate::word::Word::from_raw(content_word));
    match std::fs::write(&path, content.as_bytes()) {
        Ok(()) => tag(1),
        Err(_) => tag(0),
    }
}

/// JIT-callable: show the Win32 common file-SAVE dialog and return the
/// chosen path as a fresh Dylan `<byte-string>` Word, or the `nil`
/// immediate if the user cancelled (or any system error). The dialog
/// is owned by `hwnd_word` (so it modals against the IDE window).
///
/// Mirrors `nod_show_open_file_dialog` exactly except it calls
/// `GetSaveFileNameW` (not `GetOpenFileNameW`) and the OFN flags are
/// `OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST`:
///   * `OFN_OVERWRITEPROMPT` — if the user picks an existing file
///     name, ask them to confirm overwrite. This is the standard
///     Windows save-dialog UX.
///   * `OFN_PATHMUSTEXIST` — the *directory* in the chosen path must
///     exist. The file itself may not yet (that's the whole point of
///     "save as"). We deliberately do NOT pass `OFN_FILEMUSTEXIST`
///     here; the open dialog uses that flag, the save dialog must not.
///
/// # Safety
/// `hwnd_word` must be a Dylan fixnum encoding a valid (or 0 = no
/// owner) HWND. The dialog blocks the calling thread until the user
/// confirms or cancels.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn nod_show_save_file_dialog(hwnd_word: u64) -> u64 {
    use windows::Win32::UI::Controls::Dialogs::{
        GetSaveFileNameW, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    use windows::core::{PCWSTR, PWSTR};

    let hwnd_raw = untag_i64(hwnd_word);
    let hwnd_h = HWND(hwnd_raw as *mut std::ffi::c_void);

    // Same filter shape as the open-dialog shim — keeps the IDE's
    // save / open dialogs feeling consistent.
    let filter_utf8 =
        "Dylan (*.dylan)\0*.dylan\0Text (*.txt)\0*.txt\0All (*.*)\0*.*\0\0";
    let filter: Vec<u16> = filter_utf8.encode_utf16().collect();

    // 260 = MAX_PATH on classic Win32; same buffer sizing rationale
    // as the open-dialog shim.
    let mut path_buf: [u16; 260] = [0u16; 260];

    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd_h,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(path_buf.as_mut_ptr()),
        nMaxFile: path_buf.len() as u32,
        Flags: OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST,
        ..unsafe { std::mem::zeroed() }
    };

    // SAFETY: ofn fully populated; lpstrFile points to path_buf which
    // lives for the duration of this call; lpstrFilter points to the
    // filter Vec which also lives for the duration.
    let ok = unsafe { GetSaveFileNameW(&mut ofn) };
    drop(filter);

    if !ok.as_bool() {
        // Cancel or error — both surface as nil (the Dylan-side
        // caller's `result = nil` check covers both).
        return crate::literal_pool_immediates().nil.raw();
    }

    let len = path_buf.iter().position(|&c| c == 0).unwrap_or(path_buf.len());
    let s = String::from_utf16_lossy(&path_buf[..len]);
    crate::intern_string_literal(&s).raw()
}

// Sprint 41g's recent-files persistence shims (`nod_load_recent` /
// `nod_add_recent`) and `nod_basename` were retired in Sprint 42a Phase
// E — all that logic now lives in pure Dylan in
// `tests/nod-tests/fixtures/nod-ide.dylan` over the byte-string
// methods (`size`, `element`, `concatenate`, `copy-sequence`, `=`) and
// the `%read-file` / `%write-file` primitives.

// ─── UTF-16 helpers ───────────────────────────────────────────────────────

/// Decode a Dylan `<byte-string>` Word's UTF-8 payload to a
/// null-terminated UTF-16LE `Vec<u16>` suitable for a PCWSTR arg.
/// Returns a vec containing the U16 units followed by a 0 terminator.
fn utf16_from_dylan_byte_string(w: u64) -> Vec<u16> {
    let word = crate::word::Word::from_raw(w);
    let s = crate::winffi::read_dylan_byte_string(word);
    let mut out: Vec<u16> = s.encode_utf16().collect();
    out.push(0);
    out
}

/// Decode a Dylan `<byte-string>` Word's UTF-8 payload to a
/// `Vec<u16>` WITHOUT a trailing null — for `&[u16]`-shaped args
/// like `IDWriteFactory::CreateTextLayout` that take a length-
/// prefixed slice.
fn utf16_from_dylan_byte_string_no_null(w: u64) -> Vec<u16> {
    let word = crate::word::Word::from_raw(w);
    let s = crate::winffi::read_dylan_byte_string(word);
    s.encode_utf16().collect()
}

// ─── Sprint 35 — registered classes ──────────────────────────────────────

/// Idempotent — registers `<c-float>` and `<c-double>` Dylan classes
/// alongside the existing c-type seed classes. Sprint 35 deviation
/// (see module docs): these are *registered* but not currently
/// *exercised* by the shim functions — Sprint 35 shims take Dylan
/// `<integer>` args and convert to f32 internally. Future sprints
/// will wire float-marshaling trampoline variants and route these
/// kinds through to the shim signatures directly.
pub fn ensure_com_types_registered() {
    crate::c_types::ensure_registered();
    crate::c_types::ensure_float_types_registered();
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial(com_registry)]
    fn registry_releases_drop_com_objects() {
        _reset_registry_for_tests();
        assert_eq!(registry_len(), 0);
        // Create a DXGI factory, register it, release it. The
        // `windows` crate's Drop calls Release.
        // SAFETY: SDK-level call.
        let f: windows::core::Result<IDXGIFactory2> =
            unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)) };
        let f = f.expect("CreateDXGIFactory2 should succeed");
        let h = register(ComObject::DxgiFactory(f));
        assert_eq!(registry_len(), 1);
        assert!(release(h));
        assert_eq!(registry_len(), 0);
    }

    #[test]
    #[serial(com_registry)]
    fn release_returns_false_for_unknown_handle() {
        _reset_registry_for_tests();
        assert!(!release(99999));
    }

    #[test]
    #[serial(com_registry)]
    fn ten_register_then_release_clears_the_map() {
        _reset_registry_for_tests();
        let mut handles = Vec::new();
        for _ in 0..10 {
            // SAFETY: SDK-level call.
            let f: windows::core::Result<IDXGIFactory2> =
                unsafe { CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0)) };
            let f = f.expect("factory");
            handles.push(register(ComObject::DxgiFactory(f)));
        }
        assert_eq!(registry_len(), 10);
        for h in handles {
            assert!(release(h));
        }
        assert_eq!(registry_len(), 0);
    }
}
