//! Checked generation of a serializer's `deserialize` body.

use super::{
    builtin_element_serializer, can_derive_element_serializer, class_ty,
    collection_serializer_builder, contextual_serializer_for, decode_element_method,
    element_serializer_expr, inline_prim_methods, is_nullable, property_is_contextual, slot_width,
    value_class_underlying, virtual_iface,
};
use crate::ir::{ClassId, ExprId, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::kt_string::KtString;
use crate::plugins::PluginContext;
use crate::types::Ty;

pub(super) struct DeserializeBody<'a> {
    pub(super) function: u32,
    pub(super) serializer_class: ClassId,
    pub(super) serialized_class: ClassId,
    pub(super) fields: &'a [(String, Ty)],
    pub(super) nested_serializers: &'a [Option<ClassId>],
    pub(super) type_parameter_serializer_fields: &'a [Option<u32>],
}

impl DeserializeBody<'_> {
    pub(super) fn generate(self, ir: &mut IrFile, ctx: &PluginContext) {
        let Self {
            function: fid,
            serializer_class,
            serialized_class: foo_id,
            fields,
            nested_serializers: nested,
            type_parameter_serializer_fields: tp_field,
        } = self;
        let ser_idx = serializer_class as usize;
        let class_id = foo_id;
        if ir.classes[foo_id as usize].is_value
            && fields
                .first()
                .and_then(|(_, ty)| inline_prim_methods(ty))
                .is_some()
        {
            // A `@JvmInline value class`: deserialize inline —
            //   return new Foo(decoder.decodeInline(this.descriptor).decode<U>())
            let uty = fields[0].1;
            let (_, _, dec_name, dec_desc) = inline_prim_methods(&uty).unwrap();
            let this = ir.add_expr(IrExpr::GetValue(0));
            let desc = ir.add_expr(IrExpr::GetField {
                receiver: this,
                class: ser_idx as u32,
                index: 0,
            });
            let dec = ir.add_expr(IrExpr::GetValue(1));
            let inline_dec = ir.add_expr(IrExpr::Call {
                callee: virtual_iface(
                    "kotlinx/serialization/encoding/Decoder",
                    "decodeInline",
                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)Lkotlinx/serialization/encoding/Decoder;",
                ),
                dispatch_receiver: Some(dec),
                args: vec![desc],
            });
            let u = ir.add_expr(IrExpr::Call {
                callee: virtual_iface("kotlinx/serialization/encoding/Decoder", dec_name, dec_desc),
                dispatch_receiver: Some(inline_dec),
                args: vec![],
            });
            let foo_internal = ir.classes[foo_id as usize].fq_name_id();
            let new = ir.add_expr(IrExpr::New {
                internal: foo_internal,
                args: vec![u],
                ctor_params: None,
                ctor_desc: None,
                external_target: None,
                defaults: Box::new([]),
                default_prefix_count: 0,
            });
            let ret = ir.add_expr(IrExpr::Return(Some(new)));
            let body = ir.add_expr(IrExpr::Block {
                stmts: vec![ret],
                value: None,
            });
            ir.functions[fid as usize].body = Some(body);
            return;
        }

        // deserialize(decoder=1):
        //   val c = decoder.beginStructure(descriptor)            [local 2]
        //   var f0 = <default>; var f1 = <default>                 [locals 4, 5, ...]
        //   loop@ while (true) {
        //       val i = c.decodeElementIndex(descriptor)           [local 3]
        //       when (i) { -1 -> break@loop; 0 -> f0 = c.decode<T>Element(d,0); ... }
        //   }
        //   c.endStructure(descriptor); return Foo(seen..., f0, f1, null)
        // Unsupported element serializers receive an explicit throwing body; a
        // body-less method would make the generated serializer abstract and fail
        // while obtaining its descriptor, before deserialization is requested.
        let ser_cid = ser_idx as u32;
        if ir.classes[foo_id as usize].is_object {
            let singleton = ir.add_expr(IrExpr::StaticInstance {
                owner: foo_id,
                ty: foo_id,
                field: "INSTANCE",
            });
            let ret = ir.add_expr(IrExpr::Return(Some(singleton)));
            let body = ir.add_expr(IrExpr::Block {
                stmts: vec![ret],
                value: None,
            });
            ir.functions[fid as usize].body = Some(body);
            return;
        }
        let decodable = fields.iter().enumerate().all(|(i, (pname, t))| {
            if tp_field[i].is_some() || property_is_contextual(ctx, ir, class_id, pname) {
                return true;
            }
            // A nested @Serializable element is decodable only if its serializer is
            // actually derivable (a generic field with an un-derivable type arg is not) —
            // else deserialize stubs cleanly rather than emit a `null` element serializer.
            if nested[i].is_some() {
                return can_derive_element_serializer(ir, t);
            }
            // A standard collection field decodes through its builtin collection serializer
            // (via the `element_serializer_expr` fallback below) when its elements derive.
            if t.non_null()
                .obj_internal()
                .and_then(collection_serializer_builder)
                .is_some()
            {
                return can_derive_element_serializer(ir, t);
            }
            if is_nullable(t) {
                return builtin_element_serializer(t).is_some();
            }
            decode_element_method(t).is_some()
        });
        if !decodable {
            let message = ir.add_expr(IrExpr::Const(IrConst::String(KtString::from(
                "no serializer is available for a deserialized field type",
            ))));
            let exception = ir.new_external(
                "kotlinx/serialization/SerializationException",
                "(Ljava/lang/String;)V",
                vec![message],
            );
            let throw = ir.add_expr(IrExpr::Throw { operand: exception });
            let body = ir.add_expr(IrExpr::Block {
                stmts: vec![throw],
                value: None,
            });
            ir.functions[fid as usize].body = Some(body);
            return;
        }
        // Field-local slot for each property — `this`=0, decoder=1, c=2, i=3, then the
        // field locals from slot 4, advancing by each type's JVM width (Long/Double=2).
        let mut slots: Vec<u32> = Vec::with_capacity(fields.len());
        let mut next = 4u32;
        for (_, ty) in fields {
            slots.push(next);
            next += slot_width(ty);
        }
        let mask_count = fields.len() / 32 + 1;
        // Mask locals follow the field locals. The producer records the exact
        // deserialization constructor identity; consuming `synthetic` or arity would
        // accidentally select an unrelated generated constructor.
        let seen_slots = (0..mask_count)
            .map(|word| next + word as u32)
            .collect::<Vec<_>>();
        let constructor = ir
            .generated_secondary_constructor(
                foo_id,
                crate::ir::IrSecondaryConstructorRole::SerializationDeserialization,
            )
            .expect("a plain serializable class has its deserialization constructor");
        let synthetic_ctor_params = ir.classes[foo_id as usize].secondary_ctors
            [constructor as usize]
            .params
            .clone();
        let marker_count = synthetic_ctor_params
            .len()
            .checked_sub(mask_count + fields.len())
            .expect("deserialization constructor retains its mask and field parameters");
        assert!(
            (1..=2).contains(&marker_count),
            "deserialization constructor retains its marker parameter(s)"
        );
        let this_desc = |ir: &mut IrFile| -> ExprId {
            let r = ir.add_expr(IrExpr::GetValue(0));
            ir.add_expr(IrExpr::GetField {
                receiver: r,
                class: ser_cid,
                index: 0,
            })
        };
        let body = {
            let d0 = this_desc(ir);
            let dec = ir.add_expr(IrExpr::GetValue(1));
            let begin = ir.add_expr(IrExpr::Call {
                callee: virtual_iface(
                    "kotlinx/serialization/encoding/Decoder",
                    "beginStructure",
                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)Lkotlinx/serialization/encoding/CompositeDecoder;",
                ),
                dispatch_receiver: Some(dec),
                args: vec![d0],
            });
            let cvar = ir.add_expr(IrExpr::Variable {
                index: 2,
                ty: class_ty("kotlinx/serialization/encoding/CompositeDecoder"),
                init: Some(begin),
                named: false,
            });
            let mut stmts = vec![cvar];
            // index local (3) — declared before the field locals so emit's slot order
            // (by declaration) matches the explicit indices (this=0, decoder=1, c=2,
            // i=3, f0=4, f1=5, …).
            let izero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
            stmts.push(ir.add_expr(IrExpr::Variable {
                index: 3,
                ty: class_ty("kotlin/Int"),
                init: Some(izero),
                named: false,
            }));
            // Field locals start at their JVM zero. Optional defaults belong solely to
            // the synthetic constructor and must not be evaluated before decoding.
            for (k, (_, ty)) in fields.iter().enumerate() {
                let dc = IrConst::zero_for_value_type(*ty);
                let init = ir.add_expr(IrExpr::Const(dc));
                stmts.push(ir.add_expr(IrExpr::Variable {
                    index: slots[k],
                    ty: *ty,
                    init: Some(init),
                    named: false,
                }));
            }
            for &seen_slot in &seen_slots {
                let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                stmts.push(ir.add_expr(IrExpr::Variable {
                    index: seen_slot,
                    ty: class_ty("kotlin/Int"),
                    init: Some(zero),
                    named: false,
                }));
            }
            // loop body: i = c.decodeElementIndex(desc); when(i){…}
            let didx = this_desc(ir);
            let cdi = ir.add_expr(IrExpr::GetValue(2));
            let dei = ir.add_expr(IrExpr::Call {
                callee: virtual_iface(
                    "kotlinx/serialization/encoding/CompositeDecoder",
                    "decodeElementIndex",
                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)I",
                ),
                dispatch_receiver: Some(cdi),
                args: vec![didx],
            });
            let set_i = ir.add_expr(IrExpr::SetValue { var: 3, value: dei });
            // A sequence of single-branch `if` statements (each a Unit statement — no
            // value mixing): `if (i == -1) break`, then `if (i == k) f_k = decode…`.
            let mut loop_stmts = vec![set_i];
            let iref = ir.add_expr(IrExpr::GetValue(3));
            let neg1 = ir.add_expr(IrExpr::Const(IrConst::Int(-1)));
            let is_done = ir.add_expr(IrExpr::PrimitiveBinOp {
                op: crate::ir::IrBinOp::Eq,
                lhs: iref,
                rhs: neg1,
            });
            let brk = ir.add_expr(IrExpr::Break {
                label: Some("deser".to_string()),
            });
            let brk_blk = ir.add_expr(IrExpr::Block {
                stmts: vec![brk],
                value: None,
            });
            loop_stmts.push(ir.add_expr(IrExpr::When {
                branches: vec![(Some(is_done), brk_blk)],
            }));
            for (k, (_, ty)) in fields.iter().enumerate() {
                let iref = ir.add_expr(IrExpr::GetValue(3));
                let kc = ir.add_expr(IrExpr::Const(IrConst::Int(k as i32)));
                let is_k = ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: crate::ir::IrBinOp::Eq,
                    lhs: iref,
                    rhs: kc,
                });
                let dk = this_desc(ir);
                let idxc = ir.add_expr(IrExpr::Const(IrConst::Int(k as i32)));
                let cdk = ir.add_expr(IrExpr::GetValue(2));
                let decoded = if let Some(inst) = contextual_serializer_for(
                    ir,
                    property_is_contextual(ctx, ir, class_id, &fields[k].0),
                    ty,
                ) {
                    // Contextual element: f_k = (T) c.decode[Nullable]SerializableElement(
                    // desc, k, ContextualSerializer(<type>::class), null).
                    let prev = ir.add_expr(IrExpr::Const(IrConst::Null));
                    let method = if is_nullable(ty) {
                        "decodeNullableSerializableElement"
                    } else {
                        "decodeSerializableElement"
                    };
                    let raw = ir.add_expr(IrExpr::Call {
                        callee: virtual_iface(
                            "kotlinx/serialization/encoding/CompositeDecoder",
                            method,
                            "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/DeserializationStrategy;Ljava/lang/Object;)Ljava/lang/Object;",
                        ),
                        dispatch_receiver: Some(cdk),
                        args: vec![dk, idxc, inst, prev],
                    });
                    ir.add_expr(IrExpr::TypeOp {
                        op: IrTypeOp::Cast,
                        arg: raw,
                        type_operand: *ty,
                    })
                } else if let Some(fidx) = tp_field[k] {
                    // f_k = (T) c.decode[Nullable]SerializableElement(desc, k,
                    // this.typeSerialK, null) — the ctor-supplied type-param serializer.
                    let this_s = ir.add_expr(IrExpr::GetValue(0));
                    let inst = ir.add_expr(IrExpr::GetField {
                        receiver: this_s,
                        class: ser_cid,
                        index: fidx,
                    });
                    let prev = ir.add_expr(IrExpr::Const(IrConst::Null));
                    let method = if is_nullable(ty) {
                        "decodeNullableSerializableElement"
                    } else {
                        "decodeSerializableElement"
                    };
                    let raw = ir.add_expr(IrExpr::Call {
                        callee: virtual_iface(
                            "kotlinx/serialization/encoding/CompositeDecoder",
                            method,
                            "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/DeserializationStrategy;Ljava/lang/Object;)Ljava/lang/Object;",
                        ),
                        dispatch_receiver: Some(cdk),
                        args: vec![dk, idxc, inst, prev],
                    });
                    ir.add_expr(IrExpr::TypeOp {
                        op: IrTypeOp::Cast,
                        arg: raw,
                        type_operand: *ty,
                    })
                } else if nested[k].is_some()
                    || ty
                        .non_null()
                        .obj_internal()
                        .and_then(collection_serializer_builder)
                        .is_some()
                {
                    // f_k = (T) c.decode[Nullable]SerializableElement(desc, k,
                    // <element serializer>, null) — the nested `$serializer.INSTANCE`
                    // (non-generic) / `Foo.serializer(A_ser)` (generic) / `ListSerializer(…)`
                    // (collection). Same descriptor; the nullable variant yields null for a
                    // JSON-null element.
                    let inst = element_serializer_expr(ir, ty)
                        .unwrap_or_else(|| ir.add_expr(IrExpr::Const(IrConst::Null)));
                    let prev = ir.add_expr(IrExpr::Const(IrConst::Null));
                    let method = if is_nullable(ty) {
                        "decodeNullableSerializableElement"
                    } else {
                        "decodeSerializableElement"
                    };
                    let raw = ir.add_expr(IrExpr::Call {
                        callee: virtual_iface(
                            "kotlinx/serialization/encoding/CompositeDecoder",
                            method,
                            "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/DeserializationStrategy;Ljava/lang/Object;)Ljava/lang/Object;",
                        ),
                        dispatch_receiver: Some(cdk),
                        args: vec![dk, idxc, inst, prev],
                    });
                    ir.add_expr(IrExpr::TypeOp {
                        op: IrTypeOp::Cast,
                        arg: raw,
                        type_operand: *ty,
                    })
                } else if is_nullable(ty) {
                    // f_k = (T) c.decodeNullableSerializableElement(desc, k,
                    // <Elem>Serializer.INSTANCE, null) — yields the element or null.
                    let ser = builtin_element_serializer(ty).unwrap();
                    let inst = ir.external_static_instance(ser, ser, "INSTANCE");
                    let prev = ir.add_expr(IrExpr::Const(IrConst::Null));
                    let raw = ir.add_expr(IrExpr::Call {
                        callee: virtual_iface(
                            "kotlinx/serialization/encoding/CompositeDecoder",
                            "decodeNullableSerializableElement",
                            "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/DeserializationStrategy;Ljava/lang/Object;)Ljava/lang/Object;",
                        ),
                        dispatch_receiver: Some(cdk),
                        args: vec![dk, idxc, inst, prev],
                    });
                    ir.add_expr(IrExpr::TypeOp {
                        op: IrTypeOp::Cast,
                        arg: raw,
                        type_operand: *ty,
                    })
                } else {
                    let (mname, mdesc) = decode_element_method(ty).unwrap();
                    ir.add_expr(IrExpr::Call {
                        callee: virtual_iface(
                            "kotlinx/serialization/encoding/CompositeDecoder",
                            mname,
                            mdesc,
                        ),
                        dispatch_receiver: Some(cdk),
                        args: vec![dk, idxc],
                    })
                };
                let setk = ir.add_expr(IrExpr::SetValue {
                    var: slots[k],
                    value: decoded,
                });
                let mut decoded_stmts = vec![setk];
                {
                    // `seen[word] = seen[word] or bit` — the element arrived.
                    let seen_slot = seen_slots[k / 32];
                    let seen = ir.add_expr(IrExpr::GetValue(seen_slot));
                    let bit = ir.add_expr(IrExpr::Const(IrConst::Int(
                        1i32.wrapping_shl((k % 32) as u32),
                    )));
                    let marked = ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: crate::ir::IrBinOp::BitOr,
                        lhs: seen,
                        rhs: bit,
                    });
                    decoded_stmts.push(ir.add_expr(IrExpr::SetValue {
                        var: seen_slot,
                        value: marked,
                    }));
                }
                let setk_blk = ir.add_expr(IrExpr::Block {
                    stmts: decoded_stmts,
                    value: None,
                });
                loop_stmts.push(ir.add_expr(IrExpr::When {
                    branches: vec![(Some(is_k), setk_blk)],
                }));
            }
            let loop_body = ir.add_expr(IrExpr::Block {
                stmts: loop_stmts,
                value: None,
            });
            let cond = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
            let whilexpr = ir.add_expr(IrExpr::While {
                cond,
                body: loop_body,
                update: None,
                post_test: false,
                label: Some("deser".to_string()),
            });
            stmts.push(whilexpr);
            // endStructure
            let dend = this_desc(ir);
            let cend = ir.add_expr(IrExpr::GetValue(2));
            stmts.push(ir.add_expr(IrExpr::Call {
                callee: virtual_iface(
                    "kotlinx/serialization/encoding/CompositeDecoder",
                    "endStructure",
                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)V",
                ),
                dispatch_receiver: Some(cend),
                args: vec![dend],
            }));
            // return Foo(seen..., f0, f1, …, null...) through the exact synthetic
            // deserialization
            // constructor — it is what turns an absent required element into
            // `MissingFieldException` and an absent optional one into its default.
            let mut args: Vec<ExprId> = Vec::with_capacity(synthetic_ctor_params.len());
            for &seen_slot in &seen_slots {
                args.push(ir.add_expr(IrExpr::GetValue(seen_slot)));
            }
            for (index, &slot) in slots.iter().enumerate() {
                let argument = ir.add_expr(IrExpr::GetValue(slot));
                let parameter = synthetic_ctor_params[mask_count + index];
                if value_class_underlying(ir, &parameter).is_some() {
                    // The decode local carries the unboxed underlying while this exact
                    // serialization ABI parameter is boxed. Preserve both facts for
                    // target representation lowering.
                    ir.logical_types.insert(argument, parameter);
                    ir.physical_types.insert(argument, fields[index].1);
                }
                args.push(argument);
            }
            for _ in 0..marker_count {
                args.push(ir.add_expr(IrExpr::Const(IrConst::Null)));
            }
            let foo_internal = ir.classes[foo_id as usize].fq_name_id();
            let new = ir.add_expr(IrExpr::New {
                internal: foo_internal,
                args,
                ctor_params: Some(synthetic_ctor_params.clone()),
                ctor_desc: None,
                external_target: None,
                defaults: Box::new([]),
                default_prefix_count: 0,
            });
            ir.record_generated_secondary_constructor_call(
                new,
                foo_id,
                crate::ir::IrSecondaryConstructorRole::SerializationDeserialization,
                constructor,
            );
            stmts.push(ir.add_expr(IrExpr::Return(Some(new))));
            ir.add_expr(IrExpr::Block { stmts, value: None })
        };
        ir.functions[fid as usize].body = Some(body);
    }
}
