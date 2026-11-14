//! The annotations of the exact declaration an inherited default forwarder republishes.

use super::*;
use krusty::fir::{
    InheritedMemberName, ResolvedFunctionOverrideTarget, ResolvedPropertyOverrideTarget,
};
use krusty::ir::{DeclarationAnnotations, RetainedAnnotation};

pub(super) fn selected(
    default: &krusty::fir::ResolvedInheritedDefault,
    env: &EmitEnv<'_>,
) -> Option<DeclarationAnnotations> {
    let module = match (default.function, default.accessor) {
        (Some(ResolvedFunctionOverrideTarget::Module(callable)), None) => {
            Some(env.source_member_annotations.function(callable))
        }
        (None, Some(ResolvedPropertyOverrideTarget::Module(property))) => Some(
            if matches!(default.name, InheritedMemberName::PropertySetter(_)) {
                env.source_member_annotations.setter(property)
            } else {
                env.source_member_annotations.getter(property)
            },
        ),
        _ => None,
    };
    if let Some(annotations) = module {
        return match annotations {
            Some(annotations) => Some(annotations.clone()),
            None => {
                env.run.set_emit_error(format!(
                    "inherited default declaration has no registered source annotation facts: {} {:?} {:?}",
                    default.declaring_interface.render(), default.function, default.accessor,
                ));
                None
            }
        };
    }
    let external = match (default.function, default.accessor) {
        (Some(ResolvedFunctionOverrideTarget::External(callable)), None)
        | (None, Some(ResolvedPropertyOverrideTarget::External(callable))) => Some(callable),
        _ => None,
    };
    let identity =
        external.expect("a checked inherited default names one module or dependency declaration");
    let fact = env
        .dependency_callables
        .callable(identity)
        .expect("an inherited dependency declaration has frozen callable facts");
    if fact.annotations.is_empty() {
        return Some(DeclarationAnnotations::default());
    }
    let owner = fact.physical_owner.render();
    let name = fact.physical_name();
    let descriptor = &fact.descriptor;
    let annotations = env.bodies.class_file(&owner).and_then(|bytes| {
        krusty::jvm::classreader::read_method_annotation_applications(&bytes, name, descriptor)
    });
    let Some(annotations) = annotations else {
        env.run.set_emit_error(format!(
            "inherited default declaration has no readable annotation attributes: {owner}.{name}{descriptor}"
        ));
        return None;
    };
    Some(DeclarationAnnotations::new(
        annotations
            .into_iter()
            .map(|(retention, annotation)| RetainedAnnotation {
                retention,
                annotation: krusty::ir::AppliedAnnotation::from(&annotation),
                facts: annotation.facts,
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use krusty::fir::{CallableId, InheritedDefaultBody, ResolvedInheritedDefault};

    struct NoSourceBytes;
    impl krusty::jvm::inline::MethodBodies for NoSourceBytes {
        fn body(&self, _: &str, _: &str, _: &str) -> Option<krusty::jvm::classreader::MethodCode> {
            None
        }
        fn class_file(&self, _: &str) -> Option<Vec<u8>> {
            panic!("a module declaration must not consult dependency class bytes")
        }
    }

    #[test]
    fn an_unregistered_module_default_is_not_treated_as_unannotated() {
        let mut ir = IrFile::default();
        let interface = krusty::types::type_name("MissingInterface");
        let mut class = krusty::plugins::synthetic_class("Consumer");
        class.interfaces = vec![interface].into();
        let owner = class.fq_name_id();
        ir.add_class(class);
        let target = ResolvedFunctionOverrideTarget::Module(CallableId::from_raw(99));
        ir.inherited_defaults.insert(
            owner,
            vec![ResolvedInheritedDefault {
                name: InheritedMemberName::Function("value".into()),
                function: Some(target),
                accessor: None,
                declaring_interface: interface,
                dispatch_interface: interface,
                parameters: Box::new([]),
                parameter_identities: Box::new([]),
                result: Ty::String,
                applied_parameters: Box::new([]),
                applied_result: Ty::String,
                type_parameters: Box::new([]),
                suspend: false,
                vararg: false,
                body: InheritedDefaultBody::Module,
            }],
        );
        let run = crate::ir_emit::EmitRun::default();
        assert!(crate::ir_emit::invariant_tests::emit_for_test_with_bodies(
            &ir,
            "TestKt",
            &run,
            &NoSourceBytes
        )
        .is_none());
        assert_eq!(run.emit_error(), Some(format!(
            "inherited default declaration has no registered source annotation facts: MissingInterface {:?} None", Some(target),
        )));
    }
}
