//! Checked generation of a serializer's `deserialize` body.

use super::element_serializer::unsupported_element_serializer;
use super::{
    build_field_serializer_instance, class_ty, contextual_serializer_for, decode_element_method,
    field_serializer_of, inline_prim_methods, is_nullable, property_is_contextual,
    value_class_underlying, virtual_iface,
};
use crate::ir::{ClassId, ExprId, IrConst, IrExpr, IrFile, IrTypeOp};
use crate::kt_string::KtString;
use crate::plugins::PluginContext;
use crate::types::Ty;

/// One element's decode and the seen-mask bit that records it arrived — the same block on the
/// sequential fast path and in the index-driven loop, so it is built once and used twice.
struct ElementDecode<'a> {
    serialized_class: ClassId,
    fields: &'a [(String, Ty)],
    type_parameter_serializers: super::type_parameter_serializers::TypeParameterSerializers<'a>,
    descriptor_local: u32,
    field_locals: &'a [u32],
    seen_locals: &'a [u32],
    composite_local: u32,
    /// The local the serialized class's `$childSerializers` was loaded into, and which elements it
    /// holds — `None` when the class has no cache. A cached element is READ from the slot instead
    /// of rebuilt here, which is what kotlinc emits.
    cache: Option<(
        u32,
        &'a super::child_serializer_cache::ChildSerializerCachePlan,
    )>,
}

/// Narrow a serializer operand to the interface the element call declares.
///
/// Every serializer reaching one of these calls already implements it — a generated
/// `Foo$$serializer`, a builtin singleton, a `Lazy` slot's value — so the JVM verifies the call
/// without a cast and krusty emitted none. kotlinc narrows unconditionally, and the difference is
/// three bytes plus its pool entries at EVERY element of every generated serializer.
pub(super) fn narrowed(ir: &mut IrFile, serializer: ExprId, interface: &str) -> ExprId {
    ir.add_expr(IrExpr::TypeOp {
        op: IrTypeOp::Cast,
        arg: serializer,
        type_operand: class_ty(interface),
    })
}

impl ElementDecode<'_> {
    /// `cache[k].value` when element `k` is cached — narrowed from the `Lazy`'s erased `Object` to
    /// the decode call's `DeserializationStrategy` parameter, exactly as kotlinc does.
    fn cached_slot(&self, ir: &mut IrFile, k: usize) -> Option<ExprId> {
        let (local, plan) = self.cache?;
        super::child_serializer_cache::cached_strategy(
            ir,
            Some(plan),
            Some(local),
            k,
            self.fields.len(),
            "kotlinx/serialization/DeserializationStrategy",
        )
    }

    /// `f_k = (T) c.decode[Nullable]SerializableElement(desc, k, <serializer>, f_k)`, from the
    /// descriptor, index and composite-decoder operands the caller already read.
    ///
    /// `decodeSerializableElement` merges into the value decoded so far, so the element's own local
    /// is what it receives — a literal `null` discards whatever a merging serializer would have
    /// built on.
    ///
    /// On the first pass that local holds its JVM zero, which for a reference element is `null`:
    /// the body declares and zero-initializes every field local before decoding begins. So the
    /// first decode of an element passes the same `null` the old code spelled out, and every LATER
    /// one passes what the previous decode produced — which is the whole difference, and the reason
    /// disassembly parity alone does not prove it.
    fn decode_serializable(
        &self,
        ir: &mut IrFile,
        k: usize,
        [dk, idxc, cdk]: [ExprId; 3],
        serializer: ExprId,
    ) -> ExprId {
        let ty = self.fields[k].1;
        let inst = narrowed(
            ir,
            serializer,
            "kotlinx/serialization/DeserializationStrategy",
        );
        let prev = ir.add_expr(IrExpr::GetValue(self.field_locals[k]));
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
    }

    fn block(&self, ir: &mut IrFile, ctx: &PluginContext, k: usize) -> ExprId {
        let ty = self.fields[k].1;
        let dk = ir.add_expr(IrExpr::GetValue(self.descriptor_local));
        let idxc = ir.add_expr(IrExpr::Const(IrConst::Int(k as i32)));
        let cdk = ir.add_expr(IrExpr::GetValue(self.composite_local));
        let decoded = if let Some(inst) = contextual_serializer_for(
            ir,
            property_is_contextual(ctx, ir, self.serialized_class, &self.fields[k].0),
            &ty,
        ) {
            // Contextual element: `ContextualSerializer(<type>::class)`.
            self.decode_serializable(ir, k, [dk, idxc, cdk], inst)
        } else if let Some(internal) =
            field_serializer_of(ctx, ir, self.serialized_class, &self.fields[k].0)
        {
            // An explicit per-property serializer takes precedence over the property's type, as it
            // does in `serialize` and `childSerializers`: what `X` wrote only `X` can read back.
            let inst = build_field_serializer_instance(ir, internal);
            self.decode_serializable(ir, k, [dk, idxc, cdk], inst)
        } else if is_nullable(&ty) || decode_element_method(&ty).is_none() {
            // The nested `$serializer.INSTANCE` (non-generic) / `Foo.serializer(A_ser)` (generic) /
            // `ListSerializer(…)` (collection). Same descriptor; the nullable variant yields null
            // for a JSON-null element.
            let inst = self
                .cached_slot(ir, k)
                .or_else(|| {
                    super::element_serializer::element_serializer_expr_in(
                        ir,
                        ctx,
                        &ty,
                        self.type_parameter_serializers,
                    )
                })
                .unwrap_or_else(|| unsupported_element_serializer(ir, ty));
            self.decode_serializable(ir, k, [dk, idxc, cdk], inst)
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
            var: self.field_locals[k],
            value: decoded,
        });
        let mut decoded_stmts = vec![setk];
        {
            // `seen[word] = seen[word] or bit` — the element arrived.
            let seen_local = self.seen_locals[k / 32];
            let seen = ir.add_expr(IrExpr::GetValue(seen_local));
            let bit = ir.add_expr(IrExpr::Const(IrConst::Int(
                1i32.wrapping_shl((k % 32) as u32),
            )));
            let marked = ir.add_expr(IrExpr::PrimitiveBinOp {
                op: crate::ir::IrBinOp::BitOr,
                lhs: seen,
                rhs: bit,
            });
            decoded_stmts.push(ir.add_expr(IrExpr::SetValue {
                var: seen_local,
                value: marked,
            }));
        }
        ir.add_expr(IrExpr::Block {
            stmts: decoded_stmts,
            value: None,
        })
    }
}

pub(super) struct DeserializeBody<'a> {
    pub(super) function: u32,
    pub(super) serializer_class: ClassId,
    pub(super) serialized_class: ClassId,
    pub(super) fields: &'a [(String, Ty)],
    /// The serializers of the class's type parameters, on the generic `$serializer`.
    pub(super) type_parameter_serializers:
        super::type_parameter_serializers::TypeParameterSerializers<'a>,
    /// The serialized class's `$childSerializers` plan, or `None` when it has no cache. Passed in
    /// rather than rediscovered: the builder's answer about which properties have a slot is the
    /// only one, so a reader cannot believe in a slot that was never written.
    pub(super) cache: Option<super::child_serializer_cache::ChildSerializerCachePlan>,
}

impl DeserializeBody<'_> {
    pub(super) fn generate(self, ir: &mut IrFile, ctx: &PluginContext) {
        let Self {
            function: fid,
            serializer_class,
            serialized_class: foo_id,
            fields,
            type_parameter_serializers,
            cache: cache_plan,
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
        let decodable = fields.iter().all(|(pname, t)| {
            if property_is_contextual(ctx, ir, class_id, pname)
                || field_serializer_of(ctx, ir, class_id, pname).is_some()
            {
                return true;
            }
            // A non-null primitive decodes through its own `decode<T>Element`. Every other field
            // consumes the same semantic serializer plan as `serialize` and `childSerializers`;
            // there is no separate nested/collection classifier path. A nullable element always
            // takes the serializer path
            // (`decodeNullableSerializableElement`), which is why its builtin is not the only way to
            // decode it: a nullable nested class, enum or collection has no builtin at all.
            if !is_nullable(t) && decode_element_method(t).is_some() {
                return true;
            }
            super::element_serializer::element_serializer_plan_in(
                ir,
                ctx,
                t,
                type_parameter_serializers,
            )
            .is_some()
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
        // Semantic locals, declared in kotlinc's order. Their identities are independent of JVM
        // slots and word widths; each backend owns its physical local layout.
        let mut next_local = u32::try_from(ir.functions[fid as usize].params.len())
            .expect("too many deserialize parameters")
            + 1; // instance receiver
        let mut fresh_local = || {
            let local = next_local;
            next_local += 1;
            local
        };
        let serial_desc_local = fresh_local();
        let flag_local = fresh_local();
        let index_local = fresh_local();
        let mask_count = fields.len() / 32 + 1;
        // The producer records the exact deserialization constructor identity; consuming
        // `synthetic` or arity would accidentally select an unrelated generated constructor.
        let seen_locals = (0..mask_count).map(|_| fresh_local()).collect::<Vec<_>>();
        let field_locals = fields.iter().map(|_| fresh_local()).collect::<Vec<_>>();
        let composite_local = fresh_local();
        // The serialized class's cache, as the pass that built it published it — never rediscovered
        // by searching `ir.statics` for the field's spelling, and never re-answering which
        // properties have a slot. kotlinc numbers this local LAST, after every field and the
        // composite decoder, although it assigns it first, right behind `beginStructure`.
        let cache_local = cache_plan.as_ref().map(|_| fresh_local());
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
            |ir: &mut IrFile| -> ExprId { ir.add_expr(IrExpr::GetValue(serial_desc_local)) };
        let composite =
            |ir: &mut IrFile| -> ExprId { ir.add_expr(IrExpr::GetValue(composite_local)) };
        let elements = ElementDecode {
            serialized_class: class_id,
            fields,
            type_parameter_serializers,
            descriptor_local: serial_desc_local,
            field_locals: &field_locals,
            seen_locals: &seen_locals,
            composite_local,
            cache: cache_local.zip(cache_plan.as_ref()),
        };
        let body = {
            let this0 = ir.add_expr(IrExpr::GetValue(0));
            let descriptor_field = ir.add_expr(IrExpr::GetField {
                receiver: this0,
                class: ser_cid,
                index: 0,
            });
            let mut stmts = vec![ir.add_expr(IrExpr::Variable {
                index: serial_desc_local,
                ty: class_ty("kotlinx/serialization/descriptors/SerialDescriptor"),
                init: Some(descriptor_field),
                named: false,
            })];
            let flag_init = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
            stmts.push(ir.add_expr(IrExpr::Variable {
                index: flag_local,
                ty: Ty::Boolean,
                init: Some(flag_init),
                named: false,
            }));
            // Declared, never initialized: the element index exists only inside the loop, and the
            // target verifier state keeps its physical local `top` until the loop's first store.
            stmts.push(ir.add_expr(IrExpr::Variable {
                index: index_local,
                ty: class_ty("kotlin/Int"),
                init: None,
                named: false,
            }));
            for &seen_local in &seen_locals {
                let zero = ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                stmts.push(ir.add_expr(IrExpr::Variable {
                    index: seen_local,
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
                    index: field_locals[k],
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
                index: composite_local,
                ty: class_ty("kotlinx/serialization/encoding/CompositeDecoder"),
                init: Some(begin),
                named: false,
            }));
            if let (Some(local), Some(plan)) = (cache_local, cache_plan.as_ref()) {
                // The accessor the PLAN names, by its function id — its owner is the serialized
                // class's own interned identity, and `ClassStatic` leaves the name and the JVM
                // descriptor to be formed at the JVM boundary from the declaration itself.
                let read = ir.add_expr(IrExpr::Call {
                    callee: crate::ir::Callee::ClassStatic {
                        owner: ir.classes[foo_id as usize].fq_name_id(),
                        function: plan.accessor,
                    },
                    dispatch_receiver: None,
                    args: vec![],
                });
                stmts.push(ir.add_expr(IrExpr::Variable {
                    index: local,
                    ty: super::child_serializer_cache::lazy_cache_ty(),
                    init: Some(read),
                    named: false,
                }));
            }
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
                var: index_local,
                value: dei,
            });
            // ONE `when` over the index, not a chain of `if`s: every branch compares the same
            // `Int` local against a distinct constant, which is the shape the emitter turns into
            // kotlinc's `tableswitch`. `-1` clears the loop flag instead of breaking, so the loop
            // exits through its own condition; an index that names no element is an
            // `UnknownFieldException`, which krusty previously ignored in silence.
            let iref = ir.add_expr(IrExpr::GetValue(index_local));
            let neg1 = ir.add_expr(IrExpr::Const(IrConst::Int(-1)));
            let is_done = ir.add_expr(IrExpr::PrimitiveBinOp {
                op: crate::ir::IrBinOp::Eq,
                lhs: iref,
                rhs: neg1,
            });
            let stop = ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
            let clear_flag = ir.add_expr(IrExpr::SetValue {
                var: flag_local,
                value: stop,
            });
            let done_blk = ir.add_expr(IrExpr::Block {
                stmts: vec![clear_flag],
                value: None,
            });
            let mut branches = vec![(Some(is_done), done_blk)];
            for (k, _) in fields.iter().enumerate() {
                let iref = ir.add_expr(IrExpr::GetValue(index_local));
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
            let unknown_index = ir.add_expr(IrExpr::GetValue(index_local));
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
            let cond = ir.add_expr(IrExpr::GetValue(flag_local));
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
            let sequential_composite = ir.add_expr(IrExpr::GetValue(composite_local));
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
            for &seen_local in &seen_locals {
                args.push(ir.add_expr(IrExpr::GetValue(seen_local)));
            }
            for (index, &local) in field_locals.iter().enumerate() {
                let argument = ir.add_expr(IrExpr::GetValue(local));
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
