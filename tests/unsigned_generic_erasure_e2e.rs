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
//! The contract is the backend's, as in `unsigned_classpath_call_e2e`: declining the file is always
//! a legal outcome, emitting a class that does not load and run is not.

use super::common;
use super::common::BackendOutcome;

/// Assert the backend's own contract: whatever krusty EMITS must verify and run.
///
/// STRICTER than the sibling helper in `unsigned_classpath_call_e2e`, which accepts a decline. Every
/// shape in this file EMITS today (all four carriers included — none of them declines), and emitting
/// the right unbox is the whole property under test, so accepting a decline would let a future bail
/// leave a green test with no coverage at all. A shape that legitimately starts declining should be
/// moved to the permissive sibling deliberately, not pass silently here.
fn expect_box_emits_and_verifies(src: &str, stem: &str) {
    let stdlib = common::stdlib_jar();
    expect_box_emits_and_verifies_on(src, stem, std::slice::from_ref(&stdlib));
}

/// [`expect_box_emits_and_verifies`] against an explicit classpath — for a shape whose callee needs a
/// fixture jar no stdlib declaration provides.
fn expect_box_emits_and_verifies_on(src: &str, stem: &str, cp: &[std::path::PathBuf]) {
    let jdk = common::jdk_modules();
    match common::backend_outcome_in_process(src, stem, cp, Some(&jdk)) {
        None => panic!("{stem}: the front end rejected the source; this is a backend test"),
        Some(BackendOutcome::Emitted) => {
            let Some(out) = common::compile_and_run_box(src, stem, cp, Some(&jdk)) else {
                panic!(
                    "{stem}: krusty reported success, but the emitted class does not load and run \
                     (a class that fails verification is strictly worse than declining the file)"
                );
            };
            assert_eq!(out, "OK", "{stem}");
        }
        Some(other) => panic!(
            "{stem}: the backend declined ({other:?}). Declining is a legal outcome in general, but \
             this shape emitted when the unsigned unbox was fixed, and a decline here would mean the \
             test no longer exercises the unbox it exists to pin"
        ),
    }
}

/// `ident(5u).toString()` — the reported shape. The unbox feeds a static carrier helper
/// (`Integer.toUnsignedString`), so the wrong wrapper throws `ClassCastException` immediately.
#[test]
fn unsigned_generic_call_result_unboxes_through_its_own_value_class() {
    expect_box_emits_and_verifies(
        "fun <T> ident(t: T): T = t\n\
         fun box(): String = if (ident(5u).toString() == \"5\") \"OK\" else \"bad\"\n",
        "UIntGenericIdent",
    );
    expect_box_emits_and_verifies(
        "fun <T> ident(t: T): T = t\n\
         fun box(): String = if (ident(5uL).toString() == \"5\") \"OK\" else \"bad\"\n",
        "ULongGenericIdent",
    );
}

/// The narrow carriers. `UByte`/`UShort` live in a sign-extended `byte`/`short`, and their boxes are
/// `kotlin/UByte`/`kotlin/UShort` — a `Byte`/`Short` unbox is wrong for the same reason.
#[test]
fn narrow_unsigned_generic_call_result_unboxes_through_its_own_value_class() {
    expect_box_emits_and_verifies(
        "fun <T> ident(t: T): T = t\n\
         fun box(): String {\n\
        val b: UByte = 5u\n\
        return if (ident(b).toString() == \"5\") \"OK\" else \"bad\"\n\
    }\n",
        "UByteGenericIdent",
    );
    expect_box_emits_and_verifies(
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
    expect_box_emits_and_verifies(
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
    expect_box_emits_and_verifies(
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
fun <T : Comparable<T>> pickLib(a: T, b: T): T = if (a >= b) a else b\n",
        )],
    ) else {
        // kotlinc unavailable in this environment: the fixture cannot be built, and a source-only
        // stand-in would not exercise a CLASSPATH call at all.
        return;
    };
    let cp = [lib, stdlib];
    // The IMPORTED bare name and the FULLY QUALIFIED call are separate lowering sites, each of which
    // carried its own copy of the gate; both must reach the coercion.
    expect_box_emits_and_verifies_on(
        "import lib.pickLib\n\
\n\
fun box(): String {\n\
    val u: UInt = 5u\n\
    return if (pickLib(u, 3u) == u) \"OK\" else \"bad\"\n\
}\n",
        "UIntBoundedClasspathGeneric",
        &cp,
    );
    expect_box_emits_and_verifies_on(
        "fun box(): String {\n\
    val u: UInt = 5u\n\
    return if (lib.pickLib(u, 3u) == u) \"OK\" else \"bad\"\n\
}\n",
        "UIntBoundedClasspathGenericFq",
        &cp,
    );
}

/// The same erasure reached through a CLASSPATH generic (`kotlin.run`, an inline stdlib callable):
/// the erased-result coercion is shared, so the stdlib path must unbox identically.
#[test]
fn unsigned_classpath_generic_call_result_unboxes_through_its_own_value_class() {
    expect_box_emits_and_verifies(
        "fun box(): String = if (run { 5u }.toString() == \"5\") \"OK\" else \"bad\"\n",
        "UIntClasspathGenericRun",
    );
    expect_box_emits_and_verifies(
        "fun box(): String = if (listOf(5u, 7u).first().toString() == \"5\") \"OK\" else \"bad\"\n",
        "UIntClasspathGenericFirst",
    );
}
