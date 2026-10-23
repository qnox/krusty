//! The `serialize`/`deserialize` bodies of a `@JvmInline value class`'s generated serializer.
//!
//! A value class is written through the encoder its descriptor's `encodeInline` returns, and read
//! through `decodeInline`. A non-null primitive or `String` underlying value is coded directly
//! (`encodeInt`/`decodeInt`); any other underlying value — nullable, a value class, a collection —
//! is coded with its own element serializer through `encode[Nullable]SerializableValue` and
//! `decode[Nullable]SerializableValue`, as kotlinc does.

use super::declared_serializer::build_field_serializer_instance;
use super::deserialize_body::narrowed;
use super::element_serializer;
use super::type_argument_serializers::declared_type_spelling;
use super::type_parameter_serializers::TypeParameterSerializers;
use super::value_class_types::inline_prim_methods;
use super::{
    class_ty, complete_generated_body, declared_serializer_of, property_is_contextual,
    synthesized_accessor, ty_descriptor, virtual_iface,
};
use crate::ir::{ClassId, ExprId, IrConst, IrExpr, IrFile};
use crate::plugins::PluginContext;
use crate::types::Ty;

/// One value class's serializer member, with the class's single underlying property.
pub(super) struct ValueClassBody<'a> {
    pub(super) function: u32,
    pub(super) serializer_class: ClassId,
    pub(super) serialized_class: ClassId,
    pub(super) property: &'a (String, Ty),
    pub(super) type_parameter_serializers: TypeParameterSerializers<'a>,
}

/// How the underlying value crosses the inline encoder or decoder.
enum Coding {
    /// `encode<U>(value)` / `decode<U>()`.
    Direct {
        encode: (&'static str, &'static str),
        decode: (&'static str, &'static str),
    },
    /// `encode[Nullable]SerializableValue(serializer, value)` and its decode counterpart.
    Serializer { serializer: ExprId, nullable: bool },
}

/// Whether a property-selected serializer for a value class's underlying slot uses nullable calls.
///
/// Unlike an ordinary generic property, a value-class carrier is the representation itself. A bare
/// contextual type parameter whose upper bound admits null therefore uses kotlinc's nullable
/// value-coding path; an explicitly non-null bound does not. The default type-parameter serializer
/// path remains non-null unless the type occurrence itself is nullable.
fn carrier_is_nullable(ty: &Ty) -> bool {
    ty.is_nullable() || (ty.is_ty_param() && ty.upper_bound_admits_null())
}

impl ValueClassBody<'_> {
    fn coding(&self, ir: &mut IrFile, ctx: &PluginContext) -> Coding {
        let (name, ty) = self.property;
        let annotated = declared_type_spelling(ctx, ir, self.serialized_class, name);
        // A serializer the property names for itself takes precedence over its type's, as it does
        // for every other serializable property: what it wrote only it can read back.
        let selected = if property_is_contextual(ctx, ir, self.serialized_class, name) {
            Some(
                element_serializer::contextual_property_serializer(
                    ir,
                    ctx,
                    ty,
                    self.type_parameter_serializers,
                    &annotated,
                )
                .unwrap_or_else(|| element_serializer::unsupported_element_serializer(ir, *ty)),
            )
        } else {
            declared_serializer_of(ctx, ir, self.serialized_class, name).map(|serializer| {
                build_field_serializer_instance(
                    ir,
                    ctx,
                    serializer,
                    *ty,
                    self.type_parameter_serializers,
                    &annotated,
                )
            })
        };
        if let Some(serializer) = selected {
            return Coding::Serializer {
                serializer,
                nullable: carrier_is_nullable(ty),
            };
        }
        if !carrier_is_nullable(ty) {
            if let Some((enc_name, enc_desc, dec_name, dec_desc)) = inline_prim_methods(ty) {
                return Coding::Direct {
                    encode: (enc_name, enc_desc),
                    decode: (dec_name, dec_desc),
                };
            }
        }
        // The serializer of the NON-NULL type: nullability is the coding call's, not a
        // `.nullable` wrapper's.
        let element = ty.non_null();
        let serializer = element_serializer::element_serializer_expr_in(
            ir,
            ctx,
            &element,
            self.type_parameter_serializers,
            &annotated,
        )
        .unwrap_or_else(|| element_serializer::unsupported_element_serializer(ir, element));
        Coding::Serializer {
            serializer,
            nullable: ty.is_nullable(),
        }
    }

    fn descriptor(&self, ir: &mut IrFile) -> ExprId {
        let this = ir.add_expr(IrExpr::GetValue(0));
        ir.add_expr(IrExpr::GetField {
            receiver: this,
            class: self.serializer_class,
            index: 0,
        })
    }

    /// `encoder.encodeInline(descriptor)?.encode…(value.<property>)`, its implicit return on the
    /// class header's line as in every other generated `serialize`.
    pub(super) fn serialize(self, ir: &mut IrFile, ctx: &PluginContext) {
        let (name, ty) = self.property;
        let Some(getter_descriptor) = ty_descriptor(ctx, ty) else {
            return;
        };
        let descriptor = self.descriptor(ir);
        let encoder = ir.add_expr(IrExpr::GetValue(1));
        let inline_encoder = ir.add_expr(IrExpr::Call {
            callee: Box::new(virtual_iface(
                "kotlinx/serialization/encoding/Encoder",
                "encodeInline",
                "(Lkotlinx/serialization/descriptors/SerialDescriptor;)Lkotlinx/serialization/encoding/Encoder;",
            )),
            dispatch_receiver: Some(encoder),
            args: vec![descriptor],
        });
        let coding = self.coding(ir, ctx);
        let value = ir.add_expr(IrExpr::GetValue(2));
        let owner = ir.classes[self.serialized_class as usize].fq_name_id();
        let value = synthesized_accessor::getter(
            ir,
            value,
            owner,
            name,
            getter_descriptor,
            (self.serialized_class, 0),
            None,
        );
        // kotlinc calls the encoder `encodeInline` returns through a safe call.
        let temporary = 3;
        let declared = ir.add_expr(IrExpr::Variable {
            index: temporary,
            ty: Ty::nullable(class_ty("kotlinx/serialization/encoding/Encoder")),
            init: Some(inline_encoder),
            named: false,
        });
        let tested = ir.add_expr(IrExpr::GetValue(temporary));
        let null = ir.add_expr(IrExpr::Const(IrConst::Null));
        let absent = ir.add_expr(IrExpr::PrimitiveBinOp {
            op: crate::ir::IrBinOp::Eq,
            lhs: tested,
            rhs: null,
        });
        let receiver = ir.add_expr(IrExpr::GetValue(temporary));
        let (method, method_descriptor, args) = match coding {
            Coding::Direct {
                encode: (method, method_descriptor),
                ..
            } => (method, method_descriptor, vec![value]),
            Coding::Serializer {
                serializer,
                nullable,
            } => {
                let serializer = narrowed(
                    ir,
                    serializer,
                    "kotlinx/serialization/SerializationStrategy",
                );
                let method = if nullable {
                    "encodeNullableSerializableValue"
                } else {
                    "encodeSerializableValue"
                };
                (
                    method,
                    "(Lkotlinx/serialization/SerializationStrategy;Ljava/lang/Object;)V",
                    vec![serializer, value],
                )
            }
        };
        let encode = ir.add_expr(IrExpr::Call {
            callee: Box::new(virtual_iface(
                "kotlinx/serialization/encoding/Encoder",
                method,
                method_descriptor,
            )),
            dispatch_receiver: Some(receiver),
            args,
        });
        let null_result = ir.add_expr(IrExpr::UnitInstance);
        let guarded = ir.add_expr(IrExpr::When {
            branches: vec![(Some(absent), null_result), (None, encode)],
        });
        ir.whens.exhaustive.insert(guarded, Ty::Unit);
        ir.logical_types.insert(guarded, Ty::Unit);
        let call = ir.add_expr(IrExpr::Block {
            stmts: vec![declared],
            value: Some(guarded),
        });
        ir.logical_types.insert(call, Ty::Unit);
        let class = &ir.classes[self.serialized_class as usize];
        let (start_line, header_line) = (class.decl_start_line, class.decl_line);
        // On a one-line declaration the implicit return is already on that line.
        if header_line != 0 && header_line != start_line {
            ir.fn_close_lines.insert(self.function, header_line);
        }
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![call],
            value: None,
        });
        complete_generated_body(ir, self.function, body);
    }

    /// `return V(decoder.decodeInline(descriptor).decode…())`.
    pub(super) fn deserialize(self, ir: &mut IrFile, ctx: &PluginContext) {
        let (_, ty) = self.property;
        let descriptor = self.descriptor(ir);
        let decoder = ir.add_expr(IrExpr::GetValue(1));
        let inline_decoder = ir.add_expr(IrExpr::Call {
            callee: Box::new(virtual_iface(
                "kotlinx/serialization/encoding/Decoder",
                "decodeInline",
                "(Lkotlinx/serialization/descriptors/SerialDescriptor;)Lkotlinx/serialization/encoding/Decoder;",
            )),
            dispatch_receiver: Some(decoder),
            args: vec![descriptor],
        });
        let underlying = match self.coding(ir, ctx) {
            Coding::Direct {
                decode: (method, method_descriptor),
                ..
            } => ir.add_expr(IrExpr::Call {
                callee: Box::new(virtual_iface(
                    "kotlinx/serialization/encoding/Decoder",
                    method,
                    method_descriptor,
                )),
                dispatch_receiver: Some(inline_decoder),
                args: vec![],
            }),
            Coding::Serializer {
                serializer,
                nullable,
            } => {
                let serializer = narrowed(
                    ir,
                    serializer,
                    "kotlinx/serialization/DeserializationStrategy",
                );
                let method = if nullable {
                    "decodeNullableSerializableValue"
                } else {
                    "decodeSerializableValue"
                };
                let decoded = ir.add_expr(IrExpr::Call {
                    callee: Box::new(virtual_iface(
                        "kotlinx/serialization/encoding/Decoder",
                        method,
                        "(Lkotlinx/serialization/DeserializationStrategy;)Ljava/lang/Object;",
                    )),
                    dispatch_receiver: Some(inline_decoder),
                    args: vec![serializer],
                });
                // A scalar underlying value (`UInt`) is decoded as its box.
                let decoded_ty = if ty.is_jvm_scalar() {
                    Ty::nullable(*ty)
                } else {
                    *ty
                };
                ir.add_expr(IrExpr::TypeOp {
                    op: crate::ir::IrTypeOp::Cast,
                    arg: decoded,
                    type_operand: decoded_ty,
                })
            }
        };
        let internal = ir.classes[self.serialized_class as usize].fq_name_id();
        let value = ir.add_expr(IrExpr::New {
            internal,
            args: vec![underlying],
            ctor_params: None,
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: 0,
        });
        let ret = ir.add_expr(IrExpr::Return(Some(value)));
        let body = ir.add_expr(IrExpr::Block {
            stmts: vec![ret],
            value: None,
        });
        complete_generated_body(ir, self.function, body);
    }
}

#[cfg(test)]
mod tests {
    use super::carrier_is_nullable;
    use crate::types::Ty;

    #[test]
    fn carrier_nullability_distinguishes_a_nullable_bound_from_a_nullable_occurrence() {
        let nullable_bound = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        let non_null_bound = Ty::ty_param("N", Ty::obj("kotlin/Any"));

        assert!(carrier_is_nullable(&nullable_bound));
        assert!(!carrier_is_nullable(&non_null_bound));
        assert!(carrier_is_nullable(&Ty::nullable(non_null_bound)));
    }
}
