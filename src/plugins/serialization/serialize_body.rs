//! Generation of a serializer method and its serialized-class write helper.

use super::{
    build_field_serializer_instance, class_ty, collection_serializer_builder,
    contextual_serializer_for, element_serializer_expr, encode_element_method, field_serializer_of,
    is_nullable, property_is_contextual, ty_descriptor, virtual_iface,
};
use crate::ir::{Callee, ClassId, ExprId, IrConst, IrExpr, IrFile};
use crate::libraries::InlineKind;
use crate::names::property_getter_name;
use crate::plugins::PluginContext;
use crate::types::Ty;

fn record_serialize_lines(
    ir: &mut IrFile,
    serialized_class: ClassId,
    function: u32,
    opening_expression: ExprId,
) {
    let (start_line, header_line) = {
        let class = &ir.classes[serialized_class as usize];
        (class.decl_start_line, class.decl_line)
    };
    if start_line != 0 {
        ir.expr_lines.insert(opening_expression, start_line);
    }
    if header_line != 0 {
        ir.fn_close_lines.insert(function, header_line);
    }
}

pub(super) struct SerializeBody<'a> {
    pub(super) function: u32,
    pub(super) serializer_class: ClassId,
    pub(super) serialized_class: ClassId,
    pub(super) fields: &'a [(String, Ty)],
    pub(super) field_defaults: &'a [Option<IrConst>],
    pub(super) nested_serializers: &'a [Option<ClassId>],
    pub(super) type_parameter_serializer_fields: &'a [Option<u32>],
    pub(super) write_self: Option<u32>,
    pub(super) write_self_name: String,
}

impl SerializeBody<'_> {
    pub(super) fn generate(self, ir: &mut IrFile, ctx: &PluginContext) {
        let Self {
            function: fid,
            serializer_class,
            serialized_class: foo_id,
            fields,
            field_defaults,
            nested_serializers: nested,
            type_parameter_serializer_fields: tp_field,
            write_self,
            write_self_name,
        } = self;
        let ser_idx = serializer_class as usize;
        let class_id = foo_id;
        let serialized_name = ir.classes[foo_id as usize].fq_name_id();
        let class_internal = ir.classes[foo_id as usize].fq_name();
        // kotlinc splits this in two: `serialize` opens the structure and DELEGATES
        // the element writes to the serialized class's own `write$Self` static,
        // which is where they live:
        //
        //   val descriptor = this.descriptor                      [local 3]
        //   val output = encoder.beginStructure(descriptor)       [local 4]
        //   Foo.write$Self$main(value, output, descriptor)
        //   output.endStructure(descriptor)
        //
        // krusty inlined the element writes here and left `write$Self` an empty
        // body — so the class exported a do-nothing helper that any OTHER module's
        // generated code calls, and `serialize` differed from kotlinc's from its
        // first instruction.
        let ser_cid = ser_idx as u32;
        let this_desc = |ir: &mut IrFile| -> ExprId {
            let r = ir.add_expr(IrExpr::GetValue(0));
            ir.add_expr(IrExpr::GetField {
                receiver: r,
                class: ser_cid,
                index: 0,
            })
        };
        // The element writes, in `write$Self`'s own frame: value = 0, output = 1,
        // descriptor = 2.
        // kotlinc hands a GENERIC class's element serializers to `write$Self` as
        // extra parameters; krusty's `write$Self` has the three-parameter shape only,
        // and a generic property's encode call reads `this.typeSerial<k>` off the
        // `$serializer` INSTANCE — which a static helper has no receiver for. So the
        // delegation applies to a non-generic class, and a generic one keeps the
        // inlined shape until `write$Self` carries those serializers too.
        let delegate = write_self.is_some() && ir.classes[ser_idx].type_params.is_empty();
        let value_slot = if delegate { 0 } else { 2 };
        let encoder_slot = if delegate { 1 } else { 3 };
        let mut bail = false;
        let mut stmts: Vec<ExprId> = Vec::new();
        // `write$Self` is a STATIC MEMBER of the serialized class, so it reads the
        // property's private backing FIELD directly — which is what kotlinc emits.
        // (The old inlined shape lived on the `$serializer`, which cannot, and had to
        // go through the public getter.)
        let read_property =
            |ir: &mut IrFile, field_index: usize, name: &str, ty: &Ty| -> Option<ExprId> {
                let receiver = ir.add_expr(IrExpr::GetValue(value_slot));
                if delegate {
                    return Some(ir.add_expr(IrExpr::GetField {
                        receiver,
                        class: foo_id,
                        index: field_index as u32,
                    }));
                }
                // The generic inlined shape lives on `$serializer`, so it uses the public getter.
                let descriptor = ty_descriptor(ctx, ty)?;
                Some(ir.add_expr(IrExpr::Call {
                    callee: Callee::Virtual {
                        owner: serialized_name,
                        name: property_getter_name(name),
                        descriptor: format!("(){descriptor}"),
                        params: None,
                        interface: false,
                    },
                    dispatch_receiver: Some(receiver),
                    args: vec![],
                }))
            };
        for (i, (pname, ty)) in fields.iter().enumerate() {
            let n_before = stmts.len();
            let d = if delegate {
                ir.add_expr(IrExpr::GetValue(2))
            } else {
                this_desc(ir)
            };
            let idx = ir.add_expr(IrExpr::Const(IrConst::Int(i as i32)));
            let Some(v) = read_property(ir, i, pname, ty) else {
                bail = true;
                break;
            };
            let c = ir.add_expr(IrExpr::GetValue(encoder_slot));
            if let Some(inst) =
                contextual_serializer_for(ir, property_is_contextual(ctx, ir, class_id, pname), ty)
            {
                // Contextual element: encode[Nullable]SerializableElement(desc, i,
                // ContextualSerializer(<type>::class), value.getX()).
                let method = if is_nullable(ty) {
                    "encodeNullableSerializableElement"
                } else {
                    "encodeSerializableElement"
                };
                stmts.push(ir.add_expr(IrExpr::Call {
                    callee: virtual_iface(
                        "kotlinx/serialization/encoding/CompositeEncoder",
                        method,
                        "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/SerializationStrategy;Ljava/lang/Object;)V",
                    ),
                    dispatch_receiver: Some(c),
                    args: vec![d, idx, inst, v],
                }));
            } else if let Some(fidx) = tp_field[i] {
                // Type-parameter element: encode[Nullable]SerializableElement(desc, i,
                // this.typeSerialK, value.getX()) — the serializer is the ctor-supplied one.
                let this_s = ir.add_expr(IrExpr::GetValue(0));
                let inst = ir.add_expr(IrExpr::GetField {
                    receiver: this_s,
                    class: ser_idx as u32,
                    index: fidx,
                });
                let method = if is_nullable(ty) {
                    "encodeNullableSerializableElement"
                } else {
                    "encodeSerializableElement"
                };
                stmts.push(ir.add_expr(IrExpr::Call {
                    callee: virtual_iface(
                        "kotlinx/serialization/encoding/CompositeEncoder",
                        method,
                        "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/SerializationStrategy;Ljava/lang/Object;)V",
                    ),
                    dispatch_receiver: Some(c),
                    args: vec![d, idx, inst, v],
                }));
            } else if let Some(internal) = field_serializer_of(ctx, ir, class_id, pname) {
                // An explicit per-property serializer takes precedence over the property's type.
                let inst = build_field_serializer_instance(ir, &internal);
                let method = if is_nullable(ty) {
                    "encodeNullableSerializableElement"
                } else {
                    "encodeSerializableElement"
                };
                stmts.push(ir.add_expr(IrExpr::Call {
                    callee: virtual_iface(
                        "kotlinx/serialization/encoding/CompositeEncoder",
                        method,
                        "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/SerializationStrategy;Ljava/lang/Object;)V",
                    ),
                    dispatch_receiver: Some(c),
                    args: vec![d, idx, inst, v],
                }));
            } else if nested[i].is_some()
                || ty
                    .non_null()
                    .obj_internal()
                    .and_then(collection_serializer_builder)
                    .is_some()
            {
                // Nested @Serializable OR a standard collection: encode[Nullable]Serializable
                // Element(desc, i, <element serializer>, value.getX()) — `$serializer.INSTANCE`
                // / `Foo.serializer(A_ser)` / `ListSerializer(…)`. The nullable variant shares
                // the SAME descriptor (writes JSON null) — a method-name swap.
                let Some(inst) = element_serializer_expr(ir, ty) else {
                    bail = true;
                    break;
                };
                let method = if is_nullable(ty) {
                    "encodeNullableSerializableElement"
                } else {
                    "encodeSerializableElement"
                };
                stmts.push(ir.add_expr(IrExpr::Call {
                    callee: virtual_iface(
                        "kotlinx/serialization/encoding/CompositeEncoder",
                        method,
                        "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/SerializationStrategy;Ljava/lang/Object;)V",
                    ),
                    dispatch_receiver: Some(c),
                    args: vec![d, idx, inst, v],
                }));
            } else if is_nullable(ty) {
                // Any derivable nullable element — builtin, interface-polymorphic, sealed, or
                // nested — uses the nullable serializable call so the encoder can write JSON null.
                if let Some(inst) = element_serializer_expr(ir, ty) {
                    stmts.push(ir.add_expr(IrExpr::Call {
                        callee: virtual_iface(
                            "kotlinx/serialization/encoding/CompositeEncoder",
                            "encodeNullableSerializableElement",
                            "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/SerializationStrategy;Ljava/lang/Object;)V",
                        ),
                        dispatch_receiver: Some(c),
                        args: vec![d, idx, inst, v],
                    }));
                } else {
                    bail = true;
                    break;
                }
            } else if let Some((mname, mdesc)) = encode_element_method(ty) {
                stmts.push(ir.add_expr(IrExpr::Call {
                    callee: virtual_iface(
                        "kotlinx/serialization/encoding/CompositeEncoder",
                        mname,
                        mdesc,
                    ),
                    dispatch_receiver: Some(c),
                    args: vec![d, idx, v],
                }));
            } else if let Some(inst) = element_serializer_expr(ir, ty) {
                // A non-null reference element with a builtin/derivable serializer (e.g.
                // `Uuid`) — encodeSerializableElement(desc, i, <Elem>Serializer, value.getX()).
                stmts.push(ir.add_expr(IrExpr::Call {
                    callee: virtual_iface(
                        "kotlinx/serialization/encoding/CompositeEncoder",
                        "encodeSerializableElement",
                        "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/SerializationStrategy;Ljava/lang/Object;)V",
                    ),
                    dispatch_receiver: Some(c),
                    args: vec![d, idx, inst, v],
                }));
            } else {
                bail = true;
                break;
            }
            // OPTIONAL element (a constant default): omit it on encode when it still
            // equals the default — wrap the just-pushed encode call in
            //   if (c.shouldEncodeElementDefault(desc, i) || value.getX() != default) { … }
            if let Some(Some(dc)) = field_defaults.get(i) {
                if stmts.len() == n_before + 1 {
                    let enc_stmt = stmts.pop().unwrap();
                    let cd = if delegate {
                        ir.add_expr(IrExpr::GetValue(2))
                    } else {
                        this_desc(ir)
                    };
                    let ci = ir.add_expr(IrExpr::Const(IrConst::Int(i as i32)));
                    let cc = ir.add_expr(IrExpr::GetValue(encoder_slot));
                    let should = ir.add_expr(IrExpr::Call {
                        callee: virtual_iface(
                            "kotlinx/serialization/encoding/CompositeEncoder",
                            "shouldEncodeElementDefault",
                            "(Lkotlinx/serialization/descriptors/SerialDescriptor;I)Z",
                        ),
                        dispatch_receiver: Some(cc),
                        args: vec![cd, ci],
                    });
                    // Re-read the property because one IR expression cannot occupy two tree nodes.
                    let Some(cur) = read_property(ir, i, pname, ty) else {
                        bail = true;
                        break;
                    };
                    let def = ir.add_expr(IrExpr::Const(dc.clone()));
                    let neq = ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: crate::ir::IrBinOp::Ne,
                        lhs: cur,
                        rhs: def,
                    });
                    let cond = ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: crate::ir::IrBinOp::Or,
                        lhs: should,
                        rhs: neq,
                    });
                    stmts.push(ir.add_expr(IrExpr::When {
                        branches: vec![(Some(cond), enc_stmt)],
                    }));
                }
            }
        }
        let delegated_write_self = delegate.then_some(write_self).flatten();
        if bail {
            // Do not turn an unsupported field serializer into a successful no-op. Preserve an
            // explicit plugin-owned residual so `jvm_can_emit` declines the file with the standard
            // unsupported-IR diagnostic instead of emitting a silently incomplete serializer.
            let unsupported = ir.add_expr(IrExpr::PluginPlaceholder {
                plugin: "serialization",
                kind: "serialize-body",
                exprs: Vec::new(),
                data: vec![serialized_name],
            });
            let body = ir.add_expr(IrExpr::Block {
                stmts: vec![unsupported],
                value: None,
            });
            ir.functions[fid as usize].body = Some(body);
        } else if let Some(write_self) = delegated_write_self {
            let ws_ret = ir.add_expr(IrExpr::Return(None));
            stmts.push(ws_ret);
            let ws_body = ir.add_expr(IrExpr::Block { stmts, value: None });
            ir.functions[write_self as usize].body = Some(ws_body);

            // `serialize` itself: descriptor local, open, delegate, close.
            let descriptor_init = this_desc(ir);
            let dvar = ir.add_expr(IrExpr::Variable {
                index: 3,
                ty: class_ty("kotlinx/serialization/descriptors/SerialDescriptor"),
                init: Some(descriptor_init),
                named: false,
            });
            let enc = ir.add_expr(IrExpr::GetValue(1));
            let dbegin = ir.add_expr(IrExpr::GetValue(3));
            let begin = ir.add_expr(IrExpr::Call {
                callee: virtual_iface(
                    "kotlinx/serialization/encoding/Encoder",
                    "beginStructure",
                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)Lkotlinx/serialization/encoding/CompositeEncoder;",
                ),
                dispatch_receiver: Some(enc),
                args: vec![dbegin],
            });
            let cvar = ir.add_expr(IrExpr::Variable {
                index: 4,
                ty: class_ty("kotlinx/serialization/encoding/CompositeEncoder"),
                init: Some(begin),
                named: false,
            });
            let wvalue = ir.add_expr(IrExpr::GetValue(2));
            let woutput = ir.add_expr(IrExpr::GetValue(4));
            let wdesc = ir.add_expr(IrExpr::GetValue(3));
            let call_write_self = ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: serialized_name,
                    name: write_self_name,
                    descriptor: format!(
                        "(L{class_internal};Lkotlinx/serialization/encoding/CompositeEncoder;Lkotlinx/serialization/descriptors/SerialDescriptor;)V"
                    ),
                    inline: InlineKind::None,
                },
                dispatch_receiver: None,
                args: vec![wvalue, woutput, wdesc],
            });
            let cend = ir.add_expr(IrExpr::GetValue(4));
            let dend = ir.add_expr(IrExpr::GetValue(3));
            let end = ir.add_expr(IrExpr::Call {
                callee: virtual_iface(
                    "kotlinx/serialization/encoding/CompositeEncoder",
                    "endStructure",
                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)V",
                ),
                dispatch_receiver: Some(cend),
                args: vec![dend],
            });
            record_serialize_lines(ir, foo_id, fid, dvar);
            let body = ir.add_expr(IrExpr::Block {
                stmts: vec![dvar, cvar, call_write_self, end],
                value: None,
            });
            ir.functions[fid as usize].body = Some(body);
        } else {
            // The inlined shape (a generic class): open the structure, write the
            // elements here, close it.
            let enc = ir.add_expr(IrExpr::GetValue(1));
            let d0 = this_desc(ir);
            let begin = ir.add_expr(IrExpr::Call {
                callee: virtual_iface(
                    "kotlinx/serialization/encoding/Encoder",
                    "beginStructure",
                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)Lkotlinx/serialization/encoding/CompositeEncoder;",
                ),
                dispatch_receiver: Some(enc),
                args: vec![d0],
            });
            let cvar = ir.add_expr(IrExpr::Variable {
                index: 3,
                ty: class_ty("kotlinx/serialization/encoding/CompositeEncoder"),
                init: Some(begin),
                named: false,
            });
            let dend = this_desc(ir);
            let cend = ir.add_expr(IrExpr::GetValue(3));
            let end = ir.add_expr(IrExpr::Call {
                callee: virtual_iface(
                    "kotlinx/serialization/encoding/CompositeEncoder",
                    "endStructure",
                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)V",
                ),
                dispatch_receiver: Some(cend),
                args: vec![dend],
            });
            let mut block = vec![cvar];
            block.extend(stmts);
            block.push(end);
            record_serialize_lines(ir, foo_id, fid, cvar);
            let body = ir.add_expr(IrExpr::Block {
                stmts: block,
                value: None,
            });
            ir.functions[fid as usize].body = Some(body);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::unit;
    use super::*;
    use crate::ir::{IrField, IrFunction};
    use crate::plugins::synthetic_class;
    use crate::types::type_name;

    #[test]
    fn an_unsupported_field_preserves_a_residual_instead_of_becoming_a_no_op() {
        let mut ir = IrFile::default();
        let mut serialized = synthetic_class("demo/Foo");
        let unsupported = Ty::obj("demo/Unsupported");
        serialized.fields = vec![IrField::new("value".to_owned(), unsupported)];
        let serialized_class = ir.add_class(serialized);
        let serializer_class = ir.add_class(synthetic_class("demo/Foo$$serializer"));
        let write_self = ir.add_fun(IrFunction {
            name: "write$Self".to_owned(),
            params: Vec::new(),
            ret: unit(),
            body: None,
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let serialize = ir.add_fun(IrFunction {
            name: "serialize".to_owned(),
            params: Vec::new(),
            ret: unit(),
            body: None,
            is_static: false,
            dispatch_receiver: Some(type_name("demo/Foo$$serializer")),
            param_checks: Vec::new(),
        });
        let fields = [("value".to_owned(), unsupported)];

        SerializeBody {
            function: serialize,
            serializer_class,
            serialized_class,
            fields: &fields,
            field_defaults: &[None],
            nested_serializers: &[None],
            type_parameter_serializer_fields: &[None],
            write_self: Some(write_self),
            write_self_name: "write$Self".to_owned(),
        }
        .generate(&mut ir, &PluginContext::default());

        let body = ir.functions[serialize as usize]
            .body
            .expect("serialize body");
        let IrExpr::Block { stmts, value: None } = ir.expr(body) else {
            panic!("serialize must retain a residual block")
        };
        assert_eq!(stmts.len(), 1);
        assert!(matches!(
            ir.expr(stmts[0]),
            IrExpr::PluginPlaceholder {
                plugin: "serialization",
                kind: "serialize-body",
                exprs,
                data,
            } if exprs.is_empty() && data == &[type_name("demo/Foo")]
        ));
        assert!(!crate::jvm::ir_emit::jvm_can_emit(&ir));
    }
}
