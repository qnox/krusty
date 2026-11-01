//! A class inheriting an interface default member that declares its own type parameters gets a
//! forwarder whose generic `Signature` declares them, as kotlinc writes it: each bound as the
//! class's supertype substitutes it, the class bound ahead of the interface bounds, and a
//! `suspend` member's CPS shape after the declaration section. A type-parameter position is
//! annotated `@NotNull` only when its bound excludes null, and `@Nullable` when marked `T?`.
//! A type parameter bounded by the interface's own (`fun <T : E>` through `Picker<String>`) erases
//! to the substituted bound, beside an erased bridge, and a local class bound keeps its emitted name.

use super::common;

const LIBRARY: &str = "\
package lib\n\
\n\
interface Picker<E> {\n\
    fun <T : Comparable<T>> least(x: T, y: T): T = if (x < y) x else y\n\
    fun <T> first(xs: List<T>, fallback: E): T = xs[0]\n\
    fun <T> describe(x: T): String = \"$x\"\n\
    fun <T> count(): Int = 1\n\
    fun <T> orNull(x: T?): T? = x\n\
    fun <T : E> narrow(x: T): T = x\n\
    suspend fun <T> echo(x: T): T = x\n\
}\n\
";

const SAME_MODULE: &str = "\
package app\n\
\n\
interface Picker<E> {\n\
    fun <T : Comparable<T>> least(x: T, y: T): T = if (x < y) x else y\n\
    fun <T> first(xs: List<T>, fallback: E): T = xs[0]\n\
    fun <T> describe(x: T): String = \"$x\"\n\
    fun <T> count(): Int = 1\n\
    fun <T> orNull(x: T?): T? = x\n\
    fun <T : E> narrow(x: T): T = x\n\
    suspend fun <T> echo(x: T): T = x\n\
    fun <T, R> T.convert(r: R): R where T : CharSequence, T : Runnable = r\n\
    fun <T : Number> E.pair(x: T): Pair<E, T> = this to x\n\
}\n\
\n\
object Strings : Picker<String>\n\
\n\
class Numbers<Q : Number> : Picker<Q>\n\
\n\
fun locals(): Any {\n\
    class Bound\n\
    class Local : Picker<Bound>\n\
    return Local()\n\
}\n\
";

const CONSUMER: &str = "\
package app\n\
\n\
import lib.Picker\n\
\n\
object Strings : Picker<String>\n\
\n\
class Numbers<Q : Number> : Picker<Q>\n\
";

#[test]
fn a_generic_default_forwarder_declares_its_type_parameters() {
    for class in [
        "app/Strings",
        "app/Numbers",
        "app/GenericDefaultForwarderSignatureKt$locals$Local",
    ] {
        common::byte_diff_against_kotlinc_cp(
            "GenericDefaultForwarderSignature",
            SAME_MODULE,
            class,
            &[common::stdlib_jar()],
        )
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{class} differs from kotlinc: {diff}"));
    }
}

#[test]
fn a_dependency_generic_default_forwarder_declares_its_type_parameters() {
    let library = common::compile_libs_build(
        "DependencyGenericDefaultForwarderLibrary",
        &[("Lib.kt", LIBRARY)],
    )
    .expect("krusty builds the dependency");
    let reference = library
        .reference_out()
        .expect("reference kotlinc builds the dependency")
        .to_path_buf();
    common::assert_classes_identical_to_kotlinc_over_own_libraries(
        "DependencyGenericDefaultForwarder",
        CONSUMER,
        &["app/Strings", "app/Numbers"],
        &[reference],
        &[library.krusty_out().to_path_buf()],
    );
}
