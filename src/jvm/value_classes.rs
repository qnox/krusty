//! JVM `@JvmInline value class` IR lowering pass — an **optional, JVM-only** IR→IR transform.
//!
//! `ir_lower` keeps a value class as a plain `Class{X}` so the platform-agnostic IR stays neutral (a JS
//! backend, or a future Valhalla JVM with *native* value types, leaves value classes alone). The old
//! JVM has no native value types, so this pass realizes kotlinc's unboxed representation:
//!   * a NON-nullable `X` erases to its single field's (underlying) type `U` everywhere — signatures,
//!     fields, locals (a nullable `X?` stays the boxed `Class{X}`);
//!   * `new X(arg)` becomes `X.constructor-impl(arg): U` (the unboxed value);
//!   * sole-property access on an unboxed value (`x.v`) is identity (the value already IS the `U`);
//!   * a value-class parameter that erased to a primitive loses its non-null `checkNotNullParameter`.
//!
//! The value class's own synthesized members (`box-impl`/`unbox-impl`/`constructor-impl`/getter/`<init>`
//! — emitted by `ir_lower::synth_value_members`) genuinely operate on the boxed object, so they are NOT
//! rewritten (only their signatures erase, and `box-impl`'s return stays the boxed `X`).
//!
//! NOTE: box/unbox insertion at representation boundaries (a value flowing to `Any`/generic, or back) is
//! the next increment; this pass currently lowers the unboxed core (construction, access, erasure).

use crate::ir::{value_tails, Callee, ExprId, IrExpr, IrFile};
use crate::jvm::ir_emit::{ir_ty_to_jvm, jvm_tys};
use crate::jvm::names::{method_descriptor, property_getter_name, type_descriptor};
use crate::libraries::InlineKind;
use crate::types::{existing_type_name, type_name, Ty, TypeName};
use std::collections::{HashMap, HashSet};

/// The stdlib value classes whose underlying is JVM-native unsigned (no synthesized `-impl` members —
/// their box/unbox lives on the classpath). All erase to a signed primitive, so they contribute nothing
/// to the erasure map and are skipped when probing referenced classes.
type Under = HashMap<TypeName, Ty>;

fn is_native_unsigned(fq: TypeName) -> bool {
    fq.matches("kotlin/UByte")
        || fq.matches("kotlin/UShort")
        || fq.matches("kotlin/UInt")
        || fq.matches("kotlin/ULong")
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

/// Lower all `@JvmInline value class` usage in `ir` to the JVM's unboxed representation: erase the
/// value-class type to its single field's type, rewrite construction/sole-property access, and insert
/// box/unbox at the representation boundaries this pass models. The `bool` result is reserved for a
/// future structural bail; today it always returns `true` (the pass never skips a value-class file —
/// shapes it does not yet handle are emitted as-is, surfacing as a conformance FAIL to be fixed, not a
/// silent skip).
/// Every `Obj` class-internal name occurring anywhere in a `Ty` (recursing type arguments, arrays,
/// nullables, function types) pushed to `out`.
fn collect_obj_names(t: Ty, out: &mut Vec<TypeName>) {
    match t {
        Ty::Obj(n, args) => {
            out.push(n);
            for a in args {
                collect_obj_names(*a, out);
            }
        }
        Ty::Nullable(inner) => collect_obj_names(*inner, out),
        Ty::Fun(s) => {
            for p in &s.params {
                collect_obj_names(*p, out);
            }
            collect_obj_names(s.ret, out);
        }
        _ => {}
    }
}

/// Every class name referenced by a `Ty` anywhere in the IR — function signatures, class fields, recorded
/// logical types, and `TypeOp`/`Variable`/`InvokeFunction` type operands. The value-class pass probes each
/// against the `SymbolSource` to find the classpath value classes this file uses, without a lowerer-built
/// side map.
fn referenced_class_names(ir: &IrFile) -> Vec<TypeName> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for f in &ir.functions {
        for p in &f.params {
            collect_obj_names(*p, &mut out);
        }
        collect_obj_names(f.ret, &mut out);
    }
    for c in &ir.classes {
        for fld in &c.fields {
            collect_obj_names(fld.ty, &mut out);
        }
        for s in &c.supertypes {
            collect_obj_names(*s, &mut out);
        }
        for (_, b) in &c.type_param_bounds {
            collect_obj_names(*b, &mut out);
        }
        for a in &c.ctor_args {
            collect_obj_names(a.ty, &mut out);
        }
        for constructor in &c.secondary_ctors {
            constructor
                .prefix_params
                .iter()
                .chain(&constructor.params)
                .for_each(|ty| collect_obj_names(*ty, &mut out));
        }
        for entry in &c.enum_entries {
            entry
                .constructor_parameter_types
                .iter()
                .for_each(|ty| collect_obj_names(*ty, &mut out));
        }
        if let Some(parameters) = &c.enum_entry_of {
            parameters
                .iter()
                .for_each(|ty| collect_obj_names(*ty, &mut out));
        }
        // A dependency callable reference may be the only place a classpath value class occurs.
        // Its synthetic carrier owns both the logical FunctionN signature and the already-selected
        // declaration signature; include those types so this JVM representation pass can realize
        // mangling/erasure from the provider's value-class declaration.
        if let Some(reference) = &c.func_ref {
            reference
                .param_tys
                .iter()
                .chain(&reference.target_param_tys)
                .for_each(|ty| collect_obj_names(*ty, &mut out));
            collect_obj_names(reference.ret_ty, &mut out);
            collect_obj_names(reference.target_ret_ty, &mut out);
            if let Some(parameters) = &reference.reflection_target_param_tys {
                parameters
                    .iter()
                    .for_each(|ty| collect_obj_names(*ty, &mut out));
            }
            if let Some(result) = reference.reflection_target_ret_ty {
                collect_obj_names(result, &mut out);
            }
        }
    }
    for t in ir.logical_types.values() {
        collect_obj_names(*t, &mut out);
    }
    for e in &ir.exprs {
        match e {
            IrExpr::TypeOp { type_operand, .. } => collect_obj_names(*type_operand, &mut out),
            IrExpr::Variable { ty, .. } => collect_obj_names(*ty, &mut out),
            IrExpr::InvokeFunction { params, ret, .. } => {
                params
                    .iter()
                    .for_each(|ty| collect_obj_names(*ty, &mut out));
                collect_obj_names(*ret, &mut out);
            }
            IrExpr::PropertyRead { owner, ty, .. } | IrExpr::PropertyWrite { owner, ty, .. } => {
                // Semantic property nodes replaced realization-shaped calls, so both the declaring
                // owner (needed to recognize a value class's sole-property identity read) and logical
                // value type (needed to mangle/erase another class's accessor) are type references in
                // their own right. Omitting either makes value-class handling depend on some unrelated
                // signature also mentioning the class.
                out.push(*owner);
                collect_obj_names(*ty, &mut out);
            }
            IrExpr::New {
                internal,
                ctor_params,
                ..
            } => {
                // The constructed class itself — so a value class being constructed (`Id(x)`, incl. a
                // classpath/other-module one) enters `under` and its `New` is rewritten to
                // `constructor-impl` rather than emitted as a raw (private-`<init>`) `new`.
                out.push(*internal);
                if let Some(ps) = ctor_params {
                    for p in ps {
                        collect_obj_names(*p, &mut out);
                    }
                }
            }
            IrExpr::RefNew { elem, .. }
            | IrExpr::RefGet { elem, .. }
            | IrExpr::RefSet { elem, .. } => collect_obj_names(*elem, &mut out),
            IrExpr::Vararg { array_type, .. } | IrExpr::NewArray { array_type, .. } => {
                collect_obj_names(*array_type, &mut out)
            }
            IrExpr::Call {
                callee:
                    Callee::CrossFile { params, ret, .. }
                    | Callee::ModuleWithDefaults { params, ret, .. },
                ..
            } => {
                for parameter in params {
                    collect_obj_names(*parameter, &mut out);
                }
                collect_obj_names(*ret, &mut out);
            }
            IrExpr::Call {
                callee:
                    Callee::Virtual {
                        params: Some((ps, ret)),
                        ..
                    },
                ..
            } => {
                for p in ps {
                    collect_obj_names(*p, &mut out);
                }
                collect_obj_names(*ret, &mut out);
            }
            _ => {}
        }
    }
    out.retain(|name| seen.insert(*name));
    out
}

fn supplied_constructor_parameters<'a>(
    parameters: &'a [Ty],
    defaults: &'a [u32],
    prefix_count: u32,
) -> impl Iterator<Item = &'a Ty> {
    let prefix_count = prefix_count as usize;
    parameters
        .iter()
        .enumerate()
        .filter_map(move |(parameter, ty)| {
            (parameter < prefix_count
                || !defaults.contains(&u32::try_from(parameter - prefix_count).ok()?))
            .then_some(ty)
        })
}

/// Kotlin override declarations remain overridable unless explicitly final. Pass 1 already
/// published their exact declaration edges, so this representation pass marks the corresponding
/// IR methods without searching supertypes or comparing names.
pub(crate) fn apply_override_final_drop(ir: &mut IrFile) {
    let mut override_opens = HashSet::new();
    for (&owner, edges) in &ir.function_overrides {
        let Some(class) = ir.classes.iter().find(|class| class.fq_name == owner) else {
            continue;
        };
        if class.is_interface {
            continue;
        }
        for edge in edges {
            if edge.implementation_owner != owner {
                continue;
            }
            let function = edge.implementation_function.or_else(|| {
                let crate::fir::ResolvedFunctionOverrideTarget::Module(declaration) =
                    edge.implementation
                else {
                    return None;
                };
                ir.checked_callable_functions.get(&declaration).copied()
            });
            let Some(function) = function else {
                continue;
            };
            if class.methods.contains(&function)
                && ir
                    .functions
                    .get(function as usize)
                    .is_some_and(|function| !function.is_static)
            {
                override_opens.insert(function);
            }
        }
    }
    ir.open_methods.extend(override_opens);
}

#[must_use]
pub fn lower_value_classes(
    ir: &mut IrFile,
    classpath: &crate::jvm::classpath::Classpath,
    // Same-module SOURCE value classes (internal name → sole-field underlying), collected from the
    // frontend symbols. A value class declared in ANOTHER file of this module is neither in `ir.classes`
    // (a different file) nor reported by the resolver (whose `value_underlying` only decodes classpath
    // `@Metadata`), so its erasure/mangle map entry comes from here — without leaking value-class-ness
    // into the CHECKER's library view (which drives construction/member resolution).
    module_value_classes: &std::collections::HashMap<TypeName, Ty>,
    // Subset whose stable declaration headers describe a value-class shape supported by metadata
    // emission. This is frozen before Pass 2; no sibling body or source coordinate is retained.
    module_readable_value_classes: &std::collections::HashSet<TypeName>,
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
    // Internal name → JVM carrier of the single underlying property, before recursive value-class
    // erasure. A generic underlying property uses its declared upper bound: `S<T : String>` carries
    // `String`, while `V<T : Int>` carries `int`. Keeping the unbound `TyParam` here would incorrectly
    // force an Object slot and later descriptor code would independently specialize the same bound,
    // leaving boxing and null guards inconsistent with the emitted method descriptor.
    let under: Under = ir
        .classes
        .iter()
        .filter(|c| c.is_value)
        .filter_map(|c| {
            c.fields.first().map(|f| {
                let u = f
                    .type_param
                    .as_ref()
                    .map(|name| {
                        c.type_param_bounds
                            .iter()
                            .find(|(candidate, _)| candidate == name)
                            .map(|(_, bound)| *bound)
                            .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")))
                    })
                    .unwrap_or(f.ty)
                    .canonical_semantic();
                (c.fq_name, u)
            })
        })
        .collect();
    // Merge classpath `@JvmInline value class`es referenced by this file (`Result` → `Object`). They are
    // NOT in `ir.classes` (no synthesized members — their `-impl`/`box-impl` live on the classpath), so
    // they only contribute to the erasure map: every occurrence of their type erases to the underlying.
    // Value-class-ness is resolved through the federated `SymbolSource` (`is_value`), NOT a side map built
    // in the lowerer — the lowerer carries no value-class knowledge. Every referenced class name in the IR
    // is probed; a classpath value class contributes its `value_underlying`.
    let mut under = under;
    let mut external_underlying_properties = HashMap::new();
    for fq in referenced_class_names(ir) {
        if under.contains_key(&fq) || is_native_unsigned(fq) {
            continue;
        }
        let rendered = fq.render();
        if crate::types::prim_array_element(&rendered).is_some() {
            continue;
        }
        let dependency = classpath.value_class_declaration(fq);
        if let Some(property) = dependency
            .as_ref()
            .and_then(|declaration| declaration.property.clone())
        {
            crate::trace_compiler!(
                "value_classes",
                "external value class {} underlying property {}",
                fq,
                property
            );
            external_underlying_properties.insert(fq, property);
        }
        if let Some(u) = dependency
            .map(|declaration| declaration.underlying)
            .or_else(|| module_value_classes.get(&fq).copied())
        {
            // The underlying carries its own declared nullability — trust it: a NON-NULL reference
            // underlying (`ItemId(val value: String)`) means `ItemId?` stays UNBOXED (null carried by
            // the reference), exactly like a same-file value class. Classpath VCs come from the resolver
            // (decoded from `@Metadata`); same-module source VCs from `module_value_classes`.
            let u = u.canonical_semantic();
            let ir_under = u.scalar_value_repr().unwrap_or(u);
            under.insert(fq, ir_under);
        }
    }
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
                    erase(ty, &under)
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

    // Exact identities of instance entries synthesized while creating carrier implementations. They
    // already have their final instance ABI and must not be lowered again as user value-class members.
    let mut synthesized_value_class_instance_entries = HashSet::new();
    // Synthesize each value class's `-impl`/`equals`/`hashCode`/`toString` members up front (a JVM
    // concern — `ir_lower` only emits the plain single-field class). Done before the analysis below so
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
            shift_slots(ir, default);
        }
        let has_init = ir.classes[cid as usize].init_body.is_some();
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
        if !synth_value_members(
            ir,
            cid,
            &under,
            has_init,
            constructor_default,
            &mut synthesized_value_class_instance_entries,
        ) {
            crate::trace_compiler!(
                "value_classes",
                "synthesis rejected {}",
                ir.classes[cid as usize].fq_name.render()
            );
            return false;
        }
    }

    // Pre-erasure signatures, so box/unbox at call boundaries can see `Object`/generic param/field
    // types (which erasure leaves alone but values flowing in must be boxed to reach).
    let mut orig_params: Vec<Vec<Ty>> = ir.functions.iter().map(|f| f.params.clone()).collect();
    let orig_fields: Vec<Vec<Ty>> = ir
        .classes
        .iter()
        .map(|c| c.fields.iter().map(|f| f.ty.clone()).collect())
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
    // Slot types inside constructor-owned bodies must be captured before expression erasure. These
    // bodies contain compiler-introduced temporaries for property initializers and delegation
    // arguments; rebuilding their maps after erasure turns `val tmp: Money` into `Int` and hides the
    // required box at a generic `T` constructor slot.
    let orig_class_init_slots = ir
        .classes
        .iter()
        .enumerate()
        .map(|(class, declaration)| {
            declaration
                .init_body
                .map(|root| body_slot_map(&ir.exprs, root, &orig_ctor_args[class]))
        })
        .collect::<Vec<_>>();
    let orig_secondary_slots = ir
        .classes
        .iter()
        .enumerate()
        .map(|(class, declaration)| {
            declaration
                .secondary_ctors
                .iter()
                .enumerate()
                .map(|(constructor, body)| {
                    secondary_ctor_slot_map(&ir.exprs, body, &orig_secondary[class][constructor])
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let orig_super_slots = ir
        .classes
        .iter()
        .enumerate()
        .map(|(class, declaration)| {
            primary_super_slot_map(&ir.exprs, declaration, &orig_ctor_args[class])
        })
        .collect::<Vec<_>>();
    // Top-level initializers also own compiler-generated temporaries (notably the array-constructor
    // fill loop). Capture their semantic slot types before expression erasure; rebuilding this map
    // later would turn `Array<Value>` into its carrier-shaped type and hide the boxed reference-array
    // element boundary from `ArraySet`.
    let orig_static_slots = ir
        .statics
        .iter()
        .map(|property| body_slot_map(&ir.exprs, property.init, &[]))
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
    let orig_rets: Vec<Ty> = ir.functions.iter().map(|f| f.ret.clone()).collect();
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
    // A suspend override whose supertype observes a non-value-class result must preserve the concrete
    // value class's box across `Object`, even when its carrier happens to have the same JVM descriptor.
    // Each bridge carries the exact selected target function; emitted names are never lookup input.
    let force_boxed_suspend_returns: HashSet<u32> = ir
        .classes
        .iter()
        .flat_map(|class| class.bridges.iter())
        .filter_map(|bridge| {
            let target = bridge
                .target_function
                .filter(|target| suspend_fids.contains(target))?;
            let classifier = bridge
                .concrete_ret
                .non_null()
                .obj_internal()
                .filter(|classifier| under.contains_key(classifier))?;
            let supertype_uses_same_value_class_carrier = bridge
                .erased_ret
                .non_null()
                .obj_internal()
                .is_some_and(|result| result == classifier)
                && (!bridge.erased_ret.is_nullable() || !nullable_is_boxed(classifier, &under));
            (!supertype_uses_same_value_class_carrier).then_some(target)
        })
        .collect();
    let suspend_sig: std::collections::HashSet<(Option<TypeName>, String, usize)> = ir
        .functions
        .iter()
        .enumerate()
        .filter(|(fid, _)| suspend_fids.contains(&(*fid as u32)))
        .map(|(fid, f)| (f.dispatch_receiver, f.name.clone(), orig_params[fid].len()))
        .collect();
    // A suspend result crosses the erased `Continuation` boundary in the representation selected by
    // the value-class ABI. Record that representation against both local declarations and exact call
    // identities before either pass rewrites expressions. This includes nullable value classes:
    // `X<String>?` can use `String` itself as the nullable carrier, whereas `X<Int>?` must remain the
    // boxed `X` because an `int` cannot represent null.
    for &fid in &suspend_fids {
        if let Some(realization) = orig_rets.get(fid as usize).and_then(|result| {
            suspend_result_representation(
                result,
                &under,
                force_boxed_suspend_returns.contains(&fid),
            )
        }) {
            ir.value_class_suspend_returns.insert(fid, realization);
        }
    }
    ir.value_class_suspend_calls
        .extend(ir.suspend_calls.iter().filter_map(|(&call, result)| {
            suspend_result_representation(result, &under, false)
                .map(|realization| (call, realization))
        }));
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
            // implementation. A retained inline-body copy is analyzed in the caller's specialized slot scope
            // below and therefore does not share this `FunctionN` representation boundary. A scalar-underlying
            // value class keeps its own handling. Value-class-ness is decided HERE (with `under`), not in the
            // lambda-agnostic lowerer.
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
    // A GENERIC value class (`IC<T>`, its field typed by a type parameter → `Object`) has representation
    // krusty can't box-mark at a generic-override param without a stack-type conflict (its box/unbox differ
    // from a concrete-underlying value class). Leave such a param unmarked. A NON-generic value class marks
    // fine.
    let generic_vcs: std::collections::HashSet<TypeName> = ir
        .classes
        .iter()
        .filter(|c| c.is_value && !c.type_params.is_empty())
        .map(|c| c.fq_name)
        .collect();
    let mut slot_types = slot_types;
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
            let Some(&fid) = c
                .methods
                .iter()
                .find(|&&fid| ir.functions[fid as usize].name == b.name)
            else {
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
                    let supertype_generic = ep
                        .non_null()
                        .obj_internal()
                        .is_some_and(|n| n.matches("kotlin/Any") || n.matches("java/lang/Object"));
                    if under.contains_key(&x) && supertype_generic && !generic_vcs.contains(&x) {
                        // Mark BOXED in both the body's slot repr AND the call-boundary target
                        // (`orig_params`), so a CALLER boxes its arg into this generic-`Object` slot and the
                        // BODY unboxes it — the param is a boxed position at every boundary, consistently.
                        let boxed = Ty::nullable(Ty::obj_name(x));
                        slot_types[fid as usize].insert(base + i as u32, boxed);
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
    // `(fid, param idx, boxed value-class Ty)` for nullable-underlying value-class params — the base
    // method unboxes them (below), but its `$default` stub + call site keep them boxed (recorded here).
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
            || synthesized_value_class_instance_entries.contains(&(fid as u32));
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
            let mut mangled = vc_mangle(
                &source_name,
                &orig_params[fid],
                &orig_rets[fid],
                &callable_under,
                is_file_class,
                suspend_fids.contains(&(fid as u32)),
            );
            // Every ordinary value-class member is physically a static implementation over the
            // carrier. A signature that independently requires Kotlin's value-class hash keeps that
            // hash (`same-iUtXLc0`); otherwise kotlinc uses the structural `-impl` suffix. The source
            // name and source value parameters stay in `vc_declared_sigs` for metadata.
            let lower_value_member = vc_member && !f.is_static;
            if lower_value_member && mangled == source_name {
                mangled.push_str("-impl");
            }
            if mangled != source_name {
                if let Some(owner) = f.dispatch_receiver {
                    mangle_map.insert(
                        (owner, source_name.clone(), orig_params[fid].len()),
                        mangled.clone(),
                    );
                }
                f.name = mangled;
            }
            if lower_value_member {
                let owner = f
                    .dispatch_receiver
                    .expect("a value-class member has a dispatch receiver");
                let carrier = under.get(&owner).copied().unwrap_or(Ty::Error);
                f.params.insert(0, carrier);
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
                if vc_underlying_nullable(p, &under) {
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
                let under_nullable = orig_params[fid]
                    .get(k)
                    .is_some_and(|t| vc_underlying_nullable(t, &under));
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
    // `(class, method-index)` → the value class a member's RETURN keeps BOXED. A user value-class member
    // runs on / returns the boxed object (the erasure loop above left its VC return un-erased), so its
    // `MethodCall` result is a boxed `X` — a following unboxed-slot use (`val b: X = x.inc()`) must unbox.
    // (Getters are intercepted earlier in `repr` via `field_getters`; the static `-impl`s are `Call`s.)
    let boxed_ret_methods: HashMap<(u32, u32), TypeName> = {
        let mut m = HashMap::new();
        for (ci, c) in ir.classes.iter().enumerate() {
            for (mi, &fid) in c.methods.iter().enumerate() {
                if let Some(fq) = ir.functions[fid as usize].ret.non_null().obj_internal() {
                    if under.contains_key(&fq) {
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
                    .filter(|classifier| under.contains_key(classifier)),
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
    // `suspendCoroutine<T>` invokes its block with `SafeContinuation<T>` and obtains the result from
    // `SafeContinuation.getOrThrow(): Object`. A value-class `T` therefore crosses this generic slot as
    // its box (or null for `T?`), never as the carrier. Preserve that physical fact on the exact FIR-
    // selected intrinsic point so a value-class suspend-function tail does not descend into the
    // inlined `Unit`-returning user block and attempt to box that block's `Unit` result.
    let safe_intrinsic_boxes = ir
        .intrinsic_suspension_points
        .iter()
        .filter_map(|(&expression, point)| {
            if point.kind != crate::ir::IrIntrinsicSuspensionKind::Safe {
                return None;
            }
            point
                .result
                .non_null()
                .obj_internal()
                .filter(|classifier| under.contains_key(classifier))
                .map(|classifier| (expression, classifier))
        })
        .collect::<Vec<_>>();
    for (expression, classifier) in safe_intrinsic_boxes {
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
    // sees the underlying again. A retained inline-body used by the JVM bytecode splicer has the same
    // boundary: it replaces a `FunctionN.invoke(Object)` site, narrows that Object to the value-class box,
    // and then executes this body. Same-source FIR inlining cloned/re-homed its body before this backend
    // pass, so adapting the retained template does not touch the carrier-specialized clone. In-place: the
    // `GetValue` node itself becomes the unbox call over a fresh `GetValue`, so every reference to the node
    // (including a nested lambda's capture list) picks up the unboxed value.
    for (fid, slot, x, u) in boxed_own_reads {
        let mut reads = HashSet::new();
        if let Some(root) = ir.functions[fid as usize].body {
            collect_reachable_scoped(&ir.exprs, root, &mut reads);
        }
        // Common inline expansion consumes/re-homes the template and clears the implementation body;
        // that body already receives carrier-specialized operands. A still-live implementation body
        // means the retained template can instead be substituted at a JVM `FunctionN.invoke` boundary.
        let inline_roots = ir.functions[fid as usize].body.is_some().then(|| {
            ir.exprs
                .iter()
                .filter_map(|expression| match expression {
                    IrExpr::Lambda {
                        impl_fn,
                        inline_body: Some(body),
                        ..
                    } if *impl_fn == fid => Some(*body),
                    _ => None,
                })
                .collect::<Vec<_>>()
        });
        for root in inline_roots.into_iter().flatten() {
            collect_reachable_scoped(&ir.exprs, root, &mut reads);
        }
        let targets: Vec<ExprId> = reads
            .into_iter()
            .filter(|&id| matches!(&ir.exprs[id as usize], IrExpr::GetValue(i) if *i == slot))
            .collect();
        for id in targets {
            let get = ir.add_expr(IrExpr::GetValue(slot));
            ir.exprs[id as usize] = IrExpr::Call {
                callee: Callee::Virtual {
                    owner: x,
                    name: "unbox-impl".to_string(),
                    descriptor: format!("(){}", desc(&u)),
                    params: None,
                    interface: false,
                },
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
                if let Some(mangled) = mangle_map.get(&(*owner, name.clone(), args.len())) {
                    *name = mangled.clone();
                    *descriptor = erase_descriptor(descriptor, &under);
                }
            }
            // A SAM conversion names the interface method at the `invokedynamic` call site. When that
            // method mangled (its signature mentions a value class), the closure must implement the
            // MANGLED name — `LambdaMetafactory` binding the original spelling produces a class that
            // implements nothing the interface declares (`AbstractMethodError` at the first call).
            if let IrExpr::Lambda {
                sam: Some(target),
                arity,
                ..
            } = e
            {
                if let Some(mangled) =
                    mangle_map.get(&(target.classifier, target.method.clone(), *arity as usize))
                {
                    target.method = mangled.clone();
                }
            }
        }
    }
    // A SAM declaration may live in a sibling source file, so it is absent from this file's
    // `mangle_map`. The checker/lowerer handoff records the selected SAM's declared signature on the
    // lambda implementation; realize the call-site method name from that exact declaration here,
    // where value-class JVM naming belongs. This is deliberately not a classifier lookup: overload
    // selection is already complete and the implementation id identifies the recorded signature.
    if !callable_under.is_empty() {
        for expression in &mut ir.exprs {
            let IrExpr::Lambda {
                impl_fn,
                sam: Some(target),
                ..
            } = expression
            else {
                continue;
            };
            let Some((params, ret)) = ir.lambda_sam_signature.get(impl_fn) else {
                continue;
            };
            target.method =
                vc_mangle_once(&target.method, params, ret, &callable_under, false, false);
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
    // Rewrite cross-file calls with value-class signatures to their JVM names and types.
    if !callable_under.is_empty() {
        for e in &mut ir.exprs {
            if let IrExpr::Call {
                callee:
                    Callee::Virtual {
                        name,
                        params: Some((params, ret)),
                        ..
                    },
                ..
            } = e
            {
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
                        name,
                        params,
                        ret,
                        dispatch_receiver_ty,
                        ..
                    } => {
                        if let Some(receiver) = dispatch_receiver_ty {
                            *receiver = erase(receiver, &under);
                        }
                        (name, params, ret, Some(*target), true, true)
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
                let mangled = vc_mangle_once(
                    base,
                    &callable.parameters,
                    &callable.result,
                    &callable_under,
                    callable.owner.is_none(),
                    callable.flags.has(crate::fir::DeclarationFlags::SUSPEND),
                );
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
    // Function-reference classes have two signatures: the public `FunctionN.invoke(Object...)Object`
    // shape remains logical, while the target method call must follow JVM value-class erasure/mangling.
    for c in &mut ir.classes {
        let owner_fq = c.fq_name();
        let Some(fr) = &mut c.func_ref else {
            continue;
        };
        let local_target = fr.local_target.and_then(|target| {
            ir.functions
                .get(target as usize)
                .map(|function| (function.name.clone(), function.params.clone(), function.ret))
        });
        let first_call_arg = match fr.dispatch {
            crate::ir::FrDispatch::VirtualUnbound => 1usize,
            _ => usize::from(fr.reflection_receiver_parameter),
        };
        let call_owner = fr.call_owner;
        // The lowerer records the already-selected callable's exact target signature. Do not rebuild it
        // from `(owner, name, arity)`: overloads with equal arity are deliberately indistinguishable by
        // that key, and whichever declaration was visited last would corrupt every other reference.
        let target_decl_params = fr
            .reflection_target_param_tys
            .clone()
            .unwrap_or_else(|| fr.target_param_tys[first_call_arg..].to_vec());
        let target_decl_ret = fr.reflection_target_ret_ty.unwrap_or(fr.target_ret_ty);
        // A BOUND extension reference on a VALUE-CLASS receiver (`Z(42)::test`, `FrDispatch::StaticBound`)
        // targets a facade static whose leading param is the receiver — that receiver lives in
        // `target_param_tys` (the `target_override`), NOT in the invoke `param_tys`. Mangle against that
        // full sig (so `test` → `test-<hash>`), treat it as a file-class member, and erase THAT sig (so the
        // target descriptor keeps the receiver `int`, not an empty `()`), else the impl calls a
        // non-existent unmangled `test()`.
        let staticbound = matches!(fr.dispatch, crate::ir::FrDispatch::StaticBound);
        let call_is_file_class =
            matches!(fr.dispatch, crate::ir::FrDispatch::Static) || staticbound;
        let call_mangle_params = if staticbound {
            fr.target_param_tys.clone()
        } else {
            fr.target_param_tys[first_call_arg..].to_vec()
        };
        let fr_suspend =
            suspend_sig.contains(&(call_owner, fr.call_name.clone(), target_decl_params.len()));
        // Mangling is IDEMPOTENT here. A target from a DEPENDENCY already carries its final JVM name —
        // kotlinc mangled it when that dependency was built, and the lowerer recorded that physical
        // name — so a second pass produced `decode-X4E9McA-X4E9McA`: a method that exists nowhere, and
        // a reflection signature kotlin-reflect cannot resolve. `vc_mangle_once` leaves a name that
        // already carries exactly the suffix this signature would append. (Origin cannot be the test:
        // this pass sees one FILE, so a sibling source file's target looks foreign while its own run
        // does mangle it — skipping there emitted a call to an unmangled method that never exists.)
        let mangle_call_once = |base: &str| {
            vc_mangle_once(
                base,
                &call_mangle_params,
                &fr.target_ret_ty,
                &callable_under,
                call_is_file_class,
                fr_suspend,
            )
        };
        let mangle_reflection_once = |base: &str| {
            vc_mangle_once(
                base,
                &target_decl_params,
                &target_decl_ret,
                &callable_under,
                fr.owner_class.is_none(),
                fr_suspend,
            )
        };
        // A structural adapter invokes an exact generated common-IR function. Its signature has
        // already gone through the function erasure/boxing pass above; reuse that physical ABI
        // verbatim instead of independently erasing the adapter's logical function type.
        let mangled_call_name = local_target
            .as_ref()
            .map(|(name, _, _)| name.clone())
            .unwrap_or_else(|| mangle_call_once(&fr.call_name));
        let reflection_base = fr.reflection_name.as_deref().unwrap_or(&fr.fn_name);
        let mangled_reflection_name = mangle_reflection_once(reflection_base);
        fr.reflection_name =
            (mangled_reflection_name != fr.fn_name).then_some(mangled_reflection_name);
        fr.call_name = mangled_call_name;
        // Preserve classpath erasure already recorded in the target shape.
        let erase_src = fr.target_param_tys.clone();
        let erase_ret = fr.target_ret_ty;
        // A StaticBound receiver that is a VALUE CLASS is captured boxed (`Object`) but the mangled target
        // takes the erased underlying — record it so the emitter unboxes the receiver at `invoke`.
        if staticbound {
            fr.staticbound_recv_unbox = erase_src
                .first()
                .and_then(|t| t.non_null().obj_internal())
                .filter(|fq| callable_under.contains_key(fq));
        }
        if let Some((_, parameters, result)) = local_target {
            fr.target_param_tys = parameters;
            fr.target_ret_ty = result;
        } else {
            fr.target_param_tys = erase_src
                .iter()
                .map(|t| erase(t, &callable_under))
                .collect();
            fr.target_ret_ty = erase(&erase_ret, &callable_under);
        }
        if let Some(parameters) = &mut fr.reflection_target_param_tys {
            for parameter in parameters {
                *parameter = erase(parameter, &callable_under);
            }
        }
        if let Some(result) = &mut fr.reflection_target_ret_ty {
            *result = erase(result, &callable_under);
        }
        let target_offset = usize::from(staticbound);
        fr.unbox_params = fr
            .param_tys
            .iter()
            .enumerate()
            .map(|(i, logical)| {
                let target = fr.target_param_tys.get(i + target_offset)?;
                let fq = logical.non_null().obj_internal()?;
                // `FunctionN.invoke` always receives the logical value as an Object. Adapt it to
                // the selected callable's *physical* parameter, not merely to a type with different
                // nullability. In particular, `Value` passed to `fun f(Value?)` stays the boxed
                // `Value`: the nullable primitive-backed value class is itself the target JVM
                // reference. Unboxing it would call `f(int)` although only `f(Value)` exists.
                (callable_under.contains_key(&fq)
                    && logical != target
                    && target.non_null().obj_internal() != Some(fq))
                .then_some(fq)
            })
            .collect();
        fr.unbox_param_nullable = fr
            .param_tys
            .iter()
            // Null-preserving unboxing depends on what `FunctionN.invoke` may receive. An
            // adapted reference can narrow a nullable declaration parameter to a non-null
            // function parameter; using the declaration's nullability then fabricates a null
            // branch for a primitive target slot and produces an impossible stack-map join.
            .map(|parameter| parameter.is_nullable())
            .collect();
        fr.box_ret = fr.ret_ty.non_null().obj_internal().and_then(|fq| {
            (callable_under.contains_key(&fq)
                && target_decl_ret.non_null().obj_internal() == Some(fq)
                && fr.ret_ty != fr.target_ret_ty)
                .then_some(fq)
        });
        crate::trace_compiler!(
            "value_classes",
            "func_ref {} call_name={} ret_ty={:?} target_ret={:?} box_ret={:?}",
            owner_fq,
            fr.call_name,
            fr.ret_ty,
            fr.target_ret_ty,
            fr.box_ret
        );
    }
    // A boxed value-class object must still implement every selected interface declaration even though
    // its source member has become a static carrier function. Materialize an ordinary instance adapter
    // from the stable override edge and exact implementation identity. Descriptor-changing generic
    // bridges derived earlier remain separate; this is the concrete boxed entry kotlinc emits even when
    // the interface and implementation descriptors are otherwise identical.
    let mut interface_entries = Vec::new();
    for (&owner, edges) in &ir.function_overrides {
        let Some(class) = ir.classes.iter().find(|class| class.fq_name == owner) else {
            continue;
        };
        if !class.is_value {
            continue;
        }
        for edge in edges {
            if edge.implementation_owner != owner || !edge.overridden_is_interface {
                continue;
            }
            let implementation = edge.implementation_function.or_else(|| {
                let crate::fir::ResolvedFunctionOverrideTarget::Module(declaration) =
                    edge.implementation
                else {
                    return None;
                };
                ir.checked_callable_functions.get(&declaration).copied()
            });
            let Some(implementation) = implementation.filter(|function| {
                lowered_value_members.contains(function) && class.methods.contains(function)
            }) else {
                continue;
            };
            let entry = (
                owner,
                implementation,
                edge.name.clone(),
                edge.implementation_parameters.clone(),
                edge.implementation_result,
            );
            if !interface_entries.iter().any(|existing| existing == &entry) {
                interface_entries.push(entry);
            }
        }
    }
    for (owner, implementation, name, parameters, result) in interface_entries {
        let class = ir
            .classes
            .iter_mut()
            .find(|class| class.fq_name == owner)
            .expect("an override edge owner must remain in its IR file");
        let duplicate = class.bridges.iter().any(|bridge| {
            bridge.kind == crate::ir::BridgeKind::ValueClassInterfaceEntry
                && bridge.name == name
                && bridge.erased_params == parameters
                && bridge.erased_ret == result
        });
        if !duplicate {
            class.bridges.push(crate::ir::Bridge {
                kind: crate::ir::BridgeKind::ValueClassInterfaceEntry,
                target_function: Some(implementation),
                name,
                erased_params: parameters.clone(),
                erased_ret: result,
                concrete_params: parameters,
                concrete_ret: result,
                target_ret: None,
                type_safe_barrier: false,
                target_name: None,
                box_ret: None,
                unbox_params: Vec::new(),
            });
        }
    }

    // Exact user value-class members have now been rewritten to static carrier functions. Snapshot
    // those physical signatures before borrowing the class bridge lists; a bridge keeps the stable
    // function identity, so no emitted-name/arity lookup is needed to find its target ABI.
    let lowered_member_targets = lowered_value_members
        .iter()
        .map(|&function| {
            let target = &ir.functions[function as usize];
            (
                function,
                (target.name.clone(), target.params.clone(), target.ret),
            )
        })
        .collect::<HashMap<_, _>>();
    // A covariant-override bridge delegates to the concrete method by name (mangle the target if it was
    // mangled). When the override returns a value class, the concrete method returns the erased underlying,
    // so the bridge boxes the result back to `X` (`box_ret`). Runs even with an empty `mangle_map` — a
    // value-class GETTER bridge (`Child2.prop: Child` through `Base2.prop: Base`) needs the erase+box with
    // no mangling involved.
    {
        for c in &mut ir.classes {
            let owner_is_value = c.is_value;
            let owner_fq = c.fq_name();
            for b in &mut c.bridges {
                let lowered_member_target = b
                    .target_function
                    .and_then(|function| lowered_member_targets.get(&function))
                    .cloned();
                let logical_concrete_params = b.concrete_params.clone();
                let target = b.target_name.clone().unwrap_or_else(|| b.name.clone());
                if let Some(m) =
                    mangle_map.get(&(c.fq_name, target.clone(), b.concrete_params.len()))
                {
                    b.target_name = Some(m.clone());
                }
                let target_mentions_vc = b
                    .concrete_params
                    .iter()
                    .chain(std::iter::once(&b.concrete_ret))
                    .any(|ty| {
                        ty.non_null()
                            .obj_internal()
                            .is_some_and(|name| under.contains_key(&name))
                    });
                let bridge_mentions_vc = b
                    .erased_params
                    .iter()
                    .chain(std::iter::once(&b.erased_ret))
                    .any(|ty| {
                        ty.non_null()
                            .obj_internal()
                            .is_some_and(|name| under.contains_key(&name))
                    });
                if b.target_name.is_none()
                    && target_mentions_vc
                    && b.kind != crate::ir::BridgeKind::PropertyGetter
                {
                    let mangled = vc_mangle(
                        &target,
                        &b.concrete_params,
                        &b.concrete_ret,
                        &under,
                        false,
                        suspend_sig.contains(&(
                            Some(c.fq_name),
                            target.clone(),
                            b.concrete_params.len(),
                        )),
                    );
                    if mangled != target {
                        b.target_name = Some(mangled);
                    }
                }
                crate::trace_compiler!(
                    "value_classes",
                    "bridge {}::{} target={:?} concrete_ret={:?} erased_ret={:?} box_ret={:?}",
                    owner_fq,
                    b.name,
                    b.target_name,
                    b.concrete_ret,
                    b.erased_ret,
                    b.box_ret
                );
                let concrete_ret_vc = match &b.concrete_ret {
                    Ty::Obj(fq_name, _) if under.contains_key(fq_name) => Some(*fq_name),
                    _ => None,
                };
                let erased_ret_vc = b
                    .erased_ret
                    .non_null()
                    .obj_internal()
                    .filter(|fq_name| under.contains_key(fq_name));
                if !owner_is_value && !bridge_mentions_vc {
                    let vc_params: Vec<Option<TypeName>> = b
                        .concrete_params
                        .iter()
                        .zip(b.erased_params.iter())
                        .map(|(concrete, erased)| match concrete {
                            Ty::Obj(fq_name, _)
                                if under.contains_key(fq_name) && is_ref(erased) =>
                            {
                                Some(*fq_name)
                            }
                            _ => None,
                        })
                        .collect();
                    if vc_params.iter().any(Option::is_some) {
                        for parameter in &mut b.concrete_params {
                            *parameter = erase(parameter, &under);
                        }
                        b.unbox_params = vc_params;
                    }
                }
                if let Some(fq_name) = concrete_ret_vc {
                    if b.target_name.is_none() {
                        b.target_name = Some(b.name.clone());
                    }
                    // The bridge satisfies the (mangled) SUPERTYPE method, so it takes that method's
                    // mangled name: `vc_mangle` over the override's params + the SUPERTYPE's declared
                    // return. A VC param (`foo(i: Marker)`) mangles by the param; a literal-VC return
                    // (`fun bar(): Gx`) also mangles by the return; a generic `T` return (erased
                    // `Object`) does not.
                    // A bridge lives on a class (never a file class); its value-class return mangles.
                    if bridge_mentions_vc && b.kind != crate::ir::BridgeKind::PropertyGetter {
                        b.name = vc_mangle(
                            &b.name,
                            &b.concrete_params,
                            &b.erased_ret,
                            &under,
                            false,
                            suspend_sig.contains(&(
                                Some(c.fq_name),
                                b.name.clone(),
                                b.concrete_params.len(),
                            )),
                        );
                    }
                    // A value-class PARAM erases to its underlying in both the bridge descriptor and the
                    // target call (`foo-<hash>(Marker)` → `foo-<hash>(int)`). Done AFTER the mangle,
                    // which keys on the un-erased param type.
                    for p in b
                        .erased_params
                        .iter_mut()
                        .chain(b.concrete_params.iter_mut())
                    {
                        *p = erase(p, &under);
                    }
                    // Whether the SUPERTYPE method returns the value class in its UNBOXED form — a non-null
                    // literal (`fun bar(): Gx`), OR a nullable `X?` whose underlying is a non-null reference
                    // (so `X?` stays UNBOXED, carrying null itself, e.g. `X(val x: Any)`). Then the bridge
                    // returns the erased underlying, NO box. A nullable `X?` that BOXES (over a primitive /
                    // null-capable chain, e.g. `X(val x: Any?)` → `LX;`) or a generic `T` (erased `Object`)
                    // → bridge BOXES the value class back.
                    let supertype_returns_vc =
                        b.erased_ret
                            .non_null()
                            .obj_internal()
                            .is_some_and(|fq_name| {
                                under.contains_key(&fq_name)
                                    && (!b.erased_ret.is_nullable()
                                        || !nullable_is_boxed(fq_name, &under))
                            });
                    let concrete_carrier = erase(&b.concrete_ret, &under);
                    // An EXTERNAL value class (`Result`) is held unboxed (`Object`) everywhere in krusty —
                    // when the SUPERTYPE also carries it unboxed the bridge returns the override's already-
                    // `Object` result directly, NO `box-impl`. EXCEPTION: a GENERIC boundary — the supertype
                    // method returns an erased type variable (`fun performOperation(): T` → `Object`). There
                    // kotlinc materializes the box (`Result.box-impl(Object)Lkotlin/Result;`) so the caller
                    // observes the boxed object (its `toString`/identity), and krusty must match: `box_ret`
                    // references the classpath `box-impl`, exactly like a user value class.
                    if supertype_returns_vc {
                        b.concrete_ret = concrete_carrier;
                        b.erased_ret = b.concrete_ret.clone();
                    } else {
                        b.box_ret = Some(fq_name);
                        b.concrete_ret = concrete_carrier;
                    }
                } else if erased_ret_vc.is_some() {
                    // A bottom/null override (`Nothing`/`Nothing?`) can implement a value-class-returning
                    // member. The concrete target is not itself a value-class return, but the bridge still
                    // satisfies the SUPERTYPE declaration, whose JVM name is mangled by its value-class
                    // return type (`foo(): X?` -> `foo-<hash>()LX;`). Keep the target's source name and
                    // publish the bridge under the mangled supertype name.
                    if b.target_name.is_none() {
                        b.target_name = Some(b.name.clone());
                    }
                    if b.kind != crate::ir::BridgeKind::PropertyGetter {
                        b.name = vc_mangle(
                            &b.name,
                            &b.concrete_params,
                            &b.erased_ret,
                            &under,
                            false,
                            suspend_sig.contains(&(
                                Some(c.fq_name),
                                b.name.clone(),
                                b.concrete_params.len(),
                            )),
                        );
                    }
                    for p in b.erased_params.iter_mut() {
                        *p = erase(p, &under);
                    }
                    let supertype_returns_unboxed_vc = b
                        .erased_ret
                        .non_null()
                        .obj_internal()
                        .is_some_and(|fq_name| {
                            under.contains_key(&fq_name)
                                && (!b.erased_ret.is_nullable()
                                    || !nullable_is_boxed(fq_name, &under))
                        });
                    if supertype_returns_unboxed_vc {
                        b.erased_ret = erase(&b.erased_ret, &under);
                    }
                } else if !owner_is_value && bridge_mentions_vc {
                    // A bridge (mangled `f-<hash>` OR same-name) delegating to a concrete method with a
                    // VALUE-CLASS PARAM, where the bridge's OWN param is the erased-generic `Object`: a
                    // generic supertype method (`I<Result>.foo(T)`) keeps its `foo(Object)` bridge signature,
                    // but the incoming arg is a BOXED `X` (the generic call site boxes). Record each such
                    // param to `checkcast` + `unbox-impl`, then erase the concrete param to its underlying for
                    // the delegated call. A param already AT its underlying (bridge param not a reference —
                    // a primitive-underlying value class) needs no unbox.
                    let vc_params: Vec<Option<TypeName>> = b
                        .concrete_params
                        .iter()
                        .zip(b.erased_params.iter())
                        .map(|(cp, ep)| match cp {
                            Ty::Obj(fq_name, _) if under.contains_key(fq_name) && is_ref(ep) => {
                                Some(*fq_name)
                            }
                            _ => None,
                        })
                        .collect();
                    if vc_params.iter().any(Option::is_some) {
                        for p in b.concrete_params.iter_mut() {
                            *p = erase(p, &under);
                        }
                        b.unbox_params = vc_params;
                    }
                }
                if let Some((target_name, target_params, target_ret)) = lowered_member_target {
                    let physical_params = target_params
                        .get(1..)
                        .expect("a static value-class member target must carry its receiver");
                    assert_eq!(
                        logical_concrete_params.len(),
                        physical_params.len(),
                        "a value-class bridge target must preserve source parameter arity"
                    );
                    b.unbox_params = logical_concrete_params
                        .iter()
                        .zip(&b.erased_params)
                        .zip(physical_params)
                        .map(|((logical, erased), physical)| {
                            logical.non_null().obj_internal().filter(|classifier| {
                                under.contains_key(classifier)
                                    && is_ref(erased)
                                    && physical.non_null().obj_internal() != Some(*classifier)
                            })
                        })
                        .collect();
                    b.concrete_params = physical_params.to_vec();
                    b.target_name = Some(target_name);
                    if target_ret != b.concrete_ret {
                        b.target_ret = Some(target_ret);
                    }
                }
            }
        }
    }

    // 2. Erase class field + ctor-arg types; drop the `<init>` null-check on a constructor parameter
    //    that erased to a non-reference (a value-class ctor arg `a: Na` → `int` can't be null-checked).
    // A NON-value class whose primary ctor has a value-class-typed param gets kotlinc's private-primary +
    // synthetic marker accessor ABI — recorded BEFORE erasure loses the value-class identity of the param.
    let mut value_param_ctors: Vec<(TypeName, Vec<Ty>)> = Vec::new();
    for c in &mut ir.classes {
        if !c.is_value
            && !c.is_object
            && !c.is_interface
            && c.ctor_args.iter().any(|a| is_vc_ty(&a.ty))
        {
            // Capture the DECLARED ctor param types before the erase below rewrites them — the
            // class metadata constructor record must name the value classes.
            value_param_ctors.push((c.fq_name, c.ctor_args.iter().map(|a| a.ty).collect()));
        }
        for fld in &mut c.fields {
            fld.ty = erase(&fld.ty, &under);
        }
        for a in &mut c.ctor_args {
            // Drop the `<init>` null-check on a param that erased to a non-reference, OR whose value-class
            // underlying chain is null-capable (`ZN2(val z: ZN)` where `ZN(val z: Z1?)` → the value can be
            // null, so kotlinc emits no check). Then erase the param type itself.
            if !is_ref(&erase(&a.ty, &under)) || vc_underlying_nullable(&a.ty, &under) {
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
        for sc in &mut c.secondary_ctors {
            // Record the value-class fact BEFORE erasure: it drives kotlinc's private+marker ABI for
            // this constructor (a synthetic marker-disambiguated ctor keeps its own convention).
            if !sc.synthetic && sc.params.iter().any(is_vc_ty) {
                sc.vc_params = true;
            }
            for parameter in &mut sc.prefix_params {
                *parameter = erase(parameter, &under);
            }
            for p in &mut sc.params {
                // A SYNTHETIC marker ctor (the serialization deser ctor, disambiguated by a trailing
                // `DefaultConstructorMarker` rather than `-<hash>` mangling) keeps a nullable-underlying
                // value-class param BOXED — kotlinc can't unbox it there without the mangling.
                if sc.synthetic && vc_underlying_nullable(p, &under) {
                    continue;
                }
                *p = erase(p, &under);
            }
            let target_params = match &mut sc.delegate {
                crate::ir::CtorDelegateTarget::This { target_params, .. }
                | crate::ir::CtorDelegateTarget::Super { target_params, .. } => target_params,
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
        c.bridges.retain(|b| {
            let desc = ir_method_desc(&b.erased_params, &b.erased_ret);
            method_keys.insert((b.name.clone(), desc))
        });
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
    let unbox_receiver_casts: HashSet<u32> = ir
        .exprs
        .iter()
        .filter_map(|e| match e {
            IrExpr::Call {
                callee: Callee::Virtual { name, .. },
                dispatch_receiver: Some(r),
                ..
            } if name == "unbox-impl" => Some(*r),
            _ => None,
        })
        .collect();

    // 3. Erase every type carried inside an expression (locals, casts, vararg/array elements, …).
    //    Inside a value-class member body, an `is X`/`(X)other` whose type IS a value class must stay
    //    the BOXED class (the synthesized `equals` checks/casts the box) — keep it; everything else
    //    (including field-value operations over a nested value-class underlying) erases normally.
    let mut erased_variable_defaults = Vec::new();
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
                ctor_params: Some(ps),
                ..
            } => ps.iter_mut().for_each(|p| *p = erase(p, &under)),
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
    for (init, erased) in erased_variable_defaults {
        if matches!(
            ir.exprs[init as usize],
            IrExpr::Const(crate::ir::IrConst::Null)
        ) {
            ir.exprs[init as usize] =
                IrExpr::Const(crate::ir::IrConst::zero_for_value_type(erased));
        }
    }
    // `exhaustive_whens` drives the emitter's verifier-visible merge type, not Kotlin type checking.
    // Once a value class has been realized as its carrier, the merge frame must use that same carrier
    // (`SampleId?` over a non-null `String` field is `String`, including its null branch). Leaving the
    // semantic classifier here while branch expressions have been erased produces a StackMapTable that
    // claims `LSampleId;` above an actual `String`.
    for result in ir.exhaustive_whens.values_mut() {
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
            || synthesized_value_class_instance_entries.contains(&(fid as u32));
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
    for (cidx, c) in ir.classes.iter().enumerate() {
        // A class's `init { … }` block runs in `<init>` over the unboxed ctor params; a regular class's
        // secondary `<init>` body + `this(…)` args over the secondary params; enum-entry args in `<clinit>`
        // (static, no params); base-class `super(…)` args in the subclass `<init>` over its ctor params.
        if let Some(root) = c.init_body {
            s4_bodies.push((
                root,
                orig_class_init_slots[cidx].clone().unwrap_or_default(),
            ));
        }
        for (sidx, sc) in c.secondary_ctors.iter().enumerate() {
            let slots = orig_secondary_slots[cidx][sidx].clone();
            if let Some(b) = sc.body {
                s4_bodies.push((b, slots.clone()));
            }
            for &statement in &sc.delegate_prelude {
                s4_bodies.push((statement, slots.clone()));
            }
            for &a in &sc.delegate_args {
                s4_bodies.push((a, slots.clone()));
            }
            for &default in sc.defaults.iter().flatten() {
                s4_bodies.push((default, slots.clone()));
            }
        }
        for entry in &c.enum_entries {
            for &a in &entry.args {
                s4_bodies.push((a, HashMap::new()));
            }
        }
        let super_slots = &orig_super_slots[cidx];
        for &statement in &c.super_arg_prelude {
            s4_bodies.push((statement, super_slots.clone()));
        }
        for &argument in &c.super_args {
            s4_bodies.push((argument, super_slots.clone()));
        }
    }
    // Top-level property initializers run in the facade `<clinit>` (static, no params). A value-class
    // construction here (`val p = arrayListOf(X(0))`) must rewrite `new X` → `constructor-impl` too;
    // otherwise a private `<init>` leaks an `IllegalAccessError` from `<clinit>`.
    for (property, slots) in ir.statics.iter().zip(&orig_static_slots) {
        s4_bodies.push((property.init, slots.clone()));
    }
    append_inline_body_scopes(ir, &mut s4_bodies, &slot_types);
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
    let mut value_member_constructor_ops: Vec<(ExprId, BoxOp)> = Vec::new();
    for &id in &targets {
        let body = &s4_bodies[target_slots[&id]];
        let slots = &body.1;
        let repr_ctx = ReprCtx {
            exprs: &ir.exprs,
            rets: &orig_rets,
            fields: &orig_fields,
            slots,
            under: &under,
            types: CallTypes::of(ir),
            physical: &ir.physical_types,
            field_getters: &field_getters,
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
            let fields;
            let params: &[Ty] = match cls_by_name.get(internal) {
                Some(&class) if !orig_fields[class].is_empty() => {
                    fields = orig_fields[class].clone();
                    &fields
                }
                _ => ctor_params.as_deref().unwrap_or(&[]),
            };
            for (&argument, parameter) in args.iter().zip(supplied_constructor_parameters(
                params,
                defaults,
                *default_prefix_count,
            )) {
                let Target::UnboxedX(value_class) = target(parameter, &under) else {
                    continue;
                };
                if is_boxed_vc(
                    &ir.exprs,
                    &ir.functions,
                    &orig_fields,
                    &orig_rets,
                    slots,
                    &under,
                    CallTypes::of(ir),
                    &ir.physical_types,
                    &field_getters,
                    argument,
                    value_class,
                ) && !value_member_constructor_ops
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
                default_boxed_parameters: Vec<(usize, Ty)>,
            },
            /// Same-value-class `==`/`!=` → `equals-impl0(U, U)Z` compared against 0 (kotlinc's ABI).
            VcEq {
                ne: bool,
                lhs: ExprId,
                rhs: ExprId,
                owner: TypeName,
                descriptor: String,
            },
            /// Constructing a value class with its sole (defaulted) param omitted (`Id()`) →
            /// `constructor-impl$default(<underlying>, 1, DefaultConstructorMarker)` — mask `1` because a
            /// value class is single-field. `u` is the erased underlying.
            VcCtorDefault {
                owner: TypeName,
                u: Ty,
            },
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
                // A krusty-unboxed value class has one underlying parameter. Common IR retains the
                // checked omission directly and carries no placeholder value.
                if args.is_empty() && defaults.as_ref() == [0] && *default_prefix_count == 0 {
                    Some(Rw::VcCtorDefault { owner, u })
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
                // has its carrier result stamped separately and does not satisfy this condition.
                && !ir
                    .physical_types
                    .get(arg)
                    .is_some_and(|physical| physical.is_erased_top())
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
                callee: Callee::Virtual { owner, name, .. },
                dispatch_receiver: Some(receiver),
                args,
            } if under.contains_key(owner) => cls_by_name.get(owner).and_then(|class| {
                let expected = format!("{name}-impl");
                ir.classes[*class].methods.iter().copied().find_map(|fid| {
                    let function = ir.functions.get(fid as usize)?;
                    (function.is_static && (function.name == *name || function.name == expected))
                        .then(|| Rw::ImplCall {
                            receiver: *receiver,
                            owner: *owner,
                            name: function.name.clone(),
                            parameters: function.params.clone(),
                            result: function.ret,
                            args: args.iter().copied().map(Some).collect(),
                            extension_receiver: ir.extension_receiver_fns.contains(&fid),
                            default_boxed_parameters: ir
                                .default_stub_boxed_params
                                .get(&fid)
                                .cloned()
                                .unwrap_or_default(),
                        })
                })
            }),
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
            // `a == b` / `a != b` where BOTH operands are the same UNBOXED value class → the class's
            // static `equals-impl0(U, U)Z` compared against 0 (kotlinc's value-class equality ABI;
            // the underlying-level `areEqual`/`icmp` was semantically right but not kotlinc's shape).
            // Identity `===`/`!==` (RefEq/RefNe) is untouched, as are boxed/mixed/null operands —
            // those keep the step-5 boxing decisions.
            IrExpr::PrimitiveBinOp {
                op: op @ (crate::ir::IrBinOp::Eq | crate::ir::IrBinOp::Ne),
                lhs,
                rhs,
            } => {
                let (l, r) = (*lhs, *rhs);
                match (repr_ctx.repr(l), repr_ctx.repr(r)) {
                    (Repr::Unboxed(x), Repr::Unboxed(y)) if x == y => {
                        let u = under.get(&x).map(|t| erase(t, &under)).unwrap_or(Ty::Error);
                        let ud = desc(&u);
                        Some(Rw::VcEq {
                            ne: matches!(op, crate::ir::IrBinOp::Ne),
                            lhs: l,
                            rhs: r,
                            owner: x,
                            descriptor: format!("({ud}{ud})Z"),
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
                    default_boxed_parameters: ir
                        .default_stub_boxed_params
                        .get(&fid)
                        .cloned()
                        .unwrap_or_default(),
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
                Some(expr)
            }
            Some(Rw::VcCtorDefault { owner, u }) => {
                // `constructor-impl$default(<underlying dummy>, mask=1, DefaultConstructorMarker=null)`:
                // the stub fills the omitted param from the class's default; the dummy underlying is a
                // zero/null placeholder, the marker a trailing `null`.
                let ud = desc(&u);
                let marker = "Lkotlin/jvm/internal/DefaultConstructorMarker;";
                let dummy = ir.add_expr(IrExpr::Const(crate::ir::IrConst::zero_for_value_type(
                    u.canonical_semantic(),
                )));
                let mask = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(1)));
                let null_marker = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Null));
                ir.record_erased_value_construction(id, owner, u);
                Some(IrExpr::Call {
                    callee: Callee::Static {
                        owner,
                        name: "constructor-impl$default".to_string(),
                        descriptor: format!("({ud}I{marker}){ud}"),
                        inline: InlineKind::None,
                    },
                    dispatch_receiver: None,
                    args: vec![dummy, mask, null_marker],
                })
            }
            Some(Rw::Prop {
                receiver,
                owner,
                result,
            }) => {
                sole_property_coercions.insert(id);
                ir.logical_types.insert(id, result);
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
                    &under,
                    &orig_fields,
                    &orig_rets,
                    slots,
                    &field_getters,
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
                default_boxed_parameters,
            }) => {
                // This rewrite replaces an instance-shaped semantic call with the exact static
                // carrier implementation. Any earlier property/call stamp described the pre-rewrite
                // box; publish the implementation result now so a following sole-property read does
                // not try to unbox a carrier that is already primitive/reference-underlying.
                ir.physical_types.insert(id, result);
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
                let inferred_boxed = is_boxed_vc(
                    &ir.exprs,
                    &ir.functions,
                    &orig_fields,
                    &orig_rets,
                    slots,
                    &under,
                    CallTypes::of(ir),
                    &ir.physical_types,
                    &field_getters,
                    receiver,
                    owner,
                );
                let receiver = if inferred_boxed {
                    let underlying = under
                        .get(&owner)
                        .map(|ty| erase(ty, &under))
                        .unwrap_or(Ty::Error);
                    ir.add_expr(IrExpr::Call {
                        callee: Callee::Virtual {
                            owner,
                            name: "unbox-impl".to_string(),
                            descriptor: format!("(){}", desc(&underlying)),
                            params: None,
                            interface: false,
                        },
                        dispatch_receiver: Some(receiver),
                        args: Vec::new(),
                    })
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
                descriptor,
            }) => {
                let call = ir.add_expr(IrExpr::Call {
                    callee: Callee::Static {
                        owner,
                        name: "equals-impl0".to_string(),
                        descriptor,
                        inline: InlineKind::None,
                    },
                    dispatch_receiver: None,
                    args: vec![lhs, rhs],
                });
                let zero = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(0)));
                // `a == b` ⇒ `equals-impl0(a, b) != 0`; `a != b` ⇒ `== 0`. Both fuse to a single
                // `ifne`/`ifeq` on the call result in branch position — kotlinc's exact shape.
                Some(IrExpr::PrimitiveBinOp {
                    op: if ne {
                        crate::ir::IrBinOp::Eq
                    } else {
                        crate::ir::IrBinOp::Ne
                    },
                    lhs: call,
                    rhs: zero,
                })
            }
            None => None,
        };
        if let Some(r) = rewrite {
            ir.exprs[i] = r;
        }
    }

    // Property rewrites above can introduce a fresh `checkcast X` receiver for `X.unbox-impl()`.
    // Preserve those casts just like the unbox receivers that existed before expression-type erasure.
    let unbox_receiver_casts: HashSet<u32> = unbox_receiver_casts
        .into_iter()
        .chain(ir.exprs.iter().filter_map(|e| match e {
            IrExpr::Call {
                callee: Callee::Virtual { name, .. },
                dispatch_receiver: Some(receiver),
                ..
            } if name == "unbox-impl" => Some(*receiver),
            _ => None,
        }))
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
    // `(type-op expr, underlying)` — casts to NULLABLE reference-underlying value classes and
    // representation-changing implicit coercions are retargeted to the physical carrier. There is
    // no box-class instance at either boundary, so retaining the semantic value-class operand would
    // emit a wrong `checkcast` after the required unbox operation.
    let mut retarget: Vec<(ExprId, Ty)> = Vec::new();
    // Each body to box/unbox: every non-value-class-member function body (with its captured slot types),
    // plus every class `init { … }` block (slots = `this` + the ctor params), so a value-class member
    // call / boundary INSIDE an init block (`class B(val a: A) { init { a.f() } }`) is boxed too.
    let mut bodies: Vec<(ExprId, HashMap<u32, Ty>)> = Vec::new();
    // `fid` indexes two parallel vecs (`ir.functions` and `slot_types`), so the range loop is wanted.
    #[allow(clippy::needless_range_loop)]
    for fid in 0..ir.functions.len() {
        crate::trace_compiler!(
            "value_classes",
            "boundary body fid={fid} name={} value_member={} body={:?}",
            ir.functions[fid].name,
            vc_methods.contains(&(fid as u32)),
            ir.functions[fid].body
        );
        if vc_methods.contains(&(fid as u32)) && !lowered_value_members.contains(&(fid as u32)) {
            continue;
        }
        if let Some(root) = ir.functions[fid].body {
            bodies.push((root, slot_types[fid].clone()));
        }
        if let Some(defaults) = ir.param_defaults(fid as u32) {
            for &root in defaults.iter().flatten() {
                bodies.push((root, slot_types[fid].clone()));
            }
        }
    }
    for (cidx, c) in ir.classes.iter().enumerate() {
        if let Some(root) = c.init_body {
            bodies.push((
                root,
                orig_class_init_slots[cidx].clone().unwrap_or_default(),
            ));
        }
        // A regular class's secondary `<init>` body + its `this(…)` delegation args run over the secondary
        // params — box/unbox their value-class accesses/constructions.
        for (sidx, sc) in c.secondary_ctors.iter().enumerate() {
            let slots = orig_secondary_slots[cidx][sidx].clone();
            if let Some(b) = sc.body {
                bodies.push((b, slots.clone()));
            }
            for &statement in &sc.delegate_prelude {
                bodies.push((statement, slots.clone()));
            }
            for &a in &sc.delegate_args {
                bodies.push((a, slots.clone()));
            }
            for &default in sc.defaults.iter().flatten() {
                bodies.push((default, slots.clone()));
            }
        }
        // Base-class constructor args run in the subclass `<init>` over its primary ctor params.
        let super_slots = &orig_super_slots[cidx];
        for &statement in &c.super_arg_prelude {
            bodies.push((statement, super_slots.clone()));
        }
        for &argument in &c.super_args {
            bodies.push((argument, super_slots.clone()));
        }
    }
    // Top-level property initializers (facade `<clinit>`, static) — box/unbox their value-class accesses
    // and boundary constructions just like any function body.
    for (property, slots) in ir.statics.iter().zip(&orig_static_slots) {
        bodies.push((property.init, slots.clone()));
    }
    append_inline_body_scopes(ir, &mut bodies, &slot_types);
    for (root, slots) in &bodies {
        let root = *root;
        let repr_ctx = ReprCtx {
            exprs: &ir.exprs,
            rets: &orig_rets,
            fields: &orig_fields,
            slots,
            under: &under,
            types: CallTypes::of(ir),
            physical: &ir.physical_types,
            field_getters: &field_getters,
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
                        ops.push((
                            *arg,
                            if type_operand.is_nullable() {
                                BoxOp::UnboxNull(source)
                            } else {
                                BoxOp::Unbox(source)
                            },
                        ));
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
                    if let Some(crate::ir::IrValueClassSuspendResult::Carrier(boundary)) =
                        ir.value_class_suspend_calls.get(arg).copied()
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
                        ops.push((
                            *arg,
                            if type_operand.is_nullable() {
                                BoxOp::UnboxNull(target)
                            } else {
                                BoxOp::Unbox(target)
                            },
                        ));
                        retarget.push((id, erase(&under[&target], &under)));
                        continue;
                    }
                }
                // A checked cast/smart cast from an ordinary reference supertype (`Any`, an
                // interface, or an unstamped erased property read) to a non-null value class yields
                // the BOX object. The target position wants the carrier, so realize the same
                // cast-plus-`unbox-impl` boundary as the explicitly stamped generic-call case above.
                // Kotlin has no implicit conversion from an unrelated concrete value to a value
                // class; consequently a non-null `NotVc` operand under this checked coercion is a
                // boxed value-class reference, never a raw carrier discovered by guesswork.
                if let Target::UnboxedX(target) = target(type_operand, &under) {
                    if matches!(repr_ctx.repr(*arg), Repr::NotVc)
                        && !repr_ctx.operand_null_only(*arg)
                    {
                        ops.push((*arg, BoxOp::Unbox(target)));
                        retarget.push((id, erase(&under[&target], &under)));
                        continue;
                    }
                }
                // The sole-field coercion (`w.v` → `ImplicitCoercion(<w>, U)`) over a BOXED receiver
                // (`w!!` of a boxed `W?` shared cell): unbox the receiver first — otherwise the
                // emitter coerces the box reference straight to the underlying (`checkcast Integer`
                // on a `W` → CCE).
                if let (Some(x), true) = (
                    match repr_ctx.repr(*arg) {
                        Repr::Boxed(x) => Some(x),
                        _ => None,
                    },
                    type_operand.non_null().obj_internal().is_none()
                        || !under.contains_key(&type_operand.non_null().obj_internal().unwrap()),
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
            // The nullable-Any toString declaration consumes a reference. Other intrinsics carry
            // concrete scalar/array contracts and must not turn their receiver into an erased box.
            if let IrExpr::Call {
                callee:
                    Callee::Intrinsic {
                        operation: crate::ir::IrIntrinsic::NullableAnyToString,
                        ..
                    },
                dispatch_receiver: Some(recv),
                ..
            } = &ir.exprs[id as usize]
            {
                if let Repr::Unboxed(x) = repr_ctx.repr(*recv) {
                    ops.push((*recv, repr_ctx.box_op(*recv, x)));
                }
            }
            // A virtual/interface dispatch on an UNBOXED value-class receiver must box it — the dispatch
            // needs the boxed object. Two cases: (1) the owner is NOT the value class (an INTERFACE it
            // implements — an `IFoo by Z(x)` delegation forwarder), or (2) the owner IS the value class and
            // the callee is a SIBLING-FILE user instance method (`params: Some`; krusty emits a value
            // class's own user methods as boxed-`this` instance methods). A same-file member call takes the
            // index-resolved `MethodCall` path (boxed above); the value class's static `-impl`s are
            // `Callee::Static`, not `Virtual`, so they never reach here.
            if let IrExpr::Call {
                callee: Callee::Virtual { owner, params, .. },
                dispatch_receiver: Some(recv),
                ..
            } = &ir.exprs[id as usize]
            {
                if !is_value_class_internal(*owner, &under) || params.is_some() {
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
            // The String-plus argument is the declaration's `Any?` operand and therefore boxes an
            // unboxed value class. Dynamic invokes, reference varargs, and string templates are the
            // other erased reference boundaries handled here.
            if let IrExpr::Call {
                callee:
                    Callee::Intrinsic {
                        operation: crate::ir::IrIntrinsic::StringPlus,
                        ..
                    },
                args,
                ..
            }
            | IrExpr::InvokeFunction { args, .. }
            | IrExpr::Vararg { elements: args, .. }
            // A value-class part of a string template flows into `StringBuilder.append(Object)` /
            // `String.valueOf(Object)`, so it must box (→ the value class's `toString`).
            | IrExpr::StringConcat(args) = &ir.exprs[id as usize]
            {
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
                        ops.push((a, repr_ctx.box_op(a, x)));
                    }
                }
            }
            // A value class flowing into a resolved classpath call (`KProperty1.get(Object)`, a stdlib
            // method) is boxed at each REFERENCE parameter the descriptor declares. Calls OWNED by a
            // value class (its own `-impl`/mangled members) take the underlying — never box those.
            if let IrExpr::Call {
                callee:
                    Callee::Virtual {
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
                    }
                    | Callee::Special {
                        owner,
                        name,
                        descriptor,
                        ..
                    },
                args,
                ..
            } = &ir.exprs[id as usize]
            {
                // A call OWNED by a value class (its own `-impl`/mangled members) takes the underlying at
                // most parameters — never box those. EXCEPT when a parameter's declared type is itself a
                // BOXED value class (`ZN.constructor-impl(LZ1;)`, where `ZN`'s underlying `Z1?` boxes):
                // there the unboxed `Z1` arg must box to `LZ1;`. So for a VC-owned call, box an arg only
                // when its param descriptor is exactly `Lx;` for the arg's value class `x`.
                let vc_owned = is_value_class_internal(*owner, &under);
                // `box-impl` is the representation adapter itself: its argument is already this
                // value class's unboxed carrier. For an `Object`-underlying class (notably
                // `Result`) treating that descriptor as an erased generic slot recursively boxes
                // the carrier, producing `box-impl(box-impl(carrier))`.
                if vc_owned && name == "box-impl" {
                    continue;
                }
                let refs = descriptor_param_refs(descriptor);
                let ptypes = descriptor_param_types(descriptor);
                #[cfg(feature = "trace")]
                if crate::trace::enabled("value_classes") {
                    if let IrExpr::Call { callee, args, .. } = &ir.exprs[id as usize] {
                        let nm = match callee {
                            Callee::Static { name, .. }
                            | Callee::Virtual { name, .. }
                            | Callee::Special { name, .. } => name.as_str(),
                            _ => "?",
                        };
                        if nm.contains("getOrThrow") || nm.contains("throwOnFailure") {
                            let a0 = args.first().map(|&a| match repr_ctx.repr(a) {
                                Repr::Unboxed(_) => "Unboxed",
                                Repr::Boxed(_) => "Boxed",
                                Repr::NotVc => "NotVc",
                            });
                            crate::trace_compiler!(
                                "value_classes",
                                "call {owner}.{nm} vc_owned={vc_owned} arg0_repr={a0:?}"
                            );
                        }
                    }
                }
                for (k, a) in args.clone().into_iter().enumerate() {
                    // The selected provider declaration is authoritative when it retained a source
                    // parameter for this slot. This resolves the descriptor's irreducible `Object`
                    // ambiguity without a value-class/name special case: a direct `Result<T>`
                    // parameter wants the carrier, while a declaration type parameter wants a box.
                    if let Some(parameter) = ir
                        .call_declared_params
                        .get(&id)
                        .and_then(|parameters| parameters.get(k))
                        .copied()
                    {
                        let (value, _) = repr_ctx.through_erased_generic_coercion(a);
                        record_value_boundary(
                            &mut ops, &ir.exprs, &repr_ctx, value, parameter, &under,
                        );
                        continue;
                    }
                    // The RECEIVER (`args[0]`) of a value-class extension facade call takes the value class's
                    // OWN underlying (`getOrThrow-impl(Object)` for `Result`), so it passes UNBOXED — the
                    // dedicated `ext_call_source_receiver` handling above owns it. Never box it here, even
                    // though its `Object` param would otherwise look like a generic boxed slot.
                    if recv_is_ref_vc && k == 0 {
                        continue;
                    }
                    let (representation_value, representation) =
                        repr_ctx.through_erased_generic_coercion(a);
                    let Repr::Unboxed(x) = representation else {
                        continue;
                    };
                    // A reference parameter boxes an unboxed value-class argument unless that parameter
                    // is exactly the value class's own concrete carrier. This is independent of who owns
                    // the callable: a value-class `*-impl` can still declare an ordinary interface
                    // parameter, and that slot must receive the box implementing the interface. An erased
                    // `Object` carrier remains ambiguous and therefore boxes; exact provider declaration
                    // types took the authoritative path above.
                    let under_desc = under.get(&x).map(|u| desc(&erase(u, &under)));
                    let own_underlying = ptypes.get(k).map(String::as_str) == under_desc.as_deref()
                        && under_desc.as_deref() != Some("Ljava/lang/Object;");
                    let box_here = refs.get(k).copied().unwrap_or(false) && !own_underlying;
                    if box_here {
                        ops.push((
                            representation_value,
                            repr_ctx.box_op(representation_value, x),
                        ));
                    }
                }
            }
            // Each `(value expr, target type)` boundary in this expression.
            let pairs: Vec<(ExprId, Ty)> = match &ir.exprs[id as usize] {
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
                    let fields;
                    let targets: &[Ty] = match cls_by_name.get(internal) {
                        Some(&c) if !orig_fields[c].is_empty() => {
                            fields = orig_fields[c].clone();
                            &fields
                        }
                        _ => ctor_params.as_deref().unwrap_or(&[]),
                    };
                    args.iter()
                        .zip(supplied_constructor_parameters(
                            targets,
                            defaults,
                            *default_prefix_count,
                        ))
                        .map(|(a, p)| (*a, p.clone()))
                        .collect()
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
                // Adapt each one against the selected declaration parameter that remains after
                // removing omitted ordinals; JVM placeholders are created only after this pass.
                IrExpr::Call {
                    callee:
                        Callee::ModuleWithDefaults {
                            target, defaults, ..
                        },
                    args,
                    ..
                } => ir
                    .referenced_module_callables
                    .get(target)
                    .map(|callable| {
                        args.iter()
                            .zip(callable.parameters.iter().enumerate().filter_map(
                                |(parameter, ty)| {
                                    (!defaults.contains(&(parameter as u32))).then_some(*ty)
                                },
                            ))
                            .map(|(argument, parameter)| {
                                (
                                    repr_ctx.through_erased_generic_coercion(*argument).0,
                                    parameter,
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
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
                                Some((a.as_ref().copied()?, params.get(i)?.clone()))
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
                    Some(t) => vec![(*v, t.clone())],
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
                IrExpr::RefNew { init, .. } => match orig_ref_elems.get(&id) {
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
            rets: &orig_rets,
            fields: &orig_fields,
            slots: &slot_types[function],
            under: &under,
            types: CallTypes::of(ir),
            physical: &ir.physical_types,
            field_getters: &field_getters,
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
                rets: &orig_rets,
                fields: &orig_fields,
                slots: &orig_super_slots[class_index],
                under: &under,
                types: CallTypes::of(ir),
                physical: &ir.physical_types,
                field_getters: &field_getters,
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
                    Some(crate::ir::IrValueClassSuspendResult::Carrier(carrier))
                        if carrier.canonical_semantic()
                            == erase(&under[&x], &under).canonical_semantic()
                ) =>
            {
                // The erased CPS method descriptor says `Object`, but the continuation carries the
                // already-unboxed representation recorded for this exact call. A boundary collected
                // from the pre-CPS descriptor must not insert a value-class `unbox-impl` around it.
            }
            BoxOp::Unbox(x) => unbox_wrap(ir, id, x, &under),
            BoxOp::UnboxNull(x) => {
                unbox_wrap_nullable(ir, id, x, &under, fresh);
                fresh += 1;
            }
            BoxOp::Narrow(x) => narrow_wrap(ir, id, x),
        }
    }

    // A top-level / companion property backing field of value-class type is stored BOXED (`LX;`); its
    // initializer — which step 4 rewrote to `constructor-impl(…)` (the unboxed underlying) — must be
    // `box-impl`'d to match the field's boxed slot, exactly like a function boxing a value-class return.
    // `box_tail` only boxes an unboxed `constructor-impl`/`unbox-impl` tail, so an already-boxed init is
    // left untouched.
    for si in 0..ir.statics.len() {
        if let Some(x) = ir.statics[si]
            .ty
            .non_null()
            .obj_internal()
            .filter(|fq| under.contains_key(fq))
        {
            let root = ir.statics[si].init;
            box_tail(ir, root, x, &under);
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
                    &under,
                    &orig_rets,
                    &orig_fields,
                    &slot_types[fid],
                    &field_getters,
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
        } else if let Ty::Obj(fq_name, _) = &orig_rets[fid] {
            // A function returning the value class ITSELF (`fun test(): Z = a?.foo()!!`) whose tail is a
            // BOXED value (the `!!` of a nullable safe-call yields a boxed `Z`) must `unbox-impl` it — the
            // erased return is the underlying.
            let x = *fq_name;
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
                            box_ref_tail(
                                ir,
                                body,
                                x,
                                &under,
                                &orig_rets,
                                &orig_fields,
                                &slot_types[fid],
                                &field_getters,
                            );
                        }
                        Some(crate::ir::IrValueClassSuspendResult::Carrier(carrier)) => {
                            ir.functions[fid].ret = carrier;
                            // A safe coroutine primitive produces `T` through the generic
                            // `SafeContinuation<T>` slot, so a value-class `T` is boxed even when this
                            // suspend declaration's selected CPS boundary is its raw reference carrier.
                            // Convert that exact boxed tail once; ordinary carrier-producing tails are
                            // already unboxed and remain unchanged.
                            unbox_tail(
                                ir,
                                body,
                                x,
                                &under,
                                &orig_rets,
                                &orig_fields,
                                &slot_types[fid],
                                &field_getters,
                            );
                        }
                        None => unreachable!("the return was already identified as a value class"),
                    }
                }
            } else if under.contains_key(&x) {
                if let Some(body) = ir.functions[fid].body {
                    unbox_tail(
                        ir,
                        body,
                        x,
                        &under,
                        &orig_rets,
                        &orig_fields,
                        &slot_types[fid],
                        &field_getters,
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
                &callable_under,
                &orig_rets,
                &orig_fields,
                &slot_types[impl_fn as usize],
                &field_getters,
            );
        } else {
            // A lambda returning `Any`/an interface (not a value class itself) still boxes a value-class tail.
            box_vc_tail(ir, body, &callable_under, &orig_rets, false);
        }
    }
    for body in inline_bodies {
        box_vc_tail(ir, body, &callable_under, &orig_rets, false);
    }

    // A property whose declared type is a VALUE CLASS has a `@JvmName`-mangled accessor. The backend
    // synthesizes the accessors for a plain property and cannot know the value classes, so stamp the
    // mangled spelling onto the declaration here, where the map exists.
    for ci in 0..ir.classes.len() {
        let props: Vec<(usize, String, Ty)> = ir.classes[ci]
            .properties
            .iter()
            .enumerate()
            // Only a property the backend SYNTHESIZES an accessor for needs a stamped name. An abstract
            // or interface property keeps a real IR method, which this pass mangles like any other.
            .filter(|(_, p)| p.getter.is_none() && p.backing_field.is_some())
            .map(|(i, p)| (i, p.name.clone(), p.ty))
            .collect();
        for (index, name, ty) in props {
            let is_vc_ty = |t: &Ty| {
                t.non_null()
                    .obj_internal()
                    .is_some_and(|fq_name| under.contains_key(&fq_name))
            };
            // An OVERRIDE must carry the name its supertype's accessor has: `override val p: Nothing?`
            // over an `Inlined?` property is spelled with the supertype's mangled name, or the interface
            // call finds no implementation. So the mangling type is the supertype's when it has one.
            // The supertype's accessor has ALREADY been mangled by this pass (it is a real IR method), so
            // take its exact name rather than recomputing the hash — the override and its bridge must
            // match byte for byte or the interface call finds no implementation.
            let plain = property_getter_name(&name);
            let supers: Vec<TypeName> = ir.classes[ci]
                .supertypes
                .iter()
                .filter_map(|st| st.non_null().obj_internal())
                .chain(ir.classes[ci].interfaces.iter_ids())
                .collect();
            let super_accessor = supers
                .iter()
                .filter_map(|st| ir.classes.iter().find(|sc| sc.fq_name == *st))
                .filter(|sc| sc.properties.iter().any(|sp| sp.name == name))
                .flat_map(|sc| sc.methods.iter())
                .map(|&fid| ir.functions[fid as usize].name.clone())
                .find(|n| n.strip_prefix(&plain).is_some_and(|r| r.starts_with('-')));
            crate::trace_compiler!(
                "value_classes",
                "prop stamp {}.{} super_accessor={super_accessor:?} vc_ty={}",
                ir.classes[ci].fq_name.render(),
                name,
                is_vc_ty(&ty)
            );
            if super_accessor.is_none() && !is_vc_ty(&ty) {
                continue;
            }
            // The property's OWN accessor is mangled only when its own type is a value class. When it
            // merely overrides a value-class property (`override val p: Nothing?`), the own accessor keeps
            // the plain spelling and it is the BRIDGE that carries the supertype's mangled name.
            let own_mangled =
                is_vc_ty(&ty).then(|| vc_mangle(&plain, &[], &ty, &under, false, false));
            let getter = own_mangled.clone().unwrap_or_else(|| plain.clone());
            let setter = vc_mangle(
                &crate::names::property_setter_name(&name),
                std::slice::from_ref(&ty),
                &Ty::Unit,
                &under,
                false,
                false,
            );
            // Any call already built against the PLAIN spelling (a plugin emits `getX()` before this pass
            // runs) must move to the mangled one too — this is the single place that decides the name.
            let owner = ir.classes[ci].fq_name;
            let plain_getter = property_getter_name(&name);
            let plain_setter = crate::names::property_setter_name(&name);
            for e in ir.exprs.iter_mut() {
                if let IrExpr::Call {
                    callee:
                        Callee::Virtual {
                            owner: call_owner,
                            name: call_name,
                            ..
                        },
                    ..
                } = e
                {
                    if *call_owner != owner {
                        continue;
                    }
                    if *call_name == plain_getter {
                        *call_name = getter.clone();
                    } else if *call_name == plain_setter {
                        *call_name = setter.clone();
                    }
                }
            }
            // A bridge delegating to the accessor must target the mangled spelling too — an unmangled
            // `getProp()Bse` bridge over a value-class property calls `getProp-<hash>()I`.
            for bridge in ir.classes[ci].bridges.iter_mut() {
                let target = bridge
                    .target_name
                    .as_deref()
                    .unwrap_or(&bridge.name)
                    .to_string();
                if target == plain_getter {
                    if let Some(super_name) = super_accessor.clone() {
                        // The property OVERRIDES a value-class one: the bridge IS the supertype's mangled
                        // accessor, delegating to this class's own (plain) one.
                        bridge.name = super_name;
                        bridge.target_name = Some(getter.clone());
                    } else {
                        bridge.target_name = Some(getter.clone());
                    }
                } else if target == plain_setter {
                    bridge.target_name = Some(setter.clone());
                }
            }
            let p = &mut ir.classes[ci].properties[index];
            p.getter_jvm_name = own_mangled;
            p.setter_jvm_name = is_vc_ty(&ty).then_some(setter);
        }
    }

    // Property references cross the erased `KProperty` Object boundary. A value-class property accessor
    // itself uses the mangled name and carrier descriptor, but `get` must box that carrier and `set` must
    // unbox the incoming value-class object. Record that JVM realization on the already-selected target;
    // no declaration lookup or overload resolution happens here.
    for class in &mut ir.classes {
        let Some(reference) = class.prop_ref.as_mut() else {
            continue;
        };
        let Some(value_class) = reference
            .prop_ty
            .non_null()
            .obj_internal()
            .filter(|name| callable_under.contains_key(name))
        else {
            continue;
        };
        // A specialized generic property (`Pair<UInt, _>::first`) has semantic type `UInt`, but its
        // selected declaration still exposes `getFirst(): Object`. That object is already the boxed
        // value and the declaration is not value-class-mangled. Only a descriptor returning this
        // value class's actual carrier denotes a concrete value-class property accessor.
        if reference
            .getter_descriptor
            .as_ref()
            .is_some_and(|descriptor| {
                let physical_ret = descriptor.rsplit_once(')').map(|(_, ret)| ret);
                let carrier = callable_under
                    .get(&value_class)
                    .map(|underlying| desc(&erase(underlying, &callable_under)));
                physical_ret
                    .zip(carrier.as_deref())
                    .is_some_and(|(ret, carrier)| ret != carrier)
            })
        {
            continue;
        }
        reference.boxed_value_class = Some(value_class);
        reference.getter_name = vc_mangle(
            &property_getter_name(&reference.prop_name),
            &[],
            &reference.prop_ty,
            &callable_under,
            false,
            false,
        );
        if let Some(descriptor) = reference.getter_descriptor.as_mut() {
            *descriptor = erase_descriptor(descriptor, &callable_under);
        }
        if reference.setter_name.is_some() {
            reference.setter_name = Some(vc_mangle(
                &crate::names::property_setter_name(&reference.prop_name),
                std::slice::from_ref(&reference.prop_ty),
                &Ty::Unit,
                &callable_under,
                false,
                false,
            ));
        }
        if let Some(descriptor) = reference.setter_descriptor.as_mut() {
            *descriptor = erase_descriptor(descriptor, &callable_under);
        }
    }

    true
}

/// Box an unboxed value-class result at every tail position of `id` (recursing `when`/block/return
/// tails). `prim_only` (the lambda `() -> T` case) boxes only a primitive-underlying result — a
/// reference one already satisfies the erased `Object`; the `Any`-return case (`prim_only = false`)
/// boxes any, so an `is X`/`as X` on the result holds.
fn box_vc_tail(ir: &mut IrFile, id: ExprId, under: &Under, rets: &[Ty], prim_only: bool) {
    match &ir.exprs[id as usize] {
        IrExpr::When { branches } => {
            let rs: Vec<ExprId> = branches.iter().map(|(_, r)| *r).collect();
            for r in rs {
                box_vc_tail(ir, r, under, rets, prim_only);
            }
        }
        IrExpr::Block { value: Some(v), .. } => {
            let v = *v;
            box_vc_tail(ir, v, under, rets, prim_only);
        }
        // A statement-only block (`{ … ; return x }`) tails on its last statement.
        IrExpr::Block { value: None, stmts } => {
            if let Some(&last) = stmts.last() {
                box_vc_tail(ir, last, under, rets, prim_only);
            }
        }
        IrExpr::Return(Some(v)) => {
            let v = *v;
            box_vc_tail(ir, v, under, rets, prim_only);
        }
        // A supertype return-coercion (`make(): W` → `Any?`) wraps the value — box the INNER value, so
        // the coercion then just widens the boxed `X` (a no-op), rather than boxing the coercion result.
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } if !prim_only => {
            let arg = *arg;
            box_vc_tail(ir, arg, under, rets, prim_only);
        }
        _ => {
            if let Some(x) = unboxed_vc_class(&ir.exprs, rets, under, id, !prim_only) {
                if ir.has_external_value_class_name(x) {
                    return;
                }
                let prim = under
                    .get(&x)
                    .map(|u| !is_ref(&erase(u, under)))
                    .unwrap_or(false);
                if !prim_only || prim {
                    box_wrap(ir, id, x, under);
                }
            }
        }
    }
}

/// The value class an expr produces UNBOXED (a `constructor-impl`/`unbox-impl` result, or a local call
/// whose return type is a non-null value class), if any.
fn unboxed_vc_class(
    exprs: &[IrExpr],
    rets: &[Ty],
    under: &Under,
    id: ExprId,
    calls: bool,
) -> Option<TypeName> {
    match &exprs[id as usize] {
        IrExpr::Call {
            callee: Callee::Static { owner, name, .. },
            ..
        } if name == "constructor-impl" || name == "unbox-impl" => value_class_name(*owner, under),
        // A local call returning an unboxed value class — only considered when `calls` is set (the
        // `Any`-return case); the lambda case must NOT box these (they already satisfy `Object`).
        IrExpr::Call { callee, .. } if calls && callee.source_function().is_some() => match rets
            .get(
                callee
                    .source_function()
                    .expect("guarded same-file function call") as usize,
            ) {
            Some(Ty::Obj(fq_name, _)) if under.contains_key(fq_name) => Some(*fq_name),
            _ => None,
        },
        IrExpr::Block { value: Some(v), .. } => unboxed_vc_class(exprs, rets, under, *v, calls),
        IrExpr::NotNullAssert { operand, .. } if calls => {
            unboxed_vc_class(exprs, rets, under, *operand, calls)
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum BoxOp {
    Box(TypeName),
    BoxNull(TypeName),
    Unbox(TypeName),
    UnboxNull(TypeName),
    Narrow(TypeName),
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

struct ReprCtx<'a> {
    exprs: &'a [IrExpr],
    rets: &'a [Ty],
    fields: &'a [Vec<Ty>],
    slots: &'a HashMap<u32, Ty>,
    under: &'a Under,
    types: CallTypes<'a>,
    physical: &'a HashMap<u32, Ty>,
    field_getters: &'a FieldGetters,
}

impl ReprCtx<'_> {
    fn repr(&self, id: ExprId) -> Repr {
        repr(
            self.exprs,
            self.rets,
            self.fields,
            self.slots,
            self.under,
            self.types,
            self.physical,
            self.field_getters,
            id,
        )
    }

    fn operand_nonnull(&self, id: ExprId) -> bool {
        operand_nonnull(self.exprs, self.rets, self.fields, self.slots, id)
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
                    ops.push((
                        tail,
                        if parameter.is_nullable() {
                            BoxOp::UnboxNull(value_class)
                        } else {
                            BoxOp::Unbox(value_class)
                        },
                    ));
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
                    ops.push((
                        value,
                        if parameter.is_nullable() {
                            BoxOp::UnboxNull(value_class)
                        } else {
                            BoxOp::Unbox(value_class)
                        },
                    ));
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
        .map(|u| !is_ref(&erase(u, under)) || underlying_null_capable(u, under))
        .unwrap_or(false)
}

/// Whether a value class's unboxed representation can hold `null` — true when ANY level of the nested
/// underlying chain is declared nullable (`X(val v: Int?)`; `ZN(val z: Z1?)` → `ZN2(val z: ZN)` null-capable
/// through `Z1?`). `erase` collapses a nullable-over-non-null-reference to a non-null underlying, so this
/// walks the UNERASED chain to see the `?` erasure drops.
fn underlying_null_capable(t: &Ty, under: &Under) -> bool {
    if t.is_nullable() {
        return true;
    }
    match t.obj_internal() {
        Some(fq_name) => under
            .get(&fq_name)
            .is_some_and(|u| underlying_null_capable(u, under)),
        None => false,
    }
}

/// Whether a NON-NULL value-class type's unboxed underlying can hold null (so a `checkNotNullParameter`
/// on it would wrongly reject a legal value). True when the value class's field type erases to a
/// nullable reference (`X(val v: Int?)` → `Integer`; `X(val v: String?)` → `String?`).
fn vc_underlying_nullable(t: &Ty, under: &Under) -> bool {
    if let Ty::Obj(fq_name, _) = t {
        if let Some(u) = under.get(fq_name) {
            return underlying_null_capable(u, under);
        }
    }
    false
}

/// Whether the value the expr at `id` produces is statically NON-NULL — so boxing it (`box-impl`) can't
/// hit the value class's non-null ctor check. A construction/`!!`/non-nullable slot or return qualifies.
fn operand_nonnull(
    exprs: &[IrExpr],
    rets: &[Ty],
    fields: &[Vec<Ty>],
    slots: &HashMap<u32, Ty>,
    id: ExprId,
) -> bool {
    let non_null_ty = |t: &Ty| matches!(t, Ty::Obj(..));
    match &exprs[id as usize] {
        IrExpr::New { .. } => true,
        // A read of a non-nullable field yields a non-null value (a `val a: X` data-class property is
        // never null — box it with the plain `box-impl`, no null guard).
        IrExpr::GetField { class, index, .. } => fields
            .get(*class as usize)
            .and_then(|fs| fs.get(*index as usize))
            .is_some_and(non_null_ty),
        // The same read as a property: its declared type is what says whether the value can be null.
        IrExpr::PropertyRead { ty, .. } => non_null_ty(ty),
        IrExpr::NotNullAssert { .. } => true,
        // A successful cast to a non-null reference has a non-null result; `CastNonNull` states the
        // same contract directly. This matters for a non-null generic value class whose unboxed
        // carrier itself may contain null (`Ag<T>(null)` is still not a null `Ag<T>`).
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::Cast,
            type_operand,
            ..
        } => non_null_ty(type_operand),
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::CastNonNull,
            ..
        } => true,
        IrExpr::Call {
            callee: Callee::Static { name, .. },
            ..
        } if name == "constructor-impl" || name == "box-impl" => true,
        IrExpr::Call { callee, .. } if callee.source_function().is_some() => rets
            .get(
                callee
                    .source_function()
                    .expect("guarded same-file function call") as usize,
            )
            .is_some_and(non_null_ty),
        IrExpr::GetValue(i) => slots.get(i).is_some_and(non_null_ty),
        IrExpr::Block { value: Some(v), .. } => operand_nonnull(exprs, rets, fields, slots, *v),
        _ => false,
    }
}

/// Whether a checked value has Kotlin's null-only bottom type. This is representation evidence,
/// not data-flow inference: it follows only common-IR-transparent wrappers and declared slot/call
/// result types already fixed by the frontend.
fn operand_null_only(exprs: &[IrExpr], rets: &[Ty], slots: &HashMap<u32, Ty>, id: ExprId) -> bool {
    let null_only_ty = |ty: &Ty| *ty == Ty::Null || ty.non_null() == Ty::Nothing;
    match &exprs[id as usize] {
        IrExpr::Const(crate::ir::IrConst::Null) => true,
        IrExpr::GetValue(slot) => slots.get(slot).is_some_and(null_only_ty),
        IrExpr::Call { callee, .. } if callee.source_function().is_some() => rets
            .get(callee.source_function().expect("guarded source function") as usize)
            .is_some_and(null_only_ty),
        IrExpr::TypeOp { arg, .. } => operand_null_only(exprs, rets, slots, *arg),
        IrExpr::Block {
            value: Some(value), ..
        } => operand_null_only(exprs, rets, slots, *value),
        _ => false,
    }
}

/// The two per-expression type facts lowering hands the representation analysis. They always travel
/// together, and neither alone can classify a value-class result: `logical` says WHICH value class a
/// coerced read has after substitution, `declared` says whether the callee RETURNS one by declaration
/// (so the physical result is its erased carrier) rather than merely producing one out of a generic
/// slot (where it is a box). `List<TokenBox>.get` and `A.create(): A<String>` agree on the
/// first and differ only on the second.
#[derive(Clone, Copy)]
struct CallTypes<'a> {
    logical: &'a HashMap<u32, Ty>,
    declared: &'a HashMap<u32, Ty>,
}

impl<'a> CallTypes<'a> {
    fn of(ir: &'a IrFile) -> Self {
        CallTypes {
            logical: &ir.logical_types,
            declared: &ir.call_declared_ret,
        }
    }

    fn get(&self, id: &u32) -> Option<&Ty> {
        self.logical.get(id)
    }

    /// The value class this call returns BY DECLARATION — so its physical result is already the erased
    /// carrier and must not be unboxed again. `None` when the callee declares no class return, or
    /// declares one that is not a value class here.
    fn declared_value_class(&self, id: u32, under: &Under) -> Option<TypeName> {
        self.declared
            .get(&id)?
            .non_null()
            .obj_internal()
            .filter(|fq| under.contains_key(fq))
    }
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
#[allow(clippy::too_many_arguments)]
fn repr(
    exprs: &[IrExpr],
    rets: &[Ty],
    fields: &[Vec<Ty>],
    slots: &HashMap<u32, Ty>,
    under: &Under,
    types: CallTypes<'_>,
    physical: &HashMap<u32, Ty>,
    field_getters: &FieldGetters,
    id: ExprId,
) -> Repr {
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
            let structural = repr(
                exprs,
                rets,
                fields,
                slots,
                under,
                types,
                physical,
                field_getters,
                *value,
            );
            if !matches!(structural, Repr::NotVc) {
                return structural;
            }
        }
        return Repr::Boxed(classifier);
    }
    match &exprs[id as usize] {
        // Top-level and companion property storage keeps a value-class box. Unlike an ordinary
        // instance field (which is erased to the carrier below), its static declaration retains the
        // semantic value-class type and the emitter uses that boxed descriptor.
        IrExpr::GetStatic(_) => types
            .get(&id)
            .and_then(|ty| ty.non_null().obj_internal())
            .filter(|classifier| under.contains_key(classifier))
            .map_or(Repr::NotVc, Repr::Boxed),
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
        IrExpr::PropertyRead { ty, .. } => {
            if physical.get(&id).is_some_and(|ty| ty.is_erased_top()) {
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
            repr_of_ty(&types.declared[&id], under)
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
            match repr(
                exprs,
                rets,
                fields,
                slots,
                under,
                types,
                physical,
                field_getters,
                *arg,
            ) {
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
        IrExpr::NotNullAssert { operand, .. } => repr(
            exprs,
            rets,
            fields,
            slots,
            under,
            types,
            physical,
            field_getters,
            *operand,
        ),
        // Reading a captured mutable local through its `Ref` holder: its representation is that of the
        // boxed element type (`var res: Result<T>?` → a boxed `Result`).
        IrExpr::RefGet { elem, .. } => repr_of_ty(elem, under),
        IrExpr::Block { value: Some(v), .. } => repr(
            exprs,
            rets,
            fields,
            slots,
            under,
            types,
            physical,
            field_getters,
            *v,
        ),
        // A `when`/safe-call selects one of its branch values (`s?.foo()` → `when { s!=null -> foo(s);
        // else -> null }`): its representation is a value-producing branch's — the FIRST branch that is a
        // value class, so a boxed value-class result flowing out of a `?.` is recognized (the `null`
        // default branch is `NotVc` and skipped).
        IrExpr::When { branches } => branches
            .iter()
            .map(|(_, v)| {
                repr(
                    exprs,
                    rets,
                    fields,
                    slots,
                    under,
                    types,
                    physical,
                    field_getters,
                    *v,
                )
            })
            .find(|r| !matches!(r, Repr::NotVc))
            .unwrap_or(Repr::NotVc),
        // A function value's `invoke` returns its declared type through the `FunctionN` `Object` slot — a
        // value-class result is therefore the BOXED value class (the callable-ref adapter / lambda tail box
        // it). So a `.member` on the result unboxes it.
        IrExpr::InvokeFunction { ret, .. } => match ret.non_null().obj_internal() {
            Some(fq) if under.contains_key(&fq) => Repr::Boxed(fq),
            _ => Repr::NotVc,
        },
        // A call not matched by the value-class-specific arms above — a LIBRARY call whose logical result
        // type the lowerer recorded. Its representation depends on whether the PHYSICAL return is the value
        // class's own UNDERLYING or a generic-erased `Object`: `runCatching{…}: Result` physically returns
        // `Object` = `Result`'s underlying → the UNBOXED value class; a generic `decode(): TO = IC` returns
        // `Object` ≠ `IC`'s `double` underlying → a BOXED value class (it sat in a type-parameter slot).
        IrExpr::Call { callee, .. } => {
            // A callee that returns a value class BY DECLARATION hands back its erased CARRIER: that is
            // the whole classpath value-class RETURN ABI (`fun make(): K` → `make-<hash>()
            // Ljava/lang/String;`), and it holds whatever the underlying erases to — so it settles the
            // `Object`-underlying cases the descriptor comparison below cannot. Checked FIRST for
            // exactly that reason: `A.create(): A<String>` and `List<TokenBox>.get` both spell
            // `()Ljava/lang/Object;`, and only the declaration says the first is a carrier and the
            // second a box. Nullable declared returns are never recorded (they really are boxed).
            if let Some(declared) = types.declared_value_class(id, under) {
                return Repr::Unboxed(declared);
            }
            let Some(t) = types.get(&id) else {
                return Repr::NotVc;
            };
            let Some(x) = t
                .non_null()
                .obj_internal()
                .filter(|fq| under.contains_key(fq))
            else {
                return Repr::NotVc;
            };
            let phys_ret = match callee {
                Callee::Virtual {
                    params: Some((_, ret)),
                    ..
                } => Some(desc(ret)),
                Callee::Static { descriptor, .. }
                | Callee::Virtual { descriptor, .. }
                | Callee::Special { descriptor, .. } => {
                    descriptor.rsplit(')').next().map(str::to_string)
                }
                _ => None,
            };
            let u_desc = desc(&erase(&under[&x], under));
            if phys_ret.as_deref() == Some(u_desc.as_str()) {
                repr_of_ty(t, under)
            } else {
                Repr::Boxed(x)
            }
        }
        // A value-class GETTER / member read (statically `S<T>` though its erased form is `Object`) whose
        // SUBSTITUTED static type the lowerer recorded: repr it by that logical type, so a redundant `Cast`
        // wrapping an already-unboxed value class strips. Scoped to `MethodCall` — a getter — so it does
        // not reinterpret other erased nodes.
        _ => Repr::NotVc,
    }
}

/// Replace the expr at `id` with `(X)<orig>.unbox-impl()` — checkcast then unbox a boxed `X`.
fn unbox_wrap(ir: &mut IrFile, id: ExprId, x: TypeName, under: &Under) {
    let new_id = clone_expr_with_type_facts(ir, id);
    let cast = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::TypeOp {
        op: crate::ir::IrTypeOp::Cast,
        arg: new_id,
        type_operand: boxed_value_ty(x),
    });
    let u = under.get(&x).map(|t| erase(t, under)).unwrap_or(Ty::Error);
    let d = desc(&u);
    ir.exprs[id as usize] = IrExpr::Call {
        callee: Callee::Virtual {
            owner: x,
            name: "unbox-impl".to_string(),
            descriptor: format!("(){d}"),
            params: None,
            interface: false,
        },
        dispatch_receiver: Some(cast),
        args: vec![],
    };
    // `id` used to denote the erased reference call cloned above. It now denotes the result of
    // `unbox-impl`, so its physical fact must change with the node instead of continuing to claim
    // that the primitive carrier on the operand stack is `Object`.
    ir.physical_types.insert(id, u);
}

/// Preserve the expression's physical/logical facts while moving it below a representation wrapper.
fn clone_expr_with_type_facts(ir: &mut IrFile, id: ExprId) -> ExprId {
    let new_id = ir.exprs.len() as ExprId;
    ir.exprs.push(ir.exprs[id as usize].clone());
    if let Some(ty) = ir.physical_types.get(&id).copied() {
        ir.physical_types.insert(new_id, ty);
    }
    if let Some(ty) = ir.logical_types.get(&id).copied() {
        ir.logical_types.insert(new_id, ty);
    }
    if let Some(ty) = ir.property_declaration_types.get(&id).copied() {
        ir.property_declaration_types.insert(new_id, ty);
    }
    // The representation wrapper is not itself the selected suspension. Move that identity to the
    // cloned original so CPS appends the continuation to the call, then realizes the wrapper around
    // the call's synchronous/resumed result.
    if let Some(result) = ir.suspend_calls.remove(&id) {
        ir.suspend_calls.insert(new_id, result);
    }
    if let Some(result) = ir.value_class_suspend_calls.remove(&id) {
        ir.value_class_suspend_calls.insert(new_id, result);
    }
    if let Some(result) = ir.intrinsic_suspension_points.remove(&id) {
        ir.intrinsic_suspension_points.insert(new_id, result);
    }
    new_id
}

/// Replace an erased-reference expression with an explicit cast to its known boxed value class.
fn narrow_wrap(ir: &mut IrFile, id: ExprId, x: TypeName) {
    let arg = clone_expr_with_type_facts(ir, id);
    ir.exprs[id as usize] = IrExpr::TypeOp {
        op: crate::ir::IrTypeOp::Cast,
        arg,
        type_operand: boxed_value_ty(x),
    };
}

fn unbox_wrap_nullable(ir: &mut IrFile, id: ExprId, x: TypeName, under: &Under, slot: u32) {
    let orig_id = clone_expr_with_type_facts(ir, id);
    let boxed_ty = Ty::nullable(Ty::obj_name(x));
    let var = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::Variable {
        index: slot,
        ty: boxed_ty,
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
    let get_for_unbox = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::GetValue(slot));
    let u = under.get(&x).map(|t| erase(t, under)).unwrap_or(Ty::Error);
    let d = desc(&u);
    let unboxed = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::Call {
        callee: Callee::Virtual {
            owner: x,
            name: "unbox-impl".to_string(),
            descriptor: format!("(){d}"),
            params: None,
            interface: false,
        },
        dispatch_receiver: Some(get_for_unbox),
        args: vec![],
    });
    let when = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::When {
        branches: vec![(Some(is_null), null2), (None, unboxed)],
    });
    ir.exprs[id as usize] = IrExpr::Block {
        stmts: vec![var],
        value: Some(when),
    };
}

/// Build a sole-property access `x.v`: identity (`Block` yielding the receiver) when the receiver is an
/// unboxed value, or `receiver.unbox-impl()` when it is a boxed `X` (e.g. from a nullable-returning
/// function).
#[allow(clippy::too_many_arguments)]
fn prop_access(
    ir: &mut IrFile,
    receiver: ExprId,
    x: TypeName,
    result: Ty,
    under: &Under,
    fields: &[Vec<Ty>],
    rets: &[Ty],
    slots: &HashMap<u32, Ty>,
    field_getters: &FieldGetters,
) -> IrExpr {
    let u = under.get(&x).map(|t| erase(t, under)).unwrap_or(Ty::Error);
    // Use the same representation analysis as every other boundary. The resulting coercion tells later
    // analysis that the property itself has the underlying representation.
    let inferred_boxed = is_boxed_vc(
        &ir.exprs,
        &ir.functions,
        fields,
        rets,
        slots,
        under,
        CallTypes::of(ir),
        &ir.physical_types,
        field_getters,
        receiver,
        x,
    );
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
        let d = desc(&u);
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
        ir.add_expr(IrExpr::Call {
            callee: Callee::Virtual {
                owner: x,
                name: "unbox-impl".to_string(),
                descriptor: format!("(){d}"),
                params: None,
                interface: false,
            },
            dispatch_receiver: Some(dispatch),
            args: vec![],
        })
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

/// Whether the expr at `id` produces a BOXED value-class `x` object: a `box-impl` result, a call whose
/// return type is `X` (a nullable-over-primitive value class stays boxed), or a `!!`/identity over one.
#[allow(clippy::too_many_arguments)]
fn is_boxed_vc(
    exprs: &[IrExpr],
    funcs: &[crate::ir::IrFunction],
    fields: &[Vec<Ty>],
    rets: &[Ty],
    slots: &HashMap<u32, Ty>,
    under: &Under,
    types: CallTypes<'_>,
    physical: &HashMap<u32, Ty>,
    field_getters: &FieldGetters,
    id: ExprId,
    x: TypeName,
) -> bool {
    let x_rendered = x.render();
    let is_x = |t: &Ty| t.non_null().obj_internal().is_some_and(|n| n == x);
    if physical.get(&id).is_some_and(is_x) {
        return true;
    }
    if types.get(&id).is_some_and(is_x)
        && physical.get(&id).is_some_and(|ty| ty.is_erased_top())
        && types.declared_value_class(id, under) != Some(x)
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
        IrExpr::PropertyRead { ty, .. } => {
            (is_x(ty) && physical.get(&id).is_some_and(|ty| ty.is_erased_top()))
                || matches!(repr_of_ty(ty, under), Repr::Boxed(c) if c == x)
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
        } => descriptor.ends_with(&format!("L{x_rendered};")),
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
        // `(X)v` self-cast `ir_lower` inserts): there the cast is identity (step 5 strips it) and the
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
                && (is_boxed_vc(
                    exprs,
                    funcs,
                    fields,
                    rets,
                    slots,
                    under,
                    types,
                    physical,
                    field_getters,
                    *arg,
                    x,
                ) || !matches!(repr(exprs, rets, fields, slots, under, types, physical, field_getters, *arg), Repr::Unboxed(c) if c == x))
        }
        IrExpr::NotNullAssert { operand, .. } => is_boxed_vc(
            exprs,
            funcs,
            fields,
            rets,
            slots,
            under,
            types,
            physical,
            field_getters,
            *operand,
            x,
        ),
        // A `when` whose non-null branch yields a boxed `x` (a nullable safe-call: `box-impl` vs `null`) is
        // a boxed `x`.
        IrExpr::When { branches } => branches.iter().any(|(_, r)| {
            is_boxed_vc(
                exprs,
                funcs,
                fields,
                rets,
                slots,
                under,
                types,
                physical,
                field_getters,
                *r,
                x,
            )
        }),
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
            if matches!(target(type_operand, under), Target::UnboxedX(target) if target == x) {
                false
            } else {
                is_boxed_vc(
                    exprs,
                    funcs,
                    fields,
                    rets,
                    slots,
                    under,
                    types,
                    physical,
                    field_getters,
                    *arg,
                    x,
                )
            }
        }
        IrExpr::Block { value: Some(v), .. } => is_boxed_vc(
            exprs,
            funcs,
            fields,
            rets,
            slots,
            under,
            types,
            physical,
            field_getters,
            *v,
            x,
        ),
        _ => false,
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
/// At a function's return tail (recursing `return`/block tails), `unbox-impl` a BOXED value-class value so
/// it matches the function's erased (underlying) return type — `fun f(): Z = a?.foo()!!` returns the box.
#[allow(clippy::too_many_arguments)]
fn unbox_tail(
    ir: &mut IrFile,
    id: ExprId,
    x: TypeName,
    under: &Under,
    rets: &[Ty],
    fields: &[Vec<Ty>],
    slots: &HashMap<u32, Ty>,
    field_getters: &FieldGetters,
) {
    if ir.physical_types.get(&id).is_some_and(|ty| {
        ty.non_null()
            .obj_internal()
            .is_some_and(|classifier| classifier == x)
    }) {
        unbox_wrap(ir, id, x, under);
        return;
    }
    match &ir.exprs[id as usize] {
        IrExpr::Return(Some(v)) | IrExpr::Block { value: Some(v), .. } => {
            let v = *v;
            unbox_tail(ir, v, x, under, rets, fields, slots, field_getters);
        }
        IrExpr::Block { value: None, stmts } => {
            if let Some(&last) = stmts.last() {
                unbox_tail(ir, last, x, under, rets, fields, slots, field_getters);
            }
        }
        _ => {
            if is_boxed_vc(
                &ir.exprs,
                &ir.functions,
                fields,
                rets,
                slots,
                under,
                CallTypes::of(ir),
                &ir.physical_types,
                field_getters,
                id,
                x,
            ) {
                unbox_wrap(ir, id, x, under);
            }
        }
    }
}

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

/// Box the tail of `id` to value class `X` for a REFERENCE-slot return (a lambda's `Object`-returning
/// `invoke`): recurse `when`/block/`return` tails, and box any tail value that is not ALREADY a boxed `X`.
/// Unlike [`box_tail`] (which only boxes the syntactic `constructor-impl`/`unbox-impl` forms), this boxes
/// EVERY unboxed tail — `this`, a captured field, a library call returning the unboxed underlying — since
/// the declared value-class return `X` fixes what the box must be. Uniform across all value classes.
#[allow(clippy::too_many_arguments)]
fn box_ref_tail(
    ir: &mut IrFile,
    id: ExprId,
    x: TypeName,
    under: &Under,
    rets: &[Ty],
    fields: &[Vec<Ty>],
    slots: &HashMap<u32, Ty>,
    field_getters: &FieldGetters,
) {
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
                box_ref_tail(ir, r, x, under, rets, fields, slots, field_getters);
            }
        }
        IrExpr::Block { value: Some(v), .. } => {
            let v = *v;
            box_ref_tail(ir, v, x, under, rets, fields, slots, field_getters);
        }
        IrExpr::Block { value: None, stmts } => {
            if let Some(&last) = stmts.last() {
                box_ref_tail(ir, last, x, under, rets, fields, slots, field_getters);
            }
        }
        IrExpr::Return(Some(v)) => {
            let v = *v;
            box_ref_tail(ir, v, x, under, rets, fields, slots, field_getters);
        }
        _ => {
            // Already a boxed `X` (a `box-impl` result, a call/slot typed `X`, a `?.`-`when` box) → leave it;
            // otherwise the tail is the unboxed underlying and must be boxed to `X`.
            if !is_boxed_vc(
                &ir.exprs,
                &ir.functions,
                fields,
                rets,
                slots,
                under,
                CallTypes::of(ir),
                &ir.physical_types,
                field_getters,
                id,
                x,
            ) {
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
#[allow(clippy::too_many_arguments)]
fn box_nullable_vc_tail(
    ir: &mut IrFile,
    id: ExprId,
    x: TypeName,
    under: &Under,
    rets: &[Ty],
    fields: &[Vec<Ty>],
    slots: &HashMap<u32, Ty>,
    field_getters: &FieldGetters,
    is_tail: bool,
) {
    let recur = |ir: &mut IrFile, e: ExprId, t: bool| {
        box_nullable_vc_tail(ir, e, x, under, rets, fields, slots, field_getters, t)
    };
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
                    repr(
                        &ir.exprs,
                        rets,
                        fields,
                        slots,
                        under,
                        CallTypes::of(ir),
                        &ir.physical_types,
                        field_getters,
                        id,
                    ),
                    Repr::Unboxed(c) if c == x
                );
                let already_boxed = is_boxed_vc(
                    &ir.exprs,
                    &ir.functions,
                    fields,
                    rets,
                    slots,
                    under,
                    CallTypes::of(ir),
                    &ir.physical_types,
                    field_getters,
                    id,
                    x,
                );
                if (logical_is_x || repr_unboxed_x) && !already_boxed {
                    box_wrap(ir, id, x, under);
                }
            }
        }
    }
}

/// Replace the expr at `id` with `box-impl(<original expr at id>)`.
fn box_wrap(ir: &mut IrFile, id: ExprId, x: TypeName, under: &Under) {
    let new_id = clone_expr_with_type_facts(ir, id);
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
    let orig_id = clone_expr_with_type_facts(ir, id);
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

fn erase(t: &Ty, under: &Under) -> Ty {
    if let Some(fq_name) = t.non_null().obj_internal() {
        let nullable = t.is_nullable();
        if let Some(u) = under.get(&fq_name) {
            // A non-null `X` always erases to its underlying. A nullable `X?` erases ONLY when it is NOT
            // boxed (`nullable_is_boxed` is the single source of truth — over a non-null reference that
            // carries `null` itself); otherwise it stays the boxed `X` so `X(null)` ≠ `null`. Delegating
            // keeps erasure consistent with the box/unbox analysis for arbitrarily nested chains.
            if !nullable || !nullable_is_boxed(fq_name, under) {
                return erase(u, under);
            }
        }
    }
    *t
}

/// Select the physical result carried through a suspend function's erased `Object` boundary.
///
/// The declared type remains the semantic identity used for overloads and metadata. This target pass
/// records only how that already-selected value crosses CPS. Scalar and null-capable carriers require a
/// box; a non-null reference carrier crosses directly unless an exact override edge requires the concrete
/// value-class identity at the supertype boundary. Nullable value classes keep their ordinary erasure.
fn suspend_result_representation(
    declared: &Ty,
    under: &Under,
    force_boxed: bool,
) -> Option<crate::ir::IrValueClassSuspendResult> {
    let classifier = declared
        .non_null()
        .obj_internal()
        .filter(|classifier| under.contains_key(classifier))?;
    let carrier = erase(declared, under);
    if !declared.is_nullable() && (force_boxed || nullable_is_boxed(classifier, under)) {
        Some(crate::ir::IrValueClassSuspendResult::Boxed {
            classifier,
            carrier,
        })
    } else {
        Some(crate::ir::IrValueClassSuspendResult::Carrier(carrier))
    }
}

/// Whether the erased type occupies a JVM *reference* slot. A non-null Kotlin primitive class
/// (`kotlin/Int`, `kotlin/Boolean`, …) emits as a JVM primitive (`I`, `Z`, …), so it is NOT a
/// reference; its NULLABLE form is the boxed wrapper (`Integer`), which is. Everything else that is a
/// `Class` is a reference.
fn is_ref(t: &Ty) -> bool {
    if t.is_nullable() {
        return true;
    }
    // A Kotlin type parameter always occupies an erased JVM reference slot, even when its upper
    // bound names a primitive-like Kotlin class. Treating `T` as non-reference loses the boxing
    // boundary in `Holder<T>(value: T)` and stores an unboxed value-class carrier as `Integer`
    // instead of the value class's boxed wrapper.
    if matches!(t.non_null(), Ty::TyParam(..)) {
        return true;
    }
    // A JVM scalar (`Int`/`Long`/… AND the unsigned `UInt`/`ULong`, which are unboxed primitives) is NOT a
    // reference. Check this FIRST — `kotlin_class_internal(UInt)` is "kotlin/UInt" but `unboxed_primitive`
    // only knows the signed wrappers, so the descriptor check below would misclassify it as a reference.
    if t.is_jvm_scalar() {
        return false;
    }
    // A FUNCTION type realizes as a `FunctionN` object and an array as its array class — both are
    // references with no `kotlin_class_internal`, and the `None => false` fallback below silently
    // stripped their `checkNotNullParameter` guards (kotlinc guards a `block: () -> Unit` like any
    // other non-null reference parameter).
    if matches!(t, Ty::Fun(_)) || t.is_array() {
        return true;
    }
    // `kotlin_class_internal` (not `obj_internal`): a bare `Ty::String` variant is a REFERENCE but has no
    // `obj_internal()` — treating it as a non-reference makes `nullable_is_boxed` think a `String`-backed
    // value class is primitive-like (`Str?` wrongly boxed instead of unboxed to `String?`).
    match t.kotlin_class_internal() {
        Some(fq_name) => Ty::obj(&fq_name.render()).unboxed_primitive().is_none(),
        None => false,
    }
}

/// Each parameter type of a JVM method descriptor `(…)ret` as its descriptor string (`I`, `LZ1;`,
/// `[Ljava/lang/String;`, …) — used to box an unboxed value class only at a `Lx;`-typed parameter.
fn descriptor_param_types(descriptor: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = descriptor.as_bytes();
    let Some(end) = descriptor.find(')') else {
        return out;
    };
    let mut i = 1;
    while i < end {
        let start = i;
        while i < end && bytes[i] == b'[' {
            i += 1;
        }
        if i < end && bytes[i] == b'L' {
            while i < end && bytes[i] != b';' {
                i += 1;
            }
            i += 1;
        } else {
            i += 1;
        }
        out.push(descriptor[start..i].to_string());
    }
    out
}

/// Whether each parameter of a JVM method descriptor `(…)ret` is a reference type (`L…;` or `[…`).
fn descriptor_param_refs(descriptor: &str) -> Vec<bool> {
    let mut out = Vec::new();
    let bytes = descriptor.as_bytes();
    let Some(end) = descriptor.find(')') else {
        return out;
    };
    let mut i = 1;
    while i < end {
        match bytes[i] {
            b'[' => {
                out.push(true);
                i += 1;
                while i < end && bytes[i] == b'[' {
                    i += 1;
                }
                if i < end && bytes[i] == b'L' {
                    while i < end && bytes[i] != b';' {
                        i += 1;
                    }
                }
                i += 1;
            }
            b'L' => {
                out.push(true);
                while i < end && bytes[i] != b';' {
                    i += 1;
                }
                i += 1;
            }
            b'J' | b'D' => {
                out.push(false);
                i += 1;
            }
            _ => {
                out.push(false);
                i += 1;
            }
        }
    }
    out
}

/// Synthesize a value class's unboxed-support members directly in the IR (a JVM concern, so it lives in
/// this pass, NOT `ir_lower`): `unbox-impl`/`box-impl`/`constructor-impl`/`equals-impl0` plus structural
/// `equals`/`hashCode`/`toString` (skipped where the user defined one). The plain single-field class
/// (field, `<init>`, getter) is already emitted by `ir_lower`.
fn synth_value_members(
    ir: &mut IrFile,
    class_id: u32,
    under: &Under,
    has_init: bool,
    constructor_default: Option<ExprId>,
    synthesized_instance_entries: &mut HashSet<u32>,
) -> bool {
    let internal = ir.classes[class_id as usize].fq_name();
    let fname = ir.classes[class_id as usize].fields[0].name.clone();
    let internal_name = type_name(&internal);
    let u_ir = under.get(&internal_name).copied().unwrap_or(Ty::Error);
    // The FULLY-ERASED underlying: a NESTED value class erases through its chain to the first type that
    // stops unboxing — `NZ2(NZ1)` where `NZ1(Z?)` erases to a BOXED `Z` (`LZ;`), not `LNZ1;`. The static
    // `-impl` members take this erased type (matching kotlinc), so their hardcoded delegation descriptors
    // must use it too, or the operand-stack type won't match the actual method signature (a VerifyError).
    let eu = erase(&u_ir, under);
    // The underlying JVM descriptor (`Ljava/lang/String;`, `I`, `LZ;`, …) — the argument type of the
    // static `-impl` members, which the instance methods delegate to (matching kotlinc's value-class shape).
    let udesc = type_descriptor(ir_ty_to_jvm(&eu));
    let x_ir = Ty::obj(&internal);
    let bool_ir = Ty::obj("kotlin/Boolean");
    let int_ir = Ty::obj("kotlin/Int");
    let str_ir = Ty::obj("kotlin/String");
    let any_ir = Ty::obj("kotlin/Any");

    // A value class's declared accessors are static implementations over its carrier. The source IR
    // initially represents a computed accessor like every ordinary member (implicit receiver in slot
    // zero); replacing that receiver with an explicit carrier parameter preserves every body value id
    // while giving emission and metadata the ABI kotlinc exposes.
    let computed_accessors = ir.classes[class_id as usize]
        .properties
        .iter()
        .enumerate()
        .filter(|(_, property)| property.backing_field.is_none())
        .map(|(index, property)| {
            (
                index,
                property.getter,
                property.setter,
                property.name.clone(),
                property.is_open,
            )
        })
        .collect::<Vec<_>>();
    for (property_index, getter, setter, property_name, overrides_supertype) in computed_accessors {
        if let Some(getter) = getter {
            let source_name = property_getter_name(&property_name);
            let jvm_name = format!("{}-impl", property_getter_name(&property_name));
            let ret = {
                let function = &mut ir.functions[getter as usize];
                function.name.clone_from(&jvm_name);
                function.params.insert(0, u_ir);
                function.is_static = true;
                function.ret
            };
            ir.classes[class_id as usize].properties[property_index].getter_jvm_name =
                Some(jvm_name.clone());
            // The static `getX-impl(U)` is how an unboxed value calls its accessor. If the property
            // overrides a supertype declaration, the BOX must additionally implement the ordinary
            // virtual `getX()` entry point. Delegate from that instance method to the same implementation;
            // bridge derivation has already established the override, so no hierarchy lookup occurs here.
            if overrides_supertype {
                let this = ir.add_expr(IrExpr::GetValue(0));
                let carrier = ir.add_expr(IrExpr::GetField {
                    receiver: this,
                    class: class_id,
                    index: 0,
                });
                let call = ir.add_expr(IrExpr::Call {
                    callee: Callee::Static {
                        owner: internal_name,
                        name: jvm_name,
                        descriptor: format!("({}){}", desc(&u_ir), desc(&ret)),
                        inline: crate::libraries::InlineKind::None,
                    },
                    dispatch_receiver: None,
                    args: vec![carrier],
                });
                let returned = ir.add_expr(IrExpr::Return(Some(call)));
                let body = ir.add_expr(IrExpr::Block {
                    stmts: vec![returned],
                    value: None,
                });
                let fid = ir.add_fun(crate::ir::IrFunction {
                    name: source_name,
                    params: vec![],
                    ret,
                    body: Some(body),
                    is_static: false,
                    dispatch_receiver: Some(internal_name),
                    param_checks: vec![],
                });
                ir.classes[class_id as usize].methods.push(fid);
                synthesized_instance_entries.insert(fid);
                ir.open_methods.insert(fid);
            }
        }
        if let Some(setter) = setter {
            let jvm_name = format!(
                "{}-impl",
                crate::names::property_setter_name(&property_name)
            );
            let function = &mut ir.functions[setter as usize];
            function.name.clone_from(&jvm_name);
            function.params.insert(0, u_ir);
            function.is_static = true;
            ir.classes[class_id as usize].properties[property_index].setter_jvm_name =
                Some(jvm_name);
        }
    }

    // User-written Any overrides become the static `-impl` body. Its former receiver slot 0 is exactly
    // the first static parameter slot, so the body itself needs no slot rewrite; value-class property
    // reads inside it are lowered to the carrier later in this pass. The ordinary instance override is
    // synthesized below as the ABI delegator back to this implementation.
    let mut custom_equals = false;
    let mut custom_hash_code = false;
    let mut custom_to_string = false;
    for &fid in &ir.classes[class_id as usize].methods.clone() {
        let Some(function) = ir.functions.get_mut(fid as usize) else {
            continue;
        };
        let custom_impl = match (function.name.as_str(), function.params.as_slice()) {
            ("equals", [_]) => {
                custom_equals = true;
                Some("equals-impl")
            }
            ("hashCode", []) => {
                custom_hash_code = true;
                Some("hashCode-impl")
            }
            ("toString", []) => {
                custom_to_string = true;
                Some("toString-impl")
            }
            _ => None,
        };
        if let Some(name) = custom_impl {
            function.name = name.to_string();
            function.params.insert(0, u_ir);
            function.is_static = true;
        }
    }

    let add_static = |ir: &mut IrFile, name: &str, params: Vec<Ty>, ret: Ty, body: ExprId| -> u32 {
        let fid = ir.add_fun(crate::ir::IrFunction {
            name: name.to_string(),
            params,
            ret,
            body: Some(body),
            is_static: true,
            dispatch_receiver: Some(internal_name),
            param_checks: Vec::new(),
        });
        ir.classes[class_id as usize].methods.push(fid);
        fid
    };
    let add_inst =
        |ir: &mut IrFile, name: &str, params: Vec<Ty>, ret: Ty, body: ExprId| -> Option<u32> {
            // Don't synthesize over a user-defined member of the same name.
            let exists = ir.classes[class_id as usize]
                .methods
                .iter()
                .any(|&m| ir.functions.get(m as usize).is_some_and(|f| f.name == name));
            if exists {
                return None;
            }
            let fid = ir.add_fun(crate::ir::IrFunction {
                name: name.to_string(),
                params,
                ret,
                body: Some(body),
                is_static: false,
                dispatch_receiver: Some(internal_name),
                param_checks: Vec::new(),
            });
            ir.classes[class_id as usize].methods.push(fid);
            Some(fid)
        };
    let this_field = |ir: &mut IrFile| {
        let recv = ir.add_expr(IrExpr::GetValue(0));
        ir.add_expr(IrExpr::GetField {
            receiver: recv,
            class: class_id,
            index: 0,
        })
    };
    let str_const = |ir: &mut IrFile, s: String| {
        ir.add_expr(IrExpr::Const(crate::ir::IrConst::String(
            crate::kt_string::KtString::from(s),
        )))
    };
    let ret_block = |ir: &mut IrFile, v: ExprId| {
        let r = ir.add_expr(IrExpr::Return(Some(v)));
        ir.add_expr(IrExpr::Block {
            stmts: vec![r],
            value: None,
        })
    };

    // unbox-impl(): U — kotlinc marks it ACC_SYNTHETIC (a compiler-manufactured box adapter).
    {
        let g = this_field(ir);
        let body = ret_block(ir, g);
        if let Some(fid) = add_inst(ir, "unbox-impl", vec![], u_ir, body) {
            ir.synthetic_methods.insert(fid);
        }
    }
    // box-impl(U): X  — `new X(u)`. Also ACC_SYNTHETIC.
    {
        let arg = ir.add_expr(IrExpr::GetValue(0));
        let box_internal = ir.classes[class_id as usize].fq_name_id();
        let new = ir.add_expr(IrExpr::New {
            internal: box_internal,
            args: vec![arg],
            ctor_params: Some(vec![u_ir]),
            ctor_desc: None,
            external_target: None,
            defaults: Box::new([]),
            default_prefix_count: 0,
        });
        let body = ret_block(ir, new);
        let fid = add_static(ir, "box-impl", vec![u_ir], x_ir, body);
        ir.synthetic_methods.insert(fid);
    }
    // constructor-impl(U): U  — runs the `init { … }` block (side effects/validation), then returns the
    // arg. The init runs HERE, not in `box-impl`/`<init>`: `box-impl` only wraps an already-built value, so
    // it must NOT re-run the init. MOVE `init_body` out of the class (clearing it, so `<init>` keeps only
    // the field assignment) and inline it: `ir_lower` lowered it in an INSTANCE frame (`this`@0, ctor param
    // @1), so a sole-field read `this.<field>` is the param — rewrite it to the param, then shift every
    // value slot down by one. The resulting body still runs over the UNBOXED param (slot 0), so step-4
    // rewrites its nested value-class accesses (see the `constructor-impl` entry added to `s4_bodies`).
    {
        let mut stmts = Vec::new();
        if has_init {
            if let Some(init_root) = ir.classes[class_id as usize].init_body {
                let mut reach = HashSet::new();
                collect_reachable(&ir.exprs, init_root, &mut reach);
                let class_fq = ir.classes[class_id as usize].fq_name;
                for id in reach {
                    // The sole field read — as an indexed field read, or as the property it is.
                    let sole_field_read = match &ir.exprs[id as usize] {
                        IrExpr::GetField { class, .. } => *class == class_id,
                        IrExpr::PropertyRead { owner, .. } => *owner == class_fq,
                        _ => false,
                    };
                    if sole_field_read {
                        ir.exprs[id as usize] = IrExpr::GetValue(1); // sole field == the ctor param (slot 1)
                    }
                }
                shift_slots(ir, init_root); // slot 1 (param) → 0; no `this` use remains
                if let IrExpr::Block { stmts: bs, value } = &ir.exprs[init_root as usize] {
                    stmts.extend(bs.iter().copied());
                    if let Some(v) = value {
                        stmts.push(*v);
                    }
                } else {
                    stmts.push(init_root);
                }
                ir.classes[class_id as usize].init_body = None;
            }
        }
        let arg = ir.add_expr(IrExpr::GetValue(0));
        stmts.push(ir.add_expr(IrExpr::Return(Some(arg))));
        let body = ir.add_expr(IrExpr::Block { stmts, value: None });
        let cfid = add_static(ir, "constructor-impl", vec![u_ir], u_ir, body);
        // Unlike a source value-class member converted to `member-impl`, this generated function's
        // carrier is its declared constructor parameter, not a former dispatch receiver. Keeping an
        // owner marker here would make the default-stub emitter exclude the parameter from Kotlin's
        // mask ordinals and silently skip its checked default.
        ir.functions[cfid as usize].dispatch_receiver = None;
        ir.open_methods.insert(cfid); // kotlinc emits `constructor-impl` `public static` (non-final)
                                      // A default on the single underlying property (`ItemId(val value: String = …)`) → register it as
                                      // `constructor-impl`'s param default so the backend emits `constructor-impl$default(U, int, marker)`
                                      // (kotlinc's synthetic). The generic constructor default was reframed to this static layout above.
        if let Some(def) = constructor_default {
            ir.fn_params.insert(
                cfid,
                crate::ir::FnParamInfo::defaults(vec![fname.clone()], vec![Some(def)]),
            );
        }
    }
    // hashCode/equals/toString operate on the value class's IMMEDIATE erased underlying, NOT the final
    // primitive of a nested chain: `ZN(val z: Z1?)` erases to a BOXED `Z1` (`LZ1;`), so it hashes/compares
    // as a reference (`Objects.hashCode`/`areEqual` → `Z1`'s own members), not as the final `Int`.
    let is_ref_under = is_ref(&eu);
    // The internal name that drives `hashCode`/`equals` over the field. A NULLABLE-primitive underlying
    // (`InlineNullablePrimitive(val x: Int?)`) is stored BOXED (`Integer`, null-capable) — it is a
    // reference (`is_ref_under`), so route it to the null-safe `Objects.hashCode`/`areEqual` path (empty
    // name → the `_` arm) rather than the `non_null()` primitive name (`kotlin/Int`), which would emit an
    // `int`-identity `hashCode` returning the boxed `Integer` (a VerifyError). A NON-null primitive keeps
    // its name via `kotlin_class_internal` (NOT `obj_internal`: it arrives as a bare `Ty::Int` variant).
    let final_fq = if is_ref_under {
        String::new()
    } else {
        eu.non_null()
            .kotlin_class_internal()
            .map(|s| s.to_string())
            .unwrap_or_default()
    };
    // equals-impl0(U, U): Boolean
    {
        let a = ir.add_expr(IrExpr::GetValue(0));
        let b = ir.add_expr(IrExpr::GetValue(1));
        let cmp = if custom_equals {
            let boxed = ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: internal_name,
                    name: "box-impl".to_string(),
                    descriptor: format!("({udesc})L{internal};"),
                    inline: InlineKind::None,
                },
                dispatch_receiver: None,
                args: vec![b],
            });
            ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: internal_name,
                    name: "equals-impl".to_string(),
                    descriptor: format!("({udesc}Ljava/lang/Object;)Z"),
                    inline: InlineKind::None,
                },
                dispatch_receiver: None,
                args: vec![a, boxed],
            })
        } else {
            vc_underlying_eq(ir, a, b, is_ref_under, &final_fq)
        };
        let body = ret_block(ir, cmp);
        add_static(ir, "equals-impl0", vec![u_ir, u_ir], bool_ir, body);
    }
    // kotlinc emits the logic in a static `<name>-impl(U)` operating on the unboxed value, and the
    // instance method delegates to it (`toString()` → `toString-impl(this.field)`). The instance methods
    // and the `-impl` statics are all `open` (non-`final`).
    // toString-impl(U v): "X(field=" + v + ")" ; toString(): return toString-impl(this.field)
    {
        let simple = internal
            .rsplit('/')
            .next()
            .unwrap_or(&internal)
            .replace('$', ".");
        if !custom_to_string {
            let v = ir.add_expr(IrExpr::GetValue(0));
            // ONE `StringConcat` (not nested `+`): kotlinc builds a single `StringBuilder` and appends the
            // 1-char closing paren via `append(C)` — a nested concat would emit a second builder.
            let prefix = str_const(ir, format!("{simple}({fname}="));
            let close = str_const(ir, ")".to_string());
            let acc = ir.add_expr(IrExpr::StringConcat(vec![prefix, v, close]));
            let sbody = ret_block(ir, acc);
            let impl_fid = add_static(ir, "toString-impl", vec![u_ir], str_ir, sbody);
            ir.open_methods.insert(impl_fid);
        }
        let fv = this_field(ir);
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: internal_name,
                name: "toString-impl".to_string(),
                descriptor: format!("({udesc})Ljava/lang/String;"),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![fv],
        });
        let ibody = ret_block(ir, call);
        if let Some(fid) = add_inst(ir, "toString", vec![], str_ir, ibody) {
            ir.open_methods.insert(fid);
        }
    }
    // hashCode-impl(U v): v.hashCode() ; hashCode(): return hashCode-impl(this.field)
    {
        if !custom_hash_code {
            let v = ir.add_expr(IrExpr::GetValue(0));
            // A NON-NULL reference underlying hashes through its OWN `hashCode()` (kotlinc's shape,
            // `String.hashCode()`), not the null-safe `Objects.hashCode` — that is only for a nullable (or
            // boxed-primitive) underlying, which can actually be null.
            // Only a real non-null reference CLASS underlying: an ARRAY has no such class (`kotlin/IntArray`
            // is not a JVM type — a virtual call on it is a `NoClassDefFoundError`), and a nullable or
            // boxed-primitive underlying must keep the null-safe `Objects.hashCode`.
            let nonnull_ref_owner: Option<String> = (is_ref_under
                && !eu.is_nullable()
                && !eu.non_null().is_array()
                && matches!(eu.non_null(), Ty::String | Ty::Obj(..)))
            .then(|| eu.non_null().kotlin_class_internal().map(|s| s.to_string()))
            .flatten();
            let h = field_hash_ir(ir, v, &final_fq, nonnull_ref_owner.as_deref());
            let sbody = ret_block(ir, h);
            let impl_fid = add_static(ir, "hashCode-impl", vec![u_ir], int_ir, sbody);
            ir.open_methods.insert(impl_fid);
        }
        let fv = this_field(ir);
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: internal_name,
                name: "hashCode-impl".to_string(),
                descriptor: format!("({udesc})I"),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![fv],
        });
        let ibody = ret_block(ir, call);
        if let Some(fid) = add_inst(ir, "hashCode", vec![], int_ir, ibody) {
            ir.open_methods.insert(fid);
        }
    }
    // equals-impl(U v, Object other): other is X && equals-impl0(v, other.unbox-impl())
    // equals(other): return equals-impl(this.field, other)
    {
        if !custom_equals {
            // static: v = slot 0, other = slot 1.
            let mut stmts = Vec::new();
            let other = ir.add_expr(IrExpr::GetValue(1));
            let not_inst = ir.add_expr(IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::NotInstanceOf,
                arg: other,
                type_operand: x_ir,
            });
            stmts.push(guard_false(ir, not_inst));
            let other_v = ir.add_expr(IrExpr::GetValue(1));
            let ocast = ir.add_expr(IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::Cast,
                arg: other_v,
                type_operand: x_ir,
            });
            let ounbox = ir.add_expr(IrExpr::Call {
                callee: Callee::Virtual {
                    owner: internal_name,
                    name: "unbox-impl".to_string(),
                    descriptor: format!("(){udesc}"),
                    params: None,
                    interface: false,
                },
                dispatch_receiver: Some(ocast),
                args: vec![],
            });
            // kotlinc INLINES the underlying comparison here (it does not call `equals-impl0`), with the
            // OTHER operand first, then guards: `if (!eq) return false; return true`.
            let v = ir.add_expr(IrExpr::GetValue(0));
            let eq = vc_underlying_eq(ir, ounbox, v, is_ref_under, &final_fq);
            let zero = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(0)));
            let not_eq = ir.add_expr(IrExpr::PrimitiveBinOp {
                op: crate::ir::IrBinOp::Eq,
                lhs: eq,
                rhs: zero,
            });
            stmts.push(guard_false(ir, not_eq));
            let t = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Boolean(true)));
            stmts.push(ir.add_expr(IrExpr::Return(Some(t))));
            let sbody = ir.add_expr(IrExpr::Block { stmts, value: None });
            let impl_fid = add_static(ir, "equals-impl", vec![u_ir, any_ir], bool_ir, sbody);
            ir.open_methods.insert(impl_fid);
        }
        // instance equals(other) → return equals-impl(this.field, other)
        let fv = this_field(ir);
        let other_i = ir.add_expr(IrExpr::GetValue(1));
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: internal_name,
                name: "equals-impl".to_string(),
                descriptor: format!("({udesc}Ljava/lang/Object;)Z"),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![fv, other_i],
        });
        let ibody = ret_block(ir, call);
        if let Some(fid) = add_inst(ir, "equals", vec![any_ir], bool_ir, ibody) {
            ir.open_methods.insert(fid);
        }
    }

    // A secondary constructor becomes a static `constructor-impl` overload. Preserve its semantic
    // declaration shape before consuming the instance-constructor IR: downstream Kotlin compilation
    // selects the source constructor from metadata, then follows the exact static realization handle.
    let secondary_metadata = ir.classes[class_id as usize]
        .secondary_ctors
        .iter()
        .filter(|constructor| !constructor.synthetic)
        .map(|constructor| crate::ir::IrJvmValueClassSecondaryCtor {
            params: constructor.named_params.clone(),
            param_defaults: constructor.defaults.iter().map(Option::is_some).collect(),
            vararg_index: constructor.vararg_index,
            annotations: constructor.annotations.clone(),
            descriptor: method_descriptor(
                &jvm_tys(
                    &constructor
                        .params
                        .iter()
                        .map(|parameter| erase(parameter, under))
                        .collect::<Vec<_>>(),
                ),
                ir_ty_to_jvm(&eu),
            ),
        })
        .collect::<Vec<_>>();
    if !secondary_metadata.is_empty() {
        ir.jvm_value_class_secondary_ctors
            .insert(internal_name, secondary_metadata);
    }
    let secs = std::mem::take(&mut ir.classes[class_id as usize].secondary_ctors);
    if !secs.is_empty() {
        for sc in secs {
            if !sc.prefix_params.is_empty() {
                return false;
            }
            let crate::ir::CtorDelegateTarget::This {
                target_params,
                to_primary: _,
                default_masks,
            } = &sc.delegate
            else {
                return false;
            };
            if !default_masks.is_empty()
                || !sc.default_parameters.is_empty()
                || sc.delegate_args.len() != target_params.len()
            {
                return false;
            }
            let target_params = target_params
                .iter()
                .map(|parameter| erase(parameter, under))
                .collect::<Vec<_>>();
            let target_descriptor = method_descriptor(&jvm_tys(&target_params), ir_ty_to_jvm(&eu));

            let mut roots = sc.delegate_prelude.clone();
            roots.extend(sc.delegate_args.iter().copied());
            roots.extend(sc.body);
            let delegated_value = max_value_slot(ir, &roots).max(
                u32::try_from(sc.params.len()).expect("value-class constructor parameter count"),
            );

            for &statement in &sc.delegate_prelude {
                shift_slots(ir, statement);
            }
            for &a in &sc.delegate_args {
                shift_slots(ir, a);
            }
            if let Some(body) = sc.body {
                reframe_value_class_secondary(ir, body, delegated_value);
            }

            let mut stmts = sc.delegate_prelude.clone();
            let call = ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: internal_name,
                    name: "constructor-impl".to_string(),
                    descriptor: target_descriptor,
                    inline: InlineKind::None,
                },
                dispatch_receiver: None,
                args: sc.delegate_args.clone(),
            });
            stmts.push(ir.add_expr(IrExpr::Variable {
                index: delegated_value,
                ty: u_ir,
                init: Some(call),
                named: false,
            }));
            if let Some(body) = sc.body {
                if let IrExpr::Block { stmts: bs, value } = &ir.exprs[body as usize] {
                    stmts.extend(bs.iter().copied());
                    if let Some(value) = value {
                        stmts.push(*value);
                    }
                } else {
                    stmts.push(body);
                }
            }
            let result = ir.add_expr(IrExpr::GetValue(delegated_value));
            stmts.push(ir.add_expr(IrExpr::Return(Some(result))));
            let body = ir.add_expr(IrExpr::Block { stmts, value: None });
            add_static(ir, "constructor-impl", sc.params.clone(), u_ir, body);
        }
    }
    true
}

fn max_value_slot(ir: &IrFile, roots: &[ExprId]) -> u32 {
    let mut reachable = HashSet::new();
    for &root in roots {
        collect_reachable_scoped(&ir.exprs, root, &mut reachable);
    }
    reachable
        .into_iter()
        .filter_map(|id| match &ir.exprs[id as usize] {
            IrExpr::GetValue(index)
            | IrExpr::SetValue { var: index, .. }
            | IrExpr::Variable { index, .. } => Some(*index),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

fn reframe_value_class_secondary(ir: &mut IrFile, root: ExprId, this_value: u32) {
    let mut reachable = HashSet::new();
    collect_reachable_scoped(&ir.exprs, root, &mut reachable);
    for id in reachable {
        let index = match &mut ir.exprs[id as usize] {
            IrExpr::GetValue(index)
            | IrExpr::SetValue { var: index, .. }
            | IrExpr::Variable { index, .. } => index,
            _ => continue,
        };
        if *index == 0 {
            *index = this_value;
        } else {
            *index -= 1;
        }
    }
}

/// Decrement every value-slot index (`GetValue`/`SetValue`/`Variable`) reachable from `root` by one —
/// reframing an instance-lowered body (`this` at slot 0) as a static one (params at slot 0).
fn shift_slots(ir: &mut IrFile, root: ExprId) {
    let mut reach = HashSet::new();
    collect_reachable_scoped(&ir.exprs, root, &mut reach);
    for id in reach {
        match &mut ir.exprs[id as usize] {
            IrExpr::GetValue(i)
            | IrExpr::SetValue { var: i, .. }
            | IrExpr::Variable { index: i, .. } => {
                *i = i.saturating_sub(1);
            }
            _ => {}
        }
    }
}

/// `if (cond) return false`.
/// The value-class underlying-value equality kotlinc emits: a reference compares via
/// `Intrinsics.areEqual`, an IEEE float/double by TOTAL ORDER (`Float.compare(a,b) == 0`, so
/// `NaN == NaN` and `0.0 != -0.0`), every other primitive natively. Shared by `equals-impl0` and the
/// inlined comparison inside `equals-impl`.
fn vc_underlying_eq(
    ir: &mut IrFile,
    a: ExprId,
    b: ExprId,
    is_ref_under: bool,
    final_fq: &str,
) -> ExprId {
    if is_ref_under {
        return ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: type_name("kotlin/jvm/internal/Intrinsics"),
                name: "areEqual".into(),
                descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Z".into(),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![a, b],
        });
    }
    if existing_type_name(final_fq).is_some_and(is_ieee_fp) {
        let (owner, desc) = if final_fq == "kotlin/Float" {
            ("java/lang/Float", "(FF)I")
        } else {
            ("java/lang/Double", "(DD)I")
        };
        let call = ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: type_name(owner),
                name: "compare".into(),
                descriptor: desc.into(),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![a, b],
        });
        let zero = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(0)));
        return ir.add_expr(IrExpr::PrimitiveBinOp {
            op: crate::ir::IrBinOp::Eq,
            lhs: call,
            rhs: zero,
        });
    }
    ir.add_expr(IrExpr::PrimitiveBinOp {
        op: crate::ir::IrBinOp::Eq,
        lhs: a,
        rhs: b,
    })
}

fn guard_false(ir: &mut IrFile, cond: ExprId) -> ExprId {
    let f = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Boolean(false)));
    let r = ir.add_expr(IrExpr::Return(Some(f)));
    let blk = ir.add_expr(IrExpr::Block {
        stmts: vec![r],
        value: None,
    });
    ir.add_expr(IrExpr::When {
        branches: vec![(Some(cond), blk)],
    })
}

/// `field.hashCode()` for an underlying fq name (primitive → native, reference → `Objects.hashCode`).
fn field_hash_ir(ir: &mut IrFile, v: ExprId, fq: &str, nonnull_ref_owner: Option<&str>) -> ExprId {
    let call = |ir: &mut IrFile, owner: &str, desc: &str, v: ExprId| {
        ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: owner.into(),
                name: "hashCode".into(),
                descriptor: desc.into(),
                inline: InlineKind::None,
            },
            dispatch_receiver: None,
            args: vec![v],
        })
    };
    match fq {
        // Unsigned underlyings are unboxed to the signed primitive; their `hashCode` is that primitive's
        // (`UInt.hashCode()` = the `Int` value itself; `ULong.hashCode()` = `Long.hashCode(long)`).
        "kotlin/Int" | "kotlin/Short" | "kotlin/Byte" | "kotlin/Char" | "kotlin/UByte"
        | "kotlin/UShort" | "kotlin/UInt" => v,
        "kotlin/Boolean" => call(ir, "java/lang/Boolean", "(Z)I", v),
        "kotlin/Long" | "kotlin/ULong" => call(ir, "java/lang/Long", "(J)I", v),
        "kotlin/Double" => call(ir, "java/lang/Double", "(D)I", v),
        "kotlin/Float" => call(ir, "java/lang/Float", "(F)I", v),
        _ => match nonnull_ref_owner {
            // `v.hashCode()` on the underlying's own class.
            Some(owner) => ir.add_expr(IrExpr::Call {
                callee: Callee::Virtual {
                    owner: owner.into(),
                    name: "hashCode".into(),
                    descriptor: "()I".into(),
                    params: None,
                    interface: false,
                },
                dispatch_receiver: Some(v),
                args: vec![],
            }),
            None => call(ir, "java/util/Objects", "(Ljava/lang/Object;)I", v),
        },
    }
}

/// kotlinc's inline-class mangling info for an IR type, against the value classes in `under`.
fn mangling_info(t: &Ty, under: &Under) -> crate::jvm::inline_class::InfoForMangling {
    let (fq_name, is_value, is_nullable) = match t.non_null().obj_internal() {
        Some(fq_name) => (
            fq_name.render(),
            under.contains_key(&fq_name),
            t.is_nullable(),
        ),
        None => (String::new(), false, false),
    };
    crate::jvm::inline_class::InfoForMangling {
        is_value,
        // kotlinc hashes the declared Kotlin FqName (`pkg.Outer.Inner` — dots throughout), never the
        // JVM internal spelling, so a NESTED value class converts its `$` separator too: `I$V` must
        // hash as `I.V` or every member mentioning it gets a different `-<hash>` than kotlinc's.
        fq_name: fq_name.replace(['/', '$'], "."),
        is_nullable,
    }
}

/// kotlinc's name for a function whose JVM signature mentions a value class: `base-<hash>` (a value-class
/// parameter, or a value-class return, triggers it). Plain `base` otherwise.
/// [`vc_mangle`] that leaves an ALREADY-mangled name alone: if `base` is exactly what this signature
/// would produce from its own stem, it is returned unchanged. A JVM method name a Kotlin declaration
/// produces never contains `-` unless kotlinc's value-class mangle put it there, so splitting at the
/// last `-` and re-mangling the stem is an exact test for "this name is already the answer".
fn vc_mangle_once(
    base: &str,
    params: &[Ty],
    ret: &Ty,
    under: &Under,
    is_file_class: bool,
    is_suspend: bool,
) -> String {
    if let Some((stem, _)) = base.rsplit_once('-') {
        if vc_mangle(stem, params, ret, under, is_file_class, is_suspend) == base {
            return base.to_string();
        }
    }
    vc_mangle(base, params, ret, under, is_file_class, is_suspend)
}

fn vc_mangle(
    base: &str,
    params: &[Ty],
    ret: &Ty,
    under: &Under,
    is_file_class: bool,
    is_suspend: bool,
) -> String {
    // PARAM mangling (kotlinc `IrType.getRequiresMangling`) EXEMPTS `kotlin.Result`
    // (`!isClassWithFqName(RESULT_FQ_NAME)`), so a `Result` parameter never triggers a mangle.
    let mut pinfo: Vec<_> = params
        .iter()
        .map(|t| {
            let mut info = mangling_info(t, under);
            if info.fq_name == "kotlin.Result" {
                info.is_value = false;
            }
            info
        })
        .collect();
    // kotlinc mangles the ORIGINAL (pre-CPS) signature, which for a suspend fun includes the trailing
    // `Continuation` value parameter — a non-inline type, so it contributes the `_` placeholder. Without
    // it a suspend `f(Id): Int` would hash identically to the non-suspend overload. (A lone non-value
    // `_` never triggers mangling on its own — `requires_param_mangling` checks `is_value`.)
    if is_suspend {
        pinfo.push(crate::jvm::inline_class::InfoForMangling {
            fq_name: String::new(),
            is_value: false,
            is_nullable: false,
        });
    }
    // RETURN mangling (kotlinc `hasMangledReturnType`) does NOT exempt `Result`, but applies only when the
    // function is NOT in a file class (a top-level fn returning a value class keeps its plain name).
    let rinfo = mangling_info(ret, under);
    let ret_opt = (rinfo.is_value && !is_file_class).then_some(&rinfo);
    crate::jvm::inline_class::mangled_name(base, &pinfo, ret_opt)
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
    method_descriptor(&jvm_tys(params), ir_ty_to_jvm(ret))
}

/// Collect every `ExprId` reachable from `root` (a function body), so rewrites stay within bodies that
/// own value-class values unboxed.
/// Slot-type map for a body rooted at `root` running over `params` (slot 0 = `this`, params at 1..), plus
/// any local `Variable`s declared inside it — used to give an `init`/secondary-ctor/super-arg body the same
/// slot-typed box/unbox analysis a function body gets from its captured `slot_types`.
fn body_slot_map(exprs: &[IrExpr], root: ExprId, params: &[Ty]) -> HashMap<u32, Ty> {
    let mut slots: HashMap<u32, Ty> = HashMap::new();
    for (i, t) in params.iter().enumerate() {
        slots.insert(1 + i as u32, t.clone());
    }
    let mut reach = HashSet::new();
    collect_reachable(exprs, root, &mut reach);
    for id in reach {
        if let IrExpr::Variable { index, ty, .. } = &exprs[id as usize] {
            slots.insert(*index, *ty);
        }
    }
    slots
}

fn primary_super_slot_map(
    exprs: &[IrExpr],
    class: &crate::ir::IrClass,
    params: &[Ty],
) -> HashMap<u32, Ty> {
    let mut slots = params
        .iter()
        .enumerate()
        .map(|(index, ty)| (1 + index as u32, *ty))
        .collect::<HashMap<_, _>>();
    let mut reach = HashSet::new();
    for root in class
        .super_arg_prelude
        .iter()
        .chain(&class.super_args)
        .copied()
    {
        collect_reachable(exprs, root, &mut reach);
    }
    for id in reach {
        if let IrExpr::Variable { index, ty, .. } = &exprs[id as usize] {
            slots.insert(*index, *ty);
        }
    }
    slots
}

fn secondary_ctor_slot_map(
    exprs: &[IrExpr],
    constructor: &crate::ir::IrSecondaryCtor,
    params: &[Ty],
) -> HashMap<u32, Ty> {
    let mut slots: HashMap<u32, Ty> = params
        .iter()
        .enumerate()
        .map(|(index, ty)| (1 + index as u32, *ty))
        .collect();
    let mut reach = HashSet::new();
    for root in constructor
        .delegate_prelude
        .iter()
        .chain(&constructor.delegate_args)
        .chain(constructor.defaults.iter().flatten())
        .copied()
        .chain(constructor.body)
    {
        collect_reachable(exprs, root, &mut reach);
    }
    for id in reach {
        if let IrExpr::Variable { index, ty, .. } = &exprs[id as usize] {
            slots.insert(*index, *ty);
        }
    }
    slots
}

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
                    bodies.push((*inline_body, slots.clone()));
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
