Module: gap011-repro2

// GAP-011 repro v2 — adds the parser's actual ingredients:
// classes with slots, generic dispatch (Dispatch in IR), and
// allocating sub-calls. Mirrors dump-node's pattern: walk a tree
// of nodes, push their label into a buffer.

define class <node> (<object>)
  slot node-label :: <byte-string>, init-keyword: label:;
  slot node-left  :: <object>, init-keyword: left:, init-value: #f;
  slot node-right :: <object>, init-keyword: right:, init-value: #f;
end class;

define function make-tree (depth :: <integer>) => (<object>)
  if (depth = 0)
    make(<node>, label: "leaf")
  else
    make(<node>,
         label: "node",
         left: make-tree(depth - 1),
         right: make-tree(depth - 1))
  end if
end function make-tree;

// Pushes every byte of `s` into `v` — the dump-node/acc-string pattern.
define function flood (v :: <stretchy-vector>, s :: <byte-string>) => ()
  let i = 0;
  while (i < size(s))
    add!(v, element(s, i));
    i := i + 1;
  end while;
end function flood;

// Recursively walks the tree, pushing each node's label into v.
define function walk (n :: <object>, v :: <stretchy-vector>) => ()
  if (instance?(n, <node>))
    flood(v, node-label(n));
    walk(node-left(n), v);
    walk(node-right(n), v);
  end if
end function walk;

define function main () => (<integer>)
  let tree = make-tree(12); // 2^12 = 4096 leaves → lots of nodes, lots of allocs
  let buf = make(<stretchy-vector>);
  walk(tree, buf);
  size(buf)
end function main;
