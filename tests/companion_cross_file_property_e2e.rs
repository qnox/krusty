//! Cross-file companion-property operations must consume the declaration's recorded storage plan.
//! Backing properties use an outer-class static field; field-less properties use accessors on the
//! companion singleton. The using file must never infer that distinction from its own AST, a class
//! name, or whether the declaration came from the current file, a sibling, or a dependency provider.

use super::common;

#[test]
fn cross_file_companion_jvm_field_property_read() {
    const LIB: &str = r#"
open class Result {
    companion object {
        @JvmField
        val NAME: String = "r"
    }
}
"#;
    const USE: &str = r#"
fun name(): String = Result.NAME

fun box(): String = if (name() == "r") "OK" else "F"
"#;

    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Use.kt", USE)],
        "cross-file companion @JvmField property read",
    );
}

#[test]
fn cross_file_companion_plain_property_read() {
    const LIB: &str = r#"
package lib

open class Result {
    companion object {
        val NAME: String = "r"
    }
}
"#;
    const USE: &str = r#"
import lib.Result

fun name(): String = Result.NAME

fun box(): String = if (name() == "r") "OK" else "F"
"#;

    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Use.kt", USE)],
        "cross-file companion plain property read",
    );
}

#[test]
fn cross_file_companion_class_typed_property_read() {
    // A class-typed companion value initialized from a nested-class instance exercises the same
    // storage handoff as scalar values; neither the property's type nor its initializer shape may
    // create a special lowering path.
    const LIB: &str = r#"
package lib

open class Result {
    class Performed : Result()

    companion object {
        @JvmField
        val PERFORMED: Result = Performed()
    }
}
"#;
    const USE: &str = r#"
import lib.Result

fun done(): Result = Result.PERFORMED

fun box(): String = if (done() is Result) "OK" else "F"
"#;

    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Use.kt", USE)],
        "cross-file companion class-typed property read",
    );
}

#[test]
fn cross_file_companion_var_read() {
    const LIB: &str = r#"
package lib

class Counter {
    companion object {
        var count: Int = 41
    }
}
"#;
    const USE: &str = r#"
import lib.Counter

fun read(): Int = Counter.count + 1

fun box(): String = if (read() == 42) "OK" else "F"
"#;

    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Use.kt", USE)],
        "cross-file companion var read",
    );
}

#[test]
fn cross_file_companion_const_read() {
    const LIB: &str = r#"
package lib

class Limits {
    companion object {
        const val MAX: Int = 42
    }
}
"#;
    const USE: &str = r#"
import lib.Limits

fun limit(): Int = Limits.MAX

fun box(): String = if (limit() == 42) "OK" else "F"
"#;

    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Use.kt", USE)],
        "cross-file companion const read",
    );
}

#[test]
fn cross_file_companion_computed_property_read() {
    // A field-less computed companion property (`val X get() = …`) has no hoisted static to
    // read; the cross-file read must call the getter on the Companion singleton, exactly like
    // the same-file read.
    const LIB: &str = r#"
package lib

class Temperature {
    companion object {
        val boiling: Int
            get() = 100
    }
}
"#;
    const USE: &str = r#"
import lib.Temperature

fun read(): Int = Temperature.boiling

fun box(): String = if (read() == 100) "OK" else "F"
"#;

    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Use.kt", USE)],
        "cross-file companion computed property read",
    );
}

#[test]
fn cross_file_companion_computed_property_write_uses_the_same_storage_plan() {
    // Read and write select one declaration signature. A computed `var` has no outer static in either
    // direction, so the sibling file must invoke both accessors on the companion singleton instead of
    // letting the write fall back to the backing-field-only cross-file rejection.
    const LIB: &str = r#"
package lib

var stored: Int = 1

class Gauge {
    companion object {
        var level: Int
            get() = stored
            set(value) { stored = value }
    }
}
"#;
    const USE: &str = r#"
import lib.Gauge

fun update(): Int {
    Gauge.level = 7
    return Gauge.level
}

fun box(): String = if (update() == 7) "OK" else "F"
"#;

    common::expect_box_ok_files_with_stdlib(
        &[("Lib.kt", LIB), ("Use.kt", USE)],
        "cross-file companion computed property read and write",
    );
}
