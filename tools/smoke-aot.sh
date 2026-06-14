#!/usr/bin/env bash
# tools/smoke-aot.sh — AOT build+run smoke guard.
#
# WHY THIS EXISTS: the dump-dfm / dump-ast / eval guards check that source
# LOWERS, not that a built EXE RUNS. A change can keep dump-dfm green (and even
# the in-tree 55/55 dfm guard) while breaking AOT executables — e.g. runtime
# class-id drift (codegen bakes a class id the EXE runtime allocates
# differently) or a function/operator shim registered in the JIT path but not
# the AOT runtime path. Those only surface when you BUILD an .exe and RUN it.
#
# This script builds a handful of self-contained programs through the real AOT
# pipeline (`--parse-with-rust build`) and asserts their stdout. Run it after
# any change to nod-runtime class/shim registration, nod-sema lowering, or the
# AOT codegen path. Exit 0 = all pass.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DRIVER="$ROOT/target/debug/nod-driver.exe"
WORK="${TMPDIR:-/tmp}/nod-smoke-aot"
mkdir -p "$WORK"
fail=0

# case NAME  EXPECTED-STDOUT  <<DYLAN-SOURCE
case_run() {
  local name="$1" expected="$2"; shift 2
  local src="$WORK/$name.dylan" exe="$WORK/$name.exe"
  cat > "$src"
  if ! "$DRIVER" --parse-with-rust build "$src" -o "$exe" >/dev/null 2>"$WORK/$name.berr"; then
    echo "FAIL $name: build error"; sed 's/^/    /' "$WORK/$name.berr" | head -4; fail=1; return
  fi
  local got; got="$("$exe" 2>"$WORK/$name.rerr" | tr -d '\r')"
  if [ "$got" != "$expected" ]; then
    echo "FAIL $name: expected [$expected] got [$got]"
    head -3 "$WORK/$name.rerr" | sed 's/^/    /'; fail=1; return
  fi
  echo "ok   $name"
}

case_run arith "30" <<'EOF'
Module: t
define function main () => () format-out("%d\n", 6 * 5); end function main;
EOF

case_run forsum "5050" <<'EOF'
Module: t
define function s (n)
  let a = 0;
  for (i from 1 to n) a := a + i end;
  a
end function;
define function main () => () format-out("%d\n", s(100)); end function main;
EOF

case_run localmethod "120" <<'EOF'
Module: t
define function fact (n)
  local method go (k, acc) if (k <= 1) acc else go(k - 1, k * acc) end end method;
  go(n, 1)
end function;
define function main () => () format-out("%d\n", fact(5)); end function main;
EOF

case_run zeromethod "42" <<'EOF'
Module: t
define method answer () 6 * 7 end method;
define function main () => () format-out("%d\n", answer()); end function main;
EOF

# block(return) non-local exit through a built AOT exe (guards the iter-14 fix:
# AOT safepoint-stack truncation + extern "C-unwind" + block_id fixnum domain).
case_run blockreturn "99" <<'EOF'
Module: t
define function trial (n)
  block (return)
    if (n > 3) return(99) end;
    7
  end
end function;
define function main () => () format-out("%d\n", trial(5)); end function main;
EOF

# == / instance? as first-class function references in a built exe (guards the
# iter-14 func-ref shims being live in the AOT runtime path, not JIT-only).
case_run funcref "1" <<'EOF'
Module: t
define function b (x) => (n) if (x) 1 else 0 end end function;
define function main () => ()
  let eq = \==;
  format-out("%d\n", b(eq(7, 7)));
end function main;
EOF

echo ""
if [ "$fail" = 0 ]; then echo "AOT SMOKE OK (all cases built + ran with expected output)."; else
  echo "AOT SMOKE FAILED."; exit 1; fi
