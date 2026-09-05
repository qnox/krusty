use super::test_support::{
    checked_function_body_with_platform, jvm_stdlib_semantics, root_expression,
};
use super::*;

#[test]
fn nested_flat_map_uses_accumulated_builder_element_constraints() {
    let (body, _) = checked_function_body_with_platform(
        "fun foo(x: List<String>) =\n\
         \x20   buildList {\n\
         \x20       add(\"\")\n\
         \x20       addAll(flatMap { x })\n\
         \x20   }\n",
        "foo",
        jvm_stdlib_semantics(),
    );

    let root = body
        .expr(root_expression(&body))
        .expect("checked buildList call");
    assert_eq!(
        root.ty.get(),
        Ty::obj_args("kotlin/collections/List", &[Ty::String])
    );
    assert!(matches!(root.kind, FirExprKind::Call(_)));
}

#[test]
fn thrown_builder_argument_infers_nothing() {
    let (body, _) = checked_function_body_with_platform(
        "class Buildee<T> { fun yield(value: T) {} }\n\
         fun <T> build(block: Buildee<T>.() -> Unit): Buildee<T> = Buildee<T>()\n\
         fun foo(): Buildee<Nothing> = build { yield(throw IllegalStateException()) }\n",
        "foo",
        jvm_stdlib_semantics(),
    );

    let root = body
        .expr(root_expression(&body))
        .expect("checked build call");
    assert_eq!(root.ty.get(), Ty::obj_args("Buildee", &[Ty::Nothing]));
    assert!(matches!(root.kind, FirExprKind::Call(_)));
}
