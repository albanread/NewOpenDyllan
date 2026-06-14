Module: nod-ide

// Sprint 44 — IDE module split (part 1 of 5: Win32 c-function decls).
//
// All `define c-function` declarations the IDE needs at compile time.
// Listed in this single file so the import surface to user32.dll is
// easy to audit. The rest of the IDE (rope, helpers, syntax/editor,
// main + WNDPROC) lives in sibling files; see `unified_ide.dylan`
// for the pre-split monolithic version preserved as a safety copy.

define c-function CreateWindowExW
  (dwExStyle :: <c-int>, lpClassName :: <c-pointer>, lpWindowName :: <c-wide-string>,
   dwStyle :: <c-int>, x :: <c-int>, y :: <c-int>, nWidth :: <c-int>, nHeight :: <c-int>,
   hWndParent :: <c-pointer>, hMenu :: <c-pointer>, hInstance :: <c-pointer>,
   lpParam :: <c-pointer>)
 => (hwnd :: <c-pointer>);
    library: "user32.dll";
end;

define c-function ShowWindow
  (hwnd :: <c-pointer>, nCmdShow :: <c-int>)
 => (was-visible :: <c-bool>);
    library: "user32.dll";
end;

define c-function UpdateWindow
  (hwnd :: <c-pointer>)
 => (success :: <c-bool>);
    library: "user32.dll";
end;

define c-function InvalidateRect
  (hwnd :: <c-pointer>, lpRect :: <c-pointer>, bErase :: <c-bool>)
 => (success :: <c-bool>);
    library: "user32.dll";
end;

define c-function DefWindowProcW
  (hwnd :: <c-pointer>, msg :: <c-int>,
   wparam :: <c-pointer>, lparam :: <c-pointer>)
 => (lresult :: <c-pointer>);
    library: "user32.dll";
end;

define c-function PostQuitMessage
  (exit-code :: <c-int>)
 => ();
    library: "user32.dll";
end;

// Sprint 41e — menu API declarations (explicit so the AppendMenuW
// 4th-arg lpNewItem stays `<c-wide-string>` for menu items; we pass
// the HMENU for popup submenus via the 3rd-arg `uIDNewItem` which is
// typed `<c-pointer>` to accept both fixnum ids and HMENU values).
define c-function CreateMenu
  ()
 => (hmenu :: <c-pointer>);
    library: "user32.dll";
end;

define c-function CreatePopupMenu
  ()
 => (hmenu :: <c-pointer>);
    library: "user32.dll";
end;

define c-function AppendMenuW
  (hmenu :: <c-pointer>, uFlags :: <c-int>, uIDNewItem :: <c-pointer>,
   lpNewItem :: <c-wide-string>)
 => (success :: <c-bool>);
    library: "user32.dll";
end;

// Sprint 41g — menu rebuild helpers. `RemoveMenu` with MF_BYPOSITION
// (1024) removes the item at the given index; positions shift after
// removal so calling with position 0 repeatedly tears the submenu
// down. `DrawMenuBar` forces the OS to repaint the menu bar after
// programmatic changes (the submenu's own popup is rebuilt on the
// next click so we don't have to invalidate it explicitly).
define c-function RemoveMenu
  (hmenu :: <c-pointer>, uPosition :: <c-int>, uFlags :: <c-int>)
 => (success :: <c-bool>);
    library: "user32.dll";
end;

define c-function DrawMenuBar
  (hwnd :: <c-pointer>)
 => (success :: <c-bool>);
    library: "user32.dll";
end;

// SetWindowTextW is the Help → About workaround (see Sprint 41e
// notes) and is also what we use for the per-file title.
define c-function SetWindowTextW
  (hwnd :: <c-pointer>, lpString :: <c-wide-string>)
 => (success :: <c-bool>);
    library: "user32.dll";
end;

define c-function MessageBoxW
  (hwnd :: <c-pointer>, lpText :: <c-wide-string>, lpCaption :: <c-wide-string>,
   uType :: <c-int>)
 => (result :: <c-int>);
    library: "user32.dll";
end;
