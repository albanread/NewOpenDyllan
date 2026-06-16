Module: dylan
Author: NewOpenDylan stdlib

// ─── locators / URL class hierarchy (system + io libraries) ──────────────────
//
// Pure-Dylan `define class` NAME registrations for the OpenDylan locators
// hierarchy the system/io test corpus references by name (type annotations,
// `make(<file-locator>, …)`, `instance?`). Same AOT-safe route as
// system-classes.dylan / streams.dylan. Ids are pinned by name in
// src/nod-runtime/src/class_pins.rs, so adding these does not renumber any
// other class (no AOT/shim drift).
//
// These carry NO slots and NO operations (merge/simplify/string coercion are
// real behaviour to fill in later); they give the class IDENTITY so a file
// blocked on an undefined locator/URL class name now lowers. The constructed
// classes (<file-locator>, <directory-locator>, the *-url and *-server tree)
// are concrete so `make(<…>, …)` resolves; keyword args are currently ignored
// (no slots) — fine for compilation; the tests do not yet run.
//
// Parents that already exist: <object> (runtime seed), <error> (runtime).

define abstract class <locator> (<object>)
end class;

define abstract class <physical-locator> (<locator>)
end class;

define abstract class <server-locator> (<locator>)
end class;

define abstract class <web-locator> (<locator>)
end class;

define abstract class <file-system-locator> (<physical-locator>)
end class;

define class <directory-locator> (<file-system-locator>)
end class;

define class <file-locator> (<file-system-locator>)
end class;

define class <file-system-directory-locator> (<directory-locator>)
end class;

define class <file-system-file-locator> (<file-locator>)
end class;

define class <posix-file-system-locator> (<file-system-locator>)
end class;

define class <posix-directory-locator> (<directory-locator>)
end class;

define class <posix-file-locator> (<file-locator>)
end class;

define class <native-file-system-locator> (<file-system-locator>)
end class;

define class <native-directory-locator> (<directory-locator>)
end class;

define class <native-file-locator> (<file-locator>)
end class;

define class <microsoft-file-system-locator> (<file-system-locator>)
end class;

define class <microsoft-directory-locator> (<directory-locator>)
end class;

define class <microsoft-file-locator> (<file-locator>)
end class;

define class <microsoft-volume-locator> (<microsoft-file-system-locator>)
end class;

define class <microsoft-unc-locator> (<microsoft-file-system-locator>)
end class;

define class <microsoft-server-locator> (<server-locator>)
end class;

define class <url> (<physical-locator>)
end class;

define class <server-url> (<url>)
end class;

define class <directory-url> (<url>)
end class;

define class <file-url> (<url>)
end class;

define class <file-index-url> (<url>)
end class;

define class <cgi-url> (<url>)
end class;

define class <mail-to-locator> (<web-locator>)
end class;

define class <http-server> (<server-url>)
end class;

define class <https-server> (<server-url>)
end class;

define class <ftp-server> (<server-url>)
end class;

define class <file-server> (<server-url>)
end class;

define class <pathname> (<object>)
end class;

define class <locator-error> (<error>)
end class;
