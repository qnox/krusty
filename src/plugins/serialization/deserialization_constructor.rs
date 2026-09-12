//! Construction of the synthetic constructor used by serialization deserialization.

use super::{class_ty, value_class_underlying};
use crate::ir::{
    Callee, ClassId, CtorDelegateTarget, DeclarationAnnotations, IrConst, IrExpr, IrFile,
    IrSecondaryConstructorRole, IrSecondaryCtor, IrStatic,
};
use crate::kt_string::KtString;
use crate::libraries::InlineKind;
use crate::types::{type_name, Ty, TypeName, Visibility};

/// Required-element masks in the serialization constructor's ABI shape. kotlinc keeps an extra
/// zero mask at every exact 32-field boundary, so the word count is `len / 32 + 1`.
fn required_masks(optional: &[bool]) -> Vec<i32> {
    let mut masks = vec![0; optional.len() / 32 + 1];
    for (index, optional) in optional.iter().enumerate() {
        if !optional {
            masks[index / 32] |= 1i32.wrapping_shl((index % 32) as u32);
        }
    }
    masks
}

/// Add kotlinc's outer-class `$cachedDescriptor` for a generic serializable class. The synthetic
/// deserialization constructor has no serializer instance, so this stable class-owned field is its
/// descriptor source; recovering one from a serializer spelling or fabricating type serializers
/// would be a fallback.
pub(super) fn add_cached_descriptor(
    ir: &mut IrFile,
    owner: TypeName,
    serial_name: KtString,
    elements: &[(KtString, bool)],
) -> u32 {
    let descriptor_ty = class_ty("kotlinx/serialization/descriptors/SerialDescriptor");
    let implementation_ty =
        class_ty("kotlinx/serialization/internal/PluginGeneratedSerialDescriptor");
    let name = ir.add_expr(IrExpr::Const(IrConst::String(serial_name)));
    let generated_serializer = ir.add_expr(IrExpr::Const(IrConst::Null));
    let count = ir.add_expr(IrExpr::Const(IrConst::Int(elements.len() as i32)));
    let descriptor = ir.new_external(
        "kotlinx/serialization/internal/PluginGeneratedSerialDescriptor",
        "(Ljava/lang/String;Lkotlinx/serialization/internal/GeneratedSerializer;I)V",
        vec![name, generated_serializer, count],
    );
    let descriptor_value = 0;
    let mut statements = vec![ir.add_expr(IrExpr::Variable {
        index: descriptor_value,
        ty: implementation_ty,
        init: Some(descriptor),
        named: false,
    })];
    for (name, optional) in elements {
        let receiver = ir.add_expr(IrExpr::GetValue(descriptor_value));
        let name = ir.add_expr(IrExpr::Const(IrConst::String(name.clone())));
        let optional = ir.add_expr(IrExpr::Const(IrConst::Boolean(*optional)));
        statements.push(ir.add_expr(IrExpr::Call {
            callee: Callee::Virtual {
                owner: type_name("kotlinx/serialization/internal/PluginGeneratedSerialDescriptor"),
                name: "addElement".to_string(),
                descriptor: "(Ljava/lang/String;Z)V".to_string(),
                params: None,
                interface: false,
            },
            dispatch_receiver: Some(receiver),
            args: vec![name, optional],
        }));
    }
    let value = ir.add_expr(IrExpr::GetValue(descriptor_value));
    let value = ir.add_expr(IrExpr::TypeOp {
        op: crate::ir::IrTypeOp::Cast,
        arg: value,
        type_operand: descriptor_ty,
    });
    let init = ir.add_expr(IrExpr::Block {
        stmts: statements,
        value: Some(value),
    });
    let index = ir.statics.len() as u32;
    ir.statics.push(IrStatic {
        name: "$cachedDescriptor".to_string(),
        ty: descriptor_ty,
        init,
        is_var: false,
        is_const: false,
        owner: Some(owner),
        visibility: Visibility::Private,
        custom_accessor: true,
        line: 0,
        source_order: u32::MAX,
    });
    index
}

fn descriptor_access(
    ir: &mut IrFile,
    serializer_id: ClassId,
    cached_descriptor: Option<u32>,
) -> u32 {
    if !ir.classes[serializer_id as usize].is_object {
        return ir.add_expr(IrExpr::GetStatic(cached_descriptor.expect(
            "a generic serialization constructor requires its class-owned cached descriptor",
        )));
    }
    let instance = ir.add_expr(IrExpr::StaticInstance {
        owner: serializer_id,
        ty: serializer_id,
        field: "INSTANCE",
    });
    ir.add_expr(IrExpr::Call {
        callee: Callee::Virtual {
            owner: ir.classes[serializer_id as usize].fq_name_id(),
            name: "getDescriptor".to_string(),
            descriptor: "()Lkotlinx/serialization/descriptors/SerialDescriptor;".to_string(),
            params: None,
            interface: false,
        },
        dispatch_receiver: Some(instance),
        args: vec![],
    })
}

fn multi_mask_check(
    ir: &mut IrFile,
    serializer_id: ClassId,
    cached_descriptor: Option<u32>,
    required_masks: &[i32],
) -> u32 {
    let mut missing_conditions = Vec::with_capacity(required_masks.len());
    for (word, required) in required_masks.iter().enumerate() {
        let expected = ir.add_expr(IrExpr::Const(IrConst::Int(*required)));
        let mask = ir.add_expr(IrExpr::Const(IrConst::Int(*required)));
        let seen = ir.add_expr(IrExpr::GetValue(word as u32 + 1));
        let masked = ir.add_expr(IrExpr::PrimitiveBinOp {
            op: crate::ir::IrBinOp::BitAnd,
            lhs: mask,
            rhs: seen,
        });
        missing_conditions.push(ir.add_expr(IrExpr::PrimitiveBinOp {
            op: crate::ir::IrBinOp::Ne,
            lhs: expected,
            rhs: masked,
        }));
    }
    let mut missing_conditions = missing_conditions.into_iter();
    let mut missing = missing_conditions
        .next()
        .expect("multi-mask constructor has masks");
    for rhs in missing_conditions {
        missing = ir.add_expr(IrExpr::PrimitiveBinOp {
            op: crate::ir::IrBinOp::BitOr,
            lhs: missing,
            rhs,
        });
    }

    let array_ty = class_ty("kotlin/IntArray");
    let seen_elements = (0..required_masks.len())
        .map(|word| ir.add_expr(IrExpr::GetValue(word as u32 + 1)))
        .collect::<Vec<_>>();
    let seen_array = ir.add_expr(IrExpr::Vararg {
        array_type: array_ty,
        spreads: vec![false; seen_elements.len()],
        elements: seen_elements,
    });

    let required_elements = required_masks
        .iter()
        .map(|mask| ir.add_expr(IrExpr::Const(IrConst::Int(*mask))))
        .collect::<Vec<_>>();
    let required_array = ir.add_expr(IrExpr::Vararg {
        array_type: array_ty,
        spreads: vec![false; required_elements.len()],
        elements: required_elements,
    });
    let descriptor = descriptor_access(ir, serializer_id, cached_descriptor);
    let report = ir.add_expr(IrExpr::Call {
        callee: Callee::Static {
            owner: type_name("kotlinx/serialization/internal/PluginExceptionsKt"),
            name: "throwArrayMissingFieldException".to_string(),
            descriptor: "([I[ILkotlinx/serialization/descriptors/SerialDescriptor;)V".to_string(),
            inline: InlineKind::None,
        },
        dispatch_receiver: None,
        args: vec![seen_array, required_array, descriptor],
    });
    let reported = ir.add_expr(IrExpr::Block {
        stmts: vec![report],
        value: None,
    });
    ir.add_expr(IrExpr::When {
        branches: vec![(Some(missing), reported)],
    })
}

/// Add the ABI-only deserialization constructor for a plain serializable data class.
pub(super) fn add_deserialization_constructor(
    ir: &mut IrFile,
    class_id: ClassId,
    serializer_id: ClassId,
    fields: &[Ty],
    cached_descriptor: Option<u32>,
) {
    // The deserialization ABI retains a value-class field's BOXED source type. The extra default
    // marker below disambiguates this constructor from the physical primary constructor; the body
    // performs the representation conversion when storing the field.
    let field_tys = fields.to_vec();
    // kotlinc keeps one mask slot past every complete 32-field group.
    let mask_count = fields.len() / 32 + 1;
    let mut params = vec![Ty::Int; mask_count];
    params.extend(field_tys);
    params.push(class_ty(
        "kotlinx/serialization/internal/SerializationConstructorMarker",
    ));
    let has_value_field = fields
        .iter()
        .any(|ty| value_class_underlying(ir, ty).is_some());

    let optional = ir.classes[class_id as usize]
        .fields
        .iter()
        .take(fields.len())
        .map(crate::ir::IrField::has_default)
        .collect::<Vec<_>>();
    let required_masks = required_masks(&optional);
    debug_assert_eq!(required_masks.len(), mask_count);
    let owner = ir.classes[class_id as usize].fq_name_id();
    let constructor_defaults = ir
        .class_ctor_defaults_name(owner)
        .cloned()
        .unwrap_or_default();

    let mut delegate_prelude = Vec::new();
    if mask_count == 1 {
        let required_mask = required_masks[0];
        // `if (required != (required and seen)) throwMissingFieldException(...)`, before `super()`.
        let required = ir.add_expr(IrExpr::Const(IrConst::Int(required_mask)));
        let and_lhs = ir.add_expr(IrExpr::Const(IrConst::Int(required_mask)));
        let seen = ir.add_expr(IrExpr::GetValue(1));
        let masked = ir.add_expr(IrExpr::PrimitiveBinOp {
            op: crate::ir::IrBinOp::BitAnd,
            lhs: and_lhs,
            rhs: seen,
        });
        let missing = ir.add_expr(IrExpr::PrimitiveBinOp {
            op: crate::ir::IrBinOp::Ne,
            lhs: required,
            rhs: masked,
        });
        let seen_arg = ir.add_expr(IrExpr::GetValue(1));
        let required_arg = ir.add_expr(IrExpr::Const(IrConst::Int(required_mask)));
        let descriptor = descriptor_access(ir, serializer_id, cached_descriptor);
        let report = ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: type_name("kotlinx/serialization/internal/PluginExceptionsKt"),
                name: "throwMissingFieldException".to_string(),
                descriptor: "(IILkotlinx/serialization/descriptors/SerialDescriptor;)V".to_string(),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![seen_arg, required_arg, descriptor],
        });
        let reported = ir.add_expr(IrExpr::Block {
            stmts: vec![report],
            value: None,
        });
        delegate_prelude.push(ir.add_expr(IrExpr::When {
            branches: vec![(Some(missing), reported)],
        }));
    } else {
        delegate_prelude.push(multi_mask_check(
            ir,
            serializer_id,
            cached_descriptor,
            &required_masks,
        ));
    }

    let this = ir.add_expr(IrExpr::GetValue(0));
    let mut body_stmts = Vec::with_capacity(fields.len());
    for (index, optional) in optional.iter().copied().enumerate() {
        // `this` is slot 0; masks occupy 1..=mask_count; field arguments follow them.
        let argument = ir.add_expr(IrExpr::GetValue(index as u32 + 1 + mask_count as u32));
        if value_class_underlying(ir, &fields[index]).is_some() {
            // Unlike ordinary source-constructor parameters, this ABI slot carries the value-class
            // BOX. Publish that exact expression representation so JVM lowering inserts
            // `unbox-impl` at the erased backing-field store.
            ir.physical_types.insert(argument, fields[index]);
        }
        let store_argument = ir.add_expr(IrExpr::SetField {
            receiver: this,
            class: class_id,
            index: index as u32,
            value: argument,
        });
        let default = optional.then(|| {
            let default = constructor_defaults.get(index).copied().flatten().expect(
                "a serializable field declaring a default must retain its lowered expression",
            );
            let (default, _) = crate::ir::clone_expression_dag(ir, default);
            crate::ir::shift_value_indices(ir, default, 1, mask_count as u32);
            default
        });
        let Some(default) = default else {
            body_stmts.push(store_argument);
            continue;
        };
        let seen = ir.add_expr(IrExpr::GetValue((index / 32 + 1) as u32));
        let bit = ir.add_expr(IrExpr::Const(IrConst::Int(
            1i32.wrapping_shl((index % 32) as u32),
        )));
        let masked = ir.add_expr(IrExpr::PrimitiveBinOp {
            op: crate::ir::IrBinOp::BitAnd,
            lhs: seen,
            rhs: bit,
        });
        let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        let absent = ir.add_expr(IrExpr::PrimitiveBinOp {
            op: crate::ir::IrBinOp::Eq,
            lhs: masked,
            rhs: zero,
        });
        let store_default = ir.add_expr(IrExpr::SetField {
            receiver: this,
            class: class_id,
            index: index as u32,
            value: default,
        });
        let defaulted = ir.add_expr(IrExpr::Block {
            stmts: vec![store_default],
            value: None,
        });
        let supplied = ir.add_expr(IrExpr::Block {
            stmts: vec![store_argument],
            value: None,
        });
        body_stmts.push(ir.add_expr(IrExpr::When {
            branches: vec![(Some(absent), defaulted), (None, supplied)],
        }));
    }
    let body = ir.add_expr(IrExpr::Block {
        stmts: body_stmts,
        value: None,
    });
    let super_owner = ir.classes[class_id as usize].superclass;
    let ordinal = u32::try_from(ir.classes[class_id as usize].secondary_ctors.len())
        .expect("too many secondary constructors for an IR identity");
    ir.classes[class_id as usize]
        .secondary_ctors
        .push(IrSecondaryCtor {
            annotations: DeclarationAnnotations::default(),
            prefix_params: Vec::new(),
            vararg_index: None,
            params,
            named_params: Vec::new(),
            defaults: vec![],
            delegate_prelude,
            delegate_args: vec![],
            default_parameters: Vec::new(),
            body: Some(body),
            delegate: CtorDelegateTarget::Super {
                owner: super_owner,
                target_params: vec![],
                default_masks: vec![],
            },
            synthetic: true,
            // The JVM value-class pass makes this constructor private and emits the ordinary public
            // `DefaultConstructorMarker` accessor. The serialization constructor itself retains
            // only `SerializationConstructorMarker`, matching kotlinc's nestmate call target.
            vc_params: has_value_field,
        });
    ir.record_generated_secondary_constructor(
        class_id,
        IrSecondaryConstructorRole::SerializationDeserialization,
        ordinal,
    );
}

#[cfg(test)]
mod tests {
    use super::required_masks;

    #[test]
    fn required_masks_cover_exact_word_boundaries() {
        assert_eq!(required_masks(&[false; 31]), [0x7fff_ffff]);
        assert_eq!(required_masks(&[false; 32]), [-1, 0]);
        assert_eq!(required_masks(&[false; 33]), [-1, 1]);
        assert_eq!(required_masks(&[false, true, false]), [5]);
    }
}
