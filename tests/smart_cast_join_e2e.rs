//! After an `if`, a `var` reads as the common supertype of the types it has on each edge that
//! completes normally (kotlinc's `LogicSystem.or`): `if (x == null) x = ArrayList()` leaves a
//! `Collection<String>?` local readable as `Collection<String>`. An edge on which the local may
//! still be null keeps the declared nullable type.
use super::common;

const ACCEPTED: &str = "package flow\n\
    \n\
    open class Base\n\
    class Sub : Base()\n\
    \n\
    interface Source<out T> { fun get(): T }\n\
    interface Tagged { fun tag(): Int }\n\
    interface FacetA { fun a(): Int }\n\
    interface FacetB { fun b(): Int }\n\
    class First : Source<String>, Tagged { override fun get() = \"a\"; override fun tag() = 1 }\n\
    class Second : Source<String>, Tagged { override fun get() = \"bb\"; override fun tag() = 2 }\n\
    class Third : Source<StringBuilder> { override fun get() = StringBuilder(\"ccc\") }\n\
    class BothFacets : FacetA, FacetB { override fun a() = 7; override fun b() = 8 }\n\
    class JustFacetA : FacetA { override fun a() = 9 }\n\
    \n\
    fun collection(text: String): Int {\n\
    \x20   var before: Collection<String>? = null\n\
    \x20   if (before == null) {\n\
    \x20       before = ArrayList<String>()\n\
    \x20   }\n\
    \x20   return before.size + text.length\n\
    }\n\
    \n\
    fun subclass(): Int {\n\
    \x20   var x: Base? = null\n\
    \x20   if (x == null) x = Sub()\n\
    \x20   return x.hashCode()\n\
    }\n\
    \n\
    fun inLoop(text: String): Int {\n\
    \x20   var before: MutableSet<String>? = null\n\
    \x20   for (s in text.split(\",\")) {\n\
    \x20       if (before == null) {\n\
    \x20           before = LinkedHashSet(2)\n\
    \x20       }\n\
    \x20       before.add(s)\n\
    \x20   }\n\
    \x20   return before?.size ?: 0\n\
    }\n\
    \n\
    fun intersectionThenVariance(which: Int): Int {\n\
    \x20   var source: Source<CharSequence>? = null\n\
    \x20   when (which) {\n\
    \x20       0 -> source = First()\n\
    \x20       1 -> source = Second()\n\
    \x20       else -> source = Third()\n\
    \x20   }\n\
    \x20   return source.get().length\n\
    }\n\
    \n\
    fun threeEdgeFacetOutlivesAnIntermediateIntersection(which: Int): Int {\n\
    \x20   var source: Any? = null\n\
    \x20   when (which) {\n\
    \x20       0 -> source = First()\n\
    \x20       1 -> source = Second()\n\
    \x20       else -> source = Third()\n\
    \x20   }\n\
    \x20   return source.get().length\n\
    }\n\
    \n\
    fun finalIntersectionRetainsEveryFacet(first: Boolean): Int {\n\
    \x20   var source: Source<String> = First()\n\
    \x20   if (first) source = First() else source = Second()\n\
    \x20   return source.tag()\n\
    }\n\
    \n\
    fun edgeCarriesEveryIntersectionFacet(replace: Boolean): Int {\n\
    \x20   var value: Any = BothFacets()\n\
    \x20   if (value !is FacetA || value !is FacetB) return -1\n\
    \x20   if (replace) value = JustFacetA()\n\
    \x20   return value.a()\n\
    }\n\
    \n\
    fun box(): String =\n\
    \x20   if (collection(\"ab\") == 2 && subclass() != 0 && inLoop(\"a,b,a\") == 2 &&\n\
    \x20       intersectionThenVariance(0) == 1 && intersectionThenVariance(1) == 2 &&\n\
    \x20       intersectionThenVariance(2) == 3 &&\n\
    \x20       threeEdgeFacetOutlivesAnIntermediateIntersection(0) == 1 &&\n\
    \x20       threeEdgeFacetOutlivesAnIntermediateIntersection(1) == 2 &&\n\
    \x20       threeEdgeFacetOutlivesAnIntermediateIntersection(2) == 3 &&\n\
    \x20       finalIntersectionRetainsEveryFacet(true) == 1 &&\n\
    \x20       finalIntersectionRetainsEveryFacet(false) == 2 &&\n\
    \x20       edgeCarriesEveryIntersectionFacet(false) == 7 &&\n\
    \x20       edgeCarriesEveryIntersectionFacet(true) == 9) \"OK\" else \"FAIL\"\n";

const REJECTED: &str = "fun maybeNull(c: Boolean): Int {\n\
    \x20   var x: String? = null\n\
    \x20   if (c) x = \"a\" else x = null\n\
    \x20   return x.length\n\
    }\n\
    \n\
    fun implicitElse(c: Boolean): Int {\n\
    \x20   var x: CharSequence? = null\n\
    \x20   if (c) {\n\
    \x20       x = \"a\"\n\
    \x20   }\n\
    \x20   return x.length\n\
    }\n";

const ALL_NULL_REJECTED: &str = "fun allNull(c: Boolean): Int {\n\
    \x20   var x: String? = \"a\"\n\
    \x20   if (c) x = null else x = null\n\
    \x20   return x.length\n\
    }\n";

#[test]
fn a_var_reads_as_the_join_of_its_edge_types() {
    common::byte_diff_against_kotlinc_cp(
        "SmartCastJoin",
        ACCEPTED,
        "flow/SmartCastJoinKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("flow/SmartCastJoinKt differs from kotlinc: {diff}"));
    assert_eq!(
        common::compile_and_run_files_with_stdlib(&[("SmartCastJoin.kt", ACCEPTED)]).as_deref(),
        Some("OK")
    );
}

#[test]
fn a_possibly_null_edge_keeps_the_declared_type() {
    let result = common::compiler_diagnostics(&[("Main.kt", REJECTED)], &[common::stdlib_jar()]);
    common::expect_identical_rejection(&result, "nullable smart-cast join");
    assert_eq!(
        common::compiler_errors(&result.reference_stderr).len(),
        2,
        "kotlinc: {}",
        result.reference_stderr
    );
}

#[test]
fn unanimous_null_edges_keep_nothing_nullable() {
    let result =
        common::compiler_diagnostics(&[("Main.kt", ALL_NULL_REJECTED)], &[common::stdlib_jar()]);
    common::expect_identical_rejection(&result, "unanimous Nothing? smart-cast join");
    assert_eq!(
        common::compiler_errors(&result.reference_stderr).len(),
        1,
        "kotlinc: {}",
        result.reference_stderr
    );
}
