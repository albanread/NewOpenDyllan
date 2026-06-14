//! **Stdlib boundary**: new format-string APIs go in
//! `src/nod-dylan/dylan-sources/stdlib.dylan`. This file is the JIT
//! shim that connects Dylan's `format-out(fmt, ...)` call to the OS
//! stdout write — that part stays here per `docs/STDLIB_BOUNDARY.md`
//! (Rule 2: FFI/OS). Format-spec parsing and richer formatting (named
//! args, padding, alignment) belong in Dylan over the primitives.
//!
//! `format-out` JIT shim — the one Sprint 10 well-known intrinsic.
//!
//! The JIT'd code calls `nod_format_out(fmt, arg1, arg2, arg3)` where
//! every argument is a tagged Dylan `Word` packed into a `u64`. The
//! format string is a `<byte-string>`-tagged pointer; the args are
//! either fixnums (`%d`) or `<byte-string>`s (`%s`). The full `format`
//! library lands in Sprint 24 — this is intentionally the minimum to
//! demo "hello, world\n" and "%d" interpolation through the JIT.
//!
//! Format directives:
//!   - `%d`  fixnum (or `<integer>` once Sprint 12 boxes large ints)
//!   - `%s`  `<byte-string>`
//!   - `%%`  literal `%`
//!
//! Bad inputs (wrong tag, missing arg) write a diagnostic to stderr
//! and continue — no abort. The shim returns `0u64`; the caller
//! ignores it (Sprint 10 type estimate for `format-out` is `<unit>`).

use std::io::Write;

use crate::classes::ClassId;
use crate::strings::ByteString;
use crate::wrapper::Wrapper;
use crate::word::Word;

// Test hook: when `Some`, writes go into this buffer (cloned per test
// thread) instead of stdout. The integration tests install + drain it
// around each `eval` call.
thread_local! {
    static TEST_WRITER: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
}

/// Install a thread-local capture buffer. Subsequent `nod_format_out`
/// calls from THIS thread write into the buffer. `take_test_writer`
/// drains and reinstalls.
pub fn install_test_writer() {
    TEST_WRITER.with(|c| *c.borrow_mut() = Some(Vec::new()));
}

/// Drain the thread-local capture buffer, returning its contents and
/// clearing it. Returns `None` if no capture is active.
pub fn take_test_writer() -> Option<Vec<u8>> {
    TEST_WRITER.with(|c| {
        let mut g = c.borrow_mut();
        let out = g.take();
        // Reinstall an empty buffer so subsequent writes are still captured.
        *g = Some(Vec::new());
        out
    })
}

/// Uninstall the thread-local capture buffer; subsequent writes go
/// back to stdout.
pub fn uninstall_test_writer() {
    TEST_WRITER.with(|c| *c.borrow_mut() = None);
}

fn write_bytes(bytes: &[u8]) {
    let captured = TEST_WRITER.with(|c| {
        let mut g = c.borrow_mut();
        if let Some(buf) = g.as_mut() {
            buf.extend_from_slice(bytes);
            true
        } else {
            false
        }
    });
    if !captured {
        let stdout = std::io::stdout();
        let mut h = stdout.lock();
        let _ = h.write_all(bytes);
    }
}

fn diag(msg: &str) {
    let stderr = std::io::stderr();
    let mut h = stderr.lock();
    let _ = writeln!(h, "format-out: {msg}");
}

/// Decode a tagged `Word` as a `<byte-string>` if its wrapper class
/// matches. Returns the inline byte slice.
///
/// # Safety
///
/// `w` must be either a fixnum (returns `None`) or a pointer-tagged
/// Word whose target is a valid heap object whose first 8 bytes are
/// a `Wrapper`. The class-tag check then guarantees the rest of the
/// layout.
unsafe fn decode_byte_string<'a>(w: Word, byte_string_class: ClassId) -> Option<&'a [u8]> {
    let p = w.as_ptr::<u8>()?;
    // SAFETY: caller asserts pointer is to a wrapper-first object.
    let wrapper: Wrapper = unsafe { *(p as *const Wrapper) };
    if wrapper.class() != byte_string_class {
        return None;
    }
    // SAFETY: class match implies ByteString layout.
    let bs = unsafe { &*(p as *const ByteString) };
    // SAFETY: ByteString invariant.
    Some(unsafe { bs.bytes() })
}

/// Find `<byte-string>`'s ClassId without taking a `ClassTable`. The
/// class id is a compile-time constant by Sprint 09 design.
fn byte_string_class() -> ClassId {
    ClassId::BYTE_STRING
}

/// JIT-callable formatter. Accepts up to three Dylan-tagged args.
///
/// # Safety
///
/// `fmt` must be a tagged pointer to a valid `<byte-string>`. Each
/// `argN` must be either a fixnum or a tagged pointer to a valid
/// `<byte-string>`. The caller (JIT-emitted code) is responsible for
/// only invoking this with well-typed Dylan values; mis-tagged inputs
/// trigger a stderr diagnostic but never UB.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nod_format_out(fmt: u64, arg1: u64, arg2: u64, arg3: u64) -> u64 {
    let fmt_w = Word::from_raw(fmt);
    let args = [Word::from_raw(arg1), Word::from_raw(arg2), Word::from_raw(arg3)];
    // SAFETY: caller-asserted invariant — `fmt` is a `<byte-string>`
    // pointer or a fixnum. Both cases are gracefully handled.
    let fmt_bytes = match unsafe { decode_byte_string(fmt_w, byte_string_class()) } {
        Some(b) => b,
        None => {
            diag(&format!("format string is not a <byte-string> (raw {fmt:#x})"));
            return 0;
        }
    };
    let mut out: Vec<u8> = Vec::with_capacity(fmt_bytes.len() + 16);
    let mut next_arg: usize = 0;
    let mut i = 0;
    while i < fmt_bytes.len() {
        let c = fmt_bytes[i];
        if c != b'%' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        if i >= fmt_bytes.len() {
            diag("trailing `%` in format string");
            out.push(b'%');
            break;
        }
        match fmt_bytes[i] {
            b'%' => {
                out.push(b'%');
                i += 1;
            }
            b'd' => {
                if next_arg >= args.len() {
                    diag("too few arguments for `%d`");
                    out.extend_from_slice(b"<missing>");
                } else {
                    let a = args[next_arg];
                    next_arg += 1;
                    match a.as_fixnum() {
                        Some(n) => {
                            let s = n.to_string();
                            out.extend_from_slice(s.as_bytes());
                        }
                        None => {
                            diag(&format!("`%d` expects fixnum, got raw {:#x}", a.raw()));
                            out.extend_from_slice(b"<bad-int>");
                        }
                    }
                }
                i += 1;
            }
            b's' => {
                if next_arg >= args.len() {
                    diag("too few arguments for `%s`");
                    out.extend_from_slice(b"<missing>");
                } else {
                    let a = args[next_arg];
                    next_arg += 1;
                    // SAFETY: same caller-invariant as `fmt`.
                    match unsafe { decode_byte_string(a, byte_string_class()) } {
                        Some(b) => out.extend_from_slice(b),
                        None => {
                            diag(&format!(
                                "`%s` expects <byte-string>, got raw {:#x}",
                                a.raw()
                            ));
                            out.extend_from_slice(b"<bad-str>");
                        }
                    }
                }
                i += 1;
            }
            other => {
                diag(&format!("unsupported directive `%{}`", other as char));
                out.push(b'%');
                out.push(other);
                i += 1;
            }
        }
    }
    write_bytes(&out);
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classes::ClassTable;
    use crate::heap::Heap;

    fn capture<F: FnOnce()>(f: F) -> Vec<u8> {
        install_test_writer();
        f();
        let out = take_test_writer().unwrap_or_default();
        uninstall_test_writer();
        out
    }

    #[test]
    fn literal_passes_through() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let fmt = heap.alloc_byte_string("hello\n", &ct);
        let buf = capture(|| {
            // SAFETY: `fmt` is a valid <byte-string> Word; other args
            // are unused fixnum zeros.
            unsafe { nod_format_out(fmt.raw(), 0, 0, 0) };
        });
        assert_eq!(&buf, b"hello\n");
    }

    #[test]
    fn percent_d_formats_fixnum() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let fmt = heap.alloc_byte_string("answer: %d\n", &ct);
        let n = Word::from_fixnum(42).unwrap();
        let buf = capture(|| {
            // SAFETY: fmt is <byte-string>, arg1 is a fixnum.
            unsafe { nod_format_out(fmt.raw(), n.raw(), 0, 0) };
        });
        assert_eq!(&buf, b"answer: 42\n");
    }

    #[test]
    fn percent_s_formats_byte_string() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let fmt = heap.alloc_byte_string("hi, %s\n", &ct);
        let name = heap.alloc_byte_string("world", &ct);
        let buf = capture(|| {
            // SAFETY: fmt and name are both <byte-string>.
            unsafe { nod_format_out(fmt.raw(), name.raw(), 0, 0) };
        });
        assert_eq!(&buf, b"hi, world\n");
    }

    #[test]
    fn percent_percent_emits_literal_percent() {
        let heap = Heap::new();
        let ct = ClassTable::new();
        let fmt = heap.alloc_byte_string("100%% done\n", &ct);
        let buf = capture(|| {
            // SAFETY: fmt is <byte-string>.
            unsafe { nod_format_out(fmt.raw(), 0, 0, 0) };
        });
        assert_eq!(&buf, b"100% done\n");
    }
}
