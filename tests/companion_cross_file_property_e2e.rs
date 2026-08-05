use super::common;

// Reading a companion object's PROPERTY through the plain `ClassName.prop` shape from ANOTHER
// file of the same module: the checker resolves it through the class's `static_props`, and the
// IR backend must emit the same `getstatic ClassName.prop` the same-file read uses (krusty
// hoists a backing-field companion property to a static on the outer class). Cross-file
// companion FUNCTION calls and classpath companion property reads already worked.

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
    // The intellij-community `AnActionResult.PERFORMED` shape: a class-typed companion val
    // initialized from a nested-class instance, read from another file.
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
