//! Enum `entries` members for kotlinc byte-parity: Kotlin 2.x generates, on EVERY `enum class`, a
//! `private static final kotlin.enums.EnumEntries $ENTRIES` field (initialized in `<clinit>` from
//! `EnumEntriesKt.enumEntries($VALUES)`), a `public static EnumEntries<E> getEntries()` accessor, and a
//! private synthetic `$values()` array builder the `<clinit>` calls. krusty previously emitted neither
//! `$ENTRIES`/`getEntries` nor a `$values()` helper (it inlined the array build), so every enum class
//! diverged from kotlinc. Verified byte-identical in the differential harness; here we assert the
//! members exist and the enum still runs.

use super::common;

/// `None` means ONLY that the toolchain isn't provisioned — a source krusty REJECTS panics with its
/// diagnostics rather than skipping the assertions below.
fn classes(src: &str) -> Option<Vec<(String, Vec<u8>)>> {
    let stdlib = common::stdlib_jar();
    let jdk = common::jdk_modules();
    Some(common::expect_compile_in_process(
        src,
        "E",
        &[stdlib],
        Some(jdk.as_path()),
    ))
}

fn class_of(cs: &[(String, Vec<u8>)], name: &str) -> Option<krusty::jvm::classreader::ClassInfo> {
    cs.iter()
        .find(|(n, _)| n == name)
        .and_then(|(_, b)| krusty::jvm::classreader::parse_class(b).ok())
}

#[test]
fn enum_emits_entries_field_and_accessor() {
    let src = "enum class Color { RED, GREEN, BLUE }\n\
        fun box(): String = if (Color.RED.ordinal == 0) \"OK\" else \"no\"\n";
    let Some(cs) = classes(src) else {
        return; // toolchain unavailable
    };
    let ci = class_of(&cs, "Color").expect("Color class");

    // The `$ENTRIES` field, kotlinc's `entries` backing.
    let entries = ci
        .fields
        .iter()
        .find(|f| f.name == "$ENTRIES")
        .expect("$ENTRIES field");
    assert_eq!(entries.descriptor, "Lkotlin/enums/EnumEntries;");

    // The synthetic `$values()` array builder and the `getEntries()` accessor.
    assert!(
        ci.methods.iter().any(|m| m.name == "$values"),
        "missing $values() helper"
    );
    let get = ci
        .methods
        .iter()
        .find(|m| m.name == "getEntries")
        .expect("getEntries()");
    assert_eq!(get.descriptor, "()Lkotlin/enums/EnumEntries;");

    // Runs on a real JVM (the `<clinit>` `EnumEntriesKt.enumEntries` call resolves against stdlib).
    if let Some(box_class) = common::find_box_class(&cs) {
        let stdlib = common::stdlib_jar();
        assert_eq!(
            common::run_box(&cs, &box_class, &[stdlib]).as_deref(),
            Some("OK")
        );
    }
}

#[test]
fn plain_enum_ctor_is_private_nonsynthetic_with_signature() {
    // kotlinc's enum ctor is `private` (NOT synthetic) and carries a generic `Signature` listing only
    // the USER params (`()V` for a plain enum) — javap reads it to hide the synthetic `(String,int)`.
    // These three together make a plain enum's `<init>` byte-identical to kotlinc.
    let src = "enum class Color { RED, GREEN, BLUE }\nfun box(): String = \"OK\"\n";
    let Some(cs) = classes(src) else {
        return;
    };
    let ci = class_of(&cs, "Color").expect("Color class");
    let ctor = ci
        .methods
        .iter()
        .find(|m| m.name == "<init>")
        .expect("<init>");
    assert_eq!(
        ctor.access & 0x1000,
        0,
        "enum ctor must NOT be ACC_SYNTHETIC"
    );
    assert_ne!(ctor.access & 0x0002, 0, "enum ctor must be ACC_PRIVATE");
    assert_eq!(
        ctor.signature.as_deref(),
        Some("()V"),
        "enum ctor needs the `()V` generic Signature"
    );
}

#[test]
fn enum_with_ctor_and_method_still_emits_entries() {
    let src = "enum class Planet(val mass: Int) {\n\
        \x20   EARTH(5), MARS(6);\n\
        \x20   fun heavy(): Boolean = mass > 5\n\
        }\n\
        fun box(): String = if (Planet.MARS.heavy()) \"OK\" else \"no\"\n";
    let Some(cs) = classes(src) else {
        return;
    };
    let ci = class_of(&cs, "Planet").expect("Planet class");
    assert!(ci.fields.iter().any(|f| f.name == "$ENTRIES"));
    assert!(ci.methods.iter().any(|m| m.name == "getEntries"));
    assert!(ci.methods.iter().any(|m| m.name == "$values"));
    if let Some(box_class) = common::find_box_class(&cs) {
        let stdlib = common::stdlib_jar();
        assert_eq!(
            common::run_box(&cs, &box_class, &[stdlib]).as_deref(),
            Some("OK")
        );
    }
}

#[test]
fn enum_entries_stays_visible_through_a_companion_value() {
    const SRC: &str = "\
enum class Mode(val state: String) {\n\
    A(\"a\"), B(\"b\");\n\
    companion object {\n\
        fun fromName(name: String): Mode? = Mode.entries.find { it.state == name }\n\
        fun text() = Mode.entries.joinToString { it.state }\n\
    }\n\
}\n\
fun listed() = Mode.entries.joinToString { it.state }\n\
fun box(): String {\n\
    if (listed() != \"a, b\") return \"listed\"\n\
    if (Mode.text() != \"a, b\") return \"text\"\n\
    if (Mode.fromName(\"b\") != Mode.B) return \"find\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "enum_companion_entries");
}

#[test]
fn a_companion_member_named_entries_stays_on_the_companion() {
    const SRC: &str = "\
enum class Shadow {\n\
    A;\n\
    companion object {\n\
        val entries = 1\n\
    }\n\
}\n\
fun box(): String {\n\
    if (Shadow.entries.size != 1) return \"size\"\n\
    if (Shadow.Companion.entries != 1) return \"companion\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "enum_companion_entries_member");
}

#[test]
fn inferred_enum_entries_outranks_a_companion_member() {
    const SRC: &str = "\
enum class Shadow {\n\
    A;\n\
    companion object {\n\
        val entries = 7\n\
    }\n\
}\n\
fun count() = Shadow.entries.size\n\
fun companionValue() = Shadow.Companion.entries\n\
fun box(): String {\n\
    if (count() != 1) return \"count\"\n\
    if (companionValue() != 7) return \"companion\"\n\
    return \"OK\"\n\
}\n";
    common::expect_box_same_as_kotlinc(SRC, "enum_companion_entries_inferred");
}
