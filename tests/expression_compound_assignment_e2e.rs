use super::common;

#[test]
fn expression_target_plus_assign_matches_kotlin_runtime_behavior() {
    if !common::stdlib_toolchain_ready() {
        return;
    }
    common::expect_box_ok_with_stdlib(
        "class Counter(var value: Int) {\n\
             operator fun plusAssign(delta: Int) { value += delta }\n\
         }\n\
         val shared = Counter(40)\n\
         fun counter(): Counter = shared\n\
         fun box(): String {\n\
             counter() += 2\n\
             return if (shared.value == 42) \"OK\" else \"fail\"\n\
         }\n",
        "expression_target_plus_assign",
    );
}

#[test]
fn a_generic_argument_over_enclosing_type_parameters_selects_a_generic_plus_assign() {
    common::expect_box_same_as_kotlinc(
        include_str!(
            "box_regressions/compoundAssignmentGenericArgumentOverEnclosingTypeParameters.kt"
        ),
        "CompoundAssignmentGenericArgument",
    );
}
