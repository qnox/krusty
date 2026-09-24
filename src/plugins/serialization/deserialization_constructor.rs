//! Construction of the synthetic constructor used by serialization deserialization.

use super::serial_elements::SerialElements;
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
        setter_jvm_name: None,
        erased_declared_ty: None,
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

/// Kotlin's source-level name for the generated constructor's trailing disambiguating parameter.
const MARKER_PARAMETER: &str = "serializationConstructorMarker";

/// Add the deserialization constructor for a plain serializable data class. It is synthetic in the
/// class file but published as an internal Kotlin secondary constructor.
///
/// It takes one argument per serial element and initializes every backing field in declaration
/// order: an element from its argument (or its default when the element was absent), and a
/// `@Transient` property — which is not an element — from its initializer, as kotlinc does.
pub(super) fn add_deserialization_constructor(
    ir: &mut IrFile,
    class_id: ClassId,
    serializer_id: ClassId,
    named_fields: &[(String, Ty)],
    elements: &SerialElements,
    cached_descriptor: Option<u32>,
) {
    let element_fields = elements.select(named_fields);
    // The deserialization ABI retains a value-class field's BOXED source type. The extra default
    // marker below disambiguates this constructor from the physical primary constructor; the body
    // performs the representation conversion when storing the field.
    let field_tys = element_fields.iter().map(|(_, ty)| *ty).collect::<Vec<_>>();
    // kotlinc keeps one mask slot past every complete 32-element group.
    let mask_count = element_fields.len() / 32 + 1;
    let mut params = vec![Ty::Int; mask_count];
    params.extend(field_tys.iter().copied());
    params.push(class_ty(
        "kotlinx/serialization/internal/SerializationConstructorMarker",
    ));
    let has_value_field = field_tys
        .iter()
        .any(|ty| value_class_underlying(ir, ty).is_some());

    // An element is optional when its property declares a default: a constructor parameter's
    // default or a body property's initializer.
    let optional = elements
        .fields()
        .iter()
        .map(|&field| super::property_default::checked_default(ir, class_id, field).is_some())
        .collect::<Vec<_>>();
    let required_masks = required_masks(&optional);
    debug_assert_eq!(required_masks.len(), mask_count);
    let owner = ir.classes[class_id as usize].fq_name_id();

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
    let mut body_stmts = Vec::with_capacity(named_fields.len());
    for (field, (field_name, field_ty)) in named_fields.iter().enumerate() {
        let element = elements.fields().iter().position(|&serial| serial == field);
        // `this` is slot 0; masks occupy 1..=mask_count; element arguments follow them.
        let store_argument = element.map(|element| {
            let argument = ir.add_expr(IrExpr::GetValue(element as u32 + 1 + mask_count as u32));
            if value_class_underlying(ir, field_ty).is_some() {
                // Unlike ordinary source-constructor parameters, this ABI slot carries the
                // value-class BOX. Publish that exact expression representation so JVM lowering
                // inserts `unbox-impl` at the erased backing-field store.
                ir.physical_types.insert(argument, *field_ty);
            }
            ir.add_expr(IrExpr::SetField {
                receiver: this,
                class: class_id,
                index: field as u32,
                value: argument,
            })
        });
        // An element takes its default only when it is optional; a transient property always takes
        // its initializer. A `lateinit` transient property has none and stays unset, as kotlinc
        // leaves it: the frontend rejects every other transient property without an initializer
        // (`serialization::transient_initializer`), so none reaches this constructor.
        let applies_default = match element {
            Some(element) => optional[element],
            None => !ir.classes[class_id as usize].fields[field].is_lateinit(),
        };
        // The default reads an earlier property off the object, which this constructor has
        // already stored, as kotlinc does. Its own locals move above the constructor's parameters
        // (the marker included).
        let default = applies_default.then(|| {
            let mut read_argument = |ir: &mut IrFile, field: usize| {
                let receiver = ir.add_expr(IrExpr::GetValue(0));
                Some(ir.add_expr(IrExpr::GetField {
                    receiver,
                    class: class_id,
                    index: u32::try_from(field).ok()?,
                }))
            };
            super::property_default::default_in_frame(
                ir,
                class_id,
                field,
                super::property_default::DefaultFrame {
                    first_free_local: u32::try_from(mask_count + element_fields.len() + 2)
                        .expect("a constructor's parameters fit u32"),
                    read_field: &mut read_argument,
                    receiver: Some(0),
                    property_line: None,
                    read_line: None,
                },
            )
            // A default reading a constructor parameter that is not a property has no argument
            // here. Keep an explicit plugin-owned residual so the file is declined with the
            // unsupported-IR diagnostic rather than built with a default that was never applied.
            .unwrap_or_else(|| {
                ir.add_expr(IrExpr::PluginPlaceholder {
                    plugin: "serialization",
                    kind: "deserialization-default",
                    exprs: Vec::new(),
                    data: vec![owner],
                    types: Vec::new(),
                })
            })
        });
        let (Some(default), Some(store_argument), Some(element)) =
            (default, store_argument, element)
        else {
            // At most one of the two exists: a required element, a transient property's
            // initializer, or nothing for a `lateinit` transient property.
            let store = store_argument.or_else(|| {
                let value = default?;
                Some(store_field_default(
                    ir, class_id, this, field, field_name, value,
                ))
            });
            body_stmts.extend(store);
            continue;
        };
        let seen = ir.add_expr(IrExpr::GetValue((element / 32 + 1) as u32));
        let bit = ir.add_expr(IrExpr::Const(IrConst::Int(
            1i32.wrapping_shl((element % 32) as u32),
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
        let store_default = store_field_default(ir, class_id, this, field, field_name, default);
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
    let owner_start_line = ir.classes[class_id as usize].decl_start_line;
    let ordinal = u32::try_from(ir.classes[class_id as usize].secondary_ctors.len())
        .expect("too many secondary constructors for an IR identity");
    ir.classes[class_id as usize]
        .secondary_ctors
        .push(IrSecondaryCtor {
            // Generated: its debug identity is `generated_debug`, not a source line.
            lines: crate::ir::IrSecondaryCtorLines::default(),
            annotations: DeclarationAnnotations::default(),
            source_order: u32::MAX,
            prefix_params: Vec::new(),
            vararg_index: None,
            params,
            named_params: (0..mask_count)
                .map(|word| (format!("seen{word}"), Ty::Int))
                .chain(element_fields.iter().cloned())
                .chain(std::iter::once((
                    MARKER_PARAMETER.to_string(),
                    Ty::nullable(class_ty(
                        "kotlinx/serialization/internal/SerializationConstructorMarker",
                    )),
                )))
                .collect(),
            metadata_visibility: Some(crate::types::Visibility::Internal),
            generated_debug: crate::ir::IrGeneratedDeclarationDebug::declaration_line(
                owner_start_line,
            ),
            defaults: vec![],
            delegate_prelude,
            delegate_args: vec![],
            default_parameters: Vec::new(),
            body: Some(body),
            delegate: CtorDelegateTarget::Super {
                owner: super_owner,
                target_params: vec![],
                to_primary: true,
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

/// Store property `field`'s default. kotlinc attributes the DEFAULT VALUE to the property's own
/// declaration line and the store that follows it back to the class's start line, so stepping
/// through the constructor lands on the property whose default is being applied. The value is a
/// nested expression and the store is a statement, which is why the two go through different maps.
fn store_field_default(
    ir: &mut IrFile,
    class_id: ClassId,
    this: u32,
    field: usize,
    field_name: &str,
    default: u32,
) -> u32 {
    let store = ir.add_expr(IrExpr::SetField {
        receiver: this,
        class: class_id,
        index: field as u32,
        value: default,
    });
    let owner = ir.classes[class_id as usize].fq_name_id();
    let property_line = ir
        .prop_decl_lines
        .get(&(owner, field_name.to_string()))
        .copied()
        .filter(|line| *line != 0);
    if let Some(line) = property_line {
        ir.expr_source_lines.insert(default, line);
        let start_line = ir.classes[class_id as usize].decl_start_line;
        if start_line != 0 {
            ir.expr_lines.insert(store, start_line);
        }
    }
    store
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
