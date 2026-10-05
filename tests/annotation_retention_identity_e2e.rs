//! An annotation class's `@Retention` is the `kotlin.annotation.AnnotationRetention` entry its checked
//! argument selects, bound through the ordinary scope and import rules. The header inventory read
//! the argument's spelling instead, so an unqualified imported entry (`@Retention(SOURCE)`) or an
//! aliased one (`@Retention(Kept)`) declared no retention: the class kept the runtime default, its
//! class file named the wrong policy, and a `SOURCE` annotation was still written on the functions it
//! annotated. An entry of another enum spelled the same is an argument type mismatch and declares
//! nothing.

use super::common;

const IMPORTED: &str = "import kotlin.annotation.AnnotationRetention.SOURCE\n\
import kotlin.annotation.AnnotationRetention.BINARY as Kept\n\
\n\
@Retention(SOURCE)\n\
annotation class Gone\n\
\n\
@Retention(Kept)\n\
annotation class Binary\n\
\n\
@Retention(AnnotationRetention.RUNTIME)\n\
annotation class Visible\n\
\n\
class Probe {\n\
    @Gone @Binary @Visible\n\
    fun marked() = 1\n\
}\n\
\n\
fun box(): String {\n\
    val method = Probe::class.java.getMethod(\"marked\")\n\
    val names = method.annotations.map { it.annotationClass.simpleName }\n\
    return if (Probe().marked() == 1 && names == listOf(\"Visible\")) \"OK\" else names.toString()\n\
}\n";

#[test]
fn an_imported_or_aliased_retention_entry_declares_the_retention() {
    common::expect_box_same_as_kotlinc(IMPORTED, "RetentionImported");
}

/// The declarations alone, compiled to kotlinc's bytes: each class names the policy its entry
/// selects, and only the binary and runtime annotations are written on `marked`.
const IMPORTED_DECLARATIONS: &str = "import kotlin.annotation.AnnotationRetention.SOURCE\n\
import kotlin.annotation.AnnotationRetention.BINARY as Kept\n\
\n\
@Retention(SOURCE)\n\
annotation class Gone\n\
\n\
@Retention(Kept)\n\
annotation class Binary\n\
\n\
@Retention(AnnotationRetention.RUNTIME)\n\
annotation class Visible\n\
\n\
@Gone @Binary @Visible\n\
fun marked() = 1\n";

#[test]
fn an_imported_or_aliased_retention_entry_compiles_to_kotlincs_classes() {
    common::assert_classes_identical_to_kotlinc(
        "RetentionImportedDeclarations",
        IMPORTED_DECLARATIONS,
        &[
            "Gone",
            "Binary",
            "Visible",
            "RetentionImportedDeclarationsKt",
        ],
    );
}

/// A package-local `AnnotationRetention` shadows the default-imported one, so its `SOURCE` entry is
/// not a retention: kotlinc reports the argument type mismatch (twice, as it does for this
/// annotation argument) and nothing else.
const SHADOWED: &str = "package sample\n\
\n\
enum class AnnotationRetention { RUNTIME, BINARY, SOURCE }\n\
\n\
@Retention(AnnotationRetention.SOURCE)\n\
annotation class Marked\n\
\n\
@Marked\n\
fun use() = 1\n";

#[test]
fn a_same_spelling_entry_of_another_enum_is_rejected_like_kotlinc() {
    let sources = [("Main.kt", SHADOWED)];
    common::assert_errors_match_kotlinc(
        &sources,
        &common::language_directives::kotlinc_args(SHADOWED),
    );
}
