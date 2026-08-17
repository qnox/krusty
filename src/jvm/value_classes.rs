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

use crate::ir::{Callee, ExprId, IrExpr, IrFile};
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
type SuperMemberNames = HashMap<TypeName, Option<HashMap<String, Option<Ty>>>>;

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
                callee: Callee::CrossFile { params, ret, .. },
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

fn same_file_super_member_names(ir: &IrFile) -> SuperMemberNames {
    ir.classes
        .iter()
        .filter(|c| !c.is_value)
        .map(|c| {
            let mut names: HashMap<String, Option<Ty>> = HashMap::new();
            let mut work: Vec<TypeName> = c
                .interfaces
                .iter_ids()
                .chain(c.supertypes.iter().filter_map(|t| t.obj_internal()))
                .chain(c.has_non_top_superclass().then_some(c.superclass))
                .collect();
            let mut known = true;
            let mut seen: HashSet<TypeName> = HashSet::new();
            while let Some(s) = work.pop() {
                if s.matches("java/lang/Object") || s.matches("kotlin/Any") || !seen.insert(s) {
                    continue;
                }
                match ir.classes.iter().find(|o| o.fq_name == s) {
                    Some(sup) => {
                        for &m in &sup.methods {
                            if let Some(f) = ir.functions.get(m as usize) {
                                let getter_ret = (f.name.starts_with("get") && f.params.is_empty())
                                    .then_some(f.ret);
                                names.insert(f.name.clone(), getter_ret);
                            }
                        }
                        work.extend(sup.interfaces.iter_ids());
                        work.extend(sup.supertypes.iter().filter_map(|t| t.obj_internal()));
                        if sup.has_non_top_superclass() {
                            work.push(sup.superclass);
                        }
                    }
                    None => {
                        known = false;
                        break;
                    }
                }
            }
            crate::trace_compiler!(
                "value_classes",
                "super_member_names {} known={known} names={:?} ifaces={:?} super={:?}",
                c.fq_name.render(),
                names,
                c.interfaces,
                c.superclass
            );
            (c.fq_name, known.then_some(names))
        })
        .collect()
}

pub(crate) fn apply_override_final_drop(
    ir: &mut IrFile,
    resolver: &crate::symbol_resolver::SymbolResolver,
) {
    let super_member_names = same_file_super_member_names(ir);
    let mut override_opens: Vec<u32> = Vec::new();
    for c in &ir.classes {
        if c.is_interface || c.fq_name.render().ends_with("$$serializer") {
            continue;
        }
        let Some(Some(names)) = super_member_names.get(&c.fq_name) else {
            continue;
        };
        for &m in &c.methods {
            if let Some(f) = ir.functions.get(m as usize) {
                if !f.is_static && names.contains_key(&f.name) {
                    override_opens.push(m);
                }
            }
        }
    }
    for c in &ir.classes {
        if c.is_interface || c.fq_name.render().ends_with("$$serializer") {
            continue;
        }
        if let Some(Some(_)) = super_member_names.get(&c.fq_name) {
            continue;
        }
        let mut supers: Vec<TypeName> = c.interfaces.iter_ids().collect();
        supers.extend(c.supertypes.iter().filter_map(|t| t.obj_internal()));
        if c.has_non_top_superclass() {
            supers.push(c.superclass);
        }
        if supers.is_empty() {
            continue;
        }
        for &m in &c.methods {
            if let Some(f) = ir.functions.get(m as usize) {
                if !f.is_static
                    && supers
                        .iter()
                        .any(|s| resolver.declares_member(&s.render(), &f.name))
                {
                    override_opens.push(m);
                }
            }
        }
    }
    ir.open_methods.extend(override_opens);
}

#[must_use]
pub fn lower_value_classes(
    ir: &mut IrFile,
    resolver: &crate::symbol_resolver::SymbolResolver,
    // Same-module SOURCE value classes (internal name → sole-field underlying), collected from the
    // frontend symbols. A value class declared in ANOTHER file of this module is neither in `ir.classes`
    // (a different file) nor reported by the resolver (whose `value_underlying` only decodes classpath
    // `@Metadata`), so its erasure/mangle map entry comes from here — without leaking value-class-ness
    // into the CHECKER's library view (which drives construction/member resolution).
    module_value_classes: &std::collections::HashMap<TypeName, Ty>,
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
    // internal name → underlying (single-field) type, before erasure. NOTE: the `Object` underlying for a
    // generic value class is a deliberate approximation — the correct BOUND (`S<T: String>` → `String`)
    // BREAKS more `*Generic` files than it fixes (their lambda boxing / list iteration / equality assume the
    // `Object` repr). Metadata (`type_param_bounds`/`field_type_params`) stays ready for when downstream is.
    // We keep the `Object` underlying TYPE for a generic value class, but DO carry the nullability of its
    // type-parameter bound (`X<T: String?>` → null-capable `Object?`): that's what `nullable_is_boxed`
    // and the `checkNotNullParameter` elision key on, and unlike using the bound itself it doesn't disturb
    // the `Object`-repr that the `*Generic` files assume.
    let under: Under = ir
        .classes
        .iter()
        .filter(|c| c.is_value)
        .filter_map(|c| {
            c.fields.first().map(|f| {
                let t = &f.ty;
                // A type-parameter field is null-capable (the `Object` underlying can hold `null`) UNLESS
                // it has an explicit NON-NULL bound: `<T>`/`<T: Any?>`/`<T: String?>` → null-capable;
                // `<T: String>` → not. (Kotlin's default upper bound is the nullable `Any?`.)
                let null_capable = f.type_param.as_ref().is_some_and(|name| {
                    match c.type_param_bounds.iter().find(|(n, _)| n == name) {
                        Some((_, b)) => b.is_nullable(),
                        None => true,
                    }
                });
                let u = if null_capable { Ty::nullable(*t) } else { *t };
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
        let classifier = resolver.classifier(fq);
        if let Some(property) = classifier
            .as_ref()
            .and_then(|classifier| classifier.value_underlying_property.clone())
        {
            crate::trace_compiler!(
                "value_classes",
                "external value class {} underlying property {}",
                fq,
                property
            );
            external_underlying_properties.insert(fq, property);
        }
        if let Some(u) = classifier
            .and_then(|classifier| classifier.value_underlying)
            .or_else(|| module_value_classes.get(&fq).copied())
        {
            // The underlying carries its own declared nullability — trust it: a NON-NULL reference
            // underlying (`ItemId(val value: String)`) means `ItemId?` stays UNBOXED (null carried by
            // the reference), exactly like a same-file value class. Classpath VCs come from the resolver
            // (decoded from `@Metadata`); same-module source VCs from `module_value_classes`.
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

    // A semantic property operation deliberately keeps the Kotlin property name. For an owner compiled
    // from another source file there is no classfile for the emitter to inspect, so record the JVM
    // accessor spelling here while the original property type is still present. The emitter consults
    // this table only as its declaration-less fallback; same-file declarations and classpath metadata
    // remain authoritative. Keeping this target fact in a JVM-pass side table prevents common lowering
    // from branching on whether the owner came from this file, another module file, or the classpath.
    ir.property_accessor_jvm_realizations
        .extend(ir.exprs.iter().enumerate().filter_map(|(id, expression)| {
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
            Some((operation, (accessor, physical)))
        }));

    let value_class_ids: Vec<u32> = (0..ir.classes.len() as u32)
        .filter(|&i| ir.classes[i as usize].is_value)
        .collect();

    // A value class whose underlying (single-field) type is an INNER-class instance is unsupported:
    // the box/unbox path does not thread the enclosing `this$0` receiver an inner class carries, so
    // codegen would emit an unsound cast (the shape reaches here only via an `Outer<X>.Inner<Y>`
    // underlying). Bail so the whole file skips cleanly rather than miscompiling. An inner class is
    // identified by its synthetic `this$0` first field (created only at inner-class synthesis).
    let inner_class_names: std::collections::HashSet<TypeName> = ir
        .classes
        .iter()
        .filter(|c| c.fields.first().is_some_and(|f| f.name == "this$0"))
        .map(|c| c.fq_name)
        .collect();
    if !inner_class_names.is_empty()
        && value_class_ids.iter().any(|&cid| {
            ir.classes[cid as usize]
                .fields
                .first()
                .and_then(|f| f.ty.kotlin_class_internal())
                .is_some_and(|n| inner_class_names.contains(&n))
        })
    {
        return false;
    }

    // Synthesize each value class's `-impl`/`equals`/`hashCode`/`toString` members up front (a JVM
    // concern — `ir_lower` only emits the plain single-field class). Done before the analysis below so
    // they participate in `vc_methods`/erasure like any other method.
    for cid in value_class_ids {
        // A real value class always has its single backing field; guard malformed fieldless input.
        if ir.classes[cid as usize].fields.is_empty() {
            continue;
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
        if !synth_value_members(ir, cid, &under, has_init) {
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
        .map(|c| c.secondary_ctors.iter().map(|s| s.params.clone()).collect())
        .collect();

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
            if getter[class_index]
                .as_ref()
                .is_some_and(|name| ir.functions[fid as usize].name == *name)
            {
                vc_sole_getter_fids.insert(fid);
            }
        }
    }

    // Each source value class's getter name keyed by its internal name (`A2` → `getValue`) — to
    // recognize the legacy call-shaped form. Semantic property nodes use the source property-name map
    // below and never derive meaning from a JVM getter spelling.
    let vc_getters: HashMap<TypeName, String> = ir
        .classes
        .iter()
        .filter(|c| c.is_value)
        .filter_map(|c| {
            c.fields
                .first()
                .map(|f| (c.fq_name, property_getter_name(&f.name)))
        })
        .collect();
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
    let suspend_sig: std::collections::HashSet<(Option<TypeName>, String, usize)> = ir
        .functions
        .iter()
        .enumerate()
        .filter(|(fid, _)| suspend_fids.contains(&(*fid as u32)))
        .map(|(fid, f)| (f.dispatch_receiver, f.name.clone(), orig_params[fid].len()))
        .collect();
    // A suspend fn's result crosses the `Continuation` `Object` boundary BOXED (kotlinc boxes at the
    // CPS `areturn` and `checkcast X` + `unbox-impl`s on resume), while erasure would otherwise keep it
    // unboxed. A SAME-FILE declaration is handled: step 6 boxes its tail and records the class in
    // `suspend_boxed_value_class_returns` for the coroutine pass's resume side. A CROSS-UNIT suspend
    // call (`ir.suspend_calls` — a callee in another file/dependency, absent from `suspend_funs`) has
    // no such record, so the resume side would meet a box it does not know to unwrap: still bail there.
    let mentions_vc = |t: &Ty| {
        t.non_null()
            .obj_internal()
            .is_some_and(|fq| under.contains_key(&fq))
    };
    // Only a NON-NULL value-class RETURN is at stake; VC PARAMS pass unboxed through the mangled CPS
    // signature and work, and a nullable `X?` return is already the boxed form.
    let vc_ret = |t: &Ty| !t.is_nullable() && mentions_vc(t);
    if ir.suspend_calls.values().any(vc_ret) {
        return false;
    }
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
            // extension call on it (`it.getOrThrow()`) unboxes it. A scalar-underlying value class keeps its
            // own handling. Value-class-ness is decided HERE (with `under`), not in the lambda-agnostic lowerer.
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
                collect_reachable_scoped(&ir.exprs, &ir.inline_only_fns, root, &mut reach);
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
    let super_member_names = same_file_super_member_names(ir);
    // A getter NAME whose override pair DIVERGES in type across the same-file hierarchy (a covariant
    // `val alt: Vid` overriding `val alt: Vid?`): the two sides would hash differently, and krusty
    // doesn't emit the bridge kotlinc pairs them with — keep BOTH sides unmangled (consistent).
    let divergent_getters: HashSet<String> = {
        let mut out = HashSet::new();
        for c in &ir.classes {
            let Some(Some(names)) = super_member_names.get(&c.fq_name) else {
                continue;
            };
            for &m in &c.methods {
                if let Some(f) = ir.functions.get(m as usize) {
                    if f.name.starts_with("get") && f.params.is_empty() {
                        if let Some(Some(sup_ret)) = names.get(&f.name) {
                            if *sup_ret != f.ret {
                                out.insert(f.name.clone());
                            }
                        }
                    }
                }
            }
        }
        out
    };
    // `(fid, param idx, boxed value-class Ty)` for nullable-underlying value-class params — the base
    // method unboxes them (below), but its `$default` stub + call site keep them boxed (recorded here).
    let mut default_boxed: Vec<(u32, usize, Ty)> = Vec::new();
    // `(fid, declared name, declared params, declared ret)` — collected while `ir.functions` is borrowed
    // mutably, moved into `ir.vc_declared_sigs` once the loop releases it.
    let mut declared_sigs: Vec<(u32, String, Vec<Ty>, Ty)> = Vec::new();
    // `(fid, param slot, value class, erased underlying)` for a REFERENCE-underlying lambda own-param
    // kept boxed: the body was lowered against the erased convention (the slot IS the underlying), so
    // every read of the slot gains an `unbox-impl` after the loop (kotlinc reaches the same state via
    // its lambda-class `invoke` bridge; here the unbox is fused into the impl at each use).
    let mut boxed_own_reads: Vec<(u32, u32, TypeName, Ty)> = Vec::new();
    for (fid, f) in ir.functions.iter_mut().enumerate() {
        let is_box_impl = f.name == "box-impl";
        // A USER value-class member function's body runs on the BOXED object; its value-class-typed
        // parameters/return stay boxed (a sibling member call passes `this` — a box — directly). The
        // SYNTHESIZED members (`-impl`, `equals`/`hashCode`/`toString`, the getter, `<init>`) operate on
        // the underlying representation, so they erase like any other function.
        // A property GETTER (`getValue` on a value class, `getProperty` for a `val`) takes no value
        // parameters — match it by the `get` prefix AND an empty parameter list. It stays unmangled UNLESS
        // its owner is a supertype-free class (`getter_mangle_owners`), where a value-class-returning getter
        // safely mangles to kotlinc's `getId-<hash>` (no override to keep consistent). A user FUNCTION that
        // starts with `get` but takes parameters (`suspend fun getById(id: ItemId)`) is a normal member
        // and always mangles.
        let is_vc_field_getter = f.name.starts_with("get")
            && orig_params[fid].is_empty()
            && !f.dispatch_receiver.is_some_and(|r| {
                // Mangle this getter only when the owner's WHOLE supertype chain is same-file-known
                // AND either none of it declares this name (the class's own getter) or the
                // declaration is the SAME-typed getter (both sides mangle to the same hash). A
                // type-divergent pair stays unmangled on both sides.
                !divergent_getters.contains(&f.name)
                    && super_member_names.get(&r).is_some_and(|sups| {
                        sups.as_ref().is_some_and(|names| match names.get(&f.name) {
                            None => true,
                            Some(Some(t)) => *t == orig_rets[fid],
                            Some(None) => false,
                        })
                    })
            });
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
        ) || is_vc_field_getter;
        let vc_member = !synthesized && vc_methods.contains(&(fid as u32));
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
            if orig_params[fid]
                .iter()
                .chain(std::iter::once(&orig_rets[fid]))
                .any(is_callable_vc_ty)
            {
                declared_sigs.push((
                    fid as u32,
                    f.name.clone(),
                    orig_params[fid].clone(),
                    orig_rets[fid],
                ));
            }
            let mangled = vc_mangle(
                &f.name,
                &orig_params[fid],
                &orig_rets[fid],
                &callable_under,
                is_file_class,
                suspend_fids.contains(&(fid as u32)),
            );
            if mangled != f.name {
                if let Some(owner) = &f.dispatch_receiver {
                    mangle_map.insert(
                        (*owner, f.name.clone(), orig_params[fid].len()),
                        mangled.clone(),
                    );
                }
                f.name = mangled;
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
            if !(vc_member && is_vc_ty(p)) {
                if vc_underlying_nullable(p, &under) {
                    default_boxed.push((fid as u32, idx, *p));
                }
                *p = erase(p, &under);
            }
        }
        if !(is_box_impl || vc_member && is_vc_ty(&f.ret)) {
            f.ret = erase(&f.ret, &under);
        }
        if !f.param_checks.is_empty() {
            for (k, chk) in f.param_checks.iter_mut().enumerate() {
                // Drop the null-check when the param erased to a non-reference, OR when it was a
                // value class whose unboxed underlying is itself null-capable (e.g. `X(val v: Int?)`
                // erases to `Integer`, which the value `X(null)` leaves null) — kotlinc emits no
                // `checkNotNullParameter` there.
                let under_nullable = orig_params[fid]
                    .get(k)
                    .is_some_and(|t| vc_underlying_nullable(t, &under));
                if chk.is_some() && (!f.params.get(k).is_some_and(is_ref) || under_nullable) {
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
    // A value class with a USER instance member that RETURNS a value class (`value class Foo { fun
    // inc(): Foo }`) is not yet modeled: the member hands back a boxed `X`, and threading that through a
    // chained call / an unboxed-slot store (or the spliced body of an `inline` such member) needs a
    // box/unbox dance krusty doesn't do here. SKIP the file — never miscompile — as it was before
    // value-class construction unified. The value class's own synthesized members (`box-impl`, the
    // `Any`-overrides) are excluded; a member returning a NON-value-class lowers fine.
    let vc_member_returns_vc = boxed_ret_methods.keys().any(|&(ci, mi)| {
        let c = &ir.classes[ci as usize];
        c.is_value
            && c.methods
                .get(mi as usize)
                .and_then(|&fid| ir.functions.get(fid as usize))
                .is_some_and(|f| {
                    let n = f.name.as_str();
                    // Exclude synthesized members and only a structurally identified FIELD GETTER. A
                    // user `operator fun get(index): X` shares the `get` prefix but returns a boxed `X`;
                    // admitting it through a name-shape exception miscompiles its callers. The getter map
                    // is keyed by its resolved class/method identity and verified to read the field.
                    !n.contains("-impl")
                        && !field_getters.contains_key(&(ci, mi))
                        && !matches!(
                            n,
                            "box-impl" | "equals" | "hashCode" | "toString" | "<init>"
                        )
                })
    });
    if vc_member_returns_vc {
        return false;
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
    // A reference-underlying lambda own-param now arrives BOXED (`LX;`) but its body was lowered
    // against the erased convention (the slot as the underlying) — rewrite every read of the slot to
    // `unbox-impl` so each use sees the underlying again. In-place: the `GetValue` node itself becomes
    // the unbox call over a fresh `GetValue`, so every reference to the node (including a nested
    // lambda's capture list) picks up the unboxed value.
    for (fid, slot, x, u) in boxed_own_reads {
        let Some(root) = ir.functions[fid as usize].body else {
            continue;
        };
        let mut reads = HashSet::new();
        collect_reachable_scoped(&ir.exprs, &ir.inline_only_fns, root, &mut reads);
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
                sam: Some((interface, method, _)),
                arity,
                ..
            } = e
            {
                let owner = crate::types::type_name(interface);
                if let Some(mangled) = mangle_map.get(&(owner, method.clone(), *arity as usize)) {
                    *method = mangled.clone();
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
                sam: Some((_, method, _)),
                ..
            } = expression
            else {
                continue;
            };
            let Some((params, ret)) = ir.lambda_sam_signature.get(impl_fn) else {
                continue;
            };
            *method = vc_mangle_once(method, params, ret, &callable_under, false, false);
        }
    }
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
                let mangled = vc_mangle(name, params, ret, &callable_under, false, false);
                if &mangled != name {
                    *name = mangled;
                    for p in params.iter_mut() {
                        *p = erase(p, &under);
                    }
                    *ret = erase(ret, &under);
                }
            }
        }
        for e in &mut ir.exprs {
            let IrExpr::Call { callee, .. } = e else {
                continue;
            };
            let (name, params, ret) = match callee {
                Callee::CrossFile {
                    name, params, ret, ..
                } => (name, params, ret),
                _ => continue,
            };
            *name = vc_mangle(name, params, ret, &callable_under, true, false);
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
        let first_call_arg = match fr.dispatch {
            crate::ir::FrDispatch::VirtualUnbound => 1usize,
            _ => 0usize,
        };
        let call_owner = fr.call_owner;
        // The lowerer records the already-selected callable's exact target signature. Do not rebuild it
        // from `(owner, name, arity)`: overloads with equal arity are deliberately indistinguishable by
        // that key, and whichever declaration was visited last would corrupt every other reference.
        let target_decl_params = fr.target_param_tys[first_call_arg..].to_vec();
        let target_decl_ret = fr.target_ret_ty;
        let target_nullable: Vec<bool> = target_decl_params
            .iter()
            .map(|parameter| parameter.is_nullable())
            .collect();
        // A BOUND extension reference on a VALUE-CLASS receiver (`Z(42)::test`, `FrDispatch::StaticBound`)
        // targets a facade static whose leading param is the receiver — that receiver lives in
        // `target_param_tys` (the `target_override`), NOT in the invoke `param_tys`. Mangle against that
        // full sig (so `test` → `test-<hash>`), treat it as a file-class member, and erase THAT sig (so the
        // target descriptor keeps the receiver `int`, not an empty `()`), else the impl calls a
        // non-existent unmangled `test()`.
        let staticbound = matches!(fr.dispatch, crate::ir::FrDispatch::StaticBound);
        let is_file_class = matches!(fr.dispatch, crate::ir::FrDispatch::Static) || staticbound;
        let mangle_params = if staticbound {
            fr.target_param_tys.clone()
        } else {
            target_decl_params.clone()
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
        let mangle_once = |base: &str| {
            vc_mangle_once(
                base,
                &mangle_params,
                &target_decl_ret,
                &callable_under,
                is_file_class,
                fr_suspend,
            )
        };
        let mangled_call_name = mangle_once(&fr.call_name);
        let reflection_base = fr.reflection_name.as_deref().unwrap_or(&fr.fn_name);
        let mangled_reflection_name = mangle_once(reflection_base);
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
        fr.target_param_tys = erase_src
            .iter()
            .map(|t| erase(t, &callable_under))
            .collect();
        fr.target_ret_ty = erase(&erase_ret, &callable_under);
        let target_offset = usize::from(staticbound);
        fr.unbox_params = fr
            .param_tys
            .iter()
            .enumerate()
            .map(|(i, logical)| {
                let target = fr.target_param_tys.get(i + target_offset)?;
                let fq = logical.non_null().obj_internal()?;
                (callable_under.contains_key(&fq) && logical != target).then_some(fq)
            })
            .collect();
        fr.unbox_param_nullable = fr
            .param_tys
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let target_i = i.checked_sub(first_call_arg);
                target_i
                    .and_then(|j| target_nullable.get(j))
                    .copied()
                    .unwrap_or(false)
            })
            .collect();
        if fr
            .unbox_param_nullable
            .iter()
            .enumerate()
            .any(|(i, nullable)| {
                *nullable
                    && fr
                        .target_param_tys
                        .get(i + target_offset)
                        .is_some_and(|target| ir_ty_to_jvm(target).jvm_boxed_ref().is_some())
            })
        {
            return false;
        }
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
    // A covariant-override bridge delegates to the concrete method by name (mangle the target if it was
    // mangled). When the override returns a value class, the concrete method returns the erased underlying,
    // so the bridge boxes the result back to `X` (`box_ret`). Runs even with an empty `mangle_map` — a
    // value-class GETTER bridge (`Child2.prop: Child` through `Base2.prop: Base`) needs the erase+box with
    // no mangling involved.
    {
        for c in &mut ir.classes {
            // A value class keeps its own members' value-class PARAMS boxed (`compareTo(LFoo;)`), so a
            // bridge ON a value class delegates with the boxed param — no unboxing. A REGULAR class's
            // value-class-param method erases that param to the underlying, so its bridge unboxes.
            let owner_is_value = c.is_value;
            let owner_fq = c.fq_name();
            for b in &mut c.bridges {
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
                    && !is_property_getter_bridge_name(&target)
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
                    if bridge_mentions_vc && !is_property_getter_bridge_name(&b.name) {
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
                    // An EXTERNAL value class (`Result`) is held unboxed (`Object`) everywhere in krusty —
                    // when the SUPERTYPE also carries it unboxed the bridge returns the override's already-
                    // `Object` result directly, NO `box-impl`. EXCEPTION: a GENERIC boundary — the supertype
                    // method returns an erased type variable (`fun performOperation(): T` → `Object`). There
                    // kotlinc materializes the box (`Result.box-impl(Object)Lkotlin/Result;`) so the caller
                    // observes the boxed object (its `toString`/identity), and krusty must match: `box_ret`
                    // references the classpath `box-impl`, exactly like a user value class.
                    if supertype_returns_vc {
                        b.concrete_ret = erase(&b.concrete_ret, &under);
                        b.erased_ret = b.concrete_ret.clone();
                    } else {
                        b.box_ret = Some(fq_name);
                        b.concrete_ret = erase(&b.concrete_ret, &under);
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
                    if !is_property_getter_bridge_name(&b.name) {
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
        // A regular class's secondary-`<init>` value-class params erase too (`Test(x: String, s: S)` →
        // `(String, String)`); a value class's own secondary ctors were already consumed into static
        // `constructor-impl`s by `synth_value_members`, so this only touches regular classes.
        for sc in &mut c.secondary_ctors {
            // Record the value-class fact BEFORE erasure: it drives kotlinc's private+marker ABI for
            // this constructor (a synthetic marker-disambiguated ctor keeps its own convention).
            if !sc.synthetic && sc.params.iter().any(is_vc_ty) {
                sc.vc_params = true;
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

    // 4. Rewrite construction / property access — only in bodies that are NOT value-class members
    //    (where value-class values are unboxed). Each body carries its slot types so `prop_access` can
    //    tell an unboxed value-class receiver from a boxed one (a generic-receiver `(X)v` self-cast over an
    //    unboxed `v` is identity, not a box) — same `repr` the box/unbox analysis (step 5) uses.
    // `(root, slots, boxed_slots)`: the third element records slots that are BOXED because of the user
    // value-class member ABI. A pre-erasure non-null `X` normally means an unboxed carrier, so the
    // ordinary slot-type map cannot
    // distinguish these parameters without explicit representation evidence.
    let mut s4_bodies: Vec<(ExprId, HashMap<u32, Ty>, HashSet<u32>)> = Vec::new();
    for (fid, f) in ir.functions.iter().enumerate() {
        // SYNTHESIZED value-class members aren't rewritten (emitted boxed-correct) — EXCEPT `<init>`
        // (field-init/init-block over unboxed ctor params) and `constructor-impl` (moved `init { … }`). A
        // USER member IS rewritten. Its `this` and value-class-typed parameters remain BOXED by the JVM
        // signature chosen above; record every such slot so property reads unbox the runtime value.
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
        ) || vc_sole_getter_fids.contains(&(fid as u32));
        let user_vc_member = is_vc && !synthesized_member;
        if is_vc && !user_vc_member && f.name != "<init>" && f.name != "constructor-impl" {
            continue;
        }
        let mut boxed_slots = HashSet::new();
        if user_vc_member {
            let base = u32::from(f.dispatch_receiver.is_some() && !f.is_static);
            if base == 1 {
                boxed_slots.insert(0);
            }
            for (index, parameter) in orig_params[fid].iter().enumerate() {
                if is_vc_ty(parameter) {
                    boxed_slots.insert(base + index as u32);
                }
            }
        }
        if let Some(root) = f.body {
            s4_bodies.push((root, slot_types[fid].clone(), boxed_slots.clone()));
        }
        if let Some(defaults) = ir.param_defaults(fid as u32) {
            for &root in defaults.iter().flatten() {
                s4_bodies.push((root, slot_types[fid].clone(), boxed_slots.clone()));
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
                body_slot_map(&ir.exprs, root, &orig_ctor_args[cidx]),
                HashSet::new(),
            ));
        }
        for (sidx, sc) in c.secondary_ctors.iter().enumerate() {
            let params = &orig_secondary[cidx][sidx];
            let slots = secondary_ctor_slot_map(&ir.exprs, sc, params);
            if let Some(b) = sc.body {
                s4_bodies.push((b, slots.clone(), HashSet::new()));
            }
            for &statement in &sc.delegate_prelude {
                s4_bodies.push((statement, slots.clone(), HashSet::new()));
            }
            for &a in &sc.delegate_args {
                s4_bodies.push((a, slots.clone(), HashSet::new()));
            }
            for &default in sc.defaults.iter().flatten() {
                s4_bodies.push((default, slots.clone(), HashSet::new()));
            }
        }
        for entry in &c.enum_entries {
            for &a in &entry.args {
                s4_bodies.push((a, HashMap::new(), HashSet::new()));
            }
        }
        for &a in &c.super_args {
            s4_bodies.push((
                a,
                body_slot_map(&ir.exprs, a, &orig_ctor_args[cidx]),
                HashSet::new(),
            ));
        }
    }
    // Top-level property initializers run in the facade `<clinit>` (static, no params). A value-class
    // construction here (`val p = arrayListOf(X(0))`) must rewrite `new X` → `constructor-impl` too;
    // otherwise a private `<init>` leaks an `IllegalAccessError` from `<clinit>`.
    for s in &ir.statics {
        s4_bodies.push((s.init, HashMap::new(), HashSet::new()));
    }
    // Map each reachable target expr to its body's slot map. A real lambda body belongs only to its
    // lifted function; traversing it from the enclosing `Lambda` expression would interpret the same
    // slot indices in the wrong function scope.
    let mut target_slots: HashMap<ExprId, usize> = HashMap::new();
    for (bi, (root, _, _)) in s4_bodies.iter().enumerate() {
        let mut reach = HashSet::new();
        collect_reachable_scoped(&ir.exprs, &ir.inline_only_fns, *root, &mut reach);
        for id in reach {
            target_slots.entry(id).or_insert(bi);
        }
    }
    // Process in ascending ExprId order: a child (inner `.z`, created first → lower id) is rewritten
    // before its parent (outer `.x`), so a nested property-access chain's `prop_access` always sees the
    // child's already-rewritten (`unbox-impl`/coercion) form and decides box/unbox deterministically.
    let mut targets: Vec<ExprId> = target_slots.keys().copied().collect();
    targets.sort_unstable();
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
        let boxed_slots = &body.2;
        let i = id as usize;
        if let IrExpr::New {
            internal,
            args,
            ctor_params,
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
            for (&argument, parameter) in args.iter().zip(params) {
                let Target::UnboxedX(value_class) = target(parameter, &under) else {
                    continue;
                };
                let explicitly_boxed_argument = matches!(
                    &ir.exprs[argument as usize],
                    IrExpr::GetValue(argument_slot) if boxed_slots.contains(argument_slot)
                );
                if (explicitly_boxed_argument
                    || is_boxed_vc(
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
                    ))
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
                descriptor: String,
                args: Vec<ExprId>,
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
        let rw = match &ir.exprs[i] {
            // `new X(args)` → `X.constructor-impl(args): U`. The return is the underlying `U`; the
            // PARAMETER types come from the actual constructor arguments (a secondary constructor's
            // signature differs from the primary, e.g. `Sc(String)` delegating to `Sc(Int)`).
            IrExpr::New {
                internal,
                args,
                ctor_params,
                ctor_desc: None,
            } if under.contains_key(internal) => {
                let owner = *internal;
                let u = under
                    .get(&owner)
                    .map(|t| erase(t, &under))
                    .unwrap_or(Ty::Error);
                // A krusty-unboxed value class has ONE underlying param. No args + exactly one declared
                // param = that sole (defaulted) param omitted (`Id()`), realized by the
                // `constructor-impl$default` synthetic with mask `1`. Guarded on `len() == 1` so a
                // multi-field value class (experimental `@JvmInlineMultiFieldValueClasses`, whose mask
                // would need several bits) does NOT take this single-bit path. Any args = ordinary
                // `constructor-impl(args)`.
                if args.is_empty() && ctor_params.as_ref().is_some_and(|p| p.len() == 1) {
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
                    .is_some_and(|fq_name| under.contains_key(&fq_name))
                && !matches!(repr_ctx.repr(*arg), Repr::Boxed(_)) =>
            {
                let fq_name = type_operand.non_null().obj_internal().unwrap();
                let u = under
                    .get(&fq_name)
                    .map(|t| erase(t, &under))
                    .unwrap_or(Ty::Error);
                let owner_rendered = fq_name.render();
                Some(Rw::Ctor(IrExpr::Call {
                    callee: Callee::Static {
                        owner: fq_name,
                        name: "box-impl".to_string(),
                        descriptor: format!("({})L{owner_rendered};", desc(&u)),
                        inline: InlineKind::None,
                    },
                    dispatch_receiver: None,
                    args: vec![*arg],
                }))
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
                receiver,
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
                    descriptor: ir_method_desc(&function.params, &function.ret),
                    args: args.iter().filter_map(|arg| *arg).collect(),
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
                let dummy = ir.add_expr(IrExpr::Const(crate::ir::IrConst::zero_for_value_type(u)));
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
                    boxed_slots,
                ))
            }
            Some(Rw::ImplCall {
                receiver,
                owner,
                name,
                descriptor,
                mut args,
            }) => {
                let explicitly_boxed = matches!(
                    &ir.exprs[receiver as usize],
                    IrExpr::GetValue(slot) if boxed_slots.contains(slot)
                );
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
                let receiver = if explicitly_boxed || inferred_boxed {
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
                args.insert(0, receiver);
                Some(IrExpr::Call {
                    callee: Callee::Static {
                        owner,
                        name,
                        descriptor,
                        inline: InlineKind::None,
                    },
                    dispatch_receiver: None,
                    args,
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
    // `(cast expr, underlying)` — a `checkcast X?` to a NULLABLE reference-underlying value class is
    // retargeted to its underlying (`Str?` → `checkcast String`): there is no `Str` instance for an
    // unboxed value, so casting to the box class would `ClassCastException`.
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
        if vc_methods.contains(&(fid as u32)) {
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
            bodies.push((root, body_slot_map(&ir.exprs, root, &orig_ctor_args[cidx])));
        }
        // A regular class's secondary `<init>` body + its `this(…)` delegation args run over the secondary
        // params — box/unbox their value-class accesses/constructions.
        for (sidx, sc) in c.secondary_ctors.iter().enumerate() {
            let params = &orig_secondary[cidx][sidx];
            let slots = secondary_ctor_slot_map(&ir.exprs, sc, params);
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
        for &a in &c.super_args {
            bodies.push((a, body_slot_map(&ir.exprs, a, &orig_ctor_args[cidx])));
        }
    }
    // Top-level property initializers (facade `<clinit>`, static) — box/unbox their value-class accesses
    // and boundary constructions just like any function body.
    for s in &ir.statics {
        bodies.push((s.init, HashMap::new()));
    }
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
        collect_reachable_scoped(&ir.exprs, &ir.inline_only_fns, root, &mut reach);
        for id in reach {
            if let IrExpr::NotNullAssert { operand } = &ir.exprs[id as usize] {
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
            }
            // A value-class property accessor is a static `-impl` over the unboxed carrier, regardless
            // of whether this compilation or a dependency declared it. The sole stored property was
            // already rewritten to identity; every remaining semantic property read keeps the carrier
            // representation expected by its selected accessor.
            if let IrExpr::PropertyRead {
                receiver, owner, ..
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
                .is_some_and(|fq| under.get(&fq).is_some_and(|u| u.is_reference()));
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
                        owner, descriptor, ..
                    }
                    | Callee::Static {
                        owner, descriptor, ..
                    }
                    | Callee::Special {
                        owner, descriptor, ..
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
                    // A VC-owned call boxes an unboxed value-class arg at a parameter that is the boxed VC
                    // itself (`ZN.constructor-impl(LZ1;)`) OR an `Object` underlying (`Result<Result<Int>>`
                    // wraps a `Result` into its `Any?` field — the inner value must box to stay a `Result`).
                    // The `repr(arg) == Unboxed` gate above keeps a VC's `equals-impl0(U, U)` underlying args
                    // (which are `NotVc`) untouched.
                    let box_here = if vc_owned {
                        ptypes
                            .get(k)
                            .is_some_and(|p| *p == format!("L{x};") || p == "Ljava/lang/Object;")
                    } else {
                        // A reference param boxes an unboxed value-class arg — UNLESS the param IS the value
                        // class's OWN erased underlying (a mangled `getFor-<hash>(String)` for `Aid(String)`):
                        // there the value is already its native form and passes UNBOXED (identity). This only
                        // holds for a DISTINCT non-`Object` underlying: when the underlying erases to `Object`
                        // (`Value(Any)`) the descriptor `Ljava/lang/Object;` no longer tells a concrete
                        // VC-param apart from a generic/erased `T` slot (`.let(Foo::foo)`'s boxed receiver),
                        // and kotlinc boxes there — so only exclude when the underlying is a concrete type.
                        let under_desc = under.get(&x).map(|u| desc(&erase(u, &under)));
                        let own_underlying = ptypes.get(k).map(String::as_str)
                            == under_desc.as_deref()
                            && under_desc.as_deref() != Some("Ljava/lang/Object;");
                        refs.get(k).copied().unwrap_or(false) && !own_underlying
                    };
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
                        .zip(targets.iter())
                        .map(|(a, p)| (*a, p.clone()))
                        .collect()
                }
                IrExpr::Call {
                    callee: Callee::Local(cfid) | Callee::ClassStatic { function: cfid, .. },
                    args,
                    ..
                } => args
                    .iter()
                    .zip(orig_params[*cfid as usize].iter())
                    .map(|(a, p)| (*a, p.clone()))
                    .collect(),
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
    // A superclass invocation is not an IR `Call` node: `super_args` are emitted directly by the class
    // constructor. Apply the same semantic boundary operation using the checker-selected parameter types
    // retained beside those arguments.
    for (class_index, class) in ir.classes.iter().enumerate() {
        for (&argument, &parameter) in class.super_args.iter().zip(&class.super_ctor_params) {
            let slots = body_slot_map(&ir.exprs, argument, &orig_ctor_args[class_index]);
            let repr_ctx = ReprCtx {
                exprs: &ir.exprs,
                rets: &orig_rets,
                fields: &orig_fields,
                slots: &slots,
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
    // A cast that was STRIPPED (its operand is already the underlying) is now a `Block` — the retarget's
    // `TypeOp` match simply skips it, so a node in both lists is harmless; retarget only rewrites casts
    // that survived.
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
        if vc_methods.contains(&(fid as u32)) {
            // A value-class MEMBER returns the BOXED value-class form (its signature keeps the value
            // class — see the `vc_member && is_vc_ty(ret)` guard above). If its declared return is a
            // value class, box the tail: `IC1.invoke(): IC = IC(a)` produces the unboxed underlying via
            // `constructor-impl`, but the member must hand back a boxed `IC`. `box_tail` only boxes an
            // unboxed tail, so a member already returning a box is untouched.
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
                // …EXCEPT a `suspend fun`: the coroutine pass erases its return to `Object`, so the value
                // leaves BOXED (kotlinc: `box-impl` before the CPS `areturn`). Box the tail exactly like a
                // lambda's erased `Object` result and record the class, so the resume side can undo it.
                if let Some(body) = ir.functions[fid].body {
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
                    ir.suspend_boxed_value_class_returns.insert(fid as u32, x);
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
        IrExpr::Call {
            callee: Callee::Local(fid) | Callee::ClassStatic { function: fid, .. },
            ..
        } if calls => match rets.get(*fid as usize) {
            Some(Ty::Obj(fq_name, _)) if under.contains_key(fq_name) => Some(*fq_name),
            _ => None,
        },
        IrExpr::Block { value: Some(v), .. } => unboxed_vc_class(exprs, rets, under, *v, calls),
        IrExpr::NotNullAssert { operand } if calls => {
            unboxed_vc_class(exprs, rets, under, *operand, calls)
        }
        _ => None,
    }
}

enum BoxOp {
    Box(TypeName),
    BoxNull(TypeName),
    Unbox(TypeName),
    UnboxNull(TypeName),
    Narrow(TypeName),
}

fn value_tails(exprs: &[IrExpr], id: ExprId, out: &mut Vec<ExprId>) {
    match &exprs[id as usize] {
        IrExpr::When { branches } => {
            for &(_, result) in branches {
                value_tails(exprs, result, out);
            }
        }
        IrExpr::Block {
            value: Some(value), ..
        } => value_tails(exprs, *value, out),
        IrExpr::Block { value: None, stmts } => {
            if let Some(&last) = stmts.last() {
                value_tails(exprs, last, out);
            }
        }
        _ => out.push(id),
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
        IrExpr::Call {
            callee: Callee::Local(fid) | Callee::ClassStatic { function: fid, .. },
            ..
        } => rets.get(*fid as usize).is_some_and(non_null_ty),
        IrExpr::GetValue(i) => slots.get(i).is_some_and(non_null_ty),
        IrExpr::Block { value: Some(v), .. } => operand_nonnull(exprs, rets, fields, slots, *v),
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
    match &exprs[id as usize] {
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
        IrExpr::Call {
            callee: Callee::Local(fid) | Callee::ClassStatic { function: fid, .. },
            ..
        } => rets
            .get(*fid as usize)
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
        IrExpr::NotNullAssert { operand } => repr(
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
    boxed_slots: &HashSet<u32>,
) -> IrExpr {
    let u = under.get(&x).map(|t| erase(t, under)).unwrap_or(Ty::Error);
    // A user value-class member keeps both its receiver and every value-class-typed parameter boxed. A
    // sole-property read from any of those slots must therefore call `unbox-impl`; treating only slot 0
    // specially leaves `fun member(value: X) = value.field` trying to cast the `X` box directly to its
    // carrier. Otherwise use the representation analysis shared by every other producer. The resulting
    // coercion tells later analysis that the property itself has the underlying representation.
    let explicitly_boxed = matches!(&ir.exprs[receiver as usize],
        IrExpr::GetValue(index) if boxed_slots.contains(index));
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
        "prop access {} receiver={receiver} {:?} result={result:?} underlying={u:?} explicit_box={explicitly_boxed} inferred_box={inferred_boxed}",
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
    let inner = if explicitly_boxed || inferred_boxed {
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
    IrExpr::TypeOp {
        op: crate::ir::IrTypeOp::ImplicitCoercion,
        arg: inner,
        // A generic value class keeps an erased `Object` carrier, but an applied property read
        // (`X<Int>.x`) has a concrete Kotlin result. Preserve that selected result so this coercion
        // performs the required `Integer` unbox / reference cast instead of degrading it back to Any.
        type_operand: if result
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
        },
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
        IrExpr::Call {
            callee: Callee::Local(fid) | Callee::ClassStatic { function: fid, .. },
            ..
        } => funcs.get(*fid as usize).is_some_and(|f| is_x(&f.ret)),
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
        IrExpr::NotNullAssert { operand } => is_boxed_vc(
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
            ..
        } => is_boxed_vc(
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
        ),
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
    let orig = ir.exprs[id as usize].clone();
    let new_id = ir.exprs.len() as ExprId;
    ir.exprs.push(orig);
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
}

/// Null-safe box: replace the expr at `id` with `{ tmp = <orig>; if (tmp == null) null else box-impl(tmp) }`
/// — boxing a nullable (reference-underlying) value class without hitting the ctor null-check on `null`.
fn box_wrap_nullable(ir: &mut IrFile, id: ExprId, x: TypeName, under: &Under, slot: u32) {
    let orig = ir.exprs[id as usize].clone();
    let orig_id = ir.exprs.len() as ExprId;
    ir.exprs.push(orig);
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

/// Whether the erased type occupies a JVM *reference* slot. A non-null Kotlin primitive class
/// (`kotlin/Int`, `kotlin/Boolean`, …) emits as a JVM primitive (`I`, `Z`, …), so it is NOT a
/// reference; its NULLABLE form is the boxed wrapper (`Integer`), which is. Everything else that is a
/// `Class` is a reference.
fn is_ref(t: &Ty) -> bool {
    if t.is_nullable() {
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
fn synth_value_members(ir: &mut IrFile, class_id: u32, under: &Under, has_init: bool) -> bool {
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
        ir.open_methods.insert(cfid); // kotlinc emits `constructor-impl` `public static` (non-final)
                                      // A default on the single underlying property (`ItemId(val value: String = …)`) → register it as
                                      // `constructor-impl`'s param default so the backend emits `constructor-impl$default(U, int, marker)`
                                      // (kotlinc's synthetic). The default was lowered in the static `constructor-impl` frame (param @0).
        if let Some(def) = ir.value_ctor_default(&internal) {
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

    // A secondary constructor becomes a static `constructor-impl` overload.
    let secs = std::mem::take(&mut ir.classes[class_id as usize].secondary_ctors);
    if !secs.is_empty() {
        let udesc = type_descriptor(ir_ty_to_jvm(&u_ir));
        for sc in secs {
            let crate::ir::CtorDelegateTarget::This {
                target_params,
                to_primary: true,
                default_masks,
            } = &sc.delegate
            else {
                return false;
            };
            if target_params.as_slice() != [u_ir]
                || !default_masks.is_empty()
                || sc.delegate_args.len() != 1
            {
                return false;
            }

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
                    descriptor: format!("({udesc}){udesc}"),
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
        collect_reachable(&ir.exprs, root, &mut reachable);
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
    collect_reachable(&ir.exprs, root, &mut reachable);
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
    collect_reachable(&ir.exprs, root, &mut reach);
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
            if let Some(u) = existing_type_name(fq).and_then(|name| under.get(&name)) {
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

fn is_property_getter_bridge_name(name: &str) -> bool {
    name.starts_with("get")
        || name
            .strip_prefix("is")
            .is_some_and(|s| s.chars().next().is_some_and(char::is_uppercase))
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

/// Like [`collect_reachable], but does NOT descend into a REAL closure's lambda body — only its
/// captures. A non-inline lambda's body is a separate function (`impl_fn`) with its OWN value-index
/// numbering and slot types; reaching it from the enclosing function would let the enclosing scope's
/// slot-typed repr analysis mis-read the lambda's value-indices (e.g. box a value at a slot the
/// enclosing function happens to hold a value class in). An INLINE-only lambda IS spliced into this
/// scope, so its body is still traversed. Used by the per-function slot-typed box/unbox passes.
fn collect_reachable_scoped(
    exprs: &[IrExpr],
    inline_only: &HashSet<u32>,
    root: ExprId,
    out: &mut HashSet<ExprId>,
) {
    if !out.insert(root) {
        return;
    }
    if let IrExpr::Lambda {
        impl_fn,
        captures,
        inline_body,
        ..
    } = &exprs[root as usize]
    {
        for &c in captures {
            collect_reachable_scoped(exprs, inline_only, c, out);
        }
        if inline_only.contains(impl_fn) {
            if let Some(b) = inline_body {
                collect_reachable_scoped(exprs, inline_only, *b, out);
            }
        }
        return;
    }
    crate::ir::for_each_child(exprs, root, &mut |c| {
        collect_reachable_scoped(exprs, inline_only, c, out)
    });
}
