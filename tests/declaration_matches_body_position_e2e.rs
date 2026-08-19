//! A shape that types in a FUNCTION BODY must type in a DECLARATION.
//!
//! Every declaration-position defect this branch fixed had the same signature: the expression was
//! fine inside `fun box() { val x = … }` and rejected as `val x = …` at the top level, because the
//! walk could not type it and whatever answered instead read a record that was still being
//! determined. The invariant is worth asserting directly rather than one shape at a time.
use super::common;

const DECLS: &str = concat!(
    "class C { val bar = 42; fun gen() = bar\n",
    "    companion object { val seed = 7; fun made() = seed } }\n",
    "open class Base { val b = 1; fun inherited() = b }\n",
    "class D : Base()\n",
    "interface I { fun viaInterface(): Int }\n",
    "class E : I { val e = 2; override fun viaInterface() = e }\n",
    "class G { val g = 4; fun chain() = helper(); fun helper() = g }\n",
);

fn both_positions(shape: &str, stem: &str) {
    let declaration = format!("{DECLS}val d = {shape}\nfun box(): String = \"OK\"\n");
    let body = format!("{DECLS}fun box(): String {{ val l = {shape}; return \"OK\" }}\n");
    common::expect_box_ok_with_stdlib(&body, &format!("{stem}Body"));
    common::expect_box_ok_with_stdlib(&declaration, &format!("{stem}Decl"));
}

#[test]
fn an_inferred_member_return_types_in_both_positions() {
    both_positions("C().gen()", "MemberReturn");
}

#[test]
fn a_companion_function_return_types_in_both_positions() {
    both_positions("C.Companion.made()", "CompanionReturn");
}

#[test]
fn an_inherited_function_return_types_in_both_positions() {
    both_positions("D().inherited()", "InheritedReturn");
}

#[test]
fn an_overridden_function_return_types_in_both_positions() {
    both_positions("E().viaInterface()", "OverrideReturn");
}

#[test]
fn a_member_calling_a_member_types_in_both_positions() {
    // The member's body calls ANOTHER member whose return is also inferred, through the implicit
    // receiver. Typed `Unit` rather than `Int` while the placeholder was readable, which COMPILES —
    // running the box in both positions is what shows the type is wrong.
    both_positions("G().chain()", "ChainReturn");
}
