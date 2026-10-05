//! Constructor overload specificity compares the parameters the SUPPLIED arguments map to, in
//! argument order, exactly like a function call. Kotlin's most-specific check (FIR
//! `ConeOverloadConflictResolver`) builds each candidate's flat signature from its argument mapping,
//! so a parameter that only receives its default value takes no part in the comparison.
//!
//! Constructor selection compared the full declared parameter lists against the argument kinds by
//! position instead. A call that omits a defaulted parameter, names its arguments, or spreads a
//! vararg then had a parameter position with no argument, which no candidate could win: two
//! applicable constructors such as `(x: Int = 1, y: Int = 2)` and `(x: Long = 4L, y: Long = 5L)`
//! were reported as "none of the following candidates is applicable" for `C(x = 100)`, and as an
//! ambiguous `this(...)` delegation, although kotlinc 2.4.20 selects the `Int` constructor. Every
//! fixture below runs under both compilers, must agree, and must emit kotlinc's class files byte for
//! byte.

use super::common;

/// Run `source` under both compilers, then require each of `classes` to be kotlinc's exact bytes.
fn same_as_kotlinc(source: &str, stem: &str, classes: &[&str]) {
    common::expect_box_same_as_kotlinc(source, stem);
    common::assert_classes_identical_to_kotlinc(stem, source, classes);
}

const NUMERIC_PAIR: &str = "class C(val x: Int = 1, val y: Int = 2) {\n\
    var via = \"primary\"\n\
    constructor(x: Long = 4L, y: Long = 5L) : this(x.toInt(), y.toInt()) { via = \"long\" }\n\
}\n";

#[test]
fn a_call_omitting_a_default_compares_only_the_supplied_arguments() {
    let source = format!(
        "{NUMERIC_PAIR}\
fun show(c: C) = \"${{c.via}}(${{c.x}},${{c.y}})\"\n\
fun box(): String {{\n\
    val r = listOf(show(C(100)), show(C(x = 100)), show(C(y = 100)), show(C(100L)), show(C(y = 7L)))\n\
    val expected = listOf(\"primary(100,2)\", \"primary(100,2)\", \"primary(1,100)\", \"long(100,5)\", \"long(4,7)\")\n\
    return if (r == expected) \"OK\" else r.toString()\n\
}}\n"
    );
    same_as_kotlinc(
        &source,
        "CtorOmittedDefaultSpecificity",
        &["C", "CtorOmittedDefaultSpecificityKt"],
    );
}

#[test]
fn a_this_delegation_omitting_a_default_selects_like_a_call() {
    let source = "class C(val x: Int = 1, val y: Int = 2) {\n\
    var via = \"primary\"\n\
    constructor(x: Long = 4L, y: Long = 5L) : this(x.toInt(), y.toInt()) { via = \"long\" }\n\
    constructor() : this(8) { via = \"empty:\" + via }\n\
    constructor(s: String) : this(y = 7) { via = s + \":\" + via }\n\
}\n\
fun show(c: C) = \"${c.via}(${c.x},${c.y})\"\n\
fun box(): String {\n\
    val r = listOf(show(C()), show(C(\"named\")))\n\
    val expected = listOf(\"empty:primary(8,2)\", \"named:primary(1,7)\")\n\
    return if (r == expected) \"OK\" else r.toString()\n\
}\n";
    same_as_kotlinc(
        source,
        "CtorDelegationOmittedDefault",
        &["C", "CtorDelegationOmittedDefaultKt"],
    );
}

#[test]
fn a_qualified_nested_constructor_call_uses_the_same_comparison() {
    let source = "class Outer {\n\
    class C(val x: Int = 1, val y: Int = 2) {\n\
        var via = \"primary\"\n\
        constructor(x: Long = 4L, y: Long = 5L) : this(x.toInt(), y.toInt()) { via = \"long\" }\n\
    }\n\
}\n\
fun box(): String {\n\
    val c = Outer.C(x = 100)\n\
    val r = \"${c.via}(${c.x},${c.y})\"\n\
    return if (r == \"primary(100,2)\") \"OK\" else r\n\
}\n";
    same_as_kotlinc(
        source,
        "QualifiedCtorOmittedDefault",
        &["Outer", "Outer$C", "QualifiedCtorOmittedDefaultKt"],
    );
}

/// Each vararg element maps to the vararg parameter's element type, so a call with several elements
/// compares `Int` with `Long` once per element instead of comparing the array parameters by position.
#[test]
fn vararg_elements_compare_against_the_element_type() {
    let source = "class V(vararg val xs: Int) {\n\
    var via = \"int\"\n\
    constructor(vararg xs: Long) : this(xs.size) { via = \"long\" }\n\
}\n\
fun box(): String {\n\
    val r = listOf(V(1, 2).via, V(1L, 2L).via, V(*intArrayOf(3)).via)\n\
    return if (r == listOf(\"int\", \"long\", \"int\")) \"OK\" else r.toString()\n\
}\n";
    same_as_kotlinc(
        source,
        "VarargCtorSpecificity",
        &["V", "VarargCtorSpecificityKt"],
    );
}

/// A primary constructor whose every parameter has a default gets a JVM `<init>()` convenience
/// overload only when the class declares no constructor without value parameters; a declared
/// `constructor()` already owns that descriptor (kotlinc's `JvmDefaultConstructorLowering`).
#[test]
fn a_declared_no_argument_constructor_replaces_the_all_defaults_overload() {
    let source = "class C(val x: Int = 1) {\n\
    constructor() : this(5)\n\
}\n\
fun box(): String {\n\
    val c = C::class.java.getConstructor().newInstance()\n\
    return if (C().x == 5 && c.x == 5) \"OK\" else \"${C().x} ${c.x}\"\n\
}\n";
    common::expect_box_same_as_kotlinc(source, "DeclaredNoArgumentConstructor");
    // The byte comparison compiles against the Kotlin standard library alone, without the JDK the
    // `box` reflection needs, so it compares the class declaration on its own.
    common::assert_classes_identical_to_kotlinc(
        "DeclaredNoArgumentConstructorClass",
        "class C(val x: Int = 1) {\n    constructor() : this(5)\n}\n",
        &["C"],
    );
}

/// A kotlinc-compiled dependency's constructors enter the same selection: its `@Metadata` supplies
/// the defaults, and the specificity comparison again sees only the supplied arguments.
#[test]
fn a_dependency_constructor_omitting_a_default_selects_like_a_module_one() {
    const LIB: &str = "package dep\n\
class C(val x: Int = 1, val y: Int = 2) {\n\
    var via = \"primary\"\n\
    constructor(x: Long = 4L, y: Long = 5L) : this(x.toInt(), y.toInt()) { via = \"long\" }\n\
}\n";
    let main = "import dep.C\n\
fun show(c: C) = \"${c.via}(${c.x},${c.y})\"\n\
fun box(): String {\n\
    val r = listOf(show(C(x = 100)), show(C(y = 100L)))\n\
    return if (r == listOf(\"primary(100,2)\", \"long(4,100)\")) \"OK\" else r.toString()\n\
}\n";
    assert_eq!(
        common::expect_box_run_against_kotlinc(LIB, main).as_deref(),
        Some("OK")
    );
    let library = common::kotlinc_library(LIB).expect("reference kotlinc is provisioned");
    common::assert_classes_identical_to_kotlinc_against(
        "DependencyCtorOmittedDefault",
        main,
        &["DependencyCtorOmittedDefaultKt"],
        &[library],
    );
}
