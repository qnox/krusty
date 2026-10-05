//! A class's `@kotlin.Metadata`, computed from its common IR.

use super::*;

/// Common class-shape admission shared by the writer and transitive value-class readability. Keeping
/// these kind/constructor bails in one predicate is correctness-critical: if the writer withholds a
/// value class but the transitive check independently admits it, a mentioning class publishes a type
/// a downstream compiler reads as an ordinary box.
pub(super) fn class_metadata_common_shape_admitted(_ir: &IrFile, c: &crate::ir::IrClass) -> bool {
    !(c.prop_ref.is_some()
        || c.func_ref.is_some()
        // A published secondary constructor is described from its recorded semantic parameter
        // identities. A malformed publication contract would advertise the wrong parameter list,
        // so the class declines instead. Unpublished target realizations carry no record.
        || c.secondary_ctors
            .iter()
            .any(|sc| sc.metadata_visibility.is_some() && sc.named_params.len() != sc.params.len())
        || (!c.has_primary_ctor
            && c.secondary_ctors.is_empty()
            && !c.is_interface
            && !c.is_enum)
        || (c.fields.len() as u32) < c.ctor_param_count)
}

/// The single admission predicate for a VALUE class's own metadata record. Both the class writer
/// and transitive value-class readability call it, so adding a new write-side bail cannot silently
/// let a different class describe the withheld value class downstream.
pub(super) fn value_class_metadata_shape_admitted(ir: &IrFile, c: &crate::ir::IrClass) -> bool {
    c.is_value
        && class_metadata_common_shape_admitted(ir, c)
        // A value class's ctors realize as mangled static `constructor-impl` overloads, which the
        // secondary-ctor record path does not model — keep declining that combination.
        && c.secondary_ctors.is_empty()
        && c.fields.len() == 1
        && c.fields[0].is_final()
        && !ir.has_value_param_ctor(&c.fq_name())
}

/// Compute a class's `@kotlin.Metadata` from its IR — WIRING [`crate::metadata::class_builder::build_class`]
/// into emission. Covers a class with a primary constructor of `val`/`var` properties plus real declared
/// members (emitted with derived [`function_flags`]), and the data/value-class synthesized sets. Returns
/// `None` for still-unsupported shapes (companion/annotation/enum-entry/secondary-ctors/…), so those
/// classes emit no `@Metadata` (unchanged). Broader shapes follow as `build_class` grows.
pub(super) fn build_class_metadata(
    ir: &IrFile,
    c: &crate::ir::IrClass,
    opts: &EmitOptions,
    env: &EmitEnv<'_>,
) -> Option<KotlinMetadata> {
    build_class_metadata_with_facts(
        ir,
        env.override_results,
        c,
        opts,
        env.local_delegated(),
        env.signature_symbols,
        env.run,
    )
}

pub(super) fn build_class_metadata_with_facts(
    ir: &IrFile,
    override_results: &crate::jvm::override_results::OverrideResults,
    c: &crate::ir::IrClass,
    opts: &EmitOptions,
    locals: &crate::jvm::property_references::local_delegated_properties::LocalDelegatedProperties,
    signature_symbols: &dyn crate::backend::BackendClassifierSource,
    run: &EmitRun,
) -> Option<KotlinMetadata> {
    use crate::metadata::class_builder::{
        build_class, ClassTail, FnMeta, PropMeta, COMPONENT_FN_FLAGS, EQUALS_FN_FLAGS,
        FN_IS_SUSPEND, HASHCODE_TOSTRING_FN_FLAGS, OBJECT_CTOR_FLAGS, SEALED_CTOR_FLAGS,
    };
    if is_coroutine_state_machine(c) {
        return Some(KotlinMetadata {
            k: 3,
            mv: opts.metadata_version().to_vec(),
            xi: synthetic_class_xi(if is_continuation_class(c) {
                SYNTHETIC_PROTECTED
            } else {
                SYNTHETIC_LOCAL
            }),
            d1: vec![],
            d2: vec![],
        });
    }
    if !class_metadata_common_shape_admitted(ir, c) {
        return None;
    }
    // A `data class` also carries kotlinc's synthesized `componentN`/`copy`/`equals`/`hashCode`/
    // `toString` — derivable from the primary-ctor properties alone, so allowed alongside accessors.
    if c.is_value && !value_class_metadata_shape_admitted(ir, c) {
        return None;
    }
    // A value class's compiler-synthesized members (the static `-impl` family + their instance
    // delegators); allowed alongside the property accessor without disqualifying the shape.
    let value_method_names: std::collections::HashSet<String> = if c.is_value {
        [
            "equals",
            "hashCode",
            "toString",
            "equals-impl",
            "equals-impl0",
            "hashCode-impl",
            "toString-impl",
            "box-impl",
            "unbox-impl",
            "constructor-impl",
        ]
        .map(String::from)
        .into_iter()
        .collect()
    } else {
        std::collections::HashSet::new()
    };
    let synthesizes_copy = synthesizes_data_class_members(c);
    // `data` synthesizes over the PRIMARY-CONSTRUCTOR properties only — `c.fields` also holds the
    // backing fields of body properties (`data class P(val x: Int) { val y = 1 }` has two fields but
    // one component). Counting all of them advertised a `component2` the class file does not define,
    // and a `copy(II)` where only `copy(I)` exists; real kotlinc reading that record accepts
    // `val (a, b) = p` and binds a method that is not there.
    let data_component_fields = &c.fields[..(c.ctor_param_count as usize).min(c.fields.len())];
    let data_component_properties = if c.is_data {
        (0..data_component_fields.len())
            .map(|index| {
                c.properties
                    .iter()
                    .find(|property| property.backing_field == Some(index as u32))
            })
            .collect::<Option<Vec<_>>>()?
    } else {
        Vec::new()
    };
    // The only methods allowed in this bounded shape are the properties' own accessors (`getX`/`setX`)
    // plus a data class's synthesized set; any other real method is a shape not computed yet.
    // Accessor spellings are matched by name AND shape below, so the getter and setter names stay
    // in separate sets: a declared `operator fun getValue(thisRef, prop)` shares the JVM getter
    // name of a property called `value` but takes parameters no getter has — swallowing it by name
    // alone dropped its Function record from `@Metadata`, and a consumer could then not resolve
    // the delegate operator.
    // Accessor spellings map to the PROPERTY TYPE's descriptor: a declared function that merely
    // shares the getter name but has a different return (`val x: String` beside
    // `fun getX(): Int`) is a real Function record, not the accessor — the JVM holds both.
    let mut getter_names: std::collections::HashMap<String, String> = Default::default();
    let mut setter_names: std::collections::HashMap<String, String> = Default::default();
    for (name, ty) in c
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.ty))
        // A HOISTED companion property has no companion field, but its delegating accessors are
        // ordinary IR methods — they realize the Property record, never a Function one.
        .chain(
            c.properties
                .iter()
                .enumerate()
                .filter(|(property, p)| {
                    p.backing_field.is_none()
                        && static_fields::hoisted_static_for(ir, c, *property).is_some()
                })
                .map(|(_, p)| (p.name.as_str(), p.ty)),
        )
        // An INTERFACE property's accessor is a real default-method `IrFunction` (there is no
        // backing field to derive its name from), but it realizes the Property record — kotlinc
        // emits no Function entry for `getX` of `val x: Int get() = 1`, and a kotlinc consumer
        // reading both reports "inherited platform declarations clash" on every implementer.
        .chain(
            c.is_interface
                .then(|| c.properties.iter().map(|p| (p.name.as_str(), p.ty)))
                .into_iter()
                .flatten(),
        )
    {
        let (getter, setter) = accessor_jvm_names(c, name);
        let descriptor = crate::jvm::names::type_descriptor(jvm_declared_ty(&ty));
        getter_names.insert(getter, descriptor.clone());
        setter_names.insert(setter, descriptor);
    }
    // Member-extension-PROPERTY accessors are described as `Property` records (below), never as
    // functions — kotlinc emits no `Function` record for `getDoubled` of `val Int.doubled`.
    let ext_prop_accessor_fids: std::collections::HashSet<u32> = ir
        .member_ext_props
        .get(&c.fq_name_id())
        .into_iter()
        .flatten()
        .flat_map(|prop| std::iter::once(prop.getter).chain(prop.setter))
        .collect();
    let property_accessor_fids: std::collections::HashSet<u32> = c
        .properties
        .iter()
        .flat_map(|property| property.getter.into_iter().chain(property.setter))
        .collect();
    let source_callable_fids = ir
        .checked_callable_functions
        .values()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    // Metadata Function records come only from exact source-callable realizations. Accessors have
    // Property records, while generated declarations use the explicit publication contract below.
    let mut declared_fids: Vec<u32> = c
        .methods
        .iter()
        .copied()
        .filter(|&fid| {
            // A physical class method is not necessarily a Kotlin declaration. Lifted local
            // functions are implementation methods and have no Function entry. An `interface by`
            // forwarder is a Kotlin function too, but it is recorded after the class's own
            // declarations, sorted with the other delegation members; the override edge names it.
            // Property forwarders stay Property records. Metadata consumes those identities
            // instead of inferring declaration status from a name, descriptor, or parameter spelling.
            if !source_callable_fids.contains(&fid) || ir.is_interface_delegation_function(fid) {
                return false;
            }
            if ir.lambda_own_params_from.contains_key(&fid) || ir.synthetic_methods.contains(&fid) {
                return false;
            }
            if ext_prop_accessor_fids.contains(&fid) {
                return false;
            }
            if property_accessor_fids.contains(&fid) {
                return false;
            }
            let function = &ir.functions[fid as usize];
            let n = &function.name;
            let accessor_shaped = (function.params.is_empty()
                && getter_names.get(n).is_some_and(|descriptor| {
                    crate::jvm::names::type_descriptor(jvm_declared_ty(&function.ret))
                        == *descriptor
                }))
                || (function.params.len() == 1
                    && matches!(function.ret, Ty::Unit)
                    && setter_names.get(n).is_some_and(|descriptor| {
                        crate::jvm::names::type_descriptor(jvm_declared_ty(&function.params[0]))
                            == *descriptor
                    }));
            !accessor_shaped
                && !ir.is_data_class_member(c.fq_name_id(), fid)
                && !value_method_names.contains(n)
        })
        .collect();
    declared_fids.sort_by_key(|fid| ir.fn_source_order.get(fid).copied().unwrap_or(u32::MAX));
    let generated_publication = ir.generated_member_publication(c.fq_name_id());
    // A VALUE-CLASS-INVOLVED MEMBER is now DESCRIBED. The writer could always produce kotlinc's exact
    // payload for one (the byte-identity tests proved it); what was missing was the READ half, and the
    // classpath value-class RETURN model supplies it — `MetadataCallFacts::value_class_ret` reports
    // that the physical method already hands back the ERASED underlying, so a caller that learns the
    // Kotlin return `K` from `@Metadata` no longer also emits kotlinc's boxed sequence (`invokevirtual
    // I.f-XLNMDGE()Ljava/lang/String; checkcast K; K.unbox-impl()`) over a `String` that IS the
    // carrier. Round-tripped by `krusty_roundtrip_class_metadata_e2e`'s value-class cases (each RUNS
    // `box()`) and pinned by the box corpus's `compileKotlinAgainstKotlin/inlineClasses/*` MODULE
    // chains.
    //
    // An `Object`-erased value-class member is therefore described too. The metadata-selected
    // dependency declaration carries semantic and physical parameter/result shapes independently,
    // so a generic-underlying value class (`kotlin/Result`) no longer requires guessing from its
    // `Object` descriptor. General class/value-class shape admission is centralized below in
    // `class_metadata_common_shape_admitted` / `value_class_metadata_shape_admitted`.
    //
    // …and a class cannot be described in terms of a value class a downstream compilation cannot READ
    // as one (`value_class_is_readable`): it would see an ordinary class, cast the carrier to the box
    // and bind an instance accessor where kotlinc emits the static `-impl` — a ClassCastException.
    // Describing `Holder.make(): A` is only sound once `A` itself is described.
    let mentions_undescribed_value_class = |t: &Ty| {
        t.non_null().obj_internal().is_some_and(|fq_name| {
            // Same-file and classpath declarations are in the unified lookup. A sibling source
            // declaration is deliberately not materialized into this file's IR, so the module-origin
            // subset is also positive identity for that one case; it is not a second underlying map.
            (crate::jvm::value_classes::is_boxed_value_class(ir, fq_name)
                || ir.module_source_value_classes.contains(&fq_name))
                && !value_class_is_readable(ir, fq_name)
        })
    };
    if declared_fids
        .iter()
        .copied()
        .chain(
            generated_publication
                .into_iter()
                .flat_map(|publication| publication.functions.iter())
                .filter(|member| member.metadata.is_some())
                .map(|member| member.function),
        )
        .any(|fid| {
            ir.vc_declared_sigs
                .get(&fid)
                .is_some_and(|(_, params, ret)| {
                    params
                        .iter()
                        .chain(std::iter::once(ret))
                        .any(mentions_undescribed_value_class)
                })
        })
        || c.properties
            .iter()
            .any(|p| p.getter_jvm_name.is_some() && mentions_undescribed_value_class(&p.ty))
    {
        return None;
    }
    let desc = |t: Ty| crate::jvm::names::type_descriptor(t);
    let local_classifiers = super::super::local_classifiers::names(ir);
    // A backing field records its descriptor exactly when a reader cannot rebuild it from the
    // property type (kotlinc's `requiresSignature`).
    let requires_field_signature = |property: Ty, physical: &str| {
        super::super::metadata_method_signatures::requires_field_signature(
            property,
            physical,
            &local_classifiers,
        )
    };
    // Metadata describes Kotlin PROPERTY declarations, never physical fields. Synthetic storage such
    // as `x$delegate`, `this$0`, and interface-delegation fields has no source declaration and must not
    // leak into the metadata name/type namespace. A property's optional backing field supplies only
    // its JVM realization (descriptor, constant, and accessor descriptor).
    let mut declared_props: Vec<(u32, PropMeta)> = c
        .properties
        .iter()
        .enumerate()
        .map(|(property_index, property)| {
            crate::trace_compiler!(
                "metadata",
                "emit class metadata property owner={:?} name={} ty={:?} context={:?}",
                c.fq_name,
                property.name,
                property.ty,
                property.context_params,
            );
            let visibility = property.visibility;
            let backing = property
                .backing_field
                .and_then(|index| c.fields.get(index as usize).map(|field| (index, field)));
            let (default_getter, default_setter) = accessor_jvm_names(c, &property.name);
            // A getter's descriptor is its physical one: a scalar getter result over a reference-returning overridden
            // property returns the wrapper (see `jvm::override_results`).
            let physical_getter = |fid: u32| {
                let function = &ir.functions[fid as usize];
                (
                    function.name.clone(),
                    ir_method_desc(&function.params, &override_results.physical_result(ir, fid)),
                )
            };
            let ordinary_getter = property
                .getter
                .filter(|&fid| (fid as usize) < ir.functions.len())
                .map(physical_getter)
                .or_else(|| {
                    c.methods
                        .iter()
                        .copied()
                        .find(|&fid| ir.functions[fid as usize].name == default_getter)
                        .map(physical_getter)
                })
                .or_else(|| {
                    backing.and_then(|(_, field)| {
                        // `@JvmField` suppresses the accessor pair entirely, so there is no
                        // synthesized getter to derive from the backing field — kotlinc records the
                        // field alone.
                        let boxed = override_results
                            .boxes_member_property(c.fq_name, property_index as u32);
                        let result = if boxed {
                            Ty::nullable(property.ty)
                        } else {
                            field.ty
                        };
                        (!visibility.is_private() && !is_jvm_field(c, &property.name))
                            .then(|| (default_getter, format!("(){}", desc(result))))
                    })
                });
            let getter = if c.is_annotation {
                let stored = crate::jvm::annotation_kclass::annotation_member_jvm_type(
                    jvm_declared_ty(&property.ty),
                );
                Some((property.name.clone(), format!("(){}", desc(stored))))
            } else {
                ordinary_getter
            };
            let setter = property
                .setter
                .and_then(|fid| ir.functions.get(fid as usize))
                .map(|function| {
                    (
                        function.name.clone(),
                        ir_method_desc(&function.params, &function.ret),
                    )
                })
                .or_else(|| {
                    c.methods
                        .iter()
                        .map(|fid| &ir.functions[*fid as usize])
                        .find(|function| function.name == default_setter)
                        .map(|function| {
                            (
                                function.name.clone(),
                                ir_method_desc(&function.params, &function.ret),
                            )
                        })
                })
                .or_else(|| {
                    backing.and_then(|(_, field)| {
                        (!visibility.is_private()
                            && property.is_var
                            && !is_jvm_field(c, &property.name))
                        .then(|| (default_setter, format!("({})V", desc(field.ty))))
                    })
                });
            // A delegated property's JVM field is its `x$delegate` storage, which metadata names.
            let delegate = property
                .delegate_field
                .and_then(|i| c.fields.get(i as usize));
            (
                property.source_order,
                PropMeta {
                    return_value_status: property.return_value_status,
                    spellings: ir
                        .prop_declared_spellings
                        .get(&(c.fq_name_id(), property.name.clone()))
                        .cloned()
                        .unwrap_or_default(),
                    name: property.name.clone(),
                    ty: property.ty,
                    context_params: property.context_params.clone(),
                    is_var: property.is_var,
                    visibility,
                    // A HOISTED companion property still records a (derived) backing field — the field
                    // exists, on the outer class — and a literal-initialized `val` keeps kotlinc's
                    // HAS_CONSTANT flag exactly like an instance-field one.
                    has_constant: backing.is_some_and(|(index, field)| {
                        field.is_final() && index >= c.ctor_param_count
                    }) && property
                        .initializer
                        .is_some_and(|init| static_fields::literal_initializer(ir, init))
                        || static_fields::hoisted_static_for(ir, c, property_index).is_some_and(
                            |s| {
                                !s.is_var
                                    && s.init.is_some_and(|init| {
                                        static_fields::literal_initializer(ir, init)
                                    })
                            },
                        ),
                    is_const: false,
                    modifiers: property.modifiers,
                    setter_visibility: property.setter_visibility,
                    has_backing_field: !c.is_annotation
                        && (backing.is_some()
                            || delegate.is_some()
                            || static_fields::hoisted_static_for(ir, c, property_index).is_some())
                        && !c.is_interface,
                    tparam: match property.ty {
                        Ty::TyParam(name, _) => Some(name),
                        Ty::Nullable(inner) | Ty::PlatformNullable(inner) => match *inner {
                            Ty::TyParam(name, _) => Some(name),
                            _ => None,
                        },
                        _ => None,
                    }
                    .and_then(|parameter| {
                        let parameter = crate::types::type_parameter_source_name(parameter);
                        c.type_params
                            .iter()
                            .position(|candidate| candidate == parameter)
                    })
                    .or_else(|| {
                        ir.field_signatures(&c.fq_name()).and_then(|signatures| {
                            signatures
                                .iter()
                                .find(|(field, _)| field == &property.name)
                                .and_then(|(_, parameter)| {
                                    c.type_params
                                        .iter()
                                        .position(|candidate| candidate == parameter)
                                })
                        })
                    })
                    .map(|index| index as u32),
                    receiver: None,
                    type_params: property.type_params.clone(),
                    getter,
                    setter,
                    setter_parameter_name: super::super::parameter_names::explicit_setter(
                        ir,
                        property.setter,
                    ),
                    field_desc: backing
                        .map(|(_, field)| field.ty)
                        .or(delegate.map(|field| field.ty))
                        .or_else(|| {
                            static_fields::hoisted_static_for(ir, c, property_index)
                                .map(|storage| storage.ty)
                        })
                        .map(desc)
                        .filter(|physical| requires_field_signature(property.ty, physical)),
                    // The PHYSICAL field name when the JVM realization mangles it — an instance
                    // property beside a same-named hoisted companion static (`result` → `result$1`).
                    field_name: backing
                        .map(|(_, field)| field)
                        .or(delegate)
                        .map(|field| instance_field_jvm_name(ir, c, field))
                        .filter(|physical| *physical != property.name),
                    // A property-targeted annotation lives on its synthetic marker method; the
                    // record here is what connects the property to it (and the marker's FINAL name,
                    // which the value-class pass may have mangled with the getter's).
                    annotations: property_metadata_annotations(c, &property.name),
                    field_annotations: property_backing_field_annotations(c, &property.name),
                    synthetic_method: property_marker_signature(ir, c, &property.name),
                    // kotlinc marks an interface companion's `@JvmField` property record: the
                    // backing field was MOVED onto the interface itself.
                    moved_from_interface_companion: companion_of_interface(ir, c)
                        && static_fields::jvm_field_static_for(ir, c, property_index),
                    companion: false,
                },
            )
        })
        .collect();
    // A class's own `const val`s (`declared_class_statics`) enter here — BEFORE the extension
    // properties — and the whole set then sorts by each declaration's exact source offset (kotlinc's
    // metadata property order). The key stays attached to the declaration across JVM storage moves.
    for &static_id in ir
        .declared_class_statics
        .get(&c.fq_name_id())
        .into_iter()
        .flatten()
    {
        let prop = &ir.statics[static_id as usize];
        declared_props.push((
            prop.source_order,
            PropMeta {
                return_value_status: Default::default(),
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: prop.name.clone(),
                ty: prop.ty,
                context_params: Vec::new(),
                is_var: prop.is_var,
                visibility: prop.visibility,
                has_constant: true,
                is_const: true,
                modifiers: Default::default(),
                setter_visibility: prop.visibility,
                has_backing_field: true,
                tparam: None,
                receiver: None,
                type_params: Vec::new(),
                getter: None,
                setter: None,
                setter_parameter_name: None,
                field_desc: None,
                field_name: None,
                annotations: property_metadata_annotations(c, &prop.name),
                field_annotations: property_backing_field_annotations(c, &prop.name),
                synthetic_method: property_marker_signature(ir, c, &prop.name),
                moved_from_interface_companion: false,
                companion: false,
            },
        ));
    }
    companion_blocks::push_property_metadata(ir, c, &mut declared_props);
    declared_props.sort_by_key(|(line, _)| *line);
    let mut prop_source_orders: Vec<u32> = declared_props.iter().map(|(order, _)| *order).collect();
    let mut props: Vec<PropMeta> = declared_props
        .into_iter()
        .map(|(_, property)| property)
        .collect();
    // Member EXTENSION properties: a `Property` record with `receiver_type` and the accessor
    // signatures — the declaration the accessor methods (excluded from `declared_fids`) realize.
    for ext in ir
        .member_ext_props
        .get(&c.fq_name_id())
        .into_iter()
        .flatten()
    {
        let accessor_sig = |fid: u32| {
            ir.functions.get(fid as usize).map(|function| {
                (
                    function.name.clone(),
                    ir_method_desc(&function.params, &function.ret),
                )
            })
        };
        let ext_delegate = ext.delegate_field.and_then(|i| c.fields.get(i as usize));
        props.push(PropMeta {
            return_value_status: Default::default(),
            spellings: ir
                .prop_declared_spellings
                .get(&(c.fq_name_id(), ext.name.clone()))
                .cloned()
                .unwrap_or_default(),
            name: ext.name.clone(),
            ty: ext.ty,
            context_params: Vec::new(),
            is_var: ext.is_var,
            visibility: ext.visibility,
            has_constant: false,
            is_const: false,
            modifiers: ext.modifiers,
            setter_visibility: ext.visibility,
            has_backing_field: ext_delegate.is_some(),
            tparam: None,
            receiver: Some(ext.receiver),
            type_params: ext.type_params.clone(),
            getter: accessor_sig(ext.getter),
            setter: ext.setter.and_then(accessor_sig),
            setter_parameter_name: super::super::parameter_names::explicit_setter(ir, ext.setter),
            field_desc: ext_delegate
                .map(|field| desc(field.ty))
                .filter(|physical| requires_field_signature(ext.ty, physical)),
            field_name: ext_delegate.map(|field| instance_field_jvm_name(ir, c, field)),
            annotations: property_metadata_annotations(c, &ext.name),
            field_annotations: Default::default(),
            synthetic_method: property_marker_signature(ir, c, &ext.name),
            moved_from_interface_companion: false,
            companion: false,
        });
        prop_source_orders.push(
            ir.fn_source_order
                .get(&ext.getter)
                .copied()
                .unwrap_or(u32::MAX),
        );
    }
    let named_ctor_args: Vec<(String, Ty, bool, Option<u32>)> = c
        .ctor_args
        .iter()
        .filter_map(|arg| {
            arg.name.as_ref().map(|name| {
                (
                    name.clone(),
                    arg.declared_ty.unwrap_or(arg.ty),
                    arg.has_default,
                    arg.type_param,
                )
            })
        })
        .collect();
    // Parallel to `named_ctor_args` (hence the SAME unnamed-parameter filter): the class's
    // `ctor_param_annotations` covers every `ctor_args` entry, including the synthetic unnamed ones a
    // metadata constructor record never lists.
    let named_ctor_param_annotations: Vec<crate::metadata::MetadataAnnotations> = c
        .ctor_args
        .iter()
        .enumerate()
        .filter(|(_, arg)| arg.name.is_some())
        .map(|(i, _)| {
            crate::metadata::MetadataAnnotations::of_optional(c.ctor_param_annotations.get(i))
        })
        .collect();
    let ctor_params_with_defaults = if c.ctor_args.is_empty() {
        c.fields
            .iter()
            .take(c.ctor_param_count as usize)
            .map(|field| {
                let type_param = ir.field_signatures(&c.fq_name()).and_then(|signatures| {
                    signatures
                        .iter()
                        .find(|(name, _)| name == &field.name)
                        .and_then(|(_, type_param)| {
                            c.type_params
                                .iter()
                                .position(|candidate| candidate == type_param)
                        })
                        .map(|index| index as u32)
                });
                (
                    field.name.clone(),
                    field.ty,
                    field.has_default(),
                    type_param,
                )
            })
            .collect()
    } else {
        // `ctor_args` may contain only the unnamed enclosing-instance parameter of an `inner`
        // class. That parameter belongs in the JVM descriptor below, never in Kotlin metadata's
        // source value-parameter list; an empty `named_ctor_args` is therefore authoritative.
        named_ctor_args
    };
    // Position of a `vararg` primary-ctor parameter within the NAMED parameter list (the same
    // filtered order `ctor_params` uses) — `None` when unnamed-args fallback is in effect.
    let ctor_vararg_index = c
        .ctor_args
        .iter()
        .filter(|arg| arg.name.is_some())
        .position(|arg| arg.is_vararg);
    let ctor_params: Vec<(String, Ty)> = ctor_params_with_defaults
        .iter()
        .map(|(name, ty, _, _)| (name.clone(), *ty))
        .collect();
    let ctor_param_defaults: Vec<bool> = ctor_params_with_defaults
        .iter()
        .map(|(_, _, has_default, _)| *has_default)
        .collect();
    let ctor_param_tparams: Vec<Option<u32>> = ctor_params_with_defaults
        .iter()
        .map(|(_, _, _, type_param)| *type_param)
        .collect();
    // An `enum class`'s JVM constructor takes the two synthetic `Enum` parameters first, so its
    // recorded `JvmMethodSignature` is `(Ljava/lang/String;I…)V` — the metadata names the REAL
    // descriptor even though those parameters are not Kotlin-visible.
    let ctor_desc = format!(
        "({}{}{})V",
        if !c.is_enum {
            ""
        } else {
            "Ljava/lang/String;I"
        },
        // The physical `<init>` leads with the UNNAMED lowering-added parameters — an inner class's
        // enclosing instance (`Llib/Outer;`) — which `ctor_params` (source parameters) never carry.
        // kotlinc's record spells them (`(Llib/Outer;Ljava/lang/String;I)V`); without them a
        // consumer's constructor call is one slot short.
        c.ctor_args
            .iter()
            .filter(|arg| arg.name.is_none())
            .map(|arg| desc(arg.ty))
            .collect::<String>(),
        ctor_params
            .iter()
            .map(|(_, t)| desc(*t))
            .collect::<String>()
    );
    // A value-class-parametered primary ctor: the record names the DECLARED types (`id: ItemId` —
    // the erase pass rewrote `ctor_args` to the underlying), and its physical handle is the PUBLIC
    // synthetic marker ctor (`(…;Lkotlin/jvm/internal/DefaultConstructorMarker;)V`) — the private
    // erased `<init>` is not callable cross-class. Both exactly as kotlinc records them.
    let (ctor_params, ctor_desc) = match ir.vc_ctor_declared_params(c.fq_name_id()) {
        Some(declared) if !c.is_enum => {
            let named_declared: Vec<Ty> = c
                .ctor_args
                .iter()
                .zip(declared)
                .filter(|(arg, _)| arg.name.is_some())
                .map(|(_, ty)| *ty)
                .collect();
            let params = if named_declared.len() == ctor_params.len() {
                ctor_params
                    .iter()
                    .zip(&named_declared)
                    .map(|((name, _), ty)| (name.clone(), *ty))
                    .collect()
            } else {
                ctor_params
            };
            // The physical marker ctor spells EVERY parameter — an inner class's leading outer
            // instance included (`ctor_params` above holds only the NAMED source parameters).
            let desc = format!(
                "({}Lkotlin/jvm/internal/DefaultConstructorMarker;)V",
                c.ctor_args
                    .iter()
                    .map(|arg| desc(arg.ty))
                    .collect::<String>()
            );
            (params, desc)
        }
        _ => (ctor_params, ctor_desc),
    };
    // kotlinc's synthesized data-class methods, in declaration order: componentN, copy, equals,
    // hashCode, toString. Their shapes come entirely from the primary-ctor properties.
    let describe_methods = |fids: &[u32]| {
        fids.iter()
            .filter_map(|&fid| {
                let f = ir.functions.get(fid as usize)?;
                // Real parameter identities — metadata is reflection-visible, so a placeholder
                // would be an observable lie. A missing semantic name is rejected below.
                let parameter_identities = ir.function_parameter_identities(fid);
                // A function is described as SOURCE declared it: its own name, parameters and return
                // type. Two lowerings hide that — CPS gives a `suspend fun` a trailing `Continuation`
                // and an `Object` return, and the value-class pass mangles the name and erases the
                // value classes away. Prefer the value-class record when both applied: it ran first,
                // so it holds the fully declared form. What the JVM method actually looks like rides
                // along as a `JvmMethodSignature` (name only when mangling changed it).
                let is_suspend = ir.suspend_declared_sigs.contains_key(&fid);
                let vc = ir.vc_declared_sigs.get(&fid);
                let declared = vc
                    .map(|(n, p, r)| (n.as_str(), p.as_slice(), *r))
                    .or_else(|| {
                        ir.suspend_declared_sigs
                            .get(&fid)
                            .map(|(p, r)| (f.name.as_str(), p.as_slice(), *r))
                    });
                let (_, params, ret) =
                    declared.unwrap_or((f.name.as_str(), f.params.as_slice(), f.ret));
                let name = ir
                    .fn_source_names
                    .get(&fid)
                    .map(String::as_str)
                    .expect("a source metadata function retains its declaration name");
                let semantic_signature = ir.signatures.get(&fid);
                // A member mentioning an ENCLOSING-CLASS type parameter records its semantic shape
                // separately (`member_semantic_sigs`) — the erased params would publish `Any`.
                let member_semantic = ir
                    .member_semantic_sigs
                    .get(&fid)
                    .filter(|_| !ir.extension_receiver_fns.contains(&fid));
                let metadata_params = semantic_signature
                    .map(|signature| signature.params.as_slice())
                    .or(member_semantic.map(|(params, _)| params.as_slice()))
                    .unwrap_or(params);
                let metadata_ret = semantic_signature
                    .and_then(|signature| signature.ret)
                    .or(member_semantic.map(|(_, ret)| *ret))
                    .unwrap_or(ret);
                // A parameter's declared `?` lives in a side-table, not in `params` (which stays
                // non-null so the value-class mangle is undisturbed) — re-apply it for `@Metadata`.
                let declared_nullable = ir.fn_param_declared_nullable.get(&fid);
                let function_type_params = ir
                    .signatures
                    .get(&fid)
                    .map(|signature| {
                        signature
                            .type_params
                            .iter()
                            .map(|parameter| parameter.name.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                let semantic_function_type_params = semantic_signature
                    .map(|signature| {
                        signature
                            .type_params
                            .iter()
                            .map(|parameter| parameter.semantic_name.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                let function_type_param_bounds = semantic_signature
                    .map(|signature| {
                        signature
                            .type_params
                            .iter()
                            .map(|parameter| {
                                parameter.bounds.iter().map(|(bound, _)| *bound).collect()
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                // Restore an extension receiver to `Function.receiver_type` so metadata keeps only
                // the declaration's logical value parameters. A value-class member's static
                // `-impl` has one backend-generated carrier before that complete declaration list;
                // the exact representation marker supplies that offset. Source-owned side tables
                // remain indexed by the declaration list and never acquire the carrier slot.
                let member_context_count = ir.fn_context_counts.get(&fid).copied().unwrap_or(0);
                let is_ext = ir.extension_receiver_fns.contains(&fid)
                    && member_context_count < metadata_params.len();
                let receiver_index = is_ext.then_some(member_context_count);
                let backend_parameter_prefix =
                    usize::from(ir.jvm_value_class_receiver_impls.contains(&fid));
                let parameter_identities = parameter_identities
                    .expect("a metadata function retains exact parameter identities");
                assert!(
                    backend_parameter_prefix + metadata_params.len() <= parameter_identities.len(),
                    "metadata declaration parameters retain their exact physical identities"
                );
                let apply_nullable = |source_index: usize, t: crate::types::Ty| {
                    if declared_nullable
                        .and_then(|v| v.get(source_index))
                        .copied()
                        .unwrap_or(false)
                    {
                        crate::types::Ty::nullable(t)
                    } else {
                        t
                    }
                };
                let receiver =
                    receiver_index.map(|index| apply_nullable(index, metadata_params[index]));
                let logical_param_indices = (0..metadata_params.len())
                    .filter_map(|index| {
                        if receiver_index == Some(index) {
                            None
                        } else {
                            Some((index, backend_parameter_prefix + index))
                        }
                    })
                    .collect::<Vec<_>>();
                let logical_params: Vec<(String, crate::types::Ty)> = logical_param_indices
                    .iter()
                    .map(|&(metadata_index, physical_index)| {
                        let identity = parameter_identities
                            .get(physical_index)
                            .expect("a metadata parameter carries an exact identity");
                        let n = if matches!(
                            identity.role,
                            crate::ir::IrParameterRole::ContextReceiver { .. }
                        ) {
                            String::new()
                        } else {
                            crate::jvm::parameter_names::metadata_owned(identity)
                                .expect("a metadata value parameter carries a semantic name")
                        };
                        (
                            n,
                            apply_nullable(metadata_index, metadata_params[metadata_index]),
                        )
                    })
                    .collect();
                let context_parameter_kinds = parameter_identities
                    .iter()
                    .skip(backend_parameter_prefix)
                    .take(member_context_count)
                    .map(crate::jvm::parameter_names::metadata_context_kind)
                    .collect();
                // Per-parameter declaration facts. Only the defaults the declaration writes itself
                // count; an override's inherited defaults stay with the declaration it overrides.
                let defaults = ir.declared_param_defaults(fid).into_iter().flat_map(|ds| {
                    let own = ds
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| receiver_index != Some(*i));
                    own.map(|(_, d)| d.is_some())
                });
                let param_modifiers = declared_value_parameters(ir, fid, defaults);
                // Recorded exactly when a reader cannot rebuild the physical descriptor from the
                // declared types (kotlinc's `requiresFunctionSignature`).
                let physical = crate::jvm::names::method_descriptor(
                    &f.params,
                    override_results.physical_result(ir, fid),
                );
                let vararg_index = ir.fn_varargs.get(&fid).map(|vararg| vararg.index);
                let jvm_sig =
                    super::super::metadata_method_signatures::requires_function_signature(
                        receiver,
                        logical_params
                            .iter()
                            .enumerate()
                            .skip(member_context_count)
                            .map(|(index, (_, ty))| match vararg_index == Some(index) {
                                true => crate::metadata::vararg_recorded_type(*ty),
                                false => *ty,
                            }),
                        metadata_ret,
                        &physical,
                        &local_classifiers,
                    )
                    .then_some(physical);
                Some(FnMeta {
                    // How SOURCE spelled this member's declared types — carried on the IR because
                    // class metadata is built without the AST (see `IrFile::fn_declared_spellings`).
                    spellings: ir
                        .fn_declared_spellings
                        .get(&fid)
                        .cloned()
                        .unwrap_or_default(),
                    name: name.to_string(),
                    params: logical_params,
                    ret: metadata_ret,
                    receiver,
                    type_params: function_type_params,
                    semantic_type_params: semantic_function_type_params,
                    type_param_bounds: function_type_param_bounds,
                    flags: function_flags(ir, fid, f) | if is_suspend { FN_IS_SUSPEND } else { 0 },
                    has_function_typed_parameter: ir.function_typed_parameter_fns.contains(&fid),
                    params_have_defaults: false,
                    param_modifiers,
                    vararg_index,
                    context_count: member_context_count,
                    context_parameter_kinds,
                    jvm_sig,
                    jvm_sig_name: (name != f.name).then(|| f.name.clone()),
                    // The declaration's own annotations, mirrored into `@Metadata`. Retention split
                    // the two class-file attributes apart (`RuntimeVisible`/`RuntimeInvisible`); the
                    // metadata record keeps ONE list, so they are rejoined here — SOURCE-retained
                    // annotations were dropped during lowering and never reach either.
                    annotations: crate::metadata::MetadataAnnotations::of_optional(
                        ir.function_annotations.get(&fid),
                    ),
                    // `metadata_params` is the DECLARED parameter list (a suspend fn's synthesized
                    // `Continuation` is already dropped), so the side table lines up with it.
                    param_annotations: logical_param_indices
                        .iter()
                        .map(|&(metadata_index, _)| {
                            ir.fn_param_annotations
                                .get(&fid)
                                .and_then(|table| table.get(metadata_index))
                                .map(crate::metadata::MetadataAnnotations::of)
                                .unwrap_or_default()
                        })
                        .collect(),
                    no_infer_params: logical_param_indices
                        .iter()
                        .map(|&(metadata_index, _)| {
                            ir.fn_param_no_infer
                                .get(&fid)
                                .and_then(|flags| flags.get(metadata_index))
                                .copied()
                                .unwrap_or(false)
                        })
                        .collect(),
                })
            })
            .collect::<Vec<_>>()
    };
    let class_ty = Ty::obj_name(c.fq_name);
    let declared_method_list = describe_methods(&declared_fids);
    let declared_method_count = declared_method_list.len();
    let inferred_methods: Vec<FnMeta> = if c.is_data {
        let mut m = Vec::new();
        for (i, property) in data_component_properties.iter().enumerate() {
            let name = format!("component{}", i + 1);
            let realization = data_class_member_realization(
                ir,
                c.fq_name_id(),
                IrDataClassMemberRole::Component(i as u32),
                &name,
                &[],
                property.ty,
            )?;
            m.push(FnMeta {
                jvm_sig_name: realization.jvm_name,
                context_count: 0,
                context_parameter_kinds: Vec::new(),
                spellings: crate::spelling::DeclaredSpellings::default(),
                name,
                params: vec![],
                ret: property.ty,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: COMPONENT_FN_FLAGS,
                has_function_typed_parameter: false,
                params_have_defaults: false,
                receiver: None,
                param_modifiers: Vec::new(),
                vararg_index: None,
                jvm_sig: realization.descriptor,
                annotations: Default::default(),
                param_annotations: Vec::new(),
                no_infer_params: Vec::new(),
            });
        }
        if synthesizes_copy {
            let declared = data_component_properties
                .iter()
                .map(|property| property.ty)
                .collect::<Vec<_>>();
            let realization = data_class_member_realization(
                ir,
                c.fq_name_id(),
                IrDataClassMemberRole::Copy,
                "copy",
                &declared,
                class_ty,
            )?;
            m.push(FnMeta {
                jvm_sig_name: realization.jvm_name,
                context_count: 0,
                context_parameter_kinds: Vec::new(),
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "copy".into(),
                params: data_component_properties
                    .iter()
                    .map(|property| (property.name.clone(), property.ty))
                    .collect(),
                ret: class_ty,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: data_copy_fn_flags(ir, c),
                has_function_typed_parameter: false,
                params_have_defaults: true,
                receiver: None,
                param_modifiers: Vec::new(),
                vararg_index: None,
                jvm_sig: realization.descriptor,
                annotations: Default::default(),
                param_annotations: Vec::new(),
                no_infer_params: Vec::new(),
            });
        }
        if ir
            .data_class_member(c.fq_name_id(), IrDataClassMemberRole::Equals)
            .is_some()
        {
            m.push(FnMeta {
                context_count: 0,
                context_parameter_kinds: Vec::new(),
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "equals".into(),
                params: vec![("other".into(), Ty::nullable(Ty::obj("kotlin/Any")))],
                ret: Ty::Boolean,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: EQUALS_FN_FLAGS,
                has_function_typed_parameter: false,
                params_have_defaults: false,
                receiver: None,
                param_modifiers: Vec::new(),
                vararg_index: None,
                jvm_sig: None,
                jvm_sig_name: None,
                annotations: Default::default(),
                param_annotations: Vec::new(),
                no_infer_params: Vec::new(),
            });
        }
        if ir
            .data_class_member(c.fq_name_id(), IrDataClassMemberRole::HashCode)
            .is_some()
        {
            m.push(FnMeta {
                context_count: 0,
                context_parameter_kinds: Vec::new(),
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "hashCode".into(),
                params: vec![],
                ret: Ty::Int,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: HASHCODE_TOSTRING_FN_FLAGS,
                has_function_typed_parameter: false,
                params_have_defaults: false,
                receiver: None,
                param_modifiers: Vec::new(),
                vararg_index: None,
                jvm_sig: None,
                jvm_sig_name: None,
                annotations: Default::default(),
                param_annotations: Vec::new(),
                no_infer_params: Vec::new(),
            });
        }
        if ir
            .data_class_member(c.fq_name_id(), IrDataClassMemberRole::ToString)
            .is_some()
        {
            m.push(FnMeta {
                context_count: 0,
                context_parameter_kinds: Vec::new(),
                spellings: crate::spelling::DeclaredSpellings::default(),
                name: "toString".into(),
                params: vec![],
                ret: Ty::String,
                type_params: Vec::new(),
                semantic_type_params: Vec::new(),
                type_param_bounds: Vec::new(),
                flags: HASHCODE_TOSTRING_FN_FLAGS,
                has_function_typed_parameter: false,
                params_have_defaults: false,
                receiver: None,
                param_modifiers: Vec::new(),
                vararg_index: None,
                jvm_sig: None,
                jvm_sig_name: None,
                annotations: Default::default(),
                param_annotations: Vec::new(),
                no_infer_params: Vec::new(),
            });
        }
        // kotlinc visits the source declarations first, then the members the compiler generates.
        let mut methods = declared_method_list;
        methods.extend(m);
        methods
    } else if c.is_value {
        let mut declared = declared_method_list;
        declared.extend(value_class_override_metadata::functions(&desc(
            c.fields[0].ty,
        )));
        declared
    } else {
        declared_method_list
    };
    let mut methods = match generated_publication.map(|publication| publication.metadata_scope) {
        Some(crate::ir::IrGeneratedFunctionMetadataScope::Exclusive) => Vec::new(),
        Some(crate::ir::IrGeneratedFunctionMetadataScope::Additive) | None => inferred_methods,
    };
    let inferred_method_count = methods.len();
    if let Some(publication) = generated_publication {
        methods.extend(super::super::generated_member_metadata::functions(
            ir,
            publication,
        ));
    }
    let mut delegation = describe_methods(&metadata_member_order::delegation_functions(ir, c));
    metadata_member_order::sort_delegation_functions(&mut delegation);
    let delegation_functions = methods.len()..methods.len() + delegation.len();
    methods.extend(delegation);
    let type_aliases = ir
        .class_type_aliases
        .get(&c.fq_name_id())
        .into_iter()
        .flatten()
        .map(|alias| crate::metadata::builder::TypeAliasMeta {
            name: alias.name.clone(),
            formals: alias.formals.clone(),
            expansion: alias.expansion,
            visibility: alias.visibility,
            expansion_spelling: alias.expansion_spelling.clone(),
            decl_order: alias.source_order as usize,
        })
        .collect::<Vec<_>>();
    let delegation_properties =
        metadata_member_order::sort_delegation_properties(&mut props, &mut prop_source_orders);
    let member_order = metadata_member_order::member_order(
        ir,
        c,
        &prop_source_orders,
        &declared_fids,
        metadata_member_order::MemberRanges {
            synthesized: declared_method_count..inferred_method_count,
            delegation_functions,
            delegation_properties,
        },
        generated_publication,
        &type_aliases,
    );
    // A value class's primary constructor is realized as the static `constructor-impl` returning the
    // erased underlying, not `<init>`; its `@Metadata` signature records that.
    let vc_ctor_desc = c
        .is_value
        .then(|| format!("({0}){0}", desc(c.fields[0].ty)));
    let enum_entry_meta = enum_metadata::entries(c);
    // Metadata keeps nested declarations ordered and sealed subclasses sorted.
    // Every DECLARED direct nested classifier joins `Class.nestedClassName` (f7) — kotlinc records
    // them all, not only sealed subtypes. Declaration origin and the exact identity-tree relation
    // keep synthesized classes out without interpreting their backend spellings.
    // Common IR carries the stable declaration order selected by the frontend. The IR arena and
    // debug lines are representation facts and do not define this metadata order.
    let mut source_nested: Vec<(u32, String)> = ir
        .classes
        .iter()
        .enumerate()
        .filter(|(_, candidate)| {
            // A classifier declared in executable code is nobody's member; one nested in a local
            // class is that class's member like any other.
            candidate.is_source_declared
                && candidate.enclosure.is_none()
                && candidate.fq_name.nested_owner() == Some(c.fq_name)
        })
        .map(|(index, candidate)| {
            (
                ir.class_source_order(index as crate::ir::ClassId)
                    .expect("a source classifier carries its stable declaration order"),
                candidate.fq_name.nested_segment_ref().to_string(),
            )
        })
        .collect();
    source_nested.sort_by_key(|(source_order, _)| *source_order);
    let mut nested_names: Vec<String> = source_nested
        .into_iter()
        .map(|(_, segment)| segment)
        .collect();
    // A producer can generate a classifier that Kotlin code names (`Foo.$serializer`) and publish
    // that fact on the owning class. Other synthesized implementation classes stay out on their
    // `is_source_declared` record alone. Generated names precede the COMPANION, which kotlinc lists
    // last of that set (`$serializer` then `Companion` for a `@Serializable` class).
    let generated_nested = c.published_nested_classifiers.iter().cloned();
    // kotlinc lists the companion under `nestedClassName` (f7) TOO, alongside its own
    // `companionObjectName` (f4) record — both reference the same interned string.
    let companion_segment = c
        .companion_class
        .as_ref()
        .map(|companion| companion.nested_segment_ref().to_string());
    let at = companion_segment
        .as_ref()
        .and_then(|segment| nested_names.iter().position(|name| name == segment))
        .unwrap_or(nested_names.len());
    nested_names.splice(at..at, generated_nested);
    if let Some(segment) = companion_segment {
        if !nested_names.contains(&segment) {
            nested_names.push(segment);
        }
    }
    // `Class.sealedSubclassFqName` (f16) belongs only to a SEALED classifier — the IR records
    // subtype relationships for every class, but kotlinc writes the field for sealed ones alone
    // (a plain interface with implementors carries none).
    let sealed_sorted = if c.is_sealed {
        sorted_sealed_subclass_ids(c)
    } else {
        Vec::new()
    };
    let nested_refs: Vec<&str> = nested_names.iter().map(String::as_str).collect();
    let enclosing_type_parameters =
        super::super::local_classifiers::enclosing_type_parameters(ir, c);
    let class_type_parameters = ir
        .class_signature(&c.fq_name())
        .map(|signature| signature.type_params.as_slice())
        .unwrap_or_default();
    // `c.fields` is the JVM storage realization by this point: value-class lowering may replace a
    // generic underlying parameter with the carrier of its bound. Kotlin metadata instead describes
    // the source property. Keep those two facts separate, especially for `V<T : U<Int>>(val value: T)`:
    // the physical field may use `U`'s carrier, but Class.inlineClassUnderlyingType must still name
    // this class's own `T` identity.
    let inline_underlying = c.is_value.then(|| {
        let property = c
            .properties
            .iter()
            .find(|property| property.backing_field == Some(0))
            .expect("a value class must retain its semantic underlying property");
        (
            property.name.as_str(),
            (!property.visibility.is_public_api()).then_some(property.ty),
        )
    });
    let superclass = c.superclass;
    let any = crate::types::wk::any();
    let mut supertypes = ir
        .class_signature(&c.fq_name())
        .filter(|signature| !signature.supers.is_empty())
        .map(|signature| signature.supers.clone())
        .unwrap_or_default();
    // A generic class's recorded supers ALWAYS materialize the superclass position — holding
    // `kotlin/Any` when none was declared — because a JVM class `Signature` must name a superclass.
    // `@Metadata` records only the supertypes source DECLARED, so drop that implicit `Any`; leaving
    // it in shows every consumer a supertype the declaration never wrote. Both shapes then agree:
    // a superclass slot exists exactly when one was declared.
    if superclass == any
        && supertypes
            .first()
            .is_some_and(|first| matches!(first, Ty::Obj(n, _) if *n == any))
    {
        supertypes.remove(0);
    }
    if supertypes.is_empty() {
        if superclass != any {
            supertypes.push(Ty::obj_name(superclass));
        }
        supertypes.extend(c.interfaces.iter_ids().map(Ty::obj_name));
    }
    // The header's spellings have to follow the same shape, or every abbreviation lands on the
    // neighbouring supertype.
    let has_declared_superclass = superclass != any;
    let class_spellings = ir
        .class_declared_spellings
        .get(&c.fq_name_id())
        .cloned()
        .unwrap_or_default();
    let mut supertype_spellings = class_spellings.supertype_spellings(has_declared_superclass);
    // Both lists lead with the superclass; `@Metadata` lists it where the source wrote it.
    let superclass_position = ir.class_superclass_positions.get(&c.fq_name_id());
    if let Some(position) = superclass_position.filter(|_| has_declared_superclass) {
        let position = *position as usize;
        // An interface without a spelling has no entry yet; it must not shift the superclass's.
        if supertype_spellings.len() <= position {
            supertype_spellings.resize(position + 1, crate::spelling::Spelled::default());
        }
        supertypes[..=position].rotate_left(1);
        supertype_spellings[..=position].rotate_left(1);
    }
    let secondary_ctor_shapes =
        super::super::constructor_metadata::secondary_constructor_shapes(ir, c);
    let secondary_ctor_metas: Vec<crate::metadata::class_builder::CtorMeta> = secondary_ctor_shapes
        .iter()
        .map(|shape| crate::metadata::class_builder::CtorMeta {
            params: &shape.params,
            param_defaults: &shape.param_defaults,
            desc: &shape.descriptor,
            sig_name: shape.signature_name,
            vararg_index: shape.vararg_index,
            flags: shape.flags,
            annotations: &shape.annotations,
        })
        .collect();
    let metadata_annotations = crate::metadata::MetadataAnnotations::of(&c.applied_annotations);
    let primary_ctor_metadata_annotations = crate::metadata::MetadataAnnotations::with_records(
        &c.primary_ctor_annotations,
        primary_ctor_annotations(c),
    );
    let signature_formatter = JvmSignatureFormatter::with_symbols(ir, signature_symbols, run);
    let approximate_intersection = |ty| signature_formatter.declaration_approximation(ty);
    let (d1_bytes, d2) = build_class(
        c.fq_name_id(),
        &ctor_params,
        vc_ctor_desc.as_deref().unwrap_or(&ctor_desc),
        &props,
        &methods,
        &enum_entry_meta,
        &ClassTail {
            intersection_approximation: Some(&approximate_intersection),
            spellings: class_spellings,
            supertype_spellings: &supertype_spellings,
            type_params: &c.type_params,
            type_param_bounds: class_type_parameters,
            captured_type_params: super::super::local_classifiers::captured_type_parameters(
                c,
                &enclosing_type_parameters,
            ),
            ctor_param_tparams: &ctor_param_tparams,
            ctor_param_annotations: &named_ctor_param_annotations,
            flags: class_metadata_flags(ir, c),
            // An `enum class`'s primary ctor is private too — entries are the only instances.
            // A DECLARED constructor visibility (`class C protected constructor(…)`) takes
            // precedence: the consumer must reject constructions the declaration forbids.
            primary_ctor_flags: match ir.ctor_visibilities.get(&c.fq_name_id()) {
                Some(crate::types::Visibility::Protected) => SEALED_CTOR_FLAGS,
                Some(crate::types::Visibility::Private) => OBJECT_CTOR_FLAGS,
                _ if c.is_sealed => SEALED_CTOR_FLAGS,
                _ if c.is_singleton() || c.is_enum => OBJECT_CTOR_FLAGS,
                _ => 0,
            },
            primary_ctor_jvm_signature: !c.is_annotation,
            module_name: opts.module_name.as_deref(),
            ctor_param_defaults: &ctor_param_defaults,
            inline_underlying,
            ctor_sig_name: c.is_value.then_some("constructor-impl"),
            // An interface has no constructor; a class with ONLY secondary constructors emits no
            // primary record either (its `Class.constructor` entries are the secondaries below).
            // Every other class keeps its (possibly implicit) primary record — an `enum class`
            // without a declared constructor still records the implicit private `(String, I)` one.
            // An anonymous object's constructor (an enum entry body's too) is not callable.
            emit_primary_ctor: !c.is_interface
                && !c.is_anonymous_object
                && !c.is_enum_entry
                && (c.has_primary_ctor || c.secondary_ctors.is_empty()),
            // `jvmClassFlags` describes the interface SHAPE this compilation produced, so it tracks
            // `-jvm-default` exactly: a consumer reads it to know whether method bodies live on the
            // interface and whether a `$DefaultImpls` compatibility copy exists.
            jvm_class_flags: c
                .is_interface
                .then(|| opts.jvm_default.interface_jvm_class_flags())
                .flatten(),
            // Kotlin 1.4 introduced JVM default methods without compatibility holders. Older
            // consumers must reject this metadata instead of assuming the legacy `$DefaultImpls`
            // realization, so kotlinc attaches a compiler-version requirement to every interface.
            compiler_version_requirement: (c.is_interface
                && opts.jvm_default == JvmDefaultMode::NoCompatibility)
                .then_some(
                    crate::metadata::version_requirements::VersionRequirement::compiler(1, 4, 0),
                ),
            param_assertions: opts.param_assertions,
            // A class with a companion records its simple name (`Class.companionObjectName`, f4) —
            // the consumer resolves `C.member` through it.
            companion: c
                .companion_class
                .as_ref()
                .map(|companion| companion.nested_segment_ref()),
            secondary_ctors: &secondary_ctor_metas,
            ctor_vararg_index,
            nested: &nested_refs,
            member_order: &member_order,
            type_aliases: &type_aliases,
            sealed_subclasses: &sealed_sorted,
            supertypes: &supertypes,
            annotations: &metadata_annotations,
            primary_ctor_annotations: &primary_ctor_metadata_annotations,
            local_properties: locals.of(c.fq_name_id()),
            local_classifiers: &local_classifiers,
            enum_entry_bodies: &super::super::local_classifiers::enum_entry_bodies(ir),
            is_enum: c.is_enum,
            // kotlinc's `LanguageFeature.AnnotationsInMetadata` (since language level 2.4): an
            // older source-language level keeps the HAS_ANNOTATIONS flags but writes no records.
            // The physical metadata stamp is deliberately not consulted here.
            annotations_in_metadata: opts.annotations_in_metadata,
        },
    );
    // d1 is the protobuf payload as one `char` per byte (the constant pool writes it as modified-UTF-8).
    let d1 = vec![d1_bytes.iter().map(|&b| b as char).collect()];
    Some(KotlinMetadata {
        k: 1,
        mv: opts.metadata_version().to_vec(),
        xi: 48,
        d1,
        d2,
    })
}

/// The `@Metadata` annotations of a PROPERTY's own applications. The records name the same
/// applications the property's `get<Name>$annotations()` marker carries; a property whose only
/// application is an erased optional expectation has no marker yet still declares annotations.
fn property_metadata_annotations(
    c: &crate::ir::IrClass,
    property: &str,
) -> crate::metadata::MetadataAnnotations {
    crate::metadata::MetadataAnnotations::of_optional(
        c.property_annotations
            .iter()
            .find(|annotations| annotations.property == property)
            .map(|annotations| &annotations.annotations),
    )
}

/// The `@Metadata` annotations that landed on a property's BACKING FIELD.
fn property_backing_field_annotations(
    c: &crate::ir::IrClass,
    property: &str,
) -> crate::metadata::MetadataAnnotations {
    crate::metadata::MetadataAnnotations::of_optional(
        c.field_annotations
            .iter()
            .find(|annotations| annotations.field == property)
            .map(|annotations| &annotations.annotations),
    )
}
