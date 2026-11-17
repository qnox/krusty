//! Diagnostic context for a failed common lowering of an already-selected dependency.

use crate::fir_lower::{FirFileLoweringFailure, FirLoweringFailure};
use crate::symbol_source::SymbolSource;

pub(super) fn describe(error: &FirFileLoweringFailure, source: &dyn SymbolSource) -> String {
    let detail = format!("{error:?}");
    let FirFileLoweringFailure::Body(
        FirLoweringFailure::UnsupportedExternalCall(identity)
        | FirLoweringFailure::UnsupportedExternalCallableReference(identity)
        | FirLoweringFailure::UnsupportedExternalConstructor(identity),
    ) = error
    else {
        return detail;
    };
    // This is the registry read for the exact checked id, not a declaration/candidate lookup.
    let Some(selected) = source.external_callable(*identity) else {
        return format!("{detail}; selected external {identity:?}: declaration unavailable");
    };
    let callable = &selected.callable;
    let mut context = format!("{detail}; selected external {identity:?}: v1");
    for (key, value) in [
        ("id", identity.raw().to_string()),
        ("dependency", format!("{:?}", selected.dependency_callable)),
        (
            "declaration",
            format!("{:?}", selected.declaration_signature),
        ),
        ("owner", format!("{:?}", selected.declaration_owner)),
        ("name", format!("{:?}", callable.name)),
        ("kind", format!("{:?}", selected.kind)),
        ("parameters", format!("{:?}", callable.params)),
        ("result", format!("{:?}", callable.ret)),
        (
            "declared_frame",
            format!(
                "{:?}",
                (
                    callable.source_receiver,
                    &callable.declared_params,
                    callable.declared_ret,
                    &callable.generic_sig,
                )
            ),
        ),
        ("inline", format!("{:?}", callable.inline)),
        ("role", format!("{:?}", callable.semantic_role)),
        ("intrinsic", format!("{:?}", callable.compiler_intrinsic)),
    ] {
        use std::fmt::Write;
        // Byte framing keeps arbitrary Debug strings opaque, including delimiters and Unicode.
        write!(context, "; {key}={}:{value}", value.len()).unwrap();
    }
    context
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fir::ExternalCallableId;
    use crate::libraries::{
        ExternalCallableKind, ExternalCallableRealization, KlibDependencyCallable, LibraryCallable,
    };
    use crate::metadata::id_signature::{decode_public_id_signature, KlibDeclarationSignature};
    use crate::types::{type_name, SemanticCallableOwner, Ty};
    use std::cell::RefCell;

    fn signature() -> KlibDeclarationSignature {
        let strings = ["fixture".to_owned(), "selected".to_owned()];
        let mut common = vec![0x0a, 1, 0, 0x10, 1, 0x31];
        common.extend(0x55_u64.to_le_bytes());
        common.extend([0x20, 0]);
        let mut bytes = vec![0x0a, common.len() as u8];
        bytes.extend(common);
        KlibDeclarationSignature::Public(
            decode_public_id_signature(&bytes, &strings)
                .unwrap()
                .unwrap(),
        )
    }

    fn realization(
        id: ExternalCallableId,
        module: &str,
        parameter: Ty,
    ) -> ExternalCallableRealization {
        let owner = Some(SemanticCallableOwner::Package(type_name("fixture")));
        let mut callable = LibraryCallable::library(
            "fixture/LibraryKt",
            "selected",
            vec![parameter],
            Ty::String,
            Ty::String,
            "",
        );
        callable.external_identity = Some(id);
        callable.declared_params = Some(vec![parameter].into_boxed_slice());
        callable.declared_ret = Some(Ty::String);
        callable.declaration_owner = owner;
        ExternalCallableRealization {
            callable,
            kind: ExternalCallableKind::TopLevel,
            declaration_owner: owner,
            parameter_identities: Box::new([]),
            declaration_signature: Some(signature()),
            dependency_callable: Some(KlibDependencyCallable {
                module: module.into(),
                declaration: signature(),
            }),
            direct_c_function: None,
            klib_body_companion_receiver: None,
            klib_body_type_parameters: Box::new([]),
            klib_body_declaration_modifiers: None,
            klib_body_class_contract: None,
            klib_body_accessor: None,
            klib_body_vararg_index: None,
            klib_body_parameter_defaults: None,
            declares_parameter_defaults: false,
        }
    }

    #[derive(Default)]
    struct Selected {
        queries: RefCell<Vec<ExternalCallableId>>,
    }

    impl SymbolSource for Selected {
        fn symbols(
            &self,
            _: crate::symbol_source::SymbolNamespace,
            _: &str,
        ) -> std::rc::Rc<crate::libraries::ResolvedSymbols> {
            panic!("failure diagnostics must not select declarations by name")
        }

        fn external_callable(&self, id: ExternalCallableId) -> Option<ExternalCallableRealization> {
            self.queries.borrow_mut().push(id);
            match id.raw() {
                501 => Some(realization(id, "first", Ty::Int)),
                946 => Some(realization(id, "second", Ty::Long)),
                _ => None,
            }
        }
    }

    #[test]
    fn exact_selected_id_retains_module_signature_and_frame_for_same_spelled_declarations() {
        for (raw, expected) in [
            (
                501,
                r#"Body(UnsupportedExternalCall(ExternalCallableId(501))); selected external ExternalCallableId(501): v1; id=3:501; dependency=228:Some(KlibDependencyCallable { module: "first", declaration: Public(KlibPublicIdSignature { package: KlibNamePath { segments: ["fixture"] }, declaration: KlibNamePath { segments: ["selected"] }, member_id: Some(85), mask: 0 }) }); declaration=171:Some(Public(KlibPublicIdSignature { package: KlibNamePath { segments: ["fixture"] }, declaration: KlibNamePath { segments: ["selected"] }, member_id: Some(85), mask: 0 })); owner=34:Some(Package(TypeName("fixture"))); name=10:"selected"; kind=8:TopLevel; parameters=33:[Obj(TypeName("kotlin/Int"), [])]; result=34:Obj(TypeName("kotlin/String"), []); declared_frame=95:(None, Some([Obj(TypeName("kotlin/Int"), [])]), Some(Obj(TypeName("kotlin/String"), [])), None); inline=4:None; role=4:None; intrinsic=4:None"#,
            ),
            (
                946,
                r#"Body(UnsupportedExternalCall(ExternalCallableId(946))); selected external ExternalCallableId(946): v1; id=3:946; dependency=229:Some(KlibDependencyCallable { module: "second", declaration: Public(KlibPublicIdSignature { package: KlibNamePath { segments: ["fixture"] }, declaration: KlibNamePath { segments: ["selected"] }, member_id: Some(85), mask: 0 }) }); declaration=171:Some(Public(KlibPublicIdSignature { package: KlibNamePath { segments: ["fixture"] }, declaration: KlibNamePath { segments: ["selected"] }, member_id: Some(85), mask: 0 })); owner=34:Some(Package(TypeName("fixture"))); name=10:"selected"; kind=8:TopLevel; parameters=34:[Obj(TypeName("kotlin/Long"), [])]; result=34:Obj(TypeName("kotlin/String"), []); declared_frame=96:(None, Some([Obj(TypeName("kotlin/Long"), [])]), Some(Obj(TypeName("kotlin/String"), [])), None); inline=4:None; role=4:None; intrinsic=4:None"#,
            ),
        ] {
            let source = Selected::default();
            let id = ExternalCallableId::from_raw(raw);
            let error =
                FirFileLoweringFailure::Body(FirLoweringFailure::UnsupportedExternalCall(id));
            assert_eq!(describe(&error, &source), expected);
            assert_eq!(*source.queries.borrow(), [id]);
        }
    }

    #[test]
    fn other_lowering_failures_keep_their_message_without_querying_a_provider() {
        let source = Selected::default();
        let error = FirFileLoweringFailure::Body(FirLoweringFailure::UnsupportedIntrinsicCall);
        assert_eq!(describe(&error, &source), "Body(UnsupportedIntrinsicCall)");
        assert_eq!(*source.queries.borrow(), []);
    }

    #[test]
    fn an_absent_selected_record_is_not_recovered_by_spelling() {
        let source = Selected::default();
        let id = ExternalCallableId::from_raw(947);
        let error = FirFileLoweringFailure::Body(FirLoweringFailure::UnsupportedExternalCall(id));
        assert_eq!(
            describe(&error, &source),
            "Body(UnsupportedExternalCall(ExternalCallableId(947))); selected external ExternalCallableId(947): declaration unavailable"
        );
        assert_eq!(*source.queries.borrow(), [id]);
    }
}
