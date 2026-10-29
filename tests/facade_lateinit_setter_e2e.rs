//! A top-level `lateinit` property's accessors match kotlinc's: a write calls the default setter even
//! inside the facade that owns the field (a private property, or one with a `private set`, writes
//! its private field), and the property's annotations, `@get:`/`@set:JvmName` among them, reach
//! its declaration although it has no body. Another class probes `isInitialized` of a private
//! field through the facade's `access$get<X>$p`.

use crate::common;

const DECLARATIONS: &str = r#"
lateinit var shared: String
internal lateinit var module: String
private lateinit var hidden: String
lateinit var guarded: String
    private set
@set:JvmName("assignNamed")
lateinit var named: String
@get:JvmName("readRenamed")
lateinit var renamed: String

fun local(): String {
    shared = "a"
    module = "b"
    hidden = "c"
    guarded = "d"
    named = "e"
    renamed = "f"
    return shared + module + hidden + guarded + named + renamed
}
"#;

const CALLS: &str = r#"
class Writer {
    fun write(): String {
        shared = "f"
        module = "g"
        named = "h"
        renamed = "i"
        return shared + module + named + renamed
    }
}

fun box(): String {
    if (local() != "abcdef") return "local"
    if (Writer().write() != "fghi") return "writer"
    return "OK"
}
"#;

const SOURCES: &[(&str, &str)] = &[("Declarations.kt", DECLARATIONS), ("Calls.kt", CALLS)];

#[test]
fn lateinit_writes_call_the_setter_like_kotlinc() {
    let classes = common::classes_against_kotlinc_module(SOURCES);
    assert_eq!(classes.differences(), Vec::<String>::new());
}

#[test]
fn lateinit_writes_keep_their_semantics() {
    common::expect_box_ok_files_with_stdlib(SOURCES, "FacadeLateinitSetter");
}

const PROBED: &str = r#"
lateinit var shared: String
private lateinit var hidden: String
lateinit var guarded: String
    private set

fun assign() {
    shared = "a"
    hidden = "b"
    guarded = "c"
}

object Probe {
    fun ready() = ::shared.isInitialized && ::hidden.isInitialized && ::guarded.isInitialized
}

fun box(): String {
    if (Probe.ready()) return "early"
    assign()
    return if (Probe.ready()) "OK" else "late"
}
"#;

/// The probe of a private field from `Probe` goes through the facade's bridge. Only `Probe` is
/// compared: kotlinc also drops the guard from the getter of a property probed from another
/// class, which krusty does not reproduce yet (a `docs/SPEC.md` known gap).
#[test]
fn another_class_probes_a_private_lateinit_field_through_its_bridge() {
    common::assert_classes_identical_to_kotlinc("ProbedLateinit", PROBED, &["Probe"]);
    common::expect_box_ok_with_stdlib(PROBED, "ProbedLateinit");
}
