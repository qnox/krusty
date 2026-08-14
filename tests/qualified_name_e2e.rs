//! Fully-qualified name references, in every position Kotlin admits one.
//!
//! Kotlin lets a declaration be named by its full package path inline, with no import, wherever a
//! simple name would do: construction, companion/static member access, `object` member access, type
//! annotations (including type arguments and supertypes), class literals, `is`/`as` targets, and
//! package-qualified top-level callables. Each case is exercised against BOTH origins — a classifier
//! declared in a sibling source file of the same module, and one read back from the classpath — so
//! the qualified spelling ends at the same resolved identity regardless of where the declaration came
//! from. Every expectation here is the reference compiler's verdict (`docs/SPEC.md`,
//! "Fully-qualified name references").

use super::common;

/// The declarations every case below names by its full path. Compiled either into the same module as
/// the `box()` file or into a classpath directory, depending on the helper used.
const LIB: &str = "package plib\n\
    \n\
    class Cls(val v: String = \"OK\") {\n\
    \x20   fun method(): String = \"m\"\n\
    \x20   class Nested(val n: String = \"N\")\n\
    \x20   object NestObj { val no: String = \"NO\"; fun nf(): String = \"NF\" }\n\
    \x20   companion object {\n\
    \x20       const val CONST: String = \"C\"\n\
    \x20       val COMP: String = \"K\"\n\
    \x20       var MUT: String = \"M\"\n\
    \x20       fun cfn(): String = \"CF\"\n\
    \x20   }\n\
    }\n\
    \n\
    object Obj {\n\
    \x20   val member: String = \"OM\"\n\
    \x20   var mvar: String = \"MV\"\n\
    \x20   fun fn(): String = \"OF\"\n\
    \x20   class ONested(val x: String = \"ON\")\n\
    }\n\
    \n\
    interface Iface\n\
    open class Base(val b: String = \"B\")\n\
    enum class E { A, B }\n\
    \n\
    fun topLevelFun(): String = \"TF\"\n\
    val topLevelProp: String = \"TP\"\n\
    var topLevelVar: String = \"TV\"\n\
    const val TOP_CONST: String = \"TC\"\n\
    \n\
    typealias Alias = Cls\n";

/// Run `main` with the library declared in a SIBLING SOURCE FILE of the same module.
fn module_box(stem: &str, main: &str) {
    common::expect_box_ok_files_with_stdlib(&[("Main.kt", main), ("Lib.kt", LIB)], stem);
}

/// Run `main` with the library read back from the CLASSPATH.
fn classpath_box(tag: &str, main: &str) {
    common::expect_box_ok_against(tag, LIB, main);
}

/// Reference-compiled dependency variant: these cases consume kotlinc-emitted metadata
/// shapes krusty does not produce yet (see `common::compile_lib_ref`).
fn classpath_box_ref(tag: &str, main: &str) {
    common::expect_box_ok_against(tag, LIB, main);
}

/// Both origins for one qualified-name spelling.
fn both(tag: &str, main: &str) {
    module_box(tag, main);
    classpath_box(tag, main);
}

/// Reference-compiled dependency variant: these cases consume kotlinc-emitted metadata
/// shapes krusty does not produce yet (see `common::compile_lib_ref`).
fn both_ref(tag: &str, main: &str) {
    module_box(tag, main);
    classpath_box_ref(tag, main);
}

#[test]
fn implicit_receiver_value_root_wins_over_same_named_package() {
    common::expect_box_ok_with_stdlib(
        "class JavaValue { val io: String = \"OK\" }\n\
         class Host { val java = JavaValue(); fun result(): String = java.io }\n\
         fun box(): String = Host().result()\n",
        "ImplicitValueRootBeforePackage",
    );
}

// ---------------------------------------------------------------- construction

#[test]
fn qualified_constructor_call() {
    both("fqn_ctor", "fun box(): String = plib.Cls(\"OK\").v\n");
}

#[test]
fn qualified_constructor_call_uses_defaults() {
    both("fqn_ctor_default", "fun box(): String = plib.Cls().v\n");
}

#[test]
fn qualified_nested_constructor_call() {
    both(
        "fqn_nested_ctor",
        "fun box(): String = plib.Cls.Nested(\"OK\").n\n",
    );
}

#[test]
fn qualified_constructor_under_object() {
    both(
        "fqn_obj_nested_ctor",
        "fun box(): String = plib.Obj.ONested(\"OK\").x\n",
    );
}

/// Construction through a fully-qualified `typealias`. The module side resolves the alias by its
/// qualified name (`SymbolTable::source_alias_fqns`); the classpath side follows the facade's alias
/// table. Both ends at the alias TARGET, which is the identity the checker records — lowering must
/// follow the same edge or it drops the construction as unresolved.
#[test]
fn qualified_typealias_construction() {
    both_ref(
        "fqn_typealias_ctor",
        "fun box(): String = plib.Alias(\"OK\").v\n",
    );
}

// ------------------------------------------------- companion / static members

#[test]
fn qualified_companion_const_read() {
    both(
        "fqn_comp_const",
        "fun box(): String = if (plib.Cls.CONST == \"C\") \"OK\" else \"no\"\n",
    );
}

#[test]
fn qualified_companion_val_read() {
    both(
        "fqn_comp_val",
        "fun box(): String = if (plib.Cls.COMP == \"K\") \"OK\" else \"no\"\n",
    );
}

#[test]
fn qualified_companion_var_read() {
    both(
        "fqn_comp_var_read",
        "fun box(): String = if (plib.Cls.MUT == \"M\") \"OK\" else \"no\"\n",
    );
}

/// A companion `var` written through a qualified path. Cross-file the backing field is private to
/// its owner, so the write goes through the companion's SETTER — which is what kotlinc emits
/// (`getstatic Cls.Companion; invokevirtual Cls$Companion.setMUT`).
#[test]
fn qualified_companion_var_write() {
    both_ref(
        "fqn_comp_var_write",
        "fun box(): String {\n\
         \x20 plib.Cls.MUT = \"OK\"\n\
         \x20 return plib.Cls.MUT\n\
         }\n",
    );
}

#[test]
fn qualified_companion_function_call() {
    both(
        "fqn_comp_fun",
        "fun box(): String = if (plib.Cls.cfn() == \"CF\") \"OK\" else \"no\"\n",
    );
}

/// `pkg.Cls.Companion` — the companion singleton named EXPLICITLY. Every companion is emitted as
/// an `Outer$Companion` class in an `Outer.Companion` static field, but only one declaring a
/// supertype gets a registered signature, so the read is decided from the OWNER.
#[test]
fn qualified_companion_named_explicitly() {
    both(
        "fqn_comp_named",
        "fun box(): String = if (plib.Cls.Companion.cfn() == \"CF\") \"OK\" else \"no\"\n",
    );
}

// ------------------------------------------------------------- object members

#[test]
fn qualified_object_property_read() {
    both(
        "fqn_obj_member",
        "fun box(): String = if (plib.Obj.member == \"OM\") \"OK\" else \"no\"\n",
    );
}

#[test]
fn qualified_object_property_write() {
    both(
        "fqn_obj_var_write",
        "fun box(): String {\n\
         \x20 plib.Obj.mvar = \"OK\"\n\
         \x20 return plib.Obj.mvar\n\
         }\n",
    );
}

#[test]
fn qualified_object_function_call() {
    both(
        "fqn_obj_fun",
        "fun box(): String = if (plib.Obj.fn() == \"OF\") \"OK\" else \"no\"\n",
    );
}

#[test]
fn qualified_object_as_value() {
    both(
        "fqn_obj_value",
        "fun box(): String {\n\
         \x20 val o = plib.Obj\n\
         \x20 return if (o.fn() == \"OF\") \"OK\" else \"no\"\n\
         }\n",
    );
}

#[test]
fn qualified_nested_object_member_read() {
    both(
        "fqn_nested_obj",
        "fun box(): String = if (plib.Cls.NestObj.no == \"NO\") \"OK\" else \"no\"\n",
    );
}

#[test]
fn qualified_nested_object_function_call() {
    both(
        "fqn_nested_obj_fn",
        "fun box(): String = if (plib.Cls.NestObj.nf() == \"NF\") \"OK\" else \"no\"\n",
    );
}

// ---------------------------------------------------------------- enum entries

#[test]
fn qualified_enum_entry_read() {
    both(
        "fqn_enum_entry",
        "fun box(): String = if (plib.E.A.name == \"A\") \"OK\" else \"no\"\n",
    );
}

#[test]
fn qualified_enum_values_call() {
    both(
        "fqn_enum_values",
        "fun box(): String = if (plib.E.values().size == 2) \"OK\" else \"no\"\n",
    );
}

#[test]
fn qualified_enum_entries_read() {
    both(
        "fqn_enum_entries",
        "fun box(): String = if (plib.E.entries.size == 2) \"OK\" else \"no\"\n",
    );
}

// ------------------------------------------------------------- type positions

#[test]
fn qualified_type_annotation() {
    both(
        "fqn_type_ann",
        "fun box(): String {\n\
         \x20 val a: plib.Cls = plib.Cls(\"OK\")\n\
         \x20 return a.v\n\
         }\n",
    );
}

#[test]
fn qualified_nullable_type_annotation() {
    both(
        "fqn_type_nullable",
        "fun box(): String {\n\
         \x20 val a: plib.Cls? = plib.Cls(\"OK\")\n\
         \x20 return a!!.v\n\
         }\n",
    );
}

#[test]
fn qualified_nested_type_annotation() {
    both(
        "fqn_nested_type",
        "fun box(): String {\n\
         \x20 val a: plib.Cls.Nested = plib.Cls.Nested(\"OK\")\n\
         \x20 return a.n\n\
         }\n",
    );
}

#[test]
fn qualified_type_argument() {
    both(
        "fqn_type_arg",
        "fun box(): String {\n\
         \x20 val a: List<plib.Cls> = listOf(plib.Cls(\"OK\"))\n\
         \x20 return a[0].v\n\
         }\n",
    );
}

#[test]
fn qualified_explicit_type_argument() {
    both(
        "fqn_explicit_targ",
        "fun <T> id(t: T): T = t\n\
         fun box(): String = id<plib.Cls>(plib.Cls(\"OK\")).v\n",
    );
}

#[test]
fn qualified_supertype_class() {
    both(
        "fqn_supertype",
        "class D : plib.Base(\"OK\")\n\
         fun box(): String = D().b\n",
    );
}

#[test]
fn qualified_supertype_interface() {
    both(
        "fqn_supertype_iface",
        "class D : plib.Iface\n\
         fun box(): String {\n\
         \x20 val d: plib.Iface = D()\n\
         \x20 return if (d is D) \"OK\" else \"no\"\n\
         }\n",
    );
}

// --------------------------------------------------------------- is / as / ::

#[test]
fn qualified_is_target() {
    both(
        "fqn_is",
        "fun box(): String {\n\
         \x20 val a: Any = plib.Cls(\"OK\")\n\
         \x20 return if (a is plib.Cls) \"OK\" else \"no\"\n\
         }\n",
    );
}

#[test]
fn qualified_as_target() {
    both(
        "fqn_as",
        "fun box(): String {\n\
         \x20 val a: Any = plib.Cls(\"OK\")\n\
         \x20 return (a as plib.Cls).v\n\
         }\n",
    );
}

#[test]
fn qualified_when_is_branch() {
    both(
        "fqn_when_is",
        "fun box(): String {\n\
         \x20 val a: Any = plib.Cls(\"OK\")\n\
         \x20 return when (a) {\n\
         \x20   is plib.Cls -> a.v\n\
         \x20   else -> \"no\"\n\
         \x20 }\n\
         }\n",
    );
}

#[test]
fn qualified_class_literal() {
    both(
        "fqn_class_lit",
        "fun box(): String = if (plib.Cls::class.simpleName == \"Cls\") \"OK\" else \"no\"\n",
    );
}

#[test]
fn qualified_nested_class_literal() {
    both(
        "fqn_nested_class_lit",
        "fun box(): String = if (plib.Cls.Nested::class.simpleName == \"Nested\") \"OK\" else \"no\"\n",
    );
}

/// An unbound method reference through a fully-qualified classifier. The receiver names no value, so
/// the reference is unbound and its receiver class is its own first parameter — the same shape the
/// simple name produces. Cross-file the target has no `FunId` in this file, so the reference invokes
/// it by owner and name, exactly as the `java/lang/Object` methods already do.
#[test]
fn qualified_callable_reference() {
    both(
        "fqn_callable_ref",
        "fun box(): String {\n\
         \x20 val f = plib.Cls::method\n\
         \x20 return if (f(plib.Cls()) == \"m\") \"OK\" else \"no\"\n\
         }\n",
    );
}

// ---------------------------------------------- package-qualified top levels

#[test]
fn package_qualified_top_level_function() {
    both(
        "fqn_top_fun",
        "fun box(): String = if (plib.topLevelFun() == \"TF\") \"OK\" else \"no\"\n",
    );
}

#[test]
fn package_qualified_top_level_property_read() {
    both_ref(
        "fqn_top_prop",
        "fun box(): String = if (plib.topLevelProp == \"TP\") \"OK\" else \"no\"\n",
    );
}

#[test]
fn package_qualified_top_level_var_write() {
    both_ref(
        "fqn_top_var_write",
        "fun box(): String {\n\
         \x20 plib.topLevelVar = \"OK\"\n\
         \x20 return plib.topLevelVar\n\
         }\n",
    );
}

#[test]
fn package_qualified_top_level_const() {
    both(
        "fqn_top_const",
        "fun box(): String = if (plib.TOP_CONST == \"TC\") \"OK\" else \"no\"\n",
    );
}

// ------------------------------------------------------- deep classpath paths

#[test]
fn deep_qualified_classpath_construction() {
    common::expect_box_ok_with_stdlib(
        "fun box(): String {\n\
         \x20 val a = java.util.ArrayList<String>()\n\
         \x20 a.add(\"OK\")\n\
         \x20 return a[0]\n\
         }\n",
        "FqnDeepCtor",
    );
}

#[test]
fn deep_qualified_classpath_static_call() {
    common::expect_box_ok_with_stdlib(
        "fun box(): String = if (java.lang.Integer.parseInt(\"1\") == 1) \"OK\" else \"no\"\n",
        "FqnDeepStatic",
    );
}

#[test]
fn deep_qualified_classpath_class_literal() {
    common::expect_box_ok_with_stdlib(
        "fun box(): String =\n\
         \x20 if (java.util.ArrayList::class.simpleName == \"ArrayList\") \"OK\" else \"no\"\n",
        "FqnDeepClassLit",
    );
}

#[test]
fn deep_qualified_classpath_nested_package() {
    common::expect_box_ok_with_stdlib(
        "fun box(): String {\n\
         \x20 val a = java.util.concurrent.atomic.AtomicInteger(1)\n\
         \x20 return if (a.get() == 1) \"OK\" else \"no\"\n\
         }\n",
        "FqnDeepPkg",
    );
}

#[test]
fn qualified_kotlin_builtin_type() {
    common::expect_box_ok_with_stdlib(
        "fun box(): String {\n\
         \x20 val a: kotlin.String = \"OK\"\n\
         \x20 val b: kotlin.collections.List<kotlin.String> = listOf(a)\n\
         \x20 return b[0]\n\
         }\n",
        "FqnKotlinBuiltin",
    );
}

// ------------------------------------------------------------------ rejection

#[test]
fn qualified_name_with_unknown_package_is_rejected() {
    let Some(diagnostics) = common::diagnostics_against(
        "fqn_unknown_pkg",
        LIB,
        "fun box(): String = nosuch.pkg.Cls().v\n",
    ) else {
        return;
    };
    assert!(
        !diagnostics.is_empty(),
        "an unresolvable qualified path must be rejected, not compiled"
    );
}

#[test]
fn qualified_name_with_unknown_member_is_rejected() {
    let Some(diagnostics) = common::diagnostics_against(
        "fqn_unknown_member",
        LIB,
        "fun box(): String = plib.Cls.NOPE\n",
    ) else {
        return;
    };
    assert!(
        !diagnostics.is_empty(),
        "an unresolvable member under a qualified classifier must be rejected"
    );
}

#[test]
fn value_root_shadows_a_package_name() {
    // A local named like the package wins over the package path, exactly as kotlinc resolves it:
    // `plib` here is a value, so `plib.length` is a member read on a `String`.
    both(
        "fqn_value_root_shadows",
        "fun box(): String {\n\
         \x20 val plib = \"OK\"\n\
         \x20 return if (plib.length == 2) plib else \"no\"\n\
         }\n",
    );
}
