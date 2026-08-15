//! A SIBLING nested class referenced by SIMPLE name — `Inner(…)` written inside `Outer` or its
//! `companion object`.
//!
//! Kotlin nested-type scoping puts a nested classifier in scope throughout the enclosing class body,
//! including inside its companion. Three layers each missed a piece, and each was hidden by the one in
//! front of it:
//!
//! - `enclosing_nested_type_name` walked only `this_labels`. A PLAIN companion object has no class `this`
//!   (a companion becomes a first-class type only when it declares a supertype), so from inside one the
//!   walk saw no enclosing class at all and `Inner("1")` was "unresolved function 'Inner'" — while the
//!   same call from an ordinary member, and the qualified `Outer.Inner("1")` from the companion, both
//!   resolved. It now walks `lexical_source_class_names`, which already handled the companion for
//!   inherited-classifier lookup.
//!
//! - The checker's `supports_named` predicate had no clause for this spelling, so once the call resolved,
//!   `Inner(version = "1")` was still rejected as "named arguments are only supported for…" even from an
//!   ordinary member where the positional form worked.
//!
//! - Lowering read the constructor's parameter metadata via `class_decl(&fname)` — the WRITTEN name. A
//!   nested class's decl is keyed by its hoisted name (`Outer.Inner`), so the lookup returned nothing, the
//!   metadata that named arguments and defaults are filled from was empty, and the construction bailed the
//!   whole file at the IR backend. It now keys on the class being constructed.
//!
//! Every case here is checked against real kotlinc output.

use super::common;

fn run(src: &str) -> Option<String> {
    common::compile_and_run_with_stdlib(src, "Main")
}

/// From inside a plain `companion object`, POSITIONALLY. This is the layer that reported "unresolved
/// function" and it needs no named arguments at all — the narrowest statement of the scoping bug.
#[test]
fn a_sibling_nested_class_resolves_by_simple_name_from_a_companion() {
    const SRC: &str = "class Outer {\n\
    \x20   class Inner(val a: String)\n\
    \x20   companion object {\n\
    \x20       fun build(): Inner = Inner(\"1\")\n\
    \x20   }\n\
    }\n\
    fun box(): String = Outer.build().a\n";
    assert_eq!(
        run(SRC).expect("a sibling nested class resolves by simple name from a companion"),
        "1"
    );
}

#[test]
fn a_later_private_inner_class_constructor_resolves_in_an_anonymous_super_call() {
    const SRC: &str = "open class ActionButton(val action: Any)\n\
    class Builder { fun <T> cell(value: T): T = value }\n\
    fun <T> panel(block: Builder.() -> T): T = Builder().block()\n\
    class Outer {\n\
    \x20   fun build(): String = panel {\n\
    \x20       val button = cell(object : ActionButton(FilterAction()) {})\n\
    \x20       button.action.toString()\n\
    \x20   }\n\
    \x20   private inner class FilterAction { override fun toString(): String = \"OK\" }\n\
    }\n\
    fun box(): String = Outer().build()\n";
    // BACKEND STILL BAILS on this shape: checker-clean is asserted, emission is a known
    // gap - upgrade to `expect_true_e2e` when the backend admits it.
    let bail_diags = common::front_end_diagnostics(SRC, &[], None);
    assert!(bail_diags.is_empty(), "{bail_diags:?}");
}

#[test]
fn an_anonymous_sibling_method_preinfers_a_lexically_nested_return() {
    const SRC: &str = "open class Base\n\
    class Outer {\n\
    \x20   fun build() = object : Base() {\n\
    \x20       fun use(): String = make().value\n\
    \x20       fun make() = Inner(\"OK\")\n\
    \x20   }\n\
    \x20   private inner class Inner(val value: String)\n\
    }\n";
    // BACKEND STILL BAILS on this shape: checker-clean is asserted, emission is a known
    // gap - upgrade to `expect_true_e2e` when the backend admits it.
    let bail_diags = common::front_end_diagnostics(SRC, &[], None);
    assert!(bail_diags.is_empty(), "{bail_diags:?}");
}

#[test]
fn deeply_nested_anonymous_objects_keep_the_source_class_classifier_scope() {
    let mut source = String::from(
        "open class Base(value: Any? = null)\n\
         class Outer {\n\
         \x20   fun build() = object : Base() {\n",
    );
    for level in 0..34 {
        source.push_str(&format!("val level{level} = object : Base() {{\n"));
    }
    source.push_str("val leaf = object : Base(Inner()) {}\n");
    for _ in 0..34 {
        source.push_str("}\n");
    }
    source.push_str("}\nprivate class Inner\n}\n");

    let diagnostics = common::front_end_diagnostics(&source, &[], None);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn an_anonymous_synthetic_name_does_not_create_a_lexical_owner() {
    const SRC: &str = "open class Base\n\
    class Anon { class FilterAction }\n\
    class Outer {\n\
    \x20   open class Expected\n\
    \x20   class FilterAction : Expected()\n\
    \x20   fun build() = object : Base() {\n\
    \x20       fun pick(): Outer.Expected = FilterAction()\n\
    \x20   }\n\
    }\n";
    common::expect_true_e2e(
        "an_anonymous_synthetic_name_does_not_create_a_lexical_owner",
        SRC,
        &[],
    );
}

#[test]
fn anonymous_member_signatures_see_the_source_class_classifier_scope() {
    const SRC: &str = "class Outer {\n\
    \x20   private class Inner\n\
    \x20   fun build() = object {\n\
    \x20       fun take(value: Inner): Inner = value\n\
    \x20   }\n\
    }\n";
    common::expect_true_e2e(
        "anonymous_member_signatures_see_the_source_class_classifier_scope",
        SRC,
        &[],
    );
}

#[test]
fn anonymous_supertype_arguments_see_the_source_class_classifier_scope() {
    const SRC: &str = "open class Base<T>\n\
    class Outer {\n\
    \x20   private class Inner\n\
    \x20   fun build() = object : Base<Inner>() {}\n\
    }\n";
    common::expect_true_e2e(
        "anonymous_supertype_arguments_see_the_source_class_classifier_scope",
        SRC,
        &[],
    );
}

/// The same, with NAMED arguments, one of which omits a default — the reported shape. Needs all three
/// layers: the scope walk to resolve it, the predicate to accept the labels, and the ctor metadata to
/// place them and fill `dist`.
#[test]
fn named_arguments_on_a_sibling_nested_ctor_from_a_companion() {
    const OMITTED: &str = "class Outer(val tools: List<Inner>) {\n\
    \x20   data class Inner(val version: String, val dist: String = \"zulu\")\n\
    \x20   companion object {\n\
    \x20       fun build(): Outer = Outer(listOf(Inner(version = \"1\")))\n\
    \x20   }\n\
    }\n\
    fun box(): String = Outer.build().tools[0].dist\n";
    assert_eq!(
        run(OMITTED).expect("named argument omitting a default, from a companion"),
        "zulu"
    );

    const SUPPLIED: &str = "class Outer(val tools: List<Inner>) {\n\
    \x20   data class Inner(val version: String, val dist: String = \"zulu\")\n\
    \x20   companion object {\n\
    \x20       fun build(): Outer = Outer(listOf(Inner(version = \"1\", dist = \"z\")))\n\
    \x20   }\n\
    }\n\
    fun box(): String = Outer.build().tools[0].dist\n";
    assert_eq!(
        run(SUPPLIED).expect("both named arguments supplied, from a companion"),
        "z"
    );
}

/// Named arguments from an ORDINARY member, where the positional form already worked. This isolates the
/// `supports_named` layer from the scoping one: resolution was never the problem here.
#[test]
fn named_arguments_on_a_sibling_nested_ctor_from_an_ordinary_member() {
    const SRC: &str = "class Outer {\n\
    \x20   data class Inner(val version: String, val dist: String = \"zulu\")\n\
    \x20   fun make(): Inner = Inner(version = \"1\", dist = \"z\")\n\
    }\n\
    fun box(): String = Outer().make().dist\n";
    assert_eq!(
        run(SRC).expect("named arguments on a sibling nested ctor from an ordinary member"),
        "z"
    );
}

/// The generic resolution seam must preserve LEXICAL precedence too: the nested `Inner<T>` shadows a
/// same-named top-level class, its explicit `Long` argument supplies constructor context for the integer
/// literal, and the omitted named-call slot uses the nested class's default. This guards against falling
/// back to written-name metadata in lowering, which would select the top-level declaration or no
/// declaration instead of the class identity already selected by the checker.
#[test]
fn a_generic_sibling_nested_ctor_uses_resolved_class_identity() {
    const SRC: &str = "data class Inner(val wrong: String)\n\
    class Outer {\n\
    \x20   data class Inner<T>(val value: T, val marker: String = \"OK\")\n\
    \x20   companion object {\n\
    \x20       fun build(): Inner<Long> = Inner<Long>(value = 1)\n\
    \x20   }\n\
    }\n\
    fun box(): String {\n\
    \x20   val built = Outer.build()\n\
    \x20   return if (built.value == 1L) built.marker else \"FAIL\"\n\
    }\n";
    assert_eq!(
        run(SRC).expect("generic sibling nested constructor uses its resolved class identity"),
        "OK"
    );
}

/// Replacing the old "any source class" admission branch with semantic classifier resolution must
/// not narrow existing SOURCE behavior to primary constructors only. Source candidate selection owns
/// every secondary constructor's parameter names, so a class without a primary constructor still maps
/// a reordered named call before lowering.
#[test]
fn semantic_named_call_admission_preserves_secondary_constructors() {
    const SRC: &str = "class Built {\n\
    \x20   val text: String\n\
    \x20   constructor(first: Int, second: String) { text = first.toString() + second }\n\
    }\n\
    fun box(): String = Built(second = \"K\", first = 1).text\n";
    assert_eq!(
        run(SRC).expect("source secondary constructor keeps semantic named-argument mapping"),
        "1K"
    );
}

/// A named argument that REORDERS, so source order cannot pass for parameter order.
#[test]
fn a_reordering_named_argument_on_a_sibling_nested_ctor() {
    const SRC: &str = "class Outer {\n\
    \x20   data class Inner(val a: String, val b: String)\n\
    \x20   companion object {\n\
    \x20       fun build(): Inner = Inner(b = \"Y\", a = \"X\")\n\
    \x20   }\n\
    }\n\
    fun box(): String = Outer.build().a + Outer.build().b\n";
    assert_eq!(
        run(SRC).expect("reordered named arguments on a sibling nested ctor"),
        "XY"
    );
}

/// The spellings that ALREADY worked, kept as regression guards: the qualified form from a companion, and
/// the positional form from an ordinary member. Both were the controls that localized each layer.
#[test]
fn the_qualified_and_positional_spellings_still_work() {
    const QUALIFIED: &str = "class Outer {\n\
    \x20   data class Inner(val a: String, val b: String = \"d\")\n\
    \x20   companion object {\n\
    \x20       fun build(): Inner = Outer.Inner(\"1\", \"z\")\n\
    \x20   }\n\
    }\n\
    fun box(): String = Outer.build().b\n";
    assert_eq!(run(QUALIFIED).expect("qualified from a companion"), "z");

    const POSITIONAL: &str = "class Outer {\n\
    \x20   data class Inner(val a: String, val b: String = \"d\")\n\
    \x20   fun make(): Inner = Inner(\"1\", \"z\")\n\
    }\n\
    fun box(): String = Outer().make().b\n";
    assert_eq!(
        run(POSITIONAL).expect("positional from an ordinary member"),
        "z"
    );
}
