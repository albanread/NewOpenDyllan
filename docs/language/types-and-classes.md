# Types & Classes

Every value in Dylan belongs to a class. Classes are types: they describe
what operations a value supports, and dispatch selects methods based on
which classes an argument's runtime type belongs to.

## Defining a class

```dylan
define class <shape> (<object>)
end class;

define class <circle> (<shape>)
  slot radius :: <integer>, init-keyword: radius:;
end class;
```

The full form is:

```
define [modifiers] class <name> (<super>, …)
  [slot …;]
  …
end class;
```

**Angle-bracket naming.** The `<name>` convention is a discipline, not
syntax: any identifier may be a class name, but the angle-bracket pair
signals "this is a class" at a glance. The convention is universal across
the Dylan standard library and all NewOpenDylan fixtures.

**Superclass list.** Every class lists its direct superclasses in
parentheses. Omitting a superclass list implicitly means `(<object>)` —
the sema phase inserts `<object>` when the list is empty. Multiple entries
give multiple inheritance:

```dylan
define class <my-class> (<base-a>, <base-b>)
  …
end class;
```

Sema runs **C3 linearisation** over the parent class precedence lists (CPLs)
to produce a consistent CPL. If two parents impose conflicting orders on a
shared ancestor, the lowerer signals an inconsistent-inheritance error. See
[Semantic analysis](../compiler/sema.md) for the algorithm.

**Class adjectives.** The parser accepts `open`, `sealed`, `abstract`,
`concrete`, and `primary` as modifier keywords before `class`. Their effect
in this implementation:

| Adjective | Parser | Sema / runtime effect |
|-----------|--------|-----------------------|
| `sealed` | recorded | Sets the class's `sealed` flag; cross-library subclassing is refused at compile time |
| `open` | recorded | parsed, not enforced beyond being the absence of `sealed` |
| `abstract` | recorded | parsed; no enforcement today — `make(<abstract-class>)` is not blocked at compile time |
| `concrete` | recorded | parsed only |
| `primary` | recorded | parsed only |

See [Sealing](sealing.md) for the full sealing contract.

## Slots and accessors

A slot is a per-instance field. The parser and sema recognise these
slot options:

| Option | Example | Effect |
|--------|---------|--------|
| `:: <type>` | `slot x :: <integer>` | Type annotation; influences the slot's type used by the GC scanner |
| `init-keyword: k:` | `init-keyword: radius:` | The keyword argument `make(<C>, k: v)` writes `v` into this slot on construction |
| `required-init-keyword: k:` | `required-init-keyword: x:` | Like `init-keyword:` but the field is marked required; no runtime enforcement yet — the flag is recorded |
| `init-value: expr` | `init-value: 0` | Literal default (integer, `#t`, or `#f`) written when the keyword is absent |
| `setter: #f` | `slot n, setter: #f` | Suppresses getter/setter generation when false |

**Slot allocation.** Only `instance:` (the default) is supported.
`class:`, `each-subclass:`, and `virtual:` raise an
unsupported-slot-allocation error.

**`init-function:`** is parsed by the AST but not yet wired into sema's
slot-default logic — any non-literal, non-boolean `init-value` falls
through to an unbound default. This is a known open item.

**Auto-generated accessors.** Sema emits a getter and a setter for every
slot. For a slot named `radius` on `<circle>`:

- **Getter:** `radius(obj)` — calls the generated getter function, registered
  as a method on the generic `radius` specialised to `<circle>`.
- **Setter:** `radius(obj) := v` — lowers to `radius-setter(obj, v)`,
  registered as a method on `radius-setter` with specialisers
  `[<circle>, <object>]`.

Both participate in normal generic dispatch, so a subclass can override
either by defining its own method on the same generic.

**Inherited slots.** In single inheritance the inherited slot offset is
unchanged; in multiple inheritance the offset may shift. When it does,
sema emits an **override accessor** for the subclass so the right offset is
used per receiver. Callers write the same `x(obj)` call regardless.

The point-3d example demonstrates inherited slot access:

```dylan
define class <point-2d> (<object>)
  slot x :: <integer>, init-keyword: x:;
  slot y :: <integer>, init-keyword: y:;
end class;

define class <point-3d> (<point-2d>)
  slot z :: <integer>, init-keyword: z:;
end class;

define function sum-coords (p :: <point-3d>) => (<integer>)
  x(p) + y(p) + z(p)        // inherited + own accessors, same syntax
end function sum-coords;
```

## Making instances

`make` allocates a new instance and initialises its slots:

```dylan
let c = make(<circle>, radius: 2);
let s = make(<square>, side: 5);
```

The runtime allocates a heap object with an 8-byte `Wrapper` header
followed by one 8-byte slot per entry in the C3-merged CPL, then walks each
slot's metadata: if the matching keyword argument was supplied, that value is
written; otherwise `init-value:` is used; if neither, the slot is left
unbound. See [Runtime & object model](../compiler/runtime.md) for the heap
layout.

```mermaid
flowchart TD
    CALL[make - class - keyword args] --> ALLOC[allocate heap object]
    ALLOC --> WRAP[write Wrapper header - ClassId plus GC bits]
    WRAP --> SLOTS[iterate slot list - write keyword value or init-value]
    SLOTS --> DONE[return tagged Word pointer to new object]
```

The lowerer routes `make(<C>, k: v, …)` to a `Make` computation in DFM,
which generates the allocation call in the LLVM IR. The narrowing pass then
records the result as having class `<C>`, enabling sealed-dispatch rewrites
on subsequent operations. See [Semantic analysis](../compiler/sema.md).

After all slots are written, the allocation returns the object. The Dylan
DRM specifies that `initialize` is then called; in this implementation
user-defined `initialize` methods can be added as ordinary methods on the
`initialize` generic and will be reached via dispatch after allocation.

## Class hierarchy — a real example

The `area-shapes` example defines a two-level hierarchy with a generic
function dispatched on the concrete leaves:

```dylan
define class <shape> (<object>)
end class;

define class <circle> (<shape>)
  slot radius :: <integer>, init-keyword: radius:;
end class;

define class <square> (<shape>)
  slot side :: <integer>, init-keyword: side:;
end class;

define generic area (s :: <shape>) => (<integer>);

define method area (c :: <circle>) => (<integer>)
  radius(c) * radius(c) * 3
end method area;

define method area (s :: <square>) => (<integer>)
  side(s) * side(s)
end method area;
```

```mermaid
classDiagram
    class object {
        <<root>>
    }
    class shape {
        +area(s) integer
    }
    class circle {
        +radius integer
        +area(c) integer
    }
    class square {
        +side integer
        +area(s) integer
    }
    object <|-- shape
    shape <|-- circle
    shape <|-- square
```

An open class hierarchy shows the `open` adjective and four sibling
subclasses:

```mermaid
classDiagram
    class task {
        <<open>>
        +run-task(t, packet) integer
    }
    class idler {
        <<open>>
        +id-state integer
    }
    class worker {
        <<open>>
        +wk-state integer
    }
    class handler {
        <<open>>
        +h-state integer
    }
    class device {
        <<open>>
        +d-state integer
    }
    task <|-- idler
    task <|-- worker
    task <|-- handler
    task <|-- device
```

## Classes are types

**`instance?`** tests class membership at runtime:

```dylan
instance?(make(<circle>, radius: 2), <circle>)   // #t
instance?(make(<circle>, radius: 2), <shape>)    // #t — subclass relation
instance?(42, <integer>)                         // #t
instance?(42, <boolean>)                         // #f
```

`instance?(x, <C>)` lowers to a `TypeCheck` computation in DFM; sema
compiles it to a runtime instance check, which walks `x`'s class CPL via the
subclass test. The narrowing pass uses the result to refine type estimates
on the then-branch of an enclosing `if`.

**`<object>` is the root.** Every class has `<object>` in its CPL. A
method specialised on `<object>` is the most general possible; one
specialised on a leaf class is the most specific. The dispatch ordering
is CPL-driven: earlier in the CPL means more specific.

**Sealed vs open.** A `sealed` class refuses cross-library subclassing.
A class without the `sealed` modifier can be subclassed in any library. The
`open` adjective makes the intent explicit. See [Sealing](sealing.md) for
dispatch implications.

## Built-in types

NewOpenDylan registers these classes at process boot. They are available
in any Dylan program without an import declaration.

| Class | Kind | Notes |
|-------|------|-------|
| `<object>` | root | every class inherits from it |
| `<integer>` | immediate | 63-bit signed fixnum; encoded as `(n << 1)` in a 64-bit Word |
| `<boolean>` | immediate | `#t` and `#f` are pinned singleton heap objects |
| `<byte-string>` | heap | byte-array string; UTF-8 by convention; `<string>` is an alias |
| `<symbol>` | heap | interned, identity-comparable byte-string |
| `<pair>` | heap | cons cell — `head` and `tail` accessors |
| `<empty-list>` | heap | `nil()` — the end-of-list singleton |
| `<simple-object-vector>` | heap | fixed-length vector of Words |
| `<stretchy-vector>` | heap | growable vector |
| `<range>` | heap | integer range with `from`, `to`, `by` slots |
| `<table>` | heap | open-addressing hash table |
| `<function>` | heap | first-class function/closure |
| `<condition>` | heap | base of the condition hierarchy |

**The fixnum range.** `<integer>` encodes integers as fixnums in the
range `-(2^62) .. 2^62 - 1`. Integers outside this range are rejected at
compile time with an integer-overflow error; runtime overflow wraps
silently (no overflow trap). See [Runtime & object model](../compiler/runtime.md)
for the Word encoding.

**`<big-integer>` is not yet implemented.** A Dylan `<big-integer>` class
covering the full arbitrary-precision integer tower does not exist today.
Code that needs values above `2^62 - 1` cannot use `<integer>`.

**Floats are partially implemented.** `<single-float>` and
`<double-float>` literals can appear in expressions but return as
unboxed values from the JIT; float-typed slots are stored as pointer-
shaped Words. Float arithmetic works in practice; the full boxed-float ABI
is not yet implemented.

**`singleton` and `limited` types are not implemented.** Dylan's
`singleton(<T>, value)` and `limited(<integer>, min: …, max: …)` type
constructors do not exist in this implementation.

**Numeric tower summary.**

```mermaid
classDiagram
    class object {
        <<root>>
    }
    class number {
        <<planned>>
    }
    class integer {
        <<fixnum 63-bit>>
        range minus2pow62..2pow62minus1
    }
    class big-integer {
        <<not yet implemented>>
    }
    class float {
        <<partial>>
    }
    object <|-- number
    number <|-- integer
    number <|-- big-integer
    number <|-- float
```

## How it is implemented

This section is a pointer, not a repeat. For the compiler view:

- **Class registration, C3, slot layout:** [Semantic analysis](../compiler/sema.md) —
  class registration, the sealed-flag flip, and accessor emission.
- **Heap layout, the Wrapper header, slot offsets:** [Runtime & object model](../compiler/runtime.md) —
  "Heap object layout" and "The tagged Word".
- **Dispatch on class:** [Generic functions](generic-functions.md) —
  how accessors and user methods are resolved per receiver class.
- **GC scanning of slots:** [Garbage collector](../compiler/gc.md).

---
Next: [Generic functions & dispatch](generic-functions.md) · See also [Sealing](sealing.md)
