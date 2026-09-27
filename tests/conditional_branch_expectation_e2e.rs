//! A declared expectation fixes each conditional branch's own generic result, as kotlinc does: in
//! `val shape: Shape = when { c -> produce() else -> Circle() }` (or an assignment to a declared
//! `val`, a return, or an expression body), `produce`'s `T` is `Shape`, not the sibling branch's
//! `Circle`, also behind a block. An expectation of `Any`, a call argument's however deeply the
//! conditional sits in it, a conditional nested in another's branch, and no expectation at all
//! leave the sibling to decide.

use super::common;

const SOURCE: &str = "\
    interface Shape { fun name(): Int }\n\
    class Square : Shape { override fun name(): Int = 1 }\n\
    class Circle : Shape { override fun name(): Int = 2 }\n\
    fun <T> produce(): T = Square() as T\n\
    fun <T> produceOrNull(): T? = Square() as T\n\
    fun consume(shape: Shape): Int = shape.name()\n\
    fun keep(value: Any?) {}\n\
    var flag = true\n\
    fun declaredWhen(): Int {\n\
    \x20   val shape: Shape = when { flag -> produce() else -> Circle() }\n\
    \x20   return shape.name()\n\
    }\n\
    fun assignedWhen(): Int {\n\
    \x20   val shape: Shape\n\
    \x20   shape = when { flag -> produce() else -> Circle() }\n\
    \x20   return shape.name()\n\
    }\n\
    fun declaredNullable(): Int {\n\
    \x20   val shape: Shape? = if (flag) produce() else Circle()\n\
    \x20   return if (shape == null) 0 else shape.name()\n\
    }\n\
    fun declaredElvis(): Int {\n\
    \x20   val shape: Shape = produceOrNull() ?: Circle()\n\
    \x20   return shape.name()\n\
    }\n\
    fun declaredAny() {\n\
    \x20   val value: Any = if (flag) produce() else Circle()\n\
    \x20   keep(value)\n\
    }\n\
    fun returned(): Shape {\n\
    \x20   return if (flag) produce() else Circle()\n\
    }\n\
    fun bodied(): Shape = when { flag -> produce() else -> Circle() }\n\
    fun argument(): Int = consume(if (flag) produce() else Circle())\n\
    fun nestedArgument(): Int = consume(if (flag) (if (flag) produce() else Circle()) else Circle())\n\
    fun blockArgument(): Int = consume(if (flag) { keep(0); produce() } else Circle())\n\
    fun nestedDeclared(): Int {\n\
    \x20   val shape: Shape = if (flag) { keep(0); if (flag) produce() else Circle() } else Circle()\n\
    \x20   return shape.name()\n\
    }\n\
    fun inferred() {\n\
    \x20   val value = if (flag) produce() else Circle()\n\
    \x20   keep(value)\n\
    }\n\
    ";

#[test]
fn declared_expectations_fix_conditional_branches_like_kotlinc() {
    let pair = common::ModuleClassPair::compile(&[("Branches.kt", SOURCE)], "BranchesKt");
    // `returned` and `nestedDeclared` are checked by the box test only: their bytes differ on where
    // the upcast of a merged conditional sits (after the merge, kotlinc casts in each branch),
    // which is independent of branch inference.
    for method in [
        "declaredWhen",
        "assignedWhen",
        "declaredNullable",
        "declaredElvis",
        "declaredAny",
        "bodied",
        "argument",
        "nestedArgument",
        "blockArgument",
        "inferred",
    ] {
        let (kotlinc, krusty) = pair.method_code("BranchesKt", method);
        assert_eq!(krusty, kotlinc, "{method}");
    }
}

/// The generic branch's value is a `Square`: casting it to the sibling's `Circle` failed before.
/// kotlinc lets the sibling decide a nested conditional's `T`, so `nestedDeclared` fails that cast.
#[test]
fn a_declared_expectation_keeps_the_generic_branch_value() {
    let source = format!(
        "{SOURCE}\
        fun box(): String {{\n\
        \x20   if (declaredWhen() != 1) return \"declared\"\n\
        \x20   if (assignedWhen() != 1) return \"assigned\"\n\
        \x20   if (declaredNullable() != 1) return \"nullable\"\n\
        \x20   if (declaredElvis() != 1) return \"elvis\"\n\
        \x20   if (returned().name() != 1) return \"returned\"\n\
        \x20   if (bodied().name() != 1) return \"bodied\"\n\
        \x20   try {{ nestedDeclared(); return \"nested\" }} catch (e: Exception) {{}}\n\
        \x20   return \"OK\"\n\
        }}\n"
    );
    common::expect_box_same_as_kotlinc(&source, "Branches");
}
