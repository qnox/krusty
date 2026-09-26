//! `is`, `as` and `as?` against a mutable collection or a function type go through kotlinc's
//! `TypeIntrinsics`, and a class implementing a Kotlin collection carries kotlinc's marker
//! interface.
//!
//! A mutable collection shares its JVM interface with its read-only face, so only the
//! `KMappedMarker`/`KMutableX` markers tell a Kotlin read-only implementation apart:
//! `TypeIntrinsics.isMutableIterator` rejects a `KMappedMarker` that is not a `KMutableIterator`.
//! A function type erases to `FunctionN`, which a lambda of any arity may implement, so its checks
//! ask `isFunctionOfArity`. krusty wrote a bare `instanceof`/`checkcast` and no markers, so a
//! read-only iterator passed `is MutableIterator<*>`. A reified argument from a library body goes
//! through kotlinc's inliner, which also null-checks a non-null `as` and tests an `as?` first.
use super::common;
use super::common::{compare_with_kotlinc_plugin, method_instructions};

const LIBRARY: &str = "@file:Suppress(\"INVISIBLE_REFERENCE\", \"INVISIBLE_MEMBER\")\n\
package intrinsicfixture\n\
@kotlin.internal.InlineOnly\n\
inline fun <reified T> checks(v: Any?): Boolean = v is T\n\
@kotlin.internal.InlineOnly\n\
inline fun <reified T> casts(v: Any?): T = v as T\n\
@kotlin.internal.InlineOnly\n\
inline fun <reified T> safely(v: Any?): T? = v as? T\n";

const SOURCE: &str = "import intrinsicfixture.*\n\
class Token\n\
class ReadOnlyIterator : Iterator<String> {\n\
    override fun hasNext(): Boolean = false\n\
    override fun next(): String = \"\"\n\
}\n\
class WritableIterator : MutableIterator<String> {\n\
    override fun hasNext(): Boolean = false\n\
    override fun next(): String = \"\"\n\
    override fun remove() {}\n\
}\n\
abstract class WritableSource : MutableIterator<String>, Iterable<String>\n\
interface Listing : List<String>\n\
fun isWritable(v: Any?): Boolean = v is MutableIterator<*>\n\
fun isNotWritable(v: Any?): Boolean = v !is MutableIterator<*>\n\
fun asWritable(v: Any?): MutableIterator<*> = v as MutableIterator<*>\n\
fun asWritableOrNull(v: Any?): MutableIterator<*>? = v as MutableIterator<*>?\n\
fun safeWritable(v: Any?): MutableIterator<*>? = v as? MutableIterator<*>\n\
fun branchOnWritable(v: Any?): Int = when (v) { is MutableSet<*> -> 1; is MutableIterator<*> -> 2; else -> 3 }\n\
fun isBinary(v: Any?): Boolean = v is Function2<*, *, *>\n\
fun isWrittenUnary(v: Any?): Boolean = v is Function1<*, *>\n\
@Suppress(\"UNCHECKED_CAST\")\n\
fun asUnary(v: Any?): (Int) -> Int = v as (Int) -> Int\n\
fun safeUnary(v: Any?): Any? = v as? (Int) -> Int\n\
fun reifiedIsWritable(v: Any?): Boolean = checks<MutableIterator<*>>(v)\n\
fun reifiedAsWritable(v: Any?): MutableIterator<*> = casts<MutableIterator<*>>(v)\n\
fun reifiedSafeWritable(v: Any?): MutableIterator<*>? = safely<MutableIterator<*>>(v)\n\
fun reifiedAsToken(v: Any?): Token = casts<Token>(v)\n\
fun reifiedAsTokenOrNull(v: Any?): Token? = casts<Token?>(v)\n\
fun reifiedSafeToken(v: Any?): Token? = safely<Token>(v)\n\
fun reifiedIsUnary(v: Any?): Boolean = checks<(Int) -> Int>(v)\n";

const BOX: &str = "fun castFails(v: Any?): Boolean {\n\
    try { asWritable(v) } catch (e: ClassCastException) { return true }\n\
    return false\n\
}\n\
fun reifiedCastFails(v: Any?): Boolean {\n\
    try { reifiedAsWritable(v) } catch (e: ClassCastException) { return true }\n\
    return false\n\
}\n\
fun reifiedNullFails(): Boolean {\n\
    try { reifiedAsToken(null) } catch (e: NullPointerException) { return true }\n\
    return false\n\
}\n\
fun box(): String {\n\
    val readOnly: Any = ReadOnlyIterator()\n\
    val writable: Any = WritableIterator()\n\
    val unary: Any = { n: Int -> n }\n\
    val binary: Any = { a: Int, b: Int -> a + b }\n\
    if (isWritable(readOnly) || !isWritable(writable) || !isNotWritable(readOnly)) return \"fail is\"\n\
    if (!castFails(readOnly) || castFails(writable)) return \"fail as\"\n\
    if (asWritableOrNull(null) != null) return \"fail as null\"\n\
    if (safeWritable(readOnly) != null || safeWritable(writable) !== writable) return \"fail as?\"\n\
    if (branchOnWritable(readOnly) != 3 || branchOnWritable(writable) != 2) return \"fail when\"\n\
    if (isBinary(unary) || !isBinary(binary) || !isWrittenUnary(unary)) return \"fail function is\"\n\
    if (asUnary(unary)(4) != 4 || safeUnary(binary) != null) return \"fail function as\"\n\
    if (reifiedIsWritable(readOnly) || !reifiedIsWritable(writable)) return \"fail reified is\"\n\
    if (!reifiedCastFails(readOnly) || reifiedCastFails(writable)) return \"fail reified as\"\n\
    if (reifiedSafeWritable(readOnly) != null) return \"fail reified as?\"\n\
    if (!reifiedNullFails() || reifiedAsTokenOrNull(null) != null) return \"fail reified null\"\n\
    if (reifiedSafeToken(\"x\") != null) return \"fail reified token as?\"\n\
    if (!reifiedIsUnary(unary) || reifiedIsUnary(binary)) return \"fail reified function\"\n\
    return \"OK\"\n\
}\n";

#[test]
fn type_intrinsics_tell_mutable_collections_and_function_arities_apart() {
    let result = common::expect_box_run_against_kotlinc(LIBRARY, &format!("{SOURCE}{BOX}"))
        .expect("reference kotlinc is provisioned");
    assert_eq!(result, "OK");
}

#[test]
fn type_checks_and_casts_match_kotlinc() {
    let library = common::kotlinc_library(LIBRARY).expect("reference kotlinc is provisioned");
    let built = compare_with_kotlinc_plugin(
        "TypeIntrinsics",
        SOURCE,
        "TypeIntrinsicsKt",
        &[common::stdlib_jar(), library],
        "25",
        &[],
    )
    .expect("reference kotlinc is provisioned");
    for member in [
        "boolean isWritable(",
        "boolean isNotWritable(",
        "java.util.Iterator<?> asWritable(",
        "java.util.Iterator<?> asWritableOrNull(",
        "java.util.Iterator<?> safeWritable(",
        "int branchOnWritable(",
        "boolean isBinary(",
        "boolean isWrittenUnary(",
        "asUnary(",
        "java.lang.Object safeUnary(",
        "boolean reifiedIsWritable(",
        "java.util.Iterator<?> reifiedAsWritable(",
        "java.util.Iterator<?> reifiedSafeWritable(",
        "Token reifiedAsToken(",
        "Token reifiedAsTokenOrNull(",
        "Token reifiedSafeToken(",
        "boolean reifiedIsUnary(",
    ] {
        let reference = method_instructions(&built.reference, member);
        assert!(!reference.is_empty(), "kotlinc: {member} not found");
        assert_eq!(
            method_instructions(&built.krusty, member),
            reference,
            "{member}"
        );
    }
}

/// The class header line javap prints, which spells the interfaces from the `Signature`, and the
/// raw interface table's entries in order. (kotlinc also writes a throwing `remove()` stub into a
/// read-only collection implementation, which krusty does not yet, so members are not compared.)
fn class_header(disassembly: &str, class: &str) -> Vec<String> {
    let header = disassembly.lines().filter(|line| {
        line.contains(&format!(" {class} "))
            && (line.contains("class ") || line.contains("interface "))
    });
    let interfaces = disassembly
        .lines()
        .skip_while(|line| !line.starts_with("Constant pool:"))
        .filter(|line| line.contains("= Class ") && line.contains("markers/"));
    header.chain(interfaces).map(str::to_owned).collect()
}

#[test]
fn collection_implementations_carry_kotlinc_markers() {
    for class in [
        "ReadOnlyIterator",
        "WritableIterator",
        "WritableSource",
        "Listing",
    ] {
        let library = common::kotlinc_library(LIBRARY).expect("reference kotlinc is provisioned");
        let built = compare_with_kotlinc_plugin(
            "TypeIntrinsics",
            SOURCE,
            class,
            &[common::stdlib_jar(), library],
            "25",
            &[],
        )
        .expect("reference kotlinc is provisioned");
        let reference = class_header(&built.reference, class);
        assert!(
            reference.len() >= 2,
            "kotlinc: {class} header or markers not found"
        );
        assert_eq!(class_header(&built.krusty, class), reference, "{class}");
    }
}
