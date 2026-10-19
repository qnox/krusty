//! A reified inline body marks `enumValueOf<T>`, `enumValues<T>` and `enumEntries<T>` with the
//! `ENUM_REIFIED` operation (mode 5) followed by an erased placeholder: `aconst_null` before the
//! name operand and `Enum.valueOf(Class, String)`, `iconst_0; anewarray Enum`, or
//! `aconst_null; checkcast EnumEntries`. Inlining it for a concrete enum rewrites that placeholder
//! into the enum's own `valueOf(String)`, `values()` or `getEntries()`
//! (`ReifiedTypeInliner.processSpecialEnumFunction`). Forwarding the parameter to another reified
//! parameter keeps the placeholder and renames the marker.

use super::common;

const LIB: &str = r#"@file:OptIn(ExperimentalStdlibApi::class)
package lib

import kotlin.enums.enumEntries

inline fun <reified T : Enum<T>> parsed(s: String): T = enumValueOf<T>(s)

inline fun <reified T : Enum<T>> all(): Array<T> = enumValues<T>()

inline fun <reified T : Enum<T>> listed(): List<T> = enumEntries<T>()
"#;

const MAIN: &str = r#"import lib.all
import lib.listed
import lib.parsed

enum class Color { RED, GREEN }

inline fun <reified E : Enum<E>> forwarded(s: String): E = parsed<E>(s)

fun parse(s: String): Color = parsed<Color>(s)

fun values(): Array<Color> = all<Color>()

fun entries(): List<Color> = listed<Color>()

fun box(): String {
    if (parse("GREEN") != Color.GREEN) return "parse"
    if (values().size != 2) return "values"
    if (entries() != listOf(Color.RED, Color.GREEN)) return "entries"
    return "OK"
}
"#;

/// [`MAIN`] calling the forwarding template too: the caller inlines a body that already holds
/// the library's operation under a renamed marker, and rewrites it for its own enum.
const FORWARDING_MAIN: &str = r#"import lib.parsed

enum class Color { RED, GREEN }

inline fun <reified E : Enum<E>> forwarded(s: String): E = parsed<E>(s)

fun box(): String = if (forwarded<Color>("RED") == Color.RED) "OK" else "forward"
"#;

#[test]
fn reified_enum_operations_run_against_a_kotlinc_library() {
    for main in [MAIN, FORWARDING_MAIN] {
        let Some(output) = common::expect_box_run_against_kotlinc(LIB, main) else {
            return;
        };
        assert_eq!(output, "OK");
    }
}

#[test]
fn reified_enum_operations_are_inlined_like_kotlinc() {
    let methods = [
        "public static final Color parse(",
        "public static final Color[] values(",
        "public static final java.util.List<Color> entries(",
        "public static final <E extends java.lang.Enum<E>> E forwarded(",
    ];
    let results = common::class_bytes_diffs_against_kotlinc(
        "ReifiedEnum",
        &[("Lib.kt", LIB)],
        MAIN,
        "ReifiedEnumKt",
        &methods,
    )
    .expect("reference kotlinc is provisioned");
    for (method, result) in methods.into_iter().zip(results) {
        result.unwrap_or_else(|difference| panic!("{method}: {difference}"));
    }
}

/// A Java enum declares no `getEntries()`.
const JAVA_COLOR: &str = "package j;\npublic enum JColor { RED, GREEN }\n";

/// Compiled by kotlinc, then given the shape a Kotlin compiler before `EnumEntries` produced:
/// the metadata's `HAS_ENUM_ENTRIES` class flag cleared and `getEntries()` removed (see
/// [`legacy_enum_classes`]). kotlinc 2.4 cannot compile a pre-`EnumEntries` class itself.
const SHADE: &str = "package legacy\n\nenum class Shade { LIGHT, DARK }\n";

/// `enumEntries` of an enum without `getEntries()` reads an `$EntriesIntrinsicMappings` field the
/// consumer generates for it, one per enum in first-use order; `enumValueOf` and `enumValues`
/// call the enum's own static members, which every enum declares.
const MAPPED_MAIN: &str = r#"import j.JColor
import legacy.Shade
import lib.all
import lib.listed
import lib.parsed

fun javaEntries(): List<JColor> = listed<JColor>()

fun legacyEntries(): List<Shade> = listed<Shade>()

fun javaEntriesAgain(): List<JColor> = listed<JColor>()

fun javaParse(s: String): JColor = parsed<JColor>(s)

fun legacyValues(): Array<Shade> = all<Shade>()

fun box(): String {
    if (javaEntries() != listOf(JColor.RED, JColor.GREEN)) return "java entries"
    if (legacyEntries() != listOf(Shade.LIGHT, Shade.DARK)) return "legacy entries"
    if (javaEntriesAgain() !== javaEntries()) return "one mapping per enum"
    if (javaParse("GREEN") != JColor.GREEN) return "java parse"
    if (legacyValues()[1] != Shade.DARK) return "legacy values"
    return "OK"
}
"#;

/// The kotlinc-built `lib` (with [`SHADE`] in its legacy shape) and the javac-built [`JAVA_COLOR`].
fn mapped_enum_libraries() -> Vec<std::path::PathBuf> {
    let library = common::kotlinc_libraries(&[("Lib.kt", LIB), ("Shade.kt", SHADE)])
        .expect("reference kotlinc is provisioned");
    let legacy = legacy_enum_classes(&library, "legacy/Shade");
    let sources = [("JColor.java".to_string(), JAVA_COLOR.to_string())];
    let (java, _) = common::javac_compile(&sources, &[]).expect("javac compiles the Java enum");
    vec![legacy, java]
}

#[test]
fn enum_entries_of_enums_without_get_entries_run() {
    let mut classpath = mapped_enum_libraries();
    classpath.push(common::stdlib_jar());
    let jdk = common::jdk_modules();
    let output = common::expect_box_run(MAPPED_MAIN, "Main", &classpath, Some(jdk.as_path()));
    assert_eq!(output, "OK");
}

#[test]
fn enum_entries_of_enums_without_get_entries_read_a_mapping_like_kotlinc() {
    common::assert_classes_identical_to_kotlinc_against(
        "EntriesMapped",
        MAPPED_MAIN,
        &[
            "EntriesMappedKt",
            "EntriesMappedKt$EntriesIntrinsicMappings",
        ],
        &mapped_enum_libraries(),
    );
}

/// A copy of the class-output directory `library` in which `enum_class` has the shape of an enum
/// compiled before `EnumEntries`: its `@Metadata` class flags without `HAS_ENUM_ENTRIES` (bit 15)
/// and no `getEntries()` method. Both compilers then decide from that declaration that the enum
/// has no entries getter, and a consumer that called it anyway would fail at linkage.
fn legacy_enum_classes(library: &std::path::Path, enum_class: &str) -> std::path::PathBuf {
    let copy = common::scratch_dir().expect("scratch directory");
    copy_tree(library, &copy);
    let path = copy.join(format!("{enum_class}.class"));
    let bytes = std::fs::read(&path).expect("read the enum class");
    std::fs::write(&path, without_enum_entries(&bytes)).expect("write the legacy enum class");
    copy
}

fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).expect("create the copy");
    for entry in std::fs::read_dir(from).expect("read the library") {
        let path = entry.expect("library entry").path();
        let target = to.join(path.file_name().expect("entry name"));
        if path.is_dir() {
            copy_tree(&path, &target);
        } else {
            std::fs::copy(&path, &target).expect("copy a library file");
        }
    }
}

/// `bytes` with the `HAS_ENUM_ENTRIES` metadata flag cleared and `getEntries()` removed.
///
/// `d1` is the one UTF-8-mode metadata string (a leading U+0000, then one char per protobuf
/// byte): a delimited `StringTableTypes`, then the `Class` message, whose first field is `flags`.
fn without_enum_entries(bytes: &[u8]) -> Vec<u8> {
    const HAS_ENUM_ENTRIES: u64 = 1 << 15;
    let u2 = |bytes: &[u8], at: usize| u16::from_be_bytes([bytes[at], bytes[at + 1]]) as usize;
    let u4 = |bytes: &[u8], at: usize| {
        u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize
    };
    let mut out = bytes[..10].to_vec();
    let mut utf8 = std::collections::HashMap::new();
    let mut flags_cleared = false;
    let count = u2(bytes, 8);
    let mut at = 10;
    let mut index = 1;
    while index < count {
        let tag = bytes[at];
        let width = match tag {
            1 => 3 + u2(bytes, at + 1),
            7 | 8 | 16 | 19 | 20 => 3,
            15 => 4,
            3 | 4 | 9 | 10 | 11 | 12 | 17 | 18 => 5,
            5 | 6 => 9,
            other => panic!("constant tag {other}"),
        };
        let mut entry = bytes[at..at + width].to_vec();
        if tag == 1 {
            let chars = modified_utf8_decode(&bytes[at + 3..at + width]);
            if !flags_cleared && chars.first() == Some(&0) && chars.len() > 4 {
                let protobuf: Vec<u8> = chars[1..].iter().map(|&c| c as u8).collect();
                let patched = clear_class_flag(&protobuf, HAS_ENUM_ENTRIES);
                let encoded = modified_utf8_encode(
                    &std::iter::once(0)
                        .chain(patched.iter().map(|&b| u16::from(b)))
                        .collect::<Vec<_>>(),
                );
                entry = vec![1];
                entry.extend((encoded.len() as u16).to_be_bytes());
                entry.extend(encoded);
                flags_cleared = true;
            }
            utf8.insert(index, String::from_utf16_lossy(&chars));
        }
        out.extend(entry);
        at += width;
        index += if matches!(tag, 5 | 6) { 2 } else { 1 };
    }
    assert!(flags_cleared, "the enum carries @Metadata");
    let members = |at: &mut usize| -> Vec<(usize, usize, String)> {
        let count = u2(bytes, *at);
        *at += 2;
        (0..count)
            .map(|_| {
                let start = *at;
                let name = utf8[&u2(bytes, start + 2)].clone();
                let attributes = u2(bytes, start + 6);
                *at += 8;
                for _ in 0..attributes {
                    *at += 6 + u4(bytes, *at + 2);
                }
                (start, *at, name)
            })
            .collect()
    };
    let interfaces = u2(bytes, at + 6);
    let mut methods_at = at + 8 + 2 * interfaces;
    members(&mut methods_at);
    out.extend(&bytes[at..methods_at]);
    let mut end = methods_at;
    let methods = members(&mut end);
    let kept: Vec<_> = methods
        .iter()
        .filter(|(_, _, name)| name != "getEntries")
        .collect();
    assert_eq!(
        kept.len() + 1,
        methods.len(),
        "the enum declares one getEntries()"
    );
    out.extend((kept.len() as u16).to_be_bytes());
    for (start, end, _) in kept {
        out.extend(&bytes[*start..*end]);
    }
    out.extend(&bytes[end..]);
    out
}

/// `protobuf` with `flag` cleared in the `Class` message's leading `flags` varint.
fn clear_class_flag(protobuf: &[u8], flag: u64) -> Vec<u8> {
    let varint = |at: &mut usize| {
        let mut value = 0u64;
        let mut shift = 0;
        loop {
            let byte = protobuf[*at];
            *at += 1;
            value |= u64::from(byte & 0x7f) << shift;
            shift += 7;
            if byte < 0x80 {
                return value;
            }
        }
    };
    let mut at = 0;
    let table = varint(&mut at) as usize;
    at += table;
    assert_eq!(protobuf[at], 0x08, "the Class message opens with its flags");
    let flags_at = at + 1;
    let mut after = flags_at;
    let flags = varint(&mut after);
    assert_ne!(flags & flag, 0, "the class carries the flag");
    let mut value = flags & !flag;
    let mut out = protobuf[..flags_at].to_vec();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
    out.extend(&protobuf[after..]);
    out
}

fn modified_utf8_decode(raw: &[u8]) -> Vec<u16> {
    let mut chars = Vec::new();
    let mut at = 0;
    while at < raw.len() {
        let byte = u16::from(raw[at]);
        if byte < 0x80 {
            chars.push(byte);
            at += 1;
        } else if byte & 0xe0 == 0xc0 {
            chars.push(((byte & 0x1f) << 6) | (u16::from(raw[at + 1]) & 0x3f));
            at += 2;
        } else {
            chars.push(
                ((byte & 0x0f) << 12)
                    | ((u16::from(raw[at + 1]) & 0x3f) << 6)
                    | (u16::from(raw[at + 2]) & 0x3f),
            );
            at += 3;
        }
    }
    chars
}

fn modified_utf8_encode(chars: &[u16]) -> Vec<u8> {
    let mut raw = Vec::new();
    for &c in chars {
        if c != 0 && c < 0x80 {
            raw.push(c as u8);
        } else if c < 0x800 {
            raw.extend([0xc0 | (c >> 6) as u8, 0x80 | (c & 0x3f) as u8]);
        } else {
            raw.extend([
                0xe0 | (c >> 12) as u8,
                0x80 | ((c >> 6) & 0x3f) as u8,
                0x80 | (c & 0x3f) as u8,
            ]);
        }
    }
    raw
}

/// An enum declared in another file of the module being compiled is emitted with `getEntries()`,
/// so `enumEntries` of it calls that getter and generates no mapping class, as kotlinc does for an
/// enum of its own module.
#[test]
fn enum_entries_of_another_file_s_enum_call_its_getter() {
    let tone = "package tones\n\nenum class Tone { LOW, HIGH }\n";
    let main = "import lib.listed\nimport tones.Tone\n\n\
        fun tones(): List<Tone> = listed<Tone>()\n\n\
        fun box(): String = if (tones() == listOf(Tone.LOW, Tone.HIGH)) \"OK\" else \"tones\"\n";
    let sources = [("Tone.kt", tone), ("Main.kt", main)];
    let library = common::kotlinc_library(LIB).expect("reference kotlinc is provisioned");
    let classpath = [library, common::stdlib_jar()];
    let jdk = common::jdk_modules();
    let classes = common::compile_in_process_files(&sources, &classpath, Some(jdk.as_path()))
        .expect("krusty compiles the module");
    let mut names: Vec<&str> = classes
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| !name.ends_with(".kotlin_module"))
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["MainKt", "tones/Tone"]);
    let (_, main_class) = classes
        .iter()
        .find(|(name, _)| name == "MainKt")
        .expect("the file facade");
    assert!(common::class_calls_method(main_class, "getEntries"));
    let output = common::compile_and_run_box_files(&sources, &classpath, Some(jdk.as_path()));
    assert_eq!(output.as_deref(), Some("OK"));
}
