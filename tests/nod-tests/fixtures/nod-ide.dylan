Module: nod-ide
Precedence: c

// Sprint 44 — IDE module split (part 5 of 5: main + handle-wm-message).
//
// This file holds the WNDPROC dispatcher and the `main` function that
// builds the device chain, registers the window class, creates the
// HWND, runs the message loop, and tears down. Everything it needs is
// defined in the sibling files (built together by `nod-driver`):
//
//   ide_win_calls.dylan   — Win32 c-function declarations
//   ide_rope.dylan        — rope buffer + line-count / max-line-chars
//   ide_helpers.dylan     — pure-Dylan text / list / recent-files / title
//   ide_syntax.dylan      — cursor + scan + syntax-colour + gutter
//   nod-ide.dylan         — (this file) main + WNDPROC entry
//
// The pre-split monolithic version is preserved verbatim at
// `unified_ide.dylan` for diffing / rollback.

// Sprint 41g — File menu (Open / Save / Save As / Recent) on top of the
// Sprint 41e File / Help menu bar. The window title shows the current
// file's basename, e.g. "foo.dylan - NewOpenDylan IDE". The recent
// files list persists across runs in F:\scratch\nod-ide-recent.txt
// (most-recent first, capped at 5).
//
// MessageBoxW-from-WNDPROC remains broken (Sprint 41f investigation,
// see docs/duim-research/07-probe-findings.md); Help → About still
// uses the SetWindowTextW workaround.

define function main () => ()
  let arg-path = %argv1();
  // Sprint 43d — the buffer is a `<rope>` now. Load the file (or the
  // no-file placeholder) into a flat byte-string, then wrap it via
  // make-rope-from-string so every later read / edit goes through
  // the rope's O(log n) ops.
  let initial-bytes = if (empty?(arg-path))
                        "nod-ide: no argv[1] supplied; pass a Dylan source path as the first argument."
                      else
                        let bytes = %read-file(arg-path);
                        if (empty?(bytes))
                          "nod-ide: could not read the file passed via argv[1]."
                        else
                          bytes
                        end
                      end;
  let source-text = make-rope-from-string(initial-bytes);
  // Sprint 43d hotfix — cache the serialised flat-string view of the
  // rope across WM_PAINT calls. The old byte-string buffer cost zero
  // allocation per paint (the byte-string Word was passed straight
  // to DirectWrite); the rope serialisation costs O(n) per paint.
  // Win32 sends WM_PAINT on InvalidateRect, focus changes, drags,
  // etc., so caching is the difference between "stable IDE" and
  // "GC pressure crash". Invalidate the cache (set to "") at every
  // mutation site — Open, Recent, WM_CHAR, VK_BACK, Save-As reload.
  let cached-flat = initial-bytes;
  // Re-entrancy guard: set to 1 while a modal dialog (Open/Save As) is
  // open. GetSaveFileNameW / GetOpenFileNameW dispatch WM_ACTIVATE,
  // WM_NCACTIVATE, WM_PAINT, etc. back to our WndProc via SendMessageW.
  // Skipping WM_PAINT and WM_TIMER while in-modal-dialog avoids passing
  // stale/zero handles to DirectWrite in the re-entrant call.
  let in-modal-dialog = 0;
  // Sprint 43d — cursor-offset is the byte position where the next
  // WM_CHAR insertion lands (and what backspace removes the byte
  // before). Captured by the WNDPROC closure → auto-promoted to a
  // cell. Sprint 43e will surface this as a visible blinking
  // caret + click-to-position; for now it tracks invisibly so we
  // can prove insert/delete plumbing works end-to-end.
  let cursor-offset = 0;
  // Sprint 43e-6 — blinking cursor. SetTimer(500ms) toggles this
  // cell on every WM_TIMER; WM_PAINT only draws the cursor bar
  // when it's 1. Each cursor-mutating handler resets it to 1 so
  // the cursor stays solid during active typing/movement.
  let cursor-on = 1;
  // Sprint 43e-7 — ideal-column memory. Every horizontal cursor
  // move (left/right/home/end/click/typing/backspace) updates this
  // to the cursor's current column. Vertical moves (up/down/pgup/
  // pgdn) pass it to move-cursor-vertical so the cursor restores
  // to the original column when a long → short → long walk crosses
  // a shorter intermediate line.
  let ideal-col = 0;
  // Sprint 43e-8 — track Ctrl modifier state manually. WM_KEYDOWN
  // for VK_CONTROL (17) sets this to 1; WM_KEYUP (msg 257) for
  // VK_CONTROL clears it to 0. Avoids needing GetKeyState (which
  // isn't currently in the Win32 projection for unknown reasons
  // — investigate separately as a follow-up).
  let ctrl-down = 0;
  // Sprint 41g — current-path is a captured cell (Sprint 24 auto cell
  // promotion: any `let`-bound name assigned inside the WNDPROC
  // closure becomes a cell). Same machinery that promoted source-text
  // in Sprint 41e.
  let current-path = arg-path;
  let recent-paths = nod-load-recent();
  let d3d-device   = %d3d11-create-device();
  let dxgi-factory = %dxgi-factory-from-d3d-device(d3d-device);
  let dxgi-device  = %dxgi-device-from-d3d-device(d3d-device);
  let d2d-factory  = %d2d-create-factory();
  let d2d-device   = %d2d-create-device(d2d-factory, dxgi-device);
  let dc           = %d2d-create-device-context(d2d-device);
  let dwrite       = %dwrite-create-factory();
  let format       = %dwrite-create-text-format(dwrite, "Consolas", 1400, "en-us");
  // Sprint 43g-fix — force uniform 18.0 DIP line height with baseline
  // at 14.4 DIPs so DirectWrite's line stride matches the Dylan-side
  // line-height constant exactly. Without this, Consolas's natural
  // line height (~17 px at 14 DIPs) produces a cumulative 1 px/line
  // drift that makes the gutter's line numbers visibly slide up/down
  // by one relative to the source text as the user scrolls.
  // Encoding: x10 tag — 180 means 18.0 DIPs, 144 means 14.4 DIPs.
  %dwrite-set-line-spacing(format, 180, 144);
  let buffer-lines    = nod-rope-line-count(source-text);
  let buffer-max-cols = nod-rope-max-line-chars(source-text);
  let char-width  = 8;
  let line-height = 18;
  let pad = 8;
  // Sprint 43g — left gutter. Three columns reserved:
  //   * fold-gutter   — placeholder for collapse/expand triangles
  //   * error-gutter  — placeholder for diagnostic markers
  //   * line-num-gutter — visible 1-based line numbers (functional)
  // total-gutter-px is the width added BEFORE pad on the left side
  // of the source-text viewport. Everywhere the text used to be
  // drawn at x = pad - scroll-x-px, it now uses x = gutter-px + pad
  // - scroll-x-px. Click handler subtracts gutter-px from the
  // client X before computing the buffer position; clicks landing
  // inside the gutter are ignored (no cursor move).
  let fold-gutter-px     = 14;
  let error-gutter-px    = 14;
  let line-num-gutter-px = 40;     // fits 5 monospace digits at 8 px each
  let gutter-px = fold-gutter-px + error-gutter-px + line-num-gutter-px;
  let client-width-px  = buffer-max-cols * char-width;
  let client-height-px = buffer-lines * line-height;
  let window-width    = 1024;
  let window-height   = 768;
  let viewport-width-px  = 1024;
  let viewport-height-px = 768;
  let scroll-x-px = 0;
  let scroll-y-px = 0;
  let swap   = 0;
  let bitmap = 0;
  // Sprint 41g — build the menu bar HERE (before the WNDPROC closure
  // captures `recent-menu`) so the WM_COMMAND handler can call
  // `rebuild-recent-submenu` on `recent-menu` when the recent list
  // changes.
  let menu-bar = CreateMenu();
  let file-menu = CreatePopupMenu();
  let recent-menu = CreatePopupMenu();
  // AppendMenuW flag values (Win32 MF_*):
  //   MF_STRING    = 0      — plain text item (default)
  //   MF_GRAYED    = 1      — disabled / greyed
  //   MF_POPUP     = 16     — uIDNewItem is a submenu HMENU
  //   MF_SEPARATOR = 2048   — horizontal divider (lpNewItem ignored)
  AppendMenuW(file-menu, 0,    100, "&Open...\tCtrl+O");
  AppendMenuW(file-menu, 0,    101, "&Save\tCtrl+S");
  AppendMenuW(file-menu, 0,    102, "Save &As...\tCtrl+Shift+S");
  AppendMenuW(file-menu, 2048, 0,   "");
  AppendMenuW(file-menu, 16,   recent-menu, "&Recent");
  AppendMenuW(file-menu, 2048, 0,   "");
  AppendMenuW(file-menu, 0,    199, "E&xit\tAlt+F4");
  AppendMenuW(menu-bar,  16,   file-menu, "&File");
  let help-menu = CreatePopupMenu();
  AppendMenuW(help-menu, 0,    200, "&About");
  AppendMenuW(menu-bar,  16,   help-menu, "&Help");
  rebuild-recent-submenu(recent-menu, recent-paths);
  // Sprint 43e-4 — auto-scroll-to-cursor helper.
  //
  // Called by every cursor-mutating handler (arrow keys, Home/End,
  // WM_CHAR, VK_BACK). Computes the cursor's pixel position in
  // buffer coordinates, then nudges scroll-x-px / scroll-y-px so the
  // cursor sits inside the viewport. If the cursor is already
  // visible the scrolls are left alone; if it's off the left/right
  // edge, scrolls horizontally; if off the top/bottom, scrolls
  // vertically.
  //
  // Reads cached-flat, cursor-offset, scroll-{x,y}-px, viewport-
  // {width,height}-px, client-{width,height}-px, char-width, line-
  // height; mutates scroll-{x,y}-px; calls %set-scroll-info if a
  // scroll changed. Closes over main()'s lexical scope so callers
  // pass only the HWND.
  //
  // Line-number computation walks cached-flat once per call. O(n)
  // per cursor move where n is the byte distance from start of
  // buffer to the cursor. Sub-millisecond for typical files; rope-
  // aware line lookup is a follow-up if it ever matters.
  // Sprint 43e-7 — record the cursor's current column as the new
  // ideal-col. Called by horizontal moves (left/right/home/end/
  // click/typing/backspace); vertical moves (up/down/pgup/pgdn)
  // skip this so the ideal column survives the vertical walk.
  let update-ideal-col = method ()
    let ls = scan-line-start(cached-flat, cursor-offset);
    ideal-col := cursor-offset - ls;
    0
  end;
  let ensure-cursor-visible = method (hwnd)
    // Sprint 43e-6 — reset the blink phase. Any caller that moved
    // the cursor wants it visibly solid for the next ~500 ms;
    // otherwise the bar flickers mid-keystroke.
    cursor-on := 1;
    let bytes = cached-flat;
    let cur = cursor-offset;
    let line-start = scan-line-start(bytes, cur);
    let col = cur - line-start;
    // Count newlines in bytes[0 .. line-start) → line index.
    let line = 0;
    let i = 0;
    until (i = line-start)
      if (element(bytes, i) = 10) line := line + 1; else 0 end;
      i := i + 1;
    end;
    let cx = col * char-width;
    let cy = line * line-height;
    // Desired scroll positions: closest to current that keeps the
    // cursor's char-width × line-height rect inside the viewport.
    let new-sx = scroll-x-px;
    if (cx < new-sx)
      new-sx := cx;
    elseif (cx + char-width > new-sx + viewport-width-px)
      new-sx := cx + char-width - viewport-width-px;
    else 0 end;
    let new-sy = scroll-y-px;
    if (cy < new-sy)
      new-sy := cy;
    elseif (cy + line-height > new-sy + viewport-height-px)
      new-sy := cy + line-height - viewport-height-px;
    else 0 end;
    // Clamp to [0, max] for each axis. Negative scroll would draw
    // the buffer past the pad; over-max would draw past the buffer.
    let h-max = if (client-width-px > viewport-width-px)
                  client-width-px - viewport-width-px
                else 0 end;
    let v-max = if (client-height-px > viewport-height-px)
                  client-height-px - viewport-height-px
                else 0 end;
    if (new-sx < 0) new-sx := 0 else 0 end;
    if (new-sx > h-max) new-sx := h-max else 0 end;
    if (new-sy < 0) new-sy := 0 else 0 end;
    if (new-sy > v-max) new-sy := v-max else 0 end;
    if (new-sx ~= scroll-x-px)
      scroll-x-px := new-sx;
      %set-scroll-info(hwnd, 0, 0, client-width-px, viewport-width-px, new-sx, 1);
    else 0 end;
    if (new-sy ~= scroll-y-px)
      scroll-y-px := new-sy;
      %set-scroll-info(hwnd, 1, 0, client-height-px, viewport-height-px, new-sy, 1);
    else 0 end;
    0
  end;
  // Sprint 11d — split the WNDPROC into two parts:
  //
  //   `handle-wm-message`: a regular Dylan function. Allowed to
  //     allocate freely; runs from a precisely-tracked Dylan frame so
  //     Sprint 11b's safepoint-root machinery sees every live Word
  //     across each allocating call.
  //
  //   `wp`: the OS-facing callback shell. Does no allocation. The only
  //     thing it does is forward to `handle-wm-message`. The Rust
  //     trampoline in nod-runtime/src/callbacks.rs::wndproc_dispatch
  //     calls into `wp` via `nod_funcall4`; `wp` makes exactly one
  //     Dylan-to-Dylan call into `handle-wm-message`, which is wrapped
  //     by 11b's begin_safepoint / end_safepoint pair.
  //
  // The rule: never let the registered callback closure itself
  // allocate. Win32 callbacks aren't part of the normal Dylan call
  // flow — they're re-entered out of a native frame the GC can't
  // describe — so we keep the work in a separate frame that *is*
  // part of the normal flow. Long-term, a `define-window-class` macro
  // generates this shell shape; for now we hand-wire it.
  let handle-wm-message = method (hwnd, msg, wparam, lparam)
             if (msg = 15)  // WM_PAINT
               if (swap ~= 0 & in-modal-dialog = 0)
                 if (bitmap = 0)
                   bitmap := %d2d-create-bitmap-from-swap-chain(dc, swap);
                 else 0 end;
                 %d2d-set-target(dc, bitmap);
                 %d2d-begin-draw(dc);
                 %d2d-clear(dc, 255, 255, 255, 255);
                 let brush  = %d2d-create-solid-color-brush(dc, 0, 0, 0, 255);
                 // Sprint 43g — gutter brushes.
                 //   gutter-bg-brush:   light grey background fill.
                 //   gutter-text-brush: medium grey for line numbers.
                 //   gutter-edge-brush: darker grey for the 1px
                 //                      right-edge separator line.
                 let gutter-bg-brush   = %d2d-create-solid-color-brush(dc, 240, 240, 240, 255);
                 let gutter-text-brush = %d2d-create-solid-color-brush(dc, 130, 130, 130, 255);
                 let gutter-edge-brush = %d2d-create-solid-color-brush(dc, 200, 200, 200, 255);
                 // Sprint 43f-1 / 43f-2 — syntax-colour brushes.
                 //   keyword: medium blue (define, end, method, …)
                 //   comment: muted green (// and /* */)
                 //   string:  brick red ("...")
                 //   number:  purple (literals)
                 //   class:   teal (<foo>, <byte-string>, …)
                 // Picked from common editor palettes; colours are
                 // distinguishable on both light and dark text without
                 // overpowering the default black.
                 let keyword-brush = %d2d-create-solid-color-brush(dc, 30, 90, 200, 255);
                 let comment-brush = %d2d-create-solid-color-brush(dc, 30, 130, 50, 255);
                 let string-brush  = %d2d-create-solid-color-brush(dc, 170, 50, 30, 255);
                 let number-brush  = %d2d-create-solid-color-brush(dc, 130, 50, 170, 255);
                 let class-brush   = %d2d-create-solid-color-brush(dc, 20, 130, 140, 255);
                 // Sprint 43d hotfix — `cached-flat` is refreshed at every
                 // mutation; WM_PAINT just reuses it. Before caching
                 // we were paying an O(n) byte-string allocation per
                 // paint, which under Win32's WM_PAINT cadence (drags,
                 // focus changes, scrolls) outran the GC's destination
                 // generation and tripped GcStallError::mid_evac_oom.
                 let layout = %dwrite-create-text-layout(dwrite, cached-flat, format,
                                                         client-width-px, client-height-px);
                 // Sprint 43f-5 — colour the WHOLE buffer, let DirectWrite
                 // clip rendering to the visible region.
                 //
                 // 43f-3 (fixed overscan) and 43f-4 (scan back to the
                 // previous `define ...`) both tried to bound the
                 // tokeniser to the visible region for performance.
                 // Both produced visible artefacts at scroll boundaries:
                 // 43f-3 mis-coloured when block comments spanned more
                 // than `overscan` lines above the viewport; 43f-4
                 // mis-coloured when editing non-Dylan files (Rust,
                 // markdown, anything without a column-0 `define `).
                 //
                 // The robust shape, per the user's instinct: think of
                 // the window as a VIEW into a backing buffer that's
                 // wholly tokenised. Per-paint cost on IDE-sized files
                 // (~30 KB) is sub-millisecond; on much larger files
                 // we can revisit with a per-line tokeniser-state cache
                 // (compute on edit, lookup on paint).
                 //
                 // SetDrawingEffect calls for off-screen ranges store
                 // metadata in the layout without painting; DirectWrite
                 // clips the actual glyph rendering to the layout box
                 // intersected with the render target — so the GPU
                 // work is still bounded by the viewport.
                 let scan-start = 0;
                 let scan-end = size(cached-flat);
                 highlight-dylan-syntax(layout, cached-flat,
                                        scan-start, scan-end,
                                        keyword-brush, comment-brush,
                                        string-brush, number-brush,
                                        class-brush);
                 // Sprint 43g — text-layout origin shifts right by
                 // `gutter-px` so the gutter columns get the left
                 // strip of the viewport.
                 %d2d-draw-text-layout(dc, gutter-px + pad - scroll-x-px, pad - scroll-y-px, layout, brush);
                 // Sprint 43e-1 (revised) — visible cursor via
                 // DirectWrite hit-testing. Ask the text layout we
                 // just drew where the cursor offset lives in pixels;
                 // this is exact, no matter the font / size / kerning.
                 //
                 // %dwrite-hit-test-position returns a packed u64 with
                 // y-pixels in the high 32 bits and x-pixels in the
                 // low 32 bits, relative to the layout origin (which
                 // we passed as `pad - scroll-x-px, pad - scroll-y-px`
                 // above). Trailing-edge flag = 0 → leading edge of
                 // the character AT cursor-offset, i.e. cursor BEFORE
                 // that character. cursor-offset clamped to flat-len
                 // so an EOF cursor still hit-tests cleanly.
                 let cur-off = cursor-offset;
                 let flat-len = size(cached-flat);
                 let hit-pos = if (cur-off < flat-len) cur-off else flat-len end;
                 let packed = %dwrite-hit-test-position(layout, hit-pos, 0);
                 // Bit 31 might be set in the low 32 bits for large x;
                 // we'll mask defensively. Use mod by 2^32 for the low
                 // half and integer div for the high half.
                 let two-to-32 = 4294967296;
                 let hx = packed - (packed / two-to-32) * two-to-32;
                 let hy = packed / two-to-32;
                 let cx = gutter-px + pad - scroll-x-px + hx;
                 let cy = pad - scroll-y-px + hy;
                 // Sprint 43e-6 — blink: only draw when cursor-on = 1.
                 // Bar is 3px wide (was 2px) for visibility — at 1Hz
                 // blink the eye latches the off-state better with a
                 // slightly thicker beam.
                 if (cursor-on = 1)
                   %d2d-fill-rectangle(dc, cx, cy, cx + 3, cy + line-height, brush);
                 else 0 end;
                 // Sprint 43g — gutter rendering. Drawn AFTER the
                 // text so the gutter sits on top, hiding any text
                 // that might've been horizontally-scrolled into
                 // negative x territory (text origin = gutter-px +
                 // pad - scroll-x-px; for scroll-x-px > pad the
                 // text could otherwise bleed under the gutter).
                 //
                 //   1. Fill the gutter background (light grey).
                 //   2. Draw a 1px darker separator at the right edge.
                 //   3. Build a multi-line line-numbers string for
                 //      the visible range + small overscan.
                 //   4. Create a temporary text layout sized to the
                 //      line-num gutter column; draw it at the
                 //      line-num gutter origin.
                 %d2d-fill-rectangle(dc, 0, 0, gutter-px, viewport-height-px, gutter-bg-brush);
                 %d2d-fill-rectangle(dc, gutter-px - 1, 0, gutter-px, viewport-height-px, gutter-edge-brush);
                 let first-visible-line = scroll-y-px / line-height;
                 let lines-on-screen = viewport-height-px / line-height + 1;
                 let total-lines = buffer-lines;
                 let last-line-uncapped = first-visible-line + lines-on-screen;
                 let last-line = if (last-line-uncapped < total-lines)
                                   last-line-uncapped
                                 else total-lines end;
                 let ln-block = build-line-numbers-block(first-visible-line + 1, last-line, 5);
                 let ln-layout = %dwrite-create-text-layout(dwrite, ln-block, format,
                                                            line-num-gutter-px, viewport-height-px);
                 let ln-origin-x = fold-gutter-px + error-gutter-px;
                 let ln-origin-y = pad + first-visible-line * line-height - scroll-y-px;
                 %d2d-draw-text-layout(dc, ln-origin-x, ln-origin-y, ln-layout, gutter-text-brush);
                 %com-release(ln-layout);
                 %d2d-end-draw(dc);
                 %com-release(brush);
                 %com-release(keyword-brush);
                 %com-release(comment-brush);
                 %com-release(string-brush);
                 %com-release(number-brush);
                 %com-release(class-brush);
                 %com-release(gutter-bg-brush);
                 %com-release(gutter-text-brush);
                 %com-release(gutter-edge-brush);
                 %com-release(layout);
                 %dxgi-swap-chain-present(swap);
               else 0 end;
               0
             elseif (msg = 275 & in-modal-dialog = 0)  // WM_TIMER — Sprint 43e-6 cursor blink
               // Toggle the blink state. Phrased as explicit if /
               // else := 0 / := 1 (rather than `cursor-on := if (...)
               // 0 else 1 end`) because the latter occasionally
               // didn't visibly blink during testing — possibly a
               // Dylan-side eval quirk, possibly just hard to see at
               // 500ms with a 2px bar. This form makes the toggle
               // unambiguous from the compiler's POV.
               if (cursor-on = 1)
                 cursor-on := 0;
               else
                 cursor-on := 1;
               end;
               InvalidateRect(hwnd, 0, 0);
               0
             elseif (msg = 5)  // WM_SIZE
               if (swap ~= 0 & wparam ~= 1)
                 let new-w = %lo-word(lparam);
                 let new-h = %hi-word(lparam);
                 if (new-w > 0 & new-h > 0)
                   if (bitmap ~= 0)
                     %d2d-set-target(dc, 0);
                     %com-release(bitmap);
                     bitmap := 0;
                   else 0 end;
                   window-width  := new-w;
                   window-height := new-h;
                   viewport-width-px  := new-w;
                   viewport-height-px := new-h;
                   %dxgi-swap-chain-resize-buffers(swap, new-w, new-h);
                   %set-scroll-info(hwnd, 1, 0, client-height-px, viewport-height-px, scroll-y-px, 1);
                   %set-scroll-info(hwnd, 0, 0, client-width-px,  viewport-width-px,  scroll-x-px, 1);
                 else 0 end;
               else 0 end;
               0
             elseif (msg = 277)  // WM_VSCROLL
               let action = %lo-word(wparam);
               let new-pos = if (action = 0)        // SB_LINEUP
                               scroll-y-px - line-height
                             elseif (action = 1)    // SB_LINEDOWN
                               scroll-y-px + line-height
                             elseif (action = 2)    // SB_PAGEUP
                               scroll-y-px - (viewport-height-px - line-height)
                             elseif (action = 3)    // SB_PAGEDOWN
                               scroll-y-px + (viewport-height-px - line-height)
                             elseif (action = 4)    // SB_THUMBPOSITION
                               %hi-word(wparam)
                             elseif (action = 5)    // SB_THUMBTRACK
                               %hi-word(wparam)
                             elseif (action = 6)    // SB_TOP (Home)
                               0
                             elseif (action = 7)    // SB_BOTTOM (End)
                               client-height-px - viewport-height-px
                             else
                               scroll-y-px           // SB_ENDSCROLL / unknown
                             end;
               let max-scroll = if (client-height-px > viewport-height-px)
                                  client-height-px - viewport-height-px
                                else 0 end;
               let clamped = if (new-pos < 0) 0
                             elseif (new-pos > max-scroll) max-scroll
                             else new-pos end;
               if (clamped ~= scroll-y-px)
                 scroll-y-px := clamped;
                 %set-scroll-info(hwnd, 1, 0, client-height-px, viewport-height-px, clamped, 1);
                 InvalidateRect(hwnd, 0, 0);
               else 0 end;
               0
             elseif (msg = 276)  // WM_HSCROLL
               let action = %lo-word(wparam);
               let new-pos = if (action = 0)        // SB_LINELEFT
                               scroll-x-px - char-width
                             elseif (action = 1)    // SB_LINERIGHT
                               scroll-x-px + char-width
                             elseif (action = 2)    // SB_PAGELEFT
                               scroll-x-px - (viewport-width-px - char-width)
                             elseif (action = 3)    // SB_PAGERIGHT
                               scroll-x-px + (viewport-width-px - char-width)
                             elseif (action = 4)    // SB_THUMBPOSITION
                               %hi-word(wparam)
                             elseif (action = 5)    // SB_THUMBTRACK
                               %hi-word(wparam)
                             elseif (action = 6)    // SB_LEFT
                               0
                             elseif (action = 7)    // SB_RIGHT
                               client-width-px - viewport-width-px
                             else
                               scroll-x-px
                             end;
               let max-scroll = if (client-width-px > viewport-width-px)
                                  client-width-px - viewport-width-px
                                else 0 end;
               let clamped = if (new-pos < 0) 0
                             elseif (new-pos > max-scroll) max-scroll
                             else new-pos end;
               if (clamped ~= scroll-x-px)
                 scroll-x-px := clamped;
                 %set-scroll-info(hwnd, 0, 0, client-width-px, viewport-width-px, clamped, 1);
                 InvalidateRect(hwnd, 0, 0);
               else 0 end;
               0
             elseif (msg = 522)  // WM_MOUSEWHEEL
               let raw-delta = %hi-word(wparam);
               let signed-delta = if (raw-delta > 32767)
                                    raw-delta - 65536
                                  else
                                    raw-delta
                                  end;
               let flags = %lo-word(wparam);
               let shift-bit = (flags / 4) - (flags / 8) * 2;
               if (shift-bit = 1)
                 let chars-to-scroll = -1 * signed-delta * 3 / 120;
                 let new-pos = scroll-x-px + chars-to-scroll * char-width;
                 let max-scroll = if (client-width-px > viewport-width-px)
                                    client-width-px - viewport-width-px
                                  else 0 end;
                 let clamped = if (new-pos < 0) 0
                               elseif (new-pos > max-scroll) max-scroll
                               else new-pos end;
                 if (clamped ~= scroll-x-px)
                   scroll-x-px := clamped;
                   %set-scroll-info(hwnd, 0, 0, client-width-px, viewport-width-px, clamped, 1);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               else
                 let lines-to-scroll = -1 * signed-delta * 3 / 120;
                 let new-pos = scroll-y-px + lines-to-scroll * line-height;
                 let max-scroll = if (client-height-px > viewport-height-px)
                                    client-height-px - viewport-height-px
                                  else 0 end;
                 let clamped = if (new-pos < 0) 0
                               elseif (new-pos > max-scroll) max-scroll
                               else new-pos end;
                 if (clamped ~= scroll-y-px)
                   scroll-y-px := clamped;
                   %set-scroll-info(hwnd, 1, 0, client-height-px, viewport-height-px, clamped, 1);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               end;
               0
             elseif (msg = 257)  // WM_KEYUP — Sprint 43e-8 track Ctrl release
               let vk = %lo-word(wparam);
               if (vk = 17)        // VK_CONTROL
                 ctrl-down := 0;
               else 0 end;
               0
             elseif (msg = 256)  // WM_KEYDOWN
               let vk = %lo-word(wparam);
               if (vk = 17)        // Sprint 43e-8 — track Ctrl press
                 ctrl-down := 1;
               else 0 end;
               let v-max = if (client-height-px > viewport-height-px)
                             client-height-px - viewport-height-px
                           else 0 end;
               let h-max = if (client-width-px > viewport-width-px)
                             client-width-px - viewport-width-px
                           else 0 end;
               if (vk = 33)        // VK_PRIOR (PgUp) — Sprint 43e-4b cursor move
                 // Move the cursor up by one screenful of lines. The
                 // ensure-cursor-visible call then pulls the viewport
                 // along so the cursor stays on screen. Walk one line
                 // at a time via move-cursor-vertical — simple but
                 // O(page) cached-flat walks per press; optimisation
                 // (single-pass walk preserving the ideal column) is a
                 // follow-up if PgUp/PgDn ever feels sluggish.
                 let lines-per-page = viewport-height-px / line-height;
                 let new-off = cursor-offset;
                 let i = 0;
                 until (i = lines-per-page)
                   new-off := move-cursor-vertical(cached-flat, new-off, -1, ideal-col);
                   i := i + 1;
                 end;
                 if (new-off ~= cursor-offset)
                   cursor-offset := new-off;
                   ensure-cursor-visible(hwnd);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               elseif (vk = 34)    // VK_NEXT (PgDn) — Sprint 43e-4b cursor move
                 let lines-per-page = viewport-height-px / line-height;
                 let new-off = cursor-offset;
                 let i = 0;
                 until (i = lines-per-page)
                   new-off := move-cursor-vertical(cached-flat, new-off, 1, ideal-col);
                   i := i + 1;
                 end;
                 if (new-off ~= cursor-offset)
                   cursor-offset := new-off;
                   ensure-cursor-visible(hwnd);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               elseif (vk = 36)    // VK_HOME — Sprint 43e-3 / 43e-8
                 // Plain HOME → start of current line.
                 // Ctrl+HOME → start of buffer (offset 0).
                 // Modifier state from the `ctrl-down` cell that
                 // VK_CONTROL up/down events maintain.
                 let new-off = if (ctrl-down = 1) 0
                               else scan-line-start(cached-flat, cursor-offset) end;
                 if (new-off ~= cursor-offset)
                   cursor-offset := new-off;
                   update-ideal-col();
                   ensure-cursor-visible(hwnd);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               elseif (vk = 35)    // VK_END — Sprint 43e-3 / 43e-8
                 // Plain END → end of current line.
                 // Ctrl+END → end of buffer (size(cached-flat)).
                 let new-off = if (ctrl-down = 1) size(cached-flat)
                               else scan-line-end(cached-flat, cursor-offset, size(cached-flat)) end;
                 if (new-off ~= cursor-offset)
                   cursor-offset := new-off;
                   update-ideal-col();
                   ensure-cursor-visible(hwnd);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               elseif (vk = 37)    // VK_LEFT — Sprint 43e-2 cursor move
                 // Rebound from horizontal scroll to cursor move per the
                 // universal text-editor convention. Horizontal scroll
                 // stays available via Shift+MouseWheel, the horizontal
                 // scrollbar, and (if we add them) Ctrl+arrows.
                 if (cursor-offset > 0)
                   cursor-offset := cursor-offset - 1;
                   update-ideal-col();
                   ensure-cursor-visible(hwnd);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               elseif (vk = 39)    // VK_RIGHT — Sprint 43e-2 cursor move
                 let buf-len = size(cached-flat);
                 if (cursor-offset < buf-len)
                   cursor-offset := cursor-offset + 1;
                   update-ideal-col();
                   ensure-cursor-visible(hwnd);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               elseif (vk = 38)    // VK_UP — Sprint 43e-2 cursor move
                 // move-cursor-vertical returns the input unchanged at
                 // the top of the buffer, so the `~=` guard skips the
                 // pointless repaint.
                 let new-off = move-cursor-vertical(cached-flat, cursor-offset, -1, ideal-col);
                 if (new-off ~= cursor-offset)
                   cursor-offset := new-off;
                   ensure-cursor-visible(hwnd);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               elseif (vk = 40)    // VK_DOWN — Sprint 43e-2 cursor move
                 let new-off = move-cursor-vertical(cached-flat, cursor-offset, 1, ideal-col);
                 if (new-off ~= cursor-offset)
                   cursor-offset := new-off;
                   ensure-cursor-visible(hwnd);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               elseif (vk = 8)     // VK_BACK — Sprint 43d backspace
                 // Delete the byte at cursor-offset - 1. The rope is
                 // persistent — `source-text := rope-delete(...)` makes
                 // a new sibling tree sharing almost every leaf with
                 // the old one. Update cursor, recompute metrics,
                 // refresh cached-flat (so WM_PAINT doesn't pay for
                 // serialisation), re-issue scroll info, repaint.
                 if (cursor-offset > 0)
                   source-text := rope-delete(source-text, cursor-offset - 1, cursor-offset);
                   cursor-offset := cursor-offset - 1;
                   cached-flat := rope->string(source-text);
                   buffer-lines := nod-rope-line-count(source-text);
                   buffer-max-cols := nod-rope-max-line-chars(source-text);
                   client-width-px  := buffer-max-cols * char-width;
                   client-height-px := buffer-lines * line-height;
                   %set-scroll-info(hwnd, 1, 0, client-height-px, viewport-height-px, scroll-y-px, 1);
                   %set-scroll-info(hwnd, 0, 0, client-width-px,  viewport-width-px,  scroll-x-px, 1);
                   update-ideal-col();
                   // Sprint 43e-4 — after backspace the cursor may have
                   // walked off the left edge of the viewport on a wrap;
                   // pull the viewport back to keep it visible.
                   ensure-cursor-visible(hwnd);
                   InvalidateRect(hwnd, 0, 0);
                 else 0 end;
               else 0 end;
               0
             elseif (msg = 513)  // WM_LBUTTONDOWN — Sprint 43e-5 / 43g cursor positioning
               // lParam packs the click position as (y << 16) | x in
               // client-area coordinates (top-left of the window's
               // client area = (0, 0)). Subtract gutter-px + pad to
               // convert to layout-relative coordinates; add scroll
               // offsets. Clicks inside the gutter (cx-client <
               // gutter-px) don't move the cursor — later sprints can
               // bind those to fold-toggle or error-tooltip.
               //
               // We create a fresh text layout per click — cheaper
               // than caching it across mutations and clicks are
               // rare compared to keystrokes. The layout is released
               // immediately after the hit-test.
               let cx-client = %lo-word(lparam);
               let cy-client = %hi-word(lparam);
               if (cx-client < gutter-px)
                 0   // ignore gutter clicks for now
               else
               let layout-x = cx-client + scroll-x-px - pad - gutter-px;
               let layout-y = cy-client + scroll-y-px - pad;
               let layout = %dwrite-create-text-layout(dwrite, cached-flat, format,
                                                       client-width-px, client-height-px);
               let new-off = %dwrite-hit-test-point(layout, layout-x, layout-y);
               %com-release(layout);
               // Clamp to buffer bounds. HitTestPoint returns the
               // closest valid offset even for out-of-bounds clicks
               // but we belt-and-brace it.
               let buf-len = size(cached-flat);
               let clamped = if (new-off < 0) 0
                             elseif (new-off > buf-len) buf-len
                             else new-off end;
               if (clamped ~= cursor-offset)
                 cursor-offset := clamped;
                 update-ideal-col();
                 ensure-cursor-visible(hwnd);
                 InvalidateRect(hwnd, 0, 0);
               else 0 end;
               end;   // close the gutter-click if/else
               0
             elseif (msg = 258)  // WM_CHAR — Sprint 43d character input
               // wparam carries the character as a UTF-16 code unit.
               // Phase-2 simplicity: accept only ASCII-printable
               // (32..126), Tab (9), or Enter (13, translated to
               // '\n'=10 for our internal representation). Backspace
               // (8) is handled in WM_KEYDOWN; everything else is
               // dropped silently. Full Unicode/IME input is a later
               // sprint.
               let ch = wparam;
               let insert? = (ch >= 32 & ch <= 126) | (ch = 9) | (ch = 13);
               if (insert?)
                 let byte-code = if (ch = 13) 10 else ch end;
                 let one-byte = %byte-string-allocate(1);
                 %byte-string-element-setter(byte-code, one-byte, 0);
                 source-text := rope-insert(source-text, cursor-offset, one-byte);
                 cursor-offset := cursor-offset + 1;
                 cached-flat := rope->string(source-text);
                 buffer-lines := nod-rope-line-count(source-text);
                 buffer-max-cols := nod-rope-max-line-chars(source-text);
                 client-width-px  := buffer-max-cols * char-width;
                 client-height-px := buffer-lines * line-height;
                 %set-scroll-info(hwnd, 1, 0, client-height-px, viewport-height-px, scroll-y-px, 1);
                 %set-scroll-info(hwnd, 0, 0, client-width-px,  viewport-width-px,  scroll-x-px, 1);
                 update-ideal-col();
                 // Sprint 43e-4 — keep the cursor on screen after the
                 // insertion advanced it past the right edge of the
                 // viewport. Common case: typing at the end of a
                 // long line.
                 ensure-cursor-visible(hwnd);
                 InvalidateRect(hwnd, 0, 0);
               else 0 end;
               0
             elseif (msg = 273)  // WM_COMMAND — Sprint 41e/g menu dispatch
               // Menu items pack the command id in the wparam LOWORD;
               // wparam HIWORD is 0 for menu (vs accelerator/control).
               let cmd-id = %lo-word(wparam);
               if (cmd-id = 100)        // File → Open...
                 in-modal-dialog := 1;
                 let new-path = %show-open-file-dialog(hwnd);
                 in-modal-dialog := 0;
                 if (~ empty?(new-path))
                   let new-source = %read-file(new-path);
                   if (~ empty?(new-source))
                     // Sprint 43d — wrap the freshly read bytes in a
                     // rope before storing. All subsequent reads /
                     // edits use rope ops. Reset cursor + cache.
                     let new-rope = make-rope-from-string(new-source);
                     source-text := new-rope;
                     cursor-offset := 0;
                     cursor-on := 1;
                     ideal-col := 0;
                     cached-flat := new-source;
                     current-path := new-path;
                     buffer-lines := nod-rope-line-count(new-rope);
                     buffer-max-cols := nod-rope-max-line-chars(new-rope);
                     client-width-px  := buffer-max-cols * char-width;
                     client-height-px := buffer-lines * line-height;
                     scroll-x-px := 0;
                     scroll-y-px := 0;
                     %set-scroll-info(hwnd, 1, 0, client-height-px, viewport-height-px, 0, 1);
                     %set-scroll-info(hwnd, 0, 0, client-width-px,  viewport-width-px,  0, 1);
                     recent-paths := nod-add-recent(new-path, recent-paths);
                     rebuild-recent-submenu(recent-menu, recent-paths);
                     DrawMenuBar(hwnd);
                     update-title(hwnd, new-path);
                     InvalidateRect(hwnd, 0, 0);
                   else 0 end;
                 else 0 end;
                 0
               elseif (cmd-id = 101)    // File → Save
                 // If no current-path yet, fall through to Save As: pop
                 // the save dialog so the user can name the file. If
                 // we have a path, just rewrite that file with the
                 // in-memory contents (currently identical to what's
                 // on disk — Sprint 41h+ adds dirty-flag tracking).
                 if (empty?(current-path))
                   in-modal-dialog := 1;
                   let chosen = %show-save-file-dialog(hwnd);
                   in-modal-dialog := 0;
                   if (~ empty?(chosen))
                     // Sprint 43d — serialise rope to flat bytes for
                     // %write-file. Sprint 43e+ can switch to leaf-
                     // by-leaf streaming once we have %write-file-append.
                     let ok = %write-file(chosen, cached-flat);
                     if (ok = 1)
                       current-path := chosen;
                       recent-paths := nod-add-recent(chosen, recent-paths);
                       rebuild-recent-submenu(recent-menu, recent-paths);
                       DrawMenuBar(hwnd);
                       update-title(hwnd, chosen);
                     else 0 end;
                   else 0 end;
                 else
                   %write-file(current-path, cached-flat);
                   0
                 end;
                 0
               elseif (cmd-id = 102)    // File → Save As...
                 in-modal-dialog := 1;
                 let chosen = %show-save-file-dialog(hwnd);
                 in-modal-dialog := 0;
                 if (~ empty?(chosen))
                   let ok = %write-file(chosen, cached-flat);
                   if (ok = 1)
                     current-path := chosen;
                     recent-paths := nod-add-recent(chosen, recent-paths);
                     rebuild-recent-submenu(recent-menu, recent-paths);
                     DrawMenuBar(hwnd);
                     update-title(hwnd, chosen);
                   else 0 end;
                 else 0 end;
                 0
               elseif (cmd-id = 199)    // File → Exit
                 PostQuitMessage(0);
                 0
               elseif (cmd-id = 200)    // Help → About
                 // Sprint 41f workaround — see SetWindowTextW
                 // declaration comment above.
                 SetWindowTextW(hwnd,
                                "NewOpenDylan IDE - Sprint 41g (About)");
                 0
               elseif (cmd-id > 300 & cmd-id < 306)  // Recent items 301..305
                 // Convert 1-based menu position to 0-based list index.
                 let idx = cmd-id - 301;
                 let cursor = recent-paths;
                 let i = 0;
                 // Walk to the requested index. If the list is shorter
                 // than expected (stale menu vs. live list — shouldn't
                 // happen but defensive), `cursor` lands on nil and we
                 // bail out.
                 until (i = idx | empty?(cursor))
                   cursor := tail(cursor);
                   i := i + 1;
                 end;
                 if (~ empty?(cursor))
                   let path = head(cursor);
                   let bytes = %read-file(path);
                   if (~ empty?(bytes))
                     // Sprint 43d — wrap in rope, same as Open does;
                     // also reset cursor + cache for the new buffer.
                     let rope = make-rope-from-string(bytes);
                     source-text := rope;
                     cursor-offset := 0;
                     cursor-on := 1;
                     ideal-col := 0;
                     cached-flat := bytes;
                     current-path := path;
                     buffer-lines := nod-rope-line-count(rope);
                     buffer-max-cols := nod-rope-max-line-chars(rope);
                     client-width-px  := buffer-max-cols * char-width;
                     client-height-px := buffer-lines * line-height;
                     scroll-x-px := 0;
                     scroll-y-px := 0;
                     %set-scroll-info(hwnd, 1, 0, client-height-px, viewport-height-px, 0, 1);
                     %set-scroll-info(hwnd, 0, 0, client-width-px,  viewport-width-px,  0, 1);
                     recent-paths := nod-add-recent(path, recent-paths);
                     rebuild-recent-submenu(recent-menu, recent-paths);
                     DrawMenuBar(hwnd);
                     update-title(hwnd, path);
                     InvalidateRect(hwnd, 0, 0);
                   else 0 end;
                 else 0 end;
                 0
               else
                 // Unknown command id — defer to the OS default.
                 DefWindowProcW(hwnd, msg, wparam, lparam)
               end
             elseif (msg = 2)  // WM_DESTROY
               PostQuitMessage(0);
               0
             else
               DefWindowProcW(hwnd, msg, wparam, lparam)
             end
           end;
  // Sprint 11d — the OS-facing shell. One Dylan call, no allocations.
  // See the `handle-wm-message` definition above for the contract.
  let wp = method (hwnd, msg, wparam, lparam)
             handle-wm-message(hwnd, msg, wparam, lparam)
           end;
  let cb = as-wndproc-callback(wp);
  let atom = %register-window-class(cb, "NodIDE");
  // dwStyle = WS_OVERLAPPEDWINDOW (0xCF0000)
  //         | WS_VSCROLL          (0x00200000)
  //         | WS_HSCROLL          (0x00100000)
  //         = 16711680.
  // hMenu = `menu-bar` HMENU (10th arg).
  let hwnd = CreateWindowExW(0, atom, "NewOpenDylan IDE",
                             16711680, -2147483648, -2147483648, 1024, 768,
                             0, menu-bar, 0, 0);
  swap := %dxgi-create-swap-chain-for-hwnd(dxgi-factory, d3d-device, hwnd, 1024, 768);
  %set-scroll-info(hwnd, 1, 0, client-height-px, viewport-height-px, 0, 1);
  %set-scroll-info(hwnd, 0, 0, client-width-px,  viewport-width-px,  0, 1);
  update-title(hwnd, current-path);
  ShowWindow(hwnd, 5);
  UpdateWindow(hwnd);
  // Sprint 43e-6 — start a 500 ms blink timer. WM_TIMER (msg 275)
  // fires on this thread's message pump every ~500 ms; the
  // handler toggles `cursor-on` and invalidates the window so the
  // cursor bar appears / disappears between paints.
  // Args: (hwnd, idEvent=1, uElapse=500ms, lpTimerFunc=NULL).
  SetTimer(hwnd, 1, 500, 0);
  %run-message-loop();
end function main;
