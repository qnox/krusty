//! JVM realization of producer-owned generated-function metadata.
//!
//! Common IR publishes language-level identity, visibility, and exact parameter names. This
//! boundary combines those facts with the function's post-lowering JVM representation without
//! inventing names or selecting declarations by spelling.

use crate::ir::{IrFile, IrGeneratedMemberPublication};
use crate::metadata::class_builder::FnMeta;
use crate::types::Visibility;

pub(super) fn functions(ir: &IrFile, publication: &IrGeneratedMemberPublication) -> Vec<FnMeta> {
    publication
        .functions
        .iter()
        .filter_map(|member| {
            let metadata = member.metadata.as_ref()?;
            let function = &ir.functions[member.function as usize];
            let declared = ir.vc_declared_sigs.get(&member.function);
            let realized_params = declared
                .map(|(_, params, _)| params.as_slice())
                .unwrap_or(function.params.as_slice());
            let realized_ret = declared.map(|(_, _, ret)| *ret).unwrap_or(function.ret);
            let semantic_signature = ir.signatures.get(&member.function);
            let member_semantic = ir.member_semantic_sigs.get(&member.function);
            let params = semantic_signature
                .map(|signature| signature.params.as_slice())
                .or(member_semantic.map(|(params, _)| params.as_slice()))
                .unwrap_or(realized_params);
            let ret = semantic_signature
                .and_then(|signature| signature.ret)
                .or(member_semantic.map(|(_, ret)| *ret))
                .unwrap_or(realized_ret);
            assert_eq!(
                member.parameter_names.len(),
                params.len(),
                "generated metadata parameter identities exactly match semantic arity"
            );
            let mut result = FnMeta::plain(
                metadata.source_name.clone(),
                member
                    .parameter_names
                    .iter()
                    .cloned()
                    .zip(params.iter().copied())
                    .collect(),
                ret,
            );
            result.flags = function_flags(ir, member.function, function, metadata.visibility);
            let physical_descriptor = crate::jvm::names::method_descriptor(
                &crate::jvm::ir_emit::jvm_tys(&function.params),
                function.ret,
            );
            let declared_descriptor =
                crate::jvm::names::method_descriptor(&crate::jvm::ir_emit::jvm_tys(params), ret);
            if physical_descriptor != declared_descriptor {
                result.jvm_sig = Some(physical_descriptor);
            }
            if function.name != metadata.source_name {
                result.jvm_sig_name = Some(function.name.clone());
            }
            result.annotations = ir
                .function_annotations
                .get(&member.function)
                .map(|annotations| annotations.applications().cloned().collect())
                .unwrap_or_default();
            Some(result)
        })
        .collect()
}

fn function_flags(
    ir: &IrFile,
    function_id: u32,
    function: &crate::ir::IrFunction,
    visibility: Visibility,
) -> u64 {
    let visibility = match visibility {
        Visibility::Internal => 0,
        Visibility::Private => 1,
        Visibility::Protected => 2,
        Visibility::Public => 3,
        Visibility::PackagePrivate => {
            unreachable!("package-private is never published in Kotlin metadata")
        }
    };
    let modality = if function.body.is_none() {
        2
    } else if ir.open_methods.contains(&function_id) {
        1
    } else {
        0
    };
    (visibility << 1) | (modality << 4)
}

#[cfg(test)]
mod tests {
    use super::{function_flags, functions};
    use crate::ir::{
        IrFile, IrFunction, IrGeneratedDeclarationDebug, IrGeneratedFunctionMetadata,
        IrGeneratedFunctionMetadataScope, IrGeneratedFunctionPublication,
        IrGeneratedMemberPublication,
    };
    use crate::types::{Ty, Visibility};

    #[test]
    fn generated_function_flags_use_semantic_visibility_and_real_modality() {
        let mut ir = IrFile::default();
        let body = ir.add_expr(crate::ir::IrExpr::Block {
            stmts: Vec::new(),
            value: None,
        });
        let function = ir.add_fun(IrFunction {
            name: "physical".to_string(),
            params: Vec::new(),
            ret: Ty::Unit,
            body: Some(body),
            is_static: false,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        assert_eq!(
            function_flags(
                &ir,
                function,
                &ir.functions[function as usize],
                Visibility::Internal,
            ),
            0
        );
        ir.open_methods.insert(function);
        assert_eq!(
            function_flags(
                &ir,
                function,
                &ir.functions[function as usize],
                Visibility::Public,
            ),
            22
        );
    }

    #[test]
    fn generated_metadata_keeps_canonical_names_and_semantic_value_class_shape() {
        let mut ir = IrFile::default();
        let function = ir.add_fun(IrFunction {
            name: "write$Self$app".to_string(),
            params: vec![Ty::Long, Ty::obj("kotlin/String"), Ty::obj("kotlin/Unit")],
            ret: Ty::Unit,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: vec![None, None, None],
        });
        ir.vc_declared_sigs.insert(
            function,
            (
                "write$Self".to_string(),
                vec![Ty::obj("sample/Value"), Ty::String, Ty::Unit],
                Ty::Unit,
            ),
        );
        let publication = IrGeneratedMemberPublication {
            metadata_scope: IrGeneratedFunctionMetadataScope::Additive,
            functions: vec![IrGeneratedFunctionPublication {
                function,
                parameter_names: ["self", "output", "serialDesc"].map(String::from).to_vec(),
                metadata: Some(IrGeneratedFunctionMetadata {
                    source_name: "write$Self".to_string(),
                    visibility: Visibility::Internal,
                }),
                debug: IrGeneratedDeclarationDebug::declaration_line(3),
            }],
        };

        let metadata = functions(&ir, &publication);
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].name, "write$Self");
        assert_eq!(
            metadata[0].params,
            vec![
                ("self".to_string(), Ty::obj("sample/Value")),
                ("output".to_string(), Ty::String),
                ("serialDesc".to_string(), Ty::Unit),
            ]
        );
        assert_eq!(metadata[0].ret, Ty::Unit);
        assert_eq!(metadata[0].flags, 32);
        assert_eq!(metadata[0].jvm_sig_name.as_deref(), Some("write$Self$app"));
        assert_eq!(
            metadata[0].jvm_sig.as_deref(),
            Some("(JLjava/lang/String;Lkotlin/Unit;)V")
        );
        assert!(metadata[0].annotations.is_empty());
    }
}
