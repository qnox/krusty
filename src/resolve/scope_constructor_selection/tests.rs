use crate::ast::{Expr, ExprId, File};
use crate::diag::DiagSink;

fn calls_named(file: &File, name: &str) -> Vec<ExprId> {
    file.expr_arena
        .iter()
        .enumerate()
        .filter_map(|(index, expression)| {
            let Expr::Call { callee, .. } = expression else {
                return None;
            };
            matches!(file.expr(*callee), Expr::Name(callee) if callee == name)
                .then_some(ExprId(index as u32))
        })
        .collect()
}

#[test]
fn a_rejected_family_filtered_to_one_publishes_no_constructor_commitment() {
    // Arity leaves `constructor(f)` as the one candidate the argument maps onto, so its mismatch
    // is the one diagnostic; the rejected call commits neither a constructor nor its argument
    // slots.
    let source = "interface Factory<K, E>\n\
                  class StrFactory : Factory<String, Char>\n\
                  class Two<K, E> {\n\
                      constructor(f: Factory<K, E>)\n\
                      constructor(s: String, n: Int)\n\
                  }\n\
                  val x: Two<Char, Char> = Two<Char, Char>(StrFactory())\n";
    let mut diagnostics = DiagSink::new();
    let (file, _, info) = crate::frontend::analyze_source_standalone(source, &mut diagnostics);
    let messages = diagnostics
        .diags
        .iter()
        .map(|diagnostic| diagnostic.msg.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        messages,
        ["argument type mismatch: actual type is 'StrFactory', but 'Factory<Char, Char>' was expected."]
    );
    let info = info.expect("retained checked analysis");
    let calls = calls_named(&file, "Two");
    assert_eq!(calls.len(), 1);
    let call = calls[0];
    assert!(!info.resolved_constructors.contains_key(&call));
    assert!(!info.resolved_call_arg_slots.contains_key(&call));
}
