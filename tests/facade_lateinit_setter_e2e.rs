//! A top-level `lateinit` property's accessors match kotlinc's: a write calls the default setter even
//! inside the facade that owns the field (a private property, or one with a `private set`, writes
//! its private field), and the property's annotations, `@get:`/`@set:JvmName` among them, reach
//! its declaration although it has no body.

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
