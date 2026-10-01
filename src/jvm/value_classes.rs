//! JVM `@JvmInline value class` IR lowering pass — an **optional, JVM-only** IR→IR transform.
//!
//! Common lowering keeps a value class as a plain `Class{X}` so common IR stays target-neutral (a JS
//! backend, or a future Valhalla JVM with *native* value types, leaves value classes alone). The old
//! JVM has no native value types, so this pass realizes kotlinc's unboxed representation:
//!   * a NON-nullable `X` erases to its single field's (underlying) type `U` everywhere — signatures,
//!     fields, locals (a nullable `X?` stays the boxed `Class{X}`);
//!   * `new X(arg)` becomes `X.constructor-impl(arg): U` (the unboxed value);
//!   * sole-property access on an unboxed value (`x.v`) is identity (the value already IS the `U`);
//!   * a value-class parameter that erased to a primitive loses its non-null `checkNotNullParameter`.
//!
//! The value class's own synthesized members (`box-impl`/`unbox-impl`/`constructor-impl`/getter/`<init>`)
//! genuinely operate on the boxed object, so they are NOT
//! rewritten (only their signatures erase, and `box-impl`'s return stays the boxed `X`).

mod accessor_names;
mod bridge_names;
mod bridge_parameters;
mod bridge_realization;
mod bridge_returns;
mod call_arguments;
mod call_result_boundaries;
mod call_results;
mod constructor_arguments;
mod constructor_bodies;
mod declaration_inventory;
mod default_calls;
mod default_constructions;
mod descriptor_parameters;
mod equality;
mod function_references;
mod hidden_constructors;
mod inline_body_slots;
mod interface_entries;
mod member_names;
mod module_members;
mod operand_nullness;
mod property_references;
mod representation;
use representation::is_ref;
mod result_tail_boxing;
mod return_unboxing;
mod substitution_coercions;
mod suspend_results;
mod synth_members;
mod type_operation_roles;
mod unboxing_rewrites;
use crate::ir::{value_tails, Callee, ExprId, IrExpr, IrFile};
use crate::jvm::method_descriptors::jvm_tys;
use crate::jvm::names::{method_descriptor, property_getter_name, type_descriptor};
use crate::jvm::operation_relocation::clone_below_representation_wrapper;
use crate::jvm::physical_type::ir_ty_to_jvm;
use crate::libraries::{InlineKind, SemanticCallRole};
use crate::types::{existing_type_name, type_name, Ty, TypeName};
use call_results::CallTypes;
use member_names::{vc_mangle, vc_mangle_once, vc_member_entry_name, vc_member_impl_name};
pub(crate) use module_members::{forwarded_member_types, module_member_jvm_name};
use operand_nullness::{operand_nonnull, operand_null_only};
use representation::erase;
pub(crate) use representation::{
    boxed_value_class_carrier, boxed_value_class_names, boxed_value_class_underlying,
    instance_representation, is_boxed_value_class, type_operation_internal_name,
};
use result_tail_boxing::box_vc_tail;
use std::collections::{HashMap, HashSet};
use suspend_results::{record_suspend_results, suspend_result_representation};
use unboxing_rewrites::{narrow_wrap, unbox_call, unbox_wrap, unbox_wrap_nullable};

/// The stdlib value classes whose underlying is JVM-native unsigned (no synthesized `-impl` members —
/// their box/unbox lives on the classpath). All erase to a signed primitive, so they contribute nothing
/// to the erasure map and are skipped when probing referenced classes.
type Under = crate::value_classes::UnderlyingTypes;

struct JvmUnderlyingProjection;

impl crate::value_classes::RepresentationPolicy for JvmUnderlyingProjection {
    fn project_nullable(
        &self,
        classifier: TypeName,
        _underlying: Ty,
        declarations: &Under,
    ) -> bool {
        !nullable_is_boxed(classifier, declarations)
    }
}

/// JVM reference form of a value-class classifier. Native unsigned classifiers are also semantic
/// scalars, so their nullable form is the only `Ty` spelling that denotes their boxed reference slot;
/// ordinary value classes are references already.
fn boxed_value_ty(fq: TypeName) -> Ty {
    let ty = Ty::obj_name(fq);
    if ty.is_jvm_scalar() {
        Ty::nullable(ty)
    } else {
        ty
    }
}

/// Whether an underlying fq name is an IEEE floating-point type. A value class over `Float`/`Double`
/// compares by IEEE TOTAL ORDER (`NaN == NaN`, `0.0 != -0.0`) via `{Float,Double}.compare`, NOT a raw
/// `fcmp`/`dcmp` — matching kotlinc's `equals-impl0`.
fn is_ieee_fp(fq: TypeName) -> bool {
    fq.matches("kotlin/Float") || fq.matches("kotlin/Double")
}

fn value_class_name(internal: TypeName, under: &Under) -> Option<TypeName> {
    under.contains_key(&internal).then_some(internal)
}

fn is_value_class_internal(internal: TypeName, under: &Under) -> bool {
    value_class_name(internal, under).is_some()
}

/// `(class-index, method-index)` → value-class field type for value-class-FIELD getters of the file
/// being lowered (built once in [`lower_value_classes`], carried in [`ReprCtx`] and threaded to [`repr`]/
/// [`is_boxed_vc`]). A `MethodCall` to such a getter reprs as the field's representation — an unboxed
/// underlying. Keyed on the getter's IDENTITY (owning class + method slot), not its name, so a
/// coincidentally-named boxing override does not collide.
type FieldGetters = HashMap<(u32, u32), Ty>;

#[must_use]
/// Lower all `@JvmInline value class` usage in `ir` to the JVM's unboxed representation: erase the
/// value-class type to its single field's type, rewrite construction/sole-property access, and insert
/// box/unbox at the representation boundaries this pass models. The `bool` result is reserved for a
/// future structural bail; today it always returns `true`.
pub(crate) fn lower_value_classes(
    ir: &mut IrFile,
    classifiers: &dyn crate::types::ClassifierFactSource,
    // Same-module SOURCE value classes (internal name → sole-field underlying), collected from the
    // frontend symbols. A value class declared in ANOTHER file of this module is not in `ir.classes`;
    // the normalized classifier provider supplies its declaration facts without leaking provider
    // origin into representation decisions.
    module_value_classes: &std::collections::HashMap<TypeName, Ty>,
    // Subset whose stable declaration headers describe a value-class shape supported by metadata
    // emission. This is frozen before Pass 2; no sibling body or source coordinate is retained.
    module_readable_value_classes: &std::collections::HashSet<TypeName>,
    bridge_adaptations: &mut crate::jvm::bridge_adaptations::BridgeAdaptations,
    override_results: &mut crate::jvm::override_results::OverrideResults,
    // What the property-reference pass already selected for each synthesized reference class.
    // Realizing those accessors over a carrier is the one thing left to decide about them, and it
    // is decided from these recorded facts rather than from the reference's spelling.
    property_reference_realizations: &mut crate::jvm::property_references::PropertyReferenceRealizations,
) -> bool {
    crate::trace_compiler!(
        "value_classes",
        "lower start classes={} functions={} expressions={}",
        ir.classes.len(),
        ir.functions.len(),
        ir.exprs.len()
    );
    for (id, expression) in ir.exprs.iter().enumerate() {
        match expression {
            IrExpr::PropertyRead { owner, name, .. } => crate::trace_compiler!(
                "value_classes",
                "input property read {id}: {}.{}",
                owner,
                name
            ),
            IrExpr::Call {
                callee: Callee::Virtual { owner, name, .. },
                ..
            } => crate::trace_compiler!(
                "value_classes",
                "input virtual call {id}: {}.{}",
                owner,
                name
            ),
            _ => {}
        }
    }
    // Merge classpath `@JvmInline value class`es referenced by this file (`Result` → `Object`). They are
    // NOT in `ir.classes` (no synthesized members — their `-impl`/`box-impl` live on the classpath), so
    // they only contribute to the erasure map: every occurrence of their type erases to the underlying.
    // Every referenced classifier is probed through the normalized checked-fact boundary; the JVM
    // pass never opens source, metadata, or a classpath to rediscover semantic declarations.
    let mut under = representation::declared_underlyings(ir);
    let Some(external_underlying_properties) =
        declaration_inventory::merge_referenced(ir, classifiers, &mut under)
    else {
        return false;
    };
    // Native unsigned classes share ordinary primitive carriers in expressions, but cross boxed
    // FunctionN/property-reference ABI slots like value classes. Keep them out of the global rewrite
    // map and add them only to the callable-boundary map.
    let mut callable_under = under.clone();
    for semantic in [Ty::UByte, Ty::UShort, Ty::UInt, Ty::ULong] {
        callable_under.insert(
            semantic
                .kotlin_class_internal()
                .expect("native unsigned scalar must name its Kotlin classifier"),
            semantic
                .scalar_value_repr()
                .expect("native unsigned scalar must have a carrier"),
        );
    }
    if under.is_empty() && callable_under.is_empty() {
        return true;
    }
    call_result_boundaries::realize(ir, &callable_under);
    type_operation_roles::record_primitive_array_allocation_carriers(ir, &under);
    // Publish only the distinction the existing unified value-class lookup cannot answer: which
    // resolved value classes belong to this source module. `IrFile::is_value_class_name` already
    // recognizes same-file and external/module declarations, so copying `under` into a second public
    // name table would create two semantic authorities that can drift. The metadata writer combines
    // that existing lookup with this origin subset when deciding whether a downstream reader can see
    // the value-class record.
    ir.module_source_value_classes = under
        .keys()
        .copied()
        .filter(|fq_name| {
            module_value_classes.contains_key(fq_name)
                || ir
                    .classes
                    .iter()
                    .any(|c| c.is_value && c.fq_name == *fq_name)
        })
        .collect();
    ir.module_readable_value_classes = module_readable_value_classes.clone();

    // A TOP-LEVEL property of value-class type is realized over the CARRIER, exactly as kotlinc
    // realizes it: `private static int topLevel`, `getTopLevel()I`, `setTopLevel-<hash>(int)`. The
    // DECISION is recorded on the declaration here, before any boxing decision can read it; the
    // storage itself is erased further down, once the initializer rewrites that still speak in the
    // declared type are done. `repr` consults this mark so a read of such a field is `Unboxed` —
    // otherwise step 5 inserts an `Integer.valueOf; checkcast X; unbox-impl` adapter over a carrier
    // that was never boxed.
    //
    // A COMPANION property keeps the BOXED field (`LX;`) and has its initializer boxed to match:
    // only the top-level one lives on the file facade kotlinc erases, and holding both under one
    // rule is what named a `getTopLevel()LZ;` no declaration had.
    for property in &mut ir.statics {
        if !property.is_facade_owned() || property.accessors.any_declared() {
            continue;
        }
        // Derived from the erasure that actually happens, never from stripping nullability. `erase`
        // owns the rule and `nullable_is_boxed` is its single source of truth: `Z?` over a SCALAR
        // carrier stays the boxed `LZ;`, because a primitive cannot carry null, while `S?` over a
        // reference carrier erases to that reference, which carries null itself. Null-stripping
        // answered "carrier" for both, so a read of the boxed field reported `Unboxed` and step 5
        // unboxed a value nothing had boxed.
        let declared = property.ty;
        property.erased_declared_ty = declared
            .non_null()
            .obj_internal()
            .filter(|fq_name| under.contains_key(fq_name))
            .filter(|_| erase(&declared, &under) != declared)
            .map(|_| declared);
    }

    // A semantic property operation deliberately keeps the Kotlin property name. For an owner compiled
    // from another source file there is no classfile for the emitter to inspect, so record the JVM
    // accessor spelling here while the original property type is still present. The emitter consults
    // this table only as its declaration-less fallback; same-file declarations and classpath metadata
    // remain authoritative. Keeping this target fact in a JVM-pass side table prevents common lowering
    // from branching on whether the owner came from this file, another module file, or the classpath.
    let property_accessor_realizations = ir
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(id, expression)| {
            let operation = match expression {
                IrExpr::PropertyRead { operation, .. }
                | IrExpr::PropertyWrite { operation, .. } => operation.unwrap_or(id as u32),
                _ => return None,
            };
            let accessor = match expression {
                IrExpr::PropertyRead { name, ty, .. }
                    if ty
                        .non_null()
                        .obj_internal()
                        .is_some_and(|owner| under.contains_key(&owner)) =>
                {
                    vc_mangle(&property_getter_name(name), &[], ty, &under, false, false)
                }
                IrExpr::PropertyWrite { name, ty, .. }
                    if ty
                        .non_null()
                        .obj_internal()
                        .is_some_and(|owner| under.contains_key(&owner)) =>
                {
                    vc_mangle(
                        &crate::names::property_setter_name(name),
                        std::slice::from_ref(ty),
                        &Ty::Unit,
                        &under,
                        false,
                        false,
                    )
                }
                _ => return None,
            };
            // Record the erased property value beside the name: reads deliberately retain their logical
            // value-class type in the IR, so the declaration-less emitter cannot derive the descriptor
            // from the node after this pass.
            let physical = match expression {
                IrExpr::PropertyRead { ty, .. } | IrExpr::PropertyWrite { ty, .. } => {
                    representation::carrier_slot(*ty, &under).unwrap_or_else(|| erase(ty, &under))
                }
                _ => unreachable!("the accessor match above accepted only property operations"),
            };
            Some((id as u32, operation, accessor, physical))
        })
        .collect::<Vec<_>>();
    for (expression, operation, accessor, physical) in property_accessor_realizations {
        if matches!(ir.expr(expression), IrExpr::PropertyRead { .. }) {
            ir.physical_types.insert(expression, physical);
        }
        ir.property_accessor_jvm_realizations
            .insert(operation, (accessor, physical));
    }

    let value_class_ids: Vec<u32> = (0..ir.classes.len() as u32)
        .filter(|&i| ir.classes[i as usize].is_value)
        .collect();

    // Exact identities of members whose JVM realization synthesis already finalized.
    let mut realized_members = synth_members::SynthesizedValueMembers::default();
    // Synthesize each value class's `-impl`/`equals`/`hashCode`/`toString` members up front (a JVM
    // concern — common lowering only emits the plain single-field class). Done before the analysis below so
    // they participate in `vc_methods`/erasure like any other method.
    for cid in value_class_ids {
        // A real value class always has its single backing field; guard malformed fieldless input.
        if ir.classes[cid as usize].fields.is_empty() {
            continue;
        }
        let classifier = ir.classes[cid as usize].fq_name_id();
        let constructor_default = match ir.take_class_ctor_defaults_name(classifier) {
            None => None,
            Some(defaults) if defaults.len() == 1 => defaults[0],
            Some(_) => {
                crate::trace_compiler!(
                    "value_classes",
                    "reject value-class constructor with a non-canonical parameter layout"
                );
                return false;
            }
        };
        if let Some(default) = constructor_default {
            // Common IR records every primary-constructor default in the ordinary instance frame.
            // A JVM value class realizes that constructor as static `constructor-impl`, so remove
            // the absent `this` slot exactly here, at the representation boundary.
            synth_members::shift_slots(ir, default);
        }
        crate::trace_compiler!(
            "value_classes",
            "synthesize {} fields={:?} type-params={:?} secondary-ctors={}",
            ir.classes[cid as usize].fq_name.render(),
            ir.classes[cid as usize]
                .fields
                .iter()
                .map(|field| (field.name.as_str(), field.ty, field.type_param.as_deref()))
                .collect::<Vec<_>>(),
            ir.classes[cid as usize].type_params,
            ir.classes[cid as usize].secondary_ctors.len()
        );
        if !synth_members::synth_value_members(
            ir,
            cid,
            &under,
            &callable_under,
            ir.classes[cid as usize].init_body.is_some(),
            constructor_default,
            &mut realized_members,
        ) {
            crate::trace_compiler!(
                "value_classes",
                "synthesis rejected {}",
                ir.classes[cid as usize].fq_name.render()
            );
            return false;
        }
    }
    synth_members::enclose_in_constructor_impls(ir, &realized_members);

    // Pre-erasure signatures, so box/unbox at call boundaries can see `Object`/generic param/field
    // types (which erasure leaves alone but values flowing in must be boxed to reach).
    let mut orig_params: Vec<Vec<Ty>> = ir.functions.iter().map(|f| f.params.clone()).collect();
    let orig_fields: Vec<Vec<Ty>> = ir
        .classes
        .iter()
        .map(|c| c.fields.iter().map(|f| f.ty).collect())
        .collect();
    // Pre-erasure constructor-parameter types per class (parallel to `ir.classes`) — the slot types for
    // an `init { … }` block's box/unbox analysis (slot 0 = `this`, slots 1.. = the ctor params).
    let orig_ctor_args: Vec<Vec<Ty>> = ir
        .classes
        .iter()
        .map(|c| c.ctor_args.iter().map(|a| a.ty).collect())
        .collect();
    for (class, params) in ir.classes.iter().zip(&orig_ctor_args) {
        crate::trace_compiler!(
            "value_classes",
            "class {} constructor params={params:?}",
            class.fq_name.render()
        );
    }
    // Pre-erasure secondary-constructor parameter types (class → ctor → params) — slot types for a
    // regular class's secondary-`<init>` body/delegation box/unbox (slot 0 = `this`, slots 1.. = params).
    let orig_secondary: Vec<Vec<Vec<Ty>>> = ir
        .classes
        .iter()
        .map(|c| {
            c.secondary_ctors
                .iter()
                .map(|s| s.prefix_params.iter().chain(&s.params).copied().collect())
                .collect()
        })
        .collect();
    // Constructor-owned bodies' slot types, captured before expression erasure.
    let constructor_slots =
        constructor_bodies::ConstructorSlots::capture(ir, &orig_ctor_args, &orig_secondary);
    // Top-level initializers also own compiler-generated temporaries (notably the array-constructor
    // fill loop). Capture their semantic slot types before expression erasure; rebuilding this map
    // later would turn `Array<Value>` into its carrier-shaped type and hide the boxed reference-array
    // element boundary from `ArraySet`.
    let orig_static_slots = ir
        .statics
        .iter()
        .map(|property| constructor_bodies::slot_map(&ir.exprs, property.init, &[]))
        .collect::<Vec<_>>();

    // Value-class-FIELD getters: `(class-index, method-index)` → the field's (pre-erasure) value-class
    // type, for a plain class's property whose type is a value class. A read of one (`Test(val s: S<T>).s`
    // → `invokevirtual Test.getS()`) yields the field's UNBOXED representation (the field stores the erased
    // underlying) — UNLIKE a boxed value-class member read or a BOXING override getter (whose body isn't a
    // plain field read). `repr` consults this so a redundant `Cast` over such a getter strips and the sole-
    // field access is identity, keyed on the getter IDENTITY rather than the ambiguous static type.
    let field_getters: FieldGetters = {
        let mut m = FieldGetters::new();
        for (ci, c) in ir.classes.iter().enumerate() {
            // getter-name → (field-index, value-class field type) for value-class-typed fields.
            let getters: HashMap<String, (u32, Ty)> = c
                .fields
                .iter()
                .enumerate()
                .filter_map(|(fi, f)| {
                    let fty = orig_fields[ci][fi];
                    fty.non_null()
                        .obj_internal()
                        .filter(|i| under.contains_key(i))
                        .map(|_| (property_getter_name(&f.name), (fi as u32, fty)))
                })
                .collect();
            for (mi, &fid) in c.methods.iter().enumerate() {
                if let Some(&(fi, fty)) = getters.get(&ir.functions[fid as usize].name) {
                    // Guard against a coincidentally-named method (a BOXING override, a user method): the
                    // body must actually READ that field. A plain field getter's reachable body contains a
                    // `GetField` of `(ci, fi)`; a boxing override does not (it box-impls a value instead).
                    let field_name = ir.classes[ci].fields[fi as usize].name.clone();
                    let owner = ir.classes[ci].fq_name;
                    let reads_field = ir.functions[fid as usize].body.is_some_and(|b| {
                        let mut reach = HashSet::new();
                        collect_reachable(&ir.exprs, b, &mut reach);
                        reach.iter().any(|&e| match &ir.exprs[e as usize] {
                            IrExpr::GetField { class, index, .. } => {
                                *class as usize == ci && *index == fi
                            }
                            // The same read expressed as a property of this class.
                            IrExpr::PropertyRead { owner: o, name, .. } => {
                                *o == owner && *name == field_name
                            }
                            _ => false,
                        })
                    });
                    if reads_field {
                        m.insert((ci as u32, mi as u32), fty);
                    }
                }
            }
        }
        m
    };

    // Per-class id metadata (parallel to ir.classes).
    let is_vc: Vec<bool> = ir.classes.iter().map(|c| c.is_value).collect();
    let fq: Vec<TypeName> = ir.classes.iter().map(|c| c.fq_name).collect();
    // Resolve an `IrExpr::New`'s owner NAME back to its in-IR `ClassId` (the node no longer carries the
    // index). Only SAME-FILE classes are present; an external/other-module owner yields `None`.
    let cls_by_name: HashMap<TypeName, usize> =
        fq.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    // Getter method name for each value class's sole field (`getV`), to recognize property access.
    let getter: Vec<Option<String>> = ir
        .classes
        .iter()
        .map(|c| {
            if c.is_value {
                c.fields.first().map(|f| property_getter_name(&f.name))
            } else {
                None
            }
        })
        .collect();
    // Function identities for the stored-property getters above. A source operator/member may also be
    // named `get...`; using the owning value class plus the actual sole-field getter name prevents that
    // lexical coincidence from changing whether its body participates in value-class rewriting.
    let mut vc_sole_getter_fids = HashSet::new();
    for (class_index, class) in ir.classes.iter().enumerate() {
        if !class.is_value {
            continue;
        }
        for &fid in &class.methods {
            let function = &ir.functions[fid as usize];
            let reads_underlying_field = function.body.is_some_and(|body| {
                let mut reachable = HashSet::new();
                collect_reachable_scoped(&ir.exprs, body, &mut reachable);
                reachable
                    .into_iter()
                    .any(|expression| match &ir.exprs[expression as usize] {
                        IrExpr::GetField { class, index, .. } => {
                            *class as usize == class_index && *index == 0
                        }
                        IrExpr::PropertyRead { owner, name, .. } => {
                            *owner == ir.classes[class_index].fq_name
                                && ir.classes[class_index]
                                    .fields
                                    .first()
                                    .is_some_and(|field| field.name == *name)
                        }
                        _ => false,
                    })
            });
            if getter[class_index].as_ref().is_some_and(|name| {
                function.name == *name && function.params.is_empty() && reads_underlying_field
            }) {
                vc_sole_getter_fids.insert(fid);
            }
        }
    }

    // Each source value class's getter name keyed by its internal name (`A2` → `getValue`) — to
    // recognize the legacy call-shaped form. Semantic property nodes use the source property-name map
    // below and never derive meaning from a JVM getter spelling.
    let mut vc_getters: HashMap<TypeName, String> = ir
        .classes
        .iter()
        .filter(|c| c.is_value)
        .filter_map(|c| {
            c.fields
                .first()
                .map(|f| (c.fq_name, property_getter_name(&f.name)))
        })
        .collect();
    vc_getters.extend(
        external_underlying_properties
            .iter()
            .map(|(owner, property)| (*owner, property_getter_name(property))),
    );
    let mut vc_properties: HashMap<TypeName, String> = ir
        .classes
        .iter()
        .filter(|class| class.is_value)
        .filter_map(|class| {
            class
                .fields
                .first()
                .map(|field| (class.fq_name, field.name.clone()))
        })
        .collect();
    vc_properties.extend(external_underlying_properties);

    // Interfaces that value classes implement — a function returning one of these (or `Any`) boxes a
    // value-class tail so virtual/interface dispatch works.
    let vc_interfaces: HashSet<TypeName> = ir
        .classes
        .iter()
        .filter(|c| c.is_value)
        .flat_map(|c| c.interfaces.iter_ids())
        .collect();

    // Functions that are members of a value class — their bodies operate on the BOXED object and must
    // not be rewritten (only their signatures erase).
    let mut vc_methods: HashSet<u32> = HashSet::new();
    for c in &ir.classes {
        if c.is_value {
            vc_methods.extend(c.methods.iter().copied());
        }
    }
    // Exprs reachable from a value-class member body reference the BOXED class (`other is X`, `this.field`
    // in the synthesized `equals`) and must NOT be erased — those methods run on the boxed object.
    let mut vc_body_exprs: HashSet<ExprId> = HashSet::new();
    for &mid in &vc_methods {
        if let Some(Some(root)) = ir.functions.get(mid as usize).map(|f| f.body) {
            collect_reachable(&ir.exprs, root, &mut vc_body_exprs);
        }
    }

    // Per-function value-slot types (parameters + local `Variable`s) and return types, captured BEFORE
    // erasure so the box/unbox analysis sees `Class{X}` (non-null = unboxed, nullable = boxed).
    let orig_rets: Vec<Ty> = ir.functions.iter().map(|f| f.ret).collect();
    // Shared-cell (`Ref$XxxRef`) element types, also pre-erasure: a write into a cell whose element
    // is a boxed `X?` (or reference) is a box boundary for an unboxed value.
    let orig_ref_elems: HashMap<ExprId, Ty> = ir
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(i, e)| match e {
            IrExpr::RefNew { elem, .. } | IrExpr::RefSet { elem, .. } => Some((i as ExprId, *elem)),
            _ => None,
        })
        .collect();
    // Suspend functions, for value-class mangling: kotlinc mangles the ORIGINAL signature, which for a
    // suspend fun carries a trailing `Continuation` value parameter (a non-inline `_` element). By fid
    // for the declaration sites, and by `(owner, source-name, arity)` for the recompute sites (bridges,
    // fn-references) — keyed BEFORE any name mangling so every site agrees on the same mangled name.
    let suspend_fids: std::collections::HashSet<u32> = ir.suspend_funs.iter().copied().collect();
    let suspend_sig: std::collections::HashSet<(Option<TypeName>, String, usize)> = ir
        .functions
        .iter()
        .enumerate()
        .filter(|(fid, _)| suspend_fids.contains(&(*fid as u32)))
        .map(|(fid, f)| (f.dispatch_receiver, f.name.clone(), orig_params[fid].len()))
        .collect();
    // A suspend result crosses the erased `Continuation` boundary in the representation selected by
    // the value-class ABI. Record it against both local declarations and exact call identities
    // before either pass rewrites expressions.
    let force_boxed_suspend_returns = record_suspend_results(ir, &under, &orig_rets, &suspend_fids);
    let slot_types: Vec<HashMap<u32, Ty>> = ir
        .functions
        .iter()
        .enumerate()
        .map(|(fid, f)| {
            let mut m: HashMap<u32, Ty> = HashMap::new();
            let base = u32::from(f.dispatch_receiver.is_some() && !f.is_static);
            // A lifted lambda's OWN parameters (from this index on) arrive through the `FunctionN` generic
            // `Object` invoke slot, so a reference-underlying value-class parameter is BOXED there — type it
            // as the NULLABLE (boxed) value class so `repr` reads a boxed `X` and a value-class member/
            // extension call on it (`it.getOrThrow()`) unboxes it. This slot map describes the REAL lambda
            // implementation. A retained inline-body copy does not share this `FunctionN` representation
            // boundary: like kotlinc's inlined lambda it takes the parameter unboxed, and whoever inlines
            // it unboxes the incoming box (`inline_body_slots`). Value-class-ness is decided HERE
            // (with `under`), not in the lambda-agnostic lowerer.
            let own_from = ir.lambda_own_params_from.get(&(fid as u32)).copied();
            let sam_params = own_from.and_then(|s| {
                lambda_sam_params(&ir.lambda_sam_signature, fid as u32, s, f.params.len())
            });
            for (i, p) in f.params.iter().enumerate() {
                let boxed_own = own_from.is_some_and(|s| i as u32 >= s)
                    && !p.is_nullable()
                    && p.non_null().obj_internal().is_some_and(|fq| {
                        callable_under.contains_key(&fq)
                            && lambda_slot_is_boxed(
                                sam_params.and_then(|declared| {
                                    declared.get(i - own_from.unwrap_or(0) as usize)
                                }),
                                fq,
                            )
                    });
                let slot_ty = if boxed_own { Ty::nullable(*p) } else { *p };
                m.insert(base + i as u32, slot_ty);
            }
            if let Some(root) = f.body {
                let mut reach = HashSet::new();
                collect_reachable_scoped(&ir.exprs, root, &mut reach);
                for id in reach {
                    if let IrExpr::Variable { index, ty, .. } = &ir.exprs[id as usize] {
                        m.insert(*index, *ty);
                    }
                }
            }
            m
        })
        .collect();

    // A member method OVERRIDING a generic supertype method receives its VALUE-CLASS param BOXED: the
    // supertype's erased signature passes `Object`, so the incoming arg is a boxed `X`, not the underlying.
    // The IR's bridge record carries the evidence — a concrete VC param (`Result`) whose supertype-erased
    // counterpart is a generic reference (`Any`), with NO mangled target unboxing it (a degenerate
    // `target_name = None` bridge; a mangled `foo-<hash>` target would unbox in the bridge instead). Mark
    // such a param slot as the BOXED value class so the body unboxes it at each value-class member call —
    // matching kotlinc, which unboxes the incoming box before use. (Only the repr analysis sees this; the
    // emitted method signature is unchanged.)
    // A GENERIC value class (`IC<T>`, its field typed by a type parameter) is left unmarked: its box
    // and unbox differ from a concrete-underlying one's, which krusty can't mark without a conflict.
    let generic_vcs: std::collections::HashSet<TypeName> = ir
        .classes
        .iter()
        .filter(|c| c.is_value && !c.type_params.is_empty())
        .map(|c| c.fq_name)
        .collect();
    let inline_own_parameters = inline_body_slots::own_parameters(ir);
    let mut slot_types = slot_types;
    let mut boxed_generic_overrides = HashSet::new();
    for c in &ir.classes {
        for b in &c.bridges {
            // A VALUE-CLASS-returning override is MANGLED with fully UNBOXED params — kotlinc keeps it
            // unboxed. Only a NON-value-class-returning override keeps the erased supertype name and receives
            // its value-class param BOXED. So skip a value-class return (and a mangled-target bridge).
            if b.target_name.is_some()
                || b.concrete_ret
                    .non_null()
                    .obj_internal()
                    .is_some_and(|fq| under.contains_key(&fq))
            {
                continue;
            }
            // The bridge's exact target. A bridge to an implementation this class does not declare
            // (an inherited or external one) has no body here whose slots receive the box.
            let Some(fid) = b.target_function.filter(|fid| c.methods.contains(fid)) else {
                continue;
            };
            let f = &ir.functions[fid as usize];
            // A method MANGLED by a value-class PARAMETER (not only a value-class return) is likewise
            // unboxed in its bridge — `call(Result, IC)` mangles to `call-<hash>` because of the user value
            // class `IC` (kotlinc EXEMPTS a `kotlin.Result` param from mangling), and its bridge unboxes
            // BOTH params. The `target_name`/return checks above miss this shape (non-value-class return,
            // `target_name = None`), so its params would be wrongly marked boxed and double-unboxed at use.
            // Skip when the method is mangled — same predicate the mangle pass below applies.
            let is_file_class = f.dispatch_receiver.is_none();
            if vc_mangle(
                &f.name,
                &orig_params[fid as usize],
                &orig_rets[fid as usize],
                &under,
                is_file_class,
                suspend_fids.contains(&fid),
            ) != f.name
            {
                continue;
            }
            let base = u32::from(f.dispatch_receiver.is_some() && !f.is_static);
            for (i, (cp, ep)) in b
                .concrete_params
                .iter()
                .zip(b.erased_params.iter())
                .enumerate()
            {
                if let Some(x) = cp.non_null().obj_internal() {
                    // The supertype must pass a GENERIC `Any`/`Object` at this position — i.e. the param was a
                    // type PARAMETER there (`I<Result>.foo(T)`), so the arg is boxed. A value class that is
                    // CONCRETE in the supertype (`Core.getFor(id: Aid)`) erases to its OWN underlying
                    // (`String`), the method is mangled, and its param arrives UNBOXED — do NOT mark it.
                    let supertype_generic = ep.is_erased_top();
                    if under.contains_key(&x) && supertype_generic && !generic_vcs.contains(&x) {
                        // Mark BOXED in the body's slot repr AND the call-boundary target, so a
                        // CALLER boxes into this generic slot and the BODY unboxes it.
                        let boxed = Ty::nullable(Ty::obj_name(x));
                        slot_types[fid as usize].insert(base + i as u32, boxed);
                        boxed_generic_overrides.insert(fid);
                        if let Some(p) =
                            orig_params.get_mut(fid as usize).and_then(|v| v.get_mut(i))
                        {
                            *p = boxed;
                        }
                    }
                }
            }
        }
    }

    call_arguments::record_method_parameters(ir, &boxed_generic_overrides, &orig_params);
    // 1. Erase signatures + drop null-checks on params that erased to a non-reference. `box-impl`
    //    returns the boxed `X` (the one position not erased).
    let is_vc_ty = |t: &Ty| {
        t.non_null()
            .obj_internal()
            .is_some_and(|fq| under.contains_key(&fq))
    };
    let is_callable_vc_ty = |t: &Ty| {
        t.non_null()
            .obj_internal()
            .is_some_and(|fq| callable_under.contains_key(&fq))
    };
    // `(owner-internal, plain name, arity)` → mangled name, for rewriting resolved-by-name calls
    // (`super.f(vc)`, an interface method) to the value-class-mangled method.
    let mut mangle_map: HashMap<(TypeName, String, usize), String> = HashMap::new();
    // The declaration behind each `mangle_map` key, when exactly one ordinary (non-suspend,
    // non-value-class-member) function has it. Its erased signature is the call's descriptor: a
    // descriptor string cannot say whether `LX;` stood for `X` or for `X?` (a `T : X?` result), and
    // the two erase differently when `X?` stays boxed.
    let mut mangled_declarations: HashMap<(TypeName, String, usize), Option<u32>> = HashMap::new();
    // Exact getters whose override pair diverges in semantic type (for example `Vid` over `Vid?`).
    // The two declarations hash differently under JVM value-class mangling, so the accessor bridge
    // owns their compatibility and the implementation getter keeps its ordinary spelling.
    let mut divergent_getters = HashSet::new();
    for (&owner, edges) in &ir.property_overrides {
        let Some(class) = ir.classes.iter().find(|class| class.fq_name == owner) else {
            continue;
        };
        for edge in edges {
            if edge.implementation_owner != owner || edge.declared_type == edge.implementation_type
            {
                continue;
            }
            if let Some(getter) = class
                .properties
                .iter()
                .find(|property| property.name == edge.name)
                .and_then(|property| property.getter)
            {
                divergent_getters.insert(getter);
            }
        }
    }
    // `(fid, param idx, boxed value-class Ty)` for DEFAULTED nullable-underlying value-class params —
    // the base method unboxes them (below), but its `$default` stub + call site keep them boxed.
    let mut default_boxed: Vec<(u32, usize, Ty)> = Vec::new();
    // `(fid, declared name, declared params, declared ret)` — collected while `ir.functions` is borrowed
    // mutably, moved into `ir.vc_declared_sigs` once the loop releases it.
    let mut declared_sigs: Vec<(u32, String, Vec<Ty>, Ty)> = Vec::new();
    // `(fid, param slot, value class, erased underlying)` for a REFERENCE-underlying lambda own-param
    // kept boxed: the body was lowered against the erased convention (the slot IS the underlying), so
    // every read in the implementation gains an `unbox-impl` after the loop (kotlinc reaches the same
    // state via its lambda-class `invoke` bridge; here the unbox is fused into the impl at each use).
    let mut boxed_own_reads: Vec<(u32, u32, TypeName, Ty)> = Vec::new();
    // User-written value-class members are realized as static `*-impl` functions over the carrier.
    // Keep their declaration identities so return adaptation follows that realized ABI instead of the
    // boxed-instance convention used by synthesized wrapper members.
    let mut lowered_value_members = HashSet::new();
    // Functions whose physical name the mangling below changed, by stable identity.
    let mut renamed_functions = HashSet::new();
    for (fid, f) in ir.functions.iter_mut().enumerate() {
        let is_box_impl = f.name == "box-impl";
        // A USER value-class member function's body runs on the BOXED object; its value-class-typed
        // parameters/return stay boxed (a sibling member call passes `this` — a box — directly). The
        // SYNTHESIZED members (`-impl`, `equals`/`hashCode`/`toString`, `<init>`) operate on the
        // underlying representation, so they erase like any other function. An exact
        // type-divergent override getter also keeps its ordinary spelling; its bridge owns the
        // representation difference.
        let is_divergent_override_getter = divergent_getters.contains(&(fid as u32));
        let synthesized = matches!(
            f.name.as_str(),
            "box-impl"
                | "unbox-impl"
                | "constructor-impl"
                | "equals-impl0"
                | "equals-impl"
                | "hashCode-impl"
                | "toString-impl"
                | "equals"
                | "hashCode"
                | "toString"
                | "<init>"
        ) || is_divergent_override_getter
            || realized_members.instance_entries.contains(&(fid as u32));
        let vc_member = !synthesized && vc_methods.contains(&(fid as u32));
        let source_name = f.name.clone();
        // Mangle a USER function whose (pre-erasure) signature mentions a value class — kotlinc's
        // `base-<hash>`. Index-resolved `MethodCall`s pick this up automatically; name-resolved calls
        // (super/interface) are rewritten below via `mangle_map`.
        if !synthesized {
            // A top-level (facade/file-class) function has no dispatch receiver — its value-class RETURN
            // is not mangled; a member's is.
            let is_file_class = f.dispatch_receiver.is_none();
            // Keep the declared signature for `@Metadata`, which names the Kotlin function and its
            // declared types — the mangling and erasure below are a JVM realization it records
            // separately. Only worth keeping when a value class is actually involved.
            if vc_member
                || orig_params[fid]
                    .iter()
                    .chain(std::iter::once(&orig_rets[fid]))
                    .any(is_callable_vc_ty)
            {
                declared_sigs.push((
                    fid as u32,
                    source_name.clone(),
                    orig_params[fid].clone(),
                    orig_rets[fid],
                ));
            }
            // Every ordinary value-class member is physically a static implementation over the
            // carrier. The source name and source value parameters stay in `vc_declared_sigs` for
            // metadata.
            let lower_value_member = vc_member && !f.is_static;
            let is_suspend = suspend_fids.contains(&(fid as u32));
            let mangled = if realized_members.accessors.contains(&(fid as u32)) {
                // Already named from its declared accessor signature, before it gained the carrier.
                source_name.clone()
            } else if lower_value_member {
                vc_member_impl_name(
                    &source_name,
                    &orig_params[fid],
                    &orig_rets[fid],
                    &callable_under,
                    is_suspend,
                )
            } else {
                vc_mangle(
                    &source_name,
                    &orig_params[fid],
                    &orig_rets[fid],
                    &callable_under,
                    is_file_class,
                    is_suspend,
                )
            };
            if mangled != source_name {
                if let Some(owner) = f.dispatch_receiver {
                    let key = (owner, source_name.clone(), orig_params[fid].len());
                    let ordinary = !lower_value_member && !is_suspend;
                    mangled_declarations
                        .entry(key.clone())
                        .and_modify(|declaration| *declaration = None)
                        .or_insert(ordinary.then_some(fid as u32));
                    mangle_map.insert(key, mangled.clone());
                }
                f.name = mangled;
                renamed_functions.insert(fid as u32);
            }
            if lower_value_member {
                let owner = f
                    .dispatch_receiver
                    .expect("a value-class member has a dispatch receiver");
                let carrier = under.get(&owner).copied().unwrap_or(Ty::Error);
                f.params.insert(0, carrier);
                // The carrier is the receiver the box already checked, never a guarded parameter.
                if !f.param_checks.is_empty() {
                    f.param_checks.insert(0, None);
                }
                f.is_static = true;
                lowered_value_members.insert(fid as u32);
                // The former `this` and the new explicit carrier are both slot zero. Source value
                // parameters consequently retain their existing slots (1..). Keep the slot's semantic
                // value-class identity: its function parameter independently carries the physical `U`,
                // while representation boundaries still need to box `this` as `X`, never as U's wrapper.
                slot_types[fid].insert(0, Ty::obj_name(owner));
            }
        }
        let own_from = ir.lambda_own_params_from.get(&(fid as u32)).copied();
        let sam_params = own_from.and_then(|s| {
            lambda_sam_params(&ir.lambda_sam_signature, fid as u32, s, f.params.len())
        });
        let receivers = usize::from(f.is_static && f.dispatch_receiver.is_some())
            + usize::from(ir.extension_receiver_fns.contains(&(fid as u32)));
        let defaults = ir
            .fn_params
            .get(&(fid as u32))
            .and_then(|p| p.defaults.as_ref());
        for (idx, p) in f.params.iter_mut().enumerate() {
            // A lifted lambda's OWN value-class parameter arrives BOXED through the `FunctionN`
            // generic invoke slot, so it must KEEP the boxed `LX;` in the impl signature — erased
            // to the underlying, the indy adapter would cast the incoming box straight to the
            // underlying (`checkcast Integer` on a `W`, `checkcast String` on an `X`) and CCE.
            // (kotlinc instead falls back to a lambda CLASS whose `invoke` bridge unbox-impls
            // before a mangled erased `invoke-<hash>` — the boxed-impl indy here is sound but
            // byte-divergent; the class shape is a separate parity work item.)
            // A SAM-converted lambda answers to the interface's DECLARED slot instead: one spelled as
            // the value class itself erases to the underlying, so that parameter must NOT stay boxed.
            if own_from.is_some_and(|s| idx as u32 >= s)
                && !p.is_nullable()
                && p.non_null().obj_internal().is_some_and(|fq| {
                    callable_under.contains_key(&fq)
                        && lambda_slot_is_boxed(
                            sam_params.and_then(|d| d.get(idx - own_from.unwrap_or(0) as usize)),
                            fq,
                        )
                })
            {
                // A scalar underlying is covered by the boxed-slot repr (`X?` over a scalar IS the
                // box, so `repr_of_ty` reads `Boxed` and each use unboxes). `X?` over a reference
                // is NOT boxed (it erases to the underlying reference), so the repr machinery would
                // read the slot as already-unboxed — record it for the explicit use-site unbox
                // rewrite below instead.
                if let Some(x) = p.non_null().obj_internal() {
                    let u = erase(p, &under);
                    if is_ref(&u) && !nullable_is_boxed(x, &under) {
                        boxed_own_reads.push((fid as u32, idx as u32, x, u));
                    }
                }
                *p = Ty::nullable(*p);
                continue;
            }
            if !vc_member || f.is_static || !is_vc_ty(p) {
                let defaulted = idx
                    .checked_sub(receivers)
                    .and_then(|i| defaults?.get(i)?.as_ref());
                if defaulted.is_some() && vc_underlying_nullable(p, &under) {
                    default_boxed.push((fid as u32, idx, *p));
                }
                *p = erase(p, &under);
            }
        }
        if !(is_box_impl || vc_member && !f.is_static && is_vc_ty(&f.ret)) {
            f.ret = if suspend_fids.contains(&(fid as u32)) {
                suspend_result_representation(
                    &orig_rets[fid],
                    &under,
                    force_boxed_suspend_returns.contains(&(fid as u32)),
                )
                .map(crate::ir::IrValueClassSuspendResult::boundary_ty)
                .unwrap_or_else(|| erase(&f.ret, &under))
            } else {
                erase(&f.ret, &under)
            };
        }
        if !f.param_checks.is_empty() {
            for (k, chk) in f.param_checks.iter_mut().enumerate() {
                // Drop the null-check when the param erased to a non-reference, OR when it was a
                // value class whose unboxed underlying is itself null-capable (e.g. `X(val v: Int?)`
                // erases to `Integer`, which the value `X(null)` leaves null) — kotlinc emits no
                // `checkNotNullParameter` there. Ask the declaration-descriptor realization rather
                // than [`is_ref`]: an ordinary type parameter is a generic reference boundary, but
                // a value-class carrier can temporarily retain `TyParam<T : Int>` here and is emitted
                // as the primitive bound (`I`). The guard must agree with that final physical slot.
                // A lowered member's checks start after its carrier; `orig_params` has none. A
                // parameter holding its box (a generic override's) is marked nullable there.
                let source_index =
                    k.checked_sub(usize::from(lowered_value_members.contains(&(fid as u32))));
                let under_nullable = source_index
                    .and_then(|index| orig_params[fid].get(index))
                    .is_some_and(|t| vc_underlying_nullable(&t.non_null(), &under));
                let physical_is_ref = f
                    .params
                    .get(k)
                    .and_then(|parameter| {
                        jvm_tys(std::slice::from_ref(parameter)).into_iter().next()
                    })
                    .is_some_and(|parameter| is_ref(&parameter));
                if chk.is_some() && (!physical_is_ref || under_nullable) {
                    *chk = None;
                }
            }
        }
    }
    for function in lowered_value_members.iter().copied() {
        super::method_parameters::prepend_value_class_receiver(ir, function, "arg0");
    }
    // `(class, method-index)` → the value class a member's RETURN keeps BOXED. A user value-class member
    // runs on / returns the boxed object (the erasure loop above left its VC return un-erased), so its
    // `MethodCall` result is a boxed `X` — a following unboxed-slot use (`val b: X = x.inc()`) must unbox.
    // (Getters are intercepted earlier in `repr` via `field_getters`; the static `-impl`s are `Call`s.)
    let boxed_ret_methods: HashMap<(u32, u32), TypeName> = {
        let mut m = HashMap::new();
        for (ci, c) in ir.classes.iter().enumerate() {
            for (mi, &fid) in c.methods.iter().enumerate() {
                if let Some(fq) = ir.functions[fid as usize].ret.non_null().obj_internal() {
                    // A primitive-array value class keeps its semantic name through erasure, but the
                    // JVM result is the carrier. Marking that return boxed checkcasts the carrier
                    // to the box.
                    if under.contains_key(&fq)
                        && representation::carrier_slot(Ty::obj_name(fq), &under).is_none()
                    {
                        m.insert((ci as u32, mi as u32), fq);
                    }
                }
            }
        }
        m
    };
    // Publish the exact physical result of index-resolved calls whose selected method returns a
    // value-class box. `Call::source_function` can read the rewritten function return directly, while
    // `MethodCall` carries only `(class, method-index)`; without this stamp, the two representations
    // diverge and a direct suspend member call can be treated as its carrier before its required
    // `unbox-impl` boundary is inserted.
    let boxed_calls: Vec<(ExprId, TypeName)> = ir
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(id, expression)| {
            let id = id as ExprId;
            let selected = match expression {
                IrExpr::MethodCall { class, index, .. } => {
                    boxed_ret_methods.get(&(*class, *index)).copied()
                }
                IrExpr::Call { callee, .. } => callee
                    .source_function()
                    .and_then(|function| ir.functions.get(function as usize))
                    .and_then(|function| function.ret.non_null().obj_internal())
                    .filter(|classifier| under.contains_key(classifier))
                    .filter(|classifier| {
                        representation::carrier_slot(Ty::obj_name(*classifier), &under).is_none()
                    }),
                _ => None,
            };
            let suspended = match ir.value_class_suspend_calls.get(&id) {
                Some(crate::ir::IrValueClassSuspendResult::Boxed { classifier, .. }) => {
                    Some(*classifier)
                }
                _ => None,
            };
            selected.or(suspended).map(|classifier| (id, classifier))
        })
        .collect();
    for (call, classifier) in boxed_calls {
        ir.physical_types.insert(call, Ty::obj_name(classifier));
    }
    // An intrinsic point's value comes through a generic slot: `suspendCoroutine<T>` reads
    // `SafeContinuation.getOrThrow(): Object`, and `suspendCoroutineUninterceptedOrReturn<T>`'s block
    // returns `Any?`. A value-class `T` therefore crosses as its box (or null for `T?`), never as the
    // carrier, on either path. Preserve that physical fact on the exact FIR-selected intrinsic point
    // so a value-class suspend-function tail does not descend into the inlined user block and attempt
    // to box that block's own result.
    let intrinsic_boxes = ir
        .intrinsic_suspension_points
        .iter()
        .filter_map(|(&expression, point)| {
            point
                .result
                .non_null()
                .obj_internal()
                .filter(|classifier| under.contains_key(classifier))
                .map(|classifier| (expression, classifier))
        })
        .collect::<Vec<_>>();
    for (expression, classifier) in intrinsic_boxes {
        ir.physical_types
            .insert(expression, Ty::obj_name(classifier));
    }
    for (fid, name, params, ret) in declared_sigs {
        ir.vc_declared_sigs.insert(fid, (name, params, ret));
    }
    for (fid, idx, ty) in default_boxed {
        ir.default_stub_boxed_params
            .entry(fid)
            .or_default()
            .push((idx, ty));
    }
    // A reference-underlying lambda own-param arrives BOXED (`LX;`) at the REAL implementation's
    // `FunctionN.invoke(Object)` boundary, while its body was lowered against the erased convention
    // (the slot as the underlying). Rewrite every implementation-body read to `unbox-impl` so each use
    // sees the underlying again. A retained inline body takes the parameter unboxed and is left as it
    // is (`inline_body_slots`). In-place: the `GetValue` node itself becomes the unbox call over a fresh
    // `GetValue`, so every reference to the node (including a nested lambda's capture list) picks up
    // the unboxed value.
    for (fid, slot, x, u) in boxed_own_reads {
        let mut reads = HashSet::new();
        if let Some(root) = ir.functions[fid as usize].body {
            collect_reachable_scoped(&ir.exprs, root, &mut reads);
        }
        let targets: Vec<ExprId> = reads
            .into_iter()
            .filter(|&id| matches!(&ir.exprs[id as usize], IrExpr::GetValue(i) if *i == slot))
            .collect();
        for id in targets {
            let get = ir.add_expr(IrExpr::GetValue(slot));
            ir.exprs[id as usize] = IrExpr::Call {
                callee: Callee::realized_virtual(
                    x,
                    "unbox-impl".to_string(),
                    format!("(){}", desc(&u)),
                    None,
                    false,
                ),
                dispatch_receiver: Some(get),
                args: vec![],
            };
        }
    }

    // 1a′. A `@Serializable` property's `get<X>$annotations()` marker follows its getter's value-class
    //      mangle: when `getX` mangled to `getX-<hash>`, kotlinc names the marker `getX-<hash>$annotations`.
    //      The marker is a static (no dispatch receiver), so its owner comes from the class method list.
    if !mangle_map.is_empty() {
        let mut renames: Vec<(u32, String)> = Vec::new();
        for c in &ir.classes {
            for &fid in &c.methods {
                let Some(f) = ir.functions.get(fid as usize) else {
                    continue;
                };
                let Some(base) = f.name.strip_suffix("$annotations") else {
                    continue;
                };
                if let Some(mangled) = mangle_map.get(&(c.fq_name, base.to_string(), 0)) {
                    renames.push((fid, format!("{mangled}$annotations")));
                }
            }
        }
        for (fid, name) in renames {
            ir.functions[fid as usize].name = name;
        }
    }

    // 1b. Rewrite name-resolved calls to a mangled method (`super.f(vc)`, an interface method) — its
    //     name gets the `-<hash>` suffix and its descriptor's value-class types erase to the underlying.
    if !mangle_map.is_empty() {
        let declaration_descriptors: HashMap<(TypeName, String, usize), String> =
            mangled_declarations
                .into_iter()
                .filter_map(|(key, declaration)| {
                    let function = &ir.functions[declaration? as usize];
                    Some((key, ir_method_desc(&function.params, &function.ret)))
                })
                .collect();
        for e in &mut ir.exprs {
            if let IrExpr::Call {
                callee:
                    Callee::Special {
                        owner,
                        name,
                        descriptor,
                        ..
                    }
                    | Callee::Virtual {
                        owner,
                        name,
                        descriptor,
                        ..
                    }
                    | Callee::Static {
                        owner,
                        name,
                        descriptor,
                        ..
                    },
                args,
                ..
            } = e
            {
                let key = (*owner, name.clone(), args.len());
                if let Some(mangled) = mangle_map.get(&key) {
                    *name = mangled.clone();
                    *descriptor = declaration_descriptors
                        .get(&key)
                        .cloned()
                        .unwrap_or_else(|| erase_descriptor(descriptor, &under));
                }
            }
        }
    }
    // The checker already selected one SAM declaration. Its declared signature and suspend fact
    // travel on the lambda, so the physical slot is realized from those, not from a
    // `(classifier, name, arity)` map. Two methods of one fun interface can share a name and an
    // arity while their value-class hashes differ; the map keeps only one of them.
    if !callable_under.is_empty() {
        for expression in &mut ir.exprs {
            let IrExpr::Lambda {
                sam: Some(target), ..
            } = expression
            else {
                continue;
            };
            let method = vc_mangle_once(
                &target.method,
                &target.declared_parameters,
                &target.declared_result,
                &callable_under,
                false,
                target.suspend,
            );
            target.method = method;
        }
    }
    // Common IR keeps the SAM declaration semantic. Once this backend has chosen value-class
    // carriers, publish the exact physical interface slot beside it. LambdaMetafactory (and the
    // class-based SAM strategy) must implement that erased slot, not a descriptor reconstructed
    // from the value-class classifier spelling.
    ir.lambda_sam_jvm_signature = ir
        .lambda_sam_signature
        .iter()
        .map(|(&implementation, (parameters, result))| {
            (
                implementation,
                (
                    parameters
                        .iter()
                        .map(|parameter| erase(parameter, &under))
                        .collect(),
                    erase(result, &under),
                ),
            )
        })
        .collect();
    // A member of a sibling file's value class is realized by that file's pass as a static
    // implementation over the carrier: derive the same name from the declared signature the call
    // retains (and its checked suspend fact); step 4 moves the receiver to parameter zero. The
    // declared parameters become the selected declaration's, since an erased `Object` slot is
    // otherwise ambiguous between the carrier itself and a generic box.
    let mut sibling_member_impls: HashMap<ExprId, String> = HashMap::new();
    // Rewrite cross-file calls with value-class signatures to their JVM names and types.
    if !callable_under.is_empty() {
        for (id, e) in ir.exprs.iter_mut().enumerate() {
            if let IrExpr::Call {
                callee:
                    Callee::Virtual {
                        owner,
                        name,
                        params: Some((params, ret)),
                        ..
                    },
                dispatch_receiver,
                ..
            } = e
            {
                let id = id as ExprId;
                // The underlying property's getter is no static implementation: it is the carrier.
                if dispatch_receiver.is_some()
                    && module_value_classes.contains_key(owner)
                    && !cls_by_name.contains_key(owner)
                    && vc_getters.get(owner) != Some(name)
                {
                    let is_suspend = ir.suspend_calls.contains_key(&id);
                    let impl_name =
                        vc_member_impl_name(name, params, ret, &callable_under, is_suspend);
                    sibling_member_impls.insert(id, impl_name);
                    ir.call_declared_params
                        .entry(id)
                        .or_insert_with(|| params.clone().into_boxed_slice());
                }
                let mangled = vc_mangle_once(name, params, ret, &callable_under, false, false);
                if &mangled != name {
                    *name = mangled;
                }
                for p in params.iter_mut() {
                    *p = erase(p, &under);
                }
                *ret = erase(ret, &under);
            }
        }
        for e in &mut ir.exprs {
            let IrExpr::Call { callee, .. } = e else {
                continue;
            };
            let (name, params, ret, module_target, module_default_call, semantic_default) =
                match callee {
                    Callee::CrossFile {
                        name,
                        params,
                        ret,
                        module_target,
                        module_default_call,
                        ..
                    } => (
                        name,
                        params,
                        ret,
                        *module_target,
                        *module_default_call,
                        false,
                    ),
                    Callee::ModuleWithDefaults {
                        target,
                        default_provider,
                        name,
                        params,
                        ret,
                        dispatch_receiver_ty,
                        ..
                    } => {
                        if let Some(receiver) = dispatch_receiver_ty {
                            *receiver = erase(receiver, &under);
                        }
                        let provider = default_calls::module_provider(*target, *default_provider);
                        (name, params, ret, Some(provider), true, true)
                    }
                    _ => continue,
                };
            if let Some(callable) =
                module_target.and_then(|target| ir.referenced_module_callables.get(&target))
            {
                // `$default` is a JVM companion of the KOTLIN declaration, not a declaration whose
                // mask/marker parameters participate in value-class mangling. Mangle the finalized
                // semantic signature retained with the stable module target, then append the
                // synthetic suffix. This also preserves member-return and suspend mangling rules;
                // neither can be reconstructed from the realized static descriptor.
                let base = if module_default_call {
                    name.as_str()
                        .strip_suffix("$default")
                        .unwrap_or(name.as_str())
                } else {
                    name.as_str()
                };
                let is_suspend = callable.flags.has(crate::fir::DeclarationFlags::SUSPEND);
                // A member of a module value class is realized as a static implementation over
                // its carrier, so its `$default` companion extends that implementation's name.
                let mangled = if callable
                    .owner
                    .is_some_and(|owner| module_value_classes.contains_key(&owner))
                {
                    vc_member_impl_name(
                        base,
                        &callable.parameters,
                        &callable.result,
                        &callable_under,
                        is_suspend,
                    )
                } else {
                    vc_mangle_once(
                        base,
                        &callable.parameters,
                        &callable.result,
                        &callable_under,
                        callable.owner.is_none(),
                        is_suspend,
                    )
                };
                *name = if module_default_call && !semantic_default {
                    format!("{mangled}$default")
                } else {
                    mangled
                };
            } else {
                *name = vc_mangle(name, params, ret, &callable_under, true, false);
            }
            for parameter in params.iter_mut() {
                *parameter = erase(parameter, &under);
            }
            *ret = erase(ret, &under);
        }
    }
    function_references::realize(ir, &callable_under, &renamed_functions);
    let interface_entries = interface_entries::materialize(
        ir,
        &lowered_value_members,
        override_results,
        |ir: &IrFile, member: u32| {
            let (name, params, ret) = ir
                .vc_declared_sigs
                .get(&member)
                .expect("a lowered value-class member records its declaration");
            vc_member_entry_name(
                name,
                params,
                ret,
                &callable_under,
                suspend_fids.contains(&member),
            )
        },
    );

    // Exact user value-class members have now been rewritten to static carrier functions. Snapshot
    // those physical signatures before borrowing the class bridge lists; a bridge keeps the stable
    // function identity, so no emitted-name/arity lookup is needed to find its target ABI.
    let lowered_member_targets = lowered_value_members
        .iter()
        .map(|&function| {
            let target = &ir.functions[function as usize];
            let result = override_results.physical_result(ir, function);
            (
                function,
                (target.name.clone(), target.params.clone(), result),
            )
        })
        .collect::<HashMap<_, _>>();
    // A covariant-override bridge delegates to the concrete method by name (mangle the target if it was
    // mangled). When the override returns a value class, the concrete method returns the erased underlying,
    // so the bridge boxes the result back to `X`. Runs even with an empty `mangle_map` — a
    // value-class GETTER bridge (`Child2.prop: Child` through `Base2.prop: Base`) needs the erase+box with
    // no mangling involved.
    bridge_realization::realize(
        &mut ir.classes,
        &bridge_realization::Inputs {
            under: &under,
            callable_under: &callable_under,
            mangle_map: &mangle_map,
            suspend_sig: &suspend_sig,
            lowered_member_targets: &lowered_member_targets,
            interface_entries: &interface_entries,
        },
        bridge_adaptations,
    );

    // 2. Erase class field + ctor-arg types; drop the `<init>` null-check on a constructor parameter
    //    that erased to a non-reference (a value-class ctor arg `a: Na` → `int` can't be null-checked).
    // A NON-value class whose primary ctor has a value-class-typed param gets kotlinc's private-primary +
    // synthetic marker accessor ABI — recorded BEFORE erasure loses the value-class identity of the param.
    // Which slots count is `hidden_constructors::selecting_slots`.
    let serialization_deserialization_ctors = (0..ir.classes.len())
        .map(|class| {
            ir.generated_secondary_constructor(
                class as crate::ir::ClassId,
                crate::ir::IrSecondaryConstructorRole::SerializationDeserialization,
            )
        })
        .collect::<Vec<_>>();
    let mut value_param_ctors: Vec<(TypeName, Vec<Ty>)> = Vec::new();
    for (class, c) in ir.classes.iter_mut().enumerate() {
        if !c.is_value
            && !c.is_object
            && !c.is_interface
            && hidden_constructors::primary_has_value_class(c, &is_vc_ty)
        {
            // Capture the DECLARED ctor param types before the erase below rewrites them — the
            // class metadata constructor record must name the value classes.
            value_param_ctors.push((c.fq_name, c.ctor_args.iter().map(|a| a.ty).collect()));
        }
        for fld in &mut c.fields {
            fld.ty = erase(&fld.ty, &under);
        }
        let own_value_class = c.is_value;
        for a in &mut c.ctor_args {
            // Drop the `<init>` null-check on a param that erased to a non-reference, OR whose value-class
            // underlying chain is null-capable (`ZN2(val z: ZN)` where `ZN(val z: Z1?)` → the value can be
            // null, so kotlinc emits no check). A value class's own private `<init>` is reached only
            // from `box-impl` over an already-checked carrier and has none either. Then erase the param
            // type itself.
            if own_value_class
                || !is_ref(&erase(&a.ty, &under))
                || vc_underlying_nullable(&a.ty, &under)
            {
                a.check = None;
            }
            a.ty = erase(&a.ty, &under);
        }
        // Common IR keeps the exact semantic constructor selected for each enum entry. The entry
        // arguments have now been rewritten to their JVM carriers, so realize the parallel
        // descriptor types here as part of the same backend-owned value-class erasure. Bodied
        // entries carry the identical selected signature on their synthesized subclass.
        for entry in &mut c.enum_entries {
            for parameter in &mut entry.constructor_parameter_types {
                *parameter = erase(parameter, &under);
            }
        }
        if let Some(parameters) = &mut c.enum_entry_of {
            for parameter in parameters {
                *parameter = erase(parameter, &under);
            }
        }
        // A regular class's secondary-`<init>` value-class params erase too (`Test(x: String, s: S)` →
        // `(String, String)`); a value class's own secondary ctors were already consumed into static
        // `constructor-impl`s by `synth_value_members`, so this only touches regular classes.
        for (constructor, sc) in c.secondary_ctors.iter_mut().enumerate() {
            // Record the value-class fact BEFORE erasure: it drives kotlinc's private+marker ABI for
            // this constructor (a synthetic marker-disambiguated ctor keeps its own convention).
            if !sc.synthetic && sc.params.iter().any(is_vc_ty) {
                sc.vc_params = true;
            }
            for parameter in &mut sc.prefix_params {
                *parameter = erase(parameter, &under);
            }
            let serialization_deserialization =
                serialization_deserialization_ctors[class] == Some(constructor as u32);
            for p in &mut sc.params {
                // A SYNTHETIC marker ctor (the serialization deser ctor, disambiguated by a trailing
                // `SerializationConstructorMarker`) keeps every value-class parameter BOXED. Its
                // exact producer-recorded role distinguishes it from unrelated synthetic ctors.
                if serialization_deserialization && is_vc_ty(p) {
                    continue;
                }
                *p = erase(p, &under);
            }
            let target_params = match &mut sc.delegate {
                crate::ir::CtorDelegateTarget::This { target_params, .. }
                | crate::ir::CtorDelegateTarget::Super { target_params, .. } => target_params,
                crate::ir::CtorDelegateTarget::ImplicitEnumBase => continue,
            };
            for parameter in target_params {
                *parameter = erase(parameter, &under);
            }
        }
    }
    for (internal, declared) in value_param_ctors {
        ir.mark_value_param_ctor_name(internal);
        ir.record_vc_ctor_declared_params(internal, declared);
    }

    for c in &mut ir.classes {
        let mut method_keys: HashSet<(String, String)> = c
            .methods
            .iter()
            .map(|&fid| {
                let f = &ir.functions[fid as usize];
                (f.name.clone(), ir_method_desc(&f.params, &f.ret))
            })
            .collect();
        // A dropped bridge takes its plan with it, and a later one's plan follows its ordinal.
        let kept = c
            .bridges
            .iter()
            .map(|b| {
                let desc = ir_method_desc(&b.erased_params, &b.erased_ret);
                method_keys.insert((b.name.clone(), desc))
            })
            .collect::<Vec<_>>();
        bridge_adaptations.retain(c.fq_name_id(), &kept);
        let mut kept = kept.into_iter();
        c.bridges
            .retain(|_| kept.next().expect("one answer per bridge"));
    }

    // 2b. A same-file method whose DECLARED return is a value class `X` but whose REALIZED return is
    //     `X`'s erased underlying (an interface's `onResult-<hash>()Ljava/lang/Object;`) hands back the
    //     CARRIER. The lowerer wrapped every such call in a `Cast` to the declared type — it types calls
    //     before any erasure is known — and over a carrier that cast is a `checkcast X` no unboxed value
    //     can pass. Strip it here, before the representation analysis, which would otherwise read the
    //     cast as proof that the result is a box and `unbox-impl` it at the return tail.
    let self_casts_over_carriers: Vec<(ExprId, ExprId)> = ir
        .exprs
        .iter()
        .enumerate()
        .filter_map(|(id, e)| {
            let IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::Cast | crate::ir::IrTypeOp::CastNonNull,
                arg,
                type_operand,
            } = e
            else {
                return None;
            };
            let x = type_operand
                .non_null()
                .obj_internal()
                .filter(|fq| under.contains_key(fq))?;
            let unboxed_carrier = match &ir.exprs[*arg as usize] {
                IrExpr::MethodCall { class, index, .. } => {
                    let &fid = ir
                        .classes
                        .get(*class as usize)
                        .and_then(|c| c.methods.get(*index as usize))?;
                    let declared_x = orig_rets[fid as usize].non_null().obj_internal() == Some(x);
                    let realized_x =
                        ir.functions[fid as usize].ret.non_null().obj_internal() == Some(x);
                    declared_x && !realized_x
                }
                // A mutable captured local keeps its value in `Ref$XxxRef.element`. When the logical
                // element is an UNBOXED value class, the cell physically stores its carrier; the
                // lowerer's generic-erasure self-cast must disappear before property access decides
                // whether to call `unbox-impl`. A nullable value class whose representation is BOXED
                // deliberately does not match this arm.
                IrExpr::RefGet { .. } => ir.logical_types.get(arg).is_some_and(
                    |logical| matches!(repr_of_ty(logical, &under), Repr::Unboxed(value_class) if value_class == x),
                ),
                _ => false,
            };
            unboxed_carrier.then_some((id as ExprId, *arg))
        })
        .collect();
    for (id, arg) in self_casts_over_carriers {
        ir.exprs[id as usize] = IrExpr::Block {
            stmts: vec![],
            value: Some(arg),
        };
    }

    // A `checkcast X` that is the receiver of an `X.unbox-impl()` must KEEP its value-class type even for
    // an external value class (`((Result)boxed).unbox-impl()`) — `unbox-impl` is invoked on the boxed `X`,
    // so erasing the cast to the underlying would leave an `Object` on the stack (`VerifyError`). The cast
    // is only emitted as part of an unbox sequence, so preserving it can't affect a plain `as Result`.
    let unbox_receiver_casts: HashSet<u32> =
        type_operation_roles::recorded_unbox_type_operation_receivers(ir).collect();

    // 3. Erase every type carried inside an expression (locals, casts, vararg/array elements, …).
    //    Inside a value-class member body, an `is X`/`(X)other` whose type IS a value class must stay
    //    the BOXED class (the synthesized `equals` checks/casts the box) — keep it; everything else
    //    (including field-value operations over a nested value-class underlying) erases normally.
    let serialization_constructor_calls = (0..ir.exprs.len() as u32)
        .filter(|expression| {
            ir.generated_secondary_constructor_call(*expression)
                .is_some_and(|(_, role, _)| {
                    role == crate::ir::IrSecondaryConstructorRole::SerializationDeserialization
                })
        })
        .collect::<HashSet<_>>();
    let serialization_constructor_accessor_calls = serialization_constructor_calls
        .iter()
        .copied()
        .filter(|&expression| {
            let Some((class, _, constructor)) = ir.generated_secondary_constructor_call(expression)
            else {
                return false;
            };
            ir.classes[class as usize].secondary_ctors[constructor as usize].vc_params
        })
        .collect::<Vec<_>>();
    let mut erased_variable_defaults = Vec::new();
    // Generated serialization calls retain their boxed semantic parameter types, but the exact
    // selected constructor may still be private behind its marker accessor. Its recorded generated
    // declaration identity decides that ABI; no owner-wide parameter scan is involved.
    let mut value_class_parameter_constructions = serialization_constructor_accessor_calls;
    let primary_constructor_selection =
        hidden_constructors::PrimaryConstructorSelection::record(ir);
    for (i, e) in ir.exprs.iter_mut().enumerate() {
        let keep_box = vc_body_exprs.contains(&(i as u32));
        match e {
            IrExpr::Variable { ty, init, .. } => {
                let erased = erase(ty, &under);
                if is_ref(ty) && erased.is_jvm_scalar() {
                    if let Some(init) = init {
                        erased_variable_defaults.push((*init, erased));
                    }
                }
                *ty = erased;
            }
            // A property WRITE's carried type is the value it stores, which erases like any other. A
            // property READ's is the property's DECLARED type, which the pass's own analyses read
            // pre-erasure (exactly as they read a field's declared type) — erasing it would hide the
            // value class from them.
            IrExpr::PropertyWrite { ty, .. } => *ty = erase(ty, &under),
            IrExpr::TypeOp { type_operand, .. } => {
                // `is X` / `as X` on a value class keeps the BOXED type — the box is the only object that is
                // `instanceof X`, and a `checkcast X` of an `Any` yields a box the property access then
                // unboxes. Applies to every value class, classpath ones (`kotlin/Result`) included.
                let is_vc_ty = type_operand
                    .non_null()
                    .obj_internal()
                    .is_some_and(|fq_name| under.contains_key(&fq_name));
                if !is_vc_ty && !unbox_receiver_casts.contains(&(i as u32)) {
                    *type_operand = erase(type_operand, &under);
                }
                let _ = keep_box;
            }
            IrExpr::New {
                internal,
                ctor_params: Some(ps),
                ..
            } if !serialization_constructor_calls.contains(&(i as u32)) => {
                // Preserve the exact selected declaration fact before erasure. It applies uniformly
                // to local secondary and sibling-file constructors; owner origin is irrelevant.
                // A value class's own construction is `constructor-impl`, not the hidden-marker ABI
                // used by an ordinary class whose selected constructor declares a value-class
                // parameter.
                let hides = primary_constructor_selection.hides_value_class(
                    i as ExprId,
                    *internal,
                    ps,
                    &is_vc_ty,
                );
                if !is_value_class_internal(*internal, &under) && hides {
                    value_class_parameter_constructions.push(i as ExprId);
                }
                ps.iter_mut().for_each(|p| *p = erase(p, &under));
            }
            // A function value's `invoke` returns its declared type through the `FunctionN` generic slot — a
            // REFERENCE. A value-class return is therefore the BOXED value class (an `X` object): keep it as
            // `X` (do NOT erase to the underlying) so emit does `checkcast X` and a `.field` on the result
            // `unbox-impl`s it (see `is_boxed_vc`). The invariant — a VC in an `Object`/`FunctionN` slot is
            // the boxed VC — is upheld symmetrically by every producer (the callable-ref adapter and the
            // lambda/coroutine tail boxing).
            IrExpr::InvokeFunction { ret, .. } => {
                let boxed_vc = ret
                    .non_null()
                    .obj_internal()
                    .is_some_and(|fq| under.contains_key(&fq));
                if !boxed_vc {
                    *ret = erase(ret, &under);
                }
            }
            // An `Array<X>` of a value class is a reference array of the BOXED `X` (kotlinc) — keep the
            // element boxed (don't erase to the underlying); elements are `box-impl`'d when stored. A
            // non-value-class element is erased; a primitive array (`kotlin/IntArray`) has no element arg.
            IrExpr::Vararg { array_type, .. } | IrExpr::NewArray { array_type, .. } => {
                if let Ty::Obj(n, args) = array_type.non_null() {
                    if n.matches("kotlin/Array") {
                        if let Some(elem) = args.first().copied() {
                            let keep_boxed = elem
                                .non_null()
                                .obj_internal()
                                .is_some_and(|fq_name| under.contains_key(&fq_name));
                            let new_elem = if keep_boxed {
                                elem
                            } else {
                                erase(&elem, &under)
                            };
                            *array_type = Ty::obj_args("kotlin/Array", &[new_elem]);
                        }
                    }
                }
            }
            IrExpr::RefNew { elem, .. }
            | IrExpr::RefGet { elem, .. }
            | IrExpr::RefSet { elem, .. } => *elem = erase(elem, &under),
            IrExpr::Try { result, .. } => *result = erase(result, &under),
            _ => {}
        }
    }
    for call in value_class_parameter_constructions {
        ir.mark_value_class_parameter_construction(call);
    }
    for (init, erased) in erased_variable_defaults {
        if matches!(
            ir.exprs[init as usize],
            IrExpr::Const(crate::ir::IrConst::Null)
        ) {
            ir.exprs[init as usize] =
                IrExpr::Const(crate::ir::IrConst::zero_for_value_type(erased));
        }
    }
    // The exhaustive `when` result drives the emitter's verifier-visible merge type, not Kotlin type checking.
    // Once a value class has been realized as its carrier, the merge frame must use that same carrier
    // (`SampleId?` over a non-null `String` field is `String`, including its null branch). Leaving the
    // semantic classifier here while branch expressions have been erased produces a StackMapTable that
    // claims `LSampleId;` above an actual `String`.
    for result in ir.whens.exhaustive.values_mut() {
        *result = erase(result, &under);
    }

    // 4. Rewrite construction / property access. Each body carries its pre-erasure semantic slot types,
    //    so the same representation analysis used by step 5 distinguishes an unboxed carrier from a box.
    //    A lowered value-class member's slot zero therefore remains semantically `X` even though its
    //    static `*-impl` descriptor carries U.
    let mut s4_bodies: Vec<(ExprId, HashMap<u32, Ty>)> = Vec::new();
    for (fid, f) in ir.functions.iter().enumerate() {
        // SYNTHESIZED value-class members aren't rewritten (emitted boxed-correct) — EXCEPT `<init>`
        // (field-init/init-block over unboxed ctor params) and `constructor-impl` (moved `init { … }`). A
        // USER member IS rewritten after its static carrier ABI has been selected above.
        let is_vc = vc_methods.contains(&(fid as u32));
        // A source `operator fun get(index: Int)` also begins with `get`, but it is a user member rather
        // than a synthesized property getter. Use the same zero-parameter structural criterion as the ABI
        // signature pass above; a raw string-prefix branch would skip its construction/property rewrites.
        let synthesized_member = matches!(
            f.name.as_str(),
            "box-impl"
                | "unbox-impl"
                | "constructor-impl"
                | "equals-impl0"
                | "equals"
                | "hashCode"
                | "toString"
                | "<init>"
        ) || vc_sole_getter_fids.contains(&(fid as u32))
            || realized_members.instance_entries.contains(&(fid as u32));
        let user_vc_member = is_vc && !synthesized_member;
        if is_vc && !user_vc_member && f.name != "<init>" && f.name != "constructor-impl" {
            continue;
        }
        if let Some(root) = f.body {
            s4_bodies.push((root, slot_types[fid].clone()));
        }
        if let Some(defaults) = ir.param_defaults(fid as u32) {
            for &root in defaults.iter().flatten() {
                s4_bodies.push((root, slot_types[fid].clone()));
            }
        }
    }
    for class in 0..ir.classes.len() {
        constructor_slots.push_bodies(ir, class, &mut s4_bodies);
        for entry in &ir.classes[class].enum_entries {
            s4_bodies.extend(
                entry
                    .args
                    .iter()
                    .map(|&argument| (argument, HashMap::new())),
            );
        }
    }
    // Top-level property initializers run in the facade `<clinit>` (static, no params). A value-class
    // construction here (`val p = arrayListOf(X(0))`) must rewrite `new X` → `constructor-impl` too;
    // otherwise a private `<init>` leaks an `IllegalAccessError` from `<clinit>`.
    for (property, slots) in ir.statics.iter().zip(&orig_static_slots) {
        if let Some(init) = property.init {
            s4_bodies.push((init, slots.clone()));
        }
    }
    append_inline_body_scopes(ir, &mut s4_bodies, &slot_types, &inline_own_parameters);
    // Map each reachable target expr to its body's slot map. A real lambda body belongs only to its
    // lifted function; traversing it from the enclosing `Lambda` expression would interpret the same
    // slot indices in the wrong function scope.
    let mut target_slots: HashMap<ExprId, usize> = HashMap::new();
    for (bi, (root, _)) in s4_bodies.iter().enumerate() {
        let mut reach = HashSet::new();
        collect_reachable_scoped(&ir.exprs, *root, &mut reach);
        for id in reach {
            target_slots.entry(id).or_insert(bi);
        }
    }
    // Process in ascending ExprId order: a child (inner `.z`, created first → lower id) is rewritten
    // before its parent (outer `.x`), so a nested property-access chain's `prop_access` always sees the
    // child's already-rewritten (`unbox-impl`/coercion) form and decides box/unbox deterministically.
    let mut targets: Vec<ExprId> = target_slots.keys().copied().collect();
    targets.sort_unstable();
    // Exact identities of coercions created below to expose a value class's sole underlying
    // property. Their operand is the value-class carrier itself, but the coercion denotes property
    // extraction rather than an ordinary `X -> U` value conversion. Keep this backend-local origin
    // fact until boundary insertion so an Object carrier is not boxed back into `X`.
    let mut sole_property_coercions = HashSet::new();
    // User value-class member bodies normally stay out of the general boundary rewrite below because
    // their slot-0 `this` is the BOXED wrapper and their own member ABI deliberately preserves it.
    // A constructor nested in such a body is still an independent boundary, though: any argument whose
    // declared field/parameter is a non-null value class is physically its UNBOXED carrier. Collect only
    // those constructor edges here, using the same pre-erasure target types as the generic `New` handling
    // in step 5. This is classifier- and origin-neutral; anonymous captures are one producer of the shape,
    // but ordinary local/nested constructions obey the same representation rule.
    // Filled by each sole-property read of a nested value class's carrier and when step 5 applies
    // each `BoxOp::Unbox`; the representation queries of the later tail rewrites read it.
    let mut carrier_unboxes = CarrierUnboxes::new();
    let mut value_member_constructor_ops: Vec<(ExprId, BoxOp)> = Vec::new();
    for &id in &targets {
        let body = &s4_bodies[target_slots[&id]];
        let slots = &body.1;
        let repr_ctx = ReprCtx {
            exprs: &ir.exprs,
            funcs: &ir.functions,
            rets: &orig_rets,
            fields: &orig_fields,
            slots,
            under: &under,
            types: CallTypes::of(ir),
            physical: &ir.physical_types,
            field_getters: &field_getters,
            carrier_unboxes: &carrier_unboxes,
        };
        let i = id as usize;
        if let IrExpr::New {
            internal,
            args,
            ctor_params,
            defaults,
            default_prefix_count,
            ..
        } = &ir.exprs[i]
        {
            if serialization_constructor_calls.contains(&id) {
                for (&argument, &parameter) in args.iter().zip(
                    ctor_params
                        .as_deref()
                        .expect("generated constructor call retains its exact parameters"),
                ) {
                    let Some(value_class) = parameter
                        .non_null()
                        .obj_internal()
                        .filter(|classifier| under.contains_key(classifier))
                    else {
                        continue;
                    };
                    if repr_ctx.unboxed_value_class(argument, &under) == Some(value_class) {
                        value_member_constructor_ops
                            .push((argument, repr_ctx.box_op(argument, value_class)));
                    }
                }
                continue;
            }
            let fields;
            let params: &[Ty] = match cls_by_name.get(internal) {
                Some(&class) if !orig_fields[class].is_empty() => {
                    fields = orig_fields[class].clone();
                    &fields
                }
                _ => ctor_params.as_deref().unwrap_or(&[]),
            };
            for (&argument, parameter) in args.iter().zip(
                constructor_arguments::supplied_parameters(params, defaults, *default_prefix_count),
            ) {
                let Target::UnboxedX(value_class) = target(parameter, &under) else {
                    continue;
                };
                if repr_ctx.is_boxed_vc(argument, value_class)
                    && !value_member_constructor_ops
                        .iter()
                        .any(|(existing, _)| *existing == argument)
                {
                    value_member_constructor_ops.push((argument, BoxOp::Unbox(value_class)));
                }
            }
        }
        // First decide the rewrite WITHOUT holding a mutable borrow (so `prop_access` can `add_expr`).
        enum Rw {
            Ctor(IrExpr),
            /// A value whose checked JVM representation is the boxed value-class object.
            BoxedValue {
                expr: IrExpr,
                owner: TypeName,
            },
            /// A source value-class construction rewritten to its erased helper call. Keep this distinct
            /// from other helper-producing rewrites so downstream safety checks can rely on semantic
            /// origin instead of matching a generated method name.
            ValueConstruction {
                expr: IrExpr,
                owner: TypeName,
                underlying: Ty,
            },
            Prop {
                receiver: ExprId,
                owner: TypeName,
                result: Ty,
            },
            /// A selected value-class member that became a static `-impl`. Its former dispatch receiver
            /// becomes argument zero and must be unboxed when the user-member ABI supplied a box.
            ImplCall {
                receiver: ExprId,
                owner: TypeName,
                name: String,
                parameters: Vec<Ty>,
                result: Ty,
                args: Vec<Option<ExprId>>,
                extension_receiver: bool,
                /// The selected `-impl` in this file; a sibling file's has none here.
                function: Option<u32>,
                /// The sibling file's declaration, when the call selected one.
                module_target: Option<crate::fir::CallableId>,
            },
            /// Same-value-class non-null `==`/`!=` → `equals-impl0(U, U)Z`, negated for `!=` (kotlinc's ABI).
            VcEq {
                ne: bool,
                lhs: ExprId,
                rhs: ExprId,
                owner: TypeName,
                carrier: Ty,
            },
            /// Constructing a value class with defaulted params omitted.
            VcCtorDefault(default_constructions::DefaultConstruction),
        }
        if let IrExpr::Call {
            callee: Callee::Virtual { owner, name, .. },
            dispatch_receiver: Some(receiver),
            args,
        } = &ir.exprs[i]
        {
            if name == "equals" {
                crate::trace_compiler!(
                    "value_classes",
                    "equals candidate {id} owner={owner} known={} receiver={receiver} repr={} unboxed={:?} semantic={:?} physical={:?} args={:?}",
                    callable_under.contains_key(owner),
                    match repr_ctx.repr(*receiver) {
                        Repr::NotVc => "plain",
                        Repr::Unboxed(_) => "unboxed",
                        Repr::Boxed(_) => "boxed",
                    },
                    repr_ctx.unboxed_value_class(*receiver, &callable_under),
                    repr_ctx.types.get(receiver),
                    repr_ctx.physical.get(receiver),
                    args.iter()
                        .map(|argument| (
                            *argument,
                            repr_ctx.types.get(argument),
                            repr_ctx.physical.get(argument)
                        ))
                        .collect::<Vec<_>>()
                );
            }
        }
        let rw = match &ir.exprs[i] {
            // `new X(args)` → `X.constructor-impl(args): U`. The return is the underlying `U`; the
            // PARAMETER types come from the actual constructor arguments (a secondary constructor's
            // signature differs from the primary, e.g. `Sc(String)` delegating to `Sc(Int)`).
            IrExpr::New {
                internal,
                args,
                ctor_params,
                ctor_desc: None,
                external_target: _,
                defaults,
                default_prefix_count,
            } if under.contains_key(internal) => {
                let owner = *internal;
                let u = under
                    .get(&owner)
                    .map(|t| erase(t, &under))
                    .unwrap_or(Ty::Error);
                if !defaults.is_empty() && *default_prefix_count == 0 {
                    let ordinal = ir.construction_targets[&id].ordinal;
                    let function = realized_members.constructor_impls.get(&(owner, ordinal));
                    Some(Rw::VcCtorDefault(
                        default_constructions::DefaultConstruction {
                            owner,
                            underlying: u,
                            function: function.copied(),
                            args: args.clone(),
                            omitted: defaults.clone(),
                        },
                    ))
                } else {
                    let ret = desc(&u);
                    let params: String = match ctor_params {
                        Some(ps) => ps.iter().map(|p| desc(&erase(p, &under))).collect(),
                        None => ret.clone(),
                    };
                    Some(Rw::ValueConstruction {
                        expr: IrExpr::Call {
                            callee: Callee::Static {
                                owner,
                                name: "constructor-impl".to_string(),
                                descriptor: format!("({params}){ret}"),
                                inline: InlineKind::None,
                            },
                            dispatch_receiver: None,
                            args: args.clone(),
                        },
                        owner,
                        underlying: u,
                    })
                }
            }
            // An explicit coercion of an UNBOXED value class to a nullable `X?` (`a?.foo()` : `Z?`, the
            // `when`-branch reconciliation): `box-impl` it, so the boxed `X?` merges with the `null` branch.
            IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::ImplicitCoercion,
                arg,
                type_operand,
            } if type_operand.is_nullable()
                && type_operand
                    .non_null()
                    .obj_internal()
                    .is_some_and(|fq_name| {
                        under.contains_key(&fq_name) && nullable_is_boxed(fq_name, &under)
                    })
                // `Nothing?` contributes the null BOX itself to a nullable value-class slot. It is
                // not an underlying carrier to feed through `box-impl` (primitive-backed classes
                // would otherwise unbox null and throw before constructing the nullable result).
                && !repr_ctx.operand_null_only(*arg)
                // A generic call result physically returned through `Object` already carries a value
                // class as its BOX. The checked coercion narrows that box to `X?`; feeding the object
                // through `box-impl(U)` would instead cast it to the carrier wrapper (`Integer` for an
                // `int` carrier) and either double-box or throw. A declaration-returning value class
                // has its carrier result stamped separately and does not satisfy this condition. A
                // read of a generic property (`val boxed: T?`) is the same erased slot.
                && !ir
                    .physical_types
                    .get(arg)
                    .is_some_and(|physical| physical.is_erased_top())
                && !matches!(&ir.exprs[*arg as usize],
                    IrExpr::PropertyRead { ty, .. } if ty.non_null().is_ty_param())
                && !matches!(repr_ctx.repr(*arg), Repr::Boxed(_)) =>
            {
                let fq_name = type_operand.non_null().obj_internal().unwrap();
                let u = under
                    .get(&fq_name)
                    .map(|t| erase(t, &under))
                    .unwrap_or(Ty::Error);
                let owner_rendered = fq_name.render();
                Some(Rw::BoxedValue {
                    expr: IrExpr::Call {
                        callee: Callee::Static {
                            owner: fq_name,
                            name: "box-impl".to_string(),
                            descriptor: format!("({})L{owner_rendered};", desc(&u)),
                            inline: InlineKind::None,
                        },
                        dispatch_receiver: None,
                        args: vec![*arg],
                    },
                    owner: fq_name,
                })
            }
            // `x.v` (sole-field read): identity on an unboxed value, `unbox-impl()` on a boxed one.
            IrExpr::GetField {
                receiver,
                class,
                index,
            } if is_vc[*class as usize] => Some(Rw::Prop {
                receiver: *receiver,
                owner: fq[*class as usize],
                result: orig_fields[*class as usize][*index as usize],
            }),
            // The same read as a PROPERTY: a value class's sole property IS its erased underlying, so
            // reading it never goes through an accessor whatever the owner's declaration says.
            IrExpr::PropertyRead {
                receiver: Some(receiver),
                owner,
                name,
                ty,
                ..
            } if {
                crate::trace_compiler!(
                    "value_classes",
                    "property read candidate {}.{} underlying={:?}",
                    owner,
                    name,
                    vc_properties.get(owner)
                );
                vc_properties
                    .get(owner)
                    .is_some_and(|property| property == name)
            } =>
            {
                Some(Rw::Prop {
                    receiver: *receiver,
                    owner: *owner,
                    result: *ty,
                })
            }
            // A sole-property access resolved to `invokevirtual X.getV()` (e.g. inside another value
            // class's `init` block) — rewrite like the indexed getter.
            IrExpr::Call {
                callee:
                    Callee::Virtual {
                        owner,
                        name,
                        params,
                        ..
                    },
                dispatch_receiver: Some(receiver),
                ..
            } if vc_getters.get(owner).is_some_and(|g| g == name) => {
                value_class_name(*owner, &under).map(|owner| Rw::Prop {
                    receiver: *receiver,
                    owner,
                    result: params.as_ref().map_or(under[&owner], |(_, ret)| *ret),
                })
            }
            // An ordinary, already-selected `equals(Any?)` call on an UNBOXED value-class receiver.
            // This is purely JVM ABI realization: checked FIR and common IR retain the normal member
            // call. Equal unboxed classes compare their carriers directly; every other argument uses
            // the value class's static `equals-impl(U, Object)` entry point, whose first parameter is
            // the carrier and whose second parameter is boxed by the normal call-boundary pass below.
            IrExpr::Call {
                callee: Callee::Virtual { owner, name, .. },
                dispatch_receiver: Some(receiver),
                args,
            } if name == "equals" && args.len() == 1 && callable_under.contains_key(owner) => {
                match repr_ctx.unboxed_value_class(*receiver, &callable_under) {
                    Some(receiver_class) if receiver_class == *owner => {
                        let argument = args[0];
                        let argument_value = value_class_equals_argument(&ir.exprs, argument);
                        if repr_ctx.unboxed_value_class(argument_value, &callable_under)
                            == Some(*owner)
                        {
                            Some(Rw::Ctor(IrExpr::PrimitiveBinOp {
                                op: crate::ir::IrBinOp::Eq,
                                lhs: *receiver,
                                rhs: argument_value,
                            }))
                        } else {
                            let underlying = callable_under
                                .get(owner)
                                .map(|ty| erase(ty, &callable_under))
                                .unwrap_or(Ty::Error);
                            Some(Rw::Ctor(IrExpr::Call {
                                callee: Callee::Static {
                                    owner: *owner,
                                    name: "equals-impl".to_string(),
                                    descriptor: format!(
                                        "({}Ljava/lang/Object;)Z",
                                        desc(&underlying)
                                    ),
                                    inline: InlineKind::None,
                                },
                                dispatch_receiver: None,
                                args: vec![*receiver, argument],
                            }))
                        }
                    }
                    _ => None,
                }
            }
            // Checked same-module property reads may already be realized as a selected virtual call.
            // Once a computed value-class accessor becomes static `getX-impl(U)`, preserve that
            // selected declaration while adapting its dispatch receiver to parameter zero.
            IrExpr::Call {
                callee:
                    Callee::Virtual {
                        owner,
                        name,
                        params,
                        module_target,
                        ..
                    },
                dispatch_receiver: Some(receiver),
                args,
            } if under.contains_key(owner) => match cls_by_name.get(owner) {
                Some(class) => {
                    let expected = format!("{name}-impl");
                    ir.classes[*class].methods.iter().copied().find_map(|fid| {
                        let function = ir.functions.get(fid as usize)?;
                        (function.is_static
                            && (function.name == *name || function.name == expected))
                            .then(|| Rw::ImplCall {
                                receiver: *receiver,
                                owner: *owner,
                                name: function.name.clone(),
                                parameters: function.params.clone(),
                                result: function.ret,
                                args: args.iter().copied().map(Some).collect(),
                                extension_receiver: ir.extension_receiver_fns.contains(&fid),
                                function: Some(fid),
                                module_target: None,
                            })
                    })
                }
                // A member of a sibling file's value class: its static implementation takes the
                // erased carrier at parameter zero, followed by the member's (already erased)
                // declared parameters. Every argument is supplied — a call omitting a defaulted
                // argument is a `ModuleWithDefaults` call, never this virtual form.
                None => sibling_member_impls.get(&id).map(|impl_name| {
                    let (declared, result) = params
                        .clone()
                        .expect("a sibling value-class member call retains its declared signature");
                    let carrier = erase(&under[owner], &under);
                    Rw::ImplCall {
                        receiver: *receiver,
                        owner: *owner,
                        name: impl_name.clone(),
                        parameters: std::iter::once(carrier).chain(declared).collect(),
                        result,
                        args: args.iter().copied().map(Some).collect(),
                        extension_receiver: false,
                        function: None,
                        module_target: *module_target,
                    }
                }),
            },
            // A zero-arg `Any`-override dispatched VIRTUALLY on the value class itself (`id.hashCode()`
            // / `id.toString()` — e.g. a data class hashing its value-class field on the field's own
            // class, kotlinc's per-field shape) → the static `-impl` over the unboxed underlying
            // (`invokestatic Id.hashCode-impl(U)I`), exactly as kotlinc emits it. The receiver
            // becomes the static's sole argument (the unboxed `$this` — a `Static` callee's
            // `dispatch_receiver` is only consumed by the splice path, never plain emission).
            IrExpr::Call {
                callee: Callee::Virtual { owner, name, .. },
                dispatch_receiver: Some(receiver),
                args,
            } if args.is_empty()
                && (name == "hashCode" || name == "toString")
                && is_value_class_internal(*owner, &under) =>
            {
                let u = under
                    .get(owner)
                    .map(|t| erase(t, &under))
                    .unwrap_or(Ty::Error);
                let ret = if name == "hashCode" {
                    "I"
                } else {
                    "Ljava/lang/String;"
                };
                Some(Rw::Ctor(IrExpr::Call {
                    callee: Callee::Static {
                        owner: *owner,
                        name: format!("{name}-impl"),
                        descriptor: format!("({}){ret}", desc(&u)),
                        inline: InlineKind::None,
                    },
                    dispatch_receiver: None,
                    args: vec![*receiver],
                }))
            }
            // A selected semantic `Any` operation over a non-null UNBOXED receiver calls the value
            // class's static `-impl` directly rather than boxing the receiver to dispatch. The
            // provider attached the role to the exact declaration, and external-call realization
            // retained it on this exact call; this pass does not rediscover the declaration from a
            // JVM owner or method spelling.
            IrExpr::Call {
                dispatch_receiver: Some(receiver),
                args,
                ..
            } if args.is_empty()
                && ir.semantic_call_roles.contains_key(&id)
                && repr_ctx.operand_nonnull(*receiver)
                && matches!(repr_ctx.repr(*receiver), Repr::Unboxed(_)) =>
            {
                let (name, ret) = match ir.semantic_call_roles[&id] {
                    SemanticCallRole::KotlinAnyHashCode => ("hashCode-impl", "I"),
                    SemanticCallRole::KotlinAnyToString => ("toString-impl", "Ljava/lang/String;"),
                };
                let Repr::Unboxed(value_class) = repr_ctx.repr(*receiver) else {
                    unreachable!("guarded unboxed receiver")
                };
                let u = under
                    .get(&value_class)
                    .map(|t| erase(t, &under))
                    .unwrap_or(Ty::Error);
                Some(Rw::Ctor(IrExpr::Call {
                    callee: Callee::Static {
                        owner: value_class,
                        name: name.to_owned(),
                        descriptor: format!("({}){ret}", desc(&u)),
                        inline: InlineKind::None,
                    },
                    dispatch_receiver: None,
                    args: vec![*receiver],
                }))
            }
            // `a == b` / `a != b` where BOTH operands are the same non-null UNBOXED value class → the
            // class's static `equals-impl0(U, U)Z`, negated for `!=` (kotlinc's value-class equality ABI;
            // the underlying-level `areEqual`/`icmp` was semantically right but not kotlinc's shape).
            // Identity `===`/`!==` (RefEq/RefNe) is untouched; nullable and mixed operands are
            // specialized in step 5 (`equality`).
            IrExpr::PrimitiveBinOp {
                op: op @ (crate::ir::IrBinOp::Eq | crate::ir::IrBinOp::Ne),
                lhs,
                rhs,
            }
            | IrExpr::Equality {
                op: op @ (crate::ir::IrBinOp::Eq | crate::ir::IrBinOp::Ne),
                lhs,
                rhs,
                ..
            } => {
                let (l, r) = (*lhs, *rhs);
                match (repr_ctx.repr(l), repr_ctx.repr(r)) {
                    (Repr::Unboxed(x), Repr::Unboxed(y))
                        if x == y && repr_ctx.operand_nonnull(l) && repr_ctx.operand_nonnull(r) =>
                    {
                        Some(Rw::VcEq {
                            ne: matches!(op, crate::ir::IrBinOp::Ne),
                            lhs: l,
                            rhs: r,
                            owner: x,
                            carrier: erase(&under[&x], &under),
                        })
                    }
                    _ => None,
                }
            }
            // A source call resolved by class+method index before value-class realization may still
            // point at a user-written Any override after `synth_value_members` turns that exact
            // function into static `toString-impl`/`hashCode-impl`/`equals-impl`. Retain the selected
            // declaration, but realize its new ABI: the former dispatch receiver is parameter zero.
            IrExpr::MethodCall {
                class,
                index,
                receiver,
                args,
            } if is_vc[*class as usize]
                && ir.classes[*class as usize]
                    .methods
                    .get(*index as usize)
                    .and_then(|fid| ir.functions.get(*fid as usize))
                    .is_some_and(|function| function.is_static) =>
            {
                let fid = ir.classes[*class as usize].methods[*index as usize];
                let function = &ir.functions[fid as usize];
                Some(Rw::ImplCall {
                    receiver: *receiver,
                    owner: fq[*class as usize],
                    name: function.name.clone(),
                    parameters: function.params.clone(),
                    result: function.ret,
                    args: args.clone(),
                    extension_receiver: ir.extension_receiver_fns.contains(&fid),
                    function: Some(fid),
                    module_target: None,
                })
            }
            // `x.getV()` getter: identity on an unboxed value, `unbox-impl()` on a boxed one.
            IrExpr::MethodCall {
                class,
                index,
                receiver,
                ..
            } if is_vc[*class as usize] => {
                let cls = *class as usize;
                let name = ir.classes[cls]
                    .methods
                    .get(*index as usize)
                    .and_then(|fid| ir.functions.get(*fid as usize))
                    .map(|f| f.name.as_str());
                if name.is_some() && name == getter[cls].as_deref() {
                    let fid = ir.classes[cls].methods[*index as usize] as usize;
                    Some(Rw::Prop {
                        receiver: *receiver,
                        owner: fq[cls],
                        result: orig_rets[fid],
                    })
                } else {
                    None
                }
            }
            _ => None,
        };
        let rewrite = match rw {
            Some(Rw::Ctor(e)) => Some(e),
            Some(Rw::BoxedValue { expr, owner }) => {
                ir.physical_types.insert(id, Ty::obj_name(owner));
                Some(expr)
            }
            Some(Rw::ValueConstruction {
                expr,
                owner,
                underlying,
            }) => {
                ir.record_erased_value_construction(id, owner, underlying);
                // The construction becomes a static `constructor-impl` call over the same checked
                // declaration parameters. A generic `T` parameter consumes its argument as a box.
                if let Some(parameters) = ir.construction_declared_params.remove(&id) {
                    ir.call_declared_params.insert(id, parameters);
                }
                Some(expr)
            }
            Some(Rw::VcCtorDefault(construction)) => Some(construction.realize(ir, id, &under)),
            Some(Rw::Prop {
                receiver,
                owner,
                result,
            }) => {
                sole_property_coercions.insert(id);
                ir.logical_types.insert(id, result);
                if let Repr::Unboxed(nested) = repr_of_ty(&result, &under) {
                    carrier_unboxes.insert(id, nested);
                }
                ir.physical_types.insert(
                    id,
                    under
                        .get(&owner)
                        .map(|underlying| erase(underlying, &under))
                        .unwrap_or(Ty::Error),
                );
                Some(prop_access(
                    ir,
                    receiver,
                    owner,
                    result,
                    ReprInputs {
                        rets: &orig_rets,
                        fields: &orig_fields,
                        slots,
                        under: &under,
                        field_getters: &field_getters,
                        carrier_unboxes: &carrier_unboxes,
                    },
                ))
            }
            Some(Rw::ImplCall {
                receiver,
                owner,
                mut name,
                parameters,
                result,
                args,
                extension_receiver,
                function,
                module_target,
            }) => {
                let default_boxed_parameters = function
                    .and_then(|function| ir.default_stub_boxed_params.get(&function))
                    .cloned()
                    .unwrap_or_default();
                // This rewrite replaces an instance-shaped semantic call with the exact static
                // carrier implementation. Any earlier property/call stamp described the pre-rewrite
                // box; publish the implementation result now so a following sole-property read does
                // not try to unbox a carrier that is already primitive/reference-underlying.
                ir.physical_types.insert(
                    id,
                    representation::carrier_slot(result, &under).unwrap_or(result),
                );
                // The physical static call gains the former dispatch receiver at parameter zero.
                // Keep the checked declaration coordinates aligned with that new argument vector:
                // otherwise parameter zero from the source declaration is incorrectly applied to
                // the receiver (for example `X.foo(other: I)` boxes the `X` carrier as though it
                // were `other`). The receiver remains semantically the value class; this backend
                // pass alone decides that the selected `*-impl` consumes its carrier.
                if let Some(parameters) = ir.call_declared_params.get_mut(&id) {
                    *parameters = std::iter::once(Ty::obj_name(owner))
                        .chain(parameters.iter().copied())
                        .collect::<Vec<_>>()
                        .into_boxed_slice();
                }
                let inferred_boxed = ReprInputs {
                    rets: &orig_rets,
                    fields: &orig_fields,
                    slots,
                    under: &under,
                    field_getters: &field_getters,
                    carrier_unboxes: &carrier_unboxes,
                }
                .over(ir)
                .is_boxed_vc(receiver, owner);
                let receiver = if inferred_boxed {
                    let underlying = under
                        .get(&owner)
                        .map(|ty| erase(ty, &under))
                        .unwrap_or(Ty::Error);
                    unbox_call(ir, receiver, owner, &underlying)
                } else {
                    receiver
                };
                let uses_default_stub = args.iter().any(Option::is_none);
                let extension_prefix = usize::from(extension_receiver);
                let logical_parameter_count = args.len().saturating_sub(extension_prefix);
                let mask_count = logical_parameter_count.div_ceil(32).max(1);
                let mut masks = vec![0i32; mask_count];
                let mut call_args = Vec::with_capacity(
                    1 + args.len() + usize::from(uses_default_stub) * (mask_count + 1),
                );
                call_args.push(receiver);
                for (argument, parameter) in args.into_iter().zip(parameters.iter().skip(1)) {
                    match argument {
                        Some(argument) => call_args.push(argument),
                        None => {
                            let physical_index = call_args.len();
                            let placeholder_ty = default_boxed_parameters
                                .iter()
                                .find_map(|(index, ty)| (*index == physical_index).then_some(*ty))
                                .unwrap_or(*parameter);
                            call_args.push(ir.add_expr(IrExpr::Const(
                                crate::ir::IrConst::zero_for_value_type(placeholder_ty),
                            )));
                            let source_parameter = physical_index - 1;
                            if let Some(logical) = source_parameter.checked_sub(extension_prefix) {
                                masks[logical / 32] |= (1u32 << (logical % 32)) as i32;
                            }
                        }
                    }
                }
                if let Some(function) = function.filter(|_| !uses_default_stub) {
                    ir.jvm_member_targets.insert(id, function);
                }
                // The `-impl` of an override whose primitive result is boxed returns the wrapper.
                let boxed_result = match (function, module_target) {
                    (Some(function), _) => override_results
                        .boxes(function)
                        .then(|| ir.functions[function as usize].ret),
                    (None, Some(callable)) => override_results.boxed_callable_result(ir, callable),
                    (None, None) => None,
                };
                if let Some(primitive) = boxed_result.filter(|_| !uses_default_stub) {
                    override_results.record_static_member_call(id, primitive);
                }
                let descriptor = if uses_default_stub {
                    name.push_str("$default");
                    for mask in masks {
                        call_args.push(ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(mask))));
                    }
                    call_args.push(ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null)));
                    let mut stub_parameters = parameters;
                    for (index, ty) in default_boxed_parameters {
                        if let Some(parameter) = stub_parameters.get_mut(index) {
                            *parameter = ty;
                        }
                    }
                    stub_parameters.extend(std::iter::repeat_n(Ty::Int, mask_count));
                    stub_parameters.push(Ty::obj("java/lang/Object"));
                    ir_method_desc(&stub_parameters, &result)
                } else {
                    ir_method_desc(&parameters, &result)
                };
                Some(IrExpr::Call {
                    callee: Callee::Static {
                        owner,
                        name,
                        descriptor,
                        inline: InlineKind::None,
                    },
                    dispatch_receiver: None,
                    args: call_args,
                })
            }
            Some(Rw::VcEq {
                ne,
                lhs,
                rhs,
                owner,
                carrier,
            }) => {
                let call = equality::compare(ir, owner, &carrier, true, lhs, rhs);
                Some(equality::negated_if(ir, call, ne))
            }
            None => None,
        };
        if let Some(r) = rewrite {
            // The replacement is the carrier access or static `-impl`, not the virtual getter
            // the binding named. Leaving the binding would ask the later name stamp to rename
            // a call that no longer exists.
            accessor_names::retire_replaced_accessor_call(ir, id);
            // A user member whose static name is `unbox-impl` shares that spelling with the
            // representation call synthesized for `equals-impl`. Replacing the representation call
            // must drop its type-operation edge; the replacement is a different operation.
            type_operation_roles::retire_unbox_type_operation_edge(ir, id);
            ir.exprs[i] = r;
        }
    }

    // Property rewrites above can introduce a fresh `checkcast X` receiver for `X.unbox-impl()`.
    // Preserve those casts just like the unbox receivers that existed before expression-type erasure.
    let unbox_receiver_casts: HashSet<u32> = unbox_receiver_casts
        .into_iter()
        .chain(type_operation_roles::recorded_unbox_type_operation_receivers(ir))
        .collect();

    // 5. Box/unbox at call boundaries, per function so each value's slot type is known: an UNBOXED
    //    value-class value into a reference target (`Object`/generic/nullable-`X`) is `box-impl`'d; a
    //    BOXED one into an unboxed (non-null `X`) target is `unbox-impl`'d. Collect then apply.
    let mut ops: Vec<(ExprId, BoxOp)> = value_member_constructor_ops;
    // A `!!` over an UNBOXED primitive-underlying value class is redundant (a primitive can't be null);
    // kotlinc emits no `checkNotNull`. Strip such asserts — left in, they `checkNotNull` a primitive.
    let mut strip: Vec<(ExprId, ExprId)> = Vec::new();
    // `(comparison expr, is_ne)` — a `non-null-vc == null` folded to a constant `false`/`true`.
    let mut vacuous: Vec<(ExprId, bool)> = Vec::new();
    let mut equalities = Vec::new();
    // `(type-op expr, underlying)` — casts to NULLABLE reference-underlying value classes and
    // representation-changing implicit coercions are retargeted to the physical carrier. There is
    // no box-class instance at either boundary, so retaining the semantic value-class operand would
    // emit a wrong `checkcast` after the required unbox operation.
    let mut retarget: Vec<(ExprId, Ty)> = Vec::new();
    // Each body to box/unbox: every non-value-class-member function body (with its captured slot types),
    // plus every class `init { … }` block (slots = `this` + the ctor params), so a value-class member
    // call / boundary INSIDE an init block (`class B(val a: A) { init { a.f() } }`) is boxed too.
    let mut bodies: Vec<(ExprId, HashMap<u32, Ty>)> = Vec::new();
    for (fid, function) in ir.functions.iter().enumerate() {
        crate::trace_compiler!(
            "value_classes",
            "boundary body fid={fid} name={} value_member={} body={:?}",
            function.name,
            vc_methods.contains(&(fid as u32)),
            function.body
        );
        // A `constructor-impl` runs source constructor bodies over the carrier.
        if vc_methods.contains(&(fid as u32))
            && !lowered_value_members.contains(&(fid as u32))
            && !ir.jvm_value_class_constructor_impls.contains(&(fid as u32))
        {
            continue;
        }
        if let Some(root) = function.body {
            bodies.push((root, slot_types[fid].clone()));
        }
        if let Some(defaults) = ir.param_defaults(fid as u32) {
            for &root in defaults.iter().flatten() {
                bodies.push((root, slot_types[fid].clone()));
            }
        }
    }
    for class in 0..ir.classes.len() {
        constructor_slots.push_bodies(ir, class, &mut bodies);
    }
    // Top-level property initializers (facade `<clinit>`, static) — box/unbox their value-class accesses
    // and boundary constructions just like any function body.
    for (property, slots) in ir.statics.iter().zip(&orig_static_slots) {
        if let Some(init) = property.init {
            bodies.push((init, slots.clone()));
        }
    }
    append_inline_body_scopes(ir, &mut bodies, &slot_types, &inline_own_parameters);
    for (root, slots) in &bodies {
        let root = *root;
        let repr_ctx = ReprCtx {
            exprs: &ir.exprs,
            funcs: &ir.functions,
            rets: &orig_rets,
            fields: &orig_fields,
            slots,
            under: &under,
            types: CallTypes::of(ir),
            physical: &ir.physical_types,
            field_getters: &field_getters,
            carrier_unboxes: &carrier_unboxes,
        };
        let mut reach = HashSet::new();
        collect_reachable_scoped(&ir.exprs, root, &mut reach);
        for id in reach {
            if let IrExpr::NotNullAssert { operand, .. } = &ir.exprs[id as usize] {
                match repr_ctx.repr(*operand) {
                    // `X!!` over an UNBOXED primitive-underlying value class is redundant (a primitive
                    // can't be null); kotlinc emits no `checkNotNull`. Strip the assert.
                    Repr::Unboxed(x)
                        if under
                            .get(&x)
                            .map(|u| !is_ref(&erase(u, &under)))
                            .unwrap_or(false) =>
                    {
                        strip.push((id, *operand));
                    }
                    // `X!!` over a BOXED value class yields the NON-NULL `X` but its REPRESENTATION stays
                    // boxed — a consumer that wants the unboxed underlying unboxes at its own boundary, so
                    // unboxing here would regress a `!!` feeding a boxed slot (the `kt27096` tests).
                    other => {
                        crate::trace_compiler!(
                            "value_classes",
                            "!! at expr {id} operand {operand} repr={} (no rewrite)",
                            match other {
                                Repr::Unboxed(_) => "Unboxed",
                                Repr::Boxed(_) => "Boxed",
                                Repr::NotVc => "NotVc",
                            }
                        );
                    }
                }
            }
            // A type op (`as`/`is`) on an unboxed value class is a REFERENCE-position boundary:
            //   * to the value class ITSELF (`as X`) — identity; strip the `checkcast X` (the value is
            //     the underlying, not a box; the cast would `ClassCastException`).
            //   * to a SUPERTYPE (`as Any`, `as Interface`, `is Comparable`) — box the value first (the
            //     box, not the raw underlying, is what carries that type), then the `checkcast`/
            //     `instanceof` runs on the box.
            if let IrExpr::TypeOp {
                op:
                    op @ (crate::ir::IrTypeOp::Cast
                    | crate::ir::IrTypeOp::CastNonNull
                    | crate::ir::IrTypeOp::SafeCast
                    | crate::ir::IrTypeOp::InstanceOf
                    | crate::ir::IrTypeOp::NotInstanceOf),
                arg,
                type_operand,
            } = &ir.exprs[id as usize]
            {
                let to_self = type_operand
                    .non_null()
                    .obj_internal()
                    .is_some_and(|fq_name| under.contains_key(&fq_name));
                if let Repr::Unboxed(x) = repr_ctx.repr(*arg) {
                    if to_self
                        && matches!(
                            op,
                            crate::ir::IrTypeOp::Cast | crate::ir::IrTypeOp::CastNonNull
                        )
                        // A cast feeding `unbox-impl` must NOT be stripped — its operand is statically
                        // typed unboxed (`Result` lambda param) but actually a BOX, so the `checkcast` is
                        // required for the `unbox-impl` receiver to verify.
                        && !unbox_receiver_casts.contains(&id)
                    {
                        strip.push((id, *arg));
                    } else if (!to_self && is_ref(type_operand))
                        // `is X` on the unboxed value itself: the underlying is not an `X` instance, so
                        // the `instanceof X` must run on the box, like the supertype case.
                        || (to_self
                            && matches!(
                                op,
                                crate::ir::IrTypeOp::InstanceOf | crate::ir::IrTypeOp::NotInstanceOf
                            ))
                    {
                        ops.push((*arg, repr_ctx.box_op(*arg, x)));
                    }
                }
                if to_self
                    && matches!(
                        op,
                        crate::ir::IrTypeOp::Cast | crate::ir::IrTypeOp::CastNonNull
                    )
                    && type_operand.is_nullable()
                {
                    let fq = type_operand.non_null().obj_internal().unwrap();
                    if !nullable_is_boxed(fq, &under)
                        && !matches!(repr_ctx.repr(id), Repr::Boxed(boxed) if boxed == fq)
                    {
                        retarget.push((id, erase(&under[&fq], &under)));
                    }
                }
            }
            // A semantic `ImplicitCoercion(Object -> X)` already is the complete representation
            // boundary; the backend realizes its selected target adapter. This pass only handles the
            // distinct sole-field case below, where the coercion's TARGET is the underlying and the
            // boxed value-class identity exists solely on its source expression.
            if let IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::ImplicitCoercion,
                arg,
                type_operand,
            } = &ir.exprs[id as usize]
            {
                // `null` coerced to a nullable reference-carried value class (`null as X?`) is the
                // carrier's null directly. Keeping `LX;` on the coercion makes the verifier see an
                // `X` where the erased declaration descriptor requires `U`. This is valid only for
                // the nullable-unboxed representation; primitive/null-capable carriers remain boxed.
                if let Target::UnboxedX(target) = target(type_operand, &under) {
                    if type_operand.is_nullable()
                        && !nullable_is_boxed(target, &under)
                        && repr_ctx.operand_null_only(*arg)
                    {
                        retarget.push((id, erase(&under[&target], &under)));
                        continue;
                    }
                }
                // A declaration-returning value-class call already yields the unboxed carrier. The
                // semantic coercion to `X` must therefore target that carrier too; retaining `X`
                // makes the emitter insert `checkcast X` around an `Object` underlying such as
                // `Result` (`runCatching { 1 }` then casts `Integer` to `kotlin.Result`).
                if let (Repr::Unboxed(source), Target::UnboxedX(target)) =
                    (repr_ctx.repr(*arg), target(type_operand, &under))
                {
                    if source == target {
                        retarget.push((id, erase(&under[&source], &under)));
                        continue;
                    }
                }
                // A nullable/erased read can be physically BOXED even when its smart-cast result is
                // the non-null value class. Crossing to the value class's ordinary unboxed carrier
                // is a real representation boundary (`r: Result<T>?; r!!` is the motivating case).
                // The semantic `TypeOp` records the checked conversion; this pass supplies the
                // backend-owned box adapter without recovering any source spelling or declaration.
                if let (Repr::Boxed(source), Target::UnboxedX(target)) =
                    (repr_ctx.repr(*arg), target(type_operand, &under))
                {
                    if source == target {
                        ops.push((*arg, BoxOp::unbox(source, type_operand.is_nullable())));
                        retarget.push((id, erase(&under[&source], &under)));
                        continue;
                    }
                }
                // A specialized generic value-class property can retain the nested value class as
                // its logical result while already producing that class's concrete carrier. For
                // `Outer<T : Inner<Int>>.value`, Kotlin/JVM recursively uses `Inner`'s `int` carrier;
                // the following checked coercion to `Inner<Int>` is therefore an identity, not a
                // boxed `Inner` that needs `unbox-impl`. The physical result recorded by checked
                // lowering is authoritative whenever the carrier is not the ambiguous erased top.
                if let Target::UnboxedX(target) = target(type_operand, &under) {
                    let carrier = erase(&under[&target], &under);
                    if !carrier.is_erased_top()
                        && repr_ctx
                            .physical
                            .get(arg)
                            .is_some_and(|physical| physical.non_null() == carrier.non_null())
                    {
                        retarget.push((id, carrier));
                        continue;
                    }
                }
                // A suspend call always has an erased `Object` JVM return, but that does not make
                // its value-class result a generic boxed slot. The target pass recorded the exact
                // CPS representation before rewriting expressions. When that representation is the
                // value class's carrier, the semantic coercion is an identity after resume; casting
                // the carrier to the box and invoking `unbox-impl` would double-unbox it.
                if let Target::UnboxedX(target) = target(type_operand, &under) {
                    let carrier = erase(&under[&target], &under);
                    if let Some(crate::ir::IrValueClassSuspendResult::Carrier {
                        carrier: boundary,
                        ..
                    }) = ir.value_class_suspend_calls.get(arg).copied()
                    {
                        if boundary.canonical_semantic() == carrier.canonical_semantic() {
                            retarget.push((id, boundary));
                            continue;
                        }
                    }
                }
                // A checked generic call has a concrete logical result but physically returns its
                // erased bound. When that bound is `Object`, a value class necessarily occupies the
                // slot as its box. The frontend lowering records that physical result on the whole
                // call expression; consume it here rather than re-inferring generic substitution or
                // teaching the emitter about source declarations.
                if let Target::UnboxedX(target) = target(type_operand, &under) {
                    if repr_ctx
                        .physical
                        .get(arg)
                        .is_some_and(|physical| physical.is_erased_top())
                    {
                        ops.push((*arg, BoxOp::unbox(target, type_operand.is_nullable())));
                        retarget.push((id, erase(&under[&target], &under)));
                        continue;
                    }
                }
                // A checked cast/smart cast from an ordinary reference supertype (`Any`, an
                // interface, or an unstamped erased property read) to a non-null value class yields
                // the BOX object. The target position wants the carrier, so realize the same
                // cast-plus-`unbox-impl` boundary as the explicitly stamped generic-call case above.
                // Kotlin has no implicit conversion from an unrelated concrete value to a value
                // class; consequently a `NotVc` operand under this checked coercion is a boxed
                // value-class reference, never a raw carrier discovered by guesswork. Coerced to `X?`
                // over a reference carrier, the box may be null and unboxes null-safely.
                // A sole-property read yielding a nested value class over a reference carrier
                // (`zn.z!!` for `ZN(val z: Z1?)`) already is that class's carrier.
                let mut operand = *arg;
                while let IrExpr::NotNullAssert { operand: inner, .. } = ir.exprs[operand as usize]
                {
                    operand = inner;
                }
                if let Target::UnboxedX(target) = target(type_operand, &under) {
                    if sole_property_coercions.contains(&operand)
                        && repr_ctx.unboxed_value_class(operand, &under) == Some(target)
                    {
                        retarget.push((id, erase(&under[&target], &under)));
                        continue;
                    }
                    if matches!(repr_ctx.repr(*arg), Repr::NotVc)
                        && !repr_ctx.operand_null_only(*arg)
                    {
                        ops.push((*arg, BoxOp::unbox(target, type_operand.is_nullable())));
                        retarget.push((id, erase(&under[&target], &under)));
                        continue;
                    }
                }
                // A sole-property read of a nested value class (`ic.s` for `IC(val s: I0)`) is that
                // class's carrier, which the receiver's carrier already is.
                if let (true, Target::UnboxedX(nested)) = (
                    sole_property_coercions.contains(&id),
                    target(type_operand, &under),
                ) {
                    retarget.push((id, erase(&under[&nested], &under)));
                }
                // The sole-field coercion (`w.v` → `ImplicitCoercion(<w>, U)`) over a BOXED receiver
                // (`w!!` of a boxed `W?` shared cell): unbox the receiver first — otherwise the
                // emitter coerces the box reference straight to the underlying (`checkcast Integer`
                // on a `W` → CCE). Only a recorded sole-property read: `vc as Any?` over a `W(val a: Any?)`
                // has the carrier's type too, and keeps the box.
                if let (Some(x), true) = (
                    match repr_ctx.repr(*arg) {
                        Repr::Boxed(x) => Some(x),
                        _ => None,
                    },
                    sole_property_coercions.contains(&id),
                ) {
                    let u = under.get(&x).map(|t| erase(t, &under));
                    if u.map(|u| u.non_null()) == Some(type_operand.non_null()) {
                        ops.push((*arg, BoxOp::Unbox(x)));
                    }
                }
                // A checked coercion from a value class to a reference supertype (`X` -> `Any`, an
                // interface, or an erased type parameter) is itself a representation boundary. Realize
                // the value-class box on the operand before the ordinary JVM coercion sees its carrier;
                // otherwise a primitive carrier would be wrapped as `Integer` instead of `X`. The common
                // boundary helper also recognizes an exact reference underlying and leaves that identity
                // conversion alone.
                if !sole_property_coercions.contains(&id)
                    && is_ref(type_operand)
                    && matches!(target(type_operand, &under), Target::Boxed | Target::Other)
                {
                    record_value_boundary(
                        &mut ops,
                        &ir.exprs,
                        &repr_ctx,
                        *arg,
                        *type_operand,
                        &under,
                    );
                }
            }
            // A value-class property accessor is a static `-impl` over the unboxed carrier, regardless
            // of whether this compilation or a dependency declared it. The sole stored property was
            // already rewritten to identity; every remaining semantic property read keeps the carrier
            // representation expected by its selected accessor.
            if let IrExpr::PropertyRead {
                receiver: Some(receiver),
                owner,
                ..
            } = &ir.exprs[id as usize]
            {
                if under.contains_key(owner) {
                    if let Repr::Boxed(x) = repr_ctx.repr(*receiver) {
                        ops.push((*receiver, BoxOp::Unbox(x)));
                    }
                }
            }
            // A member call (`toString`/`equals`/`hashCode`/user method) on an UNBOXED value class
            // dispatches on the boxed object — box the receiver. (Getter calls were already rewritten to
            // identity property access in step 4, so only real instance-method calls remain here.)
            if let IrExpr::MethodCall {
                class,
                index,
                receiver,
                args,
            } = &ir.exprs[id as usize]
            {
                if is_vc[*class as usize] || !is_value_class_internal(fq[*class as usize], &under) {
                    match repr_ctx.repr(*receiver) {
                        Repr::Unboxed(x) => ops.push((*receiver, BoxOp::Box(x))),
                        Repr::Boxed(x)
                            if is_vc[*class as usize]
                                && ir
                                    .physical_types
                                    .get(receiver)
                                    .is_some_and(|ty| ty.is_erased_top()) =>
                        {
                            ops.push((*receiver, BoxOp::Narrow(x)));
                        }
                        _ => {}
                    }
                }
                // A USER value-class member keeps its value-class PARAMS boxed (`fun foo(x: Z)` → `foo(LZ;)`,
                // unlike a free function where `Z` erases). So an UNBOXED `Z` arg at such a param must box.
                if let Some(&fid) = ir.classes[*class as usize].methods.get(*index as usize) {
                    let params = ir.functions[fid as usize].params.clone();
                    for (k, a) in args.clone().into_iter().enumerate() {
                        let Some(a) = a else { continue };
                        if let Some(fq_name) =
                            params.get(k).and_then(|p| p.non_null().obj_internal())
                        {
                            if under.contains_key(&fq_name)
                                && matches!(repr_ctx.repr(a), Repr::Unboxed(ref x) if x == &fq_name)
                            {
                                ops.push((a, repr_ctx.box_op(a, fq_name)));
                            }
                        }
                    }
                }
            }
            // `==`/`!=` involving a value class. kotlinc compares two values of the SAME value class by
            // their unboxed underlying (`areEqual`/`icmp` — already correct), but a value class against
            // ANY OTHER operand (`Any`, a different type) is compared BOXED, so the synthesized
            // `equals` (with its `is X` type check) decides — `A("") == ""` must be `false`, not a raw
            // `areEqual("","")`. Box the value-class operand in that mixed case.
            if let IrExpr::PrimitiveBinOp {
                op: op @ (crate::ir::IrBinOp::Eq | crate::ir::IrBinOp::Ne),
                lhs,
                rhs,
            }
            | IrExpr::Equality {
                op: op @ (crate::ir::IrBinOp::Eq | crate::ir::IrBinOp::Ne),
                lhs,
                rhs,
                ..
            } = &ir.exprs[id as usize]
            {
                let (l, r) = (*lhs, *rhs);
                let is_ne = matches!(op, crate::ir::IrBinOp::Ne);
                let null_of = |e: ExprId| {
                    matches!(
                        ir.exprs[e as usize],
                        IrExpr::Const(crate::ir::IrConst::Null)
                    )
                };
                // `vc == null` on a NON-NULL value class is vacuously `false` (`!=` → `true`), regardless
                // of the underlying (a non-null `A(null)` is NOT null). kotlinc folds it to a constant.
                let vc_side = if null_of(l) {
                    Some(r)
                } else if null_of(r) {
                    Some(l)
                } else {
                    None
                };
                if let Some(vc) = vc_side {
                    crate::trace_compiler!(
                        "value_classes",
                        "value/null comparison expr {id} value={vc} {:?} repr={} nonnull={}",
                        &ir.exprs[vc as usize],
                        match repr_ctx.repr(vc) {
                            Repr::Unboxed(_) => "Unboxed",
                            Repr::Boxed(_) => "Boxed",
                            Repr::NotVc => "NotVc",
                        },
                        repr_ctx.operand_nonnull(vc),
                    );
                    if matches!(repr_ctx.repr(vc), Repr::Unboxed(_)) && repr_ctx.operand_nonnull(vc)
                    {
                        vacuous.push((id, is_ne));
                        continue;
                    }
                }
                if let Some(specialized) = equality::specialize(&repr_ctx, l, r) {
                    equalities.push((id, specialized));
                    continue;
                }
                for (a, other) in [(l, r), (r, l)] {
                    if let Repr::Unboxed(x) = repr_ctx.repr(a) {
                        let other_repr = repr_ctx.repr(other);
                        // A `Float`/`Double` underlying uses IEEE TOTAL-ORDER equality (`NaN == NaN`,
                        // `0.0 != -0.0`), which the synthesized `equals`/`areEqual` path implements but a
                        // raw `dcmp`/`fcmp` does not — so box even a same-class pair to route through it.
                        // `kotlin_class_internal` (not `obj_internal`): the erased underlying arrives as a
                        // bare `Ty::Float`/`Ty::Double` variant, whose `obj_internal()` is `None` — which
                        // would miss the total-order case and leave a raw `fcmp`/`dcmp` in place.
                        let total_order = matches!(
                            under.get(&x).map(|u| erase(u, &under)).and_then(|u| u.non_null().kotlin_class_internal()),
                            Some(fq_name) if is_ieee_fp(fq_name)
                        );
                        // "Same value class, same representation" — both UNBOXED. If the other side is
                        // BOXED (a nullable-`X` over a primitive, say), box this one too so both compare
                        // boxed (`areEqual` → `equals`), not a raw `icmp` of `LX;` against the underlying.
                        let same_vc =
                            !total_order && matches!(&other_repr, Repr::Unboxed(o) if *o == x);
                        let other_null = matches!(
                            ir.exprs[other as usize],
                            IrExpr::Const(crate::ir::IrConst::Null)
                        );
                        // A non-null operand boxes directly; a possibly-null one (`A?` over a reference)
                        // boxes null-safely (`a == null ? null : box-impl(a)`) so the ctor null-check
                        // isn't hit. Either way `areEqual` then runs the synthesized `equals`.
                        if !same_vc && !other_null {
                            ops.push((a, repr_ctx.box_op(a, x)));
                        }
                    }
                }
            }
            // An UNBOXED value-class receiver is boxed with `box-impl` by a virtual call whose owner is
            // not the value class (an interface, an `IFoo by Z(x)` forwarder) or which is a
            // SIBLING-FILE member (`params: Some`), by a `super` call, by an inherited default whose
            // provider is an interface's `$default`, and by the nullable-Any `toString` intrinsic.
            if let IrExpr::Call {
                callee,
                dispatch_receiver: Some(recv),
                ..
            } = &ir.exprs[id as usize]
            {
                let boxes = match callee {
                    Callee::Virtual { owner, params, .. } => {
                        !is_value_class_internal(*owner, &under) || params.is_some()
                    }
                    Callee::Special { .. } => true,
                    Callee::ModuleWithDefaults { .. } => default_calls::provider_owner(ir, callee)
                        .is_some_and(|owner| !is_value_class_internal(owner, &under)),
                    Callee::Intrinsic { operation, .. } => {
                        *operation == crate::ir::IrIntrinsic::NullableAnyToString
                    }
                    _ => false,
                };
                if boxes {
                    if let Repr::Unboxed(x) = repr_ctx.repr(*recv) {
                        ops.push((*recv, repr_ctx.box_op(*recv, x)));
                    }
                }
            }
            // The RECEIVER of a value-class MEMBER realized as a static `-impl` (`Result.getOrNull-impl(U)`,
            // `X.foo-<hash>(U, …)`) is the UNBOXED underlying `$this`. A BOXED value-class receiver reaching
            // it (a `FunctionN.invoke` result, a boxed local, a boxed member arg) must unbox. `box-impl` /
            // `constructor-impl` are static with no receiver; `unbox-impl` takes the box itself — both excluded.
            if let IrExpr::Call {
                callee: Callee::Static { owner, name, .. } | Callee::Virtual { owner, name, .. },
                dispatch_receiver: Some(recv),
                ..
            } = &ir.exprs[id as usize]
            {
                if is_value_class_internal(*owner, &under)
                    && name.contains("-impl")
                    && name != "unbox-impl"
                    && name != "box-impl"
                    && name != "constructor-impl"
                {
                    if let Repr::Boxed(x) = repr_ctx.repr(*recv) {
                        ops.push((*recv, BoxOp::Unbox(x)));
                    }
                }
            }
            // The RECEIVER of a value-class EXTENSION realized as a static FACADE method
            // (`kotlin/ResultKt.getOrThrow-impl(Object)` for `fun Result<T>.getOrThrow()`) is carried as
            // `args[0]` (NOT `dispatch_receiver`) and the facade takes the UNBOXED underlying. The lowerer
            // records the extension's declared source receiver (`ext_call_source_receiver`) with no
            // value-class reasoning of its own; decide here: when that receiver is a REFERENCE-underlying
            // value class and `args[0]` arrives BOXED (a bridge `C().foo()` overriding `Any`, a nullable
            // `x!!`, or an `as Result` cast), unbox it. A generic type-variable receiver is never recorded,
            // so `foo`-style generics keep their boxed receiver.
            let recv_is_ref_vc = ir
                .ext_call_source_receiver
                .get(&id)
                .and_then(|t| t.obj_internal())
                .is_some_and(|fq| {
                    under
                        .get(&fq)
                        .is_some_and(|underlying| erase(underlying, &under).is_reference())
                });
            if recv_is_ref_vc {
                if let IrExpr::Call { args, .. } = &ir.exprs[id as usize] {
                    if let Some(&a0) = args.first() {
                        if let Repr::Boxed(x) = repr_ctx.repr(a0) {
                            ops.push((a0, BoxOp::Unbox(x)));
                        }
                    }
                }
            }
            // Dynamic invokes, reference varargs, and string concatenations are the erased
            // reference boundaries handled here.
            if let IrExpr::InvokeFunction { args, .. }
            | IrExpr::Vararg { elements: args, .. }
            // A value-class part of a string template flows into `StringBuilder.append(Object)` /
            // `String.valueOf(Object)`, so it must box (→ the value class's `toString`) — unless it
            // is a non-null unboxed value, which kotlinc renders directly through the static
            // `toString-impl` over its carrier.
            | IrExpr::StringConcat(args) = &ir.exprs[id as usize]
            {
                let template = matches!(&ir.exprs[id as usize], IrExpr::StringConcat(_));
                for a in args.clone() {
                    let representation = repr_ctx.repr(a);
                    crate::trace_compiler!(
                        "value_classes",
                        "reference aggregate expr {id} element {a} {:?} repr={}",
                        &ir.exprs[a as usize],
                        match representation {
                            Repr::Unboxed(_) => "Unboxed",
                            Repr::Boxed(_) => "Boxed",
                            Repr::NotVc => "NotVc",
                        }
                    );
                    if let Repr::Unboxed(x) = representation {
                        let op = match repr_ctx.box_op(a, x) {
                            BoxOp::Box(x) if template => BoxOp::StringOf(x),
                            op => op,
                        };
                        ops.push((a, op));
                    }
                }
            }
            if let IrExpr::Call { callee, args, .. } = &ir.exprs[id as usize] {
                call_arguments::record_boundaries(
                    callee,
                    args,
                    ir.call_declared_params
                        .get(&id)
                        .map(|parameters| parameters.as_ref()),
                    recv_is_ref_vc,
                    &under,
                    &repr_ctx,
                    &mut ops,
                );
            }
            // Each `(value expr, target type)` boundary in this expression.
            let pairs: Vec<(ExprId, Ty)> = match &ir.exprs[id as usize] {
                // Checked declaration parameters were already applied uniformly above. They are
                // authoritative for every call shape, so no origin-specific fallback may reinterpret
                // those arguments from a source function or realized descriptor.
                IrExpr::Call { .. } if ir.call_declared_params.contains_key(&id) => Vec::new(),
                // The boundary target types are the constructor's parameter types, read from wherever they
                // are known — the same for any owner: the named class's own field types when it has them
                // (an in-IR primary ctor), otherwise the node's explicit `ctor_params` (a fieldless
                // synthesized ctor like a `FunctionReferenceImpl` subclass, OR an other-file/module ctor
                // whose param types krusty carries on the node). No same-file/other-file branch.
                IrExpr::New {
                    internal,
                    args,
                    ctor_params,
                    defaults,
                    default_prefix_count,
                    ..
                } => {
                    if serialization_constructor_calls.contains(&id) {
                        Vec::new()
                    } else {
                        let fields;
                        let targets: &[Ty] = match cls_by_name.get(internal) {
                            Some(&c) if !orig_fields[c].is_empty() => {
                                fields = orig_fields[c].clone();
                                &fields
                            }
                            _ => ctor_params.as_deref().unwrap_or(&[]),
                        };
                        args.iter()
                            .zip(constructor_arguments::supplied_parameters(
                                targets,
                                defaults,
                                *default_prefix_count,
                            ))
                            .map(|(a, p)| (*a, *p))
                            .collect()
                    }
                }
                IrExpr::Call { callee, args, .. } if callee.source_function().is_some() => {
                    let function = callee
                        .source_function()
                        .expect("guarded same-file function call");
                    let mut parameters = orig_params[function as usize].clone();
                    if matches!(
                        callee,
                        Callee::LocalDefault(_)
                            | Callee::ClassStaticDefault { .. }
                            | Callee::LocalWithDefaults { .. }
                            | Callee::ClassStaticWithDefaults { .. }
                    ) {
                        if let Some(boxed) = ir.default_stub_boxed_params.get(&function) {
                            for &(index, ty) in boxed {
                                if let Some(parameter) = parameters.get_mut(index) {
                                    // `Target::Boxed` is the semantic representation boundary. The
                                    // physical descriptor remains the non-null box type recorded in
                                    // `default_stub_boxed_params`.
                                    *parameter = Ty::nullable(ty);
                                }
                            }
                        }
                    }
                    let omitted = match callee {
                        Callee::LocalWithDefaults { defaults, .. }
                        | Callee::ClassStaticWithDefaults { defaults, .. } => {
                            Some(defaults.as_ref())
                        }
                        _ => None,
                    };
                    args.iter()
                        .zip(
                            parameters
                                .into_iter()
                                .enumerate()
                                .filter_map(|(parameter, ty)| {
                                    omitted
                                        .is_none_or(|defaults| {
                                            !defaults.contains(&(parameter as u32))
                                        })
                                        .then_some(ty)
                                }),
                        )
                        .map(|(argument, parameter)| (*argument, parameter))
                        .collect()
                }
                // A semantic sibling-source default call still carries only supplied arguments.
                // Adapt each one against the provider declaration parameter that remains after
                // removing omitted ordinals; JVM placeholders are created only after this pass.
                IrExpr::Call {
                    callee:
                        Callee::ModuleWithDefaults {
                            target,
                            default_provider,
                            defaults,
                            ..
                        },
                    args,
                    ..
                } => args
                    .iter()
                    .zip(default_calls::supplied_provider_parameters(
                        ir,
                        *target,
                        *default_provider,
                        defaults,
                    ))
                    .map(|(argument, parameter)| {
                        (
                            repr_ctx.through_erased_generic_coercion(*argument).0,
                            parameter,
                        )
                    })
                    .collect(),
                // A sibling-source call has already crossed from its stable `Module` identity into
                // the JVM `CrossFile` realization. Its retained finalized declaration signature is
                // still the authoritative representation boundary: a concrete value class selected
                // for a declaration type parameter must be BOXED into the erased generic slot. A
                // member `$default` bridge additionally leads with its dispatch receiver and trails
                // with masks/marker; neither is a Kotlin declaration parameter.
                IrExpr::Call {
                    callee:
                        Callee::CrossFile {
                            module_target: Some(target),
                            module_default_call,
                            ..
                        },
                    args,
                    ..
                } => ir
                    .referenced_module_callables
                    .get(target)
                    .map(|callable| {
                        let offset = usize::from(*module_default_call && callable.owner.is_some());
                        args.iter()
                            .skip(offset)
                            .zip(callable.parameters.iter())
                            .map(|(argument, parameter)| {
                                (
                                    repr_ctx.through_erased_generic_coercion(*argument).0,
                                    *parameter,
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                // `Array<T>.set(index, value)` stores into the receiver array's semantic element
                // slot. Reference arrays of value classes keep boxed elements, so the value-class
                // boundary belongs here alongside call parameters and fields. The receiver is often
                // a generated local read; recover its pre-erasure type from the function slot map.
                IrExpr::Call {
                    callee:
                        Callee::Intrinsic {
                            operation: crate::ir::IrIntrinsic::ArraySet,
                            ..
                        },
                    dispatch_receiver: Some(array),
                    args,
                } => {
                    let element =
                        array_element_type(&ir.exprs, repr_ctx.slots, &ir.logical_types, *array);
                    crate::trace_compiler!(
                        "value_classes",
                        "array set expr {id} receiver={array} {:?} element={element:?} args={args:?}",
                        &ir.exprs[*array as usize],
                    );
                    if let Some((value, element)) = args.get(1).copied().zip(element) {
                        record_reference_array_element_boundary(
                            &mut ops, &ir.exprs, &repr_ctx, value, element,
                        );
                    }
                    continue;
                }
                // Captures target the lifted implementation's leading parameters.
                IrExpr::Lambda {
                    impl_fn, captures, ..
                } => captures
                    .iter()
                    .zip(orig_params[*impl_fn as usize].iter())
                    .map(|(capture, parameter)| (*capture, *parameter))
                    .collect(),
                // A value-class instance-method call (`a.equals(b)`) boxes value-class arguments into
                // the method's (reference) parameters, same as a plain call.
                IrExpr::MethodCall {
                    class, index, args, ..
                } => ir.classes[*class as usize]
                    .methods
                    .get(*index as usize)
                    .map(|fid| {
                        let params = &orig_params[*fid as usize];
                        let current = &ir.functions[*fid as usize].params;
                        args.iter()
                            .enumerate()
                            .filter_map(|(i, a)| {
                                // A param that STAYED a value class post-erasure is a user vc-member's
                                // boxed `LX;` param — the dedicated arg-boxing block above handles an
                                // unboxed arg into it, and a boxed arg flows in unchanged. Exclude it from
                                // the generic boundary (whose `target()` would mis-`Unbox` a boxed arg).
                                if current
                                    .get(i)
                                    .and_then(|t| t.non_null().obj_internal())
                                    .is_some_and(|fq_name| under.contains_key(&fq_name))
                                {
                                    return None;
                                }
                                Some((a.as_ref().copied()?, *params.get(i)?))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                // A local initializer `val x: T = <vc>` — and equally a later ASSIGNMENT `x = <vc>` —
                // is a representation boundary: an unboxed value into a boxed (`Any`/`X?`-boxed/
                // generic) slot must `box-impl`. The slot's PRE-erasure declared type lives in
                // `slots` (the `Variable.ty` was erased in step 3).
                IrExpr::Variable {
                    index,
                    init: Some(v),
                    ..
                } => match slots.get(index) {
                    Some(t) => vec![(*v, *t)],
                    None => continue,
                },
                // A FIELD store is the same boundary, decided by the field's PRE-erasure declared
                // type: a suspend lambda's synthesized `invoke`/`create` stores its (boxed, cast from
                // the erased `Object` argument) value-class parameter into the param spill field the
                // erasure just retyped to the underlying — `Boxed → UnboxedX` unboxes it there.
                IrExpr::SetField {
                    class,
                    index,
                    value,
                    ..
                } => match orig_fields
                    .get(*class as usize)
                    .and_then(|fs| fs.get(*index as usize))
                {
                    Some(t) => vec![(*value, *t)],
                    None => continue,
                },
                IrExpr::SetValue { var, value } => match slots.get(var) {
                    Some(t) => vec![(*value, *t)],
                    None => continue,
                },
                // A shared-cell write/init (`Ref$ObjectRef.element = <vc>`): boundary ONLY when the
                // cell's pre-erasure element is the BOXED nullable `X?` form — a NON-null element is
                // the value's own unboxed underlying (even an `Object` underlying: the cell is the
                // vc's native slot, not a generic supertype slot), where boxing would corrupt reads.
                IrExpr::RefNew {
                    init: Some(init), ..
                } => match orig_ref_elems.get(&id) {
                    Some(t) if t.is_nullable() => vec![(*init, *t)],
                    _ => continue,
                },
                IrExpr::RefSet { value, .. } => match orig_ref_elems.get(&id) {
                    Some(t) if t.is_nullable() => vec![(*value, *t)],
                    _ => continue,
                },
                _ => continue,
            };
            for (a, p) in pairs {
                record_value_boundary(&mut ops, &ir.exprs, &repr_ctx, a, p, &under);
            }
        }
    }
    // A registered default expression is itself a parameter boundary. Walking its children above
    // handles calls and stores inside the expression, but the root must also be adapted to the
    // physical slot used by the `$default` stub. Null-capable carriers use a boxed stub slot even
    // though the real method receives the unboxed carrier.
    for function in 0..ir.functions.len() {
        let Some(defaults) = ir.param_defaults(function as u32) else {
            continue;
        };
        let repr_ctx = ReprCtx {
            exprs: &ir.exprs,
            funcs: &ir.functions,
            rets: &orig_rets,
            fields: &orig_fields,
            slots: &slot_types[function],
            under: &under,
            types: CallTypes::of(ir),
            physical: &ir.physical_types,
            field_getters: &field_getters,
            carrier_unboxes: &carrier_unboxes,
        };
        for (parameter, default) in defaults.iter().enumerate() {
            let Some(default) = *default else {
                continue;
            };
            let Some(mut target) = orig_params[function].get(parameter).copied() else {
                continue;
            };
            if let Some(boxed) = ir
                .default_stub_boxed_params
                .get(&(function as u32))
                .and_then(|boxed| {
                    boxed
                        .iter()
                        .find_map(|(index, ty)| (*index == parameter).then_some(*ty))
                })
            {
                target = Ty::nullable(boxed);
            }
            record_value_boundary(&mut ops, &ir.exprs, &repr_ctx, default, target, &under);
        }
    }
    // A superclass invocation is not an IR `Call` node: `super_args` are emitted directly by the class
    // constructor. Apply the same semantic boundary operation using the checker-selected parameter types
    // retained beside those arguments.
    for (class_index, class) in ir.classes.iter().enumerate() {
        for (&argument, &parameter) in class.super_args.iter().zip(&class.super_ctor_params) {
            let repr_ctx = ReprCtx {
                exprs: &ir.exprs,
                funcs: &ir.functions,
                rets: &orig_rets,
                fields: &orig_fields,
                slots: constructor_slots.super_args(class_index),
                under: &under,
                types: CallTypes::of(ir),
                physical: &ir.physical_types,
                field_getters: &field_getters,
                carrier_unboxes: &carrier_unboxes,
            };
            record_value_boundary(&mut ops, &ir.exprs, &repr_ctx, argument, parameter, &under);
        }
    }
    for (id, is_ne) in vacuous {
        ir.exprs[id as usize] = IrExpr::Const(crate::ir::IrConst::Boolean(is_ne));
    }
    for (id, operand) in strip {
        ir.exprs[id as usize] = IrExpr::Block {
            stmts: vec![],
            value: Some(operand),
        };
    }
    // A cast that was STRIPPED (its operand is already the underlying) is now a `Block` — the
    // retarget's `TypeOp` match simply skips it, so a node in both lists is harmless.
    for (id, underlying) in retarget {
        if let IrExpr::TypeOp { type_operand, .. } = &mut ir.exprs[id as usize] {
            *type_operand = underlying;
        }
    }
    // Fresh local slot for the null-safe box temp — above every index any function already uses.
    let mut fresh = ir
        .exprs
        .iter()
        .filter_map(|e| match e {
            IrExpr::Variable { index, .. }
            | IrExpr::GetValue(index)
            | IrExpr::SetValue { var: index, .. } => Some(*index),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        + 1;
    let mut unique_ops = HashSet::new();
    ops.retain(|operation| unique_ops.insert(*operation));
    substitution_coercions::drop_rebox_round_trips(&mut ops, ir);
    // Each `unbox-impl` realized over a suspend call whose CPS result is the value class's box, as
    // recorded for that exact call. A suspend function returning the same box hands that value
    // back as it is (see `restore_boxed_suspension_tails`).
    let mut boxed_suspension_unboxes = HashSet::new();
    for (id, op) in ops {
        crate::trace_compiler!(
            "value_classes",
            "apply representation op expr {id} {:?} op={}",
            &ir.exprs[id as usize],
            match op {
                BoxOp::Box(_) => "Box",
                BoxOp::BoxNull(_) => "BoxNull",
                BoxOp::Unbox(_) => "Unbox",
                BoxOp::UnboxNull(_) => "UnboxNull",
                BoxOp::Narrow(_) => "Narrow",
                BoxOp::StringOf(_) => "StringOf",
            }
        );
        // Box/unbox a value class at a boundary uniformly — a classpath value class (`kotlin/Result`) has
        // `box-impl`/`unbox-impl` on the classpath and is boxed in reference slots and unboxed at its
        // members like any user value class. (kotlinc observes the boxed form's `toString`/`equals`/
        // `hashCode` for a `Result` in an `Object` slot too.)
        match op {
            BoxOp::Box(x) => box_wrap(ir, id, x, &under),
            BoxOp::BoxNull(x) => {
                box_wrap_nullable(ir, id, x, &under, fresh);
                fresh += 1;
            }
            BoxOp::Unbox(x)
                if matches!(
                    ir.value_class_suspend_calls.get(&id).copied(),
                    Some(crate::ir::IrValueClassSuspendResult::Carrier { carrier, .. })
                        if carrier.canonical_semantic()
                            == erase(&under[&x], &under).canonical_semantic()
                ) =>
            {
                // The erased CPS method descriptor says `Object`, but the continuation carries the
                // already-unboxed representation recorded for this exact call. A boundary collected
                // from the pre-CPS descriptor must not insert a value-class `unbox-impl` around it.
            }
            BoxOp::Unbox(x) => {
                if matches!(
                    ir.value_class_suspend_calls.get(&id),
                    Some(crate::ir::IrValueClassSuspendResult::Boxed { classifier, .. })
                        if *classifier == x
                ) {
                    boxed_suspension_unboxes.insert(id);
                }
                unbox_wrap(ir, id, x, &under);
                carrier_unboxes.insert(id, x);
            }
            BoxOp::UnboxNull(x) => {
                unbox_wrap_nullable(ir, id, x, &under, fresh);
                fresh += 1;
            }
            BoxOp::Narrow(x) => narrow_wrap(ir, id, x),
            BoxOp::StringOf(x) => to_string_wrap(ir, id, x, &under),
        }
    }

    for (id, specialized) in equalities {
        equality::realize(ir, id, specialized, &under, &mut fresh);
    }

    // `super_ctor_params` preserves the checker-selected declaration shape long enough to drive
    // the argument boundary operations above. Emission consumes the same selected parameter list
    // as a physical JVM descriptor, so realize value-class carriers only after those semantic
    // boundary decisions are complete. Shared mutable-capture positions are rewritten to their
    // holder types by the following JVM pass from their separate exact coordinate map.
    for class in &mut ir.classes {
        for parameter in &mut class.super_ctor_params {
            *parameter = erase(parameter, &under);
        }
    }

    // Execute the storage realization decided above. A facade property marked erased takes the
    // carrier, and its setter takes the mangled name a value-class PARAMETER always earns (the
    // getter keeps its plain one: a value-class RESULT alone contributes no hash inside a file
    // class). Its initializer is already the unboxed `constructor-impl(…)` step 4 left, so nothing
    // boxes it back. Every other value-class-typed static keeps the boxed field, so its initializer
    // is boxed to match — `box_tail` only boxes an unboxed `constructor-impl`/`unbox-impl` tail, so
    // an already-boxed init is left untouched.
    for si in 0..ir.statics.len() {
        let Some(x) = ir.statics[si]
            .ty
            .non_null()
            .obj_internal()
            .filter(|fq| under.contains_key(fq))
        else {
            continue;
        };
        let erased = ir.statics[si].erased_declared_ty.is_some();
        let declared = ir.statics[si].ty;
        if let Some(root) = ir.statics[si].init.filter(|_| !erased) {
            box_tail(ir, root, x, &under);
        }
        let property = &mut ir.statics[si];
        if erased {
            property.ty = erase(&declared, &under);
        }
        // The SETTER's name is decided by its parameter, not by the storage: a value-class
        // parameter always contributes to the hash, whether the field behind it holds the carrier
        // or the box. `var nullableScalar: Crate?` keeps a boxed field AND takes
        // `setNullableScalar-<hash>(Crate)`, which tying the two decisions together left unmangled.
        // (The getter keeps its plain name: a value-class RESULT alone contributes no hash inside a
        // file class.)
        if property.is_var && property.is_facade_owned() && !property.accessors.any_declared() {
            property.setter_jvm_name = Some(vc_mangle(
                &crate::names::property_setter_name(&property.name),
                std::slice::from_ref(&declared),
                &Ty::Unit,
                &under,
                false,
                false,
            ));
        }
    }

    let lambda_implementation_ids: HashSet<u32> = ir
        .exprs
        .iter()
        .filter_map(|expression| match expression {
            IrExpr::Lambda { impl_fn, .. } => Some(*impl_fn),
            _ => None,
        })
        .collect();

    // 6. A function returning a nullable value class `X?` boxes its non-null (unboxed) results; a
    //    function declared to return a reference SUPERTYPE (`Any`/`Any?`/an interface — NOT the value
    //    class itself) boxes a value-class tail too (`fun f(): Any? = vc`).
    for fid in 0..ir.functions.len() {
        // FunctionN/SAM result representation is one boundary handled by step 7 below. Running this
        // ordinary declaration-return rewrite first would unbox a boxed lambda tail and then make the
        // lambda pass box it again.
        if lambda_implementation_ids.contains(&(fid as u32)) {
            continue;
        }
        if vc_methods.contains(&(fid as u32)) && !lowered_value_members.contains(&(fid as u32)) {
            // A synthesized wrapper member such as `box-impl` keeps the boxed value-class result.
            // User declarations are absent from this branch: they were converted to static carrier
            // functions above and take the ordinary return-boundary path below.
            if let Ty::Obj(fq, _) = &orig_rets[fid] {
                let x = *fq;
                if under.contains_key(&x) {
                    if let Some(body) = ir.functions[fid].body {
                        box_tail(ir, body, x, &under);
                    }
                }
            }
            continue;
        }
        if let Some(x) = boxed_vc(&orig_rets[fid], &under) {
            if let Some(body) = ir.functions[fid].body {
                // A nullable value-class return `X?` has the BOXED descriptor `LX;`, so a tail that is an
                // UNBOXED `X` value must be boxed — not only the syntactic `constructor-impl`/`unbox-impl`
                // forms `box_tail` handled, but also a value-class field read (`holder.item`) or a
                // call returning the unboxed underlying (`make(): X`) flowing in via nullable widening.
                // `box_nullable_vc_tail` boxes exactly the tails whose representation IS an unboxed `X`
                // (leaving `null`, already-boxed, and unrelated values — e.g. a suspend continuation's
                // `kotlin/Result` resume value that shares the boxed descriptor — untouched).
                box_nullable_vc_tail(
                    ir,
                    body,
                    x,
                    ReprInputs {
                        rets: &orig_rets,
                        fields: &orig_fields,
                        slots: &slot_types[fid],
                        under: &under,
                        field_getters: &field_getters,
                        carrier_unboxes: &carrier_unboxes,
                    },
                    true,
                );
            }
        } else if orig_rets[fid]
            .non_null()
            .obj_internal()
            .is_some_and(|fq_name| {
                fq_name.matches("kotlin/Any") || vc_interfaces.contains(&fq_name)
            })
        {
            // A function declared to return `Any` or an interface a value class implements (NOT the
            // value class itself) boxes a value-class tail so the erased call hands back a box (`is X`/
            // interface dispatch works). Concrete-type returns (e.g. `String`) are left alone.
            if let Some(body) = ir.functions[fid].body {
                box_vc_tail(ir, body, &under, &orig_rets, false);
            }
        } else if let Ty::Obj(x, _) = orig_rets[fid].non_null() {
            // A function returning the value class ITSELF (`fun test(): Z = a?.foo()!!`), or an `X?`
            // its carrier holds (a boxed `X?` took the branch above), `unbox-impl`s a BOXED tail: the
            // erased return is the underlying. A nullable tail is unboxed null-safely (`null_slot`).
            let null_slot = orig_rets[fid].is_nullable().then_some(fresh);
            fresh += u32::from(null_slot.is_some());
            if under.contains_key(&x) && suspend_fids.contains(&(fid as u32)) {
                // …EXCEPT a `suspend fun`: its CPS return is `Object`. The declaration-level suspension
                // representation decides whether the carrier crosses directly or is wrapped in the value
                // class. Besides scalar carriers, a null-capable carrier must be wrapped too: otherwise
                // the raw `null` for `X(null)` is indistinguishable from a null result at the caller.
                if let Some(body) = ir.functions[fid].body {
                    match suspend_result_representation(
                        &orig_rets[fid],
                        &under,
                        force_boxed_suspend_returns.contains(&(fid as u32)),
                    ) {
                        Some(crate::ir::IrValueClassSuspendResult::Boxed { .. }) => {
                            ir.functions[fid].ret = boxed_value_ty(x);
                            restore_boxed_suspension_tails(ir, body, &boxed_suspension_unboxes);
                            box_ref_tail(
                                ir,
                                body,
                                x,
                                ReprInputs {
                                    rets: &orig_rets,
                                    fields: &orig_fields,
                                    slots: &slot_types[fid],
                                    under: &under,
                                    field_getters: &field_getters,
                                    carrier_unboxes: &carrier_unboxes,
                                },
                            );
                        }
                        Some(crate::ir::IrValueClassSuspendResult::Carrier { carrier, .. }) => {
                            ir.functions[fid].ret = carrier;
                            // A safe coroutine primitive produces `T` through the generic
                            // `SafeContinuation<T>` slot, so a value-class `T` is boxed even when this
                            // suspend declaration's selected CPS boundary is its raw reference carrier.
                            // Convert that exact boxed tail once; ordinary carrier-producing tails are
                            // already unboxed and remain unchanged.
                            return_unboxing::unbox_tail(
                                ir,
                                body,
                                x,
                                ReprInputs {
                                    rets: &orig_rets,
                                    fields: &orig_fields,
                                    slots: &slot_types[fid],
                                    under: &under,
                                    field_getters: &field_getters,
                                    carrier_unboxes: &carrier_unboxes,
                                },
                                null_slot,
                            );
                        }
                        None => unreachable!("the return was already identified as a value class"),
                    }
                }
            } else if under.contains_key(&x) {
                if let Some(body) = ir.functions[fid].body {
                    return_unboxing::unbox_tail(
                        ir,
                        body,
                        x,
                        ReprInputs {
                            rets: &orig_rets,
                            fields: &orig_fields,
                            slots: &slot_types[fid],
                            under: &under,
                            field_getters: &field_getters,
                            carrier_unboxes: &carrier_unboxes,
                        },
                        null_slot,
                    );
                }
            }
        }
    }

    // 7. A lambda used as `() -> T` (a `FunctionN`) erases its result to `Object`, so a value-class
    //    result must be boxed at the lambda body's tail (`call { X(..) }` hands back a boxed `X`).
    let mut lambda_impls: Vec<(u32, ExprId)> = Vec::new();
    let mut inline_bodies: Vec<ExprId> = Vec::new();
    for e in &ir.exprs {
        if let IrExpr::Lambda {
            impl_fn,
            inline_body,
            ..
        } = e
        {
            // A lambda SAM-converted to a method that DECLARES this very value class as its return is
            // exempt from ALL of the tail boxing below — see the note on the loop. Its inline body
            // shares the same tail node, so it must be exempt from `box_vc_tail` too.
            if sam_declares_vc_return(ir, &orig_rets, *impl_fn, &callable_under) {
                continue;
            }
            if let Some(body) = ir.functions.get(*impl_fn as usize).and_then(|f| f.body) {
                lambda_impls.push((*impl_fn, body));
            }
            if let Some(b) = inline_body {
                inline_bodies.push(*b);
            }
        }
    }
    for (impl_fn, body) in lambda_impls {
        // A lambda's `invoke` returns `Object` (the SAM erases its result), so a value-class result occupies
        // a REFERENCE slot and must be the BOXED value class. When the lambda's declared return is a value
        // class `X`, box the tail to `X` uniformly — for EVERY value class (a classpath `kotlin/Result` is a
        // value class like any other) and EVERY tail form (`this`, a library call, a constructor) — unless
        // it is already a boxed `X`. The impl method's JVM return becomes the box type `X`.
        // A lambda whose SAM method DECLARES this value class as its return was skipped when the list
        // was collected: that return erases to the underlying (kotlinc's
        // `onResult-d1pmJ48()Ljava/lang/Object;` hands back the carrier), so the already-erased return
        // is the right one and the tail needs no boxing at all.
        if let Some(x) = orig_rets[impl_fn as usize]
            .non_null()
            .obj_internal()
            .filter(|fq| callable_under.contains_key(fq))
        {
            ir.functions[impl_fn as usize].ret = boxed_value_ty(x);
            box_ref_tail(
                ir,
                body,
                x,
                ReprInputs {
                    rets: &orig_rets,
                    fields: &orig_fields,
                    slots: &slot_types[impl_fn as usize],
                    under: &callable_under,
                    field_getters: &field_getters,
                    carrier_unboxes: &carrier_unboxes,
                },
            );
        } else {
            // A lambda returning `Any`/an interface (not a value class itself) still boxes a value-class tail.
            box_vc_tail(ir, body, &callable_under, &orig_rets, false);
        }
    }
    for body in inline_bodies {
        box_vc_tail(ir, body, &callable_under, &orig_rets, false);
    }

    // Synthesized accessors use the same callable universe as property operations. In particular,
    // built-in value classes such as UInt are callable value classes even though they are not
    // declarations owned by this source module.
    accessor_names::stamp_synthesized(ir, &callable_under);

    let realized =
        property_references::realize(ir, &callable_under, property_reference_realizations);
    type_operation_roles::record_primitive_array_type_operations(ir, &under);
    realized
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum BoxOp {
    Box(TypeName),
    BoxNull(TypeName),
    Unbox(TypeName),
    UnboxNull(TypeName),
    Narrow(TypeName),
    /// Render a non-null unboxed value as text through its class's static `toString-impl`.
    StringOf(TypeName),
}

impl BoxOp {
    /// Unbox a `value_class` box, null-safely where the value may be null.
    fn unbox(value_class: TypeName, nullable: bool) -> Self {
        if nullable {
            Self::UnboxNull(value_class)
        } else {
            Self::Unbox(value_class)
        }
    }
}

/// The representation a value-class value currently has.
#[derive(Clone, Copy)]
enum Repr {
    NotVc,
    Unboxed(TypeName),
    Boxed(TypeName),
}

/// What a target position wants of a value-class value.
#[derive(Clone, Copy)]
enum Target {
    UnboxedX(TypeName), // a non-null `X` position → wants the unboxed `U`
    Boxed,              // `Object`/generic/nullable-`X` → wants a boxed `X` object
    Other,
}

/// Each expression this pass rewrote into a value class's `unbox-impl` at a representation boundary
/// (an applied [`BoxOp::Unbox`]), with that class. Its result is the carrier, even when the
/// carrier's physical type is the same erased `Object` that a generic box occupies.
type CarrierUnboxes = HashMap<ExprId, TypeName>;

/// The per-body representation inputs that do not borrow the IR being rewritten. A step that
/// mutates the IR between queries asks [`ReprInputs::over`] for a fresh [`ReprCtx`] each time.
#[derive(Clone, Copy)]
struct ReprInputs<'a> {
    rets: &'a [Ty],
    fields: &'a [Vec<Ty>],
    slots: &'a HashMap<u32, Ty>,
    under: &'a Under,
    field_getters: &'a FieldGetters,
    carrier_unboxes: &'a CarrierUnboxes,
}

impl<'a> ReprInputs<'a> {
    fn over<'b>(self, ir: &'b IrFile) -> ReprCtx<'b>
    where
        'a: 'b,
    {
        ReprCtx {
            exprs: &ir.exprs,
            funcs: &ir.functions,
            rets: self.rets,
            fields: self.fields,
            slots: self.slots,
            under: self.under,
            types: CallTypes::of(ir),
            physical: &ir.physical_types,
            field_getters: self.field_getters,
            carrier_unboxes: self.carrier_unboxes,
        }
    }
}

struct ReprCtx<'a> {
    exprs: &'a [IrExpr],
    funcs: &'a [crate::ir::IrFunction],
    rets: &'a [Ty],
    fields: &'a [Vec<Ty>],
    slots: &'a HashMap<u32, Ty>,
    under: &'a Under,
    types: CallTypes<'a>,
    physical: &'a HashMap<u32, Ty>,
    field_getters: &'a FieldGetters,
    carrier_unboxes: &'a CarrierUnboxes,
}

impl ReprCtx<'_> {
    fn repr(&self, id: ExprId) -> Repr {
        repr(self, id)
    }

    /// A selected call is non-null when its checked declaration returns a non-null type.
    fn operand_nonnull(&self, id: ExprId) -> bool {
        operand_nonnull(self.exprs, self.rets, self.fields, self.slots, id)
            || matches!(
                self.types.declared_result(id, self.under),
                Some(Ty::Obj(..))
            )
    }

    /// Non-null value-class identity whose checked value is carried unboxed. Generated local reads
    /// can lose the declaration-oriented `repr` route after earlier IR normalization, while their
    /// exact semantic type remains attached to the expression. This consumes that existing fact; it
    /// never infers a class from a JVM carrier type.
    fn unboxed_value_class(&self, id: ExprId, known: &Under) -> Option<TypeName> {
        if let Repr::Unboxed(classifier) = self.repr(id) {
            return Some(classifier);
        }
        if self
            .physical
            .get(&id)
            .and_then(|ty| ty.non_null().obj_internal())
            .is_some_and(|classifier| known.contains_key(&classifier))
        {
            return None;
        }
        let semantic = self.types.get(&id)?;
        let classifier = semantic.non_null().obj_internal()?;
        (known.contains_key(&classifier)
            && (!semantic.is_nullable() || !nullable_is_boxed(classifier, known)))
        .then_some(classifier)
    }

    fn operand_null_only(&self, id: ExprId) -> bool {
        operand_null_only(self.exprs, self.rets, self.slots, id)
    }

    fn box_op(&self, id: ExprId, value_class: TypeName) -> BoxOp {
        if self.operand_nonnull(id) {
            BoxOp::Box(value_class)
        } else {
            BoxOp::BoxNull(value_class)
        }
    }

    /// A coercion to an erased type parameter changes the consumer's contract, not the value's current
    /// representation. Descriptor boundaries must inspect the value beneath such coercions so an
    /// unboxed value class is boxed at the reference slot, while its preceding specialized local keeps
    /// the unboxed carrier. Return the expression that should receive the representation rewrite.
    fn through_erased_generic_coercion(&self, mut id: ExprId) -> (ExprId, Repr) {
        while let IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg,
            type_operand,
        } = &self.exprs[id as usize]
        {
            if !matches!(type_operand.non_null(), Ty::TyParam(..)) {
                break;
            }
            id = *arg;
        }
        (id, self.repr(id))
    }
}

fn record_value_boundary(
    ops: &mut Vec<(ExprId, BoxOp)>,
    exprs: &[IrExpr],
    repr_ctx: &ReprCtx<'_>,
    value: ExprId,
    parameter: Ty,
    under: &Under,
) {
    let target = target(&parameter, under);
    let representation = repr_ctx.repr(value);
    crate::trace_compiler!(
        "value_classes",
        "boundary expr {value} {:?} -> param {parameter:?} repr={} target={}",
        &exprs[value as usize],
        match representation {
            Repr::Unboxed(_) => "Unboxed",
            Repr::Boxed(_) => "Boxed",
            Repr::NotVc => "NotVc",
        },
        match target {
            Target::UnboxedX(_) => "UnboxedX",
            Target::Boxed => "Boxed",
            Target::Other => "Other",
        }
    );
    let supertype_box = matches!(target, Target::Boxed)
        || (matches!(target, Target::Other)
            && is_ref(&parameter)
            && match representation {
                Repr::Unboxed(value_class) | Repr::Boxed(value_class) => {
                    let underlying = under
                        .get(&value_class)
                        .map(|underlying| erase(underlying, under).non_null());
                    let own_underlying = underlying.as_ref() == Some(&parameter.non_null())
                        && underlying
                            .as_ref()
                            .and_then(|ty| ty.obj_internal())
                            .is_none_or(|name| !name.matches("java/lang/Object"));
                    parameter.non_null().obj_internal() != Some(value_class) && !own_underlying
                }
                Repr::NotVc => false,
            });
    match representation {
        Repr::Unboxed(value_class) if supertype_box => {
            let mut tails = Vec::new();
            value_tails(exprs, value, &mut tails);
            for tail in tails {
                if matches!(repr_ctx.repr(tail), Repr::Unboxed(tail_class) if tail_class == value_class)
                {
                    ops.push((tail, repr_ctx.box_op(tail, value_class)));
                }
            }
        }
        // A boxed branch result may still contain an unboxed value-class tail. Box that tail so
        // every path entering the merge has the same representation.
        Repr::Boxed(value_class) if supertype_box => {
            let mut tails = Vec::new();
            value_tails(exprs, value, &mut tails);
            for tail in tails {
                if matches!(repr_ctx.repr(tail), Repr::Unboxed(tail_class) if tail_class == value_class)
                {
                    ops.push((tail, repr_ctx.box_op(tail, value_class)));
                }
            }
        }
        Repr::Boxed(value_class) if matches!(target, Target::UnboxedX(target_class) if target_class == value_class) =>
        {
            let mut tails = Vec::new();
            value_tails(exprs, value, &mut tails);
            for tail in tails {
                if matches!(repr_ctx.repr(tail), Repr::Boxed(tail_class) if tail_class == value_class)
                {
                    ops.push((tail, BoxOp::unbox(value_class, parameter.is_nullable())));
                }
            }
        }
        Repr::NotVc => {
            if let Target::UnboxedX(value_class) = target {
                if matches!(
                    &exprs[value as usize],
                    IrExpr::Call {
                        callee: Callee::Intrinsic { .. },
                        ..
                    }
                ) {
                    ops.push((value, BoxOp::unbox(value_class, parameter.is_nullable())));
                }
            }
        }
        _ => {}
    }
}

/// A Kotlin `Array<T>` is a JVM reference array even when `T` is a non-null value class. Its semantic
/// element type stays `T`; only this platform boundary requires the boxed `T` object for `aastore`.
fn record_reference_array_element_boundary(
    ops: &mut Vec<(ExprId, BoxOp)>,
    exprs: &[IrExpr],
    repr_ctx: &ReprCtx<'_>,
    value: ExprId,
    element: Ty,
) {
    let Some(value_class) = element
        .non_null()
        .obj_internal()
        .filter(|name| repr_ctx.under.contains_key(name))
    else {
        return;
    };
    if !matches!(repr_ctx.repr(value), Repr::Unboxed(actual) if actual == value_class) {
        return;
    }
    let mut tails = Vec::new();
    value_tails(exprs, value, &mut tails);
    for tail in tails {
        if matches!(repr_ctx.repr(tail), Repr::Unboxed(actual) if actual == value_class) {
            ops.push((tail, repr_ctx.box_op(tail, value_class)));
        }
    }
}

/// Whether a NULLABLE value class `X?` is represented BOXED. Only true when its underlying erases to a
/// primitive (a primitive can't carry null, so `X?` keeps the boxed `X`). Over a reference underlying,
/// `X?` erases to that underlying reference — represented unboxed, exactly like a non-null `X`.
fn nullable_is_boxed(x: TypeName, under: &Under) -> bool {
    // `X?` stays UNBOXED (its underlying reference carries null) only when the underlying is a NON-NULL
    // reference. Over a primitive (can't hold null) OR a NULLABLE reference (where `X(null)` and a `null`
    // `X?` would otherwise be indistinguishable), `X?` is the boxed `X`.
    under
        .get(&x)
        .map(|u| {
            !is_ref(&erase(u, under))
                || crate::value_classes::nullable_value_requires_distinct_null(x, under)
        })
        .unwrap_or(false)
}

/// Whether a NON-NULL value-class type's unboxed underlying can hold null (so a `checkNotNullParameter`
/// on it would wrongly reject a legal value). True when the value class's field type erases to a
/// nullable reference (`X(val v: Int?)` → `Integer`; `X(val v: String?)` → `String?`).
fn vc_underlying_nullable(t: &Ty, under: &Under) -> bool {
    if let Ty::Obj(fq_name, _) = t {
        if let Some(u) = under.get(fq_name) {
            return crate::value_classes::underlying_chain_accepts_null(*u, under);
        }
    }
    false
}

/// Semantic element type of an array-valued expression before value-class erasure. Generated array
/// fill loops normally pass the array through a local slot, while ordinary expressions may retain a
/// logical type directly. Follow only representation-transparent wrappers; this never resolves a type.
fn array_element_type(
    exprs: &[IrExpr],
    slots: &HashMap<u32, Ty>,
    logical: &HashMap<u32, Ty>,
    id: ExprId,
) -> Option<Ty> {
    if let Some(element) = logical.get(&id).and_then(|ty| ty.array_elem()) {
        return Some(element);
    }
    match &exprs[id as usize] {
        IrExpr::GetValue(slot) => slots.get(slot).and_then(|ty| ty.array_elem()),
        IrExpr::TypeOp { arg, .. } => array_element_type(exprs, slots, logical, *arg),
        IrExpr::Block {
            value: Some(value), ..
        } => array_element_type(exprs, slots, logical, *value),
        IrExpr::NewArray { array_type, .. } | IrExpr::Vararg { array_type, .. } => {
            array_type.array_elem()
        }
        _ => None,
    }
}

fn repr_of_ty(t: &Ty, under: &Under) -> Repr {
    if let Some(fq_name) = t.non_null().obj_internal() {
        let nullable = t.is_nullable();
        if under.contains_key(&fq_name) {
            return if nullable && nullable_is_boxed(fq_name, under) {
                Repr::Boxed(fq_name)
            } else {
                Repr::Unboxed(fq_name)
            };
        }
    }
    Repr::NotVc
}

/// The actual value passed to the already-selected `equals(Any?)` declaration. Common IR keeps the
/// checked widening to `Any?`; JVM value-class realization needs the value underneath when two
/// unboxed instances can compare their carriers directly.
fn value_class_equals_argument(exprs: &[IrExpr], argument: ExprId) -> ExprId {
    match &exprs[argument as usize] {
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg,
            type_operand,
        } if type_operand
            .non_null()
            .obj_internal()
            .is_some_and(|classifier| classifier.matches("kotlin/Any")) =>
        {
            *arg
        }
        _ => argument,
    }
}

fn target(t: &Ty, under: &Under) -> Target {
    if let Some(fq_name) = t.non_null().obj_internal() {
        let nullable = t.is_nullable();
        if under.contains_key(&fq_name) {
            return if nullable && nullable_is_boxed(fq_name, under) {
                Target::Boxed
            } else {
                Target::UnboxedX(fq_name)
            };
        }
        if fq_name.matches("kotlin/Any") {
            return Target::Boxed;
        }
    }
    Target::Other
}

/// The representation of the value the expr at `id` produces (after the construction/property rewrite).
fn repr(context: &ReprCtx<'_>, id: ExprId) -> Repr {
    let ReprCtx {
        exprs,
        rets,
        fields,
        slots,
        under,
        types,
        physical,
        field_getters,
        ..
    } = *context;
    // A backend pass may have selected the value-class box as this exact expression's physical type
    // (notably at a suspend `Object` boundary). That representation fact is later than the declaration's
    // semantic return type and therefore wins before structural call analysis.
    if let Some(classifier) = physical
        .get(&id)
        .and_then(|ty| (*ty).obj_internal())
        .filter(|classifier| under.contains_key(classifier))
    {
        // Argument normalization can wrap a selected call in a block while preserving the declaration's
        // pre-realization result on that block. If the value-producing child already proves whether the
        // selected declaration returns a carrier or a box, that structural fact wins; the block's sparse
        // type stamp is only the fallback for an erased generic result whose child cannot identify `X`.
        if let IrExpr::Block {
            value: Some(value), ..
        } = &exprs[id as usize]
        {
            let structural = repr(context, *value);
            if !matches!(structural, Repr::NotVc) {
                return structural;
            }
        }
        return Repr::Boxed(classifier);
    }
    match &exprs[id as usize] {
        // A static's storage says which representation its read has, and the declaration records
        // that decision rather than this re-deriving it: a FACADE property of value-class type is
        // realized over the carrier (`getstatic gz:I`), while every other value-class-typed static
        // — a companion's, notably — keeps the box its declaration retains and the emitter reads
        // through that boxed descriptor.
        IrExpr::GetStatic(i) => match types.erased_static_value_class(*i) {
            Some(classifier) => Repr::Unboxed(classifier),
            None => types
                .get(&id)
                .and_then(|ty| ty.non_null().obj_internal())
                .filter(|classifier| under.contains_key(classifier))
                .map_or(Repr::NotVc, Repr::Boxed),
        },
        // A field read whose declared (pre-erasure) type is a value class is the unboxed underlying
        // (a data class stores a value-class property as its erased `U`). Boxing at any reference
        // boundary (the data-class `toString`/`hashCode`/`equals` synth → `StringBuilder.append`,
        // `Objects.hashCode`, `areEqual`) then routes through the value class's own member.
        IrExpr::GetField { class, index, .. } => fields
            .get(*class as usize)
            .and_then(|fs| fs.get(*index as usize))
            .map_or(Repr::NotVc, |t| repr_of_ty(t, under)),
        // A property read carries its own declared type. Whatever accessor or field the target picks for
        // it yields the value class's ERASED underlying — the same representation a field read of one has.
        IrExpr::PropertyRead { ty, operation, .. } => {
            let recorded = operation.unwrap_or(id);
            if let Some(declared) = types.unboxed_declared_property(recorded, under) {
                repr_of_ty(&declared, under)
            } else if physical.get(&id).is_some_and(|ty| ty.is_erased_top()) {
                ty.non_null()
                    .obj_internal()
                    .filter(|owner| under.contains_key(owner))
                    .map_or(Repr::NotVc, Repr::Boxed)
            } else {
                repr_of_ty(ty, under)
            }
        }
        // A value-class-FIELD getter (`Test.getS()` for `val s: S<T>`) reprs as the field's representation
        // — the UNBOXED underlying. Keyed on the getter's IDENTITY (owning class + method slot, via
        // `field_getters`), so it is distinguished from a boxing OVERRIDE getter, which is not in the map and
        // keeps its own erased repr. The read is a resolved `MethodCall`, not a `Call { Virtual }`.
        IrExpr::MethodCall { class, index, .. }
            if field_getters.contains_key(&(*class, *index)) =>
        {
            repr_of_ty(&field_getters[&(*class, *index)], under)
        }
        IrExpr::Call {
            callee: Callee::Static { owner, name, .. },
            ..
        } if name == "constructor-impl" || name == "unbox-impl" => {
            value_class_name(*owner, under).map_or(Repr::NotVc, Repr::Unboxed)
        }
        IrExpr::Call {
            callee: Callee::Static { owner, name, .. } | Callee::Virtual { owner, name, .. },
            ..
        } if name == "box-impl" => value_class_name(*owner, under).map_or(Repr::NotVc, Repr::Boxed),
        IrExpr::Call {
            callee: Callee::Virtual { owner, name, .. },
            ..
        } if name == "unbox-impl" => {
            value_class_name(*owner, under).map_or(Repr::NotVc, Repr::Unboxed)
        }
        // Any already-selected call whose declaration returns a value class yields that declaration's
        // physical representation. This includes index-resolved `MethodCall`s (interface/class members),
        // not only `Call::source_function`; omitting them made `source.get()!!` look non-value-class and
        // preserved a `checkcast X` around the raw carrier. `declared_value_class` deliberately excludes
        // a generic `T` merely instantiated with `X`, whose erased boundary yields a BOX instead.
        IrExpr::Call { .. } | IrExpr::MethodCall { .. }
            if types.declared_value_class(id, under).is_some() =>
        {
            types
                .declared_result(id, under)
                .map_or(Repr::NotVc, |declared| repr_of_ty(&declared, under))
        }
        IrExpr::Call { callee, .. } if callee.source_function().is_some() => rets
            .get(
                callee
                    .source_function()
                    .expect("guarded same-file function call") as usize,
            )
            .map_or(Repr::NotVc, |t| repr_of_ty(t, under)),
        IrExpr::GetValue(i) => slots.get(i).map_or(Repr::NotVc, |t| repr_of_ty(t, under)),
        // `e as X` yields a boxed `X` object (checkcast of an `Any`/supertype value) — EXCEPT a redundant
        // cast over an already-unboxed `X` (a generic-erasure cast `(X)a` the front end inserts when the
        // static type flows through a type parameter, e.g. reading a `Ag2<T>` field): that stays UNBOXED,
        // so a following member call boxes it (`box-impl`) like any other unboxed receiver.
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::Cast | crate::ir::IrTypeOp::CastNonNull,
            type_operand,
            arg,
        } if type_operand
            .non_null()
            .obj_internal()
            .is_some_and(|fq| under.contains_key(&fq)) =>
        {
            let fq_name = type_operand.non_null().obj_internal().unwrap();
            match repr(context, *arg) {
                Repr::Unboxed(x) if x == fq_name => Repr::Unboxed(x),
                _ if physical.get(arg).is_some_and(|physical| {
                    physical.is_reference() && physical.non_null().obj_internal() != Some(fq_name)
                }) =>
                {
                    Repr::Boxed(fq_name)
                }
                _ => Repr::Boxed(fq_name),
            }
        }
        // A sole-field access coerces to the underlying type — its representation is that type's, NOT
        // the value class's (so `vc.field` reads as the underlying, e.g. an `Int`, not a `Meters`).
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            type_operand,
            ..
        } => repr_of_ty(type_operand, under),
        IrExpr::NotNullAssert { operand, .. } => repr(context, *operand),
        // Reading a captured mutable local through its `Ref` holder: its representation is that of the
        // boxed element type (`var res: Result<T>?` → a boxed `Result`).
        IrExpr::RefGet { elem, .. } => repr_of_ty(elem, under),
        // An array allocation produces the array type's own representation. A primitive-array
        // value class in the rewrite map is that carrier; a missing declaration stays `NotVc`.
        IrExpr::NewArray { array_type, .. } | IrExpr::Vararg { array_type, .. } => {
            repr_of_ty(array_type, under)
        }
        IrExpr::Block { value: Some(v), .. } => repr(context, *v),
        // A `when`/safe-call or a `try` selects one of its branch values (`s?.foo()` → `when {
        // s!=null -> foo(s); else -> null }`): its representation is the FIRST value-class branch's,
        // so a boxed result out of a `?.` is recognized and a diverging `try` body is skipped.
        IrExpr::When { .. } | IrExpr::Try { .. } => crate::ir::selected_values(&exprs[id as usize])
            .map(|v| repr(context, v))
            .find(|r| !matches!(r, Repr::NotVc))
            .unwrap_or(Repr::NotVc),
        // A function value's `invoke` returns its declared type through the `FunctionN` `Object` slot — a
        // value-class result is therefore the BOXED value class (the callable-ref adapter / lambda tail box
        // it). So a `.member` on the result unboxes it.
        IrExpr::InvokeFunction { ret, .. } => match ret.non_null().obj_internal() {
            Some(fq) if under.contains_key(&fq) => Repr::Boxed(fq),
            _ => Repr::NotVc,
        },
        // Calls whose earlier, identity-specific arms did not classify are handled by the one
        // backend-owned result-boundary operation. It consumes selected semantic and physical facts;
        // it never resolves a callable or dispatches from a name.
        IrExpr::Call { callee, .. } => {
            call_results::representation(id, callee, under, types, physical)
        }
        // A value-class GETTER / member read (statically `S<T>` though its erased form is `Object`) whose
        // SUBSTITUTED static type the lowerer recorded: repr it by that logical type, so a redundant `Cast`
        // wrapping an already-unboxed value class strips. Scoped to `MethodCall` — a getter — so it does
        // not reinterpret other erased nodes.
        _ => Repr::NotVc,
    }
}

/// Build a sole-property access `x.v`: identity (`Block` yielding the receiver) when the receiver is an
/// unboxed value, or `receiver.unbox-impl()` when it is a boxed `X` (e.g. from a nullable-returning
/// function).
fn prop_access(
    ir: &mut IrFile,
    receiver: ExprId,
    x: TypeName,
    result: Ty,
    inputs: ReprInputs<'_>,
) -> IrExpr {
    let under = inputs.under;
    let u = under.get(&x).map(|t| erase(t, under)).unwrap_or(Ty::Error);
    // Use the same representation analysis as every other boundary. The resulting coercion tells later
    // analysis that the property itself has the underlying representation.
    let inferred_boxed = inputs.over(ir).is_boxed_vc(receiver, x);
    crate::trace_compiler!(
        "value_classes",
        "prop access {} receiver={receiver} {:?} result={result:?} underlying={u:?} inferred_box={inferred_boxed}",
        x.render(),
        &ir.exprs[receiver as usize],
    );
    if let IrExpr::TypeOp { arg, .. } = &ir.exprs[receiver as usize] {
        crate::trace_compiler!(
            "value_classes",
            "prop access {} receiver arg={arg} {:?} logical={:?} physical={:?}",
            x.render(),
            &ir.exprs[*arg as usize],
            ir.logical_types.get(arg),
            ir.physical_types.get(arg),
        );
    }
    let inner = if inferred_boxed {
        let dispatch = if ir
            .physical_types
            .get(&receiver)
            .is_some_and(|ty| ty.is_erased_top())
        {
            ir.add_expr(IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::Cast,
                arg: receiver,
                type_operand: boxed_value_ty(x),
            })
        } else {
            receiver
        };
        unbox_call(ir, dispatch, x, &u)
    } else {
        receiver
    };
    // A generic value class keeps an erased `Object` carrier, but an applied property read
    // (`X<Int>.x`) has a concrete Kotlin result. Preserve that selected result so a real conversion
    // performs the required `Integer` unbox / reference cast instead of degrading it back to Any.
    let target = if result
        .non_null()
        .obj_internal()
        .is_some_and(|result_class| under.contains_key(&result_class))
        && u.is_erased_top()
    {
        // A value class recovered from a generic `Object` underlying is a BOX in that slot. Keep
        // the box type here; coercing straight to its carrier would cast `ICStr` to `String` before
        // the following property/member access gets a chance to call `unbox-impl`.
        result
    } else if result.is_ty_param() {
        // The declaration's generic underlying is erased (`Wrapper<T>.value: Object`), but an
        // applied read keeps the selected bound. A scalar-bounded `T` therefore needs the bound's
        // carrier (`T : Int` -> `int`), while an ordinary reference-bounded `T` keeps the erased
        // underlying. Treating every type parameter as `u` loses the only fact that can unbox the
        // result before arithmetic.
        result.scalar_value_repr().unwrap_or(u)
    } else {
        erase(&result, under)
    };
    IrExpr::TypeOp {
        op: crate::ir::IrTypeOp::ImplicitCoercion,
        arg: inner,
        type_operand: target,
    }
}

/// Whether a JVM method descriptor's return type is the classfile form of `class`.
/// `()Lpkg/Foo;` and `[Lpkg/Foo;` both end in that spelling; the comparison uses the interned
/// identity instead of rendering it.
fn descriptor_returns_class(descriptor: &str, class: TypeName) -> bool {
    let Some(internal) = descriptor.strip_suffix(';') else {
        return false;
    };
    let Some(at) = internal.rfind('L') else {
        return false;
    };
    class.matches(&internal[at + 1..])
}

impl ReprCtx<'_> {
    /// Whether the expr at `id` produces a BOXED value-class `x` object: a `box-impl` result, a call whose
    /// return type is `X` (a nullable-over-primitive value class stays boxed), or a `!!`/identity over one.
    fn is_boxed_vc(&self, id: ExprId, x: TypeName) -> bool {
        let ReprCtx {
            exprs,
            funcs,
            slots,
            fields,
            under,
            types,
            physical,
            ..
        } = *self;
        let is_x = |t: &Ty| t.non_null().obj_internal().is_some_and(|n| n == x);
        if physical.get(&id).is_some_and(is_x) {
            return true;
        }
        // An erased-top physical slot holds the box only for a generic result. A carrier that is itself
        // `Object` (`value class Box(val item: Any)`) records the same physical type once this pass has
        // unboxed that result, so the recorded unbox of `x` decides.
        if types.get(&id).is_some_and(is_x)
            && physical.get(&id).is_some_and(|ty| ty.is_erased_top())
            && types.declared_value_class(id, under) != Some(x)
            && self.carrier_unboxes.get(&id) != Some(&x)
        {
            return true;
        }
        match &exprs[id as usize] {
            // A local/param slot whose declared type is a BOXED value class `x` (a nullable `X?`, e.g. the
            // `?.` receiver temp) holds a boxed `x` — so a `.field` on it `unbox-impl`s.
            IrExpr::GetValue(i) => {
                matches!(slots.get(i).map(|t| repr_of_ty(t, under)), Some(Repr::Boxed(c)) if c == x)
            }
            IrExpr::GetField { class, index, .. } => fields
                .get(*class as usize)
                .and_then(|fs| fs.get(*index as usize))
                .is_some_and(|t| matches!(repr_of_ty(t, under), Repr::Boxed(c) if c == x)),
            IrExpr::PropertyRead { ty, operation, .. } => {
                let recorded = operation.unwrap_or(id);
                if types
                    .unboxed_declared_property(recorded, under)
                    .is_some_and(|declared| declared.non_null().obj_internal() == Some(x))
                {
                    false
                } else {
                    (is_x(ty) && physical.get(&id).is_some_and(|ty| ty.is_erased_top()))
                        || matches!(repr_of_ty(ty, under), Repr::Boxed(c) if c == x)
                }
            }
            IrExpr::Call {
                callee: Callee::Static { owner, name, .. },
                ..
            } if *owner == x && name == "box-impl" => true,
            IrExpr::Call { callee, .. } if callee.source_function().is_some() => funcs
                .get(
                    callee
                        .source_function()
                        .expect("guarded same-file function call") as usize,
                )
                .is_some_and(|function| is_x(&function.ret)),
            // A cross-file call returning the value class `x` (or `x?`) hands back a BOXED `x` — the sibling
            // facade/owner exposes the boxed wrapper across the file boundary (like a classpath member). So a
            // nullable-VC-return tail that is such a call is already boxed and must NOT be re-boxed.
            IrExpr::Call {
                callee: Callee::CrossFile { ret, .. },
                ..
            } => is_x(ret),
            IrExpr::Call {
                callee:
                    Callee::Virtual {
                        params: Some((_, ret)),
                        ..
                    },
                ..
            } => is_x(ret),
            // A function-value invocation (`fn.invoke(..)`) whose logical return is a value class `x`: the
            // generated `Function{N}.invoke` adapter returns a BOXED `x` (the underlying `box-impl`'d back —
            // a `Function`'s reference type argument is the box), so a `.field` on the result `unbox-impl`s it.
            IrExpr::InvokeFunction { ret, .. } => is_x(ret),
            IrExpr::Call {
                callee: Callee::Static { descriptor, .. } | Callee::Virtual { descriptor, .. },
                ..
            } => descriptor_returns_class(descriptor, x),
            // A stdlib reference-array element read yields a boxed element.
            IrExpr::Call {
                callee:
                    Callee::Intrinsic {
                        operation: crate::ir::IrIntrinsic::ArrayGet,
                        ..
                    },
                ..
            } => true,
            // `e as X` / `e as X?` yields a boxed `X` (e.g. casting an `Any` returned by a value-class method
            // seen through a supertype) — the property access then `unbox-impl`s it. EXCEPT when the operand is
            // ALREADY an unboxed `X` (a generic value-class receiver erased to its underlying, with a no-op
            // `(X)v` self-cast common lowering inserts): there the cast is identity (step 5 strips it) and the
            // value is the underlying, so the access is identity too.
            IrExpr::TypeOp {
                op:
                    crate::ir::IrTypeOp::Cast
                    | crate::ir::IrTypeOp::CastNonNull
                    | crate::ir::IrTypeOp::SafeCast,
                arg,
                type_operand,
            } => {
                is_x(type_operand)
                    && (self.is_boxed_vc(*arg, x)
                        || !matches!(self.repr(*arg), Repr::Unboxed(c) if c == x))
            }
            IrExpr::NotNullAssert { operand, .. } => self.is_boxed_vc(*operand, x),
            // A `when` whose non-null branch yields a boxed `x` (a nullable safe-call: `box-impl` vs `null`) is
            // a boxed `x`.
            IrExpr::When { branches } => branches.iter().any(|(_, r)| self.is_boxed_vc(*r, x)),
            // A sole-field access of a value class whose underlying is itself a BOXED value class
            // (`ZN(val z: Z1?)`) reads as `ImplicitCoercion(ZN.unbox-impl(): LZ1;)` — transparently a boxed
            // `Z1`. Recurse into the coerced value so a further `.x` on it `unbox-impl`s.
            IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::ImplicitCoercion,
                arg,
                type_operand,
            } => {
                // The coercion itself is the checked representation boundary. A non-null `X` target
                // promises the unboxed carrier after step 5, even when its operand is an erased generic
                // read such as `List<X>.get`. Treating the pre-rewrite operand as the coercion's result
                // makes a following sole-property access insert a second `unbox-impl`.
                // A surviving coercion to a boxed `X?` is one whose operand already was the box (an
                // unboxed operand was rewritten to `box-impl`), even an erased generic read.
                match target(type_operand, under) {
                    Target::UnboxedX(target) if target == x => false,
                    Target::Boxed if is_x(type_operand) => true,
                    _ => self.is_boxed_vc(*arg, x),
                }
            }
            IrExpr::Block { value: Some(v), .. } => self.is_boxed_vc(*v, x),
            _ => false,
        }
    }
}

/// A NULLABLE value-class type `X?` (which stays boxed) → its internal name.
fn boxed_vc(t: &Ty, under: &Under) -> Option<TypeName> {
    if t.is_nullable() {
        if let Some(fq_name) = t.non_null().obj_internal() {
            if under.contains_key(&fq_name) && nullable_is_boxed(fq_name, under) {
                return Some(fq_name);
            }
        }
    }
    None
}

/// Whether the expr at `id` is an UNBOXED value-class value of class `x` (a `constructor-impl`/
/// `unbox-impl` result, or an identity block over one).
fn is_unboxed_vc(exprs: &[IrExpr], id: ExprId, x: TypeName) -> bool {
    match &exprs[id as usize] {
        IrExpr::Call {
            callee: Callee::Static { owner, name, .. },
            ..
        } if *owner == x && (name == "constructor-impl" || name == "unbox-impl") => true,
        IrExpr::Block { value: Some(v), .. } => is_unboxed_vc(exprs, *v, x),
        _ => false,
    }
}

/// At a value-producing (return) position, box an unboxed `X` with `box-impl`, recursing through
/// `when`/block tails so each branch is boxed (a `null` branch is left alone).
fn box_tail(ir: &mut IrFile, id: ExprId, x: TypeName, under: &Under) {
    match &ir.exprs[id as usize] {
        IrExpr::When { branches } => {
            let rs: Vec<ExprId> = branches.iter().map(|(_, r)| *r).collect();
            for r in rs {
                box_tail(ir, r, x, under);
            }
        }
        IrExpr::Block { value: Some(v), .. } => {
            let v = *v;
            box_tail(ir, v, x, under);
        }
        // A statement-only block (`{ … ; return x }`) tails on its last statement.
        IrExpr::Block { value: None, stmts } => {
            if let Some(&last) = stmts.last() {
                box_tail(ir, last, x, under);
            }
        }
        IrExpr::Return(Some(v)) => {
            let v = *v;
            box_tail(ir, v, x, under);
        }
        _ => {
            if is_unboxed_vc(&ir.exprs, id, x) {
                box_wrap(ir, id, x, under);
            }
        }
    }
}

/// In a suspend function whose CPS result is the box of its value class, return a tail that unboxes
/// a suspend call with that same boxed result (`= delegate.parcel()`) as the box it already is,
/// before [`box_ref_tail`] would box it again: kotlinc returns the callee's `Object` untouched, so
/// the call stays a tail call. `unboxes` holds the exact `unbox-impl` nodes realized over such calls.
fn restore_boxed_suspension_tails(ir: &mut IrFile, id: ExprId, unboxes: &HashSet<ExprId>) {
    match &ir.exprs[id as usize] {
        IrExpr::When { branches } => {
            let tails: Vec<ExprId> = branches.iter().map(|(_, tail)| *tail).collect();
            for tail in tails {
                restore_boxed_suspension_tails(ir, tail, unboxes);
            }
        }
        IrExpr::Block {
            value: Some(tail), ..
        }
        | IrExpr::Return(Some(tail)) => {
            let tail = *tail;
            restore_boxed_suspension_tails(ir, tail, unboxes);
        }
        IrExpr::Block { value: None, stmts } => {
            if let Some(&last) = stmts.last() {
                restore_boxed_suspension_tails(ir, last, unboxes);
            }
        }
        // The checked coercion to the value class, realized as its carrier over the unbox.
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } if unboxes.contains(arg) => {
            let IrExpr::Call {
                dispatch_receiver: Some(boxed),
                ..
            } = ir.exprs[*arg as usize]
            else {
                return;
            };
            // The CPS result is already the box, so it needs no cast to it either. The call keeps
            // its own identity, which its suspension and representation facts are keyed by.
            let value = match ir.exprs[boxed as usize] {
                IrExpr::TypeOp {
                    op: crate::ir::IrTypeOp::Cast,
                    arg,
                    ..
                } => arg,
                _ => boxed,
            };
            ir.exprs[id as usize] = IrExpr::Block {
                stmts: Vec::new(),
                value: Some(value),
            };
        }
        _ => {}
    }
}

/// Box the tail of `id` to value class `X` for a REFERENCE-slot return (a lambda's `Object`-returning
/// `invoke`): recurse `when`/block/`return` tails, and box any tail value that is not ALREADY a boxed `X`.
/// Unlike [`box_tail`] (which only boxes the syntactic `constructor-impl`/`unbox-impl` forms), this boxes
/// EVERY unboxed tail — `this`, a captured field, a library call returning the unboxed underlying — since
/// the declared value-class return `X` fixes what the box must be. Uniform across all value classes.
fn box_ref_tail(ir: &mut IrFile, id: ExprId, x: TypeName, inputs: ReprInputs<'_>) {
    let under = inputs.under;
    // A structural intrinsic point may contain an inlined user block whose own tail has a different
    // type (`suspendCoroutine<T> { ... }` contains a `Unit` block). Its exact physical result belongs
    // to the point as a whole; never recurse through that semantic boundary and reinterpret the block
    // tail as the enclosing function's value-class result.
    if ir.physical_types.get(&id).is_some_and(|ty| {
        ty.non_null()
            .obj_internal()
            .is_some_and(|classifier| classifier == x)
    }) {
        return;
    }
    match &ir.exprs[id as usize] {
        IrExpr::When { branches } => {
            let rs: Vec<ExprId> = branches.iter().map(|(_, r)| *r).collect();
            for r in rs {
                box_ref_tail(ir, r, x, inputs);
            }
        }
        IrExpr::Block { value: Some(v), .. } => {
            let v = *v;
            box_ref_tail(ir, v, x, inputs);
        }
        IrExpr::Block { value: None, stmts } => {
            if let Some(&last) = stmts.last() {
                box_ref_tail(ir, last, x, inputs);
            }
        }
        IrExpr::Return(Some(v)) => {
            let v = *v;
            box_ref_tail(ir, v, x, inputs);
        }
        _ => {
            // Already a boxed `X` (a `box-impl` result, a call/slot typed `X`, a `?.`-`when` box) → leave it;
            // otherwise the tail is the unboxed underlying and must be boxed to `X`.
            if !inputs.over(ir).is_boxed_vc(id, x) {
                box_wrap(ir, id, x, under);
            }
        }
    }
}

/// Box the tail of a NULLABLE value-class return `X?` (boxed descriptor `LX;`). Recurses `when`/block/
/// `return` tails like [`box_ref_tail`], but boxes ONLY a tail whose representation IS an unboxed `X`
/// (a value-class field read, a call returning the unboxed underlying, a `constructor-impl`). A `null`
/// tail, an already-boxed `X`, and any UNRELATED value — e.g. a suspend continuation's `kotlin/Result`
/// resume value, which shares the boxed-`Result` return descriptor but is not itself an unboxed
/// `Result` — are left untouched. The widening counterpart of the checker accepting `X` where `X?` is
/// expected. Works for a classpath value class too (it is in `under`, so `box_wrap` emits its `box-impl`).
fn box_nullable_vc_tail(
    ir: &mut IrFile,
    id: ExprId,
    x: TypeName,
    inputs: ReprInputs<'_>,
    is_tail: bool,
) {
    let under = inputs.under;
    let recur = |ir: &mut IrFile, e: ExprId, t: bool| box_nullable_vc_tail(ir, e, x, inputs, t);
    match ir.exprs[id as usize].clone() {
        // Control flow whose branch RESULTS are tails (they inherit `is_tail`); a `when`/`if` CONDITION
        // is a plain sub-expression that may itself contain a `return` to box.
        IrExpr::When { branches } => {
            for (cond, body) in branches {
                if let Some(c) = cond {
                    recur(ir, c, false);
                }
                recur(ir, body, is_tail);
            }
        }
        IrExpr::Block { stmts, value } => {
            let n = stmts.len();
            for (i, s) in stmts.iter().enumerate() {
                // With no explicit `value`, the LAST statement is the block's value (an implicit return).
                let stmt_tail = is_tail && value.is_none() && i + 1 == n;
                recur(ir, *s, stmt_tail);
            }
            if let Some(v) = value {
                recur(ir, v, is_tail);
            }
        }
        // An explicit `return <v>` (tail OR a guard clause) boxes its returned value uniformly.
        IrExpr::Return(Some(v)) => recur(ir, v, true),
        IrExpr::Return(None) => {}
        // A loop is never a tail value, but a `return` inside its body still belongs to this function.
        IrExpr::While {
            cond, body, update, ..
        } => {
            recur(ir, cond, false);
            recur(ir, body, false);
            if let Some(u) = update {
                recur(ir, u, false);
            }
        }
        IrExpr::Try {
            body,
            catches,
            finally,
            ..
        } => {
            recur(ir, body, is_tail);
            for c in &catches {
                recur(ir, c.body, is_tail);
            }
            if let Some(f) = finally {
                recur(ir, f, false);
            }
        }
        // A lambda's `return`s are the LAMBDA's, not this function's — do not descend.
        IrExpr::Lambda { .. } => {}
        _ => {
            // First descend into any nested `return` (e.g. one inside a call argument), never a tail.
            let mut kids = Vec::new();
            crate::ir::for_each_child(&ir.exprs, id, &mut |c| kids.push(c));
            for c in kids {
                recur(ir, c, false);
            }
            // Then, at a TAIL, box this value if it is a VC-`x` value not already boxed. A tail is a VC-`x`
            // value when its logical (checker) type IS `x` — a member/local call returning `x`, an
            // `x`-typed field read — or its repr is a syntactic unboxed `x` (a `constructor-impl`). A tail
            // whose logical type is NOT `x` (a `null`, or an unrelated value that merely shares the boxed
            // return descriptor — a suspend continuation's `kotlin/Result` resume value) is left untouched.
            if is_tail {
                let logical_is_x = ir
                    .logical_types
                    .get(&id)
                    .and_then(|t| t.non_null().obj_internal())
                    .is_some_and(|n| n == x);
                let repr_unboxed_x = matches!(
                    inputs.over(ir).repr(id),
                    Repr::Unboxed(c) if c == x
                );
                let already_boxed = inputs.over(ir).is_boxed_vc(id, x);
                if (logical_is_x || repr_unboxed_x) && !already_boxed {
                    box_wrap(ir, id, x, under);
                }
            }
        }
    }
}

/// Replace the expr at `id` with `box-impl(<original expr at id>)`.
/// Replace the unboxed value at `id` with `X.toString-impl(value)`.
fn to_string_wrap(ir: &mut IrFile, id: ExprId, x: TypeName, under: &Under) {
    let new_id = clone_below_representation_wrapper(ir, id);
    let u = under.get(&x).map(|t| erase(t, under)).unwrap_or(Ty::Error);
    ir.exprs[id as usize] = IrExpr::Call {
        callee: Callee::Static {
            owner: x,
            name: "toString-impl".to_string(),
            descriptor: format!("({})Ljava/lang/String;", desc(&u)),
            inline: InlineKind::None,
        },
        dispatch_receiver: None,
        args: vec![new_id],
    };
    ir.physical_types.insert(id, Ty::String);
    // The part is still a value-class operand: its text is appended as that type, not as a String.
    ir.logical_types.insert(id, Ty::obj_name(x));
}

fn box_wrap(ir: &mut IrFile, id: ExprId, x: TypeName, under: &Under) {
    let new_id = clone_below_representation_wrapper(ir, id);
    let u = under.get(&x).map(|t| erase(t, under)).unwrap_or(Ty::Error);
    let d = desc(&u);
    let owner_rendered = x.render();
    ir.exprs[id as usize] = IrExpr::Call {
        callee: Callee::Static {
            owner: x,
            name: "box-impl".to_string(),
            descriptor: format!("({d})L{owner_rendered};"),
            inline: InlineKind::None,
        },
        dispatch_receiver: None,
        args: vec![new_id],
    };
    ir.physical_types.insert(id, Ty::obj_name(x));
}

/// Null-safe box: replace the expr at `id` with `{ tmp = <orig>; if (tmp == null) null else box-impl(tmp) }`
/// — boxing a nullable (reference-underlying) value class without hitting the ctor null-check on `null`.
fn box_wrap_nullable(ir: &mut IrFile, id: ExprId, x: TypeName, under: &Under, slot: u32) {
    let orig_id = clone_below_representation_wrapper(ir, id);
    let u = under.get(&x).map(|t| erase(t, under)).unwrap_or(Ty::Error);
    let var = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::Variable {
        index: slot,
        ty: u.clone(),
        init: Some(orig_id),
        named: false,
    });
    let get_for_test = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::GetValue(slot));
    let null1 = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::Const(crate::ir::IrConst::Null));
    let is_null = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::PrimitiveBinOp {
        op: crate::ir::IrBinOp::Eq,
        lhs: get_for_test,
        rhs: null1,
    });
    let null2 = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::Const(crate::ir::IrConst::Null));
    let get_for_box = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::GetValue(slot));
    let d = desc(&u);
    let owner_rendered = x.render();
    let boxed = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::Call {
        callee: Callee::Static {
            owner: x,
            name: "box-impl".to_string(),
            descriptor: format!("({d})L{owner_rendered};"),
            inline: InlineKind::None,
        },
        dispatch_receiver: None,
        args: vec![get_for_box],
    });
    let when = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::When {
        branches: vec![(Some(is_null), null2), (None, boxed)],
    });
    ir.null_guards.insert(when);
    ir.exprs[id as usize] = IrExpr::Block {
        stmts: vec![var],
        value: Some(when),
    };
    ir.physical_types.insert(id, Ty::obj_name(x));
}

/// Erase a value-class type to its underlying representation. Non-null `X` → underlying `U`. A nullable
/// `X?` erases to the underlying ONLY when that underlying is a reference (which can itself hold null);
/// over a primitive underlying, `X?` stays the boxed `X` (a primitive can't represent null). Non-value
/// types pass through.
/// Whether a lifted lambda realizes the value class in one of its OWN slots BOXED.
///
/// A plain `FunctionN.invoke` slot is generic (`Object`), and a value class travelling through one is
/// boxed. A SAM conversion targets a DECLARED method instead, so the answer is whatever the interface
/// spells: a slot declared as the value class itself erases to the underlying (kotlinc's
/// `ResultHandler.onResult(Ljava/lang/Object;)` carries the *carrier*, not a `kotlin/Result` box),
/// while a slot declared as a type parameter is generic again and does box. `declared` is the SAM
/// method's declaration at that position; `None` means there is no SAM (or its arity doesn't line up
/// with the lambda's own parameters), which keeps the `FunctionN` reading.
fn lambda_slot_is_boxed(declared: Option<&Ty>, value_class: TypeName) -> bool {
    declared.is_none_or(|t| t.non_null().obj_internal() != Some(value_class))
}

/// The SAM method's declared parameter types for a lifted lambda, aligned to the lambda's OWN
/// parameters (`own_from` is where those begin, after the captures). `None` unless the lambda was SAM
/// converted AND the two arities agree — an implicit receiver or context parameter in the lambda's
/// own slots has no counterpart in the interface declaration, and misaligned slots must not be read.
fn lambda_sam_params(
    signatures: &HashMap<u32, (Vec<Ty>, Ty)>,
    fid: u32,
    own_from: u32,
    total_params: usize,
) -> Option<&[Ty]> {
    let (params, _) = signatures.get(&fid)?;
    (params.len() == total_params.saturating_sub(own_from as usize)).then_some(params.as_slice())
}

/// Whether the lambda `impl_fn` was SAM-converted to a method whose DECLARED return is the very value
/// class the lambda declares — in which case the JVM return is that class's erased underlying and the
/// body's tail already produces it.
fn sam_declares_vc_return(
    ir: &crate::ir::IrFile,
    orig_rets: &[Ty],
    impl_fn: u32,
    under: &Under,
) -> bool {
    let Some(x) = orig_rets
        .get(impl_fn as usize)
        .and_then(|t| t.non_null().obj_internal())
        .filter(|fq| under.contains_key(fq))
    else {
        return false;
    };
    ir.lambda_sam_signature
        .get(&impl_fn)
        .is_some_and(|(_, ret)| ret.non_null().obj_internal() == Some(x))
}

/// Erase the value-class types in a JVM method descriptor: each `L<fq>;` whose `<fq>` is a value class
/// becomes its underlying descriptor (`(LIv;)Ljava/lang/String;` → `(I)Ljava/lang/String;`).
fn erase_descriptor(descriptor: &str, under: &Under) -> String {
    let bytes = descriptor.as_bytes();
    let mut out = String::with_capacity(descriptor.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'L' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b';' {
                j += 1;
            }
            let fq = &descriptor[start..j];
            // A value class in a JVM reference-array component is necessarily BOXED. Erase a
            // declaration slot `LValue;` to its carrier, but never rewrite the component of
            // `[LValue;`: doing so silently changes `Array<UInt>` into `IntArray` and likewise turns
            // `Array<Data>` into the array-valued carrier of `Data`.
            let underlying = (bytes.get(i.wrapping_sub(1)) != Some(&b'['))
                .then(|| existing_type_name(fq).and_then(|name| under.get(&name)))
                .flatten();
            if let Some(u) = underlying {
                out.push_str(&desc(&erase(u, under)));
            } else {
                out.push_str(&descriptor[i..=j]);
            }
            i = j + 1;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn desc(t: &Ty) -> String {
    type_descriptor(ir_ty_to_jvm(t))
}

fn ir_method_desc(params: &[Ty], ret: &Ty) -> String {
    crate::jvm::method_descriptors::ir_method_desc(params, ret)
}

/// Collect every `ExprId` reachable from `root` (a function body), so rewrites stay within bodies that
/// own value-class values unboxed.
fn collect_reachable(exprs: &[IrExpr], root: ExprId, out: &mut HashSet<ExprId>) {
    if !out.insert(root) {
        return;
    }
    crate::ir::for_each_child(exprs, root, &mut |c| collect_reachable(exprs, c, out));
}

/// Add every retained lambda inline body reachable from `bodies` as its own lexical value-slot scope.
/// The standalone `impl_fn` body and the value-producing `inline_body` are distinct expression DAGs,
/// but both use the lambda implementation's numbering. JVM inline splicing can emit the latter even
/// when the implementation method is also emitted, so representation rewrites must visit both copies.
fn append_inline_body_scopes(
    ir: &IrFile,
    bodies: &mut Vec<(ExprId, HashMap<u32, Ty>)>,
    slot_types: &[HashMap<u32, Ty>],
    own_parameters: &inline_body_slots::OwnParameters,
) {
    let mut known_roots: HashSet<ExprId> = bodies.iter().map(|(root, _)| *root).collect();
    let mut cursor = 0;
    while cursor < bodies.len() {
        let root = bodies[cursor].0;
        cursor += 1;
        let mut reachable = HashSet::new();
        collect_reachable_scoped(&ir.exprs, root, &mut reachable);
        for expression in reachable {
            let IrExpr::Lambda {
                impl_fn,
                inline_body: Some(inline_body),
                ..
            } = &ir.exprs[expression as usize]
            else {
                continue;
            };
            if known_roots.insert(*inline_body) {
                if let Some(slots) = slot_types.get(*impl_fn as usize) {
                    let slots = own_parameters.unboxed(*impl_fn, slots.clone());
                    bodies.push((*inline_body, slots));
                }
            }
        }
    }
}

/// Like [`collect_reachable], but never descends into a lambda's body — only its captures. Both a
/// closure's standalone implementation body and its retained inline body have their own value-index
/// numbering and slot types. Reaching either from the enclosing scope would let representation
/// analysis interpret those indices using the wrong lexical frame. Callers that also transform
/// retained inline bodies add them as independent roots with [`append_inline_body_scopes`].
fn collect_reachable_scoped(exprs: &[IrExpr], root: ExprId, out: &mut HashSet<ExprId>) {
    if !out.insert(root) {
        return;
    }
    if let IrExpr::Lambda { captures, .. } = &exprs[root as usize] {
        for &c in captures {
            collect_reachable_scoped(exprs, c, out);
        }
        return;
    }
    crate::ir::for_each_child(exprs, root, &mut |c| {
        collect_reachable_scoped(exprs, c, out)
    });
}

#[cfg(test)]
mod descriptor_return_tests {
    use super::descriptor_returns_class;
    use crate::types::type_name;

    #[test]
    fn descriptor_return_matches_the_interned_class_without_rendering() {
        let class = type_name("pkg/Foo$Bar");
        assert!(descriptor_returns_class("()Lpkg/Foo$Bar;", class));
        assert!(descriptor_returns_class("()[Lpkg/Foo$Bar;", class));
        assert!(!descriptor_returns_class("()I", class));
        assert!(!descriptor_returns_class("()Lpkg/Other;", class));
        assert!(!descriptor_returns_class("(Lpkg/Foo$Bar;)V", class));
        assert!(!descriptor_returns_class(
            "(Lpkg/Foo$Bar;)Lpkg/Other;",
            class
        ));
    }
}
