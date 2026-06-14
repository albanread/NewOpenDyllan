Module: dylan
Precedence: c
Author: NewOpenDylan Sprint 29 — generated bindings, do not edit by hand.

// Sprint 29 — Win32 integer constants (301 total).
//
// Regenerate via:
//     cargo run --quiet -p nod-winapi --bin generate_constants
//
// Source of truth: data/win32_constants.txt. The vendored
// windows_api.db (schema v5) carries enum *type* declarations
// but NOT the integer values of their members, so Sprint 29
// curates the most-used Win32 constants by hand. A future
// sprint that extends the upstream DB with an enum-members
// table can add a DB-extraction pass to build.rs alongside
// the curated set; this file's layout doesn't change.


// ─── Pointer / handle sentinels ────────────────────────────────────────

define constant $NULL = 0;

// ─── MessageBox flags (user32) — buttons ───────────────────────────────

define constant $MB-OK = 0;  // user32.dll
define constant $MB-OKCANCEL = 1;  // user32.dll
define constant $MB-ABORTRETRYIGNORE = 2;  // user32.dll
define constant $MB-YESNOCANCEL = 3;  // user32.dll
define constant $MB-YESNO = 4;  // user32.dll
define constant $MB-RETRYCANCEL = 5;  // user32.dll
define constant $MB-CANCELTRYCONTINUE = 6;  // user32.dll

// ─── MessageBox flags (user32) — icons ─────────────────────────────────

define constant $MB-ICONHAND = 16;  // user32.dll
define constant $MB-ICONERROR = 16;  // user32.dll
define constant $MB-ICONSTOP = 16;  // user32.dll
define constant $MB-ICONQUESTION = 32;  // user32.dll
define constant $MB-ICONEXCLAMATION = 48;  // user32.dll
define constant $MB-ICONWARNING = 48;  // user32.dll
define constant $MB-ICONASTERISK = 64;  // user32.dll
define constant $MB-ICONINFORMATION = 64;  // user32.dll

// ─── MessageBox flags (user32) — default button ────────────────────────

define constant $MB-DEFBUTTON1 = 0;  // user32.dll
define constant $MB-DEFBUTTON2 = #x100;  // user32.dll
define constant $MB-DEFBUTTON3 = #x200;  // user32.dll
define constant $MB-DEFBUTTON4 = #x300;  // user32.dll

// ─── MessageBox return values (IDOK/IDCANCEL/…) (user32) ───────────────

define constant $IDOK = 1;  // user32.dll
define constant $IDCANCEL = 2;  // user32.dll
define constant $IDABORT = 3;  // user32.dll
define constant $IDRETRY = 4;  // user32.dll
define constant $IDIGNORE = 5;  // user32.dll
define constant $IDYES = 6;  // user32.dll
define constant $IDNO = 7;  // user32.dll
define constant $IDCLOSE = 8;  // user32.dll
define constant $IDHELP = 9;  // user32.dll
define constant $IDTRYAGAIN = 10;  // user32.dll
define constant $IDCONTINUE = 11;  // user32.dll

// ─── Window messages (user32) — lifecycle ──────────────────────────────

define constant $WM-NULL = 0;  // user32.dll
define constant $WM-CREATE = 1;  // user32.dll
define constant $WM-DESTROY = 2;  // user32.dll
define constant $WM-MOVE = 3;  // user32.dll
define constant $WM-SIZE = 5;  // user32.dll
define constant $WM-ACTIVATE = 6;  // user32.dll
define constant $WM-SETFOCUS = 7;  // user32.dll
define constant $WM-KILLFOCUS = 8;  // user32.dll
define constant $WM-ENABLE = 10;  // user32.dll
define constant $WM-SETREDRAW = 11;  // user32.dll
define constant $WM-SETTEXT = 12;  // user32.dll
define constant $WM-GETTEXT = 13;  // user32.dll
define constant $WM-GETTEXTLENGTH = 14;  // user32.dll
define constant $WM-PAINT = 15;  // user32.dll
define constant $WM-CLOSE = 16;  // user32.dll
define constant $WM-QUERYENDSESSION = 17;  // user32.dll
define constant $WM-QUIT = 18;  // user32.dll
define constant $WM-QUERYOPEN = 19;  // user32.dll
define constant $WM-ERASEBKGND = 20;  // user32.dll
define constant $WM-SYSCOLORCHANGE = 21;  // user32.dll
define constant $WM-ENDSESSION = 22;  // user32.dll
define constant $WM-SHOWWINDOW = 24;  // user32.dll

// ─── Window messages (user32) — keyboard / mouse ───────────────────────

define constant $WM-KEYDOWN = #x100;  // user32.dll
define constant $WM-KEYUP = #x101;  // user32.dll
define constant $WM-CHAR = #x102;  // user32.dll
define constant $WM-DEADCHAR = #x103;  // user32.dll
define constant $WM-SYSKEYDOWN = #x104;  // user32.dll
define constant $WM-SYSKEYUP = #x105;  // user32.dll
define constant $WM-SYSCHAR = #x106;  // user32.dll
define constant $WM-SYSDEADCHAR = #x107;  // user32.dll
define constant $WM-MOUSEMOVE = #x200;  // user32.dll
define constant $WM-LBUTTONDOWN = #x201;  // user32.dll
define constant $WM-LBUTTONUP = #x202;  // user32.dll
define constant $WM-LBUTTONDBLCLK = #x203;  // user32.dll
define constant $WM-RBUTTONDOWN = #x204;  // user32.dll
define constant $WM-RBUTTONUP = #x205;  // user32.dll
define constant $WM-RBUTTONDBLCLK = #x206;  // user32.dll
define constant $WM-MBUTTONDOWN = #x207;  // user32.dll
define constant $WM-MBUTTONUP = #x208;  // user32.dll
define constant $WM-MBUTTONDBLCLK = #x209;  // user32.dll
define constant $WM-MOUSEWHEEL = #x20A;  // user32.dll
define constant $WM-MOUSEHWHEEL = #x20E;  // user32.dll

// ─── Window messages (user32) — commands & input ───────────────────────

define constant $WM-COMMAND = #x111;  // user32.dll
define constant $WM-SYSCOMMAND = #x112;  // user32.dll
define constant $WM-TIMER = #x113;  // user32.dll
define constant $WM-HSCROLL = #x114;  // user32.dll
define constant $WM-VSCROLL = #x115;  // user32.dll
define constant $WM-INITDIALOG = #x110;  // user32.dll
define constant $WM-NOTIFY = 78;  // user32.dll
define constant $WM-INPUT = 255;  // user32.dll

// ─── Window styles (user32) — top-level ────────────────────────────────

define constant $WS-OVERLAPPED = 0;  // user32.dll
define constant $WS-POPUP = #x80000000;  // user32.dll
define constant $WS-CHILD = #x40000000;  // user32.dll
define constant $WS-MINIMIZE = #x20000000;  // user32.dll
define constant $WS-VISIBLE = #x10000000;  // user32.dll
define constant $WS-DISABLED = #x8000000;  // user32.dll
define constant $WS-CLIPSIBLINGS = #x4000000;  // user32.dll
define constant $WS-CLIPCHILDREN = #x2000000;  // user32.dll
define constant $WS-MAXIMIZE = #x1000000;  // user32.dll
define constant $WS-CAPTION = #xC00000;  // user32.dll
define constant $WS-BORDER = #x800000;  // user32.dll
define constant $WS-DLGFRAME = #x400000;  // user32.dll
define constant $WS-VSCROLL = #x200000;  // user32.dll
define constant $WS-HSCROLL = #x100000;  // user32.dll
define constant $WS-SYSMENU = #x80000;  // user32.dll
define constant $WS-THICKFRAME = #x40000;  // user32.dll
define constant $WS-GROUP = #x20000;  // user32.dll
define constant $WS-TABSTOP = #x10000;  // user32.dll
define constant $WS-MINIMIZEBOX = #x20000;  // user32.dll
define constant $WS-MAXIMIZEBOX = #x10000;  // user32.dll
define constant $WS-OVERLAPPEDWINDOW = #xCF0000;  // user32.dll
define constant $WS-POPUPWINDOW = #x80880000;  // user32.dll

// ─── Window extended styles (user32) ───────────────────────────────────

define constant $WS-EX-DLGMODALFRAME = 1;  // user32.dll
define constant $WS-EX-NOPARENTNOTIFY = 4;  // user32.dll
define constant $WS-EX-TOPMOST = 8;  // user32.dll
define constant $WS-EX-ACCEPTFILES = 16;  // user32.dll
define constant $WS-EX-TRANSPARENT = 32;  // user32.dll
define constant $WS-EX-MDICHILD = 64;  // user32.dll
define constant $WS-EX-TOOLWINDOW = 128;  // user32.dll
define constant $WS-EX-WINDOWEDGE = #x100;  // user32.dll
define constant $WS-EX-CLIENTEDGE = #x200;  // user32.dll
define constant $WS-EX-CONTEXTHELP = #x400;  // user32.dll
define constant $WS-EX-RIGHT = #x1000;  // user32.dll
define constant $WS-EX-LEFT = 0;  // user32.dll
define constant $WS-EX-LAYERED = #x80000;  // user32.dll

// ─── ShowWindow commands (user32) ──────────────────────────────────────

define constant $SW-HIDE = 0;  // user32.dll
define constant $SW-SHOWNORMAL = 1;  // user32.dll
define constant $SW-NORMAL = 1;  // user32.dll
define constant $SW-SHOWMINIMIZED = 2;  // user32.dll
define constant $SW-SHOWMAXIMIZED = 3;  // user32.dll
define constant $SW-MAXIMIZE = 3;  // user32.dll
define constant $SW-SHOWNOACTIVATE = 4;  // user32.dll
define constant $SW-SHOW = 5;  // user32.dll
define constant $SW-MINIMIZE = 6;  // user32.dll
define constant $SW-SHOWMINNOACTIVE = 7;  // user32.dll
define constant $SW-SHOWNA = 8;  // user32.dll
define constant $SW-RESTORE = 9;  // user32.dll
define constant $SW-SHOWDEFAULT = 10;  // user32.dll
define constant $SW-FORCEMINIMIZE = 11;  // user32.dll

// ─── GetWindowLong / SetWindowLong offsets (user32) ────────────────────

define constant $GWL-WNDPROC = -4;  // user32.dll
define constant $GWL-HINSTANCE = -6;  // user32.dll
define constant $GWL-HWNDPARENT = -8;  // user32.dll
define constant $GWL-STYLE = -16;  // user32.dll
define constant $GWL-EXSTYLE = -20;  // user32.dll
define constant $GWL-USERDATA = -21;  // user32.dll
define constant $GWL-ID = -12;  // user32.dll
define constant $GWLP-WNDPROC = -4;  // user32.dll
define constant $GWLP-HINSTANCE = -6;  // user32.dll
define constant $GWLP-HWNDPARENT = -8;  // user32.dll
define constant $GWLP-USERDATA = -21;  // user32.dll
define constant $GWLP-ID = -12;  // user32.dll

// ─── CreateWindow defaults (user32) ────────────────────────────────────

define constant $CW-USEDEFAULT = #x80000000;  // user32.dll

// ─── Standard cursors (user32) ─────────────────────────────────────────

define constant $IDC-ARROW = #x7F00;  // user32.dll
define constant $IDC-IBEAM = #x7F01;  // user32.dll
define constant $IDC-WAIT = #x7F02;  // user32.dll
define constant $IDC-CROSS = #x7F03;  // user32.dll
define constant $IDC-UPARROW = #x7F04;  // user32.dll
define constant $IDC-SIZE = #x7F80;  // user32.dll
define constant $IDC-ICON = #x7F81;  // user32.dll
define constant $IDC-SIZENWSE = #x7F82;  // user32.dll
define constant $IDC-SIZENESW = #x7F83;  // user32.dll
define constant $IDC-SIZEWE = #x7F84;  // user32.dll
define constant $IDC-SIZENS = #x7F85;  // user32.dll
define constant $IDC-SIZEALL = #x7F86;  // user32.dll
define constant $IDC-NO = #x7F88;  // user32.dll
define constant $IDC-HAND = #x7F89;  // user32.dll
define constant $IDC-APPSTARTING = #x7F8A;  // user32.dll
define constant $IDC-HELP = #x7F8B;  // user32.dll

// ─── Standard icons (user32) ───────────────────────────────────────────

define constant $IDI-APPLICATION = #x7F00;  // user32.dll
define constant $IDI-HAND = #x7F01;  // user32.dll
define constant $IDI-QUESTION = #x7F02;  // user32.dll
define constant $IDI-EXCLAMATION = #x7F03;  // user32.dll
define constant $IDI-ASTERISK = #x7F04;  // user32.dll
define constant $IDI-WINLOGO = #x7F05;  // user32.dll
define constant $IDI-SHIELD = #x7F06;  // user32.dll

// ─── System metrics indices (user32) ───────────────────────────────────

define constant $SM-CXSCREEN = 0;  // user32.dll
define constant $SM-CYSCREEN = 1;  // user32.dll
define constant $SM-CXFULLSCREEN = 16;  // user32.dll
define constant $SM-CYFULLSCREEN = 17;  // user32.dll
define constant $SM-CXBORDER = 5;  // user32.dll
define constant $SM-CYBORDER = 6;  // user32.dll
define constant $SM-CXFRAME = 32;  // user32.dll
define constant $SM-CYFRAME = 33;  // user32.dll
define constant $SM-CXMINTRACK = 34;  // user32.dll
define constant $SM-CYMINTRACK = 35;  // user32.dll
define constant $SM-CXCURSOR = 13;  // user32.dll
define constant $SM-CYCURSOR = 14;  // user32.dll
define constant $SM-CXICON = 11;  // user32.dll
define constant $SM-CYICON = 12;  // user32.dll
define constant $SM-CMONITORS = 80;  // user32.dll
define constant $SM-REMOTESESSION = #x1000;  // user32.dll

// ─── GDI background modes (gdi32) ──────────────────────────────────────

define constant $TRANSPARENT = 1;  // gdi32.dll
define constant $OPAQUE = 2;  // gdi32.dll

// ─── GDI mapping modes (gdi32) ─────────────────────────────────────────

define constant $MM-TEXT = 1;  // gdi32.dll
define constant $MM-LOMETRIC = 2;  // gdi32.dll
define constant $MM-HIMETRIC = 3;  // gdi32.dll
define constant $MM-LOENGLISH = 4;  // gdi32.dll
define constant $MM-HIENGLISH = 5;  // gdi32.dll
define constant $MM-TWIPS = 6;  // gdi32.dll
define constant $MM-ISOTROPIC = 7;  // gdi32.dll
define constant $MM-ANISOTROPIC = 8;  // gdi32.dll

// ─── GDI ROP codes — BitBlt raster operations (gdi32) ──────────────────

define constant $SRCCOPY = #xCC0020;  // gdi32.dll
define constant $SRCPAINT = #xEE0086;  // gdi32.dll
define constant $SRCAND = #x8800C6;  // gdi32.dll
define constant $SRCINVERT = #x660046;  // gdi32.dll
define constant $SRCERASE = #x440328;  // gdi32.dll
define constant $NOTSRCCOPY = #x330008;  // gdi32.dll
define constant $NOTSRCERASE = #x1100A6;  // gdi32.dll
define constant $MERGECOPY = #xC000CA;  // gdi32.dll
define constant $MERGEPAINT = #xBB0226;  // gdi32.dll
define constant $PATCOPY = #xF00021;  // gdi32.dll
define constant $PATPAINT = #xFB0A09;  // gdi32.dll
define constant $PATINVERT = #x5A0049;  // gdi32.dll
define constant $DSTINVERT = #x550009;  // gdi32.dll
define constant $BLACKNESS = 66;  // gdi32.dll
define constant $WHITENESS = #xFF0062;  // gdi32.dll

// ─── Process / thread access rights (kernel32) ─────────────────────────

define constant $PROCESS-TERMINATE = 1;  // kernel32.dll
define constant $PROCESS-CREATE-THREAD = 2;  // kernel32.dll
define constant $PROCESS-VM-OPERATION = 8;  // kernel32.dll
define constant $PROCESS-VM-READ = 16;  // kernel32.dll
define constant $PROCESS-VM-WRITE = 32;  // kernel32.dll
define constant $PROCESS-DUP-HANDLE = 64;  // kernel32.dll
define constant $PROCESS-CREATE-PROCESS = 128;  // kernel32.dll
define constant $PROCESS-SET-QUOTA = #x100;  // kernel32.dll
define constant $PROCESS-SET-INFORMATION = #x200;  // kernel32.dll
define constant $PROCESS-QUERY-INFORMATION = #x400;  // kernel32.dll
define constant $PROCESS-ALL-ACCESS = #x1FFFFF;  // kernel32.dll

// ─── File / generic access rights (kernel32) ───────────────────────────

define constant $GENERIC-READ = #x80000000;  // kernel32.dll
define constant $GENERIC-WRITE = #x40000000;  // kernel32.dll
define constant $GENERIC-EXECUTE = #x20000000;  // kernel32.dll
define constant $GENERIC-ALL = #x10000000;  // kernel32.dll
define constant $FILE-SHARE-READ = 1;  // kernel32.dll
define constant $FILE-SHARE-WRITE = 2;  // kernel32.dll
define constant $FILE-SHARE-DELETE = 4;  // kernel32.dll
define constant $CREATE-NEW = 1;  // kernel32.dll
define constant $CREATE-ALWAYS = 2;  // kernel32.dll
define constant $OPEN-EXISTING = 3;  // kernel32.dll
define constant $OPEN-ALWAYS = 4;  // kernel32.dll
define constant $TRUNCATE-EXISTING = 5;  // kernel32.dll
define constant $FILE-ATTRIBUTE-READONLY = 1;  // kernel32.dll
define constant $FILE-ATTRIBUTE-HIDDEN = 2;  // kernel32.dll
define constant $FILE-ATTRIBUTE-SYSTEM = 4;  // kernel32.dll
define constant $FILE-ATTRIBUTE-DIRECTORY = 16;  // kernel32.dll
define constant $FILE-ATTRIBUTE-ARCHIVE = 32;  // kernel32.dll
define constant $FILE-ATTRIBUTE-NORMAL = 128;  // kernel32.dll

// ─── Memory allocation (VirtualAlloc) (kernel32) ───────────────────────

define constant $MEM-COMMIT = #x1000;  // kernel32.dll
define constant $MEM-RESERVE = #x2000;  // kernel32.dll
define constant $MEM-DECOMMIT = #x4000;  // kernel32.dll
define constant $MEM-RELEASE = #x8000;  // kernel32.dll
define constant $MEM-FREE = #x10000;  // kernel32.dll
define constant $MEM-PRIVATE = #x20000;  // kernel32.dll
define constant $MEM-MAPPED = #x40000;  // kernel32.dll
define constant $MEM-RESET = #x80000;  // kernel32.dll
define constant $PAGE-NOACCESS = 1;  // kernel32.dll
define constant $PAGE-READONLY = 2;  // kernel32.dll
define constant $PAGE-READWRITE = 4;  // kernel32.dll
define constant $PAGE-WRITECOPY = 8;  // kernel32.dll
define constant $PAGE-EXECUTE = 16;  // kernel32.dll
define constant $PAGE-EXECUTE-READ = 32;  // kernel32.dll
define constant $PAGE-EXECUTE-READWRITE = 64;  // kernel32.dll
define constant $PAGE-EXECUTE-WRITECOPY = 128;  // kernel32.dll
define constant $PAGE-GUARD = #x100;  // kernel32.dll
define constant $PAGE-NOCACHE = #x200;  // kernel32.dll

// ─── Standard handles (kernel32) ───────────────────────────────────────

define constant $STD-INPUT-HANDLE = -10;  // kernel32.dll
define constant $STD-OUTPUT-HANDLE = -11;  // kernel32.dll
define constant $STD-ERROR-HANDLE = -12;  // kernel32.dll
define constant $INVALID-HANDLE-VALUE = -1;  // kernel32.dll

// ─── WaitForSingleObject return values (kernel32) ──────────────────────

define constant $WAIT-OBJECT-0 = 0;  // kernel32.dll
define constant $WAIT-ABANDONED = 128;  // kernel32.dll
define constant $WAIT-TIMEOUT = #x102;  // kernel32.dll
define constant $WAIT-FAILED = #xFFFFFFFF;  // kernel32.dll
define constant $INFINITE = #xFFFFFFFF;  // kernel32.dll

// ─── HRESULT severity / common (combase/kernel32) ──────────────────────

define constant $S-OK = 0;  // kernel32.dll
define constant $S-FALSE = 1;  // kernel32.dll
define constant $E-FAIL = #x80004005;  // kernel32.dll
define constant $E-INVALIDARG = #x80070057;  // kernel32.dll
define constant $E-OUTOFMEMORY = #x8007000E;  // kernel32.dll
define constant $E-NOTIMPL = #x80004001;  // kernel32.dll
define constant $E-POINTER = #x80004003;  // kernel32.dll
define constant $E-NOINTERFACE = #x80004002;  // kernel32.dll
define constant $E-ABORT = #x80004004;  // kernel32.dll
define constant $E-ACCESSDENIED = #x80070005;  // kernel32.dll
define constant $E-HANDLE = #x80070006;  // kernel32.dll
define constant $E-UNEXPECTED = #x8000FFFF;  // kernel32.dll

// ─── Win32 error codes (kernel32, from GetLastError) ───────────────────

define constant $ERROR-SUCCESS = 0;  // kernel32.dll
define constant $ERROR-INVALID-FUNCTION = 1;  // kernel32.dll
define constant $ERROR-FILE-NOT-FOUND = 2;  // kernel32.dll
define constant $ERROR-PATH-NOT-FOUND = 3;  // kernel32.dll
define constant $ERROR-ACCESS-DENIED = 5;  // kernel32.dll
define constant $ERROR-INVALID-HANDLE = 6;  // kernel32.dll
define constant $ERROR-NOT-ENOUGH-MEMORY = 8;  // kernel32.dll
define constant $ERROR-INVALID-DATA = 13;  // kernel32.dll
define constant $ERROR-NO-MORE-FILES = 18;  // kernel32.dll
define constant $ERROR-NOT-READY = 21;  // kernel32.dll
define constant $ERROR-BAD-COMMAND = 22;  // kernel32.dll
define constant $ERROR-SHARING-VIOLATION = 32;  // kernel32.dll
define constant $ERROR-LOCK-VIOLATION = 33;  // kernel32.dll
define constant $ERROR-HANDLE-EOF = 38;  // kernel32.dll
define constant $ERROR-FILE-EXISTS = 80;  // kernel32.dll
define constant $ERROR-INVALID-PARAMETER = 87;  // kernel32.dll
define constant $ERROR-BROKEN-PIPE = 109;  // kernel32.dll
define constant $ERROR-BUFFER-OVERFLOW = 111;  // kernel32.dll
define constant $ERROR-DISK-FULL = 112;  // kernel32.dll
define constant $ERROR-INSUFFICIENT-BUFFER = 122;  // kernel32.dll
define constant $ERROR-INVALID-NAME = 123;  // kernel32.dll
define constant $ERROR-ALREADY-EXISTS = 183;  // kernel32.dll
define constant $ERROR-MORE-DATA = 234;  // kernel32.dll
define constant $ERROR-OPERATION-ABORTED = #x3E3;  // kernel32.dll
define constant $ERROR-IO-PENDING = #x3E5;  // kernel32.dll
define constant $ERROR-NOACCESS = #x3E6;  // kernel32.dll
