//! Name-keyed stable class-id pins for library classes (runtime-seeded
//! `ensure_*` classes + stdlib `define class`es). Decouples a library
//! class's id from registration ORDER: its id is looked up BY NAME here
//! (see `allocate_user_class_id_named` in classes.rs), so adding /
//! removing / reordering a stdlib class never renumbers another class —
//! which is what made the AOT/shim class-id drift recur.
//!
//! APPEND-ONLY CONTRACT: to add a library class, append ONE row at the
//! next free id (`PIN_CEILING`) and bump `PIN_CEILING`. NEVER reorder or
//! renumber existing rows — every baked artifact (AOT EXEs, the cached
//! parser shim, dump snapshots) depends on these exact values. The
//! `class_pins_complete_and_consistent` test asserts every registered
//! library class is pinned here.

/// `(class-name, pinned-id)` for every library class, id in
/// `[ClassId::FIRST_USER, PIN_CEILING)`.
pub const CLASS_PINS: &[(&str, u32)] = &[
    ("<condition>", 1024),
    ("<warning>", 1025),
    ("<serious-condition>", 1026),
    ("<error>", 1027),
    ("<simple-condition>", 1028),
    ("<simple-error>", 1029),
    ("<simple-warning>", 1030),
    ("<no-applicable-methods-error>", 1031),
    ("<no-next-method-error>", 1032),
    ("<simple-restart>", 1033),
    ("<exit-procedure>", 1034),
    ("<collection>", 1035),
    ("<mutable-collection>", 1036),
    ("<sequence>", 1037),
    ("<mutable-sequence>", 1038),
    ("<explicit-key-collection>", 1039),
    ("<stretchy-collection>", 1040),
    ("<iteration-state>", 1041),
    ("<range>", 1042),
    ("<stretchy-vector>", 1043),
    ("<out-of-range-error>", 1044),
    ("<table>", 1045),
    ("<not-hashable-error>", 1046),
    ("<c-struct>", 1047),
    ("<point>", 1048),
    ("<rect>", 1049),
    ("<size>", 1050),
    ("<filetime>", 1051),
    ("<systemtime>", 1052),
    ("<msg>", 1053),
    ("<wndclassexw>", 1054),
    ("<paintstruct>", 1055),
    ("<function>", 1056),
    ("<wrong-number-of-arguments-error>", 1057),
    ("<cell>", 1058),
    ("<environment>", 1059),
    ("<c-bool>", 1060),
    ("<c-int>", 1061),
    ("<c-uint>", 1062),
    ("<c-short>", 1063),
    ("<c-ushort>", 1064),
    ("<c-long>", 1065),
    ("<c-ulong>", 1066),
    ("<c-longlong>", 1067),
    ("<c-ulonglong>", 1068),
    ("<c-dword>", 1069),
    ("<c-word>", 1070),
    ("<c-byte>", 1071),
    ("<c-pointer>", 1072),
    ("<c-handle>", 1073),
    ("<c-string>", 1074),
    ("<c-wide-string>", 1075),
    ("<c-float>", 1076),
    ("<c-double>", 1077),
    ("<c-ffi-error>", 1078),
    ("<array>", 1079),
    ("<vector>", 1080),
    ("<simple-vector>", 1081),
    ("<byte-vector>", 1082),
    ("<bit-vector>", 1083),
    ("<synchronization>", 1084),
    ("<lock>", 1085),
    ("<simple-lock>", 1086),
    ("<recursive-lock>", 1087),
    ("<semaphore>", 1088),
    ("<notification>", 1089),
    ("<thread>", 1090),
    ("<generic-function>", 1091),
    ("<float-vector>", 1092),
    ("<type-error>", 1093),
    ("<type>", 1094),
    ("<class>", 1095),
    ("<singleton>", 1096),
    ("<number>", 1097),
    ("<complex>", 1098),
    ("<real>", 1099),
    ("<rational>", 1100),
    ("<float>", 1101),
    ("<byte>", 1102),
    ("<bit>", 1103),
    ("<extended-float>", 1104),
    ("<restart>", 1105),
    ("<arithmetic-error>", 1106),
    ("<arithmetic-overflow-error>", 1107),
    ("<sealed-object-error>", 1108),
    ("<stream-error>", 1109),
    ("<end-of-stream-error>", 1110),
    ("<incomplete-read-error>", 1111),
    ("<incomplete-write-error>", 1112),
    ("<test-input-stream>", 1113),
    ("<test-output-stream>", 1114),
    ("<list>", 1115),
    ("<deque>", 1116),
    ("<object-deque>", 1117),
    ("<set>", 1118),
    ("<mutable-explicit-key-collection>", 1119),
    ("<object-table>", 1120),
    ("<string-table>", 1121),
    ("<stretchy-sequence>", 1122),
    ("<stretchy-object-vector>", 1123),
    ("<single-float-vector>", 1124),
    ("<double-float-vector>", 1125),
    ("<stream>", 1126),
    ("<string-stream>", 1127),
];

/// One past the highest pinned id — the base of the per-program USER class
/// band. User (`define class`) ids allocate from here, so they do not
/// depend on the count of pinned library classes.
pub const PIN_CEILING: u32 = 1128;

/// Look up a library class's pinned id by name.
pub fn pinned_id(name: &str) -> Option<u32> {
    CLASS_PINS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, id)| *id)
}
