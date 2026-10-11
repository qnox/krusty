//! JVM realization of producer-owned generated-function metadata.
//!
//! The class's declaration record describes a generated function from the language-level identity,
//! visibility, and exact parameter names its producer published. This boundary adds how the JVM
//! realizes that function after lowering, without inventing names or selecting declarations by
//! spelling.

use crate::ir::IrFile;
use crate::metadata::class_builder::{FnMeta, JvmFunctionSignature};

/// How the JVM realizes the generated `function` its class's record describes as `record`: the
/// physical name and descriptor, each recorded only where a reader cannot rebuild it.
pub(super) fn signature(ir: &IrFile, function: u32, record: &FnMeta) -> JvmFunctionSignature {
    let realized = &ir.functions[function as usize];
    let params = record.params.iter().map(|(_, ty)| *ty).collect::<Vec<_>>();
    let physical_descriptor = crate::jvm::names::method_descriptor(
        &crate::jvm::ir_emit::jvm_tys(&realized.params),
        realized.ret,
    );
    let declared_descriptor =
        crate::jvm::names::method_descriptor(&crate::jvm::ir_emit::jvm_tys(&params), record.ret);
    // The descriptor is recorded when it differs from the declared one, and also when a reader
    // could not derive it from the recorded types at all — a `kotlin/Array` anywhere in the
    // signature, whose descriptor depends on the type ARGUMENT. That is the rule every other
    // metadata path applies, and it is why kotlinc records one for a generated
    // `childSerializers(): Array<KSerializer<*>>` while recording none for its siblings.
    let underivable = std::iter::once(record.ret)
        .chain(params.iter().copied())
        .any(crate::metadata::descriptor_needs_recording);
    JvmFunctionSignature {
        name: (realized.name != record.name).then(|| realized.name.clone()),
        desc: (physical_descriptor != declared_descriptor || underivable)
            .then_some(physical_descriptor),
    }
}

#[cfg(test)]
mod tests {
    use super::signature;
    use crate::ir::{IrFile, IrFunction};
    use crate::metadata::class_builder::FnMeta;
    use crate::types::Ty;

    fn function(ir: &mut IrFile, name: &str, params: Vec<Ty>, ret: Ty) -> u32 {
        ir.add_fun(IrFunction {
            name: name.to_string(),
            params,
            ret,
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        })
    }

    #[test]
    fn a_lowered_generated_function_records_its_physical_name_and_descriptor() {
        let mut ir = IrFile::default();
        let lowered = function(
            &mut ir,
            "write$Self$app",
            vec![Ty::Long, Ty::obj("kotlin/String"), Ty::obj("kotlin/Unit")],
            Ty::Unit,
        );
        let record = FnMeta::plain(
            "write$Self".to_string(),
            vec![
                ("self".to_string(), Ty::obj("sample/Value")),
                ("output".to_string(), Ty::String),
                ("serialDesc".to_string(), Ty::Unit),
            ],
            Ty::Unit,
        );

        let signature = signature(&ir, lowered, &record);
        assert_eq!(signature.name.as_deref(), Some("write$Self$app"));
        assert_eq!(
            signature.desc.as_deref(),
            Some("(JLjava/lang/String;Lkotlin/Unit;)V")
        );
    }

    #[test]
    fn generated_array_of_star_metadata_keeps_its_physical_descriptor() {
        let mut ir = IrFile::default();
        let element = Ty::obj_args(
            "fixtures/Coffer",
            &[Ty::star_projection(Ty::nullable(Ty::obj("fixtures/Token")))],
        );
        let array = Ty::obj_args("kotlin/Array", &[element]);
        let release = function(&mut ir, "release", Vec::new(), array);
        let record = FnMeta::plain("release".to_string(), Vec::new(), array);

        let signature = signature(&ir, release, &record);
        assert_eq!(signature.name, None);
        assert_eq!(signature.desc.as_deref(), Some("()[Lfixtures/Coffer;"));
    }
}
