//! Explicit declaration-name facts for hand-built emitter fixtures.

use super::*;
use crate::libraries::ClassifierDeclarationKey;
use crate::types::ClassifierDeclarationName;

pub(super) struct DeclarationNames<'a>(pub(super) &'a IrFile);

impl BackendClassifierSource for DeclarationNames<'_> {
    fn classifier(
        &self,
        _: crate::types::TypeName,
    ) -> Option<std::sync::Arc<crate::backend::BackendClassifierFact>> {
        None
    }

    fn classifier_declaration_name(
        &self,
        declaration: ClassifierDeclarationKey,
    ) -> Option<Option<ClassifierDeclarationName>> {
        let ClassifierDeclarationKey::Ordinary(identity) = declaration else {
            return None;
        };
        self.0
            .class_id_by_name(identity)
            .map(|id| self.0.classes[id as usize].declaration_name.clone())
    }
}

#[test]
fn nested_metadata_uses_declaration_identity_not_generated_name_shape() {
    let mut ir = IrFile::default();
    let mut outer = crate::plugins::synthetic_class("demo/Outer");
    outer.is_source_declared = true;
    outer.declaration_name = Some(ClassifierDeclarationName::TopLevel {
        package: crate::types::type_name("demo"),
        name: "Outer".into(),
    });
    let outer_id = ir.add_class(outer);
    ir.record_class_source_order(outer_id, 0);

    // A digit is valid in a source identifier; the former generated-name heuristic dropped it.
    let mut declared = crate::plugins::synthetic_class("demo/Outer$Node2");
    declared.is_source_declared = true;
    declared.declaration_name = Some(ClassifierDeclarationName::Nested {
        owner: crate::types::type_name("demo/Outer"),
        name: "Node2".into(),
    });
    let declared_id = ir.add_class(declared);
    ir.record_class_source_order(declared_id, 1);

    // Looking nested is not sufficient: implementation classes are not metadata declarations.
    ir.add_class(crate::plugins::synthetic_class("demo/Outer$Impl3"));
    crate::metadata::class_declarations::record_all(&mut ir);

    let metadata = build_class_metadata_with_facts(
        &ir,
        &crate::jvm::override_results::OverrideResults::default(),
        &ir.classes[outer_id as usize],
        &EmitOptions::default(),
        &LocalDelegatedProperties::default(),
        &DeclarationNames(&ir),
        &EmitRun::default(),
    )
    .expect("plain source class metadata");
    assert!(metadata.d2.iter().any(|entry| entry == "Node2"));
    assert!(!metadata.d2.iter().any(|entry| entry == "Impl3"));
}
