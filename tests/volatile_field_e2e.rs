//! A property annotated `@Volatile` has an `ACC_VOLATILE` backing field and one annotated
//! `@Transient` an `ACC_TRANSIENT` one, as kotlinc's `computeFieldFlags` sets them: on a class or
//! constructor property, an object property, a companion property hoisted to its outer class and a
//! top-level property. `kotlin.concurrent.Volatile` is the same annotation on the JVM.

use super::common;

const SOURCE: &str = "\
package store\n\
\n\
import kotlin.concurrent.Volatile as SharedVolatile\n\
\n\
annotation class Volatile\n\
annotation class Transient\n\
\n\
class Flags(@kotlin.jvm.Volatile var a: Int, @SharedVolatile var b: Long) {\n\
    @kotlin.jvm.Transient\n\
    var c: String = \"\"\n\
\n\
    @kotlin.jvm.Volatile\n\
    var d: Any? = null\n\
\n\
    @Volatile\n\
    var decoyVolatile: Int = 0\n\
\n\
    @Transient\n\
    var decoyTransient: Int = 0\n\
\n\
    companion object {\n\
        @kotlin.jvm.Volatile\n\
        var shared: Int = 1\n\
    }\n\
}\n\
\n\
object Single {\n\
    @kotlin.jvm.Volatile\n\
    var counter = 0\n\
}\n\
\n\
enum class EnumFlags {\n\
    A;\n\
\n\
    @kotlin.jvm.Volatile\n\
    var state = 0\n\
\n\
    @kotlin.jvm.Transient\n\
    var cache = \"\"\n\
}\n\
\n\
@kotlin.jvm.Volatile\n\
var topLevel: String = \"t\"\n\
";

#[test]
fn a_volatile_or_transient_property_marks_its_field() {
    for class in [
        "store/Flags",
        "store/Flags$Companion",
        "store/Single",
        "store/EnumFlags",
        "store/VolatileFieldKt",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "VolatileField",
            SOURCE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}

/// `@Volatile` is rejected on a `val` before the JVM backend could combine the incompatible
/// `ACC_FINAL` and `ACC_VOLATILE` field flags.
#[test]
fn volatile_is_rejected_on_an_immutable_property() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "ImmutableVolatile.kt",
            "class C {\n\
             \x20   @Volatile val value: Int = 1\n\
             }\n",
        )],
        &[],
    );
}

/// Both failures use the annotation span. kotlinc reports the immutable-property contract before
/// the missing-field target failure, so the application checker must stage them in that order.
#[test]
fn fieldless_immutable_volatile_reports_contract_before_target() {
    common::assert_error_blocks_match_kotlinc(
        &[(
            "FieldlessImmutableVolatile.kt",
            "class C {\n\
             \x20   @Volatile val value: Int get() = 1\n\
             }\n",
        )],
        &[],
    );
}

/// Suppression retains the otherwise-invalid application. kotlinc emits the unusual
/// `FINAL | VOLATILE` pair, so the backend must derive both flags from the checked declaration.
#[test]
fn suppressed_volatile_on_an_immutable_property_remains_a_field_modifier() {
    const SOURCE: &str = "package store\n\
        class Suppressed {\n\
        \x20   @Suppress(\"VOLATILE_ON_VALUE\")\n\
        \x20   @Volatile val value: Int = 1\n\
        }\n";
    common::byte_diff_against_kotlinc_cp(
        "SuppressedVolatile",
        SOURCE,
        "store/Suppressed",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("suppressed immutable @Volatile differs from kotlinc: {diff}"));
}
