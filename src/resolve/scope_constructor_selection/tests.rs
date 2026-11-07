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

#[test]
fn conditional_bounds_reach_function_scope_and_bound_inner_constructor_selection() {
    let source = r#"
class Pick<T : Number>(value: T)
class Owner {
    inner class Picked<T : Number>(value: T)
}
fun <T : Number> choose(value: T): T = value
fun selections(flag: Boolean, long: Long, int: Int) {
    val direct = Pick(if (flag) long else int)
    val bound = Owner().Picked(if (flag) long else int)
    val function = choose(if (flag) long else int)
}
"#;
    let mut diagnostics = DiagSink::new();
    let classpath = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(vec![
        crate::toolchain::stdlib_jar().expect("stdlib for the declared Number bounds"),
        crate::toolchain::jdk_modules().expect("JDK numeric superclass declarations"),
    ]));
    let platform = crate::frontend::PlatformProvider::jvm(Box::new(
        crate::jvm::jvm_libraries::JvmLibraries::new(classpath)
            .expect("JVM provider initialization"),
    ));
    let (file, _, info) = crate::frontend::analyze_source(source, platform, &mut diagnostics);
    assert!(diagnostics.diags.is_empty(), "{:?}", diagnostics.diags);
    let info = info.expect("complete checked source");
    let direct = calls_named(&file, "Pick");
    let function = calls_named(&file, "choose");
    assert_eq!(direct.len(), 1);
    assert_eq!(function.len(), 1);
    assert!(info.resolved_constructors.contains_key(&direct[0]));
    assert!(info.resolved_call_arg_slots.contains_key(&direct[0]));
    let bound = info
        .resolved_constructors
        .iter()
        .filter_map(|(call, selected)| {
            matches!(
                selected,
                crate::resolve::ResolvedConstructor::Plain { outer: Some(_), .. }
            )
            .then_some(*call)
        })
        .collect::<Vec<_>>();
    assert_eq!(bound.len(), 1);
    assert!(info.resolved_call_arg_slots.contains_key(&bound[0]));
    let direct_arguments = info.semantic_ty(direct[0]).type_args();
    let provider_arguments = info.semantic_ty(bound[0]).type_args();
    assert_eq!(direct_arguments.len(), 1);
    assert_eq!(provider_arguments, direct_arguments);
    assert_eq!(info.semantic_ty(function[0]), direct_arguments[0]);
    assert_ne!(direct_arguments[0], crate::types::Ty::Error);
    assert!(!direct_arguments[0].mentions_pending());
}
