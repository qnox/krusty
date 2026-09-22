//! A secondary constructor the checker selected, reached as itself.
//!
//! A `super(…)` delegation names the EXACT constructor the checker selected, and that may be a
//! secondary one: `class E : A { constructor() : super() }` where `A`'s no-argument constructor is
//! secondary. Taking the primary for every `super(…)` called it with the wrong arguments —
//! Cranelift's own verifier caught it as a mismatched argument count, which is a decline rather
//! than a wrong answer, and it took every file holding such a constructor with it.
//!
//! An enum CONSTANT selects the same way: `ENTRY` on an enum whose primary takes a `String` and
//! which also declares `constructor() : this("OK")` is a call to that secondary.
//!
//! Kotlin admits no two constructors of one class with the same parameter list, so a secondary
//! matching the selection IS the selection, and the primary is what remains when none does.
//!
//! Every expectation is kotlinc's, taken by running the same program under it.

use super::common::{expect_box_ok_with_stdlib, expect_native_box};

/// `super(…)` reaching the superclass's secondary constructor, and its primary beside it.
#[test]
fn a_super_delegation_reaches_the_constructor_the_checker_selected() {
    let source = "open class A(val x: Int) {\n\
         \x20   constructor() : this(1)\n\
         \x20   constructor(s: String) : this(s.length)\n\
         }\n\
         class E : A {\n\
         \x20   constructor(i: Int) : super(i)\n\
         \x20   constructor() : super()\n\
         \x20   constructor(s: String) : super(s)\n\
         }\n\
         fun box(): String {\n\
         \x20   if (E(4).x != 4) return \"fail the primary\"\n\
         \x20   if (E().x != 1) return \"fail the no-argument secondary\"\n\
         \x20   if (E(\"abc\").x != 3) return \"fail the String secondary\"\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "SuperSecondaryConstructor");
    expect_native_box(source, "SuperSecondaryConstructor", "OK");
}

/// An enum constant selecting a secondary constructor.
#[test]
fn an_enum_constant_reaches_the_secondary_constructor_it_selected() {
    let source = "enum class My(val s: String) {\n\
         \x20   PLAIN,\n\
         \x20   LOUD(\"loud\");\n\
         \x20   constructor() : this(\"OK\")\n\
         }\n\
         fun box(): String {\n\
         \x20   if (My.LOUD.s != \"loud\") return \"fail the primary\"\n\
         \x20   return My.PLAIN.s\n\
         }\n";
    expect_box_ok_with_stdlib(source, "EnumSecondaryConstructor");
    expect_native_box(source, "EnumSecondaryConstructor", "OK");
}

/// A class HEADER naming the superclass's secondary constructor.
///
/// `class D : A(4)` where `A`'s `(Int)` constructor is secondary is the same selection the
/// delegation above makes, written where a supertype is listed. Reading the primary's parameter
/// list for every base call made the arity disagree and declined the file.
#[test]
fn a_supertype_list_reaches_the_constructor_the_checker_selected() {
    let source = "sealed class A() {\n\
         \x20   var trace: String = \"primary\"\n\
         \x20   constructor(i: Int) : this() {\n\
         \x20       trace = \"secondary \" + i\n\
         \x20   }\n\
         }\n\
         class B : A()\n\
         class D : A(4)\n\
         fun box(): String {\n\
         \x20   if (B().trace != \"primary\") return \"fail B\"\n\
         \x20   if (D().trace != \"secondary 4\") return \"fail D: \" + D().trace\n\
         \x20   return \"OK\"\n\
         }\n";
    expect_box_ok_with_stdlib(source, "SupertypeSecondaryConstructor");
    expect_native_box(source, "SupertypeSecondaryConstructor", "OK");
}

/// An INNER class's secondary constructors, which all lead with the outer instance.
///
/// A `this(…)` delegation passes that prefix on to the constructor it reaches — the call was one
/// operand short of it, which Cranelift's verifier caught. A `super(…)` one reaches no constructor
/// of this class at all, so it stores the prefix itself: an inner class's outer reference goes in
/// BEFORE the base's constructor runs, which is Kotlin's own order and observable through a base
/// `init` that calls an overridden method.
///
/// A class with NO primary constructor has only these, so leaving the store out left the field
/// null and every read through it faulted.
#[test]
fn an_inner_classs_secondary_constructors_carry_the_outer_instance() {
    let source = "class Outer(val s: String) {\n\
         \x20   inner class Kept(val x: Int) {\n\
         \x20       constructor() : this(7)\n\
         \x20       fun outer() = s\n\
         \x20   }\n\
         \x20   inner class Fresh {\n\
         \x20       val x: Int\n\
         \x20       constructor(n: Int) { x = n }\n\
         \x20       constructor(t: String) { x = t.length }\n\
         \x20       fun outer() = s\n\
         \x20   }\n\
         }\n\
         fun box(): String {\n\
         \x20   val outer = Outer(\"OK\")\n\
         \x20   if (outer.Kept().x != 7) return \"fail the delegating secondary\"\n\
         \x20   if (outer.Kept().outer() != \"OK\") return \"fail its outer instance\"\n\
         \x20   if (outer.Fresh(42).x != 42) return \"fail the Int constructor\"\n\
         \x20   if (outer.Fresh(\"zzz\").x != 3) return \"fail the String constructor\"\n\
         \x20   if (outer.Fresh(42).outer() != \"OK\") return \"fail an outer with no primary\"\n\
         \x20   return outer.Fresh(\"zzz\").outer()\n\
         }\n";
    expect_box_ok_with_stdlib(source, "InnerSecondaryConstructor");
    expect_native_box(source, "InnerSecondaryConstructor", "OK");
}
