//! Checked generation of a serializer's `deserialize` body.

use super::element_serializer::always_available_builtin_serializer;
use super::{
    class_ty, collection_serializer_builder, contextual_serializer_for, decode_element_method,
    element_serializer_expr, element_serializer_plan, inline_prim_methods, is_nullable,
    property_is_contextual, slot_width, value_class_underlying, virtual_iface,
};
use crate::ir::{ClassId, ExprId, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::kt_string::KtString;
use crate::plugins::PluginContext;
use crate::types::Ty;

/// `this` is 0 and the decoder 1; the descriptor, the loop flag and the element index follow, in
/// kotlinc's order. The seen-mask words, the field locals and the composite decoder come after,
/// their count depending on the class.
const SERIAL_DESC_SLOT: u32 = 2;
const FLAG_SLOT: u32 = 3;
const INDEX_SLOT: u32 = 4;

/// One element's decode and the seen-mask bit that records it arrived — the same block on the
/// sequential fast path and in the index-driven loop, so it is built once and used twice.
struct ElementDecode<'a> {
    class_id: ClassId,
    ser_cid: u32,
    fields: &'a [(String, Ty)],
    nested: &'a [Option<ClassId>],
    tp_field: &'a [Option<u32>],
    slots: &'a [u32],
    seen_slots: &'a [u32],
    composite_slot: u32,
}

impl ElementDecode<'_> {
    fn block(&self, ir: &mut IrFile, ctx: &PluginContext, k: usize) -> ExprId {
        let ty = self.fields[k].1;
        let dk = ir.add_expr(IrExpr::GetValue(SERIAL_DESC_SLOT));
        let idxc = ir.add_expr(IrExpr::Const(IrConst::Int(k as i32)));
        let cdk = ir.add_expr(IrExpr::GetValue(self.composite_slot));
        let decoded = if let Some(inst) = contextual_serializer_for(
            ir,
            property_is_contextual(ctx, ir, self.class_id, &self.fields[k].0),
            &ty,
        ) {
            // Contextual element: f_k = (T) c.decode[Nullable]SerializableElement(
            // desc, k, ContextualSerializer(<type>::class), null).
            let prev = ir.add_expr(IrExpr::Const(IrConst::Null));
            let method = if is_nullable(&ty) {
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
                type_operand: ty,
            })
        } else if let Some(fidx) = self.tp_field[k] {
            // f_k = (T) c.decode[Nullable]SerializableElement(desc, k,
            // this.typeSerialK, null) — the ctor-supplied type-param serializer.
            let this_s = ir.add_expr(IrExpr::GetValue(0));
            let inst = ir.add_expr(IrExpr::GetField {
                receiver: this_s,
                class: self.ser_cid,
                index: fidx,
            });
            let prev = ir.add_expr(IrExpr::Const(IrConst::Null));
            let method = if is_nullable(&ty) {
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
                type_operand: ty,
            })
        } else if is_nullable(&ty) || decode_element_method(&ty).is_none() {
            // f_k = (T) c.decode[Nullable]SerializableElement(desc, k,
            // <element serializer>, null) — the nested `$serializer.INSTANCE`
            // (non-generic) / `Foo.serializer(A_ser)` (generic) / `ListSerializer(…)`
            // (collection). Same descriptor; the nullable variant yields null for a
            // JSON-null element.
            let inst = element_serializer_expr(ir, ctx, &ty)
                .unwrap_or_else(|| ir.add_expr(IrExpr::Const(IrConst::Null)));
            let prev = ir.add_expr(IrExpr::Const(IrConst::Null));
            let method = if is_nullable(&ty) {
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
                type_operand: ty,
            })
        } else if is_nullable(&ty) {
            // f_k = (T) c.decodeNullableSerializableElement(desc, k,
            // <Elem>Serializer.INSTANCE, null) — yields the element or null.
            let serializer = always_available_builtin_serializer(&ty).unwrap();
            let inst = ir.add_expr(IrExpr::ExternalStaticInstance {
                owner: serializer,
                ty: serializer,
                field: "INSTANCE".to_string(),
            });
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
                type_operand: ty,
            })
        } else {
            let (mname, mdesc) = decode_element_method(&ty).unwrap();
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
            var: self.slots[k],
            value: decoded,
        });
        let mut decoded_stmts = vec![setk];
        {
            // `seen[word] = seen[word] or bit` — the element arrived.
            let seen_slot = self.seen_slots[k / 32];
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
        setk_blk
    }
}

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
                return element_serializer_plan(ir, ctx, t).is_some();
            }
            // A standard collection field decodes through its builtin collection serializer
            // (via the `element_serializer_expr` fallback below) when its elements derive.
            if t.non_null()
                .obj_internal()
                .and_then(collection_serializer_builder)
                .is_some()
            {
                return element_serializer_plan(ir, ctx, t).is_some();
            }
            // Everything else decodes either as a PRIMITIVE through its own `decode<T>Element`, or
            // through an element serializer — the same one `serialize` and `childSerializers` use. A
            // nullable element always takes the serializer path
            // (`decodeNullableSerializableElement`), which is why its builtin is not the only way to
            // decode it: a nullable nested class, enum or collection has no builtin at all.
            if !is_nullable(t) && decode_element_method(t).is_some() {
                return true;
            }
            element_serializer_plan(ir, ctx, t).is_some()
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
        } // Local layout, in kotlinc's order: the descriptor is read ONCE into a local, the loop is
          // driven by a `flag` the `-1` case clears (rather than a `break`), and the element index
          // has NO initializer — only the loop assigns it. The composite decoder comes LAST, after
          // the masks and the field locals, because `beginStructure` runs after they are zeroed.
        let mask_count = fields.len() / 32 + 1;
        // The producer records the exact deserialization constructor identity; consuming
        // `synthetic` or arity would accidentally select an unrelated generated constructor.
        let seen_slots = (0..mask_count)
            .map(|word| INDEX_SLOT + 1 + word as u32)
            .collect::<Vec<_>>();
        let mut slots: Vec<u32> = Vec::with_capacity(fields.len());
        let mut next = INDEX_SLOT + 1 + mask_count as u32;
        for (_, ty) in fields {
            slots.push(next);
            next += slot_width(ty);
        }
        let composite_slot = next;
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
        // Every use of the descriptor and of the composite decoder reads its LOCAL.
        let this_desc =
            |ir: &mut IrFile| -> ExprId { ir.add_expr(IrExpr::GetValue(SERIAL_DESC_SLOT)) };
        let composite =
            |ir: &mut IrFile| -> ExprId { ir.add_expr(IrExpr::GetValue(composite_slot)) };
        let elements = ElementDecode {
            class_id,
            ser_cid,
            fields,
            nested,
            tp_field,
            slots: &slots,
            seen_slots: &seen_slots,
            composite_slot,
        };
        let body = {
            let this0 = ir.add_expr(IrExpr::GetValue(0));
            let descriptor_field = ir.add_expr(IrExpr::GetField {
                receiver: this0,
                class: ser_cid,
                index: 0,
            });
            let mut stmts = vec![ir.add_expr(IrExpr::Variable {
                index: SERIAL_DESC_SLOT,
                ty: class_ty("kotlinx/serialization/descriptors/SerialDescriptor"),
                init: Some(descriptor_field),
                named: false,
            })];
            let flag_init = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
            stmts.push(ir.add_expr(IrExpr::Variable {
                index: FLAG_SLOT,
                ty: Ty::Boolean,
                init: Some(flag_init),
                named: false,
            }));
            // Declared, never initialized: the element index exists only inside the loop, and the
            // verifier types its slot `top` until the loop's first store.
            stmts.push(ir.add_expr(IrExpr::Variable {
                index: INDEX_SLOT,
                ty: class_ty("kotlin/Int"),
                init: None,
                named: false,
            }));
            for &seen_slot in &seen_slots {
                let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                stmts.push(ir.add_expr(IrExpr::Variable {
                    index: seen_slot,
                    ty: class_ty("kotlin/Int"),
                    init: Some(zero),
                    named: false,
                }));
            }
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
            stmts.push(ir.add_expr(IrExpr::Variable {
                index: composite_slot,
                ty: class_ty("kotlinx/serialization/encoding/CompositeDecoder"),
                init: Some(begin),
                named: false,
            }));
            // loop body: index = composite.decodeElementIndex(serialDesc); when (index) { … }
            let didx = this_desc(ir);
            let cdi = composite(ir);
            let dei = ir.add_expr(IrExpr::Call {
                callee: virtual_iface(
                    "kotlinx/serialization/encoding/CompositeDecoder",
                    "decodeElementIndex",
                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)I",
                ),
                dispatch_receiver: Some(cdi),
                args: vec![didx],
            });
            let set_i = ir.add_expr(IrExpr::SetValue {
                var: INDEX_SLOT,
                value: dei,
            });
            // ONE `when` over the index, not a chain of `if`s: every branch compares the same
            // `Int` local against a distinct constant, which is the shape the emitter turns into
            // kotlinc's `tableswitch`. `-1` clears the loop flag instead of breaking, so the loop
            // exits through its own condition; an index that names no element is an
            // `UnknownFieldException`, which krusty previously ignored in silence.
            let iref = ir.add_expr(IrExpr::GetValue(INDEX_SLOT));
            let neg1 = ir.add_expr(IrExpr::Const(IrConst::Int(-1)));
            let is_done = ir.add_expr(IrExpr::PrimitiveBinOp {
                op: crate::ir::IrBinOp::Eq,
                lhs: iref,
                rhs: neg1,
            });
            let stop = ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
            let clear_flag = ir.add_expr(IrExpr::SetValue {
                var: FLAG_SLOT,
                value: stop,
            });
            let done_blk = ir.add_expr(IrExpr::Block {
                stmts: vec![clear_flag],
                value: None,
            });
            let mut branches = vec![(Some(is_done), done_blk)];
            for (k, (_, ty)) in fields.iter().enumerate() {
                let iref = ir.add_expr(IrExpr::GetValue(INDEX_SLOT));
                let kc = ir.add_expr(IrExpr::Const(IrConst::Int(k as i32)));
                let is_k = ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: crate::ir::IrBinOp::Eq,
                    lhs: iref,
                    rhs: kc,
                });
                let setk_blk = elements.block(ir, ctx, k);
                branches.push((Some(is_k), setk_blk));
            }
            // The `else` of that `when`: an index naming no element of this descriptor.
            let unknown_index = ir.add_expr(IrExpr::GetValue(INDEX_SLOT));
            let unknown = ir.new_external(
                "kotlinx/serialization/UnknownFieldException",
                "(I)V",
                vec![unknown_index],
            );
            let raise = ir.add_expr(IrExpr::Throw { operand: unknown });
            let raise_blk = ir.add_expr(IrExpr::Block {
                stmts: vec![raise],
                value: None,
            });
            branches.push((None, raise_blk));
            let dispatch = ir.add_expr(IrExpr::When { branches });
            let loop_body = ir.add_expr(IrExpr::Block {
                stmts: vec![set_i, dispatch],
                value: None,
            });
            let cond = ir.add_expr(IrExpr::GetValue(FLAG_SLOT));
            let whilexpr = ir.add_expr(IrExpr::While {
                cond,
                body: loop_body,
                update: None,
                post_test: false,
                label: None,
            });
            // A decoder that reports SEQUENTIAL decoding promises the elements arrive in
            // declaration order, so their indices need not be asked for at all: decode each in
            // turn and mark it seen. The blocks are the same ones the loop dispatches to — a
            // format that cannot promise it still takes the loop.
            let sequential_composite = ir.add_expr(IrExpr::GetValue(composite_slot));
            let sequential = ir.add_expr(IrExpr::Call {
                callee: virtual_iface(
                    "kotlinx/serialization/encoding/CompositeDecoder",
                    "decodeSequentially",
                    "()Z",
                ),
                dispatch_receiver: Some(sequential_composite),
                args: vec![],
            });
            let in_order = (0..fields.len())
                .map(|k| elements.block(ir, ctx, k))
                .collect::<Vec<_>>();
            let fast = ir.add_expr(IrExpr::Block {
                stmts: in_order,
                value: None,
            });
            let driven = ir.add_expr(IrExpr::Block {
                stmts: vec![whilexpr],
                value: None,
            });
            stmts.push(ir.add_expr(IrExpr::When {
                branches: vec![(Some(sequential), fast), (None, driven)],
            }));
            // endStructure
            let dend = this_desc(ir);
            let cend = composite(ir);
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
