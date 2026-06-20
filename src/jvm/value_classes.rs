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

use crate::ir::{Callee, ExprId, IrCatch, IrExpr, IrFile, IrType};
use crate::jvm::ir_emit::ir_ty_to_jvm;
use std::collections::{HashMap, HashSet};

/// Lower all `@JvmInline value class` usage in `ir` to the JVM's unboxed representation: erase the
/// value-class type to its single field's type, rewrite construction/sole-property access, and insert
/// box/unbox at the representation boundaries this pass models. The `bool` result is reserved for a
/// future structural bail; today it always returns `true` (the pass never skips a value-class file —
/// shapes it does not yet handle are emitted as-is, surfacing as a conformance FAIL to be fixed, not a
/// silent skip).
#[must_use]
pub fn lower_value_classes(ir: &mut IrFile) -> bool {
    // internal name → underlying (single-field) type, before erasure. NOTE: the `Object` underlying for a
    // generic value class is a deliberate approximation — the correct BOUND (`S<T: String>` → `String`)
    // BREAKS more `*Generic` files than it fixes (their lambda boxing / list iteration / equality assume the
    // `Object` repr). Metadata (`type_param_bounds`/`field_type_params`) stays ready for when downstream is.
    // We keep the `Object` underlying TYPE for a generic value class, but DO carry the nullability of its
    // type-parameter bound (`X<T: String?>` → null-capable `Object?`): that's what `nullable_is_boxed`
    // and the `checkNotNullParameter` elision key on, and unlike using the bound itself it doesn't disturb
    // the `Object`-repr that the `*Generic` files assume.
    let under: HashMap<String, IrType> = ir
        .classes
        .iter()
        .filter(|c| c.is_value)
        .filter_map(|c| {
            c.fields.first().map(|(_, t)| {
                // A type-parameter field is null-capable (the `Object` underlying can hold `null`) UNLESS
                // it has an explicit NON-NULL bound: `<T>`/`<T: Any?>`/`<T: String?>` → null-capable;
                // `<T: String>` → not. (Kotlin's default upper bound is the nullable `Any?`.)
                let null_capable = c
                    .field_type_params
                    .first()
                    .and_then(|o| o.as_ref())
                    .is_some_and(|name| {
                        match c.type_param_bounds.iter().find(|(n, _)| n == name) {
                            Some((_, b)) => matches!(b, IrType::Class { nullable: true, .. }),
                            None => true,
                        }
                    });
                let u = if null_capable {
                    mark_nullable_ty(t)
                } else {
                    t.clone()
                };
                (c.fq_name.clone(), u)
            })
        })
        .collect();
    if under.is_empty() {
        return true;
    }
    let value_class_ids: Vec<u32> = (0..ir.classes.len() as u32)
        .filter(|&i| ir.classes[i as usize].is_value)
        .collect();

    // Synthesize each value class's `-impl`/`equals`/`hashCode`/`toString` members up front (a JVM
    // concern — `ir_lower` only emits the plain single-field class). Done before the analysis below so
    // they participate in `vc_methods`/erasure like any other method.
    for cid in value_class_ids {
        // A real value class always has its single backing field; guard malformed fieldless input.
        if ir.classes[cid as usize].fields.is_empty() {
            continue;
        }
        let has_init = ir.classes[cid as usize].init_body.is_some();
        synth_value_members(ir, cid, &under, has_init);
    }

    // Pre-erasure signatures, so box/unbox at call boundaries can see `Object`/generic param/field
    // types (which erasure leaves alone but values flowing in must be boxed to reach).
    let orig_params: Vec<Vec<IrType>> = ir.functions.iter().map(|f| f.params.clone()).collect();
    let orig_fields: Vec<Vec<IrType>> = ir
        .classes
        .iter()
        .map(|c| c.fields.iter().map(|(_, t)| t.clone()).collect())
        .collect();
    // Pre-erasure constructor-parameter types per class (parallel to `ir.classes`) — the slot types for
    // an `init { … }` block's box/unbox analysis (slot 0 = `this`, slots 1.. = the ctor params).
    let orig_ctor_args: Vec<Vec<IrType>> = ir
        .classes
        .iter()
        .map(|c| c.ctor_args.iter().map(|(t, _)| t.clone()).collect())
        .collect();
    // Pre-erasure secondary-constructor parameter types (class → ctor → params) — slot types for a
    // regular class's secondary-`<init>` body/delegation box/unbox (slot 0 = `this`, slots 1.. = params).
    let orig_secondary: Vec<Vec<Vec<IrType>>> = ir
        .classes
        .iter()
        .map(|c| c.secondary_ctors.iter().map(|s| s.params.clone()).collect())
        .collect();

    // Per-class id metadata (parallel to ir.classes).
    let is_vc: Vec<bool> = ir.classes.iter().map(|c| c.is_value).collect();
    let fq: Vec<String> = ir.classes.iter().map(|c| c.fq_name.clone()).collect();
    // Getter method name for each value class's sole field (`getV`), to recognize property access.
    let getter: Vec<Option<String>> = ir
        .classes
        .iter()
        .map(|c| {
            if c.is_value {
                c.fields.first().map(|(n, _)| getter_name(n))
            } else {
                None
            }
        })
        .collect();

    // Each value class's getter name keyed by its internal name (`A2` → `getValue`) — to recognize a
    // sole-property access emitted as a resolved `invokevirtual X.getV()`.
    let vc_getters: HashMap<String, String> = ir
        .classes
        .iter()
        .filter(|c| c.is_value)
        .filter_map(|c| {
            c.fields
                .first()
                .map(|(n, _)| (c.fq_name.clone(), getter_name(n)))
        })
        .collect();

    // Interfaces that value classes implement — a function returning one of these (or `Any`) boxes a
    // value-class tail so virtual/interface dispatch works.
    let vc_interfaces: HashSet<String> = ir
        .classes
        .iter()
        .filter(|c| c.is_value)
        .flat_map(|c| c.interfaces.iter().cloned())
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
    let orig_rets: Vec<IrType> = ir.functions.iter().map(|f| f.ret.clone()).collect();
    let slot_types: Vec<HashMap<u32, IrType>> = ir
        .functions
        .iter()
        .map(|f| {
            let mut m: HashMap<u32, IrType> = HashMap::new();
            let base = u32::from(f.dispatch_receiver.is_some() && !f.is_static);
            for (i, p) in f.params.iter().enumerate() {
                m.insert(base + i as u32, p.clone());
            }
            if let Some(root) = f.body {
                let mut reach = HashSet::new();
                collect_reachable(&ir.exprs, root, &mut reach);
                for id in reach {
                    if let IrExpr::Variable { index, ty, .. } = &ir.exprs[id as usize] {
                        m.insert(*index, ty.clone());
                    }
                }
            }
            m
        })
        .collect();

    // 1. Erase signatures + drop null-checks on params that erased to a non-reference. `box-impl`
    //    returns the boxed `X` (the one position not erased).
    let is_vc_ty =
        |t: &IrType| matches!(t, IrType::Class { fq_name, .. } if under.contains_key(fq_name));
    // `(owner-internal, plain name, arity)` → mangled name, for rewriting resolved-by-name calls
    // (`super.f(vc)`, an interface method) to the value-class-mangled method.
    let mut mangle_map: HashMap<(String, String, usize), String> = HashMap::new();
    for (fid, f) in ir.functions.iter_mut().enumerate() {
        let is_box_impl = f.name == "box-impl";
        // A USER value-class member function's body runs on the BOXED object; its value-class-typed
        // parameters/return stay boxed (a sibling member call passes `this` — a box — directly). The
        // SYNTHESIZED members (`-impl`, `equals`/`hashCode`/`toString`, the getter, `<init>`) operate on
        // the underlying representation, so they erase like any other function.
        let synthesized = matches!(
            f.name.as_str(),
            "box-impl"
                | "unbox-impl"
                | "constructor-impl"
                | "equals-impl0"
                | "equals"
                | "hashCode"
                | "toString"
                | "<init>"
        ) || f.name.starts_with("get");
        let vc_member = !synthesized && vc_methods.contains(&(fid as u32));
        // Mangle a USER function whose (pre-erasure) signature mentions a value class — kotlinc's
        // `base-<hash>`. Index-resolved `MethodCall`s pick this up automatically; name-resolved calls
        // (super/interface) are rewritten below via `mangle_map`.
        if !synthesized {
            let mangled = vc_mangle(&f.name, &orig_params[fid], &orig_rets[fid], &under);
            if mangled != f.name {
                if let Some(owner) = &f.dispatch_receiver {
                    mangle_map.insert(
                        (owner.clone(), f.name.clone(), orig_params[fid].len()),
                        mangled.clone(),
                    );
                }
                f.name = mangled;
            }
        }
        for p in &mut f.params {
            if !(vc_member && is_vc_ty(p)) {
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
                if let Some(mangled) = mangle_map.get(&(owner.clone(), name.clone(), args.len())) {
                    *name = mangled.clone();
                    *descriptor = erase_descriptor(descriptor, &under);
                }
            }
        }
    }
    // A covariant-override bridge delegates to the concrete method by name (mangle the target if it was
    // mangled). When the override returns a value class, the concrete method returns the erased underlying,
    // so the bridge boxes the result back to `X` (`box_ret`). Runs even with an empty `mangle_map` — a
    // value-class GETTER bridge (`Child2.prop: Child` through `Base2.prop: Base`) needs the erase+box with
    // no mangling involved.
    {
        for c in &mut ir.classes {
            for b in &mut c.bridges {
                let target = b.target_name.clone().unwrap_or_else(|| b.name.clone());
                if let Some(m) =
                    mangle_map.get(&(c.fq_name.clone(), target, b.concrete_params.len()))
                {
                    b.target_name = Some(m.clone());
                }
                if let IrType::Class {
                    fq_name,
                    nullable: false,
                    ..
                } = &b.concrete_ret.clone()
                {
                    if under.contains_key(fq_name) {
                        if b.target_name.is_none() {
                            b.target_name = Some(b.name.clone());
                        }
                        // The bridge satisfies the (mangled) SUPERTYPE method, so it takes that method's
                        // mangled name: `vc_mangle` over the override's params + the SUPERTYPE's declared
                        // return. A VC param (`foo(i: Marker)`) mangles by the param; a literal-VC return
                        // (`fun bar(): Gx`) also mangles by the return; a generic `T` return (erased
                        // `Object`) does not.
                        b.name = vc_mangle(&b.name, &b.concrete_params, &b.erased_ret, &under);
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
                        // Whether the SUPERTYPE method returns the value class LITERALLY (`fun bar(): Gx` →
                        // kotlinc mangles + erases its return) vs a generic `T` erased to `Object`
                        // (`fun foo(): T`). The former → bridge returns the erased underlying, NO box; the
                        // latter → bridge BOXES the value class back to `Object`.
                        let supertype_returns_vc = matches!(&b.erased_ret,
                            IrType::Class { fq_name, .. } if under.contains_key(fq_name));
                        if supertype_returns_vc {
                            b.concrete_ret = erase(&b.concrete_ret, &under);
                            b.erased_ret = b.concrete_ret.clone();
                        } else {
                            b.box_ret = Some(fq_name.clone());
                            b.concrete_ret = erase(&b.concrete_ret, &under);
                        }
                    }
                }
            }
        }
    }

    // 2. Erase class field + ctor-arg types; drop the `<init>` null-check on a constructor parameter
    //    that erased to a non-reference (a value-class ctor arg `a: Na` → `int` can't be null-checked).
    for c in &mut ir.classes {
        // `ctor_param_checks` is parallel to the first `ctor_args`. Drop the `<init>` null-check on a param
        // that erased to a non-reference, OR whose value-class underlying chain is null-capable
        // (`ZN2(val z: ZN)` where `ZN(val z: Z1?)` → the value can be null, so kotlinc emits no check).
        for (k, a) in c.ctor_args.iter().enumerate() {
            if !is_ref(&erase(&a.0, &under)) || vc_underlying_nullable(&a.0, &under) {
                if let Some(chk) = c.ctor_param_checks.get_mut(k) {
                    *chk = None;
                }
            }
        }
        for fld in &mut c.fields {
            fld.1 = erase(&fld.1, &under);
        }
        for a in &mut c.ctor_args {
            a.0 = erase(&a.0, &under);
        }
        // A regular class's secondary-`<init>` value-class params erase too (`Test(x: String, s: S)` →
        // `(String, String)`); a value class's own secondary ctors were already consumed into static
        // `constructor-impl`s by `synth_value_members`, so this only touches regular classes.
        for sc in &mut c.secondary_ctors {
            for p in &mut sc.params {
                *p = erase(p, &under);
            }
        }
    }

    // 3. Erase every type carried inside an expression (locals, casts, vararg/array elements, …).
    //    Inside a value-class member body, an `is X`/`(X)other` whose type IS a value class must stay
    //    the BOXED class (the synthesized `equals` checks/casts the box) — keep it; everything else
    //    (including field-value operations over a nested value-class underlying) erases normally.
    for (i, e) in ir.exprs.iter_mut().enumerate() {
        let keep_box = vc_body_exprs.contains(&(i as u32));
        match e {
            IrExpr::Variable { ty, .. } => *ty = erase(ty, &under),
            IrExpr::TypeOp { type_operand, .. } => {
                // `is X` / `as X` on a value class keep the BOXED type — the box is the only object that
                // is `instanceof X`, and a `checkcast X` of an `Any` yields a box the property access
                // then unboxes. (Erasing would turn `as X` into `as <underlying>`, mis-typing the value.)
                let is_vc_ty = matches!(type_operand, IrType::Class { fq_name, .. } if under.contains_key(fq_name));
                if !is_vc_ty {
                    *type_operand = erase(type_operand, &under);
                }
                let _ = keep_box;
            }
            IrExpr::New {
                ctor_params: Some(ps),
                ..
            } => ps.iter_mut().for_each(|p| *p = erase(p, &under)),
            IrExpr::InvokeFunction { ret, .. } => *ret = erase(ret, &under),
            // An `Array<X>` of a value class is a reference array of the BOXED `X` (kotlinc) — keep the
            // element type boxed (don't erase to the underlying); elements are `box-impl`'d when stored.
            IrExpr::Vararg { element_type, .. } | IrExpr::NewArray { element_type, .. } => {
                if !matches!(element_type, IrType::Class { fq_name, .. } if under.contains_key(fq_name))
                {
                    *element_type = erase(element_type, &under)
                }
            }
            IrExpr::RefNew { elem, .. }
            | IrExpr::RefGet { elem, .. }
            | IrExpr::RefSet { elem, .. } => *elem = erase(elem, &under),
            IrExpr::Try { result, .. } => *result = erase(result, &under),
            _ => {}
        }
    }

    // 4. Rewrite construction / property access — only in bodies that are NOT value-class members
    //    (where value-class values are unboxed). Each body carries its slot types so `prop_access` can
    //    tell an unboxed value-class receiver from a boxed one (a generic-receiver `(X)v` self-cast over an
    //    unboxed `v` is identity, not a box) — same `repr` the box/unbox analysis (step 5) uses.
    // `(root, slots, boxed_this)` — `boxed_this` = the slot holding a BOXED value-class `this` (a USER
    // value-class member runs on the boxed object), so `prop_access` `unbox-impl`s a `this.field` read.
    let synthesized_member = |name: &str| {
        matches!(
            name,
            "box-impl"
                | "unbox-impl"
                | "constructor-impl"
                | "equals-impl0"
                | "equals"
                | "hashCode"
                | "toString"
                | "<init>"
        ) || name.starts_with("get")
    };
    let mut s4_bodies: Vec<(ExprId, HashMap<u32, IrType>, Option<u32>)> = Vec::new();
    for (fid, f) in ir.functions.iter().enumerate() {
        // SYNTHESIZED value-class members aren't rewritten (emitted boxed-correct) — EXCEPT `<init>`
        // (field-init/init-block over unboxed ctor params) and `constructor-impl` (moved `init { … }`). A
        // USER member IS rewritten, with `this` (slot 0) treated as a BOXED value class.
        let is_vc = vc_methods.contains(&(fid as u32));
        let user_vc_member = is_vc && !synthesized_member(&f.name);
        if is_vc && !user_vc_member && f.name != "<init>" && f.name != "constructor-impl" {
            continue;
        }
        let boxed_this =
            (user_vc_member && f.dispatch_receiver.is_some() && !f.is_static).then_some(0);
        if let Some(root) = f.body {
            s4_bodies.push((root, slot_types[fid].clone(), boxed_this));
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
                None,
            ));
        }
        for (sidx, sc) in c.secondary_ctors.iter().enumerate() {
            let params = &orig_secondary[cidx][sidx];
            if let Some(b) = sc.body {
                s4_bodies.push((b, body_slot_map(&ir.exprs, b, params), None));
            }
            for &a in &sc.delegate_args {
                s4_bodies.push((a, body_slot_map(&ir.exprs, a, params), None));
            }
        }
        for (_, args) in &c.enum_entries {
            for &a in args {
                s4_bodies.push((a, HashMap::new(), None));
            }
        }
        for &a in &c.super_args {
            s4_bodies.push((a, body_slot_map(&ir.exprs, a, &orig_ctor_args[cidx]), None));
        }
    }
    // Map each reachable target expr to its body's slot map (first body wins; bodies don't overlap).
    let mut target_slots: HashMap<ExprId, usize> = HashMap::new();
    for (bi, (root, _, _)) in s4_bodies.iter().enumerate() {
        let mut reach = HashSet::new();
        collect_reachable(&ir.exprs, *root, &mut reach);
        for id in reach {
            target_slots.entry(id).or_insert(bi);
        }
    }
    // Process in ascending ExprId order: a child (inner `.z`, created first → lower id) is rewritten
    // before its parent (outer `.x`), so a nested property-access chain's `prop_access` always sees the
    // child's already-rewritten (`unbox-impl`/coercion) form and decides box/unbox deterministically.
    let mut targets: Vec<ExprId> = target_slots.keys().copied().collect();
    targets.sort_unstable();
    for &id in &targets {
        let body = &s4_bodies[target_slots[&id]];
        let slots = &body.1;
        let boxed_this = body.2;
        let i = id as usize;
        // First decide the rewrite WITHOUT holding a mutable borrow (so `prop_access` can `add_expr`).
        enum Rw {
            Ctor(IrExpr),
            Prop(ExprId, String),
        }
        let rw = match &ir.exprs[i] {
            // `new X(args)` → `X.constructor-impl(args): U`. The return is the underlying `U`; the
            // PARAMETER types come from the actual constructor arguments (a secondary constructor's
            // signature differs from the primary, e.g. `Sc(String)` delegating to `Sc(Int)`).
            IrExpr::New {
                class,
                args,
                ctor_params,
            } if is_vc[*class as usize] => {
                let cls = *class as usize;
                let u = under
                    .get(&fq[cls])
                    .map(|t| erase(t, &under))
                    .unwrap_or(IrType::Error);
                let ret = desc(&u);
                let params: String = match ctor_params {
                    Some(ps) => ps.iter().map(|p| desc(&erase(p, &under))).collect(),
                    None => ret.clone(),
                };
                Some(Rw::Ctor(IrExpr::Call {
                    callee: Callee::Static {
                        owner: fq[cls].clone(),
                        name: "constructor-impl".to_string(),
                        descriptor: format!("({params}){ret}"),
                        inline: false,
                    },
                    dispatch_receiver: None,
                    args: args.clone(),
                }))
            }
            // An explicit coercion of an UNBOXED value class to a nullable `X?` (`a?.foo()` : `Z?`, the
            // `when`-branch reconciliation): `box-impl` it, so the boxed `X?` merges with the `null` branch.
            IrExpr::TypeOp {
                op: crate::ir::IrTypeOp::ImplicitCoercion,
                arg,
                type_operand:
                    IrType::Class {
                        nullable: true,
                        fq_name,
                        ..
                    },
            } if under.contains_key(fq_name)
                && !matches!(
                    repr(&ir.exprs, &orig_rets, &orig_fields, slots, &under, *arg),
                    Repr::Boxed(_)
                ) =>
            {
                let u = under
                    .get(fq_name)
                    .map(|t| erase(t, &under))
                    .unwrap_or(IrType::Error);
                Some(Rw::Ctor(IrExpr::Call {
                    callee: Callee::Static {
                        owner: fq_name.clone(),
                        name: "box-impl".to_string(),
                        descriptor: format!("({})L{fq_name};", desc(&u)),
                        inline: false,
                    },
                    dispatch_receiver: None,
                    args: vec![*arg],
                }))
            }
            // `x.v` (sole-field read): identity on an unboxed value, `unbox-impl()` on a boxed one.
            IrExpr::GetField {
                receiver, class, ..
            } if is_vc[*class as usize] => Some(Rw::Prop(*receiver, fq[*class as usize].clone())),
            // A sole-property access resolved to `invokevirtual X.getV()` (e.g. inside another value
            // class's `init` block) — rewrite like the indexed getter.
            IrExpr::Call {
                callee: Callee::Virtual { owner, name, .. },
                dispatch_receiver: Some(receiver),
                ..
            } if vc_getters.get(owner).is_some_and(|g| g == name) => {
                Some(Rw::Prop(*receiver, owner.clone()))
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
                    Some(Rw::Prop(*receiver, fq[cls].clone()))
                } else {
                    None
                }
            }
            _ => None,
        };
        let rewrite = match rw {
            Some(Rw::Ctor(e)) => Some(e),
            Some(Rw::Prop(receiver, x)) => Some(prop_access(
                ir,
                receiver,
                &x,
                &under,
                &orig_fields,
                &orig_rets,
                slots,
                boxed_this,
            )),
            None => None,
        };
        if let Some(r) = rewrite {
            ir.exprs[i] = r;
        }
    }

    // 5. Box/unbox at call boundaries, per function so each value's slot type is known: an UNBOXED
    //    value-class value into a reference target (`Object`/generic/nullable-`X`) is `box-impl`'d; a
    //    BOXED one into an unboxed (non-null `X`) target is `unbox-impl`'d. Collect then apply.
    let mut ops: Vec<(ExprId, BoxOp)> = Vec::new();
    // A `!!` over an UNBOXED primitive-underlying value class is redundant (a primitive can't be null);
    // kotlinc emits no `checkNotNull`. Strip such asserts — left in, they `checkNotNull` a primitive.
    let mut strip: Vec<(ExprId, ExprId)> = Vec::new();
    // `(comparison expr, is_ne)` — a `non-null-vc == null` folded to a constant `false`/`true`.
    let mut vacuous: Vec<(ExprId, bool)> = Vec::new();
    // Each body to box/unbox: every non-value-class-member function body (with its captured slot types),
    // plus every class `init { … }` block (slots = `this` + the ctor params), so a value-class member
    // call / boundary INSIDE an init block (`class B(val a: A) { init { a.f() } }`) is boxed too.
    let mut bodies: Vec<(ExprId, HashMap<u32, IrType>)> = Vec::new();
    // `fid` indexes two parallel vecs (`ir.functions` and `slot_types`), so the range loop is wanted.
    #[allow(clippy::needless_range_loop)]
    for fid in 0..ir.functions.len() {
        if vc_methods.contains(&(fid as u32)) {
            continue;
        }
        if let Some(root) = ir.functions[fid].body {
            bodies.push((root, slot_types[fid].clone()));
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
            if let Some(b) = sc.body {
                bodies.push((b, body_slot_map(&ir.exprs, b, params)));
            }
            for &a in &sc.delegate_args {
                bodies.push((a, body_slot_map(&ir.exprs, a, params)));
            }
        }
        // Base-class constructor args run in the subclass `<init>` over its primary ctor params.
        for &a in &c.super_args {
            bodies.push((a, body_slot_map(&ir.exprs, a, &orig_ctor_args[cidx])));
        }
    }
    for (root, slots) in &bodies {
        let root = *root;
        let mut reach = HashSet::new();
        collect_reachable(&ir.exprs, root, &mut reach);
        for id in reach {
            if let IrExpr::NotNullAssert { operand } = &ir.exprs[id as usize] {
                if let Repr::Unboxed(x) =
                    repr(&ir.exprs, &orig_rets, &orig_fields, slots, &under, *operand)
                {
                    if under
                        .get(&x)
                        .map(|u| !is_ref(&erase(u, &under)))
                        .unwrap_or(false)
                    {
                        strip.push((id, *operand));
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
                let to_self = matches!(type_operand, IrType::Class { fq_name, .. } if under.contains_key(fq_name));
                if let Repr::Unboxed(x) =
                    repr(&ir.exprs, &orig_rets, &orig_fields, slots, &under, *arg)
                {
                    if to_self
                        && matches!(
                            op,
                            crate::ir::IrTypeOp::Cast | crate::ir::IrTypeOp::CastNonNull
                        )
                    {
                        strip.push((id, *arg));
                    } else if !to_self && is_ref(type_operand) {
                        let op =
                            if operand_nonnull(&ir.exprs, &orig_rets, &orig_fields, slots, *arg) {
                                BoxOp::Box(x)
                            } else {
                                BoxOp::BoxNull(x)
                            };
                        ops.push((*arg, op));
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
                if is_vc[*class as usize] {
                    if let Repr::Unboxed(x) = repr(
                        &ir.exprs,
                        &orig_rets,
                        &orig_fields,
                        slots,
                        &under,
                        *receiver,
                    ) {
                        ops.push((*receiver, BoxOp::Box(x)));
                    }
                }
                // A USER value-class member keeps its value-class PARAMS boxed (`fun foo(x: Z)` → `foo(LZ;)`,
                // unlike a free function where `Z` erases). So an UNBOXED `Z` arg at such a param must box.
                if let Some(&fid) = ir.classes[*class as usize].methods.get(*index as usize) {
                    let params = ir.functions[fid as usize].params.clone();
                    for (k, a) in args.clone().into_iter().enumerate() {
                        let Some(a) = a else { continue };
                        if let Some(IrType::Class { fq_name, .. }) = params.get(k) {
                            if under.contains_key(fq_name)
                                && matches!(repr(&ir.exprs, &orig_rets, &orig_fields, slots, &under, a), Repr::Unboxed(ref x) if x == fq_name)
                            {
                                let op = if operand_nonnull(
                                    &ir.exprs,
                                    &orig_rets,
                                    &orig_fields,
                                    slots,
                                    a,
                                ) {
                                    BoxOp::Box(fq_name.clone())
                                } else {
                                    BoxOp::BoxNull(fq_name.clone())
                                };
                                ops.push((a, op));
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
                    if matches!(
                        repr(&ir.exprs, &orig_rets, &orig_fields, slots, &under, vc),
                        Repr::Unboxed(_)
                    ) && operand_nonnull(&ir.exprs, &orig_rets, &orig_fields, slots, vc)
                    {
                        vacuous.push((id, is_ne));
                        continue;
                    }
                }
                for (a, other) in [(l, r), (r, l)] {
                    if let Repr::Unboxed(x) =
                        repr(&ir.exprs, &orig_rets, &orig_fields, slots, &under, a)
                    {
                        let other_repr =
                            repr(&ir.exprs, &orig_rets, &orig_fields, slots, &under, other);
                        // A `Float`/`Double` underlying uses IEEE TOTAL-ORDER equality (`NaN == NaN`,
                        // `0.0 != -0.0`), which the synthesized `equals`/`areEqual` path implements but a
                        // raw `dcmp`/`fcmp` does not — so box even a same-class pair to route through it.
                        let total_order = matches!(
                            under.get(&x).map(|u| erase(u, &under)),
                            Some(IrType::Class { fq_name, .. }) if fq_name == "kotlin/Float" || fq_name == "kotlin/Double"
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
                            let op =
                                if operand_nonnull(&ir.exprs, &orig_rets, &orig_fields, slots, a) {
                                    BoxOp::Box(x)
                                } else {
                                    BoxOp::BoxNull(x)
                                };
                            ops.push((a, op));
                        }
                    }
                }
            }
            // The RECEIVER of an `Any`-method external call (`a.toString()`/`a.hashCode()`) on an unboxed
            // value class boxes, so the call dispatches to the box's override.
            if let IrExpr::Call {
                callee: Callee::External(_),
                dispatch_receiver: Some(recv),
                ..
            } = &ir.exprs[id as usize]
            {
                if let Repr::Unboxed(x) =
                    repr(&ir.exprs, &orig_rets, &orig_fields, slots, &under, *recv)
                {
                    let op = if operand_nonnull(&ir.exprs, &orig_rets, &orig_fields, slots, *recv) {
                        BoxOp::Box(x)
                    } else {
                        BoxOp::BoxNull(x)
                    };
                    ops.push((*recv, op));
                }
            }
            // An unboxed value class flowing into a stdlib (`External`) call or a dynamic `invoke`
            // (string-template `append`/`toString`, a generic `Object` param), or stored as a reference
            // array element (`arrayOf(X(..))` → `X[]`), must be boxed.
            if let IrExpr::Call {
                callee: Callee::External(_),
                args,
                ..
            }
            | IrExpr::InvokeFunction { args, .. }
            | IrExpr::Vararg { elements: args, .. } = &ir.exprs[id as usize]
            {
                for a in args.clone() {
                    if let Repr::Unboxed(x) =
                        repr(&ir.exprs, &orig_rets, &orig_fields, slots, &under, a)
                    {
                        let op = if operand_nonnull(&ir.exprs, &orig_rets, &orig_fields, slots, a) {
                            BoxOp::Box(x)
                        } else {
                            BoxOp::BoxNull(x)
                        };
                        ops.push((a, op));
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
                let vc_owned = under.contains_key(owner);
                let refs = descriptor_param_refs(descriptor);
                let ptypes = descriptor_param_types(descriptor);
                for (k, a) in args.clone().into_iter().enumerate() {
                    let Repr::Unboxed(x) =
                        repr(&ir.exprs, &orig_rets, &orig_fields, slots, &under, a)
                    else {
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
                        refs.get(k).copied().unwrap_or(false)
                    };
                    if box_here {
                        let op = if operand_nonnull(&ir.exprs, &orig_rets, &orig_fields, slots, a) {
                            BoxOp::Box(x)
                        } else {
                            BoxOp::BoxNull(x)
                        };
                        ops.push((a, op));
                    }
                }
            }
            // Each `(value expr, target type)` boundary in this expression.
            let pairs: Vec<(ExprId, IrType)> = match &ir.exprs[id as usize] {
                IrExpr::New { class, args, .. } => args
                    .iter()
                    .zip(orig_fields[*class as usize].iter())
                    .map(|(a, p)| (*a, p.clone()))
                    .collect(),
                IrExpr::Call {
                    callee: Callee::Local(cfid),
                    args,
                    ..
                } => args
                    .iter()
                    .zip(orig_params[*cfid as usize].iter())
                    .map(|(a, p)| (*a, p.clone()))
                    .collect(),
                // A value-class instance-method call (`a.equals(b)`) boxes value-class arguments into
                // the method's (reference) parameters, same as a plain call.
                IrExpr::MethodCall { class, index, args, .. } => ir.classes[*class as usize]
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
                                if matches!(current.get(i), Some(IrType::Class { fq_name, .. }) if under.contains_key(fq_name)) {
                                    return None;
                                }
                                Some((a.as_ref().copied()?, params.get(i)?.clone()))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                // A local initializer `val x: T = <vc>` is a representation boundary: an unboxed value
                // into a boxed (`Any`/`X?`-boxed/generic) slot must `box-impl`. The slot's PRE-erasure
                // declared type lives in `slots` (the `Variable.ty` was erased in step 3).
                IrExpr::Variable {
                    index,
                    init: Some(v),
                    ..
                } => match slots.get(index) {
                    Some(t) => vec![(*v, t.clone())],
                    None => continue,
                },
                _ => continue,
            };
            for (a, p) in pairs {
                let tgt = target(&p, &under);
                // An unboxed value class flowing to a reference SUPERTYPE — `Any`, an interface the value
                // class implements, a generic `T` — must be boxed (the box satisfies that type; the raw
                // underlying does not). `Target::Boxed` covers `Any`/nullable-`X`; a plain interface/class
                // target (`Target::Other` that is a reference and not the value class itself) also boxes.
                let supertype_box = matches!(&tgt, Target::Boxed)
                    || (matches!(tgt, Target::Other)
                        && is_ref(&p)
                        && !matches!(&p, IrType::Class { fq_name, .. } if fq_name == match &repr(&ir.exprs, &orig_rets, &orig_fields, slots, &under, a) { Repr::Unboxed(x) | Repr::Boxed(x) => x.as_str(), Repr::NotVc => "" }));
                match repr(&ir.exprs, &orig_rets, &orig_fields, slots, &under, a) {
                    Repr::Unboxed(x) if supertype_box => {
                        // A possibly-null operand (`X?` over a reference) boxes null-safely so the
                        // value class's non-null ctor check isn't hit on `null`.
                        let op = if operand_nonnull(&ir.exprs, &orig_rets, &orig_fields, slots, a) {
                            BoxOp::Box(x)
                        } else {
                            BoxOp::BoxNull(x)
                        };
                        ops.push((a, op));
                    }
                    Repr::Boxed(x) if matches!(&tgt, Target::UnboxedX(tx) if *tx == x) => {
                        ops.push((a, BoxOp::Unbox(x)))
                    }
                    // A boxed element read from a stdlib reference array (`arr[i]` → `Object`/boxed `X`)
                    // flowing into an unboxed value-class slot must `unbox-impl`.
                    Repr::NotVc => {
                        if let Target::UnboxedX(x) = &tgt {
                            if matches!(
                                &ir.exprs[a as usize],
                                IrExpr::Call {
                                    callee: Callee::External(_),
                                    ..
                                }
                            ) {
                                ops.push((a, BoxOp::Unbox(x.clone())));
                            }
                        }
                    }
                    _ => {}
                }
            }
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
        match op {
            BoxOp::Box(x) => box_wrap(ir, id, &x, &under),
            BoxOp::BoxNull(x) => {
                box_wrap_nullable(ir, id, &x, &under, fresh);
                fresh += 1;
            }
            BoxOp::Unbox(x) => unbox_wrap(ir, id, &x, &under),
        }
    }

    // 6. A function returning a nullable value class `X?` boxes its non-null (unboxed) results; a
    //    function declared to return a reference SUPERTYPE (`Any`/`Any?`/an interface — NOT the value
    //    class itself) boxes a value-class tail too (`fun f(): Any? = vc`).
    for fid in 0..ir.functions.len() {
        if vc_methods.contains(&(fid as u32)) {
            continue;
        }
        if let Some(x) = boxed_vc(&orig_rets[fid], &under) {
            if let Some(body) = ir.functions[fid].body {
                box_tail(ir, body, &x, &under);
            }
        } else if matches!(&orig_rets[fid], IrType::Class { fq_name, .. }
            if fq_name == "kotlin/Any" || vc_interfaces.contains(fq_name))
        {
            // A function declared to return `Any` or an interface a value class implements (NOT the
            // value class itself) boxes a value-class tail so the erased call hands back a box (`is X`/
            // interface dispatch works). Concrete-type returns (e.g. `String`) are left alone.
            if let Some(body) = ir.functions[fid].body {
                box_vc_tail(ir, body, &under, &orig_rets, false);
            }
        } else if let IrType::Class {
            fq_name,
            nullable: false,
            ..
        } = &orig_rets[fid]
        {
            // A function returning the value class ITSELF (`fun test(): Z = a?.foo()!!`) whose tail is a
            // BOXED value (the `!!` of a nullable safe-call yields a boxed `Z`) must `unbox-impl` it — the
            // erased return is the underlying.
            if under.contains_key(fq_name) {
                let x = fq_name.clone();
                if let Some(body) = ir.functions[fid].body {
                    unbox_tail(
                        ir,
                        body,
                        &x,
                        &under,
                        &orig_rets,
                        &orig_fields,
                        &slot_types[fid],
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
            if let Some(body) = ir.functions.get(*impl_fn as usize).and_then(|f| f.body) {
                lambda_impls.push((*impl_fn, body));
            }
            if let Some(b) = inline_body {
                inline_bodies.push(*b);
            }
        }
    }
    for (impl_fn, body) in lambda_impls {
        // A value-class result is boxed; the impl method then returns the BOX type `X` (its erased
        // underlying return would mis-type the boxed value, e.g. `LX;` vs `String`).
        if let Some(x) = tail_vc(&ir.exprs, &orig_rets, body) {
            ir.functions[impl_fn as usize].ret = IrType::Class {
                fq_name: x.clone(),
                type_args: vec![],
                nullable: false,
            };
        }
        box_vc_tail(ir, body, &under, &orig_rets, false);
    }
    for body in inline_bodies {
        box_vc_tail(ir, body, &under, &orig_rets, false);
    }

    true
}

/// Box an unboxed value-class result at every tail position of `id` (recursing `when`/block/return
/// tails). `prim_only` (the lambda `() -> T` case) boxes only a primitive-underlying result — a
/// reference one already satisfies the erased `Object`; the `Any`-return case (`prim_only = false`)
/// boxes any, so an `is X`/`as X` on the result holds.
fn box_vc_tail(
    ir: &mut IrFile,
    id: ExprId,
    under: &HashMap<String, IrType>,
    rets: &[IrType],
    prim_only: bool,
) {
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
            if let Some(x) = unboxed_vc_class(&ir.exprs, rets, id, !prim_only) {
                let prim = under
                    .get(&x)
                    .map(|u| !is_ref(&erase(u, under)))
                    .unwrap_or(false);
                if !prim_only || prim {
                    box_wrap(ir, id, &x, under);
                }
            }
        }
    }
}

/// The value class produced at a tail position of `id` (recursing `when`/block/return tails), if any.
fn tail_vc(exprs: &[IrExpr], rets: &[IrType], id: ExprId) -> Option<String> {
    match &exprs[id as usize] {
        IrExpr::When { branches } => branches.iter().find_map(|(_, r)| tail_vc(exprs, rets, *r)),
        IrExpr::Block { value: Some(v), .. } => tail_vc(exprs, rets, *v),
        IrExpr::Block { value: None, stmts } => stmts.last().and_then(|&s| tail_vc(exprs, rets, s)),
        IrExpr::Return(Some(v)) => tail_vc(exprs, rets, *v),
        _ => unboxed_vc_class(exprs, rets, id, false),
    }
}

/// The value class an expr produces UNBOXED (a `constructor-impl`/`unbox-impl` result, or a local call
/// whose return type is a non-null value class), if any.
fn unboxed_vc_class(exprs: &[IrExpr], rets: &[IrType], id: ExprId, calls: bool) -> Option<String> {
    match &exprs[id as usize] {
        IrExpr::Call {
            callee: Callee::Static { owner, name, .. },
            ..
        } if name == "constructor-impl" || name == "unbox-impl" => Some(owner.clone()),
        // A local call returning an unboxed value class — only considered when `calls` is set (the
        // `Any`-return case); the lambda case must NOT box these (they already satisfy `Object`).
        IrExpr::Call {
            callee: Callee::Local(fid),
            ..
        } if calls => match rets.get(*fid as usize) {
            Some(IrType::Class {
                fq_name,
                nullable: false,
                ..
            }) => Some(fq_name.clone()),
            _ => None,
        },
        IrExpr::Block { value: Some(v), .. } => unboxed_vc_class(exprs, rets, *v, calls),
        IrExpr::NotNullAssert { operand } if calls => {
            unboxed_vc_class(exprs, rets, *operand, calls)
        }
        _ => None,
    }
}

enum BoxOp {
    Box(String),
    BoxNull(String),
    Unbox(String),
}

/// The representation a value-class value currently has.
enum Repr {
    NotVc,
    Unboxed(String),
    Boxed(String),
}

/// What a target position wants of a value-class value.
enum Target {
    UnboxedX(String), // a non-null `X` position → wants the unboxed `U`
    Boxed,            // `Object`/generic/nullable-`X` → wants a boxed `X` object
    Other,
}

/// Whether a NULLABLE value class `X?` is represented BOXED. Only true when its underlying erases to a
/// primitive (a primitive can't carry null, so `X?` keeps the boxed `X`). Over a reference underlying,
/// `X?` erases to that underlying reference — represented unboxed, exactly like a non-null `X`.
fn nullable_is_boxed(x: &str, under: &HashMap<String, IrType>) -> bool {
    // `X?` stays UNBOXED (its underlying reference carries null) only when the underlying is a NON-NULL
    // reference. Over a primitive (can't hold null) OR a NULLABLE reference (where `X(null)` and a `null`
    // `X?` would otherwise be indistinguishable), `X?` is the boxed `X`.
    under
        .get(x)
        .map(|u| !is_ref(&erase(u, under)) || underlying_null_capable(u, under))
        .unwrap_or(false)
}

/// Whether a value class's unboxed representation can hold `null` — true when ANY level of the nested
/// underlying chain is declared nullable (`X(val v: Int?)`; `ZN(val z: Z1?)` → `ZN2(val z: ZN)` null-capable
/// through `Z1?`). `erase` collapses a nullable-over-non-null-reference to a non-null underlying, so this
/// walks the UNERASED chain to see the `?` erasure drops.
fn underlying_null_capable(t: &IrType, under: &HashMap<String, IrType>) -> bool {
    match t {
        IrType::Class { nullable: true, .. } => true,
        IrType::Class { fq_name, .. } => under
            .get(fq_name)
            .is_some_and(|u| underlying_null_capable(u, under)),
        _ => false,
    }
}

/// Whether a NON-NULL value-class type's unboxed underlying can hold null (so a `checkNotNullParameter`
/// on it would wrongly reject a legal value). True when the value class's field type erases to a
/// nullable reference (`X(val v: Int?)` → `Integer`; `X(val v: String?)` → `String?`).
fn vc_underlying_nullable(t: &IrType, under: &HashMap<String, IrType>) -> bool {
    if let IrType::Class {
        fq_name,
        nullable: false,
        ..
    } = t
    {
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
    rets: &[IrType],
    fields: &[Vec<IrType>],
    slots: &HashMap<u32, IrType>,
    id: ExprId,
) -> bool {
    let non_null_ty = |t: &IrType| {
        matches!(
            t,
            IrType::Class {
                nullable: false,
                ..
            }
        )
    };
    match &exprs[id as usize] {
        IrExpr::New { .. } => true,
        // A read of a non-nullable field yields a non-null value (a `val a: X` data-class property is
        // never null — box it with the plain `box-impl`, no null guard).
        IrExpr::GetField { class, index, .. } => fields
            .get(*class as usize)
            .and_then(|fs| fs.get(*index as usize))
            .is_some_and(non_null_ty),
        IrExpr::NotNullAssert { .. } => true,
        IrExpr::Call {
            callee: Callee::Static { name, .. },
            ..
        } if name == "constructor-impl" || name == "box-impl" => true,
        IrExpr::Call {
            callee: Callee::Local(fid),
            ..
        } => rets.get(*fid as usize).is_some_and(non_null_ty),
        IrExpr::GetValue(i) => slots.get(i).is_some_and(non_null_ty),
        IrExpr::Block { value: Some(v), .. } => operand_nonnull(exprs, rets, fields, slots, *v),
        _ => false,
    }
}

fn repr_of_ty(t: &IrType, under: &HashMap<String, IrType>) -> Repr {
    if let IrType::Class {
        fq_name, nullable, ..
    } = t
    {
        if under.contains_key(fq_name) {
            return if *nullable && nullable_is_boxed(fq_name, under) {
                Repr::Boxed(fq_name.clone())
            } else {
                Repr::Unboxed(fq_name.clone())
            };
        }
    }
    Repr::NotVc
}

fn target(t: &IrType, under: &HashMap<String, IrType>) -> Target {
    match t {
        IrType::Class {
            fq_name, nullable, ..
        } if under.contains_key(fq_name) => {
            if *nullable && nullable_is_boxed(fq_name, under) {
                Target::Boxed
            } else {
                Target::UnboxedX(fq_name.clone())
            }
        }
        IrType::Class { fq_name, .. } if fq_name == "kotlin/Any" => Target::Boxed,
        _ => Target::Other,
    }
}

/// The representation of the value the expr at `id` produces (after the construction/property rewrite).
fn repr(
    exprs: &[IrExpr],
    rets: &[IrType],
    fields: &[Vec<IrType>],
    slots: &HashMap<u32, IrType>,
    under: &HashMap<String, IrType>,
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
        IrExpr::Call {
            callee: Callee::Static { owner, name, .. },
            ..
        } if (name == "constructor-impl" || name == "unbox-impl") && under.contains_key(owner) => {
            Repr::Unboxed(owner.clone())
        }
        IrExpr::Call {
            callee: Callee::Static { owner, name, .. } | Callee::Virtual { owner, name, .. },
            ..
        } if name == "box-impl" && under.contains_key(owner) => Repr::Boxed(owner.clone()),
        IrExpr::Call {
            callee: Callee::Virtual { owner, name, .. },
            ..
        } if name == "unbox-impl" && under.contains_key(owner) => Repr::Unboxed(owner.clone()),
        IrExpr::Call {
            callee: Callee::Local(fid),
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
            type_operand: IrType::Class { fq_name, .. },
            arg,
        } if under.contains_key(fq_name) => match repr(exprs, rets, fields, slots, under, *arg) {
            Repr::Unboxed(x) if x == *fq_name => Repr::Unboxed(x),
            _ => Repr::Boxed(fq_name.clone()),
        },
        // A sole-field access coerces to the underlying type — its representation is that type's, NOT
        // the value class's (so `vc.field` reads as the underlying, e.g. an `Int`, not a `Meters`).
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            type_operand,
            ..
        } => repr_of_ty(type_operand, under),
        IrExpr::NotNullAssert { operand } => repr(exprs, rets, fields, slots, under, *operand),
        IrExpr::Block { value: Some(v), .. } => repr(exprs, rets, fields, slots, under, *v),
        _ => Repr::NotVc,
    }
}

/// Replace the expr at `id` with `(X)<orig>.unbox-impl()` — checkcast then unbox a boxed `X`.
fn unbox_wrap(ir: &mut IrFile, id: ExprId, x: &str, under: &HashMap<String, IrType>) {
    let orig = ir.exprs[id as usize].clone();
    let new_id = ir.exprs.len() as ExprId;
    ir.exprs.push(orig);
    let cast = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::TypeOp {
        op: crate::ir::IrTypeOp::Cast,
        arg: new_id,
        type_operand: IrType::Class {
            fq_name: x.to_string(),
            type_args: vec![],
            nullable: false,
        },
    });
    let u = under
        .get(x)
        .map(|t| erase(t, under))
        .unwrap_or(IrType::Error);
    let d = desc(&u);
    ir.exprs[id as usize] = IrExpr::Call {
        callee: Callee::Virtual {
            owner: x.to_string(),
            name: "unbox-impl".to_string(),
            descriptor: format!("(){d}"),
            interface: false,
        },
        dispatch_receiver: Some(cast),
        args: vec![],
    };
}

/// Build a sole-property access `x.v`: identity (`Block` yielding the receiver) when the receiver is an
/// unboxed value, or `receiver.unbox-impl()` when it is a boxed `X` (e.g. from a nullable-returning
/// function).
#[allow(clippy::too_many_arguments)]
fn prop_access(
    ir: &mut IrFile,
    receiver: ExprId,
    x: &str,
    under: &HashMap<String, IrType>,
    fields: &[Vec<IrType>],
    rets: &[IrType],
    slots: &HashMap<u32, IrType>,
    boxed_this: Option<u32>,
) -> IrExpr {
    let u = under
        .get(x)
        .map(|t| erase(t, under))
        .unwrap_or(IrType::Error);
    // `this.field` inside a USER value-class member: `this` (the `boxed_this` slot) is the BOXED object →
    // unbox. Otherwise `unbox-impl` on a boxed receiver, identity on an unboxed one. Wrap in a coercion to
    // the underlying so later representation analysis (`==` boxing) treats it as the underlying.
    let this_boxed = matches!((boxed_this, &ir.exprs[receiver as usize]),
        (Some(t), IrExpr::GetValue(i)) if *i == t);
    let inner = if this_boxed
        || is_boxed_vc(
            &ir.exprs,
            &ir.functions,
            fields,
            rets,
            slots,
            under,
            receiver,
            x,
        ) {
        let d = desc(&u);
        ir.add_expr(IrExpr::Call {
            callee: Callee::Virtual {
                owner: x.to_string(),
                name: "unbox-impl".to_string(),
                descriptor: format!("(){d}"),
                interface: false,
            },
            dispatch_receiver: Some(receiver),
            args: vec![],
        })
    } else {
        receiver
    };
    IrExpr::TypeOp {
        op: crate::ir::IrTypeOp::ImplicitCoercion,
        arg: inner,
        type_operand: u,
    }
}

/// Whether the expr at `id` produces a BOXED value-class `x` object: a `box-impl` result, a call whose
/// return type is `X` (a nullable-over-primitive value class stays boxed), or a `!!`/identity over one.
#[allow(clippy::too_many_arguments)]
fn is_boxed_vc(
    exprs: &[IrExpr],
    funcs: &[crate::ir::IrFunction],
    fields: &[Vec<IrType>],
    rets: &[IrType],
    slots: &HashMap<u32, IrType>,
    under: &HashMap<String, IrType>,
    id: ExprId,
    x: &str,
) -> bool {
    let is_x = |t: &IrType| matches!(t, IrType::Class { fq_name, .. } if fq_name == x);
    match &exprs[id as usize] {
        // A local/param slot whose declared type is a BOXED value class `x` (a nullable `X?`, e.g. the
        // `?.` receiver temp) holds a boxed `x` — so a `.field` on it `unbox-impl`s.
        IrExpr::GetValue(i) => {
            matches!(slots.get(i).map(|t| repr_of_ty(t, under)), Some(Repr::Boxed(ref c)) if c == x)
        }
        IrExpr::GetField { class, index, .. } => fields
            .get(*class as usize)
            .and_then(|fs| fs.get(*index as usize))
            .is_some_and(|t| matches!(repr_of_ty(t, under), Repr::Boxed(ref c) if c == x)),
        IrExpr::Call {
            callee: Callee::Static { owner, name, .. },
            ..
        } if owner == x && name == "box-impl" => true,
        IrExpr::Call {
            callee: Callee::Local(fid),
            ..
        } => funcs.get(*fid as usize).is_some_and(|f| is_x(&f.ret)),
        IrExpr::Call {
            callee: Callee::Static { descriptor, .. } | Callee::Virtual { descriptor, .. },
            ..
        } => descriptor.ends_with(&format!("L{x};")),
        // A stdlib reference-array element read (`arr[i]` → `kotlin/Array.get`) yields a boxed element.
        IrExpr::Call {
            callee: Callee::External(name),
            ..
        } => name == "kotlin/Array.get",
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
                && !matches!(repr(exprs, rets, fields, slots, under, *arg), Repr::Unboxed(ref c) if c == x)
        }
        IrExpr::NotNullAssert { operand } => {
            is_boxed_vc(exprs, funcs, fields, rets, slots, under, *operand, x)
        }
        // A `when` whose non-null branch yields a boxed `x` (a nullable safe-call: `box-impl` vs `null`) is
        // a boxed `x`.
        IrExpr::When { branches } => branches
            .iter()
            .any(|(_, r)| is_boxed_vc(exprs, funcs, fields, rets, slots, under, *r, x)),
        // A sole-field access of a value class whose underlying is itself a BOXED value class
        // (`ZN(val z: Z1?)`) reads as `ImplicitCoercion(ZN.unbox-impl(): LZ1;)` — transparently a boxed
        // `Z1`. Recurse into the coerced value so a further `.x` on it `unbox-impl`s.
        IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::ImplicitCoercion,
            arg,
            ..
        } => is_boxed_vc(exprs, funcs, fields, rets, slots, under, *arg, x),
        IrExpr::Block { value: Some(v), .. } => {
            is_boxed_vc(exprs, funcs, fields, rets, slots, under, *v, x)
        }
        _ => false,
    }
}

/// A NULLABLE value-class type `X?` (which stays boxed) → its internal name.
fn boxed_vc(t: &IrType, under: &HashMap<String, IrType>) -> Option<String> {
    if let IrType::Class {
        fq_name,
        nullable: true,
        ..
    } = t
    {
        if under.contains_key(fq_name) && nullable_is_boxed(fq_name, under) {
            return Some(fq_name.clone());
        }
    }
    None
}

/// Whether the expr at `id` is an UNBOXED value-class value of class `x` (a `constructor-impl`/
/// `unbox-impl` result, or an identity block over one).
fn is_unboxed_vc(exprs: &[IrExpr], id: ExprId, x: &str) -> bool {
    match &exprs[id as usize] {
        IrExpr::Call {
            callee: Callee::Static { owner, name, .. },
            ..
        } if owner == x && (name == "constructor-impl" || name == "unbox-impl") => true,
        IrExpr::Block { value: Some(v), .. } => is_unboxed_vc(exprs, *v, x),
        _ => false,
    }
}

/// At a value-producing (return) position, box an unboxed `X` with `box-impl`, recursing through
/// `when`/block tails so each branch is boxed (a `null` branch is left alone).
/// At a function's return tail (recursing `return`/block tails), `unbox-impl` a BOXED value-class value so
/// it matches the function's erased (underlying) return type — `fun f(): Z = a?.foo()!!` returns the box.
fn unbox_tail(
    ir: &mut IrFile,
    id: ExprId,
    x: &str,
    under: &HashMap<String, IrType>,
    rets: &[IrType],
    fields: &[Vec<IrType>],
    slots: &HashMap<u32, IrType>,
) {
    match &ir.exprs[id as usize] {
        IrExpr::Return(Some(v)) | IrExpr::Block { value: Some(v), .. } => {
            let v = *v;
            unbox_tail(ir, v, x, under, rets, fields, slots);
        }
        IrExpr::Block { value: None, stmts } => {
            if let Some(&last) = stmts.last() {
                unbox_tail(ir, last, x, under, rets, fields, slots);
            }
        }
        _ => {
            if is_boxed_vc(&ir.exprs, &ir.functions, fields, rets, slots, under, id, x) {
                unbox_wrap(ir, id, x, under);
            }
        }
    }
}

fn box_tail(ir: &mut IrFile, id: ExprId, x: &str, under: &HashMap<String, IrType>) {
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

/// Replace the expr at `id` with `box-impl(<original expr at id>)`.
fn box_wrap(ir: &mut IrFile, id: ExprId, x: &str, under: &HashMap<String, IrType>) {
    let orig = ir.exprs[id as usize].clone();
    let new_id = ir.exprs.len() as ExprId;
    ir.exprs.push(orig);
    let u = under
        .get(x)
        .map(|t| erase(t, under))
        .unwrap_or(IrType::Error);
    let d = desc(&u);
    ir.exprs[id as usize] = IrExpr::Call {
        callee: Callee::Static {
            owner: x.to_string(),
            name: "box-impl".to_string(),
            descriptor: format!("({d})L{x};"),
            inline: false,
        },
        dispatch_receiver: None,
        args: vec![new_id],
    };
}

/// Null-safe box: replace the expr at `id` with `{ tmp = <orig>; if (tmp == null) null else box-impl(tmp) }`
/// — boxing a nullable (reference-underlying) value class without hitting the ctor null-check on `null`.
fn box_wrap_nullable(
    ir: &mut IrFile,
    id: ExprId,
    x: &str,
    under: &HashMap<String, IrType>,
    slot: u32,
) {
    let orig = ir.exprs[id as usize].clone();
    let orig_id = ir.exprs.len() as ExprId;
    ir.exprs.push(orig);
    let u = under
        .get(x)
        .map(|t| erase(t, under))
        .unwrap_or(IrType::Error);
    let var = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::Variable {
        index: slot,
        ty: u.clone(),
        init: Some(orig_id),
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
    let boxed = ir.exprs.len() as ExprId;
    ir.exprs.push(IrExpr::Call {
        callee: Callee::Static {
            owner: x.to_string(),
            name: "box-impl".to_string(),
            descriptor: format!("({d})L{x};"),
            inline: false,
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
fn erase(t: &IrType, under: &HashMap<String, IrType>) -> IrType {
    if let IrType::Class {
        fq_name, nullable, ..
    } = t
    {
        if let Some(u) = under.get(fq_name) {
            // A non-null `X` always erases to its underlying. A nullable `X?` erases ONLY when it is NOT
            // boxed (`nullable_is_boxed` is the single source of truth — over a non-null reference that
            // carries `null` itself); otherwise it stays the boxed `X` so `X(null)` ≠ `null`. Delegating
            // keeps erasure consistent with the box/unbox analysis for arbitrarily nested chains.
            if !*nullable || !nullable_is_boxed(fq_name, under) {
                return erase(u, under);
            }
        }
    }
    t.clone()
}

/// Whether the erased type occupies a JVM *reference* slot. A non-null Kotlin primitive class
/// (`kotlin/Int`, `kotlin/Boolean`, …) emits as a JVM primitive (`I`, `Z`, …), so it is NOT a
/// reference; its NULLABLE form is the boxed wrapper (`Integer`), which is. Everything else that is a
/// `Class` is a reference.
/// A `Class` type forced nullable (carry a generic value class's nullable type-parameter bound).
fn mark_nullable_ty(t: &IrType) -> IrType {
    match t {
        IrType::Class {
            fq_name, type_args, ..
        } => IrType::Class {
            fq_name: fq_name.clone(),
            type_args: type_args.clone(),
            nullable: true,
        },
        _ => t.clone(),
    }
}

fn is_ref(t: &IrType) -> bool {
    match t {
        IrType::Class {
            fq_name,
            nullable: false,
            ..
        } => !is_primitive_class(fq_name),
        IrType::Class { .. } => true,
        _ => false,
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
    under: &HashMap<String, IrType>,
    has_init: bool,
) {
    let internal = ir.classes[class_id as usize].fq_name.clone();
    let fname = ir.classes[class_id as usize].fields[0].0.clone();
    let u_ir = under.get(&internal).cloned().unwrap_or(IrType::Error);
    let x_ir = IrType::Class {
        fq_name: internal.clone(),
        type_args: vec![],
        nullable: false,
    };
    let bool_ir = IrType::Class {
        fq_name: "kotlin/Boolean".into(),
        type_args: vec![],
        nullable: false,
    };
    let int_ir = IrType::Class {
        fq_name: "kotlin/Int".into(),
        type_args: vec![],
        nullable: false,
    };
    let str_ir = IrType::Class {
        fq_name: "kotlin/String".into(),
        type_args: vec![],
        nullable: false,
    };
    let any_ir = IrType::Class {
        fq_name: "kotlin/Any".into(),
        type_args: vec![],
        nullable: false,
    };

    let add_static =
        |ir: &mut IrFile, name: &str, params: Vec<IrType>, ret: IrType, body: ExprId| {
            let fid = ir.add_fun(crate::ir::IrFunction {
                name: name.to_string(),
                params,
                ret,
                body: Some(body),
                is_static: true,
                dispatch_receiver: Some(internal.clone()),
                param_checks: Vec::new(),
            });
            ir.classes[class_id as usize].methods.push(fid);
        };
    let add_inst = |ir: &mut IrFile, name: &str, params: Vec<IrType>, ret: IrType, body: ExprId| {
        // Don't synthesize over a user-defined member of the same name.
        let exists = ir.classes[class_id as usize]
            .methods
            .iter()
            .any(|&m| ir.functions.get(m as usize).is_some_and(|f| f.name == name));
        if exists {
            return;
        }
        let fid = ir.add_fun(crate::ir::IrFunction {
            name: name.to_string(),
            params,
            ret,
            body: Some(body),
            is_static: false,
            dispatch_receiver: Some(internal.clone()),
            param_checks: Vec::new(),
        });
        ir.classes[class_id as usize].methods.push(fid);
    };
    let this_field = |ir: &mut IrFile| {
        let recv = ir.add_expr(IrExpr::GetValue(0));
        ir.add_expr(IrExpr::GetField {
            receiver: recv,
            class: class_id,
            index: 0,
        })
    };
    let str_const =
        |ir: &mut IrFile, s: String| ir.add_expr(IrExpr::Const(crate::ir::IrConst::String(s)));
    let str_plus = |ir: &mut IrFile, acc: ExprId, arg: ExprId| {
        ir.add_expr(IrExpr::Call {
            callee: Callee::External("kotlin/String.plus".to_string()),
            dispatch_receiver: Some(acc),
            args: vec![arg],
        })
    };
    let ret_block = |ir: &mut IrFile, v: ExprId| {
        let r = ir.add_expr(IrExpr::Return(Some(v)));
        ir.add_expr(IrExpr::Block {
            stmts: vec![r],
            value: None,
        })
    };

    // unbox-impl(): U
    {
        let g = this_field(ir);
        let body = ret_block(ir, g);
        add_inst(ir, "unbox-impl", vec![], u_ir.clone(), body);
    }
    // box-impl(U): X  — `new X(u)`
    {
        let arg = ir.add_expr(IrExpr::GetValue(0));
        let new = ir.add_expr(IrExpr::New {
            class: class_id,
            args: vec![arg],
            ctor_params: Some(vec![u_ir.clone()]),
        });
        let body = ret_block(ir, new);
        add_static(ir, "box-impl", vec![u_ir.clone()], x_ir.clone(), body);
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
                for id in reach {
                    if matches!(&ir.exprs[id as usize], IrExpr::GetField { class, .. } if *class == class_id)
                    {
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
        add_static(
            ir,
            "constructor-impl",
            vec![u_ir.clone()],
            u_ir.clone(),
            body,
        );
    }
    // hashCode/equals/toString operate on the value class's IMMEDIATE erased underlying, NOT the final
    // primitive of a nested chain: `ZN(val z: Z1?)` erases to a BOXED `Z1` (`LZ1;`), so it hashes/compares
    // as a reference (`Objects.hashCode`/`areEqual` → `Z1`'s own members), not as the final `Int`.
    let eu = erase(&u_ir, under);
    let final_fq = match &eu {
        IrType::Class { fq_name, .. } => fq_name.clone(),
        _ => String::new(),
    };
    let is_ref_under = is_ref(&eu);
    // equals-impl0(U, U): Boolean
    {
        let a = ir.add_expr(IrExpr::GetValue(0));
        let b = ir.add_expr(IrExpr::GetValue(1));
        let cmp = if is_ref_under {
            ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: "kotlin/jvm/internal/Intrinsics".into(),
                    name: "areEqual".into(),
                    descriptor: "(Ljava/lang/Object;Ljava/lang/Object;)Z".into(),
                    inline: false,
                },
                dispatch_receiver: None,
                args: vec![a, b],
            })
        } else {
            ir.add_expr(IrExpr::PrimitiveBinOp {
                op: crate::ir::IrBinOp::Eq,
                lhs: a,
                rhs: b,
            })
        };
        let body = ret_block(ir, cmp);
        add_static(
            ir,
            "equals-impl0",
            vec![u_ir.clone(), u_ir.clone()],
            bool_ir.clone(),
            body,
        );
    }
    // toString(): "X(field=" + field + ")"
    {
        let simple = internal
            .rsplit('/')
            .next()
            .unwrap_or(&internal)
            .replace('$', ".");
        let mut acc = str_const(ir, format!("{simple}({fname}="));
        let fv = this_field(ir);
        acc = str_plus(ir, acc, fv);
        let close = str_const(ir, ")".to_string());
        acc = str_plus(ir, acc, close);
        let body = ret_block(ir, acc);
        add_inst(ir, "toString", vec![], str_ir.clone(), body);
    }
    // hashCode(): field.hashCode() (structural over the final underlying)
    {
        let fv = this_field(ir);
        let h = field_hash_ir(ir, fv, &final_fq);
        let body = ret_block(ir, h);
        add_inst(ir, "hashCode", vec![], int_ir.clone(), body);
    }
    // equals(other): other is X && this.field == other.field
    {
        let mut stmts = Vec::new();
        let other = ir.add_expr(IrExpr::GetValue(1));
        let not_inst = ir.add_expr(IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::NotInstanceOf,
            arg: other,
            type_operand: x_ir.clone(),
        });
        stmts.push(guard_false(ir, not_inst));
        let af = this_field(ir);
        let other_v = ir.add_expr(IrExpr::GetValue(1));
        let ocast = ir.add_expr(IrExpr::TypeOp {
            op: crate::ir::IrTypeOp::Cast,
            arg: other_v,
            type_operand: x_ir.clone(),
        });
        let bf = ir.add_expr(IrExpr::GetField {
            receiver: ocast,
            class: class_id,
            index: 0,
        });
        let ne = field_ne_ir(ir, af, bf, &final_fq);
        stmts.push(guard_false(ir, ne));
        let t = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Boolean(true)));
        stmts.push(ir.add_expr(IrExpr::Return(Some(t))));
        let body = ir.add_expr(IrExpr::Block { stmts, value: None });
        add_inst(ir, "equals", vec![any_ir.clone()], bool_ir.clone(), body);
    }

    // A secondary constructor becomes a static `constructor-impl` OVERLOAD (the unboxed model has no
    // real `<init>` to delegate to): run the secondary body, then delegate to the primary
    // `constructor-impl`. `ir_lower` lowered the body in an INSTANCE frame (`this` at slot 0, params at
    // `1..`); a static method has no `this`, so shift every slot down by one. The class's
    // `secondary_ctors` are then cleared so no instance `<init>` is also emitted.
    let secs = std::mem::take(&mut ir.classes[class_id as usize].secondary_ctors);
    if !secs.is_empty() {
        let udesc = ir_ty_to_jvm(&u_ir).descriptor();
        for sc in secs {
            // Drop the `this` slot: shift all value-slot references in the body + delegation args.
            if let Some(b) = sc.body {
                shift_slots(ir, b);
            }
            for &a in &sc.delegate_args {
                shift_slots(ir, a);
            }
            let mut stmts = Vec::new();
            if let Some(b) = sc.body {
                if let IrExpr::Block { stmts: bs, value } = &ir.exprs[b as usize] {
                    stmts.extend(bs.iter().copied());
                    if let Some(v) = value {
                        stmts.push(*v);
                    }
                } else {
                    stmts.push(b);
                }
            }
            let call = ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: internal.clone(),
                    name: "constructor-impl".to_string(),
                    descriptor: format!("({udesc}){udesc}"),
                    inline: false,
                },
                dispatch_receiver: None,
                args: sc.delegate_args.clone(),
            });
            stmts.push(ir.add_expr(IrExpr::Return(Some(call))));
            let body = ir.add_expr(IrExpr::Block { stmts, value: None });
            add_static(
                ir,
                "constructor-impl",
                sc.params.clone(),
                u_ir.clone(),
                body,
            );
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
fn field_hash_ir(ir: &mut IrFile, v: ExprId, fq: &str) -> ExprId {
    let call = |ir: &mut IrFile, owner: &str, desc: &str, v: ExprId| {
        ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: owner.into(),
                name: "hashCode".into(),
                descriptor: desc.into(),
                inline: false,
            },
            dispatch_receiver: None,
            args: vec![v],
        })
    };
    match fq {
        "kotlin/Int" | "kotlin/Short" | "kotlin/Byte" | "kotlin/Char" => v,
        "kotlin/Boolean" => call(ir, "java/lang/Boolean", "(Z)I", v),
        "kotlin/Long" => call(ir, "java/lang/Long", "(J)I", v),
        "kotlin/Double" => call(ir, "java/lang/Double", "(D)I", v),
        "kotlin/Float" => call(ir, "java/lang/Float", "(F)I", v),
        _ => call(ir, "java/util/Objects", "(Ljava/lang/Object;)I", v),
    }
}

/// `a != b` for an underlying fq name (float/double → total-order `compare != 0`; else `PrimitiveBinOp`).
fn field_ne_ir(ir: &mut IrFile, a: ExprId, b: ExprId, fq: &str) -> ExprId {
    if fq == "kotlin/Double" || fq == "kotlin/Float" {
        let (owner, desc) = if fq == "kotlin/Double" {
            ("java/lang/Double", "(DD)I")
        } else {
            ("java/lang/Float", "(FF)I")
        };
        let cmp = ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: owner.into(),
                name: "compare".into(),
                descriptor: desc.into(),
                inline: false,
            },
            dispatch_receiver: None,
            args: vec![a, b],
        });
        let z = ir.add_expr(IrExpr::Const(crate::ir::IrConst::Int(0)));
        return ir.add_expr(IrExpr::PrimitiveBinOp {
            op: crate::ir::IrBinOp::Ne,
            lhs: cmp,
            rhs: z,
        });
    }
    ir.add_expr(IrExpr::PrimitiveBinOp {
        op: crate::ir::IrBinOp::Ne,
        lhs: a,
        rhs: b,
    })
}

/// kotlinc's inline-class mangling info for an IR type, against the value classes in `under`.
fn mangling_info(
    t: &IrType,
    under: &HashMap<String, IrType>,
) -> crate::jvm::inline_class::InfoForMangling {
    let (fq_name, is_nullable) = match t {
        IrType::Class {
            fq_name, nullable, ..
        } => (fq_name.clone(), *nullable),
        _ => (String::new(), false),
    };
    crate::jvm::inline_class::InfoForMangling {
        is_value: under.contains_key(&fq_name),
        fq_name: fq_name.replace('/', "."),
        is_nullable,
    }
}

/// kotlinc's name for a function whose JVM signature mentions a value class: `base-<hash>` (a value-class
/// parameter, or a value-class return, triggers it). Plain `base` otherwise.
fn vc_mangle(
    base: &str,
    params: &[IrType],
    ret: &IrType,
    under: &HashMap<String, IrType>,
) -> String {
    let pinfo: Vec<_> = params.iter().map(|t| mangling_info(t, under)).collect();
    let rinfo = mangling_info(ret, under);
    let ret_opt = rinfo.is_value.then_some(&rinfo);
    crate::jvm::inline_class::mangled_name(base, &pinfo, ret_opt)
}

/// Erase the value-class types in a JVM method descriptor: each `L<fq>;` whose `<fq>` is a value class
/// becomes its underlying descriptor (`(LIv;)Ljava/lang/String;` → `(I)Ljava/lang/String;`).
fn erase_descriptor(descriptor: &str, under: &HashMap<String, IrType>) -> String {
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
            if let Some(u) = under.get(fq) {
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

fn is_primitive_class(fq: &str) -> bool {
    matches!(
        fq,
        "kotlin/Int"
            | "kotlin/Long"
            | "kotlin/Short"
            | "kotlin/Byte"
            | "kotlin/Boolean"
            | "kotlin/Char"
            | "kotlin/Double"
            | "kotlin/Float"
    )
}

fn desc(t: &IrType) -> String {
    ir_ty_to_jvm(t).descriptor()
}

/// kotlin getter name for a property: `value` → `getValue`.
fn getter_name(field: &str) -> String {
    let mut c = field.chars();
    match c.next() {
        Some(f) => format!("get{}{}", f.to_uppercase(), c.as_str()),
        None => "get".to_string(),
    }
}

/// Collect every `ExprId` reachable from `root` (a function body), so rewrites stay within bodies that
/// own value-class values unboxed.
/// Slot-type map for a body rooted at `root` running over `params` (slot 0 = `this`, params at 1..), plus
/// any local `Variable`s declared inside it — used to give an `init`/secondary-ctor/super-arg body the same
/// slot-typed box/unbox analysis a function body gets from its captured `slot_types`.
fn body_slot_map(exprs: &[IrExpr], root: ExprId, params: &[IrType]) -> HashMap<u32, IrType> {
    let mut slots: HashMap<u32, IrType> = HashMap::new();
    for (i, t) in params.iter().enumerate() {
        slots.insert(1 + i as u32, t.clone());
    }
    let mut reach = HashSet::new();
    collect_reachable(exprs, root, &mut reach);
    for id in reach {
        if let IrExpr::Variable { index, ty, .. } = &exprs[id as usize] {
            slots.insert(*index, ty.clone());
        }
    }
    slots
}

fn collect_reachable(exprs: &[IrExpr], root: ExprId, out: &mut HashSet<ExprId>) {
    if !out.insert(root) {
        return;
    }
    let push = |id: ExprId, out: &mut HashSet<ExprId>| collect_reachable(exprs, id, out);
    match &exprs[root as usize] {
        IrExpr::SetValue { value, .. }
        | IrExpr::TypeOp { arg: value, .. }
        | IrExpr::GetField {
            receiver: value, ..
        }
        | IrExpr::SetStatic { value, .. }
        | IrExpr::EnumValueOf { arg: value, .. }
        | IrExpr::NotNullAssert { operand: value }
        | IrExpr::RefNew { init: value, .. }
        | IrExpr::RefGet { holder: value, .. }
        | IrExpr::Throw { operand: value }
        | IrExpr::NewArray { size: value, .. } => push(*value, out),
        IrExpr::Return(Some(v)) | IrExpr::Variable { init: Some(v), .. } => push(*v, out),
        IrExpr::Call {
            dispatch_receiver,
            args,
            ..
        } => {
            if let Some(r) = dispatch_receiver {
                push(*r, out);
            }
            args.iter().for_each(|a| push(*a, out));
        }
        IrExpr::Block { stmts, value } => {
            stmts.iter().for_each(|s| push(*s, out));
            if let Some(v) = value {
                push(*v, out);
            }
        }
        IrExpr::When { branches } => branches.iter().for_each(|(c, r)| {
            if let Some(c) = c {
                push(*c, out);
            }
            push(*r, out);
        }),
        IrExpr::While {
            cond, body, update, ..
        } => {
            push(*cond, out);
            push(*body, out);
            if let Some(u) = update {
                push(*u, out);
            }
        }
        IrExpr::PrimitiveBinOp { lhs, rhs, .. } => {
            push(*lhs, out);
            push(*rhs, out);
        }
        IrExpr::SetField {
            receiver, value, ..
        }
        | IrExpr::RefSet {
            holder: receiver,
            value,
            ..
        } => {
            push(*receiver, out);
            push(*value, out);
        }
        IrExpr::New { args, .. } | IrExpr::NewExternal { args, .. } => {
            args.iter().for_each(|a| push(*a, out))
        }
        IrExpr::MethodCall { receiver, args, .. } => {
            push(*receiver, out);
            args.iter().flatten().for_each(|a| push(*a, out));
        }
        IrExpr::EnumEntry { .. } => {}
        IrExpr::Lambda {
            captures,
            inline_body,
            ..
        } => {
            captures.iter().for_each(|c| push(*c, out));
            if let Some(b) = inline_body {
                push(*b, out);
            }
        }
        IrExpr::InvokeFunction { func, args, .. } => {
            push(*func, out);
            args.iter().for_each(|a| push(*a, out));
        }
        IrExpr::Vararg { elements, .. } => elements.iter().for_each(|e| push(*e, out)),
        IrExpr::Try {
            body,
            catches,
            finally,
            ..
        } => {
            push(*body, out);
            catches.iter().for_each(|c: &IrCatch| push(c.body, out));
            if let Some(f) = finally {
                push(*f, out);
            }
        }
        _ => {}
    }
}
