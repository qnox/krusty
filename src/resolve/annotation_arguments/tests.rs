use crate::ast::{Expr, ExprId, File};
use crate::diag::DiagSink;
use crate::resolve::TypeInfo;
use crate::types::Ty;

fn analyze(source: &str) -> (File, TypeInfo) {
    let mut diagnostics = DiagSink::new();
    let (file, _, info) = crate::frontend::analyze_source_standalone(source, &mut diagnostics);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    (file, info.expect("retained checked annotation analysis"))
}

fn assert_selected_applications(
    file: &File,
    info: &TypeInfo,
    name: &str,
    expected: Ty,
    count: usize,
) {
    let calls = file
        .expr_arena
        .iter()
        .enumerate()
        .filter_map(|(index, expression)| {
            let Expr::Call { callee, .. } = expression else {
                return None;
            };
            matches!(file.expr(*callee), Expr::Name(callee) if callee == name)
                .then_some(ExprId(index as u32))
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), count);
    for call in calls {
        assert_eq!(info.semantic_ty(call), expected, "applied result of {name}");
        assert!(
            info.resolved_constructors.contains_key(&call),
            "{name} must retain its selected constructor"
        );
    }
}

#[test]
fn nested_generic_annotation_calls_retain_inferred_and_explicit_constructor_applications() {
    let source = "annotation class Leaf<T>(val n: Int)\n\
                  annotation class Envelope(val leaf: Leaf<String>)\n\
                  @Envelope(Leaf(1)) class Inferred\n\
                  @Envelope(Leaf<String>(2)) class Explicit\n";
    let (file, info) = analyze(source);
    assert_selected_applications(&file, &info, "Leaf", Ty::obj_args("Leaf", &[Ty::String]), 2);
}

#[test]
fn nested_annotation_array_literals_commit_the_selected_element_application() {
    let source = "annotation class Leaf<T>(val n: Int)\n\
                  annotation class Nest<T>(val leaves: Array<Leaf<T>>)\n\
                  annotation class Envelope(val nested: Nest<String>)\n\
                  @Envelope(Nest<String>([Leaf<String>(1), Leaf<String>(2)])) class Tagged\n";
    let (file, info) = analyze(source);
    let leaf = Ty::obj_args("Leaf", &[Ty::String]);
    assert_selected_applications(&file, &info, "Leaf", leaf, 2);
    assert_selected_applications(&file, &info, "Nest", Ty::obj_args("Nest", &[Ty::String]), 1);
    let arrays = file
        .expr_arena
        .iter()
        .enumerate()
        .filter_map(|(index, expression)| {
            matches!(expression, Expr::AnnotationArrayLiteral(_)).then_some(ExprId(index as u32))
        })
        .collect::<Vec<_>>();
    assert_eq!(arrays.len(), 1);
    assert_eq!(info.semantic_ty(arrays[0]), Ty::array(leaf));
}

#[test]
fn generic_annotation_owner_arguments_specialize_the_nested_constructor_expectation() {
    let source = "annotation class Leaf<T>(val n: Int)\n\
                  annotation class Envelope<T>(val leaf: Leaf<T>)\n\
                  @Envelope<String>(Leaf(1)) class Inferred\n\
                  @Envelope<String>(Leaf<String>(2)) class Explicit\n";
    let (file, info) = analyze(source);
    assert_selected_applications(&file, &info, "Leaf", Ty::obj_args("Leaf", &[Ty::String]), 2);
}

#[test]
fn annotation_alias_owner_expands_its_selected_declaration_before_specializing_elements() {
    let source = "annotation class Leaf<T>(val n: Int)\n\
                  annotation class Envelope<A, B>(val leaf: Leaf<B>)\n\
                  typealias Alias<T> = Envelope<Int, Leaf<T>>\n\
                  @Alias<String>(Leaf<Leaf<String>>(1)) class Tagged\n";
    let (file, info) = analyze(source);
    assert_selected_applications(
        &file,
        &info,
        "Leaf",
        Ty::obj_args("Leaf", &[Ty::obj_args("Leaf", &[Ty::String])]),
        1,
    );
}
