//! A runtime type test checks only the target's classifier, so kotlinc (FIR's `isCastErased`)
//! accepts `x is C<A>` only when the operand's static type proves the rest of `C<A>` and otherwise
//! reports CANNOT_CHECK_FOR_ERASED. The operand's type is the smart-cast intersection when flow has
//! narrowed it: after `f as Function2<Int, Continuation<Int>, Any?>` a `suspend (Int) -> Int` value
//! is both, so `f is SuspendFunction1<Int, Any?>` is an upcast. krusty kept only the cast type and
//! rejected that check, and it reported no erased check at all for an ordinary generic classifier
//! target (`x is Impl<String>` on `x: Any`). Accepted fixtures run like kotlinc's build; their
//! declarations compile to kotlinc's class files byte for byte. The `box` callers stay out of the
//! byte comparison: they exercise unrelated emission (`Ref.IntRef` initialization, the nullable
//! `is T?` expansion, a bound reference's `checkcast` to its function carrier).

use super::common;

const ACCEPTED: &str = "import kotlin.coroutines.Continuation\n\
import kotlin.coroutines.SuspendFunction1\n\
import kotlin.coroutines.EmptyCoroutineContext\n\
import kotlin.coroutines.startCoroutine\n\
\n\
interface Base<out T> { val value: T }\n\
class Impl<T>(override val value: T) : Base<T>\n\
class Pair2<A, B>(val first: A, val second: B)\n\
\n\
fun narrowed(b: Base<String>) = b is Impl<String>\n\
fun starred(x: Any) = x is Impl<*>\n\
fun readable(x: Any) = x is Base<Any?>\n\
fun nullable(b: Base<String>?) = b is Impl<String>?\n\
fun afterCast(x: Any): Boolean {\n\
    x as Base<String>\n\
    return x is Impl<String>\n\
}\n\
fun <T : Any> notNull(x: T?) = x is T\n\
inline fun <reified T> reified(x: Any?) = x is T\n\
fun partial(x: Pair2<String, *>) = x is Pair2<String, *>\n\
\n\
fun suspendAfterCast(f: suspend (Int) -> Int): Boolean {\n\
    @Suppress(\"UNCHECKED_CAST\")\n\
    f as Function2<Int, Continuation<Int>, Any?>\n\
    return f is SuspendFunction1<Int, Any?> && f is Function2<Int, Continuation<Int>, Any?>\n\
}\n\
\n\
fun box(): String {\n\
    var result = 0\n\
    val f: suspend (Int) -> Int = { it + 1 }\n\
    suspend { f(41) }.startCoroutine(Continuation(EmptyCoroutineContext) { result = it.getOrThrow() })\n\
    val checks = listOf(\n\
        narrowed(Impl(\"a\")), starred(Impl(1)), readable(Impl(2)), nullable(null),\n\
        afterCast(Impl(\"b\")), notNull<String>(\"c\"), reified<String>(\"d\"),\n\
        partial(Pair2(\"e\", 1)), suspendAfterCast(f), result == 42,\n\
    )\n\
    return if (checks.all { it }) \"OK\" else checks.toString()\n\
}\n";

#[test]
fn a_type_test_the_operand_proves_runs_like_kotlinc() {
    common::expect_box_same_as_kotlinc(ACCEPTED, "ErasedTypeCheckAccepted");
}

/// The accepted checks as declarations, compiled to kotlinc's bytes. A smart-cast operand is tested
/// unnarrowed: kotlinc's implicit cast writes no `checkcast` before an `instanceof`.
const ACCEPTED_DECLARATIONS: &str = "import kotlin.coroutines.Continuation\n\
import kotlin.coroutines.SuspendFunction1\n\
\n\
interface Base<out T>\n\
class Impl<T>(val value: T) : Base<T>\n\
class Pair2<A, B>(val first: A, val second: B)\n\
\n\
fun narrowed(b: Base<String>) = b is Impl<String>\n\
fun starred(x: Any) = x is Impl<*>\n\
fun readable(x: Any) = x is Base<Any?>\n\
fun afterCast(x: Any): Boolean {\n\
    x as Base<String>\n\
    return x is Impl<String>\n\
}\n\
inline fun <reified T> reified(x: Any?) = x is T\n\
fun partial(x: Pair2<String, *>) = x is Pair2<String, *>\n\
\n\
fun suspendAfterCast(f: suspend (Int) -> Int): Boolean {\n\
    @Suppress(\"UNCHECKED_CAST\")\n\
    f as Function2<Int, Continuation<Int>, Any?>\n\
    return f is SuspendFunction1<Int, Any?> && f is Function2<Int, Continuation<Int>, Any?>\n\
}\n";

#[test]
fn a_type_test_the_operand_proves_compiles_to_kotlincs_classes() {
    common::assert_classes_identical_to_kotlinc(
        "ErasedTypeCheckDeclarations",
        ACCEPTED_DECLARATIONS,
        &["Base", "Impl", "Pair2", "ErasedTypeCheckDeclarationsKt"],
    );
}

/// A reflective suspend reference smart cast to its `KFunction2` view keeps its declared
/// `KSuspendFunction1` type, so testing that type with a covariantly wider result is an upcast.
const REFLECTIVE: &str = "import kotlin.coroutines.Continuation\n\
import kotlin.coroutines.SuspendFunction1\n\
import kotlin.reflect.KFunction2\n\
import kotlin.reflect.KSuspendFunction1\n\
\n\
class Counter(val base: Int) {\n\
    suspend fun add(x: Int) = base + x\n\
}\n\
\n\
fun reflective(ref: KSuspendFunction1<Int, Int>): Boolean {\n\
    @Suppress(\"UNCHECKED_CAST\")\n\
    ref as KFunction2<Int, Continuation<Int>, Any?>\n\
    return ref is KSuspendFunction1<Int, Any?> && ref is SuspendFunction1<Int, Any?>\n\
}\n\
\n\
fun box(): String = if (reflective(Counter(1)::add)) \"OK\" else \"FAIL\"\n";

#[test]
fn a_smart_cast_reflective_suspend_reference_keeps_its_declared_type() {
    common::expect_box_same_as_kotlinc(REFLECTIVE, "ErasedTypeCheckReflective");
}

/// The reflective check as a declaration, compiled to kotlinc's bytes: `KSuspendFunction1<A, R>` is
/// carried as `KFunction<R>` in the generic signature, and the smart cast to `KFunction2` writes no
/// `checkcast` before the `instanceof`.
const REFLECTIVE_DECLARATION: &str = "import kotlin.coroutines.Continuation\n\
import kotlin.coroutines.SuspendFunction1\n\
import kotlin.reflect.KFunction2\n\
import kotlin.reflect.KSuspendFunction1\n\
\n\
fun reflective(ref: KSuspendFunction1<Int, Int>): Boolean {\n\
    @Suppress(\"UNCHECKED_CAST\")\n\
    ref as KFunction2<Int, Continuation<Int>, Any?>\n\
    return ref is KSuspendFunction1<Int, Any?> && ref is SuspendFunction1<Int, Any?>\n\
}\n";

#[test]
fn a_smart_cast_reflective_suspend_reference_compiles_to_kotlincs_classes() {
    common::assert_classes_identical_to_kotlinc(
        "ErasedTypeCheckReflectiveDeclaration",
        REFLECTIVE_DECLARATION,
        &["ErasedTypeCheckReflectiveDeclarationKt"],
    );
}

const REJECTED: &str = "import kotlin.coroutines.Continuation\n\
import kotlin.coroutines.SuspendFunction1\n\
\n\
interface Base<out T>\n\
class Impl<T> : Base<T>\n\
class Holder<T>\n\
\n\
fun fromAny(x: Any) = x is Impl<String>\n\
fun wrongArgument(b: Base<String>) = b is Impl<Int>\n\
fun mapValue(x: Any) = x is Map<*, Impl<Int>>\n\
fun <T> parameter(x: Any) = x is T\n\
fun <T> nullableParameter(x: T?) = x is T\n\
fun invariant(x: Holder<out Number>) = x is Holder<Int>\n\
fun suspendWithoutCast(f: suspend (Int) -> Int) = f is Function2<Int, Continuation<Int>, Any?>\n\
fun suspendFromAny(x: Any) = x is SuspendFunction1<Int, Any?>\n\
fun functionFromBase(f: Function<Int>) = f is SuspendFunction1<Int, Int>\n";

#[test]
fn a_type_test_the_operand_does_not_prove_is_rejected_like_kotlinc() {
    let sources = [("Main.kt", REJECTED)];
    common::assert_errors_match_kotlinc(
        &sources,
        &common::language_directives::kotlinc_args(REJECTED),
    );
}

/// A local class captures a generic function's type parameter in its semantic application, but
/// the runtime classifier cannot test that captured argument.
#[test]
fn a_local_class_capturing_a_function_parameter_is_rejected_like_kotlinc() {
    const SRC: &str = "fun <T> erased(x: Any): Boolean {\n\
        \x20   class Local(val value: T)\n\
        \x20   return x is Local\n\
        }\n";
    let sources = [("Main.kt", SRC)];
    common::assert_errors_match_kotlinc(&sources, &common::language_directives::kotlinc_args(SRC));
}

/// A type parameter owned by an actual outer class belongs to the local classifier's class chain;
/// FIR's local-class rule must not reject it as though it came from an enclosing function.
#[test]
fn a_local_class_using_an_outer_class_parameter_is_not_rejected_as_erased() {
    const SRC: &str = "class Outer<T>(private val value: T) {\n\
        \x20   fun check(x: Any): Boolean {\n\
        \x20       class Local(val nested: T)\n\
        \x20       return x is Local\n\
        \x20   }\n\
        }\n\
        fun box(): String = if (!Outer(1).check(\"not local\")) \"OK\" else \"FAIL\"\n";
    common::expect_box_same_as_kotlinc(SRC, "LocalClassOuterTypeParameter");
}
