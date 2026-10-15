use super::*;
use crate::diag::DiagSink;
use crate::features::LangFeatures;
use crate::frontend::{PlatformProvider, SourceInput};
use crate::libraries::{CompilerCoreDeclaration, EmptySymbolSource};

#[test]
fn any_equals_relation_is_published_for_every_classifier_placement() {
    let expected = ResolvedOperatorEqualsOverride {
        overridden: ResolvedOperatorEqualsTarget::CompilerCore(
            CompilerCoreDeclaration::KotlinAnyEquals,
        ),
        overridden_owner: crate::types::wk::any(),
        overridden_semantic_role: Some(crate::types::SemanticCallRole::KotlinAnyEquals),
        overridden_result: crate::fir::ResolvedTy::new(Ty::Boolean).unwrap(),
        overridden_suspend: false,
    };
    let source = "class Ordinary {\n\
                      override operator fun equals(other: Any?): Boolean = true\n\
                  }\n\
                  enum class Entries {\n\
                      A { override operator fun equals(other: Any?): Boolean = true }\n\
                  }\n\
                  fun owners(): Any {\n\
                      class Local {\n\
                          override operator fun equals(other: Any?): Boolean = true\n\
                      }\n\
                      return object {\n\
                          override operator fun equals(other: Any?): Boolean = true\n\
                      }\n\
                  }\n";
    let mut diagnostics = DiagSink::new();
    let mut analysis = crate::frontend::analyze_source_set_with_features(
        &[SourceInput::kotlin(source).with_file_stem("OperatorEqualsIdentity")],
        PlatformProvider::jvm(Box::new(EmptySymbolSource)),
        &LangFeatures::new(),
        &mut diagnostics,
    );
    assert!(
        diagnostics.diags.is_empty(),
        "unexpected diagnostics: {:?}",
        diagnostics.diags
    );
    let streamed = analysis
        .streamed
        .take()
        .expect("operator declarations retain a streamed semantic module");
    let (mut index, _, _, _) = streamed.module.into_parts();
    crate::resolve::publish_checked_local_signatures(
        &analysis.files[0],
        crate::fir::SourceFileId::from_raw(0),
        &analysis.symbols,
        analysis.types[0].as_ref().expect("checked source"),
        &mut index,
    )
    .expect("the exact checker-selected local relation must freeze with its stable signature");

    let core = super::super::override_plans::declared_functions(
        &EmptySymbolSource,
        Ty::obj_name(crate::types::wk::any()),
        "equals",
    );
    let [core] = core.as_slice() else {
        panic!("core provider must publish one Any.equals declaration");
    };
    assert_eq!(
        core.callable.compiler_core_declaration,
        Some(CompilerCoreDeclaration::KotlinAnyEquals)
    );
    assert_eq!(
        core.callable.declaration_owner,
        Some(crate::types::SemanticCallableOwner::Classifier(
            crate::types::wk::any()
        ))
    );
    let mut malformed_module = core.clone();
    malformed_module.stable_declaration = Some(DeclarationId::from_raw(u32::MAX));
    malformed_module.callable.external_identity =
        Some(crate::fir::ExternalCallableId::from_raw(11));
    assert_eq!(
        super::target(&index, &malformed_module),
        None,
        "a missing stable module callable must not fall back to another provider identity"
    );

    let declarations = index
        .declarations_named("equals")
        .iter()
        .copied()
        .filter(|declaration| {
            index
                .declaration_header(*declaration)
                .and_then(|header| header.owner)
                .and_then(|owner| index.declaration_header(owner))
                .is_some_and(|owner| {
                    matches!(
                        owner.kind,
                        DeclarationKind::Classifier | DeclarationKind::EnumEntry
                    )
                })
        })
        .collect::<Vec<_>>();
    assert_eq!(declarations.len(), 4, "exact four owner placements");
    let mut placements = Vec::new();
    for declaration in declarations {
        let owner = index
            .declaration_header(declaration)
            .and_then(|header| header.owner)
            .and_then(|owner| index.declaration_header(owner))
            .expect("equals owner");
        let placement = if owner.kind == DeclarationKind::EnumEntry {
            1
        } else if owner.flags.has(DeclarationFlags::ANONYMOUS_OBJECT) {
            3
        } else if owner.flags.has(DeclarationFlags::LOCAL_CLASS) {
            2
        } else {
            0
        };
        placements.push((placement, index.operator_equals_override(declaration)));
    }
    placements.sort_by_key(|(placement, _)| *placement);
    assert_eq!(
        placements,
        vec![
            (0, Some(expected)),
            (1, Some(expected)),
            (2, Some(expected)),
            (3, Some(expected))
        ],
        "the exact relation consumed without diagnostics must be frozen unchanged for every owner"
    );
}

#[test]
fn checked_local_relation_rejects_an_unpublished_stable_callable() {
    let declaration = DeclarationId::from_raw(7);
    let selected = crate::fx_hash::HashMap::from_iter([(
        declaration,
        ResolvedOperatorEqualsOverride {
            overridden: ResolvedOperatorEqualsTarget::CompilerCore(
                CompilerCoreDeclaration::KotlinAnyEquals,
            ),
            overridden_owner: crate::types::wk::any(),
            overridden_semantic_role: Some(crate::types::SemanticCallRole::KotlinAnyEquals),
            overridden_result: crate::fir::ResolvedTy::new(Ty::Boolean).unwrap(),
            overridden_suspend: false,
        },
    )]);
    let mut index = ResolvedModuleIndex::default();
    assert_eq!(
        publish_checked_locals(&mut index, &selected),
        Err(declaration)
    );
    assert_eq!(index.operator_equals_override(declaration), None);
}
