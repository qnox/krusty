//! Kotlin-metadata publication for secondary constructors.
//!
//! Common IR decides whether a constructor is a Kotlin declaration and records its semantic
//! visibility. This JVM boundary supplies descriptors and protobuf flag encoding without inferring
//! publication from a synthetic access flag, parameter spelling, or physical constructor shape.

use crate::ir::{IrClass, IrFile, IrSecondaryCtor};
use crate::jvm::{ir_emit::jvm_tys, names::type_descriptor};
use crate::metadata::class_builder::JvmConstructorSignature;

/// How the JVM realizes the class's secondary constructor at `ordinal`.
pub(super) fn secondary_constructor_signature(
    ir: &IrFile,
    class: &IrClass,
    ordinal: usize,
) -> JvmConstructorSignature {
    // A value class's lowering consumed its secondary constructors into static `constructor-impl`
    // overloads, keeping each one's handle by ordinal.
    if let Some(constructors) = ir.jvm_value_class_secondary_ctors.get(&class.fq_name_id()) {
        let constructor = constructors
            .iter()
            .find(|constructor| constructor.ordinal == ordinal)
            .expect("a published value-class secondary constructor keeps its static handle");
        return JvmConstructorSignature {
            name: "constructor-impl".into(),
            desc: constructor.descriptor.clone(),
        };
    }
    declared_constructor_signature(class, &class.secondary_ctors[ordinal])
}

fn declared_constructor_signature(
    class: &IrClass,
    constructor: &IrSecondaryCtor,
) -> JvmConstructorSignature {
    // The JVM constructor takes the enum's `(String, int)` name and ordinal first.
    let owner_prefix = if class.is_enum {
        "Ljava/lang/String;I"
    } else {
        ""
    };
    JvmConstructorSignature::init(format!(
        "({owner_prefix}{}{}{})V",
        jvm_tys(&constructor.prefix_params)
            .iter()
            .map(|&ty| type_descriptor(ty))
            .collect::<String>(),
        jvm_tys(&constructor.params)
            .iter()
            .map(|&ty| type_descriptor(ty))
            .collect::<String>(),
        if constructor.vc_params {
            "Lkotlin/jvm/internal/DefaultConstructorMarker;"
        } else {
            ""
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::{declared_constructor_signature, secondary_constructor_signature};
    use crate::ir::{
        CtorDelegateTarget, DeclarationAnnotations, IrFile, IrJvmValueClassSecondaryCtor,
        IrSecondaryCtor,
    };
    use crate::plugins::synthetic_class;
    use crate::types::{type_name, Ty, Visibility};

    fn constructor(params: Vec<Ty>) -> IrSecondaryCtor {
        IrSecondaryCtor {
            annotations: DeclarationAnnotations::default(),
            source_order: u32::MAX,
            lines: crate::ir::IrSecondaryCtorLines::default(),
            prefix_params: vec![Ty::Boolean],
            params,
            declared_spellings: crate::spelling::DeclaredSpellings::default(),
            named_params: Vec::new(),
            metadata_visibility: Some(Visibility::Internal),
            generated_debug: crate::ir::IrGeneratedDeclarationDebug::None,
            vararg_index: None,
            defaults: Vec::new(),
            delegate_prelude: Vec::new(),
            delegate_args: Vec::new(),
            default_parameters: Vec::new(),
            body: None,
            delegate: CtorDelegateTarget::Super {
                owner: type_name("kotlin/Any"),
                target_params: Vec::new(),
                target: crate::ir::IrConstructorTarget::UNRESTRICTED_PRIMARY,
                default_masks: Vec::new(),
            },
            synthetic: true,
            vc_params: false,
            param_checks: Vec::new(),
        }
    }

    #[test]
    fn a_secondary_constructor_names_its_physical_init() {
        let class = synthetic_class("sample/Owner");
        let signature = declared_constructor_signature(
            &class,
            &constructor(vec![
                Ty::Int,
                Ty::obj("kotlin/String"),
                Ty::Unit,
                Ty::ty_param("T", Ty::obj("kotlin/Any")),
                Ty::Long,
            ]),
        );
        assert_eq!(signature.name, "<init>");
        assert_eq!(
            signature.desc,
            "(ZILjava/lang/String;Lkotlin/Unit;Ljava/lang/Object;J)V"
        );
    }

    #[test]
    fn an_enum_constructor_takes_the_name_and_ordinal_first() {
        let mut class = synthetic_class("sample/Owner");
        class.is_enum = true;
        let signature = declared_constructor_signature(&class, &constructor(vec![Ty::Int]));
        assert_eq!(signature.desc, "(Ljava/lang/String;IZI)V");
    }

    #[test]
    fn a_value_class_constructor_names_the_static_handle_kept_for_its_ordinal() {
        let mut ir = IrFile::default();
        let class = synthetic_class("sample/Value");
        // The lowering consumed the class's secondary constructors; only the published ones, the
        // first and third, keep a handle.
        ir.jvm_value_class_secondary_ctors.insert(
            class.fq_name_id(),
            vec![
                IrJvmValueClassSecondaryCtor {
                    ordinal: 0,
                    descriptor: "(J)I".to_string(),
                },
                IrJvmValueClassSecondaryCtor {
                    ordinal: 2,
                    descriptor: "(Ljava/lang/String;)I".to_string(),
                },
            ],
        );
        let signature = secondary_constructor_signature(&ir, &class, 2);
        assert_eq!(signature.name, "constructor-impl");
        assert_eq!(signature.desc, "(Ljava/lang/String;)I");
    }
}
