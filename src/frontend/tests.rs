use super::*;
use crate::diag::{Diagnostic, Span};
use crate::libraries::{
    CallSig, Callables, FnKind, FunctionInfo, FunctionSet, GenericSig, LibraryCallable,
    LibraryType, PropKind, PropertyInfo, PropertySet, ResolvedSymbols, TypeKind,
};
use crate::source::SourceInput;
use crate::types::{Ty, TypeName, TypeNameList, Visibility};

mod analysis;
mod retention;
mod streaming;

#[test]
fn local_class_naming_records_exact_source_ownership_without_jvm_spelling() {
    let source = r#"
interface Probe { fun `class`(): Int }
fun consume(value: Any?) {}
fun calculate() {
    val holder = object { fun nested() = object {} }
    consume(Probe::class)
    consume(object {})
    consume(Probe::`class`)
    consume(object {})
    class Token
}
"#;
    let mut diagnostics = crate::diag::DiagSink::new();
    let mut file = parse_source(source, &LangFeatures::new(), &mut diagnostics);
    assert!(!diagnostics.has_errors(), "{:#?}", diagnostics.diags);
    record_local_class_name_provenance(&mut file);

    let mut anonymous = file
        .anonymous_object_classes
        .iter()
        .map(|(expression, declaration)| {
            (
                file.expr_spans[expression.0 as usize].lo,
                *declaration,
                file.local_class_name_provenance[declaration].clone(),
            )
        })
        .collect::<Vec<_>>();
    anonymous.sort_unstable_by_key(|(offset, _, _)| *offset);
    assert_eq!(anonymous.len(), 4);
    assert_eq!(anonymous[0].2.lexical_owner, None);
    assert_eq!(anonymous[0].2.segments, ["calculate", "holder"]);
    assert_eq!(anonymous[0].2.ordinal, Some(1));
    assert_eq!(anonymous[1].2.lexical_owner, Some(anonymous[0].1));
    assert_eq!(anonymous[1].2.segments, ["nested"]);
    assert_eq!(anonymous[1].2.ordinal, Some(1));
    assert_eq!(anonymous[2].2.lexical_owner, None);
    assert_eq!(anonymous[2].2.segments, ["calculate"]);
    assert_eq!(anonymous[2].2.ordinal, Some(1));
    assert_eq!(anonymous[3].2.lexical_owner, None);
    assert_eq!(anonymous[3].2.segments, ["calculate"]);
    // kotlinc 2.4.10: `calculate$1`, `calculate$2` for `Probe::`class``, then `calculate$3`.
    // A property initializer's object counts under `calculate$holder`, not `calculate`.
    assert_eq!(anonymous[3].2.ordinal, Some(3));

    let local = file
        .local_class_decls
        .values()
        .copied()
        .next()
        .expect("source declares one named local class");
    assert_eq!(
        file.local_class_name_provenance[&local],
        crate::ast::LocalClassNameProvenance {
            lexical_owner: None,
            segments: vec!["calculate".to_string(), "Token".to_string()],
            ordinal: None,
        }
    );
}

#[test]
fn continuation_positions_are_keyed_by_exact_declarations_in_source_order() {
    let source = "suspend fun first() {}\nsuspend fun First() {}";
    let mut diagnostics = crate::diag::DiagSink::new();
    let mut file = parse_source(source, &LangFeatures::new(), &mut diagnostics);
    assert!(!diagnostics.has_errors(), "{:#?}", diagnostics.diags);
    record_local_class_name_provenance(&mut file);

    let declarations = file
        .decls
        .iter()
        .copied()
        .filter(|declaration| matches!(file.decl(*declaration), crate::ast::Decl::Fun(_)))
        .collect::<Vec<_>>();
    assert_eq!(declarations.len(), 2);
    assert_eq!(
        file.suspend_continuation_ordinals
            .get(&crate::ast::AnonymousEnclosingFunction::TopLevel(
                declarations[0]
            )),
        Some(&1)
    );
    assert_eq!(
        file.suspend_continuation_ordinals
            .get(&crate::ast::AnonymousEnclosingFunction::TopLevel(
                declarations[1]
            )),
        Some(&2)
    );
}
