//! Generation of a serializer method and its serialized-class write helper.

use super::constructed_standard_serializers::constructed_standard_serializer;
use super::{
    build_field_serializer_instance, class_ty, contextual_serializer_for, declared_serializer_of,
    encode_element_method, is_nullable, property_is_contextual, virtual_iface,
};
use crate::ir::{Callee, ClassId, ExprId, IrConst, IrExpr, IrFile};
use crate::libraries::InlineKind;
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

/// `output.<method>(descriptor, index, serializer, value)`, one of `CompositeEncoder`'s
/// serializable-element writes. Their `value` is declared `T`, a reference slot, so a value class
/// reaches it as its box whatever its carrier, as it does at a source call of the same method.
/// The target descriptor of an overridable property's getter, which `write$Self` must call.
/// The getter is chosen by the property's modality, so a target that cannot describe its result
/// is an unsupported representation, never a reason to read the field instead.
fn overridable_getter_descriptor(ctx: &PluginContext, property: &str, ty: &Ty) -> String {
    super::ty_descriptor(ctx, ty).unwrap_or_else(|| {
        panic!("the target describes no getter result for overridable serialized property `{property}`")
    })
}

fn encode_serializable_element(
    ir: &mut IrFile,
    method: &str,
    output: ExprId,
    args: [ExprId; 4],
) -> ExprId {
    let call = ir.add_expr(IrExpr::Call {
        callee: Box::new(virtual_iface(
            "kotlinx/serialization/encoding/CompositeEncoder",
            method,
            "(Lkotlinx/serialization/descriptors/SerialDescriptor;ILkotlinx/serialization/SerializationStrategy;Ljava/lang/Object;)V",
        )),
        dispatch_receiver: Some(output),
        args: args.to_vec(),
    });
    ir.call_declared_params.insert(
        call,
        Box::new([
            class_ty("kotlinx/serialization/descriptors/SerialDescriptor"),
            Ty::Int,
            class_ty("kotlinx/serialization/SerializationStrategy"),
            Ty::nullable(class_ty("kotlin/Any")),
        ]),
    );
    call
}

/// The elements a class inherits from its serializable superclass, which the superclass's own
/// `write$Self` writes before the class writes its own.
pub(super) struct InheritedElements {
    pub(super) count: usize,
    /// The type arguments the class gives its generic superclass; its `write$Self` takes a
    /// serializer for each.
    pub(super) type_arguments: Vec<Ty>,
    pub(super) owner: crate::types::TypeName,
    /// The superclass's `write$Self` in this file; `None` for a superclass compiled apart from
    /// it, whose public `write$Self` takes the same arguments.
    pub(super) write_self: Option<u32>,
}

/// What `write$Self` writes: the serialized class's fields, its elements and its type-parameter
/// serializers.
pub(super) struct SerializedClassElements<'a> {
    pub(super) properties: &'a super::SerializedProperties,
    pub(super) elements: &'a super::SerialElements,
    pub(super) type_parameter_serializers:
        super::type_parameter_serializers::TypeParameterSerializers<'a>,
}

/// The `$serializer`'s `serialize`, which opens the structure and delegates to `write$Self`.
pub(super) struct GeneratedSerialize {
    pub(super) function: u32,
    pub(super) serializer_class: ClassId,
}

pub(super) struct SerializeBody<'a> {
    /// `None` for an abstract class, which has `write$Self` but no `$serializer`.
    pub(super) serialize: Option<GeneratedSerialize>,
    pub(super) serialized_class: ClassId,
    /// Every backing field's name and type, by field index.
    pub(super) fields: &'a [(String, Ty)],
    /// The backing field of each serial element, in element order. A `@Transient` property's
    /// field is not among them and is never written.
    pub(super) elements: &'a [usize],
    /// What the class inherits from its serializable superclass, if it has one.
    pub(super) inherited: Option<InheritedElements>,
    pub(super) field_defaults: &'a [Option<IrConst>],
    pub(super) nested_serializers: &'a [Option<ClassId>],
    /// The serializers of the class's type parameters, on the generic `$serializer`.
    pub(super) type_parameter_serializers:
        super::type_parameter_serializers::TypeParameterSerializers<'a>,
    /// The serialized class's `write$Self`, which `serialize` delegates the element writes to.
    pub(super) write_self: u32,
    pub(super) write_self_name: String,
    /// The serialized class's `$childSerializers` plan, or `None` when it has no cache.
    pub(super) cache: Option<super::child_serializer_cache::ChildSerializerCachePlan>,
}

impl SerializeBody<'_> {
    pub(super) fn generate(self, ir: &mut IrFile, ctx: &PluginContext) {
        let Self {
            serialize,
            serialized_class: foo_id,
            fields,
            elements,
            inherited,
            field_defaults,
            nested_serializers: nested,
            type_parameter_serializers,
            write_self,
            write_self_name,
            cache: plan,
        } = self;
        let class_id = foo_id;
        let serialized_name = ir.classes[foo_id as usize].fq_name_id();
        let class_internal = ir.classes[foo_id as usize].fq_name();
        // kotlinc splits this in two: `serialize` opens the structure and DELEGATES
        // the element writes to the serialized class's own `write$Self` static,
        // which is where they live:
        //
        //   val descriptor = this.descriptor                      [local 3]
        //   val output = encoder.beginStructure(descriptor)       [local 4]
        //   Foo.write$Self$main(value, output, descriptor, this.typeSerial0, …)
        //   output.endStructure(descriptor)
        // The element writes, in `write$Self`'s own frame: value = 0, output = 1,
        // descriptor = 2, then a generic class's type-parameter serializers, which `serialize`
        // passes from its `typeSerial<k>` fields.
        let value_slot = 0;
        let encoder_slot = 1;
        let element_scope = type_parameter_serializers.as_parameters(3);
        let mut bail = false;
        let mut stmts: Vec<ExprId> = Vec::new();
        // `write$Self` is a static member of the serialized class, so it reaches
        // `$childSerializers` with a plain `getstatic` rather than through the `access$…$cp`
        // bridge the `$serializer` needs. The local is the first free one after its parameters.
        // The plan, as the pass that built the cache published it, says which properties have a
        // slot and which static holds them.
        let cache_local = 3 + type_parameter_serializers.len() as u32;
        // Every nullable value-class read owns one semantic temporary. Reserve the whole range up
        // front so a checked computed default, which remaps its own locals into this frame below,
        // starts after them instead of aliasing a value still used by the guarded encode call.
        let offset = inherited.as_ref().map_or(0, |inherited| inherited.count);
        let wire_len = offset + elements.len();
        let element_temporary_start = cache_local + 1;
        let after_element_temporaries = element_temporary_start
            + u32::try_from(elements.len()).expect("too many serialized elements");
        // The element serializer for property `i`: the cache slot when the class caches it, else
        // built here. The PLAN decides, so this can never read a slot the serialized class did not
        // write.
        let element_serializer =
            |ir: &mut IrFile, ctx: &PluginContext, i: usize, property: &str, ty: &Ty| {
                if let Some(cached) = super::child_serializer_cache::cached_strategy(
                    ir,
                    plan.as_ref(),
                    Some(cache_local),
                    i,
                    wire_len,
                    "kotlinx/serialization/SerializationStrategy",
                ) {
                    return Some(cached);
                }
                let annotated = super::type_argument_serializers::declared_type_spelling(
                    ctx, ir, class_id, property,
                );
                super::element_serializer::element_serializer_expr_in(
                    ir,
                    ctx,
                    ty,
                    element_scope,
                    &annotated,
                )
            };
        // `write$Self` is a STATIC MEMBER of the serialized class, so it reads a final
        // property's private backing FIELD directly — which is what kotlinc emits once its JVM
        // lowering substitutes the default getter. An open or overriding property's getter can
        // be overridden, so kotlinc calls it.
        let read_property = |ir: &mut IrFile, field_index: usize, ty: &Ty| -> ExprId {
            let receiver = ir.add_expr(IrExpr::GetValue(value_slot));
            let field = field_index as u32;
            let overridable = ir.classes[foo_id as usize]
                .properties
                .iter()
                .find(|property| property.backing_field == Some(field))
                // kotlinc reads a non-private `lateinit` element through its getter too, which
                // throws for an uninitialized one. A private one has no getter: its field read is
                // checked where it is used.
                .filter(|property| {
                    property.is_open
                        || (!property.is_private
                            && ir.classes[foo_id as usize].fields[field_index].is_lateinit())
                })
                .map(|property| (property.name.clone(), property.getter));
            let read = match overridable {
                Some((name, declared_getter)) => {
                    let descriptor = overridable_getter_descriptor(ctx, &name, ty);
                    super::synthesized_accessor::getter(
                        ir,
                        receiver,
                        serialized_name,
                        &name,
                        descriptor,
                        (foo_id, field),
                        declared_getter,
                    )
                }
                None => ir.add_expr(IrExpr::GetField {
                    receiver,
                    class: foo_id,
                    index: field,
                }),
            };
            // An unsigned value is held as its signed carrier; the read is boxed as the
            // unsigned type it is.
            if ty.is_unsigned() {
                ir.logical_types.insert(read, *ty);
            }
            read
        };
        // The superclass's `write$Self` writes the inherited elements first, from the same object
        // viewed as the superclass.
        if let Some(inherited) = &inherited {
            let value = ir.add_expr(IrExpr::GetValue(value_slot));
            let value = ir.add_expr(IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::Cast,
                arg: value,
                type_operand: Ty::obj_name(inherited.owner),
            });
            let output = ir.add_expr(IrExpr::GetValue(encoder_slot));
            let descriptor = ir.add_expr(IrExpr::GetValue(2));
            let mut args = vec![value, output, descriptor];
            for argument in &inherited.type_arguments {
                let Some(serializer) = super::element_serializer::element_serializer_expr_in(
                    ir,
                    ctx,
                    argument,
                    element_scope,
                    crate::ir::Spelled::NONE,
                ) else {
                    bail = true;
                    break;
                };
                args.push(ir.add_expr(IrExpr::TypeOp {
                    op: crate::ir::IrTypeOp::Cast,
                    arg: serializer,
                    type_operand: super::class_ty(super::KSERIALIZER_FQ),
                }));
            }
            let callee = match inherited.write_self {
                Some(function) => Callee::ClassStatic {
                    owner: inherited.owner,
                    function,
                },
                None => {
                    let owner = inherited.owner.render();
                    let type_serializers = "Lkotlinx/serialization/KSerializer;"
                        .repeat(inherited.type_arguments.len());
                    Callee::Static {
                        owner: inherited.owner,
                        name: "write$Self".to_string(),
                        descriptor: format!(
                            "(L{owner};Lkotlinx/serialization/encoding/CompositeEncoder;Lkotlinx/serialization/descriptors/SerialDescriptor;{type_serializers})V"
                        ),
                        inline: InlineKind::None,
                    }
                }
            };
            stmts.push(ir.add_expr(IrExpr::Call {
                callee: Box::new(callee),
                dispatch_receiver: None,
                args,
            }));
        }
        // Own element `i` is written from backing field `field`; they differ after a transient
        // property. It is element `offset + i` of the wire shape, after the inherited ones.
        for (own, &field) in elements.iter().enumerate() {
            let i = offset + own;
            let (pname, ty) = &fields[field];
            let n_before = stmts.len();
            let d = ir.add_expr(IrExpr::GetValue(2));
            let idx = ir.add_expr(IrExpr::Const(IrConst::Int(i as i32)));
            let v = read_property(ir, field, ty);
            let c = ir.add_expr(IrExpr::GetValue(encoder_slot));
            let contextual = property_is_contextual(ctx, ir, class_id, pname);
            let contextual = if contextual {
                super::child_serializer_cache::cached_strategy(
                    ir,
                    plan.as_ref(),
                    Some(cache_local),
                    i,
                    wire_len,
                    "kotlinx/serialization/SerializationStrategy",
                )
                .or_else(|| contextual_serializer_for(ir, true, ty))
            } else {
                None
            };
            if let Some(inst) = contextual {
                // Contextual element: encode[Nullable]SerializableElement(desc, i,
                // ContextualSerializer(<type>::class), value.getX()).
                let method = if is_nullable(ty) {
                    "encodeNullableSerializableElement"
                } else {
                    "encodeSerializableElement"
                };
                let inst = super::deserialize_body::narrowed(
                    ir,
                    inst,
                    "kotlinx/serialization/SerializationStrategy",
                );
                stmts.push(encode_serializable_element(
                    ir,
                    method,
                    c,
                    [d, idx, inst, v],
                ));
            } else if let Some(internal) = declared_serializer_of(ctx, ir, class_id, pname) {
                // An explicit per-property serializer takes precedence over the property's type.
                let annotated = super::type_argument_serializers::declared_type_spelling(
                    ctx, ir, class_id, pname,
                );
                let inst = build_field_serializer_instance(
                    ir,
                    ctx,
                    internal,
                    *ty,
                    element_scope,
                    &annotated,
                );
                let method = if is_nullable(ty) {
                    "encodeNullableSerializableElement"
                } else {
                    "encodeSerializableElement"
                };
                let inst = super::deserialize_body::narrowed(
                    ir,
                    inst,
                    "kotlinx/serialization/SerializationStrategy",
                );
                stmts.push(encode_serializable_element(
                    ir,
                    method,
                    c,
                    [d, idx, inst, v],
                ));
            } else if nested[field].is_some()
                || ty
                    .non_null()
                    .obj_internal()
                    .and_then(constructed_standard_serializer)
                    .is_some()
            {
                // Nested @Serializable OR a constructed standard type:
                // encode[Nullable]Serializable
                // Element(desc, i, <element serializer>, value.getX()) — `$serializer.INSTANCE`
                // / `Foo.serializer(A_ser)` / `ListSerializer(…)`. The nullable variant shares
                // the SAME descriptor (writes JSON null) — a method-name swap.
                let Some(inst) = element_serializer(ir, ctx, i, pname, ty) else {
                    bail = true;
                    break;
                };
                let method = if is_nullable(ty) {
                    "encodeNullableSerializableElement"
                } else {
                    "encodeSerializableElement"
                };
                let inst = super::deserialize_body::narrowed(
                    ir,
                    inst,
                    "kotlinx/serialization/SerializationStrategy",
                );
                stmts.push(encode_serializable_element(
                    ir,
                    method,
                    c,
                    [d, idx, inst, v],
                ));
            } else if is_nullable(ty) {
                // Any derivable nullable element — builtin, interface-polymorphic, sealed, or
                // nested — uses the nullable serializable call so the encoder can write JSON null.
                if let Some(inst) = element_serializer(ir, ctx, i, pname, ty) {
                    let inst = super::deserialize_body::narrowed(
                        ir,
                        inst,
                        "kotlinx/serialization/SerializationStrategy",
                    );
                    stmts.push(encode_serializable_element(
                        ir,
                        "encodeNullableSerializableElement",
                        c,
                        [d, idx, inst, v],
                    ));
                } else {
                    bail = true;
                    break;
                }
            } else if let Some((mname, mdesc)) = encode_element_method(ty) {
                stmts.push(ir.add_expr(IrExpr::Call {
                    callee: Box::new(virtual_iface(
                        "kotlinx/serialization/encoding/CompositeEncoder",
                        mname,
                        mdesc,
                    )),
                    dispatch_receiver: Some(c),
                    args: vec![d, idx, v],
                }));
            } else if let Some(inst) = element_serializer(ir, ctx, i, pname, ty) {
                // A non-null reference element with a builtin/derivable serializer (e.g.
                // `Uuid`) — encodeSerializableElement(desc, i, <Elem>Serializer, value.getX()).
                let inst = super::deserialize_body::narrowed(
                    ir,
                    inst,
                    "kotlinx/serialization/SerializationStrategy",
                );
                stmts.push(encode_serializable_element(
                    ir,
                    "encodeSerializableElement",
                    c,
                    [d, idx, inst, v],
                ));
            } else {
                bail = true;
                break;
            }
            // OPTIONAL element: omit it on encode when it still equals the default — wrap the
            // just-pushed encode call in
            //   if (c.shouldEncodeElementDefault(desc, i) || value.getX() != default) { … }
            // A constant default is compared as that constant. Any other default is the property's
            // checked initializer, evaluated again here with an earlier property read off the
            // object being written, which is what kotlinc's `write$Self` does.
            let constant_default = field_defaults.get(field).cloned().flatten();
            let computed_default = constant_default.is_none()
                && super::property_default::checked_default(ir, foo_id, field).is_some();
            if (constant_default.is_some() || computed_default) && stmts.len() == n_before + 1 {
                let enc_stmt = stmts.pop().unwrap();
                let cd = ir.add_expr(IrExpr::GetValue(2));
                let ci = ir.add_expr(IrExpr::Const(IrConst::Int(i as i32)));
                let cc = ir.add_expr(IrExpr::GetValue(encoder_slot));
                let should = ir.add_expr(IrExpr::Call {
                    callee: Box::new(virtual_iface(
                        "kotlinx/serialization/encoding/CompositeEncoder",
                        "shouldEncodeElementDefault",
                        "(Lkotlinx/serialization/descriptors/SerialDescriptor;I)Z",
                    )),
                    dispatch_receiver: Some(cc),
                    args: vec![cd, ci],
                });
                // Re-read the property because one IR expression cannot occupy two tree nodes.
                let cur = read_property(ir, field, ty);
                let def = match &constant_default {
                    Some(dc) => {
                        let def = ir.add_expr(IrExpr::Const(dc.clone()));
                        // The default value carries the property's own declaration line, as
                        // it does in the deserialization constructor. The comparison that
                        // consumes it is marked back to the statement's line by the emitter,
                        // which is where that rule lives. A `null` default is excluded: it is
                        // tested with `ifnull`, which pushes nothing, and kotlinc marks no
                        // line for it.
                        if !matches!(dc, IrConst::Null) {
                            if let Some(line) = ir
                                .prop_decl_lines
                                .get(&(serialized_name, pname.clone()))
                                .copied()
                                .filter(|line| *line != 0)
                            {
                                ir.set_source_line(def, line);
                            }
                        }
                        def
                    }
                    None => {
                        let mut read_field = |ir: &mut IrFile, field: usize| {
                            let (_, ty) = fields.get(field)?;
                            Some(read_property(ir, field, ty))
                        };
                        let Some(def) = super::property_default::default_in_frame(
                            ir,
                            foo_id,
                            field,
                            super::property_default::DefaultFrame {
                                first_free_local: after_element_temporaries,
                                read_field: &mut read_field,
                                // `write$Self` is a static member of the class, so the object it
                                // writes stands in for the receiver the default was checked
                                // against.
                                receiver: Some(value_slot),
                                property_line: ir
                                    .prop_decl_lines
                                    .get(&(serialized_name, pname.clone()))
                                    .copied()
                                    .filter(|line| *line != 0),
                                read_line: Some(ir.classes[foo_id as usize].decl_start_line)
                                    .filter(|line| *line != 0),
                                // A read of the object being written is `write$Self`'s own, as
                                // a re-read constructor property is.
                                receiver_read_line: Some(
                                    ir.classes[foo_id as usize].decl_start_line,
                                )
                                .filter(|line| *line != 0),
                            },
                        ) else {
                            bail = true;
                            break;
                        };
                        def
                    }
                };
                let neq = ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: crate::ir::IrBinOp::Ne,
                    lhs: cur,
                    rhs: def,
                });
                // SHORT-CIRCUIT, as kotlinc shapes it: ask the encoder first and answer
                // `true` without reading the property at all, otherwise fall through to the
                // comparison. `IrBinOp::Or` here is the EAGER form — it holds the left operand
                // in a temp and combines with `ior`, which is a different method body from
                // kotlinc's for every defaulted property in the class.
                let encode_anyway = ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
                let cond = ir.add_expr(IrExpr::When {
                    branches: vec![(Some(should), encode_anyway), (None, neq)],
                });
                let guard = ir.add_expr(IrExpr::When {
                    branches: vec![(Some(cond), enc_stmt)],
                });
                stmts.push(guard);
            }
        }
        if bail {
            // Do not turn an unsupported field serializer into a successful no-op. Preserve an
            // explicit plugin-owned residual so `jvm_can_emit` declines the file with the standard
            // unsupported-IR diagnostic instead of emitting a silently incomplete serializer.
            let unsupported = ir.add_expr(IrExpr::PluginPlaceholder {
                plugin: "serialization",
                kind: "serialize-body",
                exprs: Vec::new(),
                data: vec![serialized_name],
                types: Vec::new(),
            });
            let body = ir.add_expr(IrExpr::Block {
                stmts: vec![unsupported],
                value: None,
            });
            match serialize {
                Some(serialize) => super::complete_generated_body(ir, serialize.function, body),
                None => ir.functions[write_self as usize].body = Some(body),
            }
        } else {
            if let Some(plan) = plan.as_ref() {
                // `write$Self` is a static member of the class that OWNS the cache, so it reads the
                // static directly. The plan names which one.
                let read = ir.add_expr(IrExpr::GetStatic(plan.static_index));
                let declaration = ir.add_expr(IrExpr::Variable {
                    index: cache_local,
                    ty: super::child_serializer_cache::lazy_cache_ty(),
                    init: Some(read),
                    named: false,
                });
                // kotlinc reads the cache after the superclass's `write$Self` has run.
                stmts.insert(usize::from(inherited.is_some()), declaration);
            }
            // Every statement of `write$Self` is one of the class's own declaration, so it carries
            // the class's start line: kotlinc opens the method on that line at its first
            // instruction, whether that is the cache read, an unconditional element or a guard.
            // That is also what puts the emitter's "current statement line" in effect, which the
            // comparison inside a guard's condition returns to.
            let start_line = ir.classes[foo_id as usize].decl_start_line;
            if start_line != 0 {
                for &statement in &stmts {
                    ir.expr_lines.insert(statement, start_line);
                }
            }
            let ws_ret = ir.add_expr(IrExpr::Return(None));
            stmts.push(ws_ret);
            let ws_body = ir.add_expr(IrExpr::Block { stmts, value: None });
            ir.functions[write_self as usize].body = Some(ws_body);

            // `serialize` itself: descriptor local, open, delegate, close.
            let Some(GeneratedSerialize {
                function: fid,
                serializer_class,
            }) = serialize
            else {
                return;
            };
            let receiver = ir.add_expr(IrExpr::GetValue(0));
            let descriptor_init = ir.add_expr(IrExpr::GetField {
                receiver,
                class: serializer_class,
                index: 0,
            });
            let dvar = ir.add_expr(IrExpr::Variable {
                index: 3,
                ty: class_ty("kotlinx/serialization/descriptors/SerialDescriptor"),
                init: Some(descriptor_init),
                named: false,
            });
            let enc = ir.add_expr(IrExpr::GetValue(1));
            let dbegin = ir.add_expr(IrExpr::GetValue(3));
            let begin = ir.add_expr(IrExpr::Call {
                callee: Box::new(virtual_iface(
                    "kotlinx/serialization/encoding/Encoder",
                    "beginStructure",
                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)Lkotlinx/serialization/encoding/CompositeEncoder;",
                )),
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
            let type_serializers = type_parameter_serializers.reads(ir);
            let type_serializer_descriptors =
                "Lkotlinx/serialization/KSerializer;".repeat(type_serializers.len());
            let call_write_self = ir.add_expr(IrExpr::Call {
                callee: Box::new(Callee::Static {
                    owner: serialized_name,
                    name: write_self_name,
                    descriptor: format!(
                        "(L{class_internal};Lkotlinx/serialization/encoding/CompositeEncoder;Lkotlinx/serialization/descriptors/SerialDescriptor;{type_serializer_descriptors})V"
                    ),
                    inline: InlineKind::None,
                }),
                dispatch_receiver: None,
                args: [wvalue, woutput, wdesc]
                    .into_iter()
                    .chain(type_serializers)
                    .collect(),
            });
            let cend = ir.add_expr(IrExpr::GetValue(4));
            let dend = ir.add_expr(IrExpr::GetValue(3));
            let end = ir.add_expr(IrExpr::Call {
                callee: Box::new(virtual_iface(
                    "kotlinx/serialization/encoding/CompositeEncoder",
                    "endStructure",
                    "(Lkotlinx/serialization/descriptors/SerialDescriptor;)V",
                )),
                dispatch_receiver: Some(cend),
                args: vec![dend],
            });
            record_serialize_lines(ir, foo_id, fid, dvar);
            let body = ir.add_expr(IrExpr::Block {
                stmts: vec![dvar, cvar, call_write_self, end],
                value: None,
            });
            super::complete_generated_body(ir, fid, body);
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
    #[should_panic(
        expected = "the target describes no getter result for overridable serialized property `name`"
    )]
    fn an_overridable_property_without_a_getter_descriptor_is_not_read_from_its_field() {
        // The default context's target describes no type, as an unsupported representation would.
        overridable_getter_descriptor(&PluginContext::default(), "name", &Ty::String);
    }

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
        // Declared as the handoff declares it: with a placeholder body.
        let declared =
            super::super::generated_body_placeholder(&mut ir, type_name("demo/Foo$$serializer"));
        let serialize = ir.add_fun(IrFunction {
            name: "serialize".to_owned(),
            params: Vec::new(),
            ret: unit(),
            body: Some(declared),
            is_static: false,
            dispatch_receiver: Some(type_name("demo/Foo$$serializer")),
            param_checks: Vec::new(),
        });
        let fields = [("value".to_owned(), unsupported)];

        SerializeBody {
            serialize: Some(GeneratedSerialize {
                function: serialize,
                serializer_class,
            }),
            serialized_class,
            fields: &fields,
            elements: &[0],
            inherited: None,
            field_defaults: &[None],
            nested_serializers: &[None],
            type_parameter_serializers:
                super::super::type_parameter_serializers::TypeParameterSerializers::NONE,
            write_self,
            write_self_name: "write$Self".to_owned(),
            // This fixture's property has no derivable serializer at all, so its class has no
            // cache to plan.
            cache: None,
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
                types,
            } if exprs.is_empty() && data == &[type_name("demo/Foo")] && types.is_empty()
        ));
        assert!(!crate::jvm::ir_emit::jvm_can_emit(&ir));
    }
}
