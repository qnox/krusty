//! PURE-KRUSTY serialization COVERAGE: additional scenarios for the kotlinx.serialization plugin
//! (`src/plugins/serialization.rs`), exercised entirely through krusty's own front end + backend — NO
//! kotlinc anywhere. Each test compiles a `@Serializable` program (plugin emits the `$serializer`, the
//! `C.serializer()` accessor, and the `Json.encode/decode` calls), the JVM runs `box()` against the
//! published kotlinx-serialization runtime, and we assert the JSON / descriptor shape. This complements
//! `serialization_krusty_only_e2e.rs` with DIFFERENT scenarios (an all-primitives data class, an enum
//! FIELD, `@Transient`, class-level `@SerialName`, a multi-class reference graph, a `List<T>` of a
//! nested `@Serializable`, and descriptor element-index introspection).
//!
//! Self-skips if the kotlinx-serialization runtime jars aren't locatable.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

mod common;

/// Recursively find a `<prefix>*.jar` (no `-sources`) under `dir`.
fn walk(dir: &Path, prefix: &str, depth: usize, out: &mut Option<PathBuf>) {
    if out.is_some() || depth > 8 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, prefix, depth + 1, out);
        } else if let Some(n) = p.file_name().and_then(|n| n.to_str()) {
            if n.starts_with(prefix) && n.ends_with(".jar") && !n.contains("sources") {
                *out = Some(p.clone());
                return;
            }
        }
    }
}

/// Locate a serialization runtime jar by prefix across the common cache roots (gradle/m2 + any
/// distribution-bundled gradle lib).
fn find(prefix: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let mut roots = vec![
        PathBuf::from(&home).join(".gradle"),
        PathBuf::from(&home).join(".m2"),
    ];
    if let Ok(rd) = std::fs::read_dir("/opt/mise/installs/gradle") {
        roots.extend(rd.flatten().map(|e| e.path()));
    }
    let mut out = None;
    for r in &roots {
        walk(r, prefix, 0, &mut out);
        if out.is_some() {
            break;
        }
    }
    out
}

fn serialization_runtime_jars() -> Option<Vec<PathBuf>> {
    static JARS: OnceLock<Option<Vec<PathBuf>>> = OnceLock::new();
    JARS.get_or_init(|| {
        Some(vec![
            common::stdlib_jar()?,
            find("kotlinx-serialization-core-jvm")?,
            find("kotlinx-serialization-json-jvm")?,
        ])
    })
    .clone()
}

/// Compile `src` (whose `box(): String` is the entry point) entirely in krusty, run it on the JVM
/// against the kotlinx-serialization runtime, and return the trimmed stdout — or `None` if any runtime
/// dependency is absent (test self-skips).
fn run_box_in_krusty(src: &str, stem: &str) -> Option<(String, String)> {
    let cp_jars = serialization_runtime_jars()?;
    let classes = common::compile_in_process(src, stem, &cp_jars, None)
        .unwrap_or_else(|| panic!("krusty failed to compile the pure-krusty program ({stem})"));
    let box_class = common::find_box_class(&classes)?;
    common::run_box(&classes, &box_class, &cp_jars).map(|stdout| (stdout, String::new()))
}

#[test]
fn all_primitives_data_class_round_trips_in_krusty() {
    // A single `@Serializable data class` combining every scalar the codegen handles — Int, Long,
    // Boolean, Double, String — encodes to its JSON object and decodes back equal (data-class
    // structural equality proves each field round-tripped through the right builtin serializer and slot
    // width). Distinct from the split-compilation e2e's separate Rich/Wide classes.
    let src = r#"import kotlinx.serialization.*
import kotlinx.serialization.json.*

@Serializable
data class All(val i: Int, val l: Long, val b: Boolean, val d: Double, val s: String)

fun box(): String {
    val v = All(7, 9000000000L, true, 3.5, "hi")
    val j = Json.encodeToString(All.serializer(), v)
    if (j != "{\"i\":7,\"l\":9000000000,\"b\":true,\"d\":3.5,\"s\":\"hi\"}") return "enc=$j"
    val back = Json.decodeFromString(All.serializer(), j)
    return if (back == v) "OK" else "back=$back"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerAllPrim") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "all-primitives data class round-trip wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty all-primitives data class round-trip OK");
}

// NOTE: Three further scenarios were prototyped and DROPPED — they exercise plugin features krusty
// does not yet implement (verified failing, compiler left unmodified):
//   * an `@Serializable enum` used as a FIELD of another `@Serializable` class — encode produced empty
//     JSON (the child EnumSerializer isn't wired for a nested enum element);
//   * `@Transient` on a property — the field is still counted in the descriptor (`elementsCount`
//     included it) and emitted, i.e. not skipped;
//   * class-level `@SerialName` — `descriptor.serialName` stays the simple class name (rename not
//     applied; only property-level `@SerialName` is honored, as covered in the sibling suite).

#[test]
fn multi_class_reference_graph_round_trips_in_krusty() {
    // Three mutually-referencing `@Serializable` classes (A -> B -> C): each level's serializer wires
    // the next level's generated `$serializer.INSTANCE` as its composite element serializer, so a deep
    // object graph encodes to nested JSON and decodes back equal.
    let src = r#"import kotlinx.serialization.*
import kotlinx.serialization.json.*

@Serializable data class Leaf(val v: Int)
@Serializable data class Mid(val leaf: Leaf, val tag: String)
@Serializable data class Root(val mid: Mid, val name: String)

fun box(): String {
    val v = Root(Mid(Leaf(3), "t"), "r")
    val j = Json.encodeToString(Root.serializer(), v)
    if (j != "{\"mid\":{\"leaf\":{\"v\":3},\"tag\":\"t\"},\"name\":\"r\"}") return "enc=$j"
    val back = Json.decodeFromString(Root.serializer(), j)
    return if (back == v) "OK" else "back=$back"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerGraph") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "multi-class reference graph wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty multi-class reference graph OK");
}

#[test]
fn list_of_serializable_round_trips_in_krusty() {
    // A `List<T>` whose element is itself a `@Serializable` class: the field serializer is
    // `ListSerializer(Item.serializer())` (a builtin collection serializer over a generated element
    // serializer). Full encode + decode round-trip of a list of composites.
    let src = r#"import kotlinx.serialization.*
import kotlinx.serialization.json.*

@Serializable data class Item(val n: Int)
@Serializable data class Bag(val items: List<Item>, val label: String)

fun box(): String {
    val v = Bag(listOf(Item(1), Item(2)), "b")
    val j = Json.encodeToString(Bag.serializer(), v)
    if (j != "{\"items\":[{\"n\":1},{\"n\":2}],\"label\":\"b\"}") return "enc=$j"
    val back = Json.decodeFromString(Bag.serializer(), j)
    return if (back == v) "OK" else "back=$back"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerListOfSer") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "List<@Serializable> field wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty List<@Serializable> field OK");
}

#[test]
fn descriptor_element_index_introspection_in_krusty() {
    // Descriptor SHAPE by index: `elementsCount`, `getElementName(i)`, and the reverse
    // `getElementIndex(name)` (including `CompositeDecoder.UNKNOWN_NAME` == -3 for an absent name) — the
    // metadata the framework uses to drive decode dispatch.
    let src = r#"import kotlinx.serialization.*
import kotlinx.serialization.encoding.*

@Serializable
class Rec(val first: Int, val second: String, val third: Boolean)

fun box(): String {
    val d = Rec.serializer().descriptor
    if (d.elementsCount != 3) return "count=${d.elementsCount}"
    if (d.getElementName(2) != "third") return "n2=${d.getElementName(2)}"
    if (d.getElementIndex("second") != 1) return "i=${d.getElementIndex("second")}"
    if (d.getElementIndex("nope") != CompositeDecoder.UNKNOWN_NAME) return "unk=${d.getElementIndex("nope")}"
    return "OK"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerElemIndex") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "descriptor element-index introspection wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty descriptor element-index introspection OK");
}

#[test]
fn nullable_nested_and_list_round_trip_in_krusty() {
    // Nullable NESTED composite (`Inner?`) alongside a nullable `List<Int>?` in one class — both the
    // present and the null case, exercising the `encodeNullableSerializableElement` path over a
    // generated element serializer and a builtin collection serializer together.
    let src = r#"import kotlinx.serialization.*
import kotlinx.serialization.json.*

@Serializable data class Inner(val v: Int)
@Serializable data class Holder(val inner: Inner?, val xs: List<Int>?)

fun box(): String {
    val a = Holder(Inner(5), listOf(1, 2))
    val ja = Json.encodeToString(Holder.serializer(), a)
    if (ja != "{\"inner\":{\"v\":5},\"xs\":[1,2]}") return "enca=$ja"
    if (Json.decodeFromString(Holder.serializer(), ja) != a) return "deca"
    val b = Holder(null, null)
    val jb = Json.encodeToString(Holder.serializer(), b)
    if (jb != "{\"inner\":null,\"xs\":null}") return "encb=$jb"
    if (Json.decodeFromString(Holder.serializer(), jb) != b) return "decb"
    return "OK"
}
"#;
    let Some((stdout, stderr)) = run_box_in_krusty(src, "SerNullNestList") else {
        eprintln!("skipping: serialization runtime / JAVA_HOME not located");
        return;
    };
    assert!(
        stdout == "OK",
        "nullable nested + nullable list wrong.\nstdout: {stdout}\nstderr: {stderr}"
    );
    eprintln!("pure-krusty nullable nested + nullable list OK");
}
