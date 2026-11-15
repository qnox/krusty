use super::*;
use crate::fir::{
    DeclarationAnchor, DeclarationFlags, DeclarationKind, ResolvedDeclarationHeader,
    ResolvedTypeParameterFlags, SourceFileId,
};
use crate::types::{type_name, TypeVariance, Visibility};

fn local_classifier() -> ResolvedDeclarationHeader {
    ResolvedDeclarationHeader {
        kind: DeclarationKind::Classifier,
        owner: None,
        name: None,
        visibility: Visibility::Public,
        flags: DeclarationFlags::default().with(DeclarationFlags::LOCAL_CLASS, true),
        initialization_order: None,
    }
}

fn intern_local_classifier(
    index: &mut ResolvedModuleIndex,
    name: &str,
    identity: crate::types::TypeName,
) -> crate::fir::DeclarationId {
    let declaration = index.intern_checked_local_declaration(
        DeclarationAnchor {
            source: SourceFileId::from_raw(0),
            range: crate::diag::Span::new(0, 1),
            owner: None,
            kind: DeclarationKind::Classifier,
            sibling: 0,
        },
        local_classifier(),
        name,
    );
    index
        .publish_classifier_header(declaration, identity, None, [], [], [], [])
        .unwrap();
    declaration
}

/// A header without its type-argument layout did not finalize. Projecting it would publish an
/// empty type-parameter list, which is a different fact from the parameters already on the index.
#[test]
fn a_header_without_its_type_argument_layout_stays_unpublished() {
    let mut index = ResolvedModuleIndex::default();
    let identity = type_name("sample/Child");
    let declaration = intern_local_classifier(&mut index, "Child", identity);
    index
        .publish_type_parameter(
            declaration,
            0,
            "T",
            "T",
            ResolvedTypeParameterFlags::new(TypeVariance::Invariant, false, false),
            [],
        )
        .unwrap();

    assert!(index.declaration_anchor(declaration).is_some());
    assert!(index.classifier_header(declaration).is_some());
    assert!(index.type_parameter(declaration, 0).is_some());
    assert!(index.classifier_type_arguments(declaration).is_none());
    assert!(StreamedModuleSymbols::for_file(&index, 0)
        .stable_classifier(identity)
        .is_none());

    let parameter = index.type_parameter(declaration, 0).unwrap();
    index.publish_classifier_type_arguments(declaration, 1, [parameter]);
    let projected = StreamedModuleSymbols::for_file(&index, 0)
        .stable_classifier(identity)
        .expect("a published layout is the classifier's type parameters");
    assert_eq!(projected.type_params(), &["T".to_owned()]);
}

/// An empty layout is a classifier that declares no type parameters. Absence of the layout is not
/// that fact.
#[test]
fn a_published_empty_type_argument_layout_is_projected() {
    let mut index = ResolvedModuleIndex::default();
    let identity = type_name("sample/Plain");
    let declaration = intern_local_classifier(&mut index, "Plain", identity);
    index.publish_classifier_type_arguments(declaration, 0, []);

    let projected = StreamedModuleSymbols::for_file(&index, 0)
        .stable_classifier(identity)
        .expect("a published empty layout is a classifier with no type parameters");
    assert!(projected.type_params().is_empty());
}
