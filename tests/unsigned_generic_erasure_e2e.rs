//! An unsigned value that comes back out of an ERASED GENERIC call result.
//!
//! `fun <T> ident(t: T): T` erases to `(Object)Object`, so `ident(5u)` pushes a boxed `kotlin/UInt`
//! and every use site has to unbox it back to the carrier. The unbox for an unsigned box is
//! `checkcast kotlin/UInt; invokevirtual kotlin/UInt."unbox-impl":()I` — NOT the boxed-primitive
//! `checkcast java/lang/Integer; intValue`, which throws `ClassCastException` at run time because
//! `kotlin/UInt` is not an `Integer`. krusty emitted the `Integer` round trip and reported success.
//!
//! The bug lived in the erased-call-result coercion, which routed any scalar-carrier static type
//! through the plain primitive unbox. The value-read coercion (`coerce_to_static`) had the unsigned
//! branch already, which is why the map-element path (`mapOf("k" to 5u)["k"]!!`) was correct while
//! a generic call result was not.
//!
//! A BOUNDED type parameter erases to its bound rather than to `Object` (`<T : Comparable<T>>` →
//! `Comparable`), and kotlinc unboxes there identically; both erasures are pinned here.
//!
//! These are strict regressions: every source shape must emit, verify, run, and return `OK`. A backend
//! decline is therefore a failure here rather than an accepted unsupported outcome; otherwise a new
//! bail could make the suite green without exercising the adapter it exists to pin.

use super::common;

/// Strict explicit-classpath counterpart of `common::expect_box_ok_with_stdlib`. The shared helper
/// owns compilation-failure diagnostics and distinguishes an unavailable result from a passing skip;
/// this wrapper only supplies the fixture classpath and asserts the semantic result.
fn expect_box_ok_on(src: &str, stem: &str, cp: &[std::path::PathBuf]) {
    let jdk = common::jdk_modules();
    assert_eq!(
        common::expect_box_run(src, stem, cp, Some(&jdk)),
        "OK",
        "{stem}"
    );
}

/// `ident(5u).toString()` — the reported shape. The unbox feeds a static carrier helper
/// (`Integer.toUnsignedString`), so the wrong wrapper throws `ClassCastException` immediately.
#[test]
fn unsigned_generic_call_result_unboxes_through_its_own_value_class() {
    common::expect_box_ok_with_stdlib(
        "fun <T> ident(t: T): T = t\n\
         fun box(): String = if (ident(5u).toString() == \"5\") \"OK\" else \"bad\"\n",
        "UIntGenericIdent",
    );
    common::expect_box_ok_with_stdlib(
        "fun <T> ident(t: T): T = t\n\
         fun box(): String = if (ident(5uL).toString() == \"5\") \"OK\" else \"bad\"\n",
        "ULongGenericIdent",
    );
}

/// The narrow carriers. `UByte`/`UShort` live in a sign-extended `byte`/`short`, and their boxes are
/// `kotlin/UByte`/`kotlin/UShort` — a `Byte`/`Short` unbox is wrong for the same reason.
#[test]
fn narrow_unsigned_generic_call_result_unboxes_through_its_own_value_class() {
    common::expect_box_ok_with_stdlib(
        "fun <T> ident(t: T): T = t\n\
         fun box(): String {\n\
        val b: UByte = 5u\n\
        return if (ident(b).toString() == \"5\") \"OK\" else \"bad\"\n\
    }\n",
        "UByteGenericIdent",
    );
    common::expect_box_ok_with_stdlib(
        "fun <T> ident(t: T): T = t\n\
         fun box(): String {\n\
        val s: UShort = 5u\n\
        return if (ident(s).toString() == \"5\") \"OK\" else \"bad\"\n\
    }\n",
        "UShortGenericIdent",
    );
}

/// A BOUNDED type parameter erases to its bound (`Comparable`), not to `Object`. kotlinc emits the
/// same `checkcast kotlin/UInt; unbox-impl` after the call.
#[test]
fn unsigned_bounded_generic_call_result_unboxes_through_its_own_value_class() {
    common::expect_box_ok_with_stdlib(
        "fun <T : Comparable<T>> pick(a: T, b: T): T = if (a >= b) a else b\n\
         fun box(): String {\n\
        val u: UInt = 5u\n\
        return if (pick(u, 3u) == u) \"OK\" else \"bad\"\n\
    }\n",
        "UIntBoundedGeneric",
    );
}

/// The generic result used as a RECEIVER of a member call on the value class itself. The unbox and
/// the re-box have to agree: `equals` takes the boxed receiver, so a wrong intermediate wrapper
/// surfaces as the `Integer` round trip the reported shape showed.
#[test]
fn unsigned_generic_call_result_as_value_class_receiver() {
    common::expect_box_ok_with_stdlib(
        "fun <T> ident(t: T): T = t\n\
         fun box(): String {\n\
        val s: Any = \"x\"\n\
        return if (!ident(5u).equals(s)) \"OK\" else \"bad\"\n\
    }\n",
        "UIntGenericEquals",
    );
}

/// A CLASSPATH call whose type parameter is BOUNDED erases to the bound (`Comparable`), which is not
/// the erased top — so the library call site decides on its own whether the substituted result needs
/// a coercion at all. That gate excluded unsigned deliberately, because the coercion it reached for
/// emitted the wrong (boxed-primitive) unbox; with the unbox corrected the exclusion would only leave
/// a boxed `kotlin/UInt` sitting where the carrier belongs.
///
/// Needs a fixture jar: every stdlib `<T : Comparable<T>>` helper has an unsigned specialization
/// (`maxOf(UShort, UShort)` selects `maxOf-5PvTz6A:(SS)S`), so no stdlib call reaches this erasure.
#[test]
fn unsigned_bounded_classpath_generic_call_result_unboxes_through_its_own_value_class() {
    let stdlib = common::stdlib_jar();
    let Some(lib) = common::compile_libs(
        "UBoundedGenericLib",
        &[(
            "Lib.kt",
            "package lib\n\
fun <T : Comparable<T>> pickLib(a: T, b: T): T = if (a >= b) a else b\n\
class Cell<T>(var value: T)\n",
        )],
    ) else {
        // kotlinc unavailable in this environment: the fixture cannot be built, and a source-only
        // stand-in would not exercise a CLASSPATH call at all.
        return;
    };
    let cp = [lib, stdlib];
    // The IMPORTED bare name and the FULLY QUALIFIED call are separate lowering sites, each of which
    // carried its own copy of the gate; both must reach the coercion.
    expect_box_ok_on(
        "import lib.pickLib\n\
\n\
fun box(): String {\n\
    val u: UInt = 5u\n\
    return if (pickLib(u, 3u) == u) \"OK\" else \"bad\"\n\
}\n",
        "UIntBoundedClasspathGeneric",
        &cp,
    );
    expect_box_ok_on(
        "fun box(): String {\n\
    val u: UInt = 5u\n\
    return if (lib.pickLib(u, 3u) == u) \"OK\" else \"bad\"\n\
}\n",
        "UIntBoundedClasspathGenericFq",
        &cp,
    );
    // A mutable generic property exercises both directions of the same bridge. The setter must box
    // the primitive carrier as its semantic unsigned wrapper before storing it in `Object`, and the
    // getter must select the matching unsigned unbox adapter after reading that erased slot.
    expect_box_ok_on(
        "import lib.Cell\n\
\n\
fun box(): String {\n\
    val cell = Cell(1u)\n\
    cell.value = 5u\n\
    return if (cell.value.toString() == \"5\") \"OK\" else \"bad\"\n\
}\n",
        "UnsignedGenericPropertyRoundTrip",
        &cp,
    );
}

/// The same erasure reached through a CLASSPATH generic (`kotlin.run`, an inline stdlib callable):
/// the erased-result coercion is shared, so the stdlib path must unbox identically.
#[test]
fn unsigned_classpath_generic_call_result_unboxes_through_its_own_value_class() {
    common::expect_box_ok_with_stdlib(
        "fun box(): String = if (run { 5u }.toString() == \"5\") \"OK\" else \"bad\"\n",
        "UIntClasspathGenericRun",
    );
    common::expect_box_ok_with_stdlib(
        "fun box(): String = if (listOf(5u, 7u).first().toString() == \"5\") \"OK\" else \"bad\"\n",
        "UIntClasspathGenericFirst",
    );
}

/// Generic property reads cross the same erased-reference boundary as generic function results.
/// `Pair<A, B>.first` is declared as `A` and therefore returns `Object`; after substituting
/// `A = UInt`, the bridge must select the unsigned inline-class adapter from the semantic result
/// type before mapping that type to its JVM carrier. This regression deliberately uses only a
/// standard generic property so the rule cannot depend on a fixture, owner, or accessor spelling.
#[test]
fn unsigned_generic_property_result_uses_the_semantic_unbox_adapter() {
    common::expect_box_ok_with_stdlib(
        "fun box(): String = if (Pair(5u, 1).first.toString() == \"5\") \"OK\" else \"bad\"\n",
        "UnsignedGenericPropertyResult",
    );
}

/// An inlined higher-order call still crosses the erased `FunctionN` result boundary: its lambda
/// body produces the primitive carrier, while `invoke` promises `Object`. The adapter chosen for
/// that boundary must retain the lambda's semantic unsigned type instead of boxing the carrier as
/// its signed JVM wrapper. The following generic read then exercises the matching unbox direction.
#[test]
fn unsigned_inline_lambda_result_uses_the_semantic_box_adapter() {
    common::expect_box_ok_with_stdlib(
        "fun box(): String = if (listOf(5u).map { it }.first().toString() == \"5\") \"OK\" else \"bad\"\n",
        "UnsignedInlineLambdaResult",
    );
}

/// Callable and property references are alternate lambda operands for the same inline-splice host.
/// Their generated `FunctionN` adapters must obey the identical semantic-wrapper rule instead of
/// deriving a signed wrapper from the referenced declaration's physical carrier.
#[test]
fn unsigned_inline_reference_adapters_preserve_semantic_identity() {
    common::expect_box_ok_with_stdlib(
        "fun keep(value: UInt): UInt = value\n\
         fun box(): String = if (listOf(5u).map(::keep).first().toString() == \"5\") \"OK\" else \"bad\"\n",
        "UnsignedInlineFunctionReference",
    );
    common::expect_box_ok_with_stdlib(
        "fun box(): String = if (listOf(Pair(5u, 1)).map(Pair<UInt, Int>::first).first().toString() == \"5\") \"OK\" else \"bad\"\n",
        "UnsignedInlinePropertyReference",
    );
}

/// A real `FunctionN` closure has the same boundary even when no library body is spliced. Its
/// implementation method uses primitive carriers, but the erased `invoke(Object): Object` contract
/// carries boxed Kotlin values. Both the instantiated parameter and return descriptors therefore
/// have to derive their adapters from `UInt`, not from its later JVM `int` representation.
#[test]
fn unsigned_function_value_preserves_semantic_adapters() {
    common::expect_box_ok_with_stdlib(
        "fun box(): String {\n\
             val identity: (UInt) -> UInt = { value -> value }\n\
             return if (identity(5u).toString() == \"5\") \"OK\" else \"bad\"\n\
         }\n",
        "UnsignedFunctionValue",
    );
    common::expect_box_ok_with_stdlib(
        "fun keep(value: UInt): UInt = value\n\
         fun box(): String {\n\
             val identity: (UInt) -> UInt = ::keep\n\
             return if (identity(5u).toString() == \"5\") \"OK\" else \"bad\"\n\
         }\n",
        "UnsignedFunctionReferenceValue",
    );
    common::expect_box_ok_with_stdlib(
        "fun box(): String {\n\
             val read: (Pair<UInt, Int>) -> UInt = Pair<UInt, Int>::first\n\
             return if (read(Pair(5u, 1)).toString() == \"5\") \"OK\" else \"bad\"\n\
         }\n",
        "UnsignedPropertyReferenceValue",
    );
}

/// Semantic wrapper identity refines a SCALAR carrier; it must not turn an ALREADY-BOXED reference
/// into a second boxing request. Nullable unsigned arguments arrive at `FunctionN.invoke` as
/// `kotlin/UInt` already, while a safe-call receiver can arrive as `java/lang/Integer`. Reapplying
/// `UInt.box-impl` or `Integer.valueOf` to either reference produces invalid bytecode, so these two
/// shapes pin the carrier guard independently of which wrapper class is involved.
#[test]
fn semantic_adapter_does_not_rebox_an_existing_reference_carrier() {
    common::expect_box_ok_with_stdlib(
        "val nullableUInt: UInt? = 2u\n\
         fun box(): String {\n\
             val same: (UInt?) -> Boolean = { value -> nullableUInt == value }\n\
             return if (same(2u)) \"OK\" else \"bad\"\n\
         }\n",
        "NullableUnsignedFunctionValue",
    );
    common::expect_box_ok_with_stdlib(
        "fun apply(value: Int?, operation: Int.(Int) -> Int): Int? = value?.operation(1)\n\
         fun box(): String = if (apply(1) { this + it + 2 } == 4) \"OK\" else \"bad\"\n",
        "NullableSignedFunctionReceiver",
    );
}
