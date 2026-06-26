//! AST → `krusty-ir` lowering for the core language.
//!
//! Runs after the front end (parse + type-check). Produces a backend-agnostic [`IrFile`] that any
//! backend (JVM, JS) lowers to its target — proving the FE/BE boundary is real. Covers the core
//! subset: top-level functions, simple classes (a primary constructor of `val`/`var` properties +
//! instance methods reading those fields), const/param/local, primitive arithmetic & comparison,
//! calls (local + stdlib intrinsics), construction, field/method access, `if`/`when`, `while`,
//! `return`, blocks, string templates. Anything outside the subset makes lowering return `None`, so
//! the caller keeps using the direct JVM emitter — the IR path grows one construct at a time.

use std::collections::HashMap;

use crate::ast::{self, BinOp, Decl, Expr, ExprId as AstExprId, FunBody, Stmt, TemplatePart};
use crate::ir::{
    Callee, ClassId, ExprId, IrBinOp, IrClass, IrConst, IrExpr, IrField, IrFile, IrFunction,
    IrTypeOp,
};
use crate::resolve::{SymbolTable, TypeInfo};
use crate::types::Ty;

// --- Lower-bail diagnostics ----------------------------------------------------------------------
// `lower_file` returns `None` (silently skips a file) for any construct outside the IR subset. That is
// correct for the compiler, but opaque for the box-corpus `survey` — the roadmap of what to grow next.
// So lowering records WHY it last bailed in a thread-local: a coarse phase (`gate:class`, `deep:fun`)
// plus the innermost unsupported expr/stmt variant (`expr Lambda`, `call StringBuilder`). The compiler
// never reads this; only the `survey` binary does (via `lower_bail_reason`). Zero behavioural effect.
thread_local! {
    static BAIL_REASON: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}
fn set_bail(reason: &str) {
    BAIL_REASON.with(|r| *r.borrow_mut() = reason.to_string());
}
/// The reason `lower_file` last returned `None` — a diagnostic for the box-corpus survey only.
pub fn lower_bail_reason() -> String {
    BAIL_REASON.with(|r| r.borrow().clone())
}
/// The leading variant name of a `{:?}`-formatted AST node (`"Call { .. }"` → `"Call"`).
fn bail_variant(dbg: &str) -> &str {
    dbg.split(['(', '{', ' ']).next().unwrap_or("?")
}

struct ClassInfo {
    id: ClassId,
    internal: String,
    fields: Vec<(String, Ty)>,
    /// method name → (index into the class's `methods`, FunId, return Ty).
    methods: HashMap<String, (u32, u32, Ty)>,
    /// Base class internal name (`class B : A(…)`), for inherited field/method resolution.
    super_internal: Option<String>,
}

/// Lower a checked file to IR, or `None` if it uses anything outside the core subset.
pub fn lower_file(file: &ast::File, info: &TypeInfo, syms: &SymbolTable) -> Option<IrFile> {
    let mut lo = Lower {
        afile: file,
        info,
        syms,
        ir: IrFile {
            package: file.package.clone(),
            ..Default::default()
        },
        fun_ids: HashMap::new(),
        ext_fun_ids: HashMap::new(),
        ext_prop_get_ids: HashMap::new(),
        companion_consts: HashMap::new(),
        const_lits: HashMap::new(),
        ext_prop_set_ids: HashMap::new(),
        classes: HashMap::new(),
        statics: HashMap::new(),
        scope: Vec::new(),
        next_value: 0,
        cur_class: None,
        cur_field: None,
        field_accessor_props: std::collections::HashSet::new(),
        cur_fn_name: String::new(),
        cur_fn_suspend: false,
        cur_tparams: Vec::new(),
        lambda_seq: 0,
        boxed_elem: HashMap::new(),
        local_fun_ids: HashMap::new(),
        cur_ret_ty: Ty::Unit,
        cur_method_returns_unit_ref: false,
        try_finally_stack: Vec::new(),
        companions: HashMap::new(),
        computed_props: HashMap::new(),
        local_delegated: HashMap::new(),
        cur_tailrec: None,
        expr_depth: 0,
        inline_lambdas: Vec::new(),
        inline_active: Vec::new(),
        reified_subst: Vec::new(),
        inline_return: Vec::new(),
        inline_lambda_ret: Vec::new(),
    };

    set_bail("deep"); // refined below as lowering progresses (survey diagnostic only)
                      // Only files of top-level functions + *simple* classes take the IR path.
    for &d in &file.decls {
        match file.decl(d) {
            Decl::Fun(_) => {} // top-level function, extension function, or `inline fun` (expanded at call sites)
            Decl::Class(c) if is_simple_class(c) => {}
            Decl::Class(c) if c.is_enum() && is_simple_enum(c) => {}
            Decl::Class(c) if c.is_interface() && is_simple_interface(c) => {}
            Decl::Class(c) if c.is_object() && is_simple_object(c) => {}
            // A `val` extension property (`val Recv.name get() = …`) is lowered to a static getter; the
            // unsupported shapes (`var`, no `get()`) are skipped in pass 1.
            Decl::Property(p)
                if is_plain_body_prop(p)
                    || is_computed_prop(p)
                    || p.delegate.is_some()
                    || p.receiver.is_some() => {}
            other => {
                set_bail(match other {
                    Decl::Class(c) if c.is_object() => "gate:object",
                    Decl::Class(c) if c.is_interface() => "gate:interface",
                    Decl::Class(c) if c.is_enum() => "gate:enum",
                    Decl::Class(_) => "gate:class",
                    Decl::Property(_) => "gate:property",
                    _ => "gate:other",
                });
                return None;
            }
        }
    }

    // --- suspend (coroutines) lowerability gate ---------------------------------------------------
    // The coroutine transform itself lives in `jvm::suspend` (CPS signature + state machine); this only
    // decides whether the file is lowerable at all. ir_lower lowers a suspend fn body PLAINLY (a call
    // to another suspend fn becomes an ordinary call the pass then rewrites). Skip the whole file
    // (never miscompile) for shapes the pass doesn't model: an extension/member suspend fn, or a *call*
    // to a suspend fn from a NON-suspend function (call-site continuation threading isn't modeled — and
    // calling a suspend fn from a non-suspend context is a Kotlin error anyway). The pass itself skips
    // any suspend *body* shape it can't yet restructure.
    let top_suspend: Vec<String> = file
        .decls
        .iter()
        .filter_map(|&d| match file.decl(d) {
            Decl::Fun(f) if f.is_suspend => Some(f.name.clone()),
            _ => None,
        })
        .collect();
    let member_suspend: Vec<String> = file
        .decls
        .iter()
        .filter_map(|&d| match file.decl(d) {
            Decl::Class(c) => Some(c),
            _ => None,
        })
        .flat_map(|c| {
            c.methods
                .iter()
                .filter(|m| m.is_suspend)
                .map(|m| m.name.clone())
        })
        .collect();
    if !top_suspend.is_empty() || !member_suspend.is_empty() {
        // Extension suspend fns aren't modeled. (A leaf member suspend fn IS — its CPS signature on the
        // instance method; a member suspension point is skipped by the pass.)
        for &d in &file.decls {
            if let Decl::Fun(f) = file.decl(d) {
                if f.is_suspend && f.receiver.is_some() {
                    return None;
                }
            }
        }
        // Each body, tagged by whether its owner is a suspend fn. A NON-suspend body may not call a
        // top-level suspend fn (call-site threading isn't modeled, and it's a Kotlin error). A *member*
        // suspend fn may not be called at all yet (the flattener only models static suspend calls).
        let mut bodies: Vec<(AstExprId, bool)> = Vec::new();
        for &d in &file.decls {
            match file.decl(d) {
                Decl::Fun(f) => match &f.body {
                    FunBody::Expr(e) | FunBody::Block(e) => bodies.push((*e, f.is_suspend)),
                    FunBody::None => {}
                },
                Decl::Property(p) => bodies.extend(p.init.map(|e| (e, false))),
                Decl::Class(c) => {
                    for m in &c.methods {
                        match &m.body {
                            FunBody::Expr(e) | FunBody::Block(e) => bodies.push((*e, m.is_suspend)),
                            FunBody::None => {}
                        }
                    }
                    bodies.extend(
                        c.body_props
                            .iter()
                            .filter_map(|p| p.init)
                            .map(|e| (e, false)),
                    );
                }
            }
        }
        // A NON-suspend body may not call any suspend fn (top-level or member): call-site continuation
        // threading is only modeled inside a suspend body (and calling a suspend fn from a non-suspend
        // context is a Kotlin error). A suspend body may call both — the coroutine pass threads the
        // continuation into a static (`Call`) or member (`MethodCall`) suspend call.
        for (e, owner_suspend) in bodies {
            if !owner_suspend
                && top_suspend
                    .iter()
                    .chain(member_suspend.iter())
                    .any(|n| crate::resolve::expr_uses_name_pub(file, e, n))
            {
                return None;
            }
        }
    }

    // Pass 1a: register classes (id, fields) and reserve method FunIds.
    for &d in &file.decls {
        if let Decl::Class(c) = file.decl(d) {
            let internal = class_internal(file, &c.name);
            // A generic class gets a JVM class `Signature` (kotlinc does), matching its bytecode.
            if let Some(s) = class_generic_sig(c) {
                lo.ir.class_signatures.insert(internal.clone(), s);
            }
            // A field whose declared type is a bare type parameter (`val a: A`) gets a field `Signature`
            // (`TA;`); record the (field, type-param) pairs for the JVM backend to format.
            let field_sigs = class_field_tparams(c);
            if !field_sigs.is_empty() {
                lo.ir.field_signatures.insert(internal.clone(), field_sigs);
            }
            // An `inner class` captures the enclosing instance: a synthetic `this$0` field of the outer
            // type, prepended as the first constructor-parameter field.
            let inner_outer: Option<String> = c.inner_of.as_ref().map(|o| class_internal(file, o));
            // Constructor-parameter fields, then class-body-property fields (initialized in `init_body`).
            // A field's type is resolved with `ty_of` (file-local classes + built-ins); when that erases a
            // CLASSPATH reference type to `Any` (`ty_of` doesn't consult imports), recover the concrete type
            // from the lowerer's classpath-aware `ty_ref` so the field decl, constructor parameter and getter
            // all agree on the real type (e.g. `kotlin.uuid.Uuid`, `java.net.URL`).
            let mut ctor_fields: Vec<(String, Ty)> = c
                .props
                .iter()
                .filter(|p| p.is_property)
                .map(|p| (p.name.clone(), lo.field_ty(file, &p.ty)))
                .collect();
            if let Some(outer) = &inner_outer {
                ctor_fields.insert(0, ("this$0".to_string(), Ty::obj(outer)));
            }
            let ctor_param_count = ctor_fields.len() as u32;
            // Non-null reference constructor parameters get an `Intrinsics.checkNotNullParameter` guard
            // (kotlinc does); primitives, nullable params, and class type-parameters are skipped.
            // Parallel to ALL ctor params (declaration order, `ctor_args`) — a non-null reference plain
            // parameter is guarded too (it's still a constructor argument kotlinc null-checks).
            let mut ctor_param_checks: Vec<Option<String>> = c
                .props
                .iter()
                .map(|p| {
                    let ty = ty_of(file, &p.ty);
                    let is_type_param = c.type_params.contains(&p.ty.name);
                    if !p.ty.nullable && !is_type_param && ty.is_reference() {
                        Some(p.name.clone())
                    } else {
                        None
                    }
                })
                .collect();
            // The synthetic `this$0` is not null-checked (kotlinc doesn't guard it).
            if inner_outer.is_some() {
                ctor_param_checks.insert(0, None);
            }
            // Computed body properties (custom getter, no backing field) become `getX()` methods, not
            // fields — exclude them here.
            let body_fields: Vec<(String, Ty)> = c
                .body_props
                .iter()
                .filter(|p| is_backing_field_prop(p))
                .map(|p| {
                    // Use the classpath-aware `field_ty` (not `ty_of`, which erases a CLASSPATH reference
                    // type to `Any` since it doesn't consult imports) so a body property declared with a
                    // classpath type (`override val context: CoroutineContext`) keeps its concrete field +
                    // getter return type — matching an overridden interface member's descriptor.
                    let ty =
                        p.ty.as_ref()
                            .map(|r| lo.field_ty(file, r))
                            .unwrap_or_else(|| info.ty(p.init.unwrap()));
                    (p.name.clone(), ty)
                })
                .collect();
            // A custom-accessor property is read/written ONLY via `getX`/`setX` — record it so an
            // in-class access by name routes through the accessor, not a direct field read/write.
            for p in c.body_props.iter().filter(|p| is_field_accessor_prop(p)) {
                lo.field_accessor_props
                    .insert((internal.clone(), p.name.clone()));
            }
            // Guard each delegated member property: only the simple shape is modeled — a concrete
            // (non-value-class, no `provideDelegate`) delegate whose `getValue` return type matches the
            // property type exactly (generic erasure / value-class unboxing would need a cast the inline
            // accessor doesn't emit). Anything else skips the file rather than miscompile.
            for p in c.body_props.iter().filter(|p| p.delegate.is_some()) {
                let dt = info.ty(p.delegate.unwrap());
                let di = dt.obj_internal()?;
                let is_value_cls = |internal: &str| {
                    syms.class_by_internal(internal)
                        .is_some_and(|cs| cs.value_field.is_some())
                };
                if is_value_cls(di) || syms.method_of(di, "provideDelegate").is_some() {
                    return None;
                }
                let gv = syms.method_of(di, "getValue")?;
                let prop_ty = syms
                    .classes
                    .get(&c.name)
                    .and_then(|cs| {
                        cs.props
                            .iter()
                            .find(|(n, _, _)| n == &p.name)
                            .map(|(_, t, _)| *t)
                    })
                    .unwrap_or(Ty::Error);
                // A generic delegate's `getValue` returns the ERASED `Object` (`<T> getValue(): T`); the
                // getter inserts a `checkcast`/unbox to the property type (kotlinc does the same), bridged by
                // `coerce_erased`. Only an erased-REFERENCE return is bridgeable — a concrete mismatched
                // return isn't, so bail on that.
                if gv.ret != prop_ty && !gv.ret.is_reference() {
                    return None;
                }
                if prop_ty.obj_internal().is_some_and(is_value_cls) {
                    return None;
                }
            }
            // Synthetic `x$delegate` instance fields for delegated member properties — one per delegated
            // body property, in declaration order, placed AFTER the real backing-field props (so the
            // accessor-synthesis loop, which is driven by `field_props`, never indexes them).
            let delegate_fields: Vec<(String, Ty)> = c
                .body_props
                .iter()
                .filter(|p| p.delegate.is_some())
                .map(|p| (format!("{}$delegate", p.name), info.ty(p.delegate.unwrap())))
                .collect();
            // Interface delegation `: I by d` whose delegate `d` is a NON-`val` constructor parameter has
            // no backing field — kotlinc synthesizes a `private final $$delegate_<i>` (i = delegation index)
            // holding it, stored in the ctor. (A `val`-param delegate already has its own field.)
            let ctor_prop_names: std::collections::HashSet<&str> = c
                .props
                .iter()
                .filter(|p| p.is_property)
                .map(|p| p.name.as_str())
                .collect();
            let iface_delegate_fields: Vec<(String, Ty)> = c
                .delegations
                .iter()
                .enumerate()
                .filter(|(_, (_, d))| !ctor_prop_names.contains(d.as_str()))
                .filter_map(|(i, (_, d))| {
                    c.props
                        .iter()
                        .find(|p| &p.name == d)
                        .map(|p| (format!("$$delegate_{i}"), ty_of(file, &p.ty)))
                })
                .collect();
            // Interface delegation to an EXPRESSION (`: I by Impl()`): a synthesized `$$delegate_e<j>`
            // field holding the once-evaluated expression (stored in the ctor).
            let expr_delegate_fields: Vec<(String, Ty)> = c
                .delegation_exprs
                .iter()
                .enumerate()
                .map(|(j, (_, e))| (format!("$$delegate_e{j}"), info.ty(*e)))
                .collect();
            let fields: Vec<(String, Ty)> = ctor_fields
                .into_iter()
                .chain(body_fields)
                .chain(delegate_fields.iter().cloned())
                .chain(iface_delegate_fields.iter().cloned())
                .chain(expr_delegate_fields.iter().cloned())
                .collect();
            // The names backing a `lateinit var` — `IrField::is_lateinit` is set by name below (matched
            // in the final `fields`, so any `this$0` offset is handled). The backend null-checks every
            // read of such a field.
            let lateinit_names: std::collections::HashSet<&str> = c
                .body_props
                .iter()
                .filter(|p| p.is_lateinit)
                .map(|p| p.name.as_str())
                .collect();
            // Parallel to `fields`: each field's source type-parameter name (`val x: T` → `Some("T")`),
            // else `None`. Same ordering as `fields` (ctor props, `this$0` at 0 for an inner class, then
            // backing-field body props). Neutral metadata for the value-class pass's bound resolution.
            let mut field_type_params: Vec<Option<String>> = c
                .props
                .iter()
                .filter(|p| p.is_property)
                .map(|p| {
                    c.type_params
                        .contains(&p.ty.name)
                        .then(|| p.ty.name.clone())
                })
                .collect();
            if inner_outer.is_some() {
                field_type_params.insert(0, None);
            }
            field_type_params.extend(
                c.body_props
                    .iter()
                    .filter(|p| is_backing_field_prop(p))
                    .map(|_| None),
            );
            // Parallel `None` for each synthetic `x$delegate` field (concrete delegate type, no type-param).
            field_type_params.extend(delegate_fields.iter().map(|_| None));
            // Parallel `None` for each synthetic interface-delegation `$$delegate_N`/`$$delegate_e<j>` field.
            field_type_params.extend(iface_delegate_fields.iter().map(|_| None));
            field_type_params.extend(expr_delegate_fields.iter().map(|_| None));
            let class_ty = Ty::obj(&internal);
            // Resolve a base class (`: A(args)`): only a non-interface class declared in this file is
            // supported; extending a classpath/Java type isn't modeled yet → bail.
            let super_internal: Option<String> = match &c.base_class {
                Some(base) => {
                    let is_file_class = file.decls.iter().any(|&d| matches!(file.decl(d), Decl::Class(bc) if bc.name == *base && !bc.is_interface()));
                    if !is_file_class {
                        return None;
                    }
                    Some(class_internal(file, base))
                }
                None => None,
            };
            let superclass = if c.is_enum() {
                "java/lang/Enum".to_string()
            } else {
                super_internal
                    .clone()
                    .unwrap_or_else(|| "kotlin/Any".to_string())
            };
            // Implemented interfaces (`: I, J`): a file interface, or a classpath interface
            // (`Runnable`, `Comparator`) resolved through the library set; else bail.
            let mut iface_internals = Vec::new();
            for st in &c.supertypes {
                let is_file_iface = file.decls.iter().any(|&d| matches!(file.decl(d), Decl::Class(ic) if ic.name == *st && ic.is_interface()));
                if is_file_iface {
                    iface_internals.push(class_internal(file, st));
                    continue;
                }
                let resolved = lo
                    .syms
                    .class_names
                    .get(st)
                    .cloned()
                    .unwrap_or_else(|| st.clone());
                if lo
                    .syms
                    .libraries
                    .resolve_type(&resolved)
                    .map_or(false, |t| t.is_interface())
                {
                    iface_internals.push(resolved);
                } else {
                    return None;
                }
            }
            // Per-field finality aligned to `fields`: `this$0` is final; a property field is final unless
            // `var`; each synthetic `x$delegate` field is final.
            let field_finals: Vec<bool> = inner_outer
                .iter()
                .map(|_| true)
                .chain(c.props.iter().filter(|p| p.is_property).map(|p| !p.is_var))
                .chain(
                    c.body_props
                        .iter()
                        .filter(|p| is_backing_field_prop(p))
                        .map(|p| !p.is_var),
                )
                .chain(delegate_fields.iter().map(|_| true))
                .collect();
            let ir_fields: Vec<IrField> = fields
                .iter()
                .enumerate()
                .map(|(i, (n, t))| {
                    let default = c
                        .props
                        .iter()
                        .find(|p| p.name == *n)
                        .and_then(|p| p.default)
                        .and_then(|d| const_default_of(file, d))
                        // Widen the literal to the field's type so its JVM slot width/kind matches the
                        // field local (`val x: Long = 5` parses `5` as `Int` — store it as `Long`).
                        .map(|cst| widen_const_to(cst, *t));
                    let ir = ty_to_ir(*t);
                    // Re-attach a generic field's TYPE ARGUMENTS from the property's source `TypeRef`
                    // (`ty_of`/`ty_to_ir` erase them for a general `Obj`). Read straight from the AST so
                    // `Box<Int>` keeps `<Int>` — the serialization extension needs it to build a nested
                    // generic element serializer (`Box.serializer(IntSerializer)`); descriptors still
                    // erase, so this is additive metadata on the field type only.
                    let ir = match c.props.iter().find(|p| p.name == *n) {
                        Some(p)
                            if !p.ty.targs.is_empty() && ir.non_null().obj_internal().is_some() =>
                        {
                            let fq_name = ir.non_null().obj_internal().unwrap();
                            let targs: Vec<Ty> =
                                p.ty.targs
                                    .iter()
                                    .map(|a| field_ty_with_args(file, a))
                                    .collect();
                            let base = Ty::obj_args(fq_name, &targs);
                            if ir.is_nullable() {
                                Ty::nullable(base)
                            } else {
                                base
                            }
                        }
                        _ => ir,
                    };
                    // A field carries its declared nullability into the IrType (`Ty` drops it). The
                    // JVM value-class pass keys boxing + null-check elision on a value class's
                    // underlying `?` (`X(val v: Int?)` → nullable `Integer`).
                    let ir = if c.props.iter().any(|p| p.name == *n && p.ty.nullable) {
                        mark_nullable(ir)
                    } else {
                        ir
                    };
                    IrField {
                        name: n.clone(),
                        ty: ir,
                        type_param: field_type_params[i].clone(),
                        default,
                        // `field_finals` covers up to the `x$delegate` fields; the trailing
                        // interface-delegation fields (`$$delegate_N`/`$$delegate_e<j>`) default to
                        // non-final, matching the prior `field_final.get(i).unwrap_or(false)`.
                        is_final: field_finals.get(i).copied().unwrap_or(false),
                        is_private: true, // user backing fields are all private (default)
                        is_lateinit: lateinit_names.contains(n.as_str()),
                    }
                })
                .collect();
            let id = lo.ir.add_class(IrClass {
                fq_name: internal.clone(),
                serial_names: serial_names_of(file, c),
                custom_serializer: lo.custom_serializer_of(c),
                field_serializers: lo.field_serializers_of(c),
                contextual_fields: lo.contextual_fields_of(c),
                is_value: c.is_value,
                type_param_bounds: c
                    .type_param_bounds
                    .iter()
                    .map(|(n, tr)| {
                        let bt = ty_to_ir(ty_of(file, tr));
                        (n.clone(), if tr.nullable { mark_nullable(bt) } else { bt })
                    })
                    .collect(),
                type_params: c.type_params.clone(),
                supertypes: vec![],
                fields: ir_fields,
                ctor_param_count,
                // All primary-ctor params in declaration order; `is_field` = it's a `val`/`var` property.
                // An inner class's synthetic `this$0` (the outer instance) is the first field param.
                ctor_args: inner_outer
                    .iter()
                    .map(|o| (ty_to_ir(Ty::obj(o)), true))
                    .chain(c.props.iter().map(|p| {
                        // Carry a declared `?` into the ctor-param IrType (like the field), so a nullable
                        // value-class parameter erases to the boxed `X` consistently with its getter/field.
                        // Use `field_ty` so a classpath-typed param matches the field/getter (not erased `Any`).
                        let t = ty_to_ir(lo.field_ty(file, &p.ty));
                        let t = if p.ty.nullable { mark_nullable(t) } else { t };
                        (t, p.is_property)
                    }))
                    .collect(),
                init_body: None,
                explicit_param_stores: false,
                methods: vec![],
                is_interface: c.is_interface(),
                is_annotation: c.is_annotation(),
                annotation_impl_of: None,
                is_sealed: c.is_sealed,
                is_abstract: c.is_abstract,
                superclass,
                super_args: Vec::new(),
                // Entry names now; constructor-arg value-ids are lowered in pass 2.
                enum_entries: c
                    .enum_entries
                    .iter()
                    .map(|n| (n.clone(), Vec::new()))
                    .collect(),
                enum_entry_subclass: vec![None; c.enum_entries.len()],
                enum_entry_of: None,
                prop_ref: None,
                func_ref: None,
                bridges: Vec::new(),
                interfaces: iface_internals,
                is_object: c.is_object(),
                ctor_param_checks,
                is_companion: false,
                companion_class: None,
                secondary_ctors: vec![],
                has_primary_ctor: c.has_primary_ctor,
            });
            // For an `annotation class`, ALSO emit the synthetic IMPLEMENTATION class (kotlinc's
            // `…$annotationImpl`) implementing the annotation interface + the `java.lang.annotation.
            // Annotation` contract, so `A(args)` can construct an annotation instance. The backend
            // generates the whole impl from `fields` (see `emit_annotation_impl_class`).
            if c.is_annotation() {
                let mut impl_class = lo.ir.classes[id as usize].clone();
                impl_class.fq_name = format!("{internal}$annotationImpl");
                impl_class.is_annotation = false;
                impl_class.annotation_impl_of = Some(internal.clone());
                impl_class.interfaces = vec![internal.clone()];
                impl_class.superclass = "java/lang/Object".to_string();
                impl_class.supertypes = vec![];
                impl_class.methods = vec![];
                lo.ir.add_class(impl_class);
            }
            let mut methods = HashMap::new();
            let mut method_fids = Vec::new();
            for (mi, m) in c.methods.iter().enumerate() {
                let sig = syms.classes.get(&c.name)?.methods.get(&m.name)?;
                let ret = sig.ret;
                let params: Vec<Ty> = sig.params.iter().map(|t| ty_to_ir(*t)).collect();
                let param_checks = param_checks_for(m, &sig.params);
                // The checker `Ty` carries no nullability, so recover the declared `?` from the method's
                // AST return type (`fun f(): T?`) — same as a top-level function. A nullable value-class
                // return (`Ucn?`) must stay the boxed `X`, not erase to the unboxed underlying.
                let ret_ir = ty_to_ir(ret);
                let ret_ir = if m.ret.as_ref().is_some_and(|r| r.nullable) {
                    mark_nullable(ret_ir)
                } else {
                    ret_ir
                };
                let fid = lo.ir.add_fun(IrFunction {
                    name: m.name.clone(),
                    params,
                    ret: ret_ir,
                    body: None,
                    is_static: false,
                    dispatch_receiver: Some(internal.clone()),
                    param_checks,
                });
                // Mark a method with default parameters now (pass 1) so a call lowered before this
                // class's pass-2 body sees that it has defaults; the real default exprs are lowered in
                // pass 2 and overwrite this marker. (>31 parameters — kotlinc's multi-`int` mask — aren't
                // modeled; leaving them unmarked makes an omitted-arg call bail, so the file is skipped.)
                if m.params.iter().any(|p| p.default.is_some()) && m.params.len() <= 31 {
                    lo.ir.fn_param_defaults.insert(fid, Vec::new());
                    lo.ir
                        .fn_param_names
                        .insert(fid, m.params.iter().map(|p| p.name.clone()).collect());
                    // An INTERFACE method is abstract (no body), so pass 2's body loop never fills its
                    // default exprs — lower them HERE. Restrict to CONSTANT defaults (a literal needs no
                    // param/`this` scope), so the `$default` stub is always valid; a non-constant interface
                    // default isn't modeled — drop the marker so an omitted-arg call bails (skips), never
                    // miscompiles. kotlinc realizes interface defaults via a static `<iface>.<name>$default`.
                    if c.is_interface() {
                        let mut defaults = Vec::new();
                        let mut modelable = true;
                        for (p, t) in m.params.iter().zip(&sig.params) {
                            match p.default {
                                None => defaults.push(None), // a required parameter
                                Some(d) if is_const_literal(file, d) => {
                                    match lo.lower_arg(d, &ty_to_ir(*t)) {
                                        Some(e) => defaults.push(Some(e)),
                                        None => {
                                            modelable = false;
                                            break;
                                        }
                                    }
                                }
                                Some(_) => {
                                    modelable = false; // a non-constant default isn't modeled
                                    break;
                                }
                            }
                        }
                        if modelable {
                            lo.ir.fn_param_defaults.insert(fid, defaults);
                        } else {
                            lo.ir.fn_param_defaults.remove(&fid);
                            lo.ir.fn_param_names.remove(&fid);
                        }
                    }
                }
                // Tag a `suspend` member method for the coroutine pass (same as a top-level suspend fun).
                if m.is_suspend {
                    lo.ir.suspend_funs.push(fid);
                }
                // A generic member method gets the same JVM `Signature` as a generic top-level function.
                if let Some(s) = fn_generic_sig(m) {
                    lo.ir.signatures.insert(fid, s);
                }
                methods.insert(m.name.clone(), (mi as u32, fid, ret));
                method_fids.push(fid);
            }
            // Computed body properties → `getX()` instance methods (no backing field).
            for p in c.body_props.iter().filter(|p| is_computed_prop(p)) {
                let ty = body_prop_ty(file, info, p);
                let gname = getter_name(&p.name);
                let mi = method_fids.len() as u32;
                let fid = lo.ir.add_fun(IrFunction {
                    name: gname.clone(),
                    params: vec![],
                    ret: ty_to_ir(ty),
                    body: None,
                    is_static: false,
                    dispatch_receiver: Some(internal.clone()),
                    param_checks: vec![],
                });
                methods.insert(gname, (mi, fid, ty));
                method_fids.push(fid);
            }
            // Delegated body properties (`val/var x by Del()`) → a `getX()` (and `setX()` for a `var`)
            // instance method that calls the delegate's `getValue`/`setValue`. Bodies built in pass 2.
            for p in c.body_props.iter().filter(|p| p.delegate.is_some()) {
                let prop_ty = syms
                    .classes
                    .get(&c.name)
                    .and_then(|cs| {
                        cs.props
                            .iter()
                            .find(|(n, _, _)| n == &p.name)
                            .map(|(_, t, _)| *t)
                    })
                    .unwrap_or(Ty::Error);
                let gname = getter_name(&p.name);
                let mi = method_fids.len() as u32;
                let fid = lo.ir.add_fun(IrFunction {
                    name: gname.clone(),
                    params: vec![],
                    ret: ty_to_ir(prop_ty),
                    body: None,
                    is_static: false,
                    dispatch_receiver: Some(internal.clone()),
                    param_checks: vec![],
                });
                methods.insert(gname, (mi, fid, prop_ty));
                method_fids.push(fid);
                if p.is_var {
                    let sname = setter_name(&p.name);
                    let mi = method_fids.len() as u32;
                    let fid = lo.ir.add_fun(IrFunction {
                        name: sname.clone(),
                        params: vec![ty_to_ir(prop_ty)],
                        ret: Ty::Unit,
                        body: None,
                        is_static: false,
                        dispatch_receiver: Some(internal.clone()),
                        param_checks: vec![],
                    });
                    methods.insert(sname, (mi, fid, Ty::Unit));
                    method_fids.push(fid);
                }
            }
            // Abstract properties (`abstract val x: T`, and every interface property) → an abstract
            // `getX()` (and `setX()` for a `var`) the implementing class overrides with its field
            // accessor.
            {
                for p in c
                    .body_props
                    .iter()
                    .filter(|p| p.is_abstract || (c.is_interface() && !is_computed_prop(p)))
                {
                    let ty =
                        p.ty.as_ref()
                            .map(|r| ty_of(file, r))
                            .unwrap_or_else(|| Ty::obj("kotlin/Any"));
                    let gname = getter_name(&p.name);
                    if !methods.contains_key(&gname) {
                        let mi = method_fids.len() as u32;
                        let fid = lo.ir.add_fun(IrFunction {
                            name: gname.clone(),
                            params: vec![],
                            ret: ty_to_ir(ty),
                            body: None,
                            is_static: false,
                            dispatch_receiver: Some(internal.clone()),
                            param_checks: vec![],
                        });
                        methods.insert(gname, (mi, fid, ty));
                        method_fids.push(fid);
                    }
                    if p.is_var {
                        let sname = setter_name(&p.name);
                        if !methods.contains_key(&sname) {
                            let mi = method_fids.len() as u32;
                            let fid = lo.ir.add_fun(IrFunction {
                                name: sname.clone(),
                                params: vec![ty_to_ir(ty)],
                                ret: Ty::Unit,
                                body: None,
                                is_static: false,
                                dispatch_receiver: Some(internal.clone()),
                                param_checks: vec![],
                            });
                            methods.insert(sname, (mi, fid, Ty::Unit));
                            method_fids.push(fid);
                        }
                    }
                }
            }
            // Synthesize `getX()`/`setX()` accessors for each backing-field property (kotlinc emits
            // them; the fields are private). Getter returns the field; setter (var only) writes it.
            // Enums keep their existing shape (separate emit path); interfaces have no backing fields.
            if !c.is_interface() && !c.is_enum() {
                let field_props: Vec<(String, bool)> = c
                    .props
                    .iter()
                    .filter(|p| p.is_property)
                    .map(|p| (p.name.clone(), p.is_var))
                    .chain(
                        c.body_props
                            .iter()
                            .filter(|p| is_backing_field_prop(p))
                            .map(|p| (p.name.clone(), p.is_var)),
                    )
                    .collect();
                // An inner class's `this$0` occupies field index 0, so the declared properties' fields
                // are shifted by one — map each property's `field_props` index to its real field index.
                let field_offset = if inner_outer.is_some() { 1 } else { 0 };
                // For a generic class, a field typed by a bare type parameter (`val a: A`) → its
                // synthesized accessors carry a JVM `Signature` (`getA()` → `()TA;`, `setA(TA;)V`).
                let field_tp: std::collections::HashMap<String, String> =
                    class_field_tparams(c).into_iter().collect();
                for (pi, (pname, is_var)) in field_props.iter().enumerate() {
                    let fidx = pi + field_offset;
                    let fty = fields[fidx].1;
                    // Use the class field's IrType (carries declared `?` via `mark_nullable`), not the
                    // bare `Ty` — so a nullable value-class property getter erases consistently with the
                    // field (`z: Z1?` → `LZ1;`, not the collapsed final underlying).
                    let fty_ir = lo.ir.classes[id as usize].fields[fidx].ty.clone();
                    let gname = getter_name(pname);
                    if !methods.contains_key(&gname) {
                        // A plain field read; if the field is `lateinit` the backend's `GetField`
                        // emission inserts the uninitialized null-check throw (so does every other read).
                        let this_e = lo.ir.add_expr(IrExpr::GetValue(0));
                        let gf = lo.ir.add_expr(IrExpr::GetField {
                            receiver: this_e,
                            class: id,
                            index: fidx as u32,
                        });
                        let ret = lo.ir.add_expr(IrExpr::Return(Some(gf)));
                        let body = lo.ir.add_expr(IrExpr::Block {
                            stmts: vec![ret],
                            value: None,
                        });
                        let mi = method_fids.len() as u32;
                        let fid = lo.ir.add_fun(IrFunction {
                            name: gname.clone(),
                            params: vec![],
                            ret: fty_ir.clone(),
                            body: Some(body),
                            is_static: false,
                            dispatch_receiver: Some(internal.clone()),
                            param_checks: vec![],
                        });
                        if let Some(tp) = field_tp.get(pname) {
                            lo.ir.signatures.insert(
                                fid,
                                crate::ir::IrGenericSig {
                                    type_params: vec![],
                                    param_tparams: vec![],
                                    ret_tparam: Some(tp.clone()),
                                },
                            );
                        }
                        methods.insert(gname, (mi, fid, fty));
                        method_fids.push(fid);
                    }
                    if *is_var {
                        let sname = setter_name(pname);
                        if !methods.contains_key(&sname) {
                            let this_e = lo.ir.add_expr(IrExpr::GetValue(0));
                            let v = lo.ir.add_expr(IrExpr::GetValue(1));
                            let sf = lo.ir.add_expr(IrExpr::SetField {
                                receiver: this_e,
                                class: id,
                                index: fidx as u32,
                                value: v,
                            });
                            let body = lo.ir.add_expr(IrExpr::Block {
                                stmts: vec![sf],
                                value: None,
                            });
                            let mi = method_fids.len() as u32;
                            let fid = lo.ir.add_fun(IrFunction {
                                name: sname.clone(),
                                params: vec![fty_ir.clone()],
                                ret: Ty::Unit,
                                body: Some(body),
                                is_static: false,
                                dispatch_receiver: Some(internal.clone()),
                                param_checks: vec![],
                            });
                            // `var x = …; private set` — the setter is emitted `private`.
                            if c.body_props.iter().any(|p| {
                                &p.name == pname && p.setter.as_ref().is_some_and(|s| s.is_private)
                            }) {
                                lo.ir.private_methods.insert(fid);
                            }
                            if let Some(tp) = field_tp.get(pname) {
                                lo.ir.signatures.insert(
                                    fid,
                                    crate::ir::IrGenericSig {
                                        type_params: vec![],
                                        param_tparams: vec![Some(tp.clone())],
                                        ret_tparam: None,
                                    },
                                );
                            }
                            methods.insert(sname, (mi, fid, Ty::Unit));
                            method_fids.push(fid);
                        }
                    }
                }
            }
            lo.ir.classes[id as usize].methods = method_fids;
            let _ = class_ty;
            lo.classes.insert(
                internal.clone(),
                ClassInfo {
                    id,
                    internal: internal.clone(),
                    fields,
                    methods,
                    super_internal,
                },
            );
            // A `companion object`'s `const val`s become `public static final` + `ConstantValue` fields
            // on the OUTER class (kotlinc's layout) — emitted as owned statics here, read as `getstatic
            // C.X`. (`companion_props_lowerable` guarantees they are all plain const at the gate.)
            for cp in &c.companion_props {
                if !cp.is_const {
                    continue;
                }
                let cty = body_prop_ty(file, info, cp);
                lo.cur_class = None;
                lo.scope.clear();
                lo.next_value = 0;
                if let (Some(initx), false) = (cp.init, cty == Ty::Error) {
                    if let Some(init) = lo.lower_arg(initx, &ty_to_ir(cty)) {
                        lo.ir.statics.push(crate::ir::IrStatic {
                            name: cp.name.clone(),
                            ty: ty_to_ir(cty),
                            init,
                            is_var: false,
                            is_const: true,
                            owner: Some(internal.clone()),
                        });
                        lo.companion_consts
                            .insert((internal.clone(), cp.name.clone()), cty);
                    }
                }
            }
            // `companion object` with methods → a synthesized `C$Companion` class (private ctor, the
            // companion methods as instance methods) + a `Companion` field on the outer class. Also
            // synthesized for a method-LESS companion that declares a supertype (`companion object :
            // EmptyContinuation()`) — it must still be emitted (extending its base/interfaces) so the
            // companion is usable as a value of that type.
            if !c.companion_methods.is_empty()
                || c.companion_base.is_some()
                || !c.companion_supertypes.is_empty()
            {
                let comp_fq = format!("{internal}$Companion");
                // The companion's declared supertypes (`companion object : Base, I`): make the synthesized
                // companion implement its declared INTERFACES so it is genuinely an `I` at runtime. The
                // base CLASS is left as `kotlin/Any` (as before this change ignored the whole list) — a real
                // `super(args)` call isn't built in this registration pass; a companion that NEEDS its base
                // type (e.g. the coroutine `EmptyContinuation`) simply isn't usable as that type yet.
                // Only FILE interfaces are added, and only when no method needs a BRIDGE: the synthesized
                // companion isn't run through interface-bridge generation, so a companion method overriding
                // an interface method with a DIFFERENT erased descriptor (generic/covariant) would
                // `AbstractMethodError`. Verify each overriding method matches exactly; otherwise (or for a
                // classpath interface, whose methods aren't checked here) skip the file — never miscompile.
                let mut comp_ifaces = Vec::new();
                let csig0 = syms.classes.get(&c.name)?;
                for st in &c.companion_supertypes {
                    let is_file_iface = file.decls.iter().any(|&d| matches!(file.decl(d), Decl::Class(ic) if ic.name == *st && ic.is_interface()));
                    if !is_file_iface {
                        return None;
                    }
                    let iface_internal = class_internal(file, st);
                    if let Some(isig) = syms.class_by_internal(&iface_internal) {
                        for (mname, cm) in &csig0.static_methods {
                            if let Some(im) = isig.methods.get(mname) {
                                let ip: String = im.params.iter().map(|t| t.descriptor()).collect();
                                let cp: String = cm.params.iter().map(|t| t.descriptor()).collect();
                                if ip != cp || im.ret.descriptor() != cm.ret.descriptor() {
                                    return None; // would need a bridge — skip, never miscompile
                                }
                            }
                        }
                    }
                    comp_ifaces.push(iface_internal);
                }
                // A companion with a declared base CLASS (`companion object : Base()`): make the synthesized
                // companion extend it (so the companion — used as a value — is an instance of `Base`, e.g.
                // the coroutine `EmptyContinuation`). Only a FILE base with NO explicit base args is modeled:
                // a no-arg base → `super()`; an all-defaulted base → fill the base's default exprs into
                // `super(…)` (mirrors the regular-class super-default-fill). The plan is computed as owned
                // data first so the `syms` borrow is dropped before lowering. Any other shape → keep `Any`.
                let comp_base_plan: Option<(String, Vec<(AstExprId, Ty)>)> = match &c.companion_base
                {
                    Some(base) if c.companion_base_args.is_empty() => {
                        let is_file_class = file.decls.iter().any(|&d| matches!(file.decl(d), Decl::Class(bc) if bc.name == *base && !bc.is_interface()));
                        if !is_file_class {
                            return None;
                        }
                        let base_internal = class_internal(file, base);
                        let bsig = syms.class_by_internal(&base_internal)?;
                        let n = bsig.ctor_params.len();
                        if n == 0 {
                            Some((base_internal, Vec::new()))
                        } else if bsig.ctor_defaults.len() >= n
                            && bsig.ctor_defaults[..n].iter().all(|d| d.is_some())
                        {
                            let plan = (0..n)
                                .map(|i| (bsig.ctor_defaults[i].unwrap(), bsig.ctor_params[i]))
                                .collect();
                            Some((base_internal, plan))
                        } else {
                            return None; // a required base param without a default — can't fill
                        }
                    }
                    Some(_) => return None, // explicit base args on a companion — not modeled
                    None => None,
                };
                let (comp_super, comp_super_args) =
                    if let Some((base_internal, plan)) = comp_base_plan {
                        lo.scope.clear();
                        lo.next_value = 0;
                        lo.cur_class = Some(comp_fq.clone());
                        let this_v = lo.fresh_value();
                        lo.scope
                            .push(("this".to_string(), this_v, Ty::obj(&comp_fq)));
                        let mut sargs = Vec::new();
                        for (d, pt) in &plan {
                            sargs.push(lo.lower_arg(*d, &ty_to_ir(*pt))?);
                        }
                        (base_internal, sargs)
                    } else {
                        ("kotlin/Any".to_string(), Vec::new())
                    };
                let comp_id = lo.ir.add_class(IrClass {
                    serial_names: Vec::new(),
                    custom_serializer: None,
                    field_serializers: Vec::new(),
                    contextual_fields: Vec::new(),
                    fq_name: comp_fq.clone(),
                    is_value: false,
                    type_param_bounds: vec![],
                    type_params: Vec::new(),
                    supertypes: vec![],
                    fields: vec![],
                    ctor_param_count: 0,
                    ctor_args: vec![],
                    init_body: None,
                    explicit_param_stores: false,
                    methods: vec![],
                    is_interface: false,
                    is_annotation: false,
                    annotation_impl_of: None,

                    is_sealed: false,
                    is_abstract: false,
                    superclass: comp_super,
                    super_args: comp_super_args,
                    enum_entries: vec![],
                    enum_entry_subclass: vec![],
                    enum_entry_of: None,
                    prop_ref: None,
                    func_ref: None,
                    bridges: vec![],
                    interfaces: comp_ifaces,
                    is_object: false,
                    ctor_param_checks: vec![],
                    is_companion: true,
                    companion_class: None,
                    secondary_ctors: vec![],
                    has_primary_ctor: true,
                });
                let csig = syms.classes.get(&c.name)?;
                let mut cmethods = HashMap::new();
                let mut cmethod_fids = Vec::new();
                for (mi, m) in c.companion_methods.iter().enumerate() {
                    let sig = csig.static_methods.get(&m.name)?;
                    let ret = sig.ret;
                    let params: Vec<Ty> = sig.params.iter().map(|t| ty_to_ir(*t)).collect();
                    let param_checks = param_checks_for(m, &sig.params);
                    let fid = lo.ir.add_fun(IrFunction {
                        name: m.name.clone(),
                        params,
                        ret: ty_to_ir(ret),
                        body: None,
                        is_static: false,
                        dispatch_receiver: Some(comp_fq.clone()),
                        param_checks,
                    });
                    cmethods.insert(m.name.clone(), (mi as u32, fid, ret));
                    cmethod_fids.push(fid);
                }
                lo.ir.classes[comp_id as usize].methods = cmethod_fids;
                lo.ir.classes[id as usize].companion_class = Some(comp_fq.clone());
                lo.classes.insert(
                    comp_fq.clone(),
                    ClassInfo {
                        id: comp_id,
                        internal: comp_fq.clone(),
                        fields: vec![],
                        methods: cmethods,
                        super_internal: None,
                    },
                );
                lo.companions.insert(internal.clone(), comp_fq);
            }
            // A `data class`'s equals/hashCode/toString/componentN are Kotlin language semantics —
            // synthesize them here as ordinary IR methods (backend-agnostic), registered so calls
            // resolve and the generic method emitter handles them.
            if c.is_data {
                // An inner `data class`'s synthetic `this$0` sits at field 0, which `synth_data_members`
                // (componentN/copy/equals/hashCode over `fields[..n]`) would treat as the first data
                // property — skip rather than miscompile.
                if c.inner_of.is_some() {
                    return None;
                }
                lo.synth_data_members(&internal, id, ctor_param_count as usize);
            }
            // A `@JvmInline value class` is emitted as a plain single-field class here (field, `<init>`,
            // getter); the JVM `value_classes` pass synthesizes its unboxed-support members
            // (`box-impl`/`unbox-impl`/`constructor-impl`/`equals-impl0`/`equals`/`hashCode`/`toString`).
            // Interface delegation `: I by d` is sugar — synthesize a forwarder for each of `I`'s
            // methods that calls `this.d.method(args)`. Bails the file if a delegate can't be modeled.
            if !c.delegations.is_empty() || !c.delegation_exprs.is_empty() {
                lo.synth_delegation_forwarders(file, c, &internal, id)?;
            }
        }
    }
    // Pass 1b: register top-level functions and extension functions. An `inline fun` is not emitted
    // as a standalone method — it is expanded at each call site (pass 2 skips it too).
    for &d in &file.decls {
        if let Decl::Fun(f) = file.decl(d) {
            if f.is_inline {
                continue;
            }
            if let Some(recv_ref) = &f.receiver {
                // Extension function `fun Recv.name(…)` → a static method whose first parameter is the
                // receiver (Kotlin's compilation strategy). Keyed by (receiver descriptor, name). A
                // receiver that doesn't resolve to a concrete type (a generic `T.foo()`) isn't modeled —
                // bail rather than guess `Object`.
                let recv_ty = lo.ext_receiver_ty(file, recv_ref);
                if recv_ty == Ty::Error {
                    return None;
                }
                let recv_desc = recv_ty.descriptor();
                let sig = syms.ext_funs.get(&(recv_desc.clone(), f.name.clone()))?;
                let mut params = vec![ty_to_ir(recv_ty)];
                params.extend(sig.params.iter().map(|t| ty_to_ir(*t)));
                let ret = ty_to_ir(sig.ret);
                let id = lo.ir.add_fun(IrFunction {
                    name: f.name.clone(),
                    params,
                    ret,
                    body: None,
                    is_static: true,
                    dispatch_receiver: None,
                    param_checks: vec![],
                });
                lo.ext_fun_ids.insert((recv_desc, f.name.clone()), id);
            } else {
                // This declaration's own overload (matched by erased parameter descriptors when the
                // name is overloaded).
                let sigs = syms.funs.get(&f.name)?;
                let sig = if sigs.len() == 1 {
                    &sigs[0]
                } else {
                    let want: String = f
                        .params
                        .iter()
                        .map(|p| ty_of(file, &p.ty).descriptor())
                        .collect();
                    sigs.iter()
                        .find(|s| crate::resolve::erased_params_key(s) == want)?
                };
                let params: Vec<Ty> = sig
                    .params
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let ir = ty_to_ir(*t);
                        if f.params.get(i).is_some_and(|p| p.ty.nullable) {
                            mark_nullable(ir)
                        } else {
                            ir
                        }
                    })
                    .collect();
                let ret = ty_to_ir(
                    info.fun_ret_overrides
                        .get(&f.name)
                        .copied()
                        .unwrap_or(sig.ret),
                );
                let ret = if f.ret.as_ref().is_some_and(|r| r.nullable) {
                    mark_nullable(ret)
                } else {
                    ret
                };
                let param_checks = param_checks_for(f, &sig.params);
                let id = lo.ir.add_fun(IrFunction {
                    name: f.name.clone(),
                    params,
                    ret,
                    body: None,
                    is_static: true,
                    dispatch_receiver: None,
                    param_checks,
                });
                lo.fun_ids
                    .insert((f.name.clone(), crate::resolve::erased_params_key(sig)), id);
                // Emit a JVM generic `Signature` for a type-parameterized function (kotlinc does), so the
                // bytecode matches for generics. `None` for non-generic / not-yet-modeled shapes.
                if let Some(s) = fn_generic_sig(f) {
                    lo.ir.signatures.insert(id, s);
                }
                // Tag a `suspend fun` for the coroutine pass (`jvm::suspend`), which owns the whole
                // transform (CPS signature now; state machine later) — ir_lower keeps the plain form,
                // mirroring how value classes are lowered plain here and transformed in a later pass.
                if f.is_suspend {
                    lo.ir.suspend_funs.push(id);
                }
            }
        }
    }
    // Pass 1b': register lifted local functions (`fun` inside a function body) as private static
    // methods on the facade. The checker mangled each to `$local$<stmtid>` and rejected captures, so
    // only non-capturing local functions reach here. A call to one routes to its `FunId` in pass 2.
    for (i, s) in file.stmt_arena.iter().enumerate() {
        if let Stmt::LocalFun(_) = s {
            let stmt_id = crate::ast::StmtId(i as u32);
            if let Some((mangled, sig)) = info.local_fun_sigs.get(&stmt_id) {
                // Captured outer locals become extra leading parameters (a boxed var is passed as its
                // `Ref` holder reference, an ordinary one by value), then the declared parameters.
                let mut params: Vec<Ty> = Vec::new();
                if let Some(caps) = info.local_fun_captures.get(&stmt_id) {
                    for (name, ty) in caps {
                        params.push(captured_param_ir(name, *ty, &info.boxed_vars));
                    }
                }
                params.extend(sig.params.iter().map(|t| ty_to_ir(*t)));
                let ret = ty_to_ir(sig.ret);
                let id = lo.ir.add_fun(IrFunction {
                    name: mangled.clone(),
                    params,
                    ret,
                    body: None,
                    is_static: true,
                    dispatch_receiver: None,
                    param_checks: vec![],
                });
                lo.local_fun_ids.insert(stmt_id, id);
            }
        }
    }
    // Pass 1c: assign top-level-property indices (initializers lowered in pass 2). Registered before
    // any body so a function may read a top-level property as `GetStatic`.
    for &d in &file.decls {
        if let Decl::Property(p) = file.decl(d) {
            // A top-level delegated property (`val x: T by Del()`): register a `getX()` accessor so reads
            // route to it (like a computed property), and RESERVE the two synthetic backing-field static
            // slots (`x$delegate`, `x$kprop`) in declaration order so later non-delegated statics get the
            // matching indices. The field initializers + `getX()` body are built in pass 2.
            if p.delegate.is_some() {
                // An EXTENSION delegated property (`val Recv.x by …`) isn't modeled — skip the file
                // (pass 2 would otherwise treat it as an extension property and find no getter).
                if p.receiver.is_some() {
                    return None;
                }
                let ty = lo.delegated_prop_type(p)?;
                let fid = lo.ir.add_fun(IrFunction {
                    name: getter_name(&p.name),
                    params: vec![],
                    ret: ty_to_ir(ty),
                    body: None,
                    is_static: true,
                    dispatch_receiver: None,
                    param_checks: vec![],
                });
                lo.computed_props.insert(p.name.clone(), (fid, ty));
                let d_ty = p.delegate.map(|de| info.ty(de)).unwrap_or(Ty::Error);
                let d_idx = lo.statics.len() as u32;
                lo.statics
                    .insert(format!("{}$delegate", p.name), (d_idx, d_ty));
                let k_idx = lo.statics.len() as u32;
                lo.statics.insert(
                    format!("{}$kprop", p.name),
                    (k_idx, Ty::obj("kotlin/reflect/KProperty")),
                );
                continue;
            }
            // An extension property (`val/var Recv.name: T get() = … [set(v) = …]`) → a static
            // `getName(Recv): T` (and, for a `var`, `setName(Recv, T)`), exactly like an extension
            // function's lowering. No backing field; only a `get()`-bodied property is modeled. A `var`
            // needs an explicit `set(v) { … }` body (no backing field to default to).
            if let Some(recv_ref) = &p.receiver {
                if p.getter.is_none() || p.delegate.is_some() {
                    return None;
                }
                let has_set_body = p
                    .setter
                    .as_ref()
                    .is_some_and(|s| s.body.is_some() && s.param.is_some());
                if p.is_var && !has_set_body {
                    return None;
                }
                let recv_ty = ty_of(file, recv_ref);
                let recv_desc = recv_ty.descriptor();
                let pty = body_prop_ty(file, info, p);
                let gfid = lo.ir.add_fun(IrFunction {
                    name: getter_name(&p.name),
                    params: vec![ty_to_ir(recv_ty)],
                    ret: ty_to_ir(pty),
                    body: None,
                    is_static: true,
                    dispatch_receiver: None,
                    param_checks: vec![],
                });
                lo.ext_prop_get_ids
                    .insert((recv_desc.clone(), p.name.clone()), gfid);
                if p.is_var {
                    let sfid = lo.ir.add_fun(IrFunction {
                        name: setter_name(&p.name),
                        params: vec![ty_to_ir(recv_ty), ty_to_ir(pty)],
                        ret: ty_to_ir(Ty::Unit),
                        body: None,
                        is_static: true,
                        dispatch_receiver: None,
                        param_checks: vec![],
                    });
                    lo.ext_prop_set_ids
                        .insert((recv_desc, p.name.clone()), sfid);
                }
                continue;
            }
            let ty = body_prop_ty(file, info, p);
            if is_computed_prop(p) {
                // A computed property: a `getX()` accessor (static on the facade), no backing field.
                let fid = lo.ir.add_fun(IrFunction {
                    name: getter_name(&p.name),
                    params: vec![],
                    ret: ty_to_ir(ty),
                    body: None,
                    is_static: true,
                    dispatch_receiver: None,
                    param_checks: vec![],
                });
                lo.computed_props.insert(p.name.clone(), (fid, ty));
            } else {
                let idx = lo.statics.len() as u32;
                lo.statics.insert(p.name.clone(), (idx, ty));
                // A top-level `const val` with a compile-time literal initializer: record its value so a
                // same-file read inlines it (`ldc`), byte-identical to kotlinc, instead of `getstatic`.
                if p.is_const {
                    if let Some(c) = p.init.and_then(|i| ast_literal_const(file, i, ty)) {
                        lo.const_lits.insert(p.name.clone(), c);
                    }
                }
            }
        }
    }

    // Pass 2: lower bodies.
    for &d in &file.decls {
        match file.decl(d) {
            Decl::Fun(f) if f.is_inline => {} // inline functions are expanded at call sites, not emitted
            Decl::Fun(f) => {
                set_bail("deep:fun");
                lo.scope.clear();
                lo.next_value = 0;
                lo.cur_class = None;
                lo.cur_fn_name = f.name.clone();
                lo.cur_fn_suspend = f.is_suspend;
                // `as T` erasure is wired for TOP-LEVEL function type parameters only (the dominant
                // bucket). A class/method/inline type-parameter cast finds no match here and falls
                // through to `ty_ref`, which returns `None` for a bare `T` → the file safely bails
                // (never miscompiles) exactly as before.
                lo.cur_tparams = collect_tparams(
                    file,
                    &f.type_params,
                    &f.type_param_bounds,
                    &f.non_null_type_params,
                );
                lo.lambda_seq = 0;
                let (fid, sig) = if let Some(recv_ref) = &f.receiver {
                    // Extension body: `this` is the receiver (parameter 0), then the declared params.
                    let recv_ty = lo.ext_receiver_ty(file, recv_ref);
                    let recv_desc = recv_ty.descriptor();
                    let fid = lo.ext_fun_ids[&(recv_desc.clone(), f.name.clone())];
                    let this_v = lo.fresh_value();
                    lo.scope.push(("this".to_string(), this_v, recv_ty));
                    (fid, syms.ext_funs.get(&(recv_desc, f.name.clone()))?)
                } else {
                    let sigs = syms.funs.get(&f.name)?;
                    let sig = if sigs.len() == 1 {
                        &sigs[0]
                    } else {
                        let want: String = f
                            .params
                            .iter()
                            .map(|p| ty_of(file, &p.ty).descriptor())
                            .collect();
                        sigs.iter()
                            .find(|s| crate::resolve::erased_params_key(s) == want)?
                    };
                    let fid = *lo
                        .fun_ids
                        .get(&(f.name.clone(), crate::resolve::erased_params_key(sig)))?;
                    (fid, sig)
                };
                let mut param_vals = Vec::new();
                for (p, t) in f.params.iter().zip(&sig.params) {
                    let v = lo.fresh_value();
                    param_vals.push(v);
                    lo.scope.push((p.name.clone(), v, *t));
                }
                // Register parameter defaults for a plain top-level function (no extension receiver, no
                // vararg, ≤31 params) so a transform/plugin can read the lowered default exprs. Lowered
                // with the STATIC value layout — params at values `0..n` (no `this`), the layout these
                // bodies already use. This does NOT emit a `name$default` stub: stub emission runs only on
                // the class path (`emit_default_stub`), never the facade, so a top-level function's codegen
                // is unchanged (top-level calls keep filling omitted args at the call site).
                if f.receiver.is_none()
                    && f.params.iter().any(|p| p.default.is_some())
                    && !f.params.iter().any(|p| p.is_vararg)
                    && f.params.len() <= 31
                {
                    let mut defaults = Vec::new();
                    for (p, t) in f.params.iter().zip(&sig.params) {
                        match p.default {
                            Some(d) => defaults.push(Some(lo.lower_arg(d, &ty_to_ir(*t))?)),
                            None => defaults.push(None),
                        }
                    }
                    lo.ir.fn_param_defaults.insert(fid, defaults);
                    lo.ir
                        .fn_param_names
                        .insert(fid, f.params.iter().map(|p| p.name.clone()).collect());
                }
                let ret_ty = lo.ir.functions[fid as usize].ret.clone();
                // A top-level `tailrec fun` (no extension receiver): rewrite its tail self-calls into a
                // `while(true)` loop (param reassignment + `continue`) so deep recursion doesn't overflow.
                // An EXTENSION/infix `tailrec` (receiver) isn't transformed here — skip the file rather
                // than emit stack-overflowing recursion.
                if f.is_tailrec {
                    if f.receiver.is_some() {
                        return None;
                    }
                    lo.lower_tailrec_body(f, &ret_ty, fid, param_vals, sig.params.clone())?;
                } else {
                    lo.lower_body(&f.body, &ret_ty, fid)?;
                }
            }
            Decl::Class(c) => {
                set_bail("deep:class");
                let internal = class_internal(file, &c.name);
                // A method that overrides a base method with a *different erased signature* (a
                // generic/covariant override) needs a synthetic JVM bridge that krusty doesn't emit
                // yet — bail rather than miscompile (the erased call wouldn't reach the override).
                if let Some(super_int) = lo.classes[&internal].super_internal.clone() {
                    for m in &c.methods {
                        if let Some((_, _, base_fid, _)) = lo.resolve_method(&super_int, &m.name) {
                            // A param/return typed by a class type-param that carries a *class* upper
                            // bound (`class D<T : Foo> : Base<T>() { override fun bar(x: T) }`): kotlinc
                            // erases the override to the bound (`bar(Foo)`) and synthesizes a `bar(Object)`
                            // bridge that `checkcast`s to `Foo` — that cast is observable (it throws CCE on
                            // an out-of-bound arg passed through the erased supertype). krusty erases the
                            // type-param to `Object` instead, so it would emit neither the bound descriptor
                            // nor the casting bridge — a miscompile. Skip until bound-aware erasure exists.
                            let bound_tp = |r: &crate::ast::TypeRef| {
                                r.fun_params.is_empty()
                                    && c.type_param_bounds.iter().any(|(n, _)| *n == r.name)
                            };
                            if m.params.iter().any(|p| bound_tp(&p.ty))
                                || m.ret.as_ref().is_some_and(bound_tp)
                            {
                                return None;
                            }
                            let own_fid = lo.classes[&internal].methods[&m.name].1;
                            let bp = lo.ir.functions[base_fid as usize].params.clone();
                            let br = lo.ir.functions[base_fid as usize].ret.clone();
                            let op = lo.ir.functions[own_fid as usize].params.clone();
                            let or = lo.ir.functions[own_fid as usize].ret.clone();
                            if bp != op || br != or {
                                // Generic/covariant override → synthesize an `ACC_BRIDGE` method with
                                // the supertype's erased descriptor that delegates to the concrete one.
                                let cid = lo.classes[&internal].id;
                                lo.ir.classes[cid as usize].bridges.push(crate::ir::Bridge {
                                    name: m.name.clone(),
                                    erased_params: bp,
                                    erased_ret: br,
                                    concrete_params: op,
                                    concrete_ret: or,
                                    target_name: None,
                                    box_ret: None,
                                    unbox_params: Vec::new(),
                                });
                            }
                        }
                    }
                    // A property redeclared in the subclass (`override val field`) overrides the base's
                    // `getX()`, so external access dispatches virtually to the subclass field — correct.
                    // But a *base-class member that reads the property internally* reads the field
                    // directly (not via `getX`), bypassing the override. Bail only then.
                    let own_fields: Vec<&String> = c
                        .props
                        .iter()
                        .filter(|p| p.is_property)
                        .map(|p| &p.name)
                        .chain(c.body_props.iter().map(|p| &p.name))
                        .collect();
                    let base_name = c.base_class.clone();
                    let base_decl = base_name.as_ref().and_then(|bn| {
                        file.decls.iter().find_map(|&d| match file.decl(d) {
                            Decl::Class(bc) if bc.name == *bn => Some(bc),
                            _ => None,
                        })
                    });
                    for fname in own_fields {
                        if lo.resolve_field(&super_int, fname).is_some() {
                            // A base with its own base, or a base member reading `fname`, risks the
                            // internal-read bypass — bail conservatively; else the override is safe.
                            let unsafe_base = base_decl.map_or(true, |bd| {
                                bd.base_class.is_some()
                                    || bd.methods.iter().any(|m| match &m.body {
                                        FunBody::Expr(e) | FunBody::Block(e) => {
                                            crate::resolve::expr_uses_name_pub(file, *e, fname)
                                        }
                                        FunBody::None => false,
                                    })
                                    || bd.body_props.iter().any(|p| {
                                        p.init.map_or(false, |e| {
                                            crate::resolve::expr_uses_name_pub(file, e, fname)
                                        })
                                    })
                            });
                            if unsafe_base {
                                return None;
                            }
                        }
                    }
                }
                // Property getter bridges: a property overriding a supertype property with a different
                // erased type (a covariant override `from: Sub` over `from: Super`, or a generic
                // `val x: T` erased to `Object` overridden with a concrete type) needs a synthetic
                // `getX()` returning the supertype's (erased) type that delegates to the concrete getter —
                // else a call through the supertype reference resolves to the missing erased getter.
                if !c.is_interface() {
                    let cid = lo.classes[&internal].id;
                    for sup in lo.syms.supertype_internals(&internal) {
                        let Some(sc) = lo.syms.class_by_internal(&sup) else {
                            continue;
                        };
                        for (pname, sty, _) in sc.props.clone() {
                            if let Some((own_ty, _)) = lo.syms.prop_of(&internal, &pname) {
                                if sty.descriptor() != own_ty.descriptor() {
                                    let gname = getter_name(&pname);
                                    let already = lo.ir.classes[cid as usize]
                                        .bridges
                                        .iter()
                                        .any(|b| b.name == gname && b.erased_params.is_empty());
                                    if !already {
                                        lo.ir.classes[cid as usize].bridges.push(
                                            crate::ir::Bridge {
                                                name: gname,
                                                erased_params: vec![],
                                                erased_ret: ty_to_ir(sty),
                                                concrete_params: vec![],
                                                concrete_ret: ty_to_ir(own_ty),
                                                target_name: None,
                                                box_ret: None,
                                                unbox_params: Vec::new(),
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                // Interface bridges: for each implemented-interface method, if the class's actual
                // implementation (declared or inherited) has a different erased signature than the
                // interface's, add a bridge with the interface's descriptor delegating to the impl.
                if !c.is_interface() {
                    let cid = lo.classes[&internal].id;
                    let ifaces = lo.ir.classes[cid as usize].interfaces.clone();
                    let mut seen: std::collections::HashSet<String> = lo.ir.classes[cid as usize]
                        .bridges
                        .iter()
                        .map(|b| format!("{}{:?}{:?}", b.name, b.erased_params, b.erased_ret))
                        .collect();
                    for itf in &ifaces {
                        for (mname, ifid) in lo.collect_iface_methods(itf) {
                            if let Some((_, _, impl_fid, _)) = lo.resolve_method(&internal, &mname)
                            {
                                let ip = lo.ir.functions[ifid as usize].params.clone();
                                let ir_ = lo.ir.functions[ifid as usize].ret.clone();
                                let cp = lo.ir.functions[impl_fid as usize].params.clone();
                                let cr = lo.ir.functions[impl_fid as usize].ret.clone();
                                if (ip != cp || ir_ != cr)
                                    && seen.insert(format!("{}{:?}{:?}", mname, ip, ir_))
                                {
                                    lo.ir.classes[cid as usize].bridges.push(crate::ir::Bridge {
                                        name: mname,
                                        erased_params: ip,
                                        erased_ret: ir_,
                                        concrete_params: cp,
                                        concrete_ret: cr,
                                        target_name: None,
                                        box_ret: None,
                                        unbox_params: Vec::new(),
                                    });
                                }
                            }
                        }
                        // A *classpath* generic interface (`Comparable<Foo>`, `Iterable<E>`, …) isn't in
                        // `self.classes`, so its erased single-abstract-method comes from the library set.
                        // When the class's override has a specialized descriptor (`compareTo(Foo)` vs the
                        // interface's erased `compareTo(Object)`), emit the `ACC_BRIDGE` the JVM needs to
                        // dispatch an interface-typed call — without it `(x as Comparable).compareTo(y)`
                        // hits `AbstractMethodError` instead of running the override (or throwing CCE).
                        if !lo.classes.contains_key(itf) {
                            if let Some(m) = lo.syms.libraries.sam_method(itf) {
                                if let Some((_, _, impl_fid, _)) =
                                    lo.resolve_method(&internal, &m.name)
                                {
                                    let ip: Vec<Ty> =
                                        m.params.iter().map(|t| ty_to_ir(*t)).collect();
                                    let ir_ = ty_to_ir(m.ret);
                                    let cp = lo.ir.functions[impl_fid as usize].params.clone();
                                    let cr = lo.ir.functions[impl_fid as usize].ret.clone();
                                    if (ip != cp || ir_ != cr)
                                        && seen.insert(format!("{}{:?}{:?}", m.name, ip, ir_))
                                    {
                                        lo.ir.classes[cid as usize].bridges.push(
                                            crate::ir::Bridge {
                                                name: m.name.clone(),
                                                erased_params: ip,
                                                erased_ret: ir_,
                                                concrete_params: cp,
                                                concrete_ret: cr,
                                                target_name: None,
                                                box_ret: None,
                                                unbox_params: Vec::new(),
                                            },
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                // An interface's abstract methods have no body; its DEFAULT methods (with a body) are
                // lowered like instance methods (fall through to the normal method-body loop below).
                if c.is_interface() && c.methods.iter().all(|m| matches!(m.body, FunBody::None)) {
                    continue;
                }
                for m in &c.methods {
                    // An abstract method has no body — leave its `IrFunction.body` as `None`.
                    if matches!(m.body, FunBody::None) {
                        continue;
                    }
                    // A `tailrec` MEMBER method isn't loop-transformed (only top-level functions are) —
                    // skip the file rather than emit stack-overflowing recursion.
                    if m.is_tailrec {
                        return None;
                    }
                    let (_, fid, _) = lo.classes[&internal].methods[&m.name];
                    lo.scope.clear();
                    lo.next_value = 0;
                    lo.cur_class = Some(internal.clone());
                    lo.cur_fn_name = m.name.clone();
                    lo.cur_fn_suspend = m.is_suspend;
                    lo.lambda_seq = 0;
                    // `this` is value 0.
                    let this_v = lo.fresh_value();
                    lo.scope
                        .push(("this".to_string(), this_v, Ty::obj(&internal)));
                    let sig = syms.classes.get(&c.name)?.methods.get(&m.name)?.clone();
                    for (p, t) in m.params.iter().zip(&sig.params) {
                        let v = lo.fresh_value();
                        lo.scope.push((p.name.clone(), v, *t));
                    }
                    // Register parameter defaults (the JVM backend realizes them via the `$default`
                    // stub). Lowered with `this` = value 0 and the params = values 1..=n — the stub's
                    // value layout. `None` for a required parameter. Gated identically to the pass-1
                    // marker (no interface defaults, ≤31 parameters).
                    if m.params.iter().any(|p| p.default.is_some())
                        && !c.is_interface()
                        && m.params.len() <= 31
                    {
                        let mut defaults = Vec::new();
                        for (p, t) in m.params.iter().zip(&sig.params) {
                            match p.default {
                                Some(d) => defaults.push(Some(lo.lower_arg(d, &ty_to_ir(*t))?)),
                                None => defaults.push(None),
                            }
                        }
                        lo.ir.fn_param_defaults.insert(fid, defaults);
                    }
                    let ret_ty = lo.ir.functions[fid as usize].ret.clone();
                    lo.lower_body(&m.body, &ret_ty, fid)?;
                }
                // Computed body-property getter bodies → `getX()` methods on the class.
                for p in c.body_props.iter().filter(|p| is_computed_prop(p)) {
                    let gname = getter_name(&p.name);
                    let (_, fid, _) = lo.classes[&internal].methods[&gname];
                    lo.scope.clear();
                    lo.next_value = 0;
                    lo.cur_class = Some(internal.clone());
                    lo.cur_fn_name = gname;
                    lo.lambda_seq = 0;
                    let this_v = lo.fresh_value();
                    lo.scope
                        .push(("this".to_string(), this_v, Ty::obj(&internal)));
                    let ret_ty = lo.ir.functions[fid as usize].ret.clone();
                    let body = p.getter.clone().unwrap();
                    lo.lower_body(&body, &ret_ty, fid)?;
                }
                // Field-backed custom accessors (`val x = init get() = field…`): overwrite the default
                // `getX`/`setX` body (built in pass 1) with the lowered custom accessor, binding the
                // `field` keyword to the property's backing field via `cur_field`.
                for p in c.body_props.iter().filter(|p| is_field_accessor_prop(p)) {
                    let class_id = lo.classes[&internal].id;
                    let fidx = lo.ir.classes[class_id as usize]
                        .fields
                        .iter()
                        .position(|f| f.name == p.name)? as u32;
                    let fty_ir = lo.ir.classes[class_id as usize].fields[fidx as usize]
                        .ty
                        .clone();
                    if let Some(getter) = p.getter.clone() {
                        let gname = getter_name(&p.name);
                        let (_, fid, _) = lo.classes[&internal].methods[&gname];
                        lo.scope.clear();
                        lo.next_value = 0;
                        lo.cur_class = Some(internal.clone());
                        lo.cur_field = Some((class_id, fidx, fty_ir.clone()));
                        lo.cur_fn_name = gname;
                        lo.lambda_seq = 0;
                        let this_v = lo.fresh_value();
                        lo.scope
                            .push(("this".to_string(), this_v, Ty::obj(&internal)));
                        let ret_ty = lo.ir.functions[fid as usize].ret.clone();
                        lo.lower_body(&getter, &ret_ty, fid)?;
                        lo.cur_field = None;
                    }
                    if p.is_var {
                        if let Some(setter) =
                            p.setter.as_ref().filter(|s| s.body.is_some()).cloned()
                        {
                            let sname = setter_name(&p.name);
                            let (_, fid, _) = lo.classes[&internal].methods[&sname];
                            let pty = body_prop_ty(file, info, p);
                            lo.scope.clear();
                            lo.next_value = 0;
                            lo.cur_class = Some(internal.clone());
                            lo.cur_field = Some((class_id, fidx, fty_ir.clone()));
                            lo.cur_fn_name = sname;
                            lo.lambda_seq = 0;
                            let this_v = lo.fresh_value();
                            lo.scope
                                .push(("this".to_string(), this_v, Ty::obj(&internal)));
                            let v_v = lo.fresh_value();
                            lo.scope.push((
                                setter.param.clone().unwrap_or_else(|| "value".to_string()),
                                v_v,
                                pty,
                            ));
                            let sbody = setter.body.clone().unwrap();
                            lo.lower_body(&sbody, &Ty::Unit, fid)?;
                            lo.cur_field = None;
                        }
                    }
                }
                // Delegated body-property accessors → `getX()`/`setX()` calling the delegate's
                // getValue/setValue, with the `KProperty` passed inline as a `PropertyReference1Impl`.
                for p in c.body_props.iter().filter(|p| p.delegate.is_some()) {
                    let class_id = lo.classes[&internal].id;
                    let delegate_ty = lo.info.ty(p.delegate.unwrap());
                    let delegate_internal = delegate_ty.obj_internal()?.to_string();
                    let fname = format!("{}$delegate", p.name);
                    let field_idx = lo.ir.classes[class_id as usize]
                        .fields
                        .iter()
                        .position(|f| f.name == fname)
                        .expect("delegate field") as u32;
                    // Build a fresh `PropertyReference1Impl(A::class, "x", "getX()<ret>", 0)`.
                    let gname = getter_name(&p.name);
                    let (_, get_fid, prop_ty) = lo.classes[&internal].methods[&gname];
                    let ret_desc = prop_ty.descriptor();
                    let make_propref = |lo: &mut Lower| {
                        let cls = lo.ir.add_expr(IrExpr::ClassConst {
                            internal: internal.clone(),
                        });
                        let nm = lo
                            .ir
                            .add_expr(IrExpr::Const(IrConst::String(p.name.clone())));
                        let sigc = lo.ir.add_expr(IrExpr::Const(IrConst::String(format!(
                            "{}(){}",
                            gname, ret_desc
                        ))));
                        let flag = lo.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                        lo.ir.add_expr(IrExpr::NewExternal {
                            internal: "kotlin/jvm/internal/PropertyReference1Impl".to_string(),
                            ctor_desc: "(Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V"
                                .to_string(),
                            args: vec![cls, nm, sigc, flag],
                        })
                    };
                    // getX(): return this.x$delegate.getValue(this, propref)
                    let gv = lo.syms.method_of(&delegate_internal, "getValue")?;
                    let gv_desc = {
                        let mut s = String::from("(");
                        for pt in &gv.params {
                            s.push_str(&pt.descriptor());
                        }
                        s.push(')');
                        s.push_str(&gv.ret.descriptor());
                        s
                    };
                    let this_e = lo.ir.add_expr(IrExpr::GetValue(0));
                    let dele = lo.ir.add_expr(IrExpr::GetField {
                        receiver: this_e,
                        class: class_id,
                        index: field_idx,
                    });
                    let this_arg = lo.ir.add_expr(IrExpr::GetValue(0));
                    let pref = make_propref(&mut lo);
                    let call = lo.ir.add_expr(IrExpr::Call {
                        callee: crate::ir::Callee::Virtual {
                            owner: delegate_internal.clone(),
                            name: "getValue".to_string(),
                            descriptor: gv_desc,
                            interface: false,
                        },
                        dispatch_receiver: Some(dele),
                        args: vec![this_arg, pref],
                    });
                    // A generic delegate's `getValue` returns the erased `Object`; coerce to the property
                    // type (`checkcast`/unbox), exactly as kotlinc does.
                    let coerced = lo.coerce_erased(call, prop_ty, gv.ret);
                    let ret = lo.ir.add_expr(IrExpr::Return(Some(coerced)));
                    let body = lo.ir.add_expr(IrExpr::Block {
                        stmts: vec![ret],
                        value: None,
                    });
                    lo.ir.functions[get_fid as usize].body = Some(body);
                    // setX(value): this.x$delegate.setValue(this, propref, value)
                    if p.is_var {
                        let sname = setter_name(&p.name);
                        let (_, set_fid, _) = lo.classes[&internal].methods[&sname];
                        let sv = lo.syms.method_of(&delegate_internal, "setValue")?;
                        let sv_desc = {
                            let mut s = String::from("(");
                            for pt in &sv.params {
                                s.push_str(&pt.descriptor());
                            }
                            s.push(')');
                            s.push_str(&sv.ret.descriptor());
                            s
                        };
                        let this_e = lo.ir.add_expr(IrExpr::GetValue(0));
                        let dele = lo.ir.add_expr(IrExpr::GetField {
                            receiver: this_e,
                            class: class_id,
                            index: field_idx,
                        });
                        let this_arg = lo.ir.add_expr(IrExpr::GetValue(0));
                        let pref = make_propref(&mut lo);
                        let value_arg = lo.ir.add_expr(IrExpr::GetValue(1));
                        // A generic delegate's `setValue` takes the ERASED value param (`<T> setValue(…, i:
                        // T)`); a PRIMITIVE property value boxes into it (`Integer.valueOf`), exactly as
                        // kotlinc does. A reference value passes through.
                        let value_arg = match sv.params.last() {
                            Some(vp) if vp.is_reference() && prop_ty.is_primitive() => {
                                lo.ir.add_expr(IrExpr::TypeOp {
                                    op: IrTypeOp::ImplicitCoercion,
                                    arg: value_arg,
                                    type_operand: ty_to_ir(*vp),
                                })
                            }
                            _ => value_arg,
                        };
                        let call = lo.ir.add_expr(IrExpr::Call {
                            callee: crate::ir::Callee::Virtual {
                                owner: delegate_internal.clone(),
                                name: "setValue".to_string(),
                                descriptor: sv_desc,
                                interface: false,
                            },
                            dispatch_receiver: Some(dele),
                            args: vec![this_arg, pref, value_arg],
                        });
                        let body = lo.ir.add_expr(IrExpr::Block {
                            stmts: vec![call],
                            value: None,
                        });
                        lo.ir.functions[set_fid as usize].body = Some(body);
                    }
                }
                // Companion method bodies — lowered on the synthesized `C$Companion` class.
                if let Some(comp_fq) = lo.companions.get(&internal).cloned() {
                    for m in &c.companion_methods {
                        if matches!(m.body, FunBody::None) {
                            continue;
                        }
                        let (_, fid, _) = lo.classes[&comp_fq].methods[&m.name];
                        lo.scope.clear();
                        lo.next_value = 0;
                        lo.cur_class = Some(comp_fq.clone());
                        lo.cur_fn_name = m.name.clone();
                        lo.lambda_seq = 0;
                        let this_v = lo.fresh_value();
                        lo.scope
                            .push(("this".to_string(), this_v, Ty::obj(&comp_fq)));
                        let sig = syms.classes.get(&c.name)?.static_methods.get(&m.name)?;
                        for (p, t) in m.params.iter().zip(&sig.params) {
                            let v = lo.fresh_value();
                            lo.scope.push((p.name.clone(), v, *t));
                        }
                        let ret_ty = lo.ir.functions[fid as usize].ret.clone();
                        lo.lower_body(&m.body, &ret_ty, fid)?;
                    }
                }
                // A superclass whose constructor needs more arguments than are supplied (`object : A()`
                // where `A(val x = …)` has defaulted parameters). krusty fills the base's defaults into the
                // `super(…)` call when NO explicit base args are written and EVERY base param has a default
                // (the common `: A()` shape) — handled by `super_default_fill` below. Any other arity
                // mismatch (some explicit + some defaulted, or a required param missing) still bails.
                let mut super_default_fill = false;
                if let Some(s) = lo.classes[&internal].super_internal.clone() {
                    let sup_params = lo
                        .classes
                        .get(&s)
                        .map(|sup| lo.ir.classes[sup.id as usize].ctor_param_count as usize)
                        .unwrap_or(0);
                    // For a class with NO primary constructor the base arguments are supplied by each
                    // secondary ctor's `super(…)` (validated in the secondary-ctor lowering), not by a
                    // supertype-list `: Base(args)` — so this `base_args` arity check doesn't apply.
                    if sup_params > c.base_args.len() && c.has_primary_ctor {
                        // `: A()` (no explicit args) where every base param has a default (read from the
                        // base's resolve `ClassSig`) → fill them.
                        let defaults_ok = c.base_args.is_empty()
                            && lo.syms.class_by_internal(&s).is_some_and(|cs| {
                                cs.ctor_defaults.len() >= sup_params
                                    && cs.ctor_defaults[..sup_params].iter().all(|d| d.is_some())
                            });
                        if defaults_ok {
                            super_default_fill = true;
                        } else {
                            return None;
                        }
                    }
                    // An anonymous object extending a *parameterized* base class can reference the
                    // enclosing instance's (private) members, which Kotlin binds by capture — not by
                    // inheritance (a base's private field is invisible to a subclass). krusty has no
                    // outer-instance capture, so it would resolve such a name to the inherited field
                    // and miscompile (KT-3684). Bail those; SAM-style anon objects over interfaces or
                    // no-argument classes are unaffected.
                    if internal.contains("$anon$") && sup_params > 0 {
                        return None;
                    }
                }
                // `: A()` where every base param has a default — fill the base ctor's default-value exprs
                // into `super(…)` (the same defaults a `new A()` construction fills at the call site, since
                // krusty has no synthetic `$default` ctor). The defaults are evaluated with only `this` in
                // scope (no explicit args reference the subclass params); a default referencing a base param
                // would fail to resolve and bail via `lower_arg`'s `?`.
                if super_default_fill {
                    let class_id = lo.classes[&internal].id;
                    let sup_internal = lo.classes[&internal].super_internal.clone()?;
                    let sup_id = lo.classes.get(&sup_internal).map(|s| s.id)?;
                    let super_field_tys: Vec<Ty> = {
                        let sup = &lo.ir.classes[sup_id as usize];
                        let n = sup.ctor_param_count as usize;
                        if sup.ctor_args.is_empty() {
                            sup.fields[..n].iter().map(|f| f.ty).collect()
                        } else {
                            sup.ctor_args[..n].iter().map(|(t, _)| *t).collect()
                        }
                    };
                    let defaults: Vec<AstExprId> =
                        lo.syms.class_by_internal(&sup_internal)?.ctor_defaults
                            [..super_field_tys.len()]
                            .iter()
                            .map(|d| d.expect("super_default_fill checked all Some"))
                            .collect();
                    lo.scope.clear();
                    lo.next_value = 0;
                    lo.cur_class = Some(internal.clone());
                    let this_v = lo.fresh_value();
                    lo.scope
                        .push(("this".to_string(), this_v, Ty::obj(&internal)));
                    let mut sargs = Vec::new();
                    for (d, ft) in defaults.iter().zip(&super_field_tys) {
                        sargs.push(lo.lower_arg(*d, ft)?);
                    }
                    lo.ir.classes[class_id as usize].super_args = sargs;
                }
                // Base-class constructor arguments (`: A(args)`), evaluated with the primary-ctor
                // params in scope (`this`=0, params 1..N), coerced to the super's parameter types.
                if !c.base_args.is_empty() {
                    let class_id = lo.classes[&internal].id;
                    lo.scope.clear();
                    lo.next_value = 0;
                    lo.cur_class = Some(internal.clone());
                    let this_v = lo.fresh_value();
                    lo.scope
                        .push(("this".to_string(), this_v, Ty::obj(&internal)));
                    // ALL ctor params (property and plain) are in scope as values `1..=M` in declaration
                    // order — a plain parameter is an argument the initializer / `super(…)` can read.
                    for p in c.props.iter() {
                        let v = lo.fresh_value();
                        lo.scope.push((p.name.clone(), v, ty_of(file, &p.ty)));
                    }
                    let super_field_tys: Vec<Ty> = lo.classes[&internal]
                        .super_internal
                        .clone()
                        .and_then(|s| lo.classes.get(&s).map(|sup| sup.id))
                        .map(|sid| {
                            let sup = &lo.ir.classes[sid as usize];
                            if sup.ctor_args.is_empty() {
                                let n = sup.ctor_param_count as usize;
                                sup.fields[..n].iter().map(|f| f.ty.clone()).collect()
                            } else {
                                sup.ctor_args.iter().map(|(t, _)| t.clone()).collect()
                            }
                        })
                        .unwrap_or_default();
                    // The `super(…)` call must match the base's PRIMARY constructor exactly: same arity,
                    // and each argument assignable to the corresponding param (no narrowing/erasure). A
                    // mismatch means the call actually targets a base SECONDARY constructor (with
                    // defaults) — which krusty doesn't resolve — so bail rather than emit a `<init>` call
                    // whose stack shape won't verify. A branchy super argument (`Base("O" + if(…))`) emits
                    // merge frames in the pre-`super()` region the flat ctor emitter can't reconcile — bail.
                    if c.base_args.len() != super_field_tys.len()
                        || c.base_args.iter().any(|&a| body_contains_branch(file, a))
                        || c.base_args.iter().zip(&super_field_tys).any(|(&a, ft)| {
                            let at = info.ty(a);
                            // Exact IR-type match is fine; a reference arg into a reference param (erased
                            // generic / `Any`) is fine; anything else (e.g. an `Int` arg into a `String`
                            // param — the call really targets a secondary ctor) is a mis-target → bail.
                            &ty_to_ir(at) != ft
                                && !(at.is_reference() && ir_type_is_reference(ft))
                                && at != Ty::Error
                        })
                    {
                        return None;
                    }
                    let mut sargs = Vec::new();
                    for (a, ft) in c.base_args.iter().zip(&super_field_tys) {
                        sargs.push(lo.lower_arg(*a, ft)?);
                    }
                    lo.ir.classes[class_id as usize].super_args = sargs;
                }
                // Constructor body: run body-property initializers and `init { … }` blocks in source
                // order, with `this` = value 0 and the constructor params as values 1..=N. For a class
                // with NO primary constructor the init steps run inside each `super(…)`-reaching secondary
                // constructor instead (lowered there, in that ctor's own value space) — skip here.
                let has_delegated = c.body_props.iter().any(|p| p.delegate.is_some());
                // Interface delegation `: I by d` whose `d` is a NON-`val` param needs a `$$delegate_<i>`
                // field store in the ctor.
                let has_iface_synth_delegate = c
                    .delegations
                    .iter()
                    .any(|(_, d)| !c.props.iter().any(|p| p.is_property && &p.name == d))
                    || !c.delegation_exprs.is_empty();
                // Run whenever there is ANY ctor body to lower OR any primary-constructor `val`/`var`
                // param (or inner `this$0`) to store: the param→field stores are desugared explicitly
                // into `init_body` here (no implicit auto-store in the backend).
                if (!c.init_order.is_empty()
                    || has_delegated
                    || has_iface_synth_delegate
                    || c.inner_of.is_some()
                    || c.props.iter().any(|p| p.is_property))
                    && c.has_primary_ctor
                {
                    let class_id = lo.classes[&internal].id;
                    let ctor_count = lo.ir.classes[class_id as usize].ctor_param_count;
                    let _ = ctor_count;
                    lo.scope.clear();
                    lo.next_value = 0;
                    lo.cur_class = Some(internal.clone());
                    let this_v = lo.fresh_value();
                    lo.scope
                        .push(("this".to_string(), this_v, Ty::obj(&internal)));
                    // An inner class's synthetic `this$0` is the first constructor parameter (value 1).
                    if let Some(outer) = &c.inner_of {
                        let v = lo.fresh_value();
                        lo.scope.push((
                            "this$0".to_string(),
                            v,
                            Ty::obj(&class_internal(file, outer)),
                        ));
                    }
                    // ALL ctor params (property and plain) in scope as values, declaration order.
                    for p in c.props.iter() {
                        let v = lo.fresh_value();
                        lo.scope.push((p.name.clone(), v, ty_of(file, &p.ty)));
                    }
                    let mut stmts = Vec::new();
                    // Desugar the primary-constructor `val`/`var` sugar: store the inner `this$0` and each
                    // property param to its backing field, right after `super(…)` and before the body
                    // initializers/`init` blocks — exactly what kotlinc emits. This used to be an implicit
                    // auto-store in `ir_emit`; carrying it as explicit `SetField`s keeps the IR sugar-free.
                    //
                    // A `@JvmInline value class` is EXCLUDED: its field store belongs only in the boxed
                    // `<init>`, but the IR ctor body is ALSO reused for the UNBOXED `constructor-impl`
                    // (where `this` is the bare underlying value — a `putfield` through it is invalid). The
                    // backend's auto-store places the store in `<init>` only, so leave that path for them.
                    if !c.is_value {
                        let mut targets: Vec<(String, u32)> = Vec::new();
                        let mut field_i = 0u32;
                        if c.inner_of.is_some() {
                            targets.push(("this$0".to_string(), field_i));
                            field_i += 1;
                        }
                        for p in c.props.iter().filter(|p| p.is_property) {
                            targets.push((p.name.clone(), field_i));
                            field_i += 1;
                        }
                        for (name, idx) in targets {
                            let (pv, _) = lo.lookup(&name)?;
                            let recv = lo.ir.add_expr(IrExpr::GetValue(this_v));
                            let val = lo.ir.add_expr(IrExpr::GetValue(pv));
                            stmts.push(lo.ir.add_expr(IrExpr::SetField {
                                receiver: recv,
                                class: class_id,
                                index: idx,
                                value: val,
                            }));
                        }
                        // These stores ARE the param→field stores — backend must not auto-store too.
                        lo.ir.classes[class_id as usize].explicit_param_stores = true;
                    }
                    // Interface-delegation `$$delegate_<i>` stores (`this.$$delegate_i = <delegate param>`),
                    // first in the ctor body (kotlinc stores them right after `super()`).
                    for (di, (_iface, dname)) in c.delegations.iter().enumerate() {
                        if c.props.iter().any(|p| p.is_property && &p.name == dname) {
                            continue; // a `val`-param delegate has its own auto-stored field
                        }
                        let synth = format!("$$delegate_{di}");
                        let fidx = lo.ir.classes[class_id as usize]
                            .fields
                            .iter()
                            .position(|f| f.name == synth)?
                            as u32;
                        let (pv, _) = lo.lookup(dname)?;
                        let this_e = lo.ir.add_expr(IrExpr::GetValue(this_v));
                        let val_e = lo.ir.add_expr(IrExpr::GetValue(pv));
                        let sf = lo.ir.add_expr(IrExpr::SetField {
                            receiver: this_e,
                            class: class_id,
                            index: fidx,
                            value: val_e,
                        });
                        stmts.push(sf);
                    }
                    // Expression delegates (`: I by Impl()`): evaluate the expression once into its
                    // `$$delegate_e<j>` field.
                    for (j, (_iface, e)) in c.delegation_exprs.iter().enumerate() {
                        let synth = format!("$$delegate_e{j}");
                        let fidx = lo.ir.classes[class_id as usize]
                            .fields
                            .iter()
                            .position(|f| f.name == synth)?
                            as u32;
                        let fty = lo.ir.classes[class_id as usize].fields[fidx as usize]
                            .ty
                            .clone();
                        let val_e = lo.lower_arg(*e, &fty)?;
                        let this_e = lo.ir.add_expr(IrExpr::GetValue(this_v));
                        let sf = lo.ir.add_expr(IrExpr::SetField {
                            receiver: this_e,
                            class: class_id,
                            index: fidx,
                            value: val_e,
                        });
                        stmts.push(sf);
                    }
                    for step in &c.init_order {
                        match step {
                            ast::ClassInit::PropInit(i) => {
                                // A computed or abstract body property has no backing field here —
                                // nothing to initialize. A deferred `val` (`val a: Int`, no initializer)
                                // is assigned later in an `init` block, not at its declaration — skip it
                                // here too (it has no init expression).
                                if !is_backing_field_prop(&c.body_props[*i as usize])
                                    || c.body_props[*i as usize].init.is_none()
                                {
                                    continue;
                                }
                                // Computed body properties are not fields, so the field index counts
                                // only the non-computed body properties before this one.
                                let body_offset = c.body_props[..*i as usize]
                                    .iter()
                                    .filter(|p| is_backing_field_prop(p))
                                    .count();
                                let field_idx = ctor_count + body_offset as u32;
                                let field_ty = lo.ir.classes[class_id as usize].fields
                                    [field_idx as usize]
                                    .ty
                                    .clone();
                                let init_e = c.body_props[*i].init.unwrap();
                                // A branchy body-property initializer (`val k = when { … }`) emits
                                // merge-point frames in the constructor's init context that the flat
                                // emitter doesn't reconcile yet — bail rather than miscompile.
                                if matches!(
                                    lo.afile.expr(init_e),
                                    Expr::When { .. }
                                        | Expr::If { .. }
                                        | Expr::Elvis { .. }
                                        | Expr::Block { .. }
                                        | Expr::Try { .. }
                                ) {
                                    return None;
                                }
                                let val = lo.lower_arg(init_e, &field_ty)?;
                                let recv = lo.ir.add_expr(IrExpr::GetValue(this_v));
                                stmts.push(lo.ir.add_expr(IrExpr::SetField {
                                    receiver: recv,
                                    class: class_id,
                                    index: field_idx,
                                    value: val,
                                }));
                            }
                            ast::ClassInit::Block(e) => {
                                // An `init { … }` block: lower its statements for effect.
                                let Expr::Block {
                                    stmts: bs,
                                    trailing,
                                } = lo.afile.expr(*e).clone()
                                else {
                                    return None;
                                };
                                // A branchy VALUE assignment in `init` (`a = if(…) … else …`) emits merge
                                // frames in the constructor the flat emitter doesn't reconcile (a
                                // verify/runtime error) — bail. A branchy *statement* (`if (c) { … }`) is
                                // fine. (Pre-existing limitation, surfaced by deferred-`val` init.)
                                for &s in &bs {
                                    let branchy = match lo.afile.stmt(s) {
                                        Stmt::Assign { value, .. }
                                        | Stmt::AssignMember { value, .. } => {
                                            body_contains_branch(lo.afile, *value)
                                        }
                                        Stmt::Local { init, .. } => {
                                            body_contains_branch(lo.afile, *init)
                                        }
                                        _ => false,
                                    };
                                    if branchy {
                                        return None;
                                    }
                                }
                                for s in bs {
                                    stmts.push(lo.stmt(s)?);
                                }
                                if let Some(t) = trailing {
                                    stmts.push(lo.expr(t)?);
                                }
                            }
                        }
                    }
                    // Initialize each delegated property's `x$delegate` field: `this.x$delegate = Del()`.
                    for p in c.body_props.iter().filter(|p| p.delegate.is_some()) {
                        let fname = format!("{}$delegate", p.name);
                        let field_idx = lo.ir.classes[class_id as usize]
                            .fields
                            .iter()
                            .position(|f| f.name == fname)
                            .expect("delegate field registered in pass 1")
                            as u32;
                        let field_ty = lo.ir.classes[class_id as usize].fields[field_idx as usize]
                            .ty
                            .clone();
                        let val = lo.lower_arg(p.delegate.unwrap(), &field_ty)?;
                        let recv = lo.ir.add_expr(IrExpr::GetValue(this_v));
                        stmts.push(lo.ir.add_expr(IrExpr::SetField {
                            receiver: recv,
                            class: class_id,
                            index: field_idx,
                            value: val,
                        }));
                    }
                    let body = lo.ir.add_expr(IrExpr::Block { stmts, value: None });
                    lo.ir.classes[class_id as usize].init_body = Some(body);
                }
                // Lower each secondary constructor to an extra `<init>(p)`. For a class WITH a primary
                // ctor every secondary delegates to it via `this(…)`. For a class with NO primary ctor a
                // secondary delegates either to a sibling (`this(…)`) or to `super(…)` (or implicitly);
                // a `super(…)`-reaching ctor also runs the field initializers + `init {}` blocks. For a
                // value class the JVM value-class pass turns these lowered `IrSecondaryCtor`s into static
                // `constructor-impl` overloads.
                if !c.secondary_ctors.is_empty() {
                    use crate::ir::CtorDelegateTarget;
                    let class_id = lo.classes[&internal].id;
                    let primary_param_tys: Vec<Ty> = {
                        let n = lo.ir.classes[class_id as usize].ctor_param_count as usize;
                        lo.ir.classes[class_id as usize].fields[..n]
                            .iter()
                            .map(|f| f.ty.clone())
                            .collect()
                    };
                    // The IR param types of every secondary ctor (for resolving a sibling `this(…)`).
                    let sec_param_tys: Vec<Vec<Ty>> = c
                        .secondary_ctors
                        .iter()
                        .map(|sc| {
                            sc.params
                                .iter()
                                .map(|p| ty_to_ir(ty_of(file, &p.ty)))
                                .collect()
                        })
                        .collect();
                    let super_param_tys = lo.super_ctor_param_tys(&internal);
                    let mut secs = Vec::new();
                    for sc in &c.secondary_ctors {
                        lo.scope.clear();
                        lo.next_value = 0;
                        lo.cur_class = Some(internal.clone());
                        let this_v = lo.fresh_value();
                        lo.scope
                            .push(("this".to_string(), this_v, Ty::obj(&internal)));
                        let mut param_irs = Vec::new();
                        // A secondary ctor with a defaulted parameter needs kotlinc's synthetic
                        // `<init>(…, int mask, DefaultConstructorMarker)` overload, which krusty doesn't
                        // emit — a call omitting the default would hit a missing `<init>`. Bail.
                        if sc.params.iter().any(|p| p.default.is_some()) {
                            return None;
                        }
                        for p in &sc.params {
                            let pty = ty_of(file, &p.ty);
                            let v = lo.fresh_value();
                            lo.scope.push((p.name.clone(), v, pty));
                            param_irs.push(ty_to_ir(pty));
                        }
                        // Resolve the delegation target + the parameter types its args must coerce to.
                        let (delegate, delegate_args, target_tys, run_init): (
                            CtorDelegateTarget,
                            Vec<AstExprId>,
                            Vec<Ty>,
                            bool,
                        ) = match &sc.delegation {
                            ast::CtorDelegation::This(args) => {
                                let target = if c.has_primary_ctor {
                                    primary_param_tys.clone()
                                } else {
                                    // Pick the sibling secondary ctor this `this(…)` targets: prefer the
                                    // one whose parameter types accept the arguments, else the unique
                                    // same-arity ctor. (Ambiguity type-matching can't resolve bails.)
                                    let arg_irs: Vec<Ty> =
                                        args.iter().map(|a| ty_to_ir(lo.info.ty(*a))).collect();
                                    let typed = sec_param_tys.iter().find(|p| {
                                        p.len() == arg_irs.len()
                                            && arg_irs
                                                .iter()
                                                .zip(p.iter())
                                                .all(|(a, pp)| ir_arg_assignable(a, pp))
                                    });
                                    match typed {
                                        Some(p) => p.clone(),
                                        None => {
                                            let same: Vec<&Vec<Ty>> = sec_param_tys
                                                .iter()
                                                .filter(|p| p.len() == args.len())
                                                .collect();
                                            if same.len() != 1 {
                                                return None;
                                            }
                                            same[0].clone()
                                        }
                                    }
                                };
                                (
                                    CtorDelegateTarget::This {
                                        target_params: target.clone(),
                                    },
                                    args.clone(),
                                    target,
                                    false,
                                )
                            }
                            ast::CtorDelegation::Super(args) => {
                                if c.has_primary_ctor {
                                    return None; // a secondary in a primary class must delegate via this(…)
                                }
                                (
                                    CtorDelegateTarget::Super,
                                    args.clone(),
                                    super_param_tys.clone(),
                                    true,
                                )
                            }
                            ast::CtorDelegation::None => {
                                if c.has_primary_ctor {
                                    return None;
                                }
                                // Implicit `super()` — must be a no-arg base (Object, or a base ctor we
                                // can't pass args to here). Bail if the base needs constructor arguments.
                                if !super_param_tys.is_empty() {
                                    return None;
                                }
                                (CtorDelegateTarget::Super, vec![], vec![], true)
                            }
                        };
                        if delegate_args.len() != target_tys.len() {
                            return None;
                        }
                        let mut dargs = Vec::new();
                        for (a, ft) in delegate_args.iter().zip(&target_tys) {
                            dargs.push(lo.lower_arg(*a, ft)?);
                        }
                        // A `super(…)`-reaching ctor runs the init steps (field initializers + `init {}`)
                        // before its own body; a `this(…)` ctor runs only its body.
                        let mut out = Vec::new();
                        if run_init {
                            out.extend(lo.lower_class_init_steps(c, class_id)?);
                        }
                        if let Some(b) = sc.body {
                            lo.append_body_stmts(b, &mut out)?;
                        }
                        let body = if out.is_empty() {
                            None
                        } else {
                            Some(lo.ir.add_expr(IrExpr::Block {
                                stmts: out,
                                value: None,
                            }))
                        };
                        secs.push(crate::ir::IrSecondaryCtor {
                            params: param_irs,
                            delegate_args: dargs,
                            body,
                            delegate,
                        });
                    }
                    lo.ir.classes[class_id as usize].secondary_ctors = secs;
                }
                // Enum entries: lower each entry's constructor arguments (constant expressions
                // evaluated in `<clinit>`), coerced to the matching ctor-parameter field type.
                if c.is_enum() {
                    // Soundness gate for `enum class E : I`: every ABSTRACT interface member must be
                    // satisfied — by a concrete enum method of that name, or by an override in EVERY
                    // entry body. Otherwise the JVM throws `AbstractMethodError`/`IncompatibleClassChange`
                    // at an interface-typed call (e.g. an interface `val ordinal` mapped to `getOrdinal`
                    // that the enum doesn't provide). A classpath-interface supertype (abstractness not
                    // checked here) bails conservatively.
                    for st in &c.supertypes {
                        let Some(ic) = file.decls.iter().find_map(|&d| match file.decl(d) {
                            Decl::Class(ic) if ic.name == *st && ic.is_interface() => Some(ic),
                            _ => None,
                        }) else {
                            return None; // non-file / non-interface supertype on an enum — skip
                        };
                        let generic = !ic.type_params.is_empty();
                        let mut abstract_members: Vec<String> = ic
                            .methods
                            .iter()
                            .filter(|m| matches!(m.body, FunBody::None))
                            .map(|m| m.name.clone())
                            .collect();
                        for p in &ic.body_props {
                            abstract_members.push(getter_name(&p.name));
                            if p.is_var {
                                abstract_members.push(setter_name(&p.name));
                            }
                        }
                        for m in abstract_members {
                            let enum_has = c
                                .methods
                                .iter()
                                .any(|em| em.name == m && !matches!(em.body, FunBody::None));
                            let all_entries_override = !c.enum_entries.is_empty()
                                && c.enum_entry_bodies
                                    .iter()
                                    .all(|b| b.iter().any(|bm| bm.name == m));
                            if !enum_has && !all_entries_override {
                                return None; // unsatisfied abstract interface member — skip
                            }
                            // A GENERIC interface needs an erased bridge (`foo(Object)`→`foo(String)`).
                            // The bridge is computed for the ENUM class (an enum-level override) — so a
                            // generic method satisfied only by PER-ENTRY overrides (bridge would belong on
                            // each entry subclass, not modeled) skips rather than miscompiles.
                            if generic && !enum_has {
                                return None;
                            }
                        }
                    }
                    let class_id = lo.classes[&internal].id;
                    let ctor_count = lo.ir.classes[class_id as usize].ctor_param_count as usize;
                    let field_tys: Vec<Ty> = lo.ir.classes[class_id as usize].fields[..ctor_count]
                        .iter()
                        .map(|f| f.ty.clone())
                        .collect();
                    for (ei, args) in c.enum_entry_args.iter().enumerate() {
                        lo.scope.clear();
                        lo.next_value = 0;
                        lo.cur_class = None;
                        let mut lowered = Vec::new();
                        for (arg, ft) in args.iter().zip(&field_tys) {
                            // Branchy entry args (`X(1 == 1)`) are handled by the `<clinit>` spill.
                            lowered.push(lo.lower_arg(*arg, ft)?);
                        }
                        // Reject an entry whose arg count doesn't match the ctor (default args etc.).
                        if lowered.len() != ctor_count {
                            return None;
                        }
                        lo.ir.classes[class_id as usize].enum_entries[ei].1 = lowered;
                    }
                    // Entry bodies (`ENTRY { override fun m() = … }`) → a synthesized subclass per
                    // bodied entry (`Enum$ENTRY extends Enum`) whose overrides are lowered with the
                    // enum's field/`this` scope (so a read of a constructor `val` becomes a getfield on
                    // the enum). The entry is then constructed as `new Enum$ENTRY(...)`.
                    for (ei, body) in c.enum_entry_bodies.iter().enumerate() {
                        let eprops = c.enum_entry_props.get(ei).cloned().unwrap_or_default();
                        if body.is_empty() && eprops.is_empty() {
                            continue;
                        }
                        let entry_name = &c.enum_entries[ei];
                        let sub_fq = format!("{internal}${entry_name}");
                        // Entry-body PROPERTIES become backing fields (+ getters + ctor init) on the
                        // subclass. Only a plainly-initialized `val`/`var` is modeled; a getter/setter/
                        // delegate/lateinit prop bails (skip, never miscompile).
                        let mut prop_fields: Vec<(String, Ty)> = Vec::new();
                        for p in &eprops {
                            if p.init.is_none()
                                || p.getter.is_some()
                                || p.setter.is_some()
                                || p.delegate.is_some()
                                || p.is_lateinit
                            {
                                return None;
                            }
                            let ty = match &p.ty {
                                Some(r) => ty_of(lo.afile, r),
                                None => lo.info.ty(p.init.unwrap()),
                            };
                            if ty == Ty::Error {
                                return None;
                            }
                            prop_fields.push((p.name.clone(), ty));
                        }
                        let sub_id = lo.ir.add_class(IrClass {
                            serial_names: Vec::new(),
                            custom_serializer: None,
                            field_serializers: Vec::new(),
                            contextual_fields: Vec::new(),
                            fq_name: sub_fq.clone(),
                            is_value: false,
                            type_param_bounds: vec![],
                            type_params: Vec::new(),
                            supertypes: vec![],
                            fields: prop_fields
                                .iter()
                                .zip(eprops.iter())
                                .map(|((n, t), p)| IrField {
                                    is_final: !p.is_var,
                                    ..IrField::new(n.clone(), ty_to_ir(*t))
                                })
                                .collect(),
                            ctor_param_count: 0,
                            ctor_args: vec![],
                            init_body: None,
                            explicit_param_stores: false,
                            methods: vec![],
                            is_interface: false,
                            is_annotation: false,
                            annotation_impl_of: None,

                            is_sealed: false,
                            is_abstract: false,
                            superclass: internal.clone(),
                            super_args: vec![],
                            enum_entries: vec![],
                            enum_entry_subclass: vec![],
                            enum_entry_of: Some(field_tys.clone()),
                            prop_ref: None,
                            func_ref: None,
                            bridges: vec![],
                            interfaces: vec![],
                            is_object: false,
                            ctor_param_checks: vec![],
                            is_companion: false,
                            companion_class: None,
                            secondary_ctors: vec![],
                            has_primary_ctor: true,
                        });
                        // Register the subclass so an override body resolves a prop as `this.<field>` and
                        // getter synthesis can attach. Methods are filled in below.
                        lo.classes.insert(
                            sub_fq.clone(),
                            ClassInfo {
                                id: sub_id,
                                internal: sub_fq.clone(),
                                fields: prop_fields.clone(),
                                methods: HashMap::new(),
                                super_internal: Some(internal.clone()),
                            },
                        );
                        // A body that reads a prop resolves it as a subclass field → `cur_class` must be
                        // the subclass; a property-less entry keeps the enum scope (unchanged behavior).
                        // (When the subclass scope is used, a bare read of an INHERITED enum constructor
                        // property routes through its `getX()` getter; a shape that resolution can't
                        // reach cleanly skips the file — never a miscompile.)
                        let body_cur = if prop_fields.is_empty() {
                            internal.clone()
                        } else {
                            sub_fq.clone()
                        };
                        // Subclass ctor init: `this.<prop> = <init>` for each property (run after super()).
                        if !prop_fields.is_empty() {
                            lo.scope.clear();
                            lo.next_value = 0;
                            lo.cur_class = Some(sub_fq.clone());
                            lo.cur_fn_name = "<init>".to_string();
                            lo.lambda_seq = 0;
                            let this_v = lo.fresh_value();
                            lo.scope
                                .push(("this".to_string(), this_v, Ty::obj(&sub_fq)));
                            let mut stmts = Vec::new();
                            for (fi, (_, fty)) in prop_fields.iter().enumerate() {
                                let init = eprops[fi].init.unwrap();
                                let val = lo.lower_arg(init, &ty_to_ir(*fty))?;
                                let recv = lo.ir.add_expr(IrExpr::GetValue(this_v));
                                stmts.push(lo.ir.add_expr(IrExpr::SetField {
                                    receiver: recv,
                                    class: sub_id,
                                    index: fi as u32,
                                    value: val,
                                }));
                            }
                            let blk = lo.ir.add_expr(IrExpr::Block { stmts, value: None });
                            lo.ir.classes[sub_id as usize].init_body = Some(blk);
                        }
                        let mut mfids = Vec::new();
                        for bm in body {
                            // The override conforms to the abstract member it overrides — use that
                            // signature for the emitted descriptor (kotlinc's erased override shape). The
                            // member is declared on the enum itself, OR on an implemented interface
                            // (`enum class E : I { A { override fun … } }`).
                            let sig = match syms
                                .classes
                                .get(&c.name)
                                .and_then(|cs| cs.methods.get(&bm.name))
                            {
                                Some(s) => s.clone(),
                                None => syms
                                    .supertype_methods(&internal)
                                    .into_iter()
                                    .find(|(n, _)| n == &bm.name)
                                    .map(|(_, s)| s)?,
                            };
                            let params: Vec<Ty> = sig.params.iter().map(|t| ty_to_ir(*t)).collect();
                            let fid = lo.ir.add_fun(IrFunction {
                                name: bm.name.clone(),
                                params,
                                ret: ty_to_ir(sig.ret),
                                body: None,
                                is_static: false,
                                dispatch_receiver: Some(sub_fq.clone()),
                                param_checks: vec![],
                            });
                            lo.scope.clear();
                            lo.next_value = 0;
                            lo.cur_class = Some(body_cur.clone());
                            lo.cur_fn_name = bm.name.clone();
                            lo.lambda_seq = 0;
                            let this_v = lo.fresh_value();
                            lo.scope
                                .push(("this".to_string(), this_v, Ty::obj(&body_cur)));
                            for (p, t) in bm.params.iter().zip(&sig.params) {
                                let v = lo.fresh_value();
                                lo.scope.push((p.name.clone(), v, *t));
                            }
                            let ret_ty = ty_to_ir(sig.ret);
                            lo.lower_body(&bm.body, &ret_ty, fid)?;
                            mfids.push(fid);
                        }
                        lo.ir.classes[sub_id as usize].methods = mfids;
                        // Property getters (`getX()` → `return this.<field>`), kotlinc emits them on the
                        // subclass. Appended after the overrides (add_synth_method pushes onto methods).
                        for (fi, (pname, pty)) in prop_fields.iter().enumerate() {
                            let getter = getter_name(pname);
                            let get = lo.this_field(sub_id, fi as u32);
                            let ret = lo.ir.add_expr(IrExpr::Return(Some(get)));
                            let gbody = lo.ir.add_expr(IrExpr::Block {
                                stmts: vec![ret],
                                value: None,
                            });
                            lo.add_synth_method(
                                &sub_fq,
                                sub_id,
                                &getter,
                                vec![],
                                *pty,
                                gbody,
                                true,
                            );
                        }
                        lo.ir.classes[class_id as usize].enum_entry_subclass[ei] = Some(sub_fq);
                    }
                }
            }
            Decl::Property(p) => {
                set_bail("deep:property");
                lo.scope.clear();
                lo.next_value = 0;
                lo.cur_class = None;
                if let Some(recv_ref) = &p.receiver {
                    // Extension property getter: lower `get() = …` with `this` = receiver (param 0).
                    let recv_ty = ty_of(file, recv_ref);
                    let recv_desc = recv_ty.descriptor();
                    let pty = body_prop_ty(file, info, p);
                    let gfid = *lo
                        .ext_prop_get_ids
                        .get(&(recv_desc.clone(), p.name.clone()))?;
                    lo.cur_fn_name = getter_name(&p.name);
                    lo.lambda_seq = 0;
                    let this_v = lo.fresh_value();
                    lo.scope.push(("this".to_string(), this_v, recv_ty));
                    let body = p.getter.clone().unwrap();
                    lo.lower_body(&body, &ty_to_ir(pty), gfid)?;
                    // `var` extension property setter: `set(v) { … }` with `this` = receiver (param 0),
                    // the value parameter `v` (param 1).
                    if p.is_var {
                        let sfid = *lo.ext_prop_set_ids.get(&(recv_desc, p.name.clone()))?;
                        let setter = p.setter.as_ref().unwrap();
                        lo.scope.clear();
                        lo.next_value = 0;
                        lo.cur_fn_name = setter_name(&p.name);
                        lo.lambda_seq = 0;
                        let this_v = lo.fresh_value();
                        lo.scope.push(("this".to_string(), this_v, recv_ty));
                        let v_v = lo.fresh_value();
                        lo.scope.push((setter.param.clone().unwrap(), v_v, pty));
                        let sbody = setter.body.clone().unwrap();
                        lo.lower_body(&sbody, &ty_to_ir(Ty::Unit), sfid)?;
                    }
                } else if p.delegate.is_some() {
                    lo.lower_delegated_top_level(p)?;
                } else if let Some(&(fid, ty)) = lo.computed_props.get(&p.name) {
                    // A computed property: lower its custom getter into the `getX()` body.
                    lo.cur_fn_name = getter_name(&p.name);
                    lo.lambda_seq = 0;
                    let ret_ty = ty_to_ir(ty);
                    let body = p.getter.clone().unwrap();
                    lo.lower_body(&body, &ret_ty, fid)?;
                } else {
                    let (_, ty) = lo.statics[&p.name].clone();
                    let ir_ty = ty_to_ir(ty);
                    let init = lo.lower_arg(p.init.unwrap(), &ir_ty)?;
                    lo.ir.statics.push(crate::ir::IrStatic {
                        name: p.name.clone(),
                        ty: ir_ty,
                        init,
                        is_var: p.is_var,
                        is_const: p.is_const,
                        owner: None,
                    });
                }
            }
        }
    }
    // Pass 2': lower the lifted local-function bodies. A non-capturing local function lowers like a
    // top-level static method: its parameters are values `0..n`, no captured outer scope.
    let local_funs: Vec<(crate::ast::StmtId, ast::FunDecl)> = file
        .stmt_arena
        .iter()
        .enumerate()
        .filter_map(|(i, s)| match s {
            Stmt::LocalFun(f) => Some((crate::ast::StmtId(i as u32), f.clone())),
            _ => None,
        })
        .collect();
    for (stmt_id, f) in &local_funs {
        let Some(&fid) = lo.local_fun_ids.get(stmt_id) else {
            continue;
        };
        lo.scope.clear();
        lo.next_value = 0;
        lo.cur_class = None;
        lo.cur_fn_name = lo.ir.functions[fid as usize].name.clone();
        lo.lambda_seq = 0;
        let sig = info.local_fun_sigs.get(stmt_id)?.1.clone();
        // Captured outer locals occupy the leading value slots; a boxed one binds its `Ref` holder
        // (reads/writes go through `element` via `boxed_elem`), an ordinary one binds its value.
        if let Some(caps) = info.local_fun_captures.get(stmt_id) {
            for (name, ty) in caps {
                let v = lo.fresh_value();
                if info.boxed_vars.contains(name) {
                    lo.scope
                        .push((name.clone(), v, Ty::obj(ref_holder_internal(*ty))));
                    lo.boxed_elem.insert(name.clone(), *ty);
                } else {
                    lo.scope.push((name.clone(), v, *ty));
                }
            }
        }
        for (p, t) in f.params.iter().zip(&sig.params) {
            let v = lo.fresh_value();
            lo.scope.push((p.name.clone(), v, *t));
        }
        let ret_ty = lo.ir.functions[fid as usize].ret.clone();
        lo.lower_body(&f.body, &ret_ty, fid)?;
    }
    // A covariant/generic override returning `Nothing` (always throws) needs a bridge krusty can't emit:
    // the throwing concrete method leaves nothing to `areturn` as the erased reference return. Skip the
    // file rather than emit bad bytecode (cf. inlineClasses/overrideReturnNothing).
    // A covariant/generic override returning `Nothing` (always throws) lowers its return to the JVM
    // `java/lang/Void` repr; the bridge would `areturn` it as the erased reference return, which krusty's
    // bridge emitter can't reconcile (the throwing concrete leaves nothing). Skip the file rather than
    // emit bad bytecode (cf. inlineClasses/overrideReturnNothing).
    let nothing_bridge = lo.ir.classes.iter().any(|c| {
        c.bridges
            .iter()
            .any(|b| b.concrete_ret.non_null().obj_internal() == Some("java/lang/Void"))
    });
    if nothing_bridge {
        return None;
    }
    // Discover classpath `@JvmInline value class`es referenced by type in this file and record their
    // REFERENCE underlying (`Result` → `Object`), so the value-class pass erases them like a user value
    // class. A primitive-underlying value class (`UInt`/`ULong` → `Int`/`Long`) is EXCLUDED — it keeps
    // its existing dedicated handling, and erasing it here would disturb that.
    {
        let mut referenced: std::collections::HashSet<String> = std::collections::HashSet::new();
        let note = |t: &Ty, set: &mut std::collections::HashSet<String>| {
            if let Some(fq_name) = t.non_null().obj_internal() {
                set.insert(fq_name.to_string());
            }
        };
        for f in &lo.ir.functions {
            for p in &f.params {
                note(p, &mut referenced);
            }
            note(&f.ret, &mut referenced);
        }
        for c in &lo.ir.classes {
            for f in &c.fields {
                note(&f.ty, &mut referenced);
            }
            for (t, _) in &c.ctor_args {
                note(t, &mut referenced);
            }
        }
        for s in &lo.ir.statics {
            note(&s.ty, &mut referenced);
        }
        for e in &lo.ir.exprs {
            if let IrExpr::Variable { ty, .. } = e {
                note(ty, &mut referenced);
            }
        }
        for fq in referenced {
            if let Some(under) = lo
                .syms
                .libraries
                .resolve_type(&fq)
                .and_then(|t| t.value_underlying)
            {
                if !under.is_primitive() {
                    // The underlying reference is null-capable (`Result`'s `Any?`), mirroring how the pass
                    // marks a type-parameter value-class field.
                    let ir_under = Ty::nullable(ty_to_ir(under));
                    lo.ir.external_value_classes.insert(fq, ir_under);
                }
            }
        }
    }
    Some(lo.ir)
}

/// A class is in the IR subset if: a primary constructor of only `val`/`var` properties, no base
/// class/interfaces, no body properties, no companion/secondary/init, and methods (expr- or
/// The type parameters of a generic function as `(name, bound, non_null)`, for lowering `as T`. `bound`
/// is the declared upper bound kept VERBATIM as an `IrType` — `<T : CharSequence>` → `CharSequence`,
/// an unbounded `<T>` → `kotlin/Any` (NOT erased here; the JVM emitter collapses it to a concrete
/// class). `non_null` marks a non-nullable bound (`<T : Any>`, `<T : Foo>`) — such a cast null-checks
/// (kotlinc emits `Intrinsics.checkNotNull`); an unbounded `<T>` (= `<T : Any?>`) does not.
fn collect_tparams(
    file: &ast::File,
    names: &[String],
    bounds: &[(String, ast::TypeRef)],
    non_null: &std::collections::HashSet<String>,
) -> Vec<(String, Ty, bool)> {
    names
        .iter()
        .map(|name| {
            let bound_ref = bounds.iter().find(|(n, _)| n == name).map(|(_, tr)| tr);
            let bound = bound_ref
                .map(|tr| ty_to_ir(ty_of(file, tr)))
                .unwrap_or(Ty::nullable(Ty::obj("kotlin/Any")));
            let non_null = non_null.contains(name) || bound_ref.is_some_and(|tr| !tr.nullable);
            (name.clone(), bound, non_null)
        })
        .collect()
}

/// block-bodied) without an extension receiver.
/// A `const val`'s compile-time literal initializer as an `IrConst`, narrowed to its declared type
/// (`const val b: Byte = 1` → `Byte(1)`). `None` for any non-literal initializer (then the read stays a
/// `getstatic`). Lets a same-file const read inline the value (`ldc`), byte-identical to kotlinc.
fn ast_literal_const(file: &ast::File, e: AstExprId, ty: Ty) -> Option<crate::ir::IrConst> {
    use crate::ir::IrConst;
    use ast::Expr;
    Some(match file.expr(e) {
        Expr::IntLit(v) => match ty {
            Ty::Byte => IrConst::Byte(*v as i8),
            Ty::Short => IrConst::Short(*v as i16),
            Ty::Char => IrConst::Char(char::from_u32(*v as u32)?),
            _ => IrConst::Int(*v as i32),
        },
        Expr::LongLit(v) => IrConst::Long(*v),
        Expr::DoubleLit(v) => IrConst::Double(*v),
        Expr::FloatLit(v) => IrConst::Float(*v),
        Expr::BoolLit(b) => IrConst::Boolean(*b),
        Expr::StringLit(s) => IrConst::String(s.clone()),
        Expr::CharLit(c) => IrConst::Char(*c),
        Expr::UIntLit(v) => IrConst::Int(*v as i32),
        Expr::ULongLit(v) => IrConst::Long(*v),
        _ => return None,
    })
}

/// Whether a class's `companion object` properties are all lowerable: each a `const val` with a plain
/// compile-time initializer (no getter/setter/delegate). Such a const becomes a `public static final` +
/// `ConstantValue` field on the OUTER class (kotlinc's layout). An empty list is trivially lowerable.
fn companion_props_lowerable(c: &ast::ClassDecl) -> bool {
    c.companion_props.iter().all(|p| {
        p.is_const
            && !p.is_var
            && p.init.is_some()
            && p.getter.is_none()
            && p.setter.is_none()
            && p.delegate.is_none()
    })
}

fn is_simple_class(c: &ast::ClassDecl) -> bool {
    // A `data class` is structurally a simple class; its equals/hashCode/toString/componentN are
    // synthesized as ordinary IR methods (see `synth_data_members`). `value`/inline classes need
    // unboxing and are excluded.
    // A base class (`: A(args)`) is allowed when `A` is itself a simple/open class in this file
    // (checked at registration); interface supertypes are not yet supported.
    // Interface supertypes (`class C : I`) are allowed when each is a file interface (checked at
    // registration); a base class is allowed when it's a simple/open file class.
    // `abstract class` is allowed: its abstract methods (no body) are emitted as `ACC_ABSTRACT`,
    // concrete methods normally. A `@JvmInline value class` is structurally a single-field class; its
    // unboxed-support members are synthesized (see `synth_value_members`). Use-site unboxing isn't done
    // yet, so the resolver still rejects value-class *files* — admission here is for member synthesis.
    // A `companion object` with only methods is supported (synthesized `C$Companion` class). A companion
    // `const val` (compile-time literal) is supported too — emitted as a `public static final` +
    // `ConstantValue` field on the OUTER class (kotlinc's layout). A NON-const companion property (needs
    // the `access$getX$cp` accessor + `Companion.getX()`) is not yet modeled.
    !c.is_object() && !c.is_enum() && !c.is_interface() && companion_props_lowerable(c)
        // Secondary constructors: in a class WITH a primary ctor each must delegate to it (`this(…)`);
        // a class with NO primary ctor admits `this(…)` (to a sibling), `super(…)`, or implicit
        // delegation — each becomes its own `<init>` (see the secondary-ctor lowering).
        && (if c.has_primary_ctor {
            c.secondary_ctors.iter().all(|sc| matches!(sc.delegation, ast::CtorDelegation::This(_)))
        } else {
            true
        })
        // A non-`val`/`var` primary-ctor parameter (`class C(x: Int) { val y = x }`) is an argument only
        // (no field), available in the constructor body — lowered via `ctor_args`. (Both property and
        // plain params are fine here.)
        // Body properties (`class C { val x = … }`) are allowed when they're plain backing fields
        // initialized in the constructor; `init { … }` blocks run there too (see `init_order`). An
        // `abstract val x: T` (no field, emitted as an abstract `getX()`) is also allowed.
        && c.body_props.iter().all(|p| is_plain_body_prop(p) || is_computed_prop(p) || is_field_accessor_prop(p) || p.is_abstract || is_deferred_val_prop(p) || is_lateinit_prop(p) || p.delegate.is_some())
        // Methods are non-extension; an abstract method (no body) is allowed on an abstract class
        // (the checker only permits that), and emitted as an `ACC_ABSTRACT` declaration.
        && c.methods.iter().all(|m| m.receiver.is_none())
}

/// An `enum class` the IR can emit: a primary constructor of `val`/`var` props, concrete (non-extension,
/// bodied) or abstract methods, plain body-props, and no companion / secondary ctors / supertypes.
/// Per-entry bodies (`ENTRY { override fun m() = … }`) are emitted as anonymous subclasses; only
/// method overrides are supported (a property override would need backing-field plumbing — deferred).
fn is_simple_enum(c: &ast::ClassDecl) -> bool {
    let abstract_names: std::collections::HashSet<&str> = c
        .methods
        .iter()
        .filter(|m| matches!(m.body, FunBody::None))
        .map(|m| m.name.as_str())
        .collect();
    c.is_enum()
        && c.companion_methods.is_empty() && c.companion_props.is_empty()
        && c.secondary_ctors.is_empty()
        // An enum may implement interfaces (`enum class E : I`) — supertypes are interfaces only (an enum
        // can't extend a class); the lowering resolves them as interfaces or bails.
        && c.props.iter().all(|p| p.is_property)
        // A body property must be a plain backing field — an `abstract val` (entry-overridden property)
        // isn't modeled yet.
        && c.body_props.iter().all(|p| is_plain_body_prop(p) && !p.is_abstract)
        && c.methods.iter().all(|m| m.receiver.is_none())
        // Entry-body overrides: concrete, non-extension methods only.
        && c.enum_entry_bodies.iter().all(|b| b.iter().all(|m| m.receiver.is_none() && !matches!(m.body, FunBody::None)))
        // Every entry must override EVERY abstract member: a bodyless entry would instantiate the
        // abstract enum, and an entry that overrides only some members would leave its synthesized
        // subclass with an unimplemented abstract method (AbstractMethodError). kotlinc requires full
        // coverage; if any entry's overrides don't cover all abstract members, skip (never miscompile).
        && (abstract_names.is_empty() || c.enum_entry_bodies.iter().all(|b| {
            let overridden: std::collections::HashSet<&str> = b.iter().map(|m| m.name.as_str()).collect();
            abstract_names.iter().all(|n| overridden.contains(n))
        }))
}

/// An `object Foo` the IR can emit as a singleton: no primary-constructor params, plain body
/// properties, concrete (bodied, non-extension) methods, no inheritance/interfaces/companion.
fn is_simple_object(c: &ast::ClassDecl) -> bool {
    c.is_object()
        // INTERFACE supertypes are allowed (`object X : KSerializer<C>`); a base CLASS supertype
        // (`object A : Sealed()`) too — the general class lowering computes the `superclass` + emits the
        // `super(args)` call, and bails the file if the base isn't a simple file class. But a base class
        // AND interfaces together (`object O : A(), T`) invites a qualified `super<A>`/`super<T>` call
        // krusty doesn't dispatch — skip that combination.
        && (c.base_class.is_none() || c.supertypes.is_empty())
        && c.companion_methods.is_empty() && c.companion_props.is_empty() && c.secondary_ctors.is_empty()
        && c.props.is_empty()
        && c.body_props.iter().all(is_plain_body_prop)
        && c.methods.iter().all(|m| m.receiver.is_none() && !matches!(m.body, FunBody::None))
        // An `init { … }` block with side effects must not run when a `const val` is read (a const is
        // inlined, not fetched through INSTANCE) — krusty doesn't model const-inlining, so bail.
        && c.init_order.iter().all(|s| matches!(s, ast::ClassInit::PropInit(_)))
        // A `const val` read through a method during property initialization would observe the
        // uninitialized backing field (kotlinc inlines the constant; krusty doesn't). Bail.
        && !(c.body_props.iter().any(|p| p.is_const) && !c.methods.is_empty())
}

/// An `interface` the IR can emit: only abstract methods (no default/bodied methods, which need a
/// `DefaultImpls` class), no properties (abstract property getters not modeled), no companion.
fn is_simple_interface(c: &ast::ClassDecl) -> bool {
    c.is_interface()
        && c.companion_methods.is_empty() && c.companion_props.is_empty()
        && c.props.is_empty()
        // Abstract properties (`val x: T`, no initializer/getter) become abstract `getX()`/`setX()`;
        // a property with an initializer or custom getter (an interface can't have a backing field)
        // isn't modeled.
        && c.body_props.iter().all(|p| p.init.is_none() && p.getter.is_none() && p.ty.is_some())
        // Non-extension methods only; a method may be abstract (no body) OR a default method (with a
        // body, emitted as a JVM default method).
        && c.methods.iter().all(|m| m.receiver.is_none())
}

/// A class-body property that is a plain backing field: a normal (non-extension) `val`/`var` with an
/// initializer and no custom getter/setter and not `lateinit`.
/// Whether a file-local delegate class's `getValue` references its `KProperty` parameter (its second
/// parameter) in the body — i.e. uses property reflection. Such delegates need the property reference
/// to resolve through `@Metadata` (not emitted for delegated properties), so they're skipped. A
/// library/classpath delegate (not found here) returns `false` — library `getValue`s don't reflect.
fn delegate_getvalue_uses_property(file: &ast::File, internal: &str) -> bool {
    for &d in &file.decls {
        let ast::Decl::Class(c) = file.decl(d) else {
            continue;
        };
        if internal != c.name && !internal.ends_with(&format!("/{}", c.name)) {
            continue;
        }
        for m in &c.methods {
            if m.name != "getValue" {
                continue;
            }
            let Some(param) = m.params.get(1) else {
                continue;
            };
            let body = match &m.body {
                ast::FunBody::Expr(e) | ast::FunBody::Block(e) => *e,
                ast::FunBody::None => continue,
            };
            if crate::resolve::expr_uses_name_pub(file, body, &param.name) {
                return true;
            }
        }
    }
    false
}

fn is_plain_body_prop(p: &ast::PropDecl) -> bool {
    p.receiver.is_none()
        && !p.is_lateinit
        && p.getter.is_none()
        // A visibility-only setter (`var x = 1; private set`, no body) is still a plain backing field — the
        // only difference is the setter's access flag (handled at emit). A setter with a BODY is not plain.
        && p.setter.as_ref().is_none_or(|s| s.body.is_none())
        && p.init.is_some()
}

/// Whether a body-property initializer *AST expression* is the field's JVM default value:
/// `0`/`0L`/`0.0`/`0.0f`/`false`/`'\0'`/`null`, or such a zero literal under a primitive conversion
/// (`0.toByte()`/`0.toChar()`). kotlinc elides a field initializer equal to the field's default — the
/// JVM zero-initializes the field, so re-storing the default would clobber a value a base constructor's
/// virtual call already wrote. Decided on the AST (a default-value literal has no side effect to lose).
fn ast_init_is_jvm_default(file: &ast::File, e: AstExprId) -> bool {
    match file.expr(e) {
        Expr::IntLit(0) | Expr::LongLit(0) | Expr::NullLit | Expr::BoolLit(false) => true,
        Expr::DoubleLit(d) => *d == 0.0,
        Expr::FloatLit(f) => *f == 0.0,
        Expr::CharLit(c) => *c as u32 == 0,
        // `0.toByte()` / `0.toChar()` — a primitive conversion of a zero literal is still the default.
        Expr::Call { callee, args } if args.is_empty() => match file.expr(*callee) {
            Expr::Member { receiver, name }
                if crate::resolve::conversion_target(name).is_some() =>
            {
                ast_init_is_jvm_default(file, *receiver)
            }
            _ => false,
        },
        _ => false,
    }
}

/// A *deferred* `val` body property: declared with an explicit type and NO initializer/getter/setter
/// (`val a: Int`), assigned exactly once in an `init` block. It's a real backing field, just initialized
/// in the constructor body rather than at the declaration.
fn is_deferred_val_prop(p: &ast::PropDecl) -> bool {
    !p.is_var
        && p.receiver.is_none()
        && !p.is_lateinit
        && p.init.is_none()
        && p.getter.is_none()
        && p.setter.is_none()
        && !p.is_abstract
        && p.ty.is_some()
}

/// A `lateinit var x: T` — a mutable backing-field property with no initializer (the field defaults to
/// `null`); the synthesized getter throws `UninitializedPropertyAccessException` when the field is still
/// `null`. The declared type is non-null but the JVM field is a plain (nullable-at-runtime) reference.
fn is_lateinit_prop(p: &ast::PropDecl) -> bool {
    p.is_lateinit
        && p.is_var
        && p.receiver.is_none()
        && p.init.is_none()
        && p.getter.is_none()
        && p.setter.is_none()
        && !p.is_abstract
        && p.ty.is_some()
}

/// A computed property `val x: T get() = expr` — a custom getter, no backing field (no initializer),
/// immutable (no setter). Compiled to a `getX()` accessor; reads call it.
/// The `Ty` of a body property: its explicit annotation, else inferred from the getter body (a computed
/// `val xx get() = x`) or the initializer.
fn body_prop_ty(file: &ast::File, info: &TypeInfo, p: &ast::PropDecl) -> Ty {
    if let Some(r) = p.ty.as_ref() {
        ty_of(file, r)
    } else if let Some(FunBody::Expr(g) | FunBody::Block(g)) = p.getter {
        info.ty(g)
    } else if let Some(i) = p.init {
        info.ty(i)
    } else {
        Ty::Error
    }
}

fn is_computed_prop(p: &ast::PropDecl) -> bool {
    p.receiver.is_none()
        && !p.is_lateinit
        && !p.is_var
        && p.init.is_none()
        && p.getter.is_some()
        && p.setter.is_none()
    // The type may be inferred from the getter body (`val xx get() = x`) — no explicit annotation needed.
}

/// A body property with a real backing field — neither a computed property (custom getter, no field)
/// nor an `abstract` one (emitted as an abstract `getX()`, the field lives on the subclass) nor a
/// delegated one (`by Del()` — its field is the synthetic `x$delegate`, accessor calls `getValue`).
fn is_backing_field_prop(p: &ast::PropDecl) -> bool {
    !is_computed_prop(p) && !p.is_abstract && p.delegate.is_none()
}

/// A backing-field property whose accessor is CUSTOM and reads/writes `field` (`val x = init get() =
/// field…`, `var x = init set(v) { field = … }`). The backing field is emitted as usual; the
/// synthesized `getX`/`setX` run the custom body (with `field` bound to that field) instead of the
/// default field read/write.
fn is_field_accessor_prop(p: &ast::PropDecl) -> bool {
    is_backing_field_prop(p)
        && p.init.is_some()
        && (p.getter.is_some() || p.setter.as_ref().is_some_and(|s| s.body.is_some()))
}

/// The JVM accessor name for a property: `x` → `getX`; an `is`-prefixed boolean keeps its name
/// (`isEmpty` → `isEmpty`), matching kotlinc.
fn getter_name(prop: &str) -> String {
    let b = prop.as_bytes();
    if prop.starts_with("is") && b.len() > 2 && b[2].is_ascii_uppercase() {
        return prop.to_string();
    }
    let mut c = prop.chars();
    format!("get{}{}", c.next().unwrap().to_uppercase(), c.as_str())
}

/// The JVM setter name for a property: `x` → `setX`; `isOpen` → `setOpen` (the `is` prefix is dropped).
fn setter_name(prop: &str) -> String {
    let b = prop.as_bytes();
    let base = if prop.starts_with("is") && b.len() > 2 && b[2].is_ascii_uppercase() {
        &prop[2..]
    } else {
        prop
    };
    let mut c = base.chars();
    format!("set{}{}", c.next().unwrap().to_uppercase(), c.as_str())
}

/// One active inlined-lambda parameter: `(param name, lambda parameter names, lambda body, lambda
/// parameter types, the inline fn's name — the implicit label for a `return@<fn>` local return)`.
type InlineLambda = (String, Vec<String>, AstExprId, Vec<Ty>, String);

/// Lowering info for a local delegated property (`val x by Del()` in a function body). Reads compile to
/// `<delegate>.getValue(null, propref)`, a `var`'s writes to `setValue(null, propref, value)`.
#[derive(Clone)]
struct LocalDelegate {
    /// JVM internal name of the delegate type (owner of `getValue`/`setValue`).
    delegate_internal: String,
    /// `getValue(Object, KProperty)ret` descriptor.
    getvalue_desc: String,
    /// `setValue(Object, KProperty, value)V` descriptor — `None` for a `val`.
    setvalue_desc: Option<String>,
    /// The property name (for the inline `PropertyReference0Impl` metadata).
    name: String,
    /// The property type's JVM descriptor (for the property-reference signature string).
    ret_desc: String,
}

/// Context for lowering a `tailrec` function into a `while(true)` loop.
#[derive(Clone)]
struct TailrecCtx {
    name: String,
    param_vals: Vec<u32>,
    param_tys: Vec<Ty>,
    label: String,
}

pub(crate) struct Lower<'a> {
    afile: &'a ast::File,
    info: &'a TypeInfo,
    syms: &'a SymbolTable,
    ir: IrFile,
    /// Top-level function ids keyed by (name, erased-parameter-descriptor) so overloads (same name,
    /// different params) each map to their own compiled method.
    fun_ids: HashMap<(String, String), u32>,
    /// Top-level extension functions, keyed by `(receiver type descriptor, name)` — separate from
    /// `fun_ids` since `fun Int.foo()` and `fun String.foo()` share a name but differ by receiver.
    ext_fun_ids: HashMap<(String, String), u32>,
    /// Top-level extension PROPERTIES (`val/var Recv.name: T get() = … [set(v) = …]`), keyed by
    /// `(receiver descriptor, name)` → the synthesized static getter (`getName(Recv): T`) / setter
    /// (`setName(Recv, T)`) FunId.
    ext_prop_get_ids: HashMap<(String, String), u32>,
    /// `(outer class internal, companion `const val` name)` → its type. Such a const lives as a
    /// `public static final` field on the OUTER class; a `C.X` read lowers to `getstatic C.X`.
    companion_consts: HashMap<(String, String), Ty>,
    /// Top-level `const val` name → its compile-time literal value. A same-file read inlines this as a
    /// constant (kotlinc's `ldc`), exactly like the reference compiler — byte-identical, no `getstatic`.
    const_lits: HashMap<String, crate::ir::IrConst>,
    ext_prop_set_ids: HashMap<(String, String), u32>,
    classes: HashMap<String, ClassInfo>,
    /// Top-level property name → (index into `ir.statics`, type).
    statics: HashMap<String, (u32, Ty)>,
    scope: Vec<(String, u32, Ty)>,
    next_value: u32,
    cur_class: Option<String>,
    /// When lowering a property's custom accessor body (`get()`/`set()`), the property's backing field
    /// `(class_id, field_index, field_ir_type)` — so the `field` keyword reads/writes it. `None`
    /// outside an accessor body.
    cur_field: Option<(u32, u32, Ty)>,
    /// `(class internal, property name)` for every property with a CUSTOM accessor over a backing
    /// field. Such a property is read/written ONLY through `getX`/`setX` (even in-class) — never as a
    /// direct field — so `resolve_field` declines it (the `field` keyword reaches the field via
    /// `cur_field` instead).
    field_accessor_props: std::collections::HashSet<(String, String)>,
    /// Name of the enclosing function/method being lowered — used to name synthesized lambda impl
    /// methods `<enclosing>$lambda$<n>` (matching kotlinc).
    cur_fn_name: String,
    /// Whether the enclosing function is `suspend`. A non-suspend function cannot call a suspend fn, so
    /// `&&`/`||` short-circuit safely there; inside a suspend body the right operand may carry a
    /// suspension that the CPS flattener models only at unconditional positions, so `&&`/`||` keep the
    /// eager (operands-unconditional) form there until the flattener models conditional suspension.
    cur_fn_suspend: bool,
    /// Type parameters in scope for the function body being lowered: `(name, bound, non_null)`. `bound`
    /// is the declared upper bound as an un-erased `IrType` (`kotlin/Any` when unbounded); `non_null`
    /// is set for a non-nullable bound (`<T : Any>`, `<T : Foo>`) — drives the `as T` null assertion.
    cur_tparams: Vec<(String, Ty, bool)>,
    /// Per-enclosing-function counter for lambda impl-method naming.
    lambda_seq: u32,
    /// A boxed mutable-capture local's name → its element (unboxed) type. The scope holds the name
    /// bound to the `Ref$XxxRef` HOLDER value; reads/writes go through `RefGet`/`RefSet` with this type.
    boxed_elem: HashMap<String, Ty>,
    /// A local function's `StmtId` → its lifted static `FunId` (a private method on the facade). A call
    /// to it (via `info.local_call_map`) lowers to `Callee::Local(fid)`.
    local_fun_ids: HashMap<crate::ast::StmtId, u32>,
    /// Return type of the function currently being lowered — used to coerce `return` values (e.g. a
    /// generic-erased `Object` return gets the `checkcast` kotlinc inserts).
    cur_ret_ty: Ty,
    /// True while lowering a method whose JVM return is the `kotlin/Unit` SINGLETON (a reference) rather
    /// than `void` — i.e. a `() -> Unit` lambda's closure method (its `invoke` returns `Object`). A
    /// valueless `return`/`return@lambda` there must push `Unit.INSTANCE` (`areturn`), not `return`
    /// (`void`), or the verifier rejects it ("method expects a return value"). A plain `Unit` function
    /// (`fun f(): Unit`) is a `void` method, so this stays false for it.
    cur_method_returns_unit_ref: bool,
    /// `finally` blocks of the enclosing `try`s (outermost first) whose protected region covers the
    /// statement being lowered. A `return` inside them must run each `finally` (innermost first) before
    /// transferring control — so the lowerer inlines them at the `return`. Pushed while a try-body/catch
    /// with a `finally` is lowered.
    try_finally_stack: Vec<AstExprId>,
    /// Outer-class internal name → its `C$Companion` internal name, for routing `C.foo()` calls.
    companions: HashMap<String, String>,
    /// Top-level computed property name → (its synthesized `getX()` `FunId`, property type). A read of
    /// the property compiles to a call to the getter (there is no backing field).
    computed_props: HashMap<String, (u32, Ty)>,
    /// Local delegated property name → its delegate info. A read of the name compiles to the delegate's
    /// `getValue` (a `var`'s write to `setValue`); there is no value slot for the property itself, only
    /// the synthesized `$delegate` local (its value index is held here).
    local_delegated: HashMap<String, LocalDelegate>,
    /// Active `tailrec` function being lowered: its (unqualified) name, the value indices of its
    /// parameters (to reassign), and their types. A tail-position `return f(args)` to this same function
    /// becomes "reassign the params, `continue` the wrapping `while(true)`" instead of a real call.
    cur_tailrec: Option<TailrecCtx>,
    /// Current expression-lowering recursion depth — guards against a stack overflow on a pathologically
    /// deep expression (a stress test with thousands of nested operators): past the limit, lowering
    /// bails (the file is skipped, never miscompiled or crashed).
    expr_depth: u32,
    /// Active inlined-lambda parameters while expanding an `inline fun` body, as a stack so nested
    /// inline calls compose: a call `param(args)` in the inline body inlines the lambda body in place.
    inline_lambdas: Vec<InlineLambda>,
    /// Call-site expression ids currently being inline-expanded. A genuinely recursive inline call
    /// re-enters the SAME call site (the `rec(n-1)` in `rec`'s own body), which would expand forever —
    /// so re-entering an active call id bails (the file skips). Source-level NESTING (`a { a { 5 } }`)
    /// uses DISTINCT call sites, so it is allowed. (kotlinc rejects only genuine recursion.)
    inline_active: Vec<u32>,
    /// Active reified type-parameter bindings while expanding a `<reified T>` inline fn: `T` → the
    /// call's actual type argument. Consulted by `subst_type_ref` so `is T`/`as T`/`T::class` in the
    /// inlined body specialize to the concrete type. A stack — nested reified inline calls compose.
    reified_subst: Vec<std::collections::HashMap<String, ast::TypeRef>>,
    /// Active inline-fn return targets while expanding an `inline fun` whose body has `return`: each is
    /// `(result slot, end label, return type)`. A `return x` in the inlined body lowers to `result = x;
    /// break@end` (the body is wrapped in a `do { … } while(false)` labeled `end`), turning the function
    /// return into a jump to the body's end. Innermost (`.last()`) is the enclosing inline fn; lambda
    /// args with returns are pre-bailed, so a `return` always belongs to the innermost inline body.
    inline_return: Vec<(u32, String, Ty)>,
    /// Active *labeled* lambda-return targets while splicing an inline lambda whose body contains a
    /// `return@<label>` (a local return from that lambda). Each is `(label, result slot, end label,
    /// return type)`: a `return@label x` lowers to `slot = x; break@end`, the spliced lambda body being
    /// wrapped in a `do { … } while(false)`. The label is the inline fn name the lambda was passed to.
    inline_lambda_ret: Vec<(String, u32, String, Ty)>,
}

impl<'a> Lower<'a> {
    /// The arg-binding call-resolution layer over this lowerer's [`LibrarySet`]. Cheap to construct.
    fn resolver(&self) -> crate::call_resolver::CallResolver<'_> {
        crate::call_resolver::CallResolver::new(&*self.syms.libraries)
    }
    /// Whether the current module declares a top-level function `name` (shadow-precedence test) — asked
    /// through the module source rather than touching `syms.funs` directly.
    fn module_declares(&self, name: &str) -> bool {
        crate::module_symbols::ModuleSymbols::new(self.syms).declares_top_level(name)
    }
    fn fresh_value(&mut self) -> u32 {
        let v = self.next_value;
        self.next_value += 1;
        v
    }

    /// The parameter types of a class's superclass constructor (`super(args)` targets these). Empty for
    /// `java/lang/Object` or a base whose IR class isn't in this file (then we can't model the call).
    fn super_ctor_param_tys(&self, internal: &str) -> Vec<Ty> {
        self.classes[internal]
            .super_internal
            .clone()
            .and_then(|s| self.classes.get(&s).map(|sup| sup.id))
            .map(|sid| {
                let sup = &self.ir.classes[sid as usize];
                if sup.ctor_args.is_empty() {
                    let n = sup.ctor_param_count as usize;
                    sup.fields[..n].iter().map(|f| f.ty.clone()).collect()
                } else {
                    sup.ctor_args.iter().map(|(t, _)| t.clone()).collect()
                }
            })
            .unwrap_or_default()
    }

    /// Lower a class's body-property initializers + `init {}` blocks (source order) into IR effect
    /// statements, assuming `this` (and any constructor params) are already in scope and value numbering
    /// continues from `self.next_value`. Returns `None` on an unsupported initializer shape (a branchy
    /// value initializer in a constructor). Used by both the primary `<init>` and each `super(...)`-
    /// reaching secondary constructor of a no-primary class.
    fn lower_class_init_steps(&mut self, c: &ast::ClassDecl, class_id: u32) -> Option<Vec<ExprId>> {
        let ctor_count = self.ir.classes[class_id as usize].ctor_param_count;
        let this_v = self
            .scope
            .iter()
            .find(|(n, _, _)| n == "this")
            .map(|(_, v, _)| *v)?;
        let mut stmts = Vec::new();
        for step in &c.init_order {
            match step {
                ast::ClassInit::PropInit(i) => {
                    if !is_backing_field_prop(&c.body_props[*i]) || c.body_props[*i].init.is_none()
                    {
                        continue;
                    }
                    let body_offset = c.body_props[..*i]
                        .iter()
                        .filter(|p| is_backing_field_prop(p))
                        .count();
                    let field_idx = ctor_count + body_offset as u32;
                    let field_ty = self.ir.classes[class_id as usize].fields[field_idx as usize]
                        .ty
                        .clone();
                    let init_e = c.body_props[*i].init.unwrap();
                    // kotlinc elides a field initializer that stores the field's JVM default value — a
                    // base-class constructor's virtual call may have already written the field, and a
                    // default-value store would clobber it (see `fieldInitializerOptimization`).
                    if ast_init_is_jvm_default(self.afile, init_e) {
                        continue;
                    }
                    if matches!(
                        self.afile.expr(init_e),
                        Expr::When { .. }
                            | Expr::If { .. }
                            | Expr::Elvis { .. }
                            | Expr::Block { .. }
                            | Expr::Try { .. }
                    ) {
                        return None;
                    }
                    let val = self.lower_arg(init_e, &field_ty)?;
                    let recv = self.ir.add_expr(IrExpr::GetValue(this_v));
                    stmts.push(self.ir.add_expr(IrExpr::SetField {
                        receiver: recv,
                        class: class_id,
                        index: field_idx,
                        value: val,
                    }));
                }
                ast::ClassInit::Block(e) => {
                    let Expr::Block {
                        stmts: bs,
                        trailing,
                    } = self.afile.expr(*e).clone()
                    else {
                        return None;
                    };
                    for &s in &bs {
                        let branchy = match self.afile.stmt(s) {
                            Stmt::Assign { value, .. } | Stmt::AssignMember { value, .. } => {
                                body_contains_branch(self.afile, *value)
                            }
                            Stmt::Local { init, .. } => body_contains_branch(self.afile, *init),
                            _ => false,
                        };
                        if branchy {
                            return None;
                        }
                    }
                    for s in bs {
                        stmts.push(self.stmt(s)?);
                    }
                    if let Some(t) = trailing {
                        stmts.push(self.expr(t)?);
                    }
                }
            }
        }
        Some(stmts)
    }

    // ---- Builder surface for the synthetic registry (`crate::synthetics`) ------------------------
    // A synthetic's IR-override body builds its IR against the active `Lower` through these. Kept thin
    // so the registry (not lowering) owns each synthetic's body.

    /// Append an IR expression, returning its id.
    pub(crate) fn emit(&mut self, e: IrExpr) -> ExprId {
        self.ir.add_expr(e)
    }

    /// Lower an argument expression in value position (no target-element coercion).
    pub(crate) fn synth_expr(&mut self, e: AstExprId) -> Option<ExprId> {
        self.expr(e)
    }

    /// Whether `e` lowers with control flow (records a stackmap frame) — a synthetic that builds a
    /// value on the stack declines such an element rather than strand operands across the frame.
    pub(crate) fn synth_is_branchy(&self, e: AstExprId) -> bool {
        is_branchy(self.afile, e)
    }

    /// The element type of an array-creating call (`arrayOf`/`arrayOfNulls<T>`/`Array<T>(n){}`/
    /// `emptyArray<T>()`). Prefers the call's explicit type argument resolved through `ty_ref` — so a
    /// reified `T` inside an expanded `<reified T>` inline body specializes the element (`new String[]`,
    /// not the erased `Object[]`); falls back to the checker-inferred `Array<T>` element otherwise.
    pub(crate) fn synth_array_elem(&self, call: AstExprId) -> Option<Ty> {
        if let Some(t) = self
            .afile
            .call_type_args
            .get(&call.0)
            .and_then(|ts| ts.first())
            .and_then(|tr| self.ty_ref(tr))
        {
            return Some(t);
        }
        self.info.ty(call).array_elem()
    }

    /// The type of a delegated property — the explicit annotation if present, else inferred from the
    /// delegate's `getValue` return type. `None` if the delegate type isn't a resolvable class with a
    /// `getValue` member (e.g. an extension-operator delegate — not modeled yet).
    fn delegated_prop_type(&self, p: &ast::PropDecl) -> Option<Ty> {
        if let Some(tref) = p.ty.as_ref() {
            return Some(ty_of(self.afile, tref));
        }
        let delegate_ty = self.info.ty(p.delegate?);
        let internal = delegate_ty.obj_internal()?;
        Some(self.syms.method_of(internal, "getValue")?.ret)
    }

    /// Build a fresh `PropertyReference0Impl(<facade>::class, name, "name()<ret>", 0)` for a local
    /// delegated property's `getValue`/`setValue` call (the `KProperty` argument). The enclosing facade
    /// is the empty-internal `ClassConst` sentinel (resolved to `self.facade` at emit).
    fn make_local_propref(&mut self, ld: &LocalDelegate) -> ExprId {
        let cls = self.ir.add_expr(IrExpr::ClassConst {
            internal: String::new(),
        });
        let nm = self
            .ir
            .add_expr(IrExpr::Const(IrConst::String(ld.name.clone())));
        let sig = self.ir.add_expr(IrExpr::Const(IrConst::String(format!(
            "{}(){}",
            getter_name(&ld.name),
            ld.ret_desc
        ))));
        let flag = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        self.ir.add_expr(IrExpr::NewExternal {
            internal: "kotlin/jvm/internal/PropertyReference0Impl".to_string(),
            ctor_desc: "(Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V".to_string(),
            args: vec![cls, nm, sig, flag],
        })
    }

    /// Lower a top-level delegated property `val x: T by Del()` (pass 2). Model: two synthetic statics —
    /// `x$delegate: Del` (init = the delegate expression) and `x$kprop: KProperty` (init = an inline
    /// `PropertyReference0Impl(Facade::class, "x", "getX()<ret>", 1)`) — plus a `getX()` body that calls
    /// `x$delegate.getValue(null, x$kprop)`. Reads of `x` route to `getX()` via `computed_props` (set in
    /// pass 1c). Returns `None` (skips the file) if `getValue` can't be resolved on the delegate type.
    /// (kotlinc keeps the `KProperty`s in one `$$delegatedProperties` array; the per-prop field here is
    /// runtime-equivalent — a byte-parity nicety to revisit.)
    fn lower_delegated_top_level(&mut self, p: &ast::PropDecl) -> Option<()> {
        let delegate_expr = p.delegate?;
        let delegate_ty = self.info.ty(delegate_expr);
        let delegate_internal = delegate_ty.obj_internal()?.to_string();
        // If the delegate's `getValue` reflects on its `KProperty` parameter (`p.name`, `p.returnType`,
        // `p.toString()`, …), correctness needs the synthesized property reference to resolve through the
        // facade's `@Metadata` — which krusty doesn't emit for delegated properties. Skip rather than
        // miscompile. A `getValue` that ignores the property parameter is unaffected.
        if delegate_getvalue_uses_property(self.afile, &delegate_internal) {
            return None;
        }
        let gv = self.syms.method_of(&delegate_internal, "getValue")?;
        let prop_ty = self.delegated_prop_type(p)?;

        // getValue descriptor `(thisRef, KProperty)ret` from the resolved signature.
        let mut gv_desc = String::from("(");
        for pt in &gv.params {
            gv_desc.push_str(&pt.descriptor());
        }
        gv_desc.push(')');
        gv_desc.push_str(&gv.ret.descriptor());

        // x$delegate: Del — init = lowered delegate expression.
        let delegate_ir = ty_to_ir(delegate_ty);
        let init_d = self.lower_arg(delegate_expr, &delegate_ir)?;
        let idx_d = self.ir.statics.len() as u32;
        self.ir.statics.push(crate::ir::IrStatic {
            name: format!("{}$delegate", p.name),
            owner: None,
            ty: delegate_ir,
            init: init_d,
            is_var: false,
            is_const: false,
        });

        // x$kprop: KProperty — init = new PropertyReference0Impl(Facade::class, "x", "getX()<ret>", 1).
        let facade_cls = self.ir.add_expr(IrExpr::ClassConst {
            internal: String::new(),
        });
        let name_c = self
            .ir
            .add_expr(IrExpr::Const(IrConst::String(p.name.clone())));
        let sig_str = format!("{}(){}", getter_name(&p.name), prop_ty.descriptor());
        let sig_c = self.ir.add_expr(IrExpr::Const(IrConst::String(sig_str)));
        let flag_c = self.ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let propref = self.ir.add_expr(IrExpr::NewExternal {
            internal: "kotlin/jvm/internal/PropertyReference0Impl".to_string(),
            ctor_desc: "(Ljava/lang/Class;Ljava/lang/String;Ljava/lang/String;I)V".to_string(),
            args: vec![facade_cls, name_c, sig_c, flag_c],
        });
        let kprop_ty = ty_to_ir(Ty::obj("kotlin/reflect/KProperty"));
        let idx_p = self.ir.statics.len() as u32;
        self.ir.statics.push(crate::ir::IrStatic {
            name: format!("{}$kprop", p.name),
            owner: None,
            ty: kprop_ty,
            init: propref,
            is_var: false,
            is_const: false,
        });

        // getX(): return x$delegate.getValue(null, x$kprop).
        let get_d = self.ir.add_expr(IrExpr::GetStatic(idx_d));
        let null_a = self.ir.add_expr(IrExpr::Const(IrConst::Null));
        let get_p = self.ir.add_expr(IrExpr::GetStatic(idx_p));
        let is_iface = self
            .syms
            .class_by_internal(&delegate_internal)
            .map(|c| c.is_interface)
            .unwrap_or(false);
        let call = self.ir.add_expr(IrExpr::Call {
            callee: crate::ir::Callee::Virtual {
                owner: delegate_internal,
                name: "getValue".to_string(),
                descriptor: gv_desc,
                interface: is_iface,
            },
            dispatch_receiver: Some(get_d),
            args: vec![null_a, get_p],
        });
        let ret = self.ir.add_expr(IrExpr::Return(Some(call)));
        let body = self.ir.add_expr(IrExpr::Block {
            stmts: vec![ret],
            value: None,
        });
        let (fid, _) = self.computed_props[&p.name];
        self.ir.functions[fid as usize].body = Some(body);
        Some(())
    }

    /// Destructure a trailing lambda argument into `(parameter names, body)`, or `None` if `arg` is
    /// not a lambda.
    pub(crate) fn synth_arg_lambda(&self, arg: AstExprId) -> Option<(Vec<String>, AstExprId)> {
        match self.afile.expr(arg).clone() {
            Expr::Lambda { params, body } => Some((params, body)),
            _ => None,
        }
    }

    /// Build the fill loop shared by `IntArray(n) { i -> e }` and `Array<T>(n) { i -> e }`:
    ///   `{ val n = <size>; val a = new T[n]; var i = 0; while (i < n) { a[i] = <body[it:=i]>; i++ }; a }`
    /// `NewArray` allocates (`newarray`/`anewarray`); `kotlin/Array.set` stores (the backend picks
    /// `iastore`/`aastore`/… by the array element type).
    pub(crate) fn build_fill_array(
        &mut self,
        elem: Ty,
        size_arg: AstExprId,
        params: Vec<String>,
        body: AstExprId,
    ) -> Option<ExprId> {
        let elem_ir = ty_to_ir(elem);
        let int_ir = ty_to_ir(Ty::Int);
        // val n = <size> (evaluated once — the bound is read again in the loop)
        let size = self.lower_arg(size_arg, &int_ir)?;
        let n_v = self.fresh_value();
        let var_n = self.ir.add_expr(IrExpr::Variable {
            index: n_v,
            ty: int_ir.clone(),
            init: Some(size),
        });
        // val a = new T[n]
        let gn0 = self.ir.add_expr(IrExpr::GetValue(n_v));
        let alloc = self.ir.add_expr(IrExpr::NewArray {
            element_type: elem_ir.clone(),
            size: gn0,
        });
        let arr_v = self.fresh_value();
        let arr_ir = ty_to_ir(Ty::array(elem));
        let var_arr = self.ir.add_expr(IrExpr::Variable {
            index: arr_v,
            ty: arr_ir,
            init: Some(alloc),
        });
        // var i = 0
        let i_v = self.fresh_value();
        let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        let var_i = self.ir.add_expr(IrExpr::Variable {
            index: i_v,
            ty: int_ir,
            init: Some(zero),
        });
        // cond: i < n
        let gi = self.ir.add_expr(IrExpr::GetValue(i_v));
        let gn = self.ir.add_expr(IrExpr::GetValue(n_v));
        let cond = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Lt,
            lhs: gi,
            rhs: gn,
        });
        // body: a[i] = <lambda body with the index param bound to i>
        let pname = params.first().cloned().unwrap_or_else(|| "it".to_string());
        let depth = self.scope.len();
        self.scope.push((pname, i_v, Ty::Int));
        let body_val = self.lower_arg(body, &elem_ir);
        self.scope.truncate(depth);
        let body_val = body_val?;
        // Spill the element value into a temp before the store: a branchy body (`{ it % 2 == 0 }`)
        // records a stackmap frame, and `kotlin/Array.set` pushes the array + index *before* the value
        // — without the spill those operands would be stranded on the stack across that frame.
        let tmp_v = self.fresh_value();
        let var_tmp = self.ir.add_expr(IrExpr::Variable {
            index: tmp_v,
            ty: elem_ir,
            init: Some(body_val),
        });
        let ga = self.ir.add_expr(IrExpr::GetValue(arr_v));
        let gi2 = self.ir.add_expr(IrExpr::GetValue(i_v));
        let gtmp = self.ir.add_expr(IrExpr::GetValue(tmp_v));
        let set = self.ir.add_expr(IrExpr::Call {
            callee: Callee::External("kotlin/Array.set".to_string()),
            dispatch_receiver: Some(ga),
            args: vec![gi2, gtmp],
        });
        let wbody = self.ir.add_expr(IrExpr::Block {
            stmts: vec![var_tmp, set],
            value: None,
        });
        // update: i = i + 1
        let gi3 = self.ir.add_expr(IrExpr::GetValue(i_v));
        let one = self.ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let inc_val = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Add,
            lhs: gi3,
            rhs: one,
        });
        let inc = self.ir.add_expr(IrExpr::SetValue {
            var: i_v,
            value: inc_val,
        });
        let wh = self.ir.add_expr(IrExpr::While {
            cond,
            body: wbody,
            update: Some(inc),
            post_test: false,
            label: None,
        });
        let result = self.ir.add_expr(IrExpr::GetValue(arr_v));
        Some(self.ir.add_expr(IrExpr::Block {
            stmts: vec![var_n, var_arr, var_i, wh],
            value: Some(result),
        }))
    }

    /// Resolve a `catch` exception type name to its JVM internal name (mirrors the checker): a file
    /// class, a known class-name, or a classpath/stdlib throwable alias.
    fn catch_internal(&self, name: &str) -> Option<String> {
        self.syms
            .class_names
            .get(name)
            .cloned()
            .or_else(|| self.syms.classes.get(name).map(|c| c.internal.clone()))
    }

    /// Lower construction of a classpath (non-IR) class — `RuntimeException("x")`, `StringBuilder()`.
    /// The constructor descriptor is resolved from the classpath; arguments are coerced to its
    /// parameter types. Bails when the constructor can't be resolved or arity mismatches.
    fn lower_external_new(&mut self, internal: &str, args: &[AstExprId]) -> Option<u32> {
        // A user class defined in ANOTHER file of this compilation (found by internal name in the global
        // symbol table, but not in THIS file's IR classes) → construct via `NewCrossFile` from its
        // `ClassSig` ctor params. Only the simple exact-arity primary-ctor case (defaults/secondary bail).
        if let Some(cs) = self.syms.class_by_internal(internal) {
            // A sibling-file value class (unboxed — no instance `<init>`), annotation (an interface +
            // synthetic impl), or inner class (needs an outer instance) isn't constructed via a plain
            // cross-file `new`; bail those (the test skips rather than miscompiles).
            if cs.value_field.is_some() || cs.is_annotation || cs.inner_of.is_some() {
                return None;
            }
            if !cs.is_interface && cs.ctor_params.len() == args.len() {
                let params: Vec<Ty> = cs.ctor_params.iter().map(|t| ty_to_ir(*t)).collect();
                let mut a = Vec::new();
                for (arg, pty) in args.iter().zip(&params) {
                    a.push(self.lower_arg(*arg, pty)?);
                }
                return Some(self.ir.add_expr(IrExpr::NewCrossFile {
                    internal: internal.to_string(),
                    params,
                    args: a,
                }));
            }
            return None; // sibling-file user class but arity/defaults/secondary not modeled cross-file
        }
        let arg_tys: Vec<Ty> = args.iter().map(|a| self.info.ty(*a)).collect();
        let ctor =
            crate::call_resolver::resolve_constructor(&*self.syms.libraries, internal, &arg_tys)?;
        if ctor.params.len() != args.len() {
            return None;
        }
        let mut a = Vec::new();
        for (arg, pty) in args.iter().zip(&ctor.params) {
            let pty = ty_to_ir(*pty);
            a.push(self.lower_arg(*arg, &pty)?);
        }
        Some(self.ir.add_expr(IrExpr::NewExternal {
            internal: internal.to_string(),
            ctor_desc: ctor.descriptor,
            args: a,
        }))
    }

    /// Whether `internal` names a `@JvmInline value`/inline class (unboxed representation) — a file
    /// class in this compilation or a classpath one.
    fn is_value_class(&self, internal: &str) -> bool {
        self.syms
            .class_by_internal(internal)
            .is_some_and(|cs| cs.value_field.is_some())
            || self
                .syms
                .libraries
                .resolve_type(internal)
                .is_some_and(|t| t.value_underlying.is_some())
    }

    /// Whether top-level function `fname` declares a bare type-parameter return (`fun <T> f(): T`).
    fn callee_returns_typaram(&self, fname: &str) -> bool {
        self.afile.decls.iter().any(|&d| {
            matches!(self.afile.decl(d), Decl::Fun(f)
                if f.name == fname
                    && !f.type_params.is_empty()
                    && f.ret.as_ref().is_some_and(|r| f.type_params.contains(&r.name)))
        })
    }

    /// A call to a type-parameter-returning function whose erased `Object` result needs a coercion
    /// krusty doesn't model — skip the file rather than miscompile. The result of `f<Unit>()` must
    /// become `Unit.INSTANCE` (not the raw object); inside an `inline` expansion an erased result
    /// feeding a boxed/`Int?` slot mis-frames the verifier. Both reach here as a tail-returned `T`.
    fn erased_generic_call_unmodeled(&self, e: AstExprId, fname: &str) -> bool {
        if !self.callee_returns_typaram(fname) {
            return false;
        }
        let unit_targ = self
            .afile
            .call_type_args
            .get(&e.0)
            .and_then(|ts| ts.first())
            .is_some_and(|r| matches!(r.name.as_str(), "Unit" | "Nothing"));
        unit_targ || !self.inline_active.is_empty()
    }

    /// A call whose declared return is a type parameter erases to `Object`; the checker recovers the
    /// real static type at the call site (a primitive type argument arrives here as its boxed wrapper,
    /// `Integer` — the erased slot's actual reference representation). When that type is a more specific
    /// reference, insert the `checkcast` kotlinc emits so a member access on the result resolves and
    /// verifies (`asSeq<String>(x).length`); the wrapper is unboxed to a primitive only at a use site
    /// that needs it, by the normal coercion path — never eagerly here (an `Int?` consumer keeps it
    /// boxed, and `null` must not be unboxed).
    fn coerce_erased_call_result(&mut self, e: AstExprId, call: u32, ret: &Ty) -> u32 {
        let erased = ret.non_null().obj_internal() == Some("kotlin/Any");
        if !erased {
            return call;
        }
        let st = self.info.ty(e);
        if st.is_reference() && st != Ty::obj("kotlin/Any") && st != Ty::Null {
            return self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::Cast,
                arg: call,
                type_operand: ty_to_ir(st),
            });
        }
        call
    }

    /// Lower a lambda literal `{ a, b -> body }` to an `IrExpr::Lambda` (emitted as `invokedynamic` +
    /// `LambdaMetafactory`). The body becomes a synthesized static method `<enclosing>$lambda$<n>`
    /// with the lambda's (real, from the checker) parameter types. Non-capturing only: a body that
    /// reads any enclosing local/parameter, or a lambda inside a class method (which could capture
    /// `this`/fields), bails (`None`) rather than miscompile.
    /// Append a loop body's statements to `out`: a block's statements (plus its trailing expression),
    /// or a single non-block body expression (`for (x in xs) f(x)` — no braces). Returns `None` if any
    /// statement can't be lowered.
    /// A statement that transfers control away unconditionally (`return`/`break`/`continue`, or an
    /// expression of type `Nothing` — a `throw` or a call that never returns). Code after it in the same
    /// block is unreachable; kotlinc drops it, and emitting it would leave the verifier without the
    /// stackmap frame a (dead) branch target needs (`VerifyError: Expecting a stack map frame`).
    fn stmt_diverges(&self, s: crate::ast::StmtId) -> bool {
        match self.afile.stmt(s) {
            Stmt::Return(..) | Stmt::Break(_) | Stmt::Continue(_) => true,
            Stmt::Expr(e) => self.info.ty(*e) == Ty::Nothing,
            _ => false,
        }
    }

    /// The underlying type of a `@JvmInline value class` (`Z(val v: Int)` → `Int`), or `None` for an
    /// ordinary type. A value-class value is represented unboxed as this type.
    fn value_class_underlying(&self, t: Ty) -> Option<Ty> {
        let internal = t.obj_internal()?;
        self.syms
            .classes
            .get(internal)
            .and_then(|c| c.value_field.as_ref())
            .map(|(_, u)| *u)
    }

    fn append_body_stmts(&mut self, body: AstExprId, out: &mut Vec<u32>) -> Option<()> {
        match self.afile.expr(body).clone() {
            Expr::Block { stmts, trailing } => {
                let mut diverged = false;
                for s in stmts {
                    self.append_stmt(s, out)?;
                    if self.stmt_diverges(s) {
                        diverged = true;
                        break;
                    }
                }
                if !diverged {
                    if let Some(t) = trailing {
                        out.push(self.expr(t)?);
                    }
                }
            }
            _ => out.push(self.expr(body)?),
        }
        Some(())
    }

    /// Lower one statement into `out`. A destructuring declaration splices its bindings directly so the
    /// component locals live in the enclosing scope (a nested `Block` would scope them away at emit).
    fn append_stmt(&mut self, s: crate::ast::StmtId, out: &mut Vec<u32>) -> Option<()> {
        if let Stmt::Destructure { entries, init } = self.afile.stmt(s).clone() {
            return self.lower_destructure(&entries, init, out);
        }
        out.push(self.stmt(s)?);
        Some(())
    }

    /// Lower `val (a, b, …) = init` into `out`: a temp bound to `init`, then one local per component
    /// (`a = temp.component1()`, …). Each component is a user-class `componentN` (data class) or a
    /// library member (`Pair`, `Map.Entry`); a generic component's erased return coerces to its element.
    /// Lower a single-spread call `foo(*a)` to a top-level `vararg` function: pass the array through
    /// `Arrays.copyOf(a, a.size)` + `checkcast`, exactly as kotlinc does, instead of packing the array
    /// as one element. Returns `None` (→ the file skips) for any shape this doesn't handle: more than one
    /// argument, a non-spread sole argument, a non-`Name` (non-reusable) spread expression, a primitive
    /// element type, or a callee that isn't a single-`vararg`-parameter top-level function in this file.
    fn lower_single_spread_call(
        &mut self,
        callee: AstExprId,
        args: &[AstExprId],
    ) -> Option<ExprId> {
        if args.len() != 1 || !self.afile.is_spread_arg(args[0]) {
            return None; // only `foo(*a)` (one spread arg, no fixed/mixed args)
        }
        let spread = args[0];
        // kotlinc loads the spread twice (`aload a; aload a; arraylength`); only a simple reusable name
        // (a local/parameter) is safe to lower twice — a complex expression would need a temp we don't
        // emit, so skip it.
        if !matches!(self.afile.expr(spread), ast::Expr::Name(_)) {
            return None;
        }
        let ast::Expr::Name(fname) = self.afile.expr(callee).clone() else {
            return None;
        };
        let decl = self.top_fun_decl(&fname)?;
        if decl.params.len() != 1 || !decl.params[0].is_vararg {
            return None;
        }
        let elem = ty_of(self.afile, &decl.params[0].ty);
        // A genuine JVM primitive element (`vararg xs: Int` → `IntArray`/`[I`) uses the matching
        // `Arrays.copyOf(int[], int): int[]` overload and needs NO checkcast (the result is already the
        // exact array type). Unsigned `UInt`/`ULong` varargs are a value-class array (`UIntArray`) with a
        // different copy path — leave those (skip) rather than miscompile.
        let prim = matches!(
            elem,
            Ty::Int
                | Ty::Long
                | Ty::Byte
                | Ty::Short
                | Ty::Char
                | Ty::Boolean
                | Ty::Float
                | Ty::Double
        );
        if !elem.is_reference() && !prim {
            return None;
        }
        let array_ty = Ty::array(elem);
        let key: String = array_ty.descriptor();
        let fid = *self.fun_ids.get(&(fname.clone(), key))?;
        let array_ir = ty_to_ir(array_ty);

        // `Arrays.copyOf(a, a.size)` — the primitive overload returns the exact array type (no cast); the
        // reference overload returns `Object[]` and needs a `checkcast` to the element array type.
        let a0 = self.lower_arg(spread, &array_ir)?;
        let a1 = self.lower_arg(spread, &array_ir)?;
        let size = self.ir.add_expr(IrExpr::Call {
            callee: Callee::External("kotlin/Array.size".to_string()),
            dispatch_receiver: Some(a1),
            args: vec![],
        });
        let copyof_desc = if prim {
            let p = elem.descriptor();
            format!("([{p}I)[{p}")
        } else {
            "([Ljava/lang/Object;I)[Ljava/lang/Object;".to_string()
        };
        let copy = self.ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: "java/util/Arrays".to_string(),
                name: "copyOf".to_string(),
                descriptor: copyof_desc,
                inline: false,
                must_inline: false,
            },
            dispatch_receiver: None,
            args: vec![a0, size],
        });
        let arg = if prim {
            copy // primitive `copyOf` already returns `[<prim>` — no cast
        } else {
            self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::Cast,
                arg: copy,
                type_operand: array_ir,
            })
        };
        Some(self.ir.add_expr(IrExpr::Call {
            callee: Callee::Local(fid),
            dispatch_receiver: None,
            args: vec![arg],
        }))
    }

    fn lower_destructure(
        &mut self,
        entries: &[(String, bool)],
        init: AstExprId,
        out: &mut Vec<u32>,
    ) -> Option<()> {
        let it_ty = self.info.ty(init);
        let internal = it_ty.obj_internal()?.to_string();
        let init_v = self.expr(init)?;
        let tmp = self.fresh_value();
        out.push(self.ir.add_expr(IrExpr::Variable {
            index: tmp,
            ty: ty_to_ir(it_ty),
            init: Some(init_v),
        }));
        for (idx, (name, _)) in entries.iter().enumerate() {
            if name == "_" {
                continue;
            }
            let comp = format!("component{}", idx + 1);
            let recv = self.ir.add_expr(IrExpr::GetValue(tmp));
            let (call, log_ty) = if let Some((class, index, _, _)) =
                self.resolve_method(&internal, &comp)
            {
                let ret = self
                    .syms
                    .method_of(&internal, &comp)
                    .map(|s| s.ret)
                    .unwrap_or_else(|| Ty::obj("kotlin/Any"));
                (
                    self.ir.add_expr(IrExpr::MethodCall {
                        class,
                        index,
                        receiver: recv,
                        args: vec![],
                    }),
                    ret,
                )
            } else if self.class_of(it_ty).is_none()
                && self.syms.class_by_internal(&internal).is_some_and(|cs| {
                    cs.value_field.is_none()
                        && cs.methods.get(&comp).is_some_and(|s| s.params.is_empty())
                })
            {
                // `componentN` of a class defined in ANOTHER file of this compilation → `CrossFileVirtual`
                // (mirrors a cross-file instance call), so a destructure of a sibling-file value resolves.
                let (ret, interface) = {
                    let cs = self.syms.class_by_internal(&internal).unwrap();
                    (cs.methods[&comp].ret, cs.is_interface)
                };
                let c = self.ir.add_expr(IrExpr::Call {
                    callee: Callee::CrossFileVirtual {
                        owner: internal.clone(),
                        name: comp.clone(),
                        params: vec![],
                        ret: ty_to_ir(ret),
                        interface,
                    },
                    dispatch_receiver: Some(recv),
                    args: vec![],
                });
                (c, ret)
            } else if let Some(m) =
                crate::call_resolver::resolve_instance(&*self.syms.libraries, &internal, &comp, &[])
            {
                let is_iface = self
                    .syms
                    .libraries
                    .resolve_type(&internal)
                    .map_or(false, |t| t.is_interface());
                let log = self
                    .syms
                    .libraries
                    .member_return(it_ty, &comp, &[])
                    .unwrap_or(m.ret);
                let c = self.ir.add_expr(IrExpr::Call {
                    callee: Callee::Virtual {
                        owner: internal.clone(),
                        name: comp.clone(),
                        descriptor: m.descriptor.clone(),
                        interface: is_iface,
                    },
                    dispatch_receiver: Some(recv),
                    args: vec![],
                });
                (self.coerce_erased(c, log, m.ret), log)
            } else if let Some(c) =
                self.syms
                    .libraries
                    .resolve_callable(&comp, Some(it_ty), &[], &[])
            {
                // `List.component1()` etc. are stdlib extensions: `invokestatic facade.componentN(recv)`.
                let call = self.ir.add_expr(IrExpr::Call {
                    callee: Callee::Static {
                        owner: c.owner,
                        name: c.name,
                        descriptor: c.descriptor,
                        inline: c.is_inline,
                        must_inline: false,
                    },
                    dispatch_receiver: None,
                    args: vec![recv],
                });
                (self.coerce_erased(call, c.ret, c.physical_ret), c.ret)
            } else {
                // An indexable type: `componentN` is the inline `get(N-1)`.
                let m = crate::call_resolver::resolve_instance(
                    &*self.syms.libraries,
                    &internal,
                    "get",
                    &[Ty::Int],
                )?;
                let is_iface = self
                    .syms
                    .libraries
                    .resolve_type(&internal)
                    .map_or(false, |t| t.is_interface());
                let log = self
                    .syms
                    .libraries
                    .member_return(it_ty, "get", &[Ty::Int])
                    .unwrap_or(m.ret);
                let i = self.ir.add_expr(IrExpr::Const(IrConst::Int(idx as i32)));
                let c = self.ir.add_expr(IrExpr::Call {
                    callee: Callee::Virtual {
                        owner: internal.clone(),
                        name: "get".to_string(),
                        descriptor: m.descriptor.clone(),
                        interface: is_iface,
                    },
                    dispatch_receiver: Some(recv),
                    args: vec![i],
                });
                (self.coerce_erased(c, log, m.ret), log)
            };
            // A `var` component captured AND written by a closure is boxed into a `Ref$XxxRef`, exactly
            // like a plain mutable local (see the `Stmt::Local` path) — without this the closure mutates
            // a private copy and the outer read misses it (e.g. `var [a,b]=A(); { a=3 }()`).
            if self.info.boxed_vars.contains(name) {
                let elem_ty = self.value_class_underlying(log_ty).unwrap_or(log_ty);
                let elem = ty_to_ir(elem_ty);
                let holder = self.fresh_value();
                let holder_ty = Ty::obj(ref_holder_internal(elem_ty));
                self.scope.push((name.clone(), holder, holder_ty));
                self.boxed_elem.insert(name.clone(), elem_ty);
                let new_ref = self.ir.add_expr(IrExpr::RefNew { elem, init: call });
                out.push(self.ir.add_expr(IrExpr::Variable {
                    index: holder,
                    ty: ty_to_ir(holder_ty),
                    init: Some(new_ref),
                }));
            } else {
                let v = self.fresh_value();
                self.scope.push((name.clone(), v, log_ty));
                out.push(self.ir.add_expr(IrExpr::Variable {
                    index: v,
                    ty: ty_to_ir(log_ty),
                    init: Some(call),
                }));
            }
        }
        Some(())
    }

    fn lower_lambda(&mut self, e: AstExprId, params: &[String], body: AstExprId) -> Option<u32> {
        self.lower_lambda_sam(e, params, body, None)
    }

    /// `sam`: `(interface internal name, abstract-method name, method returns void)`. The void flag
    /// distinguishes a SAM whose method is `()V` (`Runnable.run`) — the impl runs the body for effect
    /// and returns void — from a `Unit`-typed-but-`Object`-returning target (`FunctionN.invoke`).
    fn lower_lambda_sam(
        &mut self,
        e: AstExprId,
        params: &[String],
        body: AstExprId,
        sam: Option<(String, String, bool)>,
    ) -> Option<u32> {
        let Ty::Fun(sig) = self.info.ty(e) else {
            return None;
        };
        // A `suspend` lambda needs a `SuspendLambda` subclass with a state-machine `invokeSuspend`, not
        // krusty's `invokedynamic`/`LambdaMetafactory` path — not yet modeled. Bail (skip the file)
        // rather than emit a plain `Function0` where a `Function1<Continuation,…>` is expected.
        if sig.suspend {
            return None;
        }
        let arity = sig.params.len();
        // A lambda inside a class method could capture `this`/fields — not modeled yet.
        if self.cur_class.is_some() {
            return None;
        }
        // A `Nothing`-returning lambda whose body is an unconditional NON-LOCAL `return` (`f { return … }`)
        // is handled by the diverging path below: it only ever splices (the impl method is marked
        // inline-only and not emitted), so its `return` becomes the enclosing fn's return, and the splicer
        // synthesizes the stack-map frame the host's now-unreachable post-invoke continuation needs. But a
        // Nothing lambda WITHOUT a bare return (a `throw`, or only a `return@label` — which can materialize
        // as a real closure) isn't modeled that way — bail (skip, never miscompile) as before.
        if sig.ret == Ty::Nothing && !body_has_bare_return(self.afile, body) {
            return None;
        }
        // Bound parameter names: explicit, or the implicit single `it` for a unary lambda.
        let bind_names: Vec<String> = if !params.is_empty() {
            params.to_vec()
        } else if arity == 1 {
            vec!["it".to_string()]
        } else if arity == 0 {
            vec![]
        } else {
            return None;
        };
        if bind_names.len() != arity {
            return None;
        }
        // Captured free variables: enclosing locals/parameters the body reads but the lambda doesn't
        // bind. Each is passed into the impl method as a leading parameter and bound into the closure
        // at the `invokedynamic` call site (kotlinc's capture convention). Dedup by name, keeping the
        // innermost binding (the last in the scope stack).
        let mut captures: Vec<(String, u32, Ty)> = Vec::new();
        for (name, v, ty) in self.scope.iter().rev() {
            if !bind_names.contains(name)
                && !captures.iter().any(|(n, _, _)| n == name)
                && crate::resolve::expr_uses_name_pub(self.afile, body, name)
            {
                captures.push((name.clone(), *v, *ty));
            }
        }
        captures.reverse();
        // The capture values are read in the *enclosing* scope before it's swapped out.
        let capture_vals: Vec<u32> = captures
            .iter()
            .map(|(_, v, _)| self.ir.add_expr(IrExpr::GetValue(*v)))
            .collect();
        // Lower the body in a fresh value-numbering scope: captured params first (values `0..n_cap`),
        // then the lambda's own parameters.
        let saved_scope = std::mem::take(&mut self.scope);
        let saved_next = self.next_value;
        self.next_value = 0;
        for (name, _, ty) in &captures {
            let v = self.fresh_value();
            self.scope.push((name.clone(), v, *ty));
        }
        for (name, pty) in bind_names.iter().zip(sig.params.iter()) {
            let v = self.fresh_value();
            self.scope.push((name.clone(), v, *pty));
        }
        // The closure method returns the `kotlin/Unit` SINGLETON (a reference) when the lambda is
        // `() -> Unit` and not a `void` SAM (`Runnable`) nor a diverging body — so a `return@lambda`
        // inside it (a LOCAL return from the closure method) must `areturn Unit.INSTANCE`, not `return`.
        let sam_void_pre = matches!(&sam, Some((_, _, true)));
        let returns_unit_ref =
            sig.ret == Ty::Unit && !sam_void_pre && self.info.ty(body) != Ty::Nothing;
        let saved_unit_ref =
            std::mem::replace(&mut self.cur_method_returns_unit_ref, returns_unit_ref);
        let ve = self.expr(body);
        self.cur_method_returns_unit_ref = saved_unit_ref;
        self.scope = saved_scope;
        self.next_value = saved_next;
        let ve = ve?;
        let diverges = self.info.ty(body) == Ty::Nothing;
        // The SAM's `invoke` returns `Object`, so the impl method returns a reference. A `Unit` lambda
        // runs its body for effect then returns the `kotlin/Unit` singleton; a value lambda returns its
        // (boxed) body value; a diverging body falls through to its own `throw`/`return`.
        let sam_void = matches!(&sam, Some((_, _, true)));
        // `block` is the impl-method body (with a synthetic `return`); `inline_body` is the equivalent
        // *value-producing* form (no synthetic return) the bytecode inliner emits directly — so a
        // user `return` in the lambda becomes a real return from the *enclosing* method (a correct
        // non-local return), not the lambda.
        let (ret_ty, block, inline_body) = if diverges {
            let b = self.ir.add_expr(IrExpr::Block {
                stmts: vec![ve],
                value: None,
            });
            (ty_to_ir(sig.ret), b, ve)
        } else if sam_void {
            // The SAM method returns `void` (`run()V`): run the body for effect, no return value.
            let b = self.ir.add_expr(IrExpr::Block {
                stmts: vec![ve],
                value: None,
            });
            (ty_to_ir(Ty::Unit), b, ve)
        } else if sig.ret == Ty::Unit {
            let unit = self.ir.add_expr(IrExpr::UnitInstance);
            let ret = self.ir.add_expr(IrExpr::Return(Some(unit)));
            let b = self.ir.add_expr(IrExpr::Block {
                stmts: vec![ve, ret],
                value: None,
            });
            let inline_b = self.ir.add_expr(IrExpr::Block {
                stmts: vec![ve],
                value: Some(unit),
            });
            (ty_to_ir(Ty::obj("kotlin/Unit")), b, inline_b)
        } else {
            let ret = self.ir.add_expr(IrExpr::Return(Some(ve)));
            let b = self.ir.add_expr(IrExpr::Block {
                stmts: vec![ret],
                value: None,
            });
            (ty_to_ir(sig.ret), b, ve)
        };
        let impl_name = format!("{}$lambda${}", self.cur_fn_name, self.lambda_seq);
        self.lambda_seq += 1;
        // Impl parameters: captured variables first, then the lambda's own parameters.
        let mut params_ir: Vec<Ty> = captures.iter().map(|(_, _, t)| ty_to_ir(*t)).collect();
        params_ir.extend(sig.params.iter().map(|t| ty_to_ir(*t)));
        let fid = self.ir.add_fun(IrFunction {
            name: impl_name,
            params: params_ir,
            ret: ret_ty,
            body: Some(block),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        // A BARE `return` in a lambda body is a NON-LOCAL return (from the enclosing function) — valid only
        // when the lambda is spliced inline, never as a standalone closure method (the `areturn` would carry
        // the enclosing fn's return type, not the lambda's). Mark the impl method inline-only so the backend
        // skips it (an invalid, dead method); the splice uses `inline_body`. A LABELED `return@x` is a LOCAL
        // return from the lambda (a normal `return` in the closure method), so it stays emittable.
        if body_has_bare_return(self.afile, body) {
            self.ir.inline_only_fns.insert(fid);
        }
        Some(self.ir.add_expr(IrExpr::Lambda {
            impl_fn: fid,
            arity: arity as u8,
            captures: capture_vals,
            sam: sam.map(|(i, m, _)| (i, m)),
            inline_body: Some(inline_body),
        }))
    }

    /// Lower a `suspend` lambda literal to a concrete `kotlin/coroutines/jvm/internal/SuspendLambda`
    /// subclass (kotlinc's representation), returning a `New` of that class with the captured values and
    /// a `null` completion. Arity 0 with no INTERNAL suspension for now: the class implements
    /// `Function{n+1}`, captures the free variables as fields (set in `<init>`, copied into the fresh
    /// instance `invoke` builds), and carries `invokeSuspend(Object)` (the body, result boxed) plus the
    /// erased `invoke(Object)` (`new This(captures.., (Continuation)arg).invokeSuspend(Unit)`).
    fn lower_suspend_lambda(
        &mut self,
        body: AstExprId,
        params: &[Ty],
        bind_names: Vec<String>,
    ) -> Option<u32> {
        let arity = params.len();
        if self.cur_class.is_some() {
            return None;
        }
        // Captured free variables: enclosing locals/parameters the body reads but the lambda doesn't
        // bind as one of its own parameters.
        let mut captures: Vec<(String, u32, Ty)> = Vec::new();
        for (name, v, ty) in self.scope.iter().rev() {
            if !bind_names.contains(name)
                && !captures.iter().any(|(n, _, _)| n == name)
                && crate::resolve::expr_uses_name_pub(self.afile, body, name)
            {
                captures.push((name.clone(), *v, *ty));
            }
        }
        captures.reverse();
        let n_cap = captures.len() as u32;
        // Own parameters are modeled only for a LEAF lambda (no captures, no internal suspension) for
        // now — a param + capture/suspension combination needs the general lambda-mode machine.
        if arity > 0 && n_cap > 0 {
            return None;
        }
        // Does the lambda body call a `suspend` function? (AST-level, like the file's suspend gate.)
        // If so its `invokeSuspend` is a state machine with the lambda instance as the continuation —
        // for now only a single TAIL suspend call (`{ foo() }`) with no captures is modeled; anything
        // else bails (skip the file) rather than miscompile the continuation threading.
        let susp_names: std::collections::HashSet<String> = self
            .afile
            .decls
            .iter()
            .filter_map(|&d| match self.afile.decl(d) {
                ast::Decl::Fun(f) if f.is_suspend => Some(f.name.clone()),
                _ => None,
            })
            .collect();
        let call_names_cell = std::cell::RefCell::new(Vec::new());
        collect_call_names(self.afile, body, &call_names_cell);
        let call_names = call_names_cell.into_inner();
        let body_suspends = call_names
            .iter()
            .any(|n| susp_names.contains(n) || self.resolver().toplevel_is_suspend(n));
        let jvm_arity = arity + 1; // + the trailing continuation
        let internal = class_internal(
            self.afile,
            &format!("{}$suspend${}", self.cur_fn_name, self.lambda_seq),
        );
        self.lambda_seq += 1;
        let cont_ir = ty_to_ir(Ty::obj("kotlin/coroutines/Continuation"));
        let object_ir = ty_to_ir(Ty::obj("kotlin/Any"));

        // Fields: one per captured variable. `<init>(cap0.., Continuation completion)` stores each
        // capture, then `super(jvm_arity, completion)`. Ctor value-indices: this=0, cap_i=1+i,
        // completion=1+n_cap.
        let mut fields: Vec<(String, Ty)> = captures
            .iter()
            .map(|(name, _, ty)| (name.clone(), ty_to_ir(*ty)))
            .collect();
        // Own parameters become fields after the captures — set by `create(value.., completion)` (NOT
        // the constructor / creation site). `param_field_base` is their first field index.
        let param_field_base = n_cap;
        for (name, ty) in bind_names.iter().zip(params.iter()) {
            fields.push((name.clone(), ty_to_ir(*ty)));
        }
        // A suspending lambda's state-machine fields go after the captures/params. For the SINGLE-
        // suspension inline machine a `label` field is appended below (`label_idx`); the general
        // (multi-suspension) machine has its `result`/`label`/spilled fields added by the coroutine pass.
        let label_idx = n_cap + arity as u32;
        let ctor_args: Vec<(Ty, bool)> = captures
            .iter()
            .map(|(_, _, ty)| (ty_to_ir(*ty), false))
            .chain(std::iter::once((cont_ir.clone(), false)))
            .collect();
        let init_stores: Vec<u32> = (0..n_cap)
            .map(|i| {
                let this = self.ir.add_expr(IrExpr::GetValue(0));
                let val = self.ir.add_expr(IrExpr::GetValue(1 + i));
                self.ir.add_expr(IrExpr::SetField {
                    receiver: this,
                    class: 0, // patched after add_class (class_id known then)
                    index: i,
                    value: val,
                })
            })
            .collect();
        let init_body = (!init_stores.is_empty()).then(|| {
            self.ir.add_expr(IrExpr::Block {
                stmts: init_stores.clone(),
                value: None,
            })
        });
        let arity_const = self
            .ir
            .add_expr(IrExpr::Const(IrConst::Int(jvm_arity as i32)));
        let completion_get = self.ir.add_expr(IrExpr::GetValue(1 + n_cap));
        // Captures and own parameters are `final`; only the `label` state cursor is mutable. All fields
        // are non-private (read/written by the coroutine state machine cross-class).
        let ir_fields: Vec<IrField> = fields
            .into_iter()
            .enumerate()
            .map(|(i, (name, ty))| IrField {
                is_final: i < (n_cap + arity as u32) as usize,
                is_private: false,
                ..IrField::new(name, ty)
            })
            .collect();
        let class = IrClass {
            fq_name: internal.clone(),
            serial_names: Vec::new(),
            custom_serializer: None,
            field_serializers: Vec::new(),
            contextual_fields: Vec::new(),
            is_value: false,
            type_param_bounds: vec![],
            type_params: Vec::new(),
            supertypes: vec![],
            fields: ir_fields,
            ctor_param_count: 0,
            ctor_args,
            init_body,
            explicit_param_stores: false,
            methods: vec![],
            is_interface: false,
            is_annotation: false,
            annotation_impl_of: None,

            is_sealed: false,
            is_abstract: false,
            superclass: "kotlin/coroutines/jvm/internal/SuspendLambda".to_string(),
            super_args: vec![arity_const, completion_get],
            enum_entries: vec![],
            enum_entry_subclass: vec![],
            enum_entry_of: None,
            prop_ref: None,
            func_ref: None,
            bridges: vec![],
            interfaces: vec![format!("kotlin/jvm/functions/Function{jvm_arity}")],
            is_object: false,
            ctor_param_checks: vec![],
            is_companion: false,
            companion_class: None,
            secondary_ctors: vec![],
            has_primary_ctor: true,
        };
        let class_id = self.ir.add_class(class);
        // Patch the `<init>` field stores with the now-known class id.
        for &s in &init_stores {
            if let IrExpr::SetField { class, .. } = &mut self.ir.exprs[s as usize] {
                *class = class_id;
            }
        }

        // invokeSuspend(Object result): either the LEAF form (throwOnFailure; load captures; return
        // box(<body>)) or, when the body suspends, a TAIL state machine with `this` as the continuation.
        let throw_on_failure = |s: &mut Self, value_idx: u32| {
            let v = s.ir.add_expr(IrExpr::GetValue(value_idx));
            s.ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: "kotlin/ResultKt".to_string(),
                    name: "throwOnFailure".to_string(),
                    descriptor: "(Ljava/lang/Object;)V".to_string(),
                    inline: false,
                    must_inline: false,
                },
                dispatch_receiver: None,
                args: vec![v],
            })
        };
        // Set when the body needs the general (multi-suspension / control-flow) lambda-mode machine,
        // built by the coroutine pass from the plain `invokeSuspend` body.
        let mut needs_pass_sm = false;
        let inv_susp_body = if body_suspends {
            // Single-suspension lambda (`{ foo() }` or `{ val a = foo(); <tail> }`), no captures (gated
            // above). The continuation is `this`: thread it into the call, dispatch on `this.label`.
            // invokeSuspend value-indices are `this`=0, `result`=1, body locals from 2 — so lower the
            // body with `next_value` reset to 2 (saved/restored so the enclosing method is unaffected).
            let saved_next_sm = self.next_value;
            self.next_value = 2;
            // Bind captures to locals 2..2+n_cap (the coroutine pass reloads them from their fields at
            // each invokeSuspend entry). Only the GENERAL machine is used when there are captures; the
            // inline hand-roll requires `n_cap == 0`.
            let saved_scope_sm = std::mem::take(&mut self.scope);
            for (name, _, ty) in &captures {
                let lv = self.fresh_value();
                self.scope.push((name.clone(), lv, *ty));
            }
            // Own parameters bind to locals after the captures (their fields are reloaded by the pass).
            for (name, pty) in bind_names.iter().zip(params.iter()) {
                let lv = self.fresh_value();
                self.scope.push((name.clone(), lv, *pty));
            }
            let body_val = self.expr(body)?;
            self.scope = saved_scope_sm;
            // Extract the suspend `call`, an optional `bound` local (`val a = <call>`) and `tail_expr`
            // (the expression computed after the binding). Shapes: `{ foo() }` (call, no bound), and
            // `{ val a = foo(); <tail> }` (call = the binding init, bound = `a`, tail = the value).
            let (tail, bound): (u32, Option<(u32, Ty)>) = match &self.ir.exprs[body_val as usize] {
                IrExpr::Block {
                    stmts,
                    value: Some(v),
                } if stmts.is_empty() => (*v, None),
                IrExpr::Block {
                    stmts,
                    value: Some(v),
                } if stmts.len() == 1 => {
                    if let IrExpr::Variable {
                        index,
                        ty,
                        init: Some(init),
                    } = &self.ir.exprs[stmts[0] as usize]
                    {
                        (*init, Some((*index, ty.clone())))
                    } else {
                        return None;
                    }
                }
                _ => (body_val, None),
            };
            let tail_expr = bound
                .as_ref()
                .map(|_| match &self.ir.exprs[body_val as usize] {
                    IrExpr::Block { value: Some(v), .. } => *v,
                    _ => body_val,
                });
            let is_susp = matches!(&self.ir.exprs[tail as usize],
                IrExpr::Call { callee: Callee::Local(fid), .. } if self.ir.suspend_funs.contains(fid))
                || self.ir.suspend_calls.contains_key(&tail);
            // Only a SINGLE suspension is modeled by this two-state form. If the body calls a suspend fn
            // more than once (a second suspension in the tail or elsewhere), bail for the general machine.
            let n_susp = {
                let c = std::cell::RefCell::new(Vec::new());
                collect_call_names(self.afile, body, &c);
                c.into_inner()
                    .iter()
                    .filter(|n| susp_names.contains(*n) || self.resolver().toplevel_is_suspend(n))
                    .count()
            };
            // A clean SINGLE tail/bound suspension uses the inline two-state machine here; anything else
            // (a second suspension, control flow) gets the general lambda-mode machine from the pass.
            let handroll = is_susp && n_susp == 1 && n_cap == 0 && arity == 0;
            if !handroll {
                needs_pass_sm = true;
                // `tmp` goes ABOVE the body's locals (next_value still points past them here).
                let tmp_idx = self.next_value;
                self.next_value = saved_next_sm;
                let body_ty = ty_to_ir(self.info.ty(body));
                let (mut b_stmts, b_val) = match &self.ir.exprs[body_val as usize] {
                    IrExpr::Block {
                        stmts,
                        value: Some(v),
                    } => (stmts.clone(), *v),
                    _ => (Vec::new(), body_val),
                };
                // Bind the body value to a temp (`val tmp = <value>; return box(tmp)`) so a CONDITIONAL
                // suspension in the value (`if (c) foo() else 7`) surfaces as a `Variable{init: When}`
                // the flattener's `stmt_cond_suspension` handles — not a raw `return box(When)`.
                b_stmts.push(self.ir.add_expr(IrExpr::Variable {
                    index: tmp_idx,
                    ty: body_ty,
                    init: Some(b_val),
                }));
                let tmpg = self.ir.add_expr(IrExpr::GetValue(tmp_idx));
                let boxed = self.ir.add_expr(IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    arg: tmpg,
                    type_operand: object_ir.clone(),
                });
                b_stmts.push(self.ir.add_expr(IrExpr::Return(Some(boxed))));
                self.ir.add_expr(IrExpr::Block {
                    stmts: b_stmts,
                    value: None,
                })
            } else {
                // The inline machine's `label` field is appended to the lambda class now.
                {
                    let cls = &mut self.ir.classes[class_id as usize];
                    cls.fields.push(IrField {
                        is_private: false,
                        ..IrField::new("label".to_string(), ty_to_ir(Ty::Int))
                    });
                }
                // Thread `this` (as Continuation) as the callee's trailing argument.
                let this_for_cont = self.ir.add_expr(IrExpr::GetValue(0));
                let this_cont = self.ir.add_expr(IrExpr::TypeOp {
                    op: IrTypeOp::Cast,
                    arg: this_for_cont,
                    type_operand: cont_ir.clone(),
                });
                // For a same-file callee (`Local`) the CPS descriptor comes from the callee's
                // pass-rewritten signature; a classpath (`Static`) / sibling (`CrossFile`) callee is
                // resolved by its LOGICAL signature, so rewrite it to the physical CPS form here (append the
                // `Continuation` parameter, erase the return to `Object`).
                let cont_param_ty = cont_ir.clone();
                let object_ret_ty = object_ir.clone();
                match &mut self.ir.exprs[tail as usize] {
                    IrExpr::Call { args, callee, .. } => {
                        match callee {
                            Callee::Static { descriptor, .. } => {
                                let close = descriptor.rfind(')').unwrap_or(descriptor.len());
                                *descriptor = format!(
                                    "{}Lkotlin/coroutines/Continuation;)Ljava/lang/Object;",
                                    &descriptor[..close]
                                );
                            }
                            Callee::CrossFile { params, ret, .. } => {
                                params.push(cont_param_ty);
                                *ret = object_ret_ty;
                            }
                            _ => {}
                        }
                        args.push(this_cont);
                    }
                    _ => return None,
                }
                // SUSPENDED marker into local 2 (this=0, result=1).
                let susp_call = self.ir.add_expr(IrExpr::Call {
                    callee: Callee::Static {
                        owner: "kotlin/coroutines/intrinsics/IntrinsicsKt".to_string(),
                        name: "getCOROUTINE_SUSPENDED".to_string(),
                        descriptor: "()Ljava/lang/Object;".to_string(),
                        inline: false,
                        must_inline: false,
                    },
                    dispatch_receiver: None,
                    args: vec![],
                });
                // Machine locals are allocated ABOVE the body's locals (the body was already lowered) so the
                // bound `a` slot (if any) can't collide with the SUSPENDED marker / call-result temp.
                let susp_idx = self.fresh_value();
                let r_idx = self.fresh_value();
                let susp_var = self.ir.add_expr(IrExpr::Variable {
                    index: susp_idx,
                    ty: object_ir.clone(),
                    init: Some(susp_call),
                });
                // State 0: throwOnFailure(result); this.label = 1; r = call; if r==SUSPENDED return it; tail.
                let s0_tof = throw_on_failure(self, 1);
                let this_l = self.ir.add_expr(IrExpr::GetValue(0));
                let one = self.ir.add_expr(IrExpr::Const(IrConst::Int(1)));
                let set_label = self.ir.add_expr(IrExpr::SetField {
                    receiver: this_l,
                    class: class_id,
                    index: label_idx,
                    value: one,
                });
                let r_var = self.ir.add_expr(IrExpr::Variable {
                    index: r_idx,
                    ty: object_ir.clone(),
                    init: Some(tail),
                });
                let rg = self.ir.add_expr(IrExpr::GetValue(r_idx));
                let sg = self.ir.add_expr(IrExpr::GetValue(susp_idx));
                let is_eq = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: IrBinOp::RefEq,
                    lhs: rg,
                    rhs: sg,
                });
                let sg2 = self.ir.add_expr(IrExpr::GetValue(susp_idx));
                let ret_susp = self.ir.add_expr(IrExpr::Return(Some(sg2)));
                let ret_susp_b = self.ir.add_expr(IrExpr::Block {
                    stmts: vec![ret_susp],
                    value: None,
                });
                let empty = self.ir.add_expr(IrExpr::Block {
                    stmts: vec![],
                    value: None,
                });
                let susp_when = self.ir.add_expr(IrExpr::When {
                    branches: vec![(Some(is_eq), ret_susp_b), (None, empty)],
                });
                // After the suspend call completes (synchronously here), bind the result `a` and run the
                // tail expression; with no binding the lambda simply returns the suspension value. `tail_at`
                // builds `[ (val a = unbox(src);) return box(tail_expr | src) ]` for a result at value `src`.
                let tail_at = |this: &mut Self, src: u32| -> Vec<u32> {
                    if let (Some((a_idx, a_ty)), Some(te)) = (&bound, tail_expr) {
                        let srcg = this.ir.add_expr(IrExpr::GetValue(src));
                        let unb = this.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg: srcg,
                            type_operand: a_ty.clone(),
                        });
                        let bind = this.ir.add_expr(IrExpr::Variable {
                            index: *a_idx,
                            ty: a_ty.clone(),
                            init: Some(unb),
                        });
                        let boxed = this.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg: te,
                            type_operand: object_ir.clone(),
                        });
                        vec![bind, this.ir.add_expr(IrExpr::Return(Some(boxed)))]
                    } else {
                        let g = this.ir.add_expr(IrExpr::GetValue(src));
                        vec![this.ir.add_expr(IrExpr::Return(Some(g)))]
                    }
                };
                let mut s0_stmts = vec![s0_tof, set_label, r_var, susp_when];
                s0_stmts.extend(tail_at(self, r_idx));
                let s0 = self.ir.add_expr(IrExpr::Block {
                    stmts: s0_stmts,
                    value: None,
                });
                // State 1 (resume): throwOnFailure(result); bind `a` from `result`; run the tail.
                let s1_tof = throw_on_failure(self, 1);
                let mut s1_stmts = vec![s1_tof];
                s1_stmts.extend(tail_at(self, 1));
                let s1 = self.ir.add_expr(IrExpr::Block {
                    stmts: s1_stmts,
                    value: None,
                });
                // Dispatch on `this.label`.
                let lbl0r = self.ir.add_expr(IrExpr::GetValue(0));
                let lbl0 = self.ir.add_expr(IrExpr::GetField {
                    receiver: lbl0r,
                    class: class_id,
                    index: label_idx,
                });
                let c0 = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                let cond0 = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: IrBinOp::Eq,
                    lhs: lbl0,
                    rhs: c0,
                });
                let lbl1r = self.ir.add_expr(IrExpr::GetValue(0));
                let lbl1 = self.ir.add_expr(IrExpr::GetField {
                    receiver: lbl1r,
                    class: class_id,
                    index: label_idx,
                });
                let c1 = self.ir.add_expr(IrExpr::Const(IrConst::Int(1)));
                let cond1 = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: IrBinOp::Eq,
                    lhs: lbl1,
                    rhs: c1,
                });
                let msg = self.ir.add_expr(IrExpr::Const(IrConst::String(
                    "call to 'resume' before 'invoke' with coroutine".to_string(),
                )));
                let exc = self.ir.add_expr(IrExpr::NewExternal {
                    internal: "java/lang/IllegalStateException".to_string(),
                    ctor_desc: "(Ljava/lang/String;)V".to_string(),
                    args: vec![msg],
                });
                let throw = self.ir.add_expr(IrExpr::Throw { operand: exc });
                let else_b = self.ir.add_expr(IrExpr::Block {
                    stmts: vec![throw],
                    value: None,
                });
                let dispatch = self.ir.add_expr(IrExpr::When {
                    branches: vec![(Some(cond0), s0), (Some(cond1), s1), (None, else_b)],
                });
                self.next_value = saved_next_sm;
                self.ir.add_expr(IrExpr::Block {
                    stmts: vec![susp_var, dispatch],
                    value: None,
                })
            }
        } else {
            let mut stmts = vec![throw_on_failure(self, 1)];
            let saved_scope = std::mem::take(&mut self.scope);
            let saved_next = self.next_value;
            self.next_value = 2; // this=0, result=1
            for (i, (name, _, ty)) in captures.iter().enumerate() {
                let lv = self.fresh_value();
                let this = self.ir.add_expr(IrExpr::GetValue(0));
                let getf = self.ir.add_expr(IrExpr::GetField {
                    receiver: this,
                    class: class_id,
                    index: i as u32,
                });
                stmts.push(self.ir.add_expr(IrExpr::Variable {
                    index: lv,
                    ty: ty_to_ir(*ty),
                    init: Some(getf),
                }));
                self.scope.push((name.clone(), lv, *ty));
            }
            // Own parameters: load each from its field and bind it in the body's scope. The source
            // `Ty` comes from the type checker (the lambda parameter's declared/inferred type).
            for (i, name) in bind_names.iter().enumerate() {
                let pty = params[i];
                let lv = self.fresh_value();
                let this = self.ir.add_expr(IrExpr::GetValue(0));
                let getf = self.ir.add_expr(IrExpr::GetField {
                    receiver: this,
                    class: class_id,
                    index: param_field_base + i as u32,
                });
                stmts.push(self.ir.add_expr(IrExpr::Variable {
                    index: lv,
                    ty: ty_to_ir(pty),
                    init: Some(getf),
                }));
                self.scope.push((name.clone(), lv, pty));
            }
            let body_val = self.expr(body);
            self.scope = saved_scope;
            self.next_value = saved_next;
            let body_val = body_val?;
            let body_ty = self.info.ty(body);
            if body_ty == Ty::Unit {
                // A `suspend () -> Unit` lambda: run the body for effect, then return the `Unit`
                // singleton — boxing a Unit-typed (no-value) body would `areturn` an empty stack.
                stmts.push(body_val);
                let unit = self.ir.add_expr(IrExpr::UnitInstance);
                stmts.push(self.ir.add_expr(IrExpr::Return(Some(unit))));
            } else if body_ty == Ty::Nothing {
                // The body always diverges (throws/returns) — no trailing return.
                stmts.push(body_val);
            } else {
                let boxed = self.ir.add_expr(IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    arg: body_val,
                    type_operand: object_ir.clone(),
                });
                stmts.push(self.ir.add_expr(IrExpr::Return(Some(boxed))));
            }
            self.ir.add_expr(IrExpr::Block { stmts, value: None })
        };
        let invoke_susp_fid = self.add_synth_method(
            &internal,
            class_id,
            "invokeSuspend",
            vec![object_ir.clone()],
            Ty::obj("kotlin/Any"),
            inv_susp_body,
            true,
        )?; // invokeSuspend is method index 0.
            // Hand the plain `invokeSuspend` to the coroutine pass to flatten into the general state machine
            // (its result/label/spilled fields are appended after the captures/params at `field_base`).
        if needs_pass_sm {
            self.ir
                .suspend_lambda_sm
                .push((invoke_susp_fid, class_id, n_cap + arity as u32));
        }

        // invoke(Object p0.., Object completion): `r = new This(this.cap.., (Continuation)completion);
        // r.param_i = (paramType)p_i; return r.invokeSuspend(Unit.INSTANCE)`. Value-indices: this=0,
        // params 1..=arity, completion=arity+1, the fresh `r` local at arity+2.
        let lambda_ty = Ty::obj(&internal);
        let completion_idx = arity as u32 + 1;
        let mut new_args: Vec<u32> = (0..n_cap)
            .map(|i| {
                let this = self.ir.add_expr(IrExpr::GetValue(0));
                self.ir.add_expr(IrExpr::GetField {
                    receiver: this,
                    class: class_id,
                    index: i,
                })
            })
            .collect();
        let comp_get = self.ir.add_expr(IrExpr::GetValue(completion_idx));
        new_args.push(self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: comp_get,
            type_operand: cont_ir.clone(),
        }));
        let new_inst = self.ir.add_expr(IrExpr::New {
            class: class_id,
            args: new_args,
            ctor_params: None,
        });
        let r_idx = arity as u32 + 2;
        let mut inv_stmts = vec![self.ir.add_expr(IrExpr::Variable {
            index: r_idx,
            ty: lambda_ty.clone(),
            init: Some(new_inst),
        })];
        // Store each own parameter (coerced from the erased `Object` argument) into its field.
        for (i, pty) in params.iter().enumerate() {
            let pv = self.ir.add_expr(IrExpr::GetValue(1 + i as u32));
            let coerced = self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: pv,
                type_operand: ty_to_ir(*pty),
            });
            let rg = self.ir.add_expr(IrExpr::GetValue(r_idx));
            inv_stmts.push(self.ir.add_expr(IrExpr::SetField {
                receiver: rg,
                class: class_id,
                index: param_field_base + i as u32,
                value: coerced,
            }));
        }
        let rg2 = self.ir.add_expr(IrExpr::GetValue(r_idx));
        let unit = self.ir.add_expr(IrExpr::UnitInstance);
        let call_is = self.ir.add_expr(IrExpr::MethodCall {
            class: class_id,
            index: 0,
            receiver: rg2,
            args: vec![Some(unit)],
        });
        inv_stmts.push(self.ir.add_expr(IrExpr::Return(Some(call_is))));
        let inv_body = self.ir.add_expr(IrExpr::Block {
            stmts: inv_stmts,
            value: None,
        });
        self.add_synth_method(
            &internal,
            class_id,
            "invoke",
            vec![object_ir.clone(); arity + 1],
            Ty::obj("kotlin/Any"),
            inv_body,
            true,
        )?;

        // create(value.., Continuation completion): Continuation — the `SuspendLambda` factory that
        // `startCoroutine`/`createCoroutine` invoke at runtime (the base throws "not overridden"). Builds
        // `new This(this.cap.., completion)`, stores each own parameter, and returns the new lambda.
        // Value-indices: this=0, params 1..=arity, completion=arity+1, the fresh `r` at arity+2.
        let mut create_new_args: Vec<u32> = (0..n_cap)
            .map(|i| {
                let this = self.ir.add_expr(IrExpr::GetValue(0));
                self.ir.add_expr(IrExpr::GetField {
                    receiver: this,
                    class: class_id,
                    index: i,
                })
            })
            .collect();
        // The completion parameter is already a `Continuation` (the ctor's last param) — no cast.
        create_new_args.push(self.ir.add_expr(IrExpr::GetValue(completion_idx)));
        let create_new = self.ir.add_expr(IrExpr::New {
            class: class_id,
            args: create_new_args,
            ctor_params: None,
        });
        let cr_idx = arity as u32 + 2;
        let mut create_stmts = vec![self.ir.add_expr(IrExpr::Variable {
            index: cr_idx,
            ty: lambda_ty,
            init: Some(create_new),
        })];
        for (i, pty) in params.iter().enumerate() {
            let pv = self.ir.add_expr(IrExpr::GetValue(1 + i as u32));
            let coerced = self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: pv,
                type_operand: ty_to_ir(*pty),
            });
            let rg = self.ir.add_expr(IrExpr::GetValue(cr_idx));
            create_stmts.push(self.ir.add_expr(IrExpr::SetField {
                receiver: rg,
                class: class_id,
                index: param_field_base + i as u32,
                value: coerced,
            }));
        }
        let crg = self.ir.add_expr(IrExpr::GetValue(cr_idx));
        create_stmts.push(self.ir.add_expr(IrExpr::Return(Some(crg))));
        let create_body = self.ir.add_expr(IrExpr::Block {
            stmts: create_stmts,
            value: None,
        });
        let mut create_params: Vec<Ty> = vec![Ty::obj("kotlin/Any"); arity];
        create_params.push(Ty::obj("kotlin/coroutines/Continuation"));
        self.add_synth_method(
            &internal,
            class_id,
            "create",
            create_params,
            Ty::obj("kotlin/coroutines/Continuation"),
            create_body,
            true,
        )?;

        // Creation site: `new This(captures.., (Continuation) null)`.
        let mut site_args: Vec<u32> = captures
            .iter()
            .map(|(_, v, _)| self.ir.add_expr(IrExpr::GetValue(*v)))
            .collect();
        site_args.push(self.ir.add_expr(IrExpr::Const(IrConst::Null)));
        Some(self.ir.add_expr(IrExpr::New {
            class: class_id,
            args: site_args,
            ctor_params: None,
        }))
    }

    /// Register a synthesized instance method (a real `IrFunction` with an IR body) on a class, so
    /// it resolves like any other method and the generic emitter handles it — no backend special-case.
    fn add_synth_method(
        &mut self,
        internal: &str,
        class_id: ClassId,
        name: &str,
        params: Vec<Ty>,
        ret: Ty,
        body: u32,
        force_override: bool,
    ) -> Option<u32> {
        if self
            .classes
            .get(internal)
            .map_or(false, |ci| ci.methods.contains_key(name))
        {
            return None; // a user-defined override exists — don't synthesize over it
        }
        // Don't synthesize over a member a superclass provides. For a `data class` member
        // (`force_override`), only a *final* base member blocks generation — an `open` override IS
        // overridden by the synthesized member (KT-6206); a final one is inherited (can't override).
        // Other synthesis (an interface-delegation forwarder) inherits any base member.
        if let Some(s) = self
            .classes
            .get(internal)
            .and_then(|ci| ci.super_internal.clone())
        {
            let blocks = if force_override {
                self.syms
                    .method_of(&s, name)
                    .map_or(false, |sig| sig.is_final)
            } else {
                self.resolve_method(&s, name).is_some()
            };
            if blocks {
                return None;
            }
        }
        let fid = self.ir.add_fun(IrFunction {
            name: name.to_string(),
            params,
            ret: ty_to_ir(ret),
            body: Some(body),
            is_static: false,
            dispatch_receiver: Some(internal.to_string()),
            param_checks: Vec::new(),
        });
        let idx = self.ir.classes[class_id as usize].methods.len() as u32;
        self.ir.classes[class_id as usize].methods.push(fid);
        if let Some(ci) = self.classes.get_mut(internal) {
            ci.methods.insert(name.to_string(), (idx, fid, ret));
        }
        Some(fid)
    }

    /// Synthesize forwarding methods for `: Iface by delegate` — each of `Iface`'s methods becomes
    /// `fun m(args) = this.delegate.m(args)` (an `invokeinterface` on the delegate field). Only
    /// user-interface delegation with a simple `val`-parameter delegate is modeled; anything else
    /// (classpath interface, missing field) returns `None` so the file is skipped, never miscompiled.
    fn synth_delegation_forwarders(
        &mut self,
        file: &ast::File,
        c: &ast::ClassDecl,
        internal: &str,
        class_id: ClassId,
    ) -> Option<()> {
        for (di, (iface_name, delegate)) in c.delegations.iter().enumerate() {
            // A `val`-param delegate has its own field (`a`); a non-`val` param uses the synthesized
            // `$$delegate_<di>` (see field synthesis + the ctor store).
            let is_synth = !c.props.iter().any(|p| p.is_property && &p.name == delegate);
            let synth_name = format!("$$delegate_{di}");
            let delegate_idx =
                self.classes
                    .get(internal)?
                    .fields
                    .iter()
                    .position(|(n, _)| n == delegate || n == &synth_name)? as u32;
            self.forward_iface_methods(
                file,
                iface_name,
                delegate_idx,
                class_id,
                internal,
                is_synth,
            )?;
        }
        // Expression delegates (`: I by Impl()`) always use a synthesized `$$delegate_e<j>` field.
        for (j, (iface_name, e)) in c.delegation_exprs.iter().enumerate() {
            // A VALUE-class delegate is unboxed (e.g. `Z(x)` → `Integer`) and does not implement the
            // interface at runtime — forwarding an interface call to it fails. Skip (never miscompile).
            if self
                .info
                .ty(*e)
                .obj_internal()
                .is_some_and(|i| self.is_value_class(i))
            {
                return None;
            }
            let synth_name = format!("$$delegate_e{j}");
            let delegate_idx = self
                .classes
                .get(internal)?
                .fields
                .iter()
                .position(|(n, _)| n == &synth_name)? as u32;
            self.forward_iface_methods(file, iface_name, delegate_idx, class_id, internal, true)?;
        }
        Some(())
    }

    /// Synthesize, on `internal`, a forwarding method for each of interface `iface_name`'s methods that
    /// calls it on the delegate field at `delegate_idx` (`fun m(args) = this.<field>.m(args)`). When
    /// the delegate is synthesized (a non-`val` param or an expression — not a `val`-param field), bail
    /// (skip the file) on a PROPERTY interface (`getX`/`setX` would go un-forwarded → `AbstractMethodError`)
    /// or a GENERIC one (`A<Long, Int>` needs substituted-type bridges a raw forward mis-coerces).
    fn forward_iface_methods(
        &mut self,
        file: &ast::File,
        iface_name: &str,
        delegate_idx: u32,
        class_id: ClassId,
        internal: &str,
        is_synth: bool,
    ) -> Option<()> {
        if is_synth {
            if self
                .syms
                .classes
                .get(iface_name)
                .is_some_and(|s| !s.props.is_empty())
            {
                return None;
            }
            if file.decls.iter().any(|&d| {
                matches!(file.decl(d), Decl::Class(ic) if ic.name == *iface_name && !ic.type_params.is_empty())
            }) {
                return None;
            }
        }
        let iface_internal = class_internal(file, iface_name);
        let methods: Vec<(String, Vec<Ty>, Ty)> = self
            .syms
            .classes
            .get(iface_name)?
            .methods
            .iter()
            .map(|(n, s)| (n.clone(), s.params.clone(), s.ret))
            .collect();
        for (mname, params, ret) in methods {
            let params_ir: Vec<Ty> = params.iter().map(|t| ty_to_ir(*t)).collect();
            let descriptor = format!(
                "({}){}",
                params.iter().map(|t| t.descriptor()).collect::<String>(),
                ret.descriptor()
            );
            let field = self.this_field(class_id, delegate_idx);
            let args: Vec<u32> = (0..params.len())
                .map(|i| self.ir.add_expr(IrExpr::GetValue(i as u32 + 1)))
                .collect();
            let call = self.ir.add_expr(IrExpr::Call {
                callee: Callee::Virtual {
                    owner: iface_internal.clone(),
                    name: mname.clone(),
                    descriptor,
                    interface: true,
                },
                dispatch_receiver: Some(field),
                args,
            });
            let body = if ret == Ty::Unit {
                self.ir.add_expr(IrExpr::Block {
                    stmts: vec![call],
                    value: None,
                })
            } else {
                let ret_stmt = self.ir.add_expr(IrExpr::Return(Some(call)));
                self.ir.add_expr(IrExpr::Block {
                    stmts: vec![ret_stmt],
                    value: None,
                })
            };
            self.add_synth_method(internal, class_id, &mname, params_ir, ret, body, false);
        }
        Some(())
    }

    /// Convert an unsigned value (`UInt`/`ULong`, represented as int/long) to its unsigned-decimal
    /// `String` via `Integer.toUnsignedString`/`Long.toUnsignedString` — what kotlinc uses for an
    /// unsigned `toString()`/string-template part (a signed `toString` would print the wrong value).
    /// Box an unsigned primitive (`UInt`/`ULong`, represented as int/long) to its inline-class object
    /// via `kotlin/UInt."box-impl"(I)Lkotlin/UInt;` (kotlinc's synthetic factory) — NOT
    /// `Integer.valueOf`, which would lose the unsigned identity (`is UInt`, the unsigned `toString`).
    fn box_unsigned(&mut self, val: u32, ty: Ty) -> u32 {
        let (owner, prim) = if ty == Ty::UInt {
            ("kotlin/UInt", "I")
        } else {
            ("kotlin/ULong", "J")
        };
        self.ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: owner.to_string(),
                name: "box-impl".to_string(),
                descriptor: format!("({prim})L{owner};"),
                inline: false,
                must_inline: false,
            },
            dispatch_receiver: None,
            args: vec![val],
        })
    }

    /// Unbox a (possibly `Object`-typed) `kotlin/UInt`/`ULong` object back to its int/long: checkcast
    /// to the inline-class type, then `unbox-impl`.
    fn unbox_unsigned(&mut self, val: u32, ty: Ty) -> u32 {
        let (owner, prim) = if ty == Ty::UInt {
            ("kotlin/UInt", "I")
        } else {
            ("kotlin/ULong", "J")
        };
        let cast = self.ir.add_expr(IrExpr::TypeOp {
            op: IrTypeOp::Cast,
            arg: val,
            type_operand: ty_to_ir(Ty::obj(owner)),
        });
        self.ir.add_expr(IrExpr::Call {
            callee: Callee::Virtual {
                owner: owner.to_string(),
                name: "unbox-impl".to_string(),
                descriptor: format!("(){prim}"),
                interface: false,
            },
            dispatch_receiver: Some(cast),
            args: vec![],
        })
    }

    fn unsigned_to_string(&mut self, val: u32, ty: Ty) -> u32 {
        let (owner, prim) = if ty == Ty::UInt {
            ("java/lang/Integer", "I")
        } else {
            ("java/lang/Long", "J")
        };
        self.ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: owner.to_string(),
                name: "toUnsignedString".to_string(),
                descriptor: format!("({prim})Ljava/lang/String;"),
                inline: false,
                must_inline: false,
            },
            dispatch_receiver: None,
            args: vec![val],
        })
    }

    fn ir_const_str(&mut self, s: String) -> u32 {
        self.ir.add_expr(IrExpr::Const(IrConst::String(s)))
    }
    fn this_field(&mut self, class_id: ClassId, i: u32) -> u32 {
        let this = self.ir.add_expr(IrExpr::GetValue(0));
        self.ir.add_expr(IrExpr::GetField {
            receiver: this,
            class: class_id,
            index: i,
        })
    }
    fn static_call(&mut self, fq: &str, args: Vec<u32>) -> u32 {
        self.ir.add_expr(IrExpr::Call {
            callee: Callee::External(fq.to_string()),
            dispatch_receiver: None,
            args,
        })
    }
    /// The `Int` hash of a field value `v` of type `t` (Kotlin's per-field `.hashCode()`). A value-class
    /// field reads here as its erased underlying; the JVM value-class pass boxes it at the reference
    /// boundary (`Objects.hashCode`) so the value class's own `hashCode` runs.
    /// Whether class `class_id`'s field `i` has a nullable reference type (its lowered `IrType` carries
    /// the `?`), so `hashCode` keeps it on the null-safe `Objects.hashCode` path.
    fn field_nullable(&self, class_id: ClassId, i: usize) -> bool {
        self.ir.classes[class_id as usize].fields[i]
            .ty
            .is_nullable()
    }
    fn field_hash(&mut self, v: u32, t: Ty, nullable: bool) -> u32 {
        match t {
            // kotlinc hashes each primitive field via its boxed type's static `hashCode(prim)` (so the
            // bytecode matches even though `Integer.hashCode(I)` is the identity on `int`).
            Ty::Int => self.static_call("java/lang/Integer.hashCode", vec![v]),
            Ty::Short => self.static_call("java/lang/Short.hashCode", vec![v]),
            Ty::Byte => self.static_call("java/lang/Byte.hashCode", vec![v]),
            Ty::Char => self.static_call("java/lang/Character.hashCode", vec![v]),
            Ty::Boolean => self.static_call("java/lang/Boolean.hashCode", vec![v]),
            // A NON-null `String` hashes via its own `String.hashCode()` (kotlinc's `invokevirtual`),
            // matching the bytecode. A nullable one stays on `Objects.hashCode` (which null-guards,
            // returning 0 for `null`) — the null-guarded-ternary form is a future parity item.
            Ty::String if !nullable => self.ir.add_expr(IrExpr::Call {
                callee: Callee::External("kotlin/Any.hashCode".to_string()),
                dispatch_receiver: Some(v),
                args: vec![],
            }),
            Ty::Long => self.static_call("java/lang/Long.hashCode", vec![v]),
            Ty::Double => self.static_call("java/lang/Double.hashCode", vec![v]),
            Ty::Float => self.static_call("java/lang/Float.hashCode", vec![v]),
            // An array property hashes by reference identity (`Objects.hashCode`), matching kotlinc — a
            // data class does NOT content-hash arrays (consistent with its reference-based `equals`).
            _ => self.static_call("java/util/Objects.hashCode", vec![v]),
        }
    }
    /// A `Boolean` IR expr testing field *inequality* (IEEE-aware for float/double, structural for
    /// refs) — used to build `equals` as a chain of `if (a != b) return false` early-outs.
    fn field_ne(&mut self, a: u32, b: u32, t: Ty) -> u32 {
        match t {
            Ty::Double | Ty::Float => {
                let fq = if t == Ty::Double {
                    "java/lang/Double.compare"
                } else {
                    "java/lang/Float.compare"
                };
                let cmp = self.static_call(fq, vec![a, b]);
                let z = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: IrBinOp::Ne,
                    lhs: cmp,
                    rhs: z,
                })
            }
            // Int/Long/… → native compare; reference (incl. an array property, which a data class
            // compares by reference, not content) → `!Intrinsics.areEqual` via the reference Ne path.
            _ => self.ir.add_expr(IrExpr::PrimitiveBinOp {
                op: IrBinOp::Ne,
                lhs: a,
                rhs: b,
            }),
        }
    }
    /// `if (cond) return false` — a no-`else` statement-`when` whose only branch diverges.
    fn guard_return_false(&mut self, cond: u32) -> u32 {
        self.guard_return_bool(cond, false)
    }
    /// `if (cond) return <b>` — a no-`else` statement-`when` whose only branch diverges.
    fn guard_return_bool(&mut self, cond: u32, b: bool) -> u32 {
        let f = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(b)));
        let ret = self.ir.add_expr(IrExpr::Return(Some(f)));
        let blk = self.ir.add_expr(IrExpr::Block {
            stmts: vec![ret],
            value: None,
        });
        self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(cond), blk)],
        })
    }

    /// Synthesize a `data class`'s `componentN`/`toString`/`hashCode`/`equals` as IR methods over the
    /// first `n` (primary-constructor) fields.
    fn synth_data_members(&mut self, internal: &str, class_id: ClassId, n: usize) {
        let fields: Vec<(String, Ty)> = self.classes[internal].fields[..n].to_vec();

        // componentN(): `return this.fieldN`.
        for (i, (_, t)) in fields.iter().enumerate() {
            let get = self.this_field(class_id, i as u32);
            let ret = self.ir.add_expr(IrExpr::Return(Some(get)));
            let body = self.ir.add_expr(IrExpr::Block {
                stmts: vec![ret],
                value: None,
            });
            self.add_synth_method(
                internal,
                class_id,
                &format!("component{}", i + 1),
                vec![],
                *t,
                body,
                true,
            );
        }

        // copy(f1, f2, …): `return P(f1, f2, …)`. Emitted BEFORE toString/hashCode/equals to match
        // kotlinc's data-class member order (componentN, copy, copy$default, toString, hashCode, equals).
        {
            let params: Vec<Ty> = fields.iter().map(|(_, t)| ty_to_ir(*t)).collect();
            let args: Vec<u32> = (0..fields.len())
                .map(|i| self.ir.add_expr(IrExpr::GetValue(i as u32 + 1)))
                .collect();
            let new = self.ir.add_expr(IrExpr::New {
                class: class_id,
                args,
                ctor_params: None,
            });
            let ret = self.ir.add_expr(IrExpr::Return(Some(new)));
            let body = self.ir.add_expr(IrExpr::Block {
                stmts: vec![ret],
                value: None,
            });
            if let Some(copy_fid) = self.add_synth_method(
                internal,
                class_id,
                "copy",
                params,
                Ty::obj(internal),
                body,
                true,
            ) {
                // `copy`'s parameters are the primary-ctor properties, so they take the SAME
                // `checkNotNullParameter` guards kotlinc emits at the constructor — a non-null reference
                // parameter is null-checked at `copy` entry. Reuse the class's precomputed ctor checks.
                let mut checks = self.ir.classes[class_id as usize].ctor_param_checks.clone();
                checks.resize(fields.len(), None);
                self.ir.functions[copy_fid as usize].param_checks = checks;
                // Each `copy` parameter defaults to the corresponding property of the receiver (the JVM
                // backend realizes this as `copy$default`). The mask is one `int` (≤31 params).
                if fields.len() <= 31 {
                    let defaults: Vec<Option<u32>> = (0..fields.len())
                        .map(|i| {
                            let this = self.ir.add_expr(IrExpr::GetValue(0));
                            Some(self.ir.add_expr(IrExpr::GetField {
                                receiver: this,
                                class: class_id,
                                index: i as u32,
                            }))
                        })
                        .collect();
                    self.ir.fn_param_defaults.insert(copy_fid, defaults);
                    self.ir
                        .fn_param_names
                        .insert(copy_fid, fields.iter().map(|(n, _)| n.clone()).collect());
                }
            }
        }

        // toString(): `"Simple(f1=" + f1 + ", f2=" + f2 + ")"`.
        {
            let simple = internal
                .rsplit('/')
                .next()
                .unwrap_or(internal)
                .replace('$', ".");
            // Build ONE `StringConcat` (kotlinc emits a single `StringBuilder`): the class-name prefix is
            // merged with the first field's `name=` (`"P(x="`), then each field value, `", name="`
            // separators, and a closing `")"`.
            let mut parts: Vec<u32> = Vec::new();
            let mut prefix = format!("{simple}(");
            for (i, (name, _)) in fields.iter().enumerate() {
                if i == 0 {
                    prefix.push_str(&format!("{name}="));
                    parts.push(self.ir_const_str(std::mem::take(&mut prefix)));
                } else {
                    parts.push(self.ir_const_str(format!(", {name}=")));
                }
                let mut fv = self.this_field(class_id, i as u32);
                // A data class renders an array property with `java.util.Arrays.toString(field)` (so
                // `[true]`, not the default `[Z@hash`), matching kotlinc.
                if let Some(param) =
                    data_array_param(&self.ir.classes[class_id as usize].fields[i].ty)
                {
                    fv = self.ir.add_expr(IrExpr::Call {
                        callee: Callee::Static {
                            owner: "java/util/Arrays".to_string(),
                            name: "toString".to_string(),
                            descriptor: format!("({param})Ljava/lang/String;"),
                            inline: false,
                            must_inline: false,
                        },
                        dispatch_receiver: None,
                        args: vec![fv],
                    });
                }
                parts.push(fv);
            }
            if fields.is_empty() {
                parts.push(self.ir_const_str(prefix));
            }
            parts.push(self.ir_const_str(")".to_string()));
            let acc = self.ir.add_expr(IrExpr::StringConcat(parts));
            let ret = self.ir.add_expr(IrExpr::Return(Some(acc)));
            let body = self.ir.add_expr(IrExpr::Block {
                stmts: vec![ret],
                value: None,
            });
            if let Some(fid) = self.add_synth_method(
                internal,
                class_id,
                "toString",
                vec![],
                Ty::String,
                body,
                true,
            ) {
                self.ir.open_methods.insert(fid); // kotlinc keeps the Object-override open
            }
        }

        // hashCode(): kotlinc emits `return 0` for an empty data class, `return h(f0)` for a single field,
        // and for ≥2 fields a `result` LOCAL it folds into — `result = h(f0); result = result*31 + h(fN);
        // return result` (an explicit `istore`/`iload` round-trip per field). Match that shape exactly.
        {
            let body = if fields.is_empty() {
                let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                let ret = self.ir.add_expr(IrExpr::Return(Some(zero)));
                self.ir.add_expr(IrExpr::Block {
                    stmts: vec![ret],
                    value: None,
                })
            } else if fields.len() == 1 {
                let fv = self.this_field(class_id, 0);
                let n = self.field_nullable(class_id, 0);
                let h = self.field_hash(fv, fields[0].1, n);
                let ret = self.ir.add_expr(IrExpr::Return(Some(h)));
                self.ir.add_expr(IrExpr::Block {
                    stmts: vec![ret],
                    value: None,
                })
            } else {
                // `result` occupies the first local slot after `this` (hashCode takes no parameters).
                const RV: u32 = 1;
                let mut stmts = Vec::new();
                let f0 = self.this_field(class_id, 0);
                let n0 = self.field_nullable(class_id, 0);
                let h0 = self.field_hash(f0, fields[0].1, n0);
                stmts.push(self.ir.add_expr(IrExpr::Variable {
                    index: RV,
                    ty: ty_to_ir(Ty::Int),
                    init: Some(h0),
                }));
                for (i, f) in fields.iter().enumerate().skip(1) {
                    let fv = self.this_field(class_id, i as u32);
                    let ni = self.field_nullable(class_id, i);
                    let h = self.field_hash(fv, f.1, ni);
                    let prev = self.ir.add_expr(IrExpr::GetValue(RV));
                    let c31 = self.ir.add_expr(IrExpr::Const(IrConst::Int(31)));
                    let mul = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: IrBinOp::Mul,
                        lhs: prev,
                        rhs: c31,
                    });
                    let add = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: IrBinOp::Add,
                        lhs: mul,
                        rhs: h,
                    });
                    stmts.push(self.ir.add_expr(IrExpr::SetValue {
                        var: RV,
                        value: add,
                    }));
                }
                let getr = self.ir.add_expr(IrExpr::GetValue(RV));
                stmts.push(self.ir.add_expr(IrExpr::Return(Some(getr))));
                self.ir.add_expr(IrExpr::Block { stmts, value: None })
            };
            if let Some(fid) =
                self.add_synth_method(internal, class_id, "hashCode", vec![], Ty::Int, body, true)
            {
                self.ir.open_methods.insert(fid); // kotlinc keeps the Object-override open
            }
        }

        // equals(other), matching kotlinc's shape exactly:
        //   if (this === other) return true
        //   if (other !is T) return false
        //   val o = other as T            // cast ONCE into a local, then reuse
        //   if (this.f1 != o.f1) return false; …
        //   return true
        {
            let class_ty = ty_to_ir(Ty::obj(internal));
            // The cast result lives in the first local slot after `this` (0) and `other` (1).
            const OV: u32 = 2;
            let mut stmts = Vec::new();
            // `this === other` referential-identity fast-path.
            let this0 = self.ir.add_expr(IrExpr::GetValue(0));
            let other0 = self.ir.add_expr(IrExpr::GetValue(1));
            let ident = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                op: IrBinOp::RefEq,
                lhs: this0,
                rhs: other0,
            });
            let id_guard = self.guard_return_bool(ident, true);
            stmts.push(id_guard);
            // `other !is T` → return false.
            let other = self.ir.add_expr(IrExpr::GetValue(1));
            let not_inst = self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::NotInstanceOf,
                arg: other,
                type_operand: class_ty.clone(),
            });
            let g = self.guard_return_false(not_inst);
            stmts.push(g);
            // `val o = other as T` — one checkcast, stored to the local.
            let other_v = self.ir.add_expr(IrExpr::GetValue(1));
            let ocast = self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::Cast,
                arg: other_v,
                type_operand: class_ty.clone(),
            });
            stmts.push(self.ir.add_expr(IrExpr::Variable {
                index: OV,
                ty: class_ty.clone(),
                init: Some(ocast),
            }));
            for (i, (_, t)) in fields.iter().enumerate() {
                let af = self.this_field(class_id, i as u32);
                let o_local = self.ir.add_expr(IrExpr::GetValue(OV));
                let bf = self.ir.add_expr(IrExpr::GetField {
                    receiver: o_local,
                    class: class_id,
                    index: i as u32,
                });
                let ne = self.field_ne(af, bf, *t);
                let g = self.guard_return_false(ne);
                stmts.push(g);
            }
            let t = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
            stmts.push(self.ir.add_expr(IrExpr::Return(Some(t))));
            let body = self.ir.add_expr(IrExpr::Block { stmts, value: None });
            let obj = ty_to_ir(Ty::obj("kotlin/Any"));
            if let Some(fid) = self.add_synth_method(
                internal,
                class_id,
                "equals",
                vec![obj],
                Ty::Boolean,
                body,
                true,
            ) {
                self.ir.open_methods.insert(fid); // kotlinc keeps the Object-override open
            }
        }
    }

    fn lookup(&self, name: &str) -> Option<(u32, Ty)> {
        self.scope
            .iter()
            .rev()
            .find(|(n, _, _)| n == name)
            .map(|(_, v, t)| (*v, *t))
    }

    fn class_of(&self, ty: Ty) -> Option<&ClassInfo> {
        ty.obj_internal().and_then(|i| self.classes.get(i))
    }

    /// Route a library `inline fun` call with a lambda argument (`recv.<name> { … }`) to the bytecode
    /// inliner (`Callee::Static` with `inline`), carrying the receiver and the lambda — so its real body
    /// is spliced rather than desugared per-function. Metadata-driven: gated on the resolved callable's
    /// `is_inline` flag, not the name. Only routes a non-capturing, single-value-return lambda (which the
    /// emitter is guaranteed to splice); `None` ⇒ the call falls through to its desugar / normal lowering.
    fn try_route_lambda_inline(
        &mut self,
        name: &str,
        receiver: AstExprId,
        lam_arg: AstExprId,
        rty: Ty,
    ) -> Option<u32> {
        // The bytecode splicer substitutes the receiver inline; it can relocate a simple value but not
        // an `invokedynamic` (a lambda literal or callable reference `::A` as the receiver), so the
        // splice would fail at emit and fall back to a real call to the private `@InlineOnly` callee
        // (IllegalAccessError). Bail so the `let`/`also` desugar — which binds the receiver to a local
        // first — handles it instead.
        if matches!(
            self.afile.expr(receiver),
            Expr::CallableRef { .. } | Expr::Lambda { .. }
        ) {
            return None;
        }
        // Resolve via the inline-only path, which (unlike `resolve_callable`) matches `@InlineOnly`
        // package-private scope fns (`let`/`also`) — safe because we *inline* it (no call is emitted).
        let c = self
            .syms
            .libraries
            .resolve_scope_inline(name, rty, &[self.info.ty(lam_arg)])?;
        if !c.is_inline {
            return None;
        }
        // The platform must be able to splice this body (branchless, single lambda-invoke, single exit) —
        // else the emitter would fall back to a real call, which is broken for an `@InlineOnly` callee.
        if !self
            .syms
            .libraries
            .can_inline_lambda(&c.owner, &c.name, &c.descriptor)
        {
            return None;
        }
        // The emitter's lambda-splice is branchless-only: a branch in the lambda body produces a
        // stackmap frame it can't relocate mid-splice. Route only a branchless body; a branchy one
        // falls through to the per-function desugar, which lowers the body with normal branchy codegen.
        if let Expr::Lambda { body, .. } = self.afile.expr(lam_arg) {
            if body_contains_branch(self.afile, *body) {
                return None;
            }
        }
        let lam = self.expr(lam_arg)?;
        // The argument must be a real lambda (with an `inline_body` to splice) — a callable reference
        // (`::foo`) has none. The emitter handles any lambda body, incl. captures and non-local return.
        if !matches!(
            self.ir.expr(lam),
            IrExpr::Lambda {
                inline_body: Some(_),
                ..
            }
        ) {
            return None;
        }
        let recv = self.expr(receiver)?;
        let (logical, physical) = (c.ret, c.physical_ret);
        let must_inline = c.must_inline;
        let call = self.ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: c.owner,
                name: c.name,
                descriptor: c.descriptor,
                inline: true,
                // A non-public `@InlineOnly` scope fn (`let`/`also`/…) has no callable method — a failed
                // splice must SKIP the file, never an `invokestatic` on the private method. A PUBLIC host
                // (`map`/`fold`/`forEach`) can fall back to a real call, so `must_inline` is false for it.
                must_inline,
            },
            dispatch_receiver: None,
            args: vec![recv, lam],
        });
        // The inline fn's erased return is `Object` (a generic `R`); coerce the spliced result to the
        // logical return type (`5.let { it+1 }: Int` unboxes `Integer`→`int`), as a normal call would.
        Some(self.coerce_erased(call, logical, physical))
    }

    fn top_fun_decl(&self, name: &str) -> Option<&ast::FunDecl> {
        self.afile
            .decls
            .iter()
            .find_map(|&d| match self.afile.decl(d) {
                Decl::Fun(f) if f.name == name => Some(f),
                _ => None,
            })
    }

    /// The internal name of an explicit serializer `X` from `@Serializable(with = X::class)` on class
    /// `c`, or `None`. The arg is a class-literal `X::class` (`CallableRef{name:"class"}`); `X`'s
    /// internal comes from the checker's type of the literal's receiver.
    fn custom_serializer_of(&self, c: &ast::ClassDecl) -> Option<String> {
        let i = c
            .annotations
            .iter()
            .position(|a| a.rsplit(['/', '.']).next() == Some("Serializable"))?;
        let arg = c.annotation_args.get(i).and_then(|args| args.first())?;
        // `X::class` — a class literal; annotation args aren't type-checked, so resolve `X`'s internal
        // name from the symbol table (imports/classpath seed) rather than the checker's expr types.
        if let Expr::CallableRef {
            receiver: Some(r),
            name,
        } = self.afile.expr(*arg)
        {
            if name == "class" {
                if let Expr::Name(x) = self.afile.expr(*r) {
                    return self.syms.class_names.get(x).cloned();
                }
            }
        }
        None
    }

    /// Per-property explicit serializers from `@Serializable(with = X::class)` on constructor
    /// properties of `c`, as `(property_name, X_internal)`. Mirrors [`custom_serializer_of`] but per
    /// field; `X`'s internal name comes from the symbol table (annotation args aren't type-checked).
    fn field_serializers_of(&self, c: &ast::ClassDecl) -> Vec<(String, String)> {
        c.props
            .iter()
            .filter_map(|p| {
                let i = p
                    .annotations
                    .iter()
                    .position(|a| a.rsplit(['/', '.']).next() == Some("Serializable"))?;
                let arg = p.annotation_args.get(i).and_then(|args| args.first())?;
                if let Expr::CallableRef {
                    receiver: Some(r),
                    name,
                } = self.afile.expr(*arg)
                {
                    if name == "class" {
                        if let Expr::Name(x) = self.afile.expr(*r) {
                            return Some((p.name.clone(), self.syms.class_names.get(x)?.clone()));
                        }
                    }
                }
                None
            })
            .collect()
    }

    /// Property names whose element serializer is CONTEXTUAL: a property carrying `@Contextual`, or one
    /// whose type is named in a file-level `@file:UseContextualSerialization(<type>::class)`. Matching is
    /// by type NAME (typealias-expanded on both sides), so `@file:UseContextualSerialization(MyDate::class)`
    /// (where `typealias MyDate = java.time.LocalDate`) covers both a `MyDate` and a `java.time.LocalDate`
    /// property. The plugin emits `ContextualSerializer(<type>::class)` for these (descriptor kind CONTEXTUAL).
    fn contextual_fields_of(&self, c: &ast::ClassDecl) -> Vec<String> {
        // Canonical (typealias-expanded) type names named by the file's `@UseContextualSerialization`.
        // Matching is on the FULL canonical name (never a bare simple name), so a same-simple-name class
        // in a different package is NOT mis-marked contextual.
        let mut names: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (ann, args) in &self.afile.file_annotations {
            if ann != "UseContextualSerialization" {
                continue;
            }
            for &arg in args {
                if let Expr::CallableRef {
                    receiver: Some(r),
                    name,
                } = self.afile.expr(arg)
                {
                    if name == "class" {
                        if let Expr::Name(x) = self.afile.expr(*r) {
                            names.insert(self.canonical_type_name(x));
                        }
                    }
                }
            }
        }
        c.props
            .iter()
            .filter_map(|p| {
                let has_contextual = p
                    .annotations
                    .iter()
                    .any(|a| a.rsplit(['/', '.']).next() == Some("Contextual"));
                if has_contextual || names.contains(&self.canonical_type_name(&p.ty.name)) {
                    Some(p.name.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    /// A type name's `typealias` target (`MyDate` → `java.time.LocalDate`) or the name unchanged. Lets a
    /// contextual-type reference match a property declared via either the alias or the underlying type,
    /// while comparing FULL names only (so `java.time.LocalDate` never matches `other.pkg.LocalDate`).
    fn canonical_type_name(&self, name: &str) -> String {
        self.afile
            .type_aliases
            .iter()
            .find(|(a, _)| a == name)
            .map(|(_, t)| t.clone())
            .unwrap_or_else(|| name.to_string())
    }

    fn class_decl(&self, name: &str) -> Option<&ast::ClassDecl> {
        self.afile
            .decls
            .iter()
            .find_map(|&d| match self.afile.decl(d) {
                Decl::Class(c) if c.name == name => Some(c),
                _ => None,
            })
    }

    /// The IR fq name of a `@Serializable` USER class for `ty` (the value/type-arg of a reified
    /// serialization call), or `None`. Detection mirrors the plugin/checker (annotation simple name).
    fn serializable_internal(&self, ty: Ty) -> Option<String> {
        let internal = ty.obj_internal()?;
        let simple = internal.rsplit('/').next().unwrap_or(internal);
        let cd = self.class_decl(simple)?;
        let is_ser = cd
            .annotations
            .iter()
            .any(|a| a.rsplit(['/', '.']).next() == Some("Serializable"));
        if !is_ser {
            return None;
        }
        self.classes
            .get(&class_internal(self.afile, simple))
            .map(|ci| self.ir.classes[ci.id as usize].fq_name.clone())
    }

    /// `C.serializer()` as an `invokestatic C.serializer()LKSerializer;` (the plugin fills the body
    /// before emit) — the same form the explicit `C.serializer()` call lowers to.
    fn serializer_crossfile(&mut self, c_internal: &str) -> u32 {
        let ret = ty_to_ir(Ty::obj_args(
            "kotlinx/serialization/KSerializer",
            &[Ty::obj(c_internal)],
        ));
        self.ir.add_expr(IrExpr::Call {
            callee: Callee::CrossFile {
                facade: c_internal.to_string(),
                name: "serializer".to_string(),
                params: vec![],
                ret,
            },
            dispatch_receiver: None,
            args: vec![],
        })
    }

    /// Whether classpath type `internal` is a kotlinx `StringFormat` — detected structurally by the
    /// presence of the 2-arg `encodeToString(SerializationStrategy, Any)` member (no hardcoded subtype).
    fn is_string_format(&self, internal: &str) -> bool {
        crate::call_resolver::resolve_instance(
            &*self.syms.libraries,
            internal,
            "encodeToString",
            &[
                Ty::obj("kotlinx/serialization/SerializationStrategy"),
                Ty::obj("kotlin/Any"),
            ],
        )
        .is_some()
    }

    /// Desugar a REIFIED kotlinx serialization round-trip call — `fmt.encodeToString(x)` /
    /// `fmt.decodeFromString<C>(s)` — into the 2-arg member with a synthesized `C.serializer()`. The
    /// reified inline form can't be called directly (`UnsupportedOperationException` at runtime), so
    /// krusty rewrites it the way kotlinc's inliner would. `None` (fall through) unless the receiver is a
    /// `StringFormat` and `C` is a `@Serializable` user class.
    fn try_reified_serial(
        &mut self,
        receiver: AstExprId,
        name: &str,
        args: &[AstExprId],
        call: AstExprId,
    ) -> Option<u32> {
        if args.len() != 1 {
            return None;
        }
        let fmt = self.info.ty(receiver).obj_internal()?.to_string();
        if !self.is_string_format(&fmt) {
            return None;
        }
        match name {
            "encodeToString" => {
                let c = self.serializable_internal(self.info.ty(args[0]))?;
                let recv = self.expr(receiver)?;
                let ser = self.serializer_crossfile(&c);
                let val = self.expr(args[0])?;
                Some(self.ir.add_expr(IrExpr::Call {
                    callee: Callee::Virtual {
                        owner: fmt,
                        name: "encodeToString".to_string(),
                        descriptor:
                            "(Lkotlinx/serialization/SerializationStrategy;Ljava/lang/Object;)Ljava/lang/String;"
                                .to_string(),
                        interface: false,
                    },
                    dispatch_receiver: Some(recv),
                    args: vec![ser, val],
                }))
            }
            "decodeFromString" => {
                // The decoded type is the explicit type argument `<C>`.
                let targ = self
                    .afile
                    .call_type_args
                    .get(&call.0)
                    .and_then(|ts| ts.first())
                    .and_then(|tr| self.ty_ref(tr))?;
                let c = self.serializable_internal(targ)?;
                let recv = self.expr(receiver)?;
                let ser = self.serializer_crossfile(&c);
                let s = self.expr(args[0])?;
                let decoded = self.ir.add_expr(IrExpr::Call {
                    callee: Callee::Virtual {
                        owner: fmt,
                        name: "decodeFromString".to_string(),
                        descriptor:
                            "(Lkotlinx/serialization/DeserializationStrategy;Ljava/lang/String;)Ljava/lang/Object;"
                                .to_string(),
                        interface: false,
                    },
                    dispatch_receiver: Some(recv),
                    args: vec![ser, s],
                });
                // The 2-arg member returns erased `Object`; checkcast to `C`.
                Some(self.coerce_erased(decoded, Ty::obj(&c), Ty::obj("kotlin/Any")))
            }
            _ => None,
        }
    }

    /// Lower a call's arguments, filling omitted trailing parameters from their **constant-literal**
    /// defaults (`fun f(x: Int = 5)` called `f()`). A non-literal default (one referencing other
    /// params or `this`) needs the `$default` synthetic method krusty doesn't emit yet → `None`.
    fn lower_args_defaulted(
        &mut self,
        call: AstExprId,
        param_meta: &[(String, Option<AstExprId>)],
        args: &[AstExprId],
        ir_params: &[Ty],
    ) -> Option<Vec<u32>> {
        let n = ir_params.len();
        if args.len() > n {
            return None;
        }
        // Place each argument into its parameter slot: a positional arg fills the next free position;
        // a named arg (`x = …`) fills its named parameter. Unfilled slots take constant-literal
        // defaults. (Arguments are evaluated in slot order — fine for the side-effect-free common case.)
        let names = self
            .afile
            .call_arg_names
            .get(&call.0)
            .cloned()
            .unwrap_or_default();
        let mut slot: Vec<Option<AstExprId>> = vec![None; n];
        let mut pos = 0;
        // The slot each SOURCE-order argument lands in. Kotlin evaluates arguments in source order; this
        // helper lowers them in slot order, so a named-argument call that REORDERS evaluation
        // (`f(b = …, a = …)`) would run side effects out of order. Detect a non-monotonic placement and,
        // if any reordered argument may have side effects, skip (proper source-order temp-spilling isn't
        // modeled yet) — pure reordered arguments (const/name reads) are order-independent and proceed.
        let mut arg_slot: Vec<usize> = Vec::with_capacity(args.len());
        for (i, &arg) in args.iter().enumerate() {
            match names.get(i).and_then(|o| o.as_ref()) {
                None => {
                    if pos >= n {
                        return None;
                    }
                    slot[pos] = Some(arg);
                    arg_slot.push(pos);
                    pos += 1;
                }
                Some(nm) => {
                    let idx = param_meta.iter().position(|(name, _)| name == nm)?;
                    if idx >= n || slot[idx].is_some() {
                        return None;
                    }
                    slot[idx] = Some(arg);
                    arg_slot.push(idx);
                }
            }
        }
        let reordered = arg_slot.windows(2).any(|w| w[0] > w[1]);
        if reordered
            && args.iter().any(|&a| {
                !is_const_literal(self.afile, a) && !matches!(self.afile.expr(a), Expr::Name(_))
            })
        {
            return None;
        }
        let mut a = Vec::new();
        for (k, pt) in ir_params.iter().enumerate() {
            match slot[k] {
                Some(arg) => a.push(self.lower_arg(arg, pt)?),
                None => {
                    let def = param_meta.get(k).and_then(|(_, d)| *d)?;
                    if !is_const_literal(self.afile, def) {
                        return None;
                    }
                    a.push(self.lower_arg(def, pt)?);
                }
            }
        }
        Some(a)
    }

    /// Reorder a NAMED-argument call to a CLASSPATH top-level function (`foo(b = …, a = …)`) into
    /// declared-parameter order, so the positional lowering below sees the arguments positionally. The
    /// parameter names come from the callee's `@Metadata` (a classpath function isn't in `fun_ids`/
    /// `fn_facades`, so the same-file/module named-arg paths don't apply). Returns the reordered AST
    /// argument ids, or `None` when this isn't a uniquely-resolvable classpath named call (then the caller
    /// leaves the args untouched). Conservative: requires a SINGLE top-level overload carrying names, an
    /// exact-arity call (no omitted defaults), every label to be a known parameter, and — when reordering
    /// changes evaluation order — every argument to be side-effect-free (a const/name), matching
    /// `lower_args_defaulted`.
    fn reorder_classpath_named_args(
        &self,
        call: AstExprId,
        fname: &str,
        args: &[AstExprId],
    ) -> Option<Vec<AstExprId>> {
        // The single classpath top-level overload with recorded parameter names (federated library set —
        // a classpath function is not a module function, so `ModuleSymbols` wouldn't surface it).
        let sets: Vec<Vec<String>> = self
            .syms
            .libraries
            .functions(fname, None)
            .overloads
            .into_iter()
            .filter(|o| {
                o.kind == crate::libraries::FnKind::TopLevel && !o.call_sig.param_names.is_empty()
            })
            .map(|o| o.call_sig.param_names)
            .collect();
        let [param_names] = sets.as_slice() else {
            return None;
        };
        self.reorder_by_param_names(call, args, param_names)
    }

    /// Reorder a NAMED-argument call to a CLASSPATH instance MEMBER or EXTENSION (`g.greet(b = …, a = …)`
    /// / `"s".tag(b = …, a = …)`) into declared-parameter order, from the callee's `@Metadata` names
    /// (a single `Member`/`Extension` overload's `CallSig.param_names` — the LOGICAL params, receiver
    /// excluded). `rt` is the receiver type. `None` when not a uniquely-resolvable classpath named call.
    fn reorder_classpath_named_member_args(
        &self,
        call: AstExprId,
        rt: Ty,
        name: &str,
        args: &[AstExprId],
    ) -> Option<Vec<AstExprId>> {
        let sets: Vec<Vec<String>> = self
            .syms
            .libraries
            .functions(name, Some(rt))
            .overloads
            .into_iter()
            .filter(|o| {
                matches!(
                    o.kind,
                    crate::libraries::FnKind::Member | crate::libraries::FnKind::Extension
                ) && !o.call_sig.param_names.is_empty()
            })
            .map(|o| o.call_sig.param_names)
            .collect();
        let [param_names] = sets.as_slice() else {
            return None;
        };
        self.reorder_by_param_names(call, args, param_names)
    }

    /// Map a named/positional argument list onto `param_names`-ordered positions (the shared core of the
    /// top-level and member classpath named-argument reorders). Requires an exact-arity call (no omitted
    /// defaults), every label to name a parameter, and — when the placement reorders evaluation — every
    /// argument to be side-effect-free (a const/name), matching `lower_args_defaulted`. `None` otherwise.
    fn reorder_by_param_names(
        &self,
        call: AstExprId,
        args: &[AstExprId],
        param_names: &[String],
    ) -> Option<Vec<AstExprId>> {
        let names = self.afile.call_arg_names.get(&call.0)?;
        let n = param_names.len();
        if args.len() != n {
            return None;
        }
        let mut slot: Vec<Option<AstExprId>> = vec![None; n];
        let mut pos = 0usize;
        let mut arg_slot: Vec<usize> = Vec::with_capacity(args.len());
        for (i, &arg) in args.iter().enumerate() {
            match names.get(i).and_then(|o| o.as_ref()) {
                None => {
                    if pos >= n || slot[pos].is_some() {
                        return None;
                    }
                    slot[pos] = Some(arg);
                    arg_slot.push(pos);
                    pos += 1;
                }
                Some(nm) => {
                    let idx = param_names.iter().position(|p| p == nm)?;
                    if slot[idx].is_some() {
                        return None;
                    }
                    slot[idx] = Some(arg);
                    arg_slot.push(idx);
                }
            }
        }
        // Reordering changes evaluation order; only proceed when each argument is side-effect-free.
        let reordered = arg_slot.windows(2).any(|w| w[0] > w[1]);
        if reordered
            && args.iter().any(|&a| {
                !is_const_literal(self.afile, a) && !matches!(self.afile.expr(a), Expr::Name(_))
            })
        {
            return None;
        }
        slot.into_iter().collect()
    }

    /// The receiver's class type for member access. The checker types a bare `object` name as
    /// `Error` (it's only a qualifier), so map an object-name receiver to its object type; otherwise
    /// use the checker's inferred type.
    fn recv_ty(&self, receiver: AstExprId) -> Ty {
        if let Expr::Name(rn) = self.afile.expr(receiver) {
            let internal = class_internal(self.afile, rn);
            if self
                .classes
                .get(&internal)
                .map_or(false, |ci| self.ir.classes[ci.id as usize].is_object)
            {
                return Ty::obj(&internal);
            }
        }
        self.info.ty(receiver)
    }

    /// An arithmetic operator member of a primitive numeric type called by its METHOD name
    /// (`a.plus(b)`, `a.times(b)`, … — valid Kotlin, identical to `a + b`). The checker already typed it;
    /// lower it to the same `PrimitiveBinOp` the operator form produces (with mixed-operand promotion and
    /// the unsigned `div`/`rem` intrinsics). Returns `None` for a name/receiver this doesn't model.
    fn lower_prim_op_method(&mut self, recv: AstExprId, name: &str, arg: AstExprId) -> Option<u32> {
        let op = match name {
            "plus" => BinOp::Add,
            "minus" => BinOp::Sub,
            "times" => BinOp::Mul,
            "div" => BinOp::Div,
            "rem" => BinOp::Rem,
            _ => return None,
        };
        let (lt, rt) = (self.info.ty(recv), self.info.ty(arg));
        if !lt.is_primitive() || !rt.is_primitive() || lt == Ty::Boolean || rt == Ty::Boolean {
            return None;
        }
        // Unsigned `div`/`rem` need the JDK unsigned intrinsics (signed `+`/`-`/`*` share opcodes).
        if lt.is_unsigned() && matches!(op, BinOp::Div | BinOp::Rem) {
            let is_uint = lt == Ty::UInt;
            let owner = if is_uint {
                "java/lang/Integer"
            } else {
                "java/lang/Long"
            };
            let prim = if is_uint { "I" } else { "J" };
            let l = self.expr(recv)?;
            let r = self.expr(arg)?;
            let mname = if op == BinOp::Div {
                "divideUnsigned"
            } else {
                "remainderUnsigned"
            };
            return Some(self.ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: owner.to_string(),
                    name: mname.to_string(),
                    descriptor: format!("({prim}{prim}){prim}"),
                    inline: false,
                    must_inline: false,
                },
                dispatch_receiver: None,
                args: vec![l, r],
            }));
        }
        let irop = bin_to_ir(op)?;
        let mut l = self.expr(recv)?;
        let mut r = self.expr(arg)?;
        // `Char` arithmetic (`'a'.plus(1)`): operate on ints (no promotion between Char/Int), then
        // truncate back to `Char` if the result is a `Char` — mirrors the `Expr::Binary` Char path.
        if lt == Ty::Char
            && matches!(op, BinOp::Add | BinOp::Sub)
            && (rt == Ty::Int || rt == Ty::Char)
        {
            let int_ir = ty_to_ir(Ty::Int);
            l = self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: l,
                type_operand: int_ir.clone(),
            });
            if rt == Ty::Char {
                r = self.ir.add_expr(IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    arg: r,
                    type_operand: int_ir,
                });
            }
            let raw = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                op: irop,
                lhs: l,
                rhs: r,
            });
            return Some(raw);
        }
        // Mixed numeric operands (`1L.plus(2)`): promote both to the common type before the op.
        if lt != rt {
            let p = Ty::promote(lt, rt)?;
            let pir = ty_to_ir(p);
            if lt != p {
                l = self.ir.add_expr(IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    arg: l,
                    type_operand: pir.clone(),
                });
            }
            if rt != p {
                r = self.ir.add_expr(IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    arg: r,
                    type_operand: pir,
                });
            }
        }
        Some(self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: irop,
            lhs: l,
            rhs: r,
        }))
    }

    /// Resolve a field by name, walking the superclass chain. Returns the *owning* class id, the
    /// field index within that class, and its type.
    fn resolve_field(&self, internal: &str, name: &str) -> Option<(ClassId, u32, Ty)> {
        let mut cur = Some(internal.to_string());
        while let Some(ci_name) = cur {
            let ci = self.classes.get(&ci_name)?;
            if let Some(idx) = ci.fields.iter().position(|(fn_, _)| fn_ == name) {
                // A custom-accessor property is never a direct field read: decline it so the caller
                // routes the access through `getX`/`setX` (the accessor's own `field` reaches the
                // field via `cur_field`, not this resolver).
                if self
                    .field_accessor_props
                    .contains(&(ci_name.clone(), name.to_string()))
                {
                    return None;
                }
                return Some((ci.id, idx as u32, ci.fields[idx].1));
            }
            cur = ci.super_internal.clone();
        }
        None
    }

    /// All `(method_name, FunId)` declared by an interface and its super-interfaces (transitively).
    fn collect_iface_methods(&self, itf: &str) -> Vec<(String, u32)> {
        let mut out = Vec::new();
        let mut stack = vec![itf.to_string()];
        let mut seen = std::collections::HashSet::new();
        while let Some(i) = stack.pop() {
            if !seen.insert(i.clone()) {
                continue;
            }
            if let Some(ci) = self.classes.get(&i) {
                for (name, &(_, fid, _)) in &ci.methods {
                    out.push((name.clone(), fid));
                }
                for sup in self.ir.classes[ci.id as usize].interfaces.clone() {
                    stack.push(sup);
                }
            }
        }
        out
    }

    /// Resolve a method by name, walking the superclass chain. Returns the *owning* class id, the
    /// method index within that class, its FunId, and its return type.
    /// Lower an unbound property reference `Type::prop` (typed `KProperty1`) to a synthesized
    /// `PropertyReference1Impl` singleton, read as its `INSTANCE`. Bound (`obj::prop`) and mutable
    /// (`KMutableProperty*`) references aren't modeled yet (`None` ⇒ skip).
    fn lower_prop_ref(&mut self, e: AstExprId, recv: AstExprId, name: &str) -> Option<u32> {
        // Unbound `Type::prop` (a `KProperty1` singleton) or bound `obj::prop` (a `KProperty0` carrying
        // the captured receiver).
        let bound = match self.info.ty(e).obj_internal()? {
            "kotlin/reflect/KProperty1" => false,
            "kotlin/reflect/KProperty0" => true,
            _ => return None,
        };
        let Expr::Name(rn) = self.afile.expr(recv).clone() else {
            return None;
        };
        // Bound: the receiver is an in-scope value; its type gives the owner. Unbound: `rn` is the class.
        let (owner, recv_val) = if bound {
            let (v, ty) = self.lookup(&rn)?;
            (ty.obj_internal()?.to_string(), Some(v))
        } else {
            (class_internal(self.afile, &rn), None)
        };
        let owner_id = self.classes.get(&owner)?.id;
        let prop_ty = {
            let cls = &self.ir.classes[owner_id as usize];
            let idx = cls.fields.iter().position(|f| f.name == *name)?;
            cls.fields[idx].ty.clone()
        };
        let synth_fq = class_internal(
            self.afile,
            &format!("{}$propref${}${}", self.cur_fn_name, name, self.lambda_seq),
        );
        self.lambda_seq += 1;
        let superclass = if bound {
            "kotlin/jvm/internal/PropertyReference0Impl"
        } else {
            "kotlin/jvm/internal/PropertyReference1Impl"
        };
        let synth_id = self.ir.add_class(IrClass {
            fq_name: synth_fq,
            serial_names: Vec::new(),
            custom_serializer: None,
            field_serializers: Vec::new(),
            contextual_fields: Vec::new(),
            is_value: false,
            type_param_bounds: vec![],
            type_params: Vec::new(),
            supertypes: vec![],
            fields: vec![],
            ctor_param_count: 0,
            ctor_args: vec![],
            init_body: None,
            explicit_param_stores: false,
            methods: vec![],
            is_interface: false,
            is_annotation: false,
            annotation_impl_of: None,

            is_sealed: false,
            is_abstract: false,
            superclass: superclass.to_string(),
            super_args: vec![],
            enum_entries: vec![],
            enum_entry_subclass: vec![],
            enum_entry_of: None,
            prop_ref: Some(crate::ir::PropRef {
                owner_internal: owner,
                prop_name: name.to_string(),
                getter_name: getter_name(name),
                prop_ty,
                bound,
            }),
            func_ref: None,
            bridges: vec![],
            interfaces: vec![],
            is_object: false,
            ctor_param_checks: vec![],
            is_companion: false,
            companion_class: None,
            secondary_ctors: vec![],
            has_primary_ctor: true,
        });
        if let Some(v) = recv_val {
            // `new <Synth>(receiver)` — the captured receiver is the constructor's `Object` argument.
            let recv_e = self.ir.add_expr(IrExpr::GetValue(v));
            Some(self.ir.add_expr(IrExpr::New {
                class: synth_id,
                args: vec![recv_e],
                ctor_params: Some(vec![ty_to_ir(Ty::obj("kotlin/Any"))]),
            }))
        } else {
            Some(self.ir.add_expr(IrExpr::StaticInstance {
                owner: synth_id,
                ty: synth_id,
                field: "INSTANCE",
            }))
        }
    }

    /// Lower a method reference `obj::m` (bound — the receiver is an in-scope value, captured) or
    /// `Type::m` (unbound — the receiver is a user class, supplied as the first argument) to a
    /// synthesized static impl `(receiver, args…) -> receiver.m(args)` wrapped in a closure, exactly
    /// as a lambda `{ a -> obj.m(a) }` / `{ r, a -> r.m(a) }` would lower. `params`/`ret` are the
    /// reference's function type. Only user-class methods are modeled (the receiver/method must
    /// resolve in the IR class table); a `Unit`/`Nothing` return is skipped.
    /// Wrap a `Unit`-returning target function in a SAM impl `(target params) -> { target(params);
    /// return Unit.INSTANCE }`. A `Unit` method handle returns `void`, but a functional interface's
    /// `invoke` must yield the `kotlin/Unit` singleton — `LambdaMetafactory` won't adapt `void`. `uniq`
    /// (the ref's AST id) keeps the synthesized name distinct across overloaded enclosing functions.
    fn unit_ref_wrapper(&mut self, target_fid: u32, uniq: u32) -> u32 {
        let params = self.ir.functions[target_fid as usize].params.clone();
        let argvals: Vec<u32> = (0..params.len() as u32)
            .map(|i| self.ir.add_expr(IrExpr::GetValue(i)))
            .collect();
        let call = self.ir.add_expr(IrExpr::Call {
            callee: Callee::Local(target_fid),
            dispatch_receiver: None,
            args: argvals,
        });
        let unit = self.ir.add_expr(IrExpr::UnitInstance);
        let ret_e = self.ir.add_expr(IrExpr::Return(Some(unit)));
        let block = self.ir.add_expr(IrExpr::Block {
            stmts: vec![call, ret_e],
            value: None,
        });
        let impl_name = format!("{}$unitref${}", self.cur_fn_name, uniq);
        self.ir.add_fun(IrFunction {
            name: impl_name,
            params,
            ret: ty_to_ir(Ty::obj("kotlin/Unit")),
            body: Some(block),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        })
    }

    fn lower_method_ref(
        &mut self,
        e: AstExprId,
        recv: AstExprId,
        name: &str,
        params: &[Ty],
        ret: Ty,
    ) -> Option<u32> {
        if ret == Ty::Nothing {
            return None;
        }
        let Expr::Name(rn) = self.afile.expr(recv).clone() else {
            return None;
        };
        // Bound `obj::m` (`rn` an in-scope value) / `O::m` (`rn` an object → its `INSTANCE`): the
        // receiver is CAPTURED. Unbound `Type::m` (`rn` a class): the receiver is the reference's first
        // parameter (`param_tys[0]`). `capture` holds the bound receiver expression.
        let (capture, recv_ty): (Option<u32>, Ty) = match self.lookup(&rn) {
            Some((v, ty)) => (Some(self.ir.add_expr(IrExpr::GetValue(v))), ty),
            None => {
                let internal = class_internal(self.afile, &rn);
                let cid = self.classes.get(&internal)?.id;
                if self.ir.classes[cid as usize].is_object {
                    let inst = self.ir.add_expr(IrExpr::StaticInstance {
                        owner: cid,
                        ty: cid,
                        field: "INSTANCE",
                    });
                    (Some(inst), Ty::obj(&internal))
                } else {
                    (None, *params.first()?)
                }
            }
        };
        let internal = recv_ty.obj_internal()?.to_string();
        // Only a user-class method is modeled (the invoke does `invokevirtual`/`invokeinterface
        // internal.name`); a classpath/library receiver fails here → bail (skip), never miscompile.
        self.resolve_method(&internal, name)?;
        // Dispatch on the receiver's STATIC type: an interface receiver needs `invokeinterface`.
        let call_interface = self
            .classes
            .get(&internal)
            .is_some_and(|ci| self.ir.classes[ci.id as usize].is_interface);
        let bound = capture.is_some();
        let dispatch = if bound {
            crate::ir::FrDispatch::VirtualBound
        } else {
            crate::ir::FrDispatch::VirtualUnbound
        };
        let param_tys: Vec<Ty> = params.iter().map(|t| ty_to_ir(*t)).collect();
        Some(self.make_func_ref(
            e.0,
            bound,
            params.len() as u8,
            internal.clone(),
            name.to_string(),
            0, // member
            dispatch,
            internal,
            name.to_string(),
            call_interface,
            param_tys,
            ty_to_ir(ret),
            capture,
        ))
    }

    /// Bound callable reference on an arbitrary EXPRESSION receiver (`"abc"::get`, `1::foo`, `mk()::m`):
    /// the receiver is evaluated once and captured into the closure. Handles a bound extension function
    /// (`expr::extFun` → the lifted static `extFun(recv, args…)`, captured receiver) and a bound member
    /// on a user-class receiver (a synthesized impl invoking the member). Returns None for unmodeled
    /// shapes (library-type members, `Unit`/`Nothing` SAM-return).
    fn lower_bound_expr_ref(
        &mut self,
        e: AstExprId,
        recv: AstExprId,
        name: &str,
        params: &[Ty],
        ret: Ty,
    ) -> Option<u32> {
        if ret == Ty::Nothing {
            return None;
        }
        let rty = self.info.ty(recv);
        // Bound extension reference: the extension is a lifted static `name(recv, args…)`. Capture the
        // receiver; the metafactory binds it and `invoke(args)` supplies the rest (same as a local-fun
        // ref). `arity` = the extension's declared params (the receiver is bound, not a parameter). A
        // `Unit` return is wrapped so `invoke` yields the `Unit` singleton.
        if let Some(&fid) = self.ext_fun_ids.get(&(rty.descriptor(), name.to_string())) {
            let cap = self.expr(recv)?;
            let impl_fn = if ret == Ty::Unit {
                self.unit_ref_wrapper(fid, e.0)
            } else {
                fid
            };
            return Some(self.ir.add_expr(IrExpr::Lambda {
                impl_fn,
                arity: params.len() as u8,
                captures: vec![cap],
                sam: None,
                inline_body: None,
            }));
        }
        // Bound member reference on a user-class receiver: synthesize `(recv, args…) -> recv.name(args…)`
        // and capture the receiver. (Library-type members aren't IR classes → `resolve_method` fails.)
        let internal = rty.obj_internal()?.to_string();
        let (class_id, index, _fid, _ret) = self.resolve_method(&internal, name)?;
        let cap = self.expr(recv)?;
        let recv_v = self.ir.add_expr(IrExpr::GetValue(0));
        let arg_vs: Vec<Option<u32>> = (0..params.len() as u32)
            .map(|i| Some(self.ir.add_expr(IrExpr::GetValue(i + 1))))
            .collect();
        let mc = self.ir.add_expr(IrExpr::MethodCall {
            class: class_id,
            index,
            receiver: recv_v,
            args: arg_vs,
        });
        let (stmts, impl_ret) = if ret == Ty::Unit {
            let unit = self.ir.add_expr(IrExpr::UnitInstance);
            let ret_e = self.ir.add_expr(IrExpr::Return(Some(unit)));
            (vec![mc, ret_e], ty_to_ir(Ty::obj("kotlin/Unit")))
        } else {
            let ret_e = self.ir.add_expr(IrExpr::Return(Some(mc)));
            (vec![ret_e], ty_to_ir(ret))
        };
        let block = self.ir.add_expr(IrExpr::Block { stmts, value: None });
        // Name with the ref's globally-unique AST expr id (not the per-function `lambda_seq`): two
        // OVERLOADED enclosing functions share `cur_fn_name`, so a seq-based name would clash.
        let impl_name = format!("{}$boundref${}", self.cur_fn_name, e.0);
        let mut impl_params: Vec<Ty> = vec![ty_to_ir(rty)];
        impl_params.extend(params.iter().map(|t| ty_to_ir(*t)));
        let bfid = self.ir.add_fun(IrFunction {
            name: impl_name,
            params: impl_params,
            ret: impl_ret,
            body: Some(block),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        Some(self.ir.add_expr(IrExpr::Lambda {
            impl_fn: bfid,
            arity: params.len() as u8,
            captures: vec![cap],
            sam: None,
            inline_body: None,
        }))
    }

    /// Build a synthesized function-reference subclass of `FunctionReferenceImpl` (real Kotlin reference
    /// EQUALITY) and return the expression that produces an instance: `new <Synth>(receiver)` for a bound
    /// ref, or `<Synth>.INSTANCE` for an unbound one. See `crate::ir::FuncRef` / `emit_func_ref_class`.
    #[allow(clippy::too_many_arguments)]
    fn make_func_ref(
        &mut self,
        uniq: u32,
        bound: bool,
        arity: u8,
        owner_class: String,
        fn_name: String,
        flags: i32,
        dispatch: crate::ir::FrDispatch,
        call_owner: String,
        call_name: String,
        call_interface: bool,
        param_tys: Vec<Ty>,
        ret_ty: Ty,
        capture: Option<u32>,
    ) -> u32 {
        let synth_fq = class_internal(self.afile, &format!("{}$fnref${}", self.cur_fn_name, uniq));
        let synth_id = self.ir.add_class(IrClass {
            fq_name: synth_fq,
            serial_names: Vec::new(),
            custom_serializer: None,
            field_serializers: Vec::new(),
            contextual_fields: Vec::new(),
            is_value: false,
            type_param_bounds: vec![],
            type_params: Vec::new(),
            supertypes: vec![],
            fields: vec![],
            ctor_param_count: 0,
            ctor_args: vec![],
            init_body: None,
            explicit_param_stores: false,
            methods: vec![],
            is_interface: false,
            is_annotation: false,
            annotation_impl_of: None,

            is_sealed: false,
            is_abstract: false,
            superclass: "kotlin/jvm/internal/FunctionReferenceImpl".to_string(),
            super_args: vec![],
            enum_entries: vec![],
            enum_entry_subclass: vec![],
            enum_entry_of: None,
            prop_ref: None,
            func_ref: Some(crate::ir::FuncRef {
                bound,
                arity,
                owner_class,
                fn_name,
                flags,
                dispatch,
                call_owner,
                call_name,
                call_interface,
                param_tys,
                ret_ty,
            }),
            bridges: vec![],
            interfaces: vec![],
            is_object: false,
            ctor_param_checks: vec![],
            is_companion: false,
            companion_class: None,
            secondary_ctors: vec![],
            has_primary_ctor: true,
        });
        match capture {
            Some(cap) => self.ir.add_expr(IrExpr::New {
                class: synth_id,
                args: vec![cap],
                ctor_params: Some(vec![ty_to_ir(Ty::obj("kotlin/Any"))]),
            }),
            None => self.ir.add_expr(IrExpr::StaticInstance {
                owner: synth_id,
                ty: synth_id,
                field: "INSTANCE",
            }),
        }
    }

    /// For an unqualified call inside an inner class, resolve `name` as an ENCLOSING method (reached
    /// through `this$0`). Returns `(method_class, method_index, method_fid, inner_class_id)`.
    fn inner_outer_method(&self, name: &str) -> Option<(ClassId, u32, u32, ClassId)> {
        let cur = self.cur_class.as_ref()?;
        let cur_id = self.classes.get(cur)?.id;
        let outer = match self.ir.classes[cur_id as usize].fields.first() {
            Some(IrField { name: n0, ty, .. })
                if n0 == "this$0" && ty.non_null().obj_internal().is_some() =>
            {
                ty.non_null().obj_internal().unwrap().to_string()
            }
            _ => return None,
        };
        let (c, i, f, _) = self.resolve_method(&outer, name)?;
        Some((c, i, f, cur_id))
    }

    fn resolve_method(&self, internal: &str, name: &str) -> Option<(ClassId, u32, u32, Ty)> {
        let mut cur = Some(internal.to_string());
        while let Some(ci_name) = cur {
            // Stop at a non-IR (classpath) super rather than aborting the whole lookup — the interface
            // default-method fallback below still applies.
            let Some(ci) = self.classes.get(&ci_name) else {
                break;
            };
            if let Some(&(idx, fid, ret)) = ci.methods.get(name) {
                return Some((ci.id, idx, fid, ret));
            }
            cur = ci.super_internal.clone();
        }
        // Inherited interface DEFAULT method (`class C : I` calling `I`'s `fun f() = …` it doesn't
        // override): search the class's (and supers') interfaces transitively for a method WITH a body.
        // Returns the interface's class id, so the call emits `invokeinterface` on the receiver.
        // A VALUE-class receiver (erased to its underlying type) needs boxing to dispatch an interface
        // default — not modeled here, so skip the fallback (the caller bails → file skipped, not wrong).
        if self
            .syms
            .class_by_internal(internal)
            .is_some_and(|c| c.value_field.is_some())
        {
            return None;
        }
        let mut stack = vec![internal.to_string()];
        let mut seen = std::collections::HashSet::new();
        while let Some(cn) = stack.pop() {
            if !seen.insert(cn.clone()) {
                continue;
            }
            let Some(ci) = self.classes.get(&cn) else {
                continue;
            };
            if let Some(sup) = &ci.super_internal {
                stack.push(sup.clone());
            }
            for itf in &self.ir.classes[ci.id as usize].interfaces {
                if let Some(ici) = self.classes.get(itf) {
                    // Only a genuine DEFAULT method (a body in the source) is a valid inherited target.
                    // An ABSTRACT interface method reached here (e.g. via class delegation `by`, where
                    // the class neither defines nor IR-registers it) would emit an `invokeinterface` to an
                    // unimplemented method (`AbstractMethodError`). Check the AST (order-independent; the
                    // IR body is set later in pass 2).
                    if self.iface_method_is_default(itf, name) {
                        if let Some(&(idx, fid, ret)) = ici.methods.get(name) {
                            return Some((ici.id, idx, fid, ret));
                        }
                    }
                }
                stack.push(itf.clone());
            }
        }
        None
    }

    /// Find an interface method `name` (in `internal`'s transitive interface hierarchy) that declares
    /// DEFAULT ARGUMENTS (a registered `fn_param_defaults` — i.e. a `$default` stub is emitted on that
    /// interface). Used to resolve an OMITTED-argument call `impl.foo()` whose default is declared on a
    /// super-interface and not redeclared on the override. Returns the INTERFACE's class id (so the call
    /// targets `<iface>.foo$default`).
    fn resolve_defaulted_iface_method(
        &self,
        internal: &str,
        name: &str,
    ) -> Option<(ClassId, u32, u32, Ty)> {
        let mut stack = vec![internal.to_string()];
        let mut seen = std::collections::HashSet::new();
        let mut found: Vec<(ClassId, u32, u32, Ty)> = Vec::new();
        while let Some(cn) = stack.pop() {
            if !seen.insert(cn.clone()) {
                continue;
            }
            let Some(ci) = self.classes.get(&cn) else {
                continue;
            };
            if let Some(sup) = &ci.super_internal {
                stack.push(sup.clone());
            }
            for itf in &self.ir.classes[ci.id as usize].interfaces {
                if let Some(ici) = self.classes.get(itf) {
                    if let Some(&(idx, fid, ret)) = ici.methods.get(name) {
                        if self.ir.fn_param_defaults.contains_key(&fid)
                            && !found.iter().any(|(_, _, f, _)| *f == fid)
                        {
                            found.push((ici.id, idx, fid, ret));
                        }
                    }
                }
                stack.push(itf.clone());
            }
        }
        // Exactly one interface declares the default → use it. Multiple distinct ones (a diamond like
        // `class C : A, B` where both `A` and `B` default the parameter) is an ambiguity whose Kotlin
        // resolution isn't modeled — bail (the file is SKIPPED, never miscompiled).
        match found.as_slice() {
            [one] => Some(*one),
            _ => None,
        }
    }

    /// Whether a same-file interface's method is a DEFAULT method (declared with a body) — checked on
    /// the AST so it's independent of pass-2 lowering order.
    fn iface_method_is_default(&self, iface_internal: &str, name: &str) -> bool {
        self.afile.decls.iter().any(|&d| {
            matches!(self.afile.decl(d), Decl::Class(c) if c.is_interface()
                && class_internal(self.afile, &c.name) == iface_internal
                && c.methods.iter().any(|m| m.name == name && !matches!(m.body, FunBody::None)))
        })
    }

    /// Lower a call argument, inserting an explicit `ImplicitCoercion` when a primitive must box
    /// into a reference parameter (`Int` → `Any`) or a wrapper must unbox into a primitive param.
    /// Box/unbox is the backend's concern, but the *coercion* is explicit in the IR.
    /// Whether `e` is the `emptyArray()` reified intrinsic call — a bare `emptyArray(...)` with no
    /// arguments. kotlinc treats `emptyArray`/`arrayOfNulls`/`arrayOf` as codegen intrinsics; this
    /// recognizes the empty case so it can be specialized to the target element type at the use site.
    fn is_empty_array_intrinsic(&self, e: AstExprId) -> bool {
        if let Expr::Call { callee, args } = self.afile.expr(e) {
            if args.is_empty() {
                if let Expr::Name(n) = self.afile.expr(*callee) {
                    // `emptyArray` is a compiler intrinsic (no stdlib body) recognized by resolved symbol;
                    // a user-defined function or local of that name shadows it, exactly as in kotlinc.
                    return n == "emptyArray"
                        && self.lookup(n).is_none()
                        && !self.module_declares(n);
                }
            }
        }
        false
    }

    /// Lower `for (x in iterable)` over a non-array `iterable` via the Kotlin iterator protocol:
    /// `val it = iterable.iterator(); while (it.hasNext()) { val x = it.next(); body }`. The element
    /// type comes from the iterable's generic argument (`List<Int>` → `Int`); `next()` returns the
    /// erased `Object`, so a primitive element unboxes and a specific reference checkcasts. Bails
    /// (skip) if the iterator methods or the element type can't be resolved.
    /// `for (x in range)` over an `IntRange`/`LongRange`/`CharRange` value → a counted loop:
    /// `last = range.getLast(); i = range.getFirst(); while (i <= last) { x = i; …; i++ }` (step +1).
    /// The bounds are read once via the virtual getters; element/counter are the unboxed primitive.
    fn lower_foreach_range(
        &mut self,
        name: &str,
        iterable: AstExprId,
        body: AstExprId,
        it_ty: Ty,
        elem: Ty,
        prim_desc: &str,
        label: Option<String>,
    ) -> Option<u32> {
        let internal = it_ty.obj_internal()?.to_string();
        let depth = self.scope.len();
        let elem_ir = ty_to_ir(elem);
        // The `first`/`last` getters: a signed range names them `getFirst`/`getLast`; an unsigned range's
        // are mangled inline-class members (`getFirst-pVg5ArA`), looked up from the classpath by prefix.
        let (gf_name, gf_desc, gl_name, gl_desc) = if elem.is_unsigned() {
            let (gf, gfd) = self.syms.libraries.mangled_member(&internal, "getFirst-")?;
            let (gl, gld) = self.syms.libraries.mangled_member(&internal, "getLast-")?;
            (gf, gfd, gl, gld)
        } else {
            (
                "getFirst".to_string(),
                format!("(){prim_desc}"),
                "getLast".to_string(),
                format!("(){prim_desc}"),
            )
        };
        // Evaluate the range once into a temp (the getters must share one receiver).
        let rng = self.expr(iterable)?;
        let r_v = self.fresh_value();
        let var_r = self.ir.add_expr(IrExpr::Variable {
            index: r_v,
            ty: ty_to_ir(it_ty),
            init: Some(rng),
        });
        let getter = |this: &mut Self, name: &str, desc: &str| {
            let recv = this.ir.add_expr(IrExpr::GetValue(r_v));
            this.ir.add_expr(IrExpr::Call {
                callee: Callee::Virtual {
                    owner: internal.clone(),
                    name: name.to_string(),
                    descriptor: desc.to_string(),
                    interface: false,
                },
                dispatch_receiver: Some(recv),
                args: vec![],
            })
        };
        // i = range.getFirst()
        let first = getter(self, &gf_name, &gf_desc);
        let i_v = self.fresh_value();
        self.scope.push((name.to_string(), i_v, elem));
        let var_i = self.ir.add_expr(IrExpr::Variable {
            index: i_v,
            ty: elem_ir.clone(),
            init: Some(first),
        });
        // last = range.getLast()  (hoisted)
        let last = getter(self, &gl_name, &gl_desc);
        let n_v = self.fresh_value();
        let var_n = self.ir.add_expr(IrExpr::Variable {
            index: n_v,
            ty: elem_ir.clone(),
            init: Some(last),
        });
        // condition: i <= last (unsigned: compareUnsigned(i, last) <= 0, so values past the sign bit
        // order correctly — a signed `<=` would end the loop early).
        let gi = self.ir.add_expr(IrExpr::GetValue(i_v));
        let gn = self.ir.add_expr(IrExpr::GetValue(n_v));
        let cond = if elem.is_unsigned() {
            let (owner, prim) = if elem == Ty::UInt {
                ("java/lang/Integer", "I")
            } else {
                ("java/lang/Long", "J")
            };
            let call = self.ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: owner.to_string(),
                    name: "compareUnsigned".to_string(),
                    descriptor: format!("({prim}{prim})I"),
                    inline: false,
                    must_inline: false,
                },
                dispatch_receiver: None,
                args: vec![gi, gn],
            });
            let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
            self.ir.add_expr(IrExpr::PrimitiveBinOp {
                op: IrBinOp::Le,
                lhs: call,
                rhs: zero,
            })
        } else {
            self.ir.add_expr(IrExpr::PrimitiveBinOp {
                op: IrBinOp::Le,
                lhs: gi,
                rhs: gn,
            })
        };
        // body (the loop variable `x` is the counter `i` itself)
        let mut out = Vec::new();
        if self.append_body_stmts(body, &mut out).is_none() {
            self.scope.truncate(depth);
            return None;
        }
        // i += 1  (the loop update, at the `continue` target)
        let gi2 = self.ir.add_expr(IrExpr::GetValue(i_v));
        let one = self
            .ir
            .add_expr(IrExpr::Const(if matches!(elem, Ty::Long | Ty::ULong) {
                IrConst::Long(1)
            } else {
                IrConst::Int(1)
            }));
        let inc = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Add,
            lhs: gi2,
            rhs: one,
        });
        let incs = self.ir.add_expr(IrExpr::SetValue {
            var: i_v,
            value: inc,
        });
        // Break when the counter reaches the inclusive last *before* incrementing, so a range ending at
        // `Int.MAX_VALUE`/`Long.MAX_VALUE` doesn't wrap past it and loop forever (same overflow-safe
        // counted-loop shape as `Stmt::For`). The break + increment are the `update` (the `continue`
        // target), so a `continue` also hits the bound check rather than skipping to the wrapping `i++`.
        let ic = self.ir.add_expr(IrExpr::GetValue(i_v));
        let ec = self.ir.add_expr(IrExpr::GetValue(n_v));
        let at_end = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Eq,
            lhs: ic,
            rhs: ec,
        });
        let brk = self.ir.add_expr(IrExpr::Break { label: None });
        let if_break = self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(at_end), brk)],
        });
        let update = self.ir.add_expr(IrExpr::Block {
            stmts: vec![if_break, incs],
            value: None,
        });
        let wbody = self.ir.add_expr(IrExpr::Block {
            stmts: out,
            value: None,
        });
        let wh = self.ir.add_expr(IrExpr::While {
            cond,
            body: wbody,
            update: Some(update),
            post_test: false,
            label,
        });
        self.scope.truncate(depth);
        Some(self.ir.add_expr(IrExpr::Block {
            stmts: vec![var_r, var_i, var_n, wh],
            value: None,
        }))
    }

    /// `for (x in progression)` over an `IntProgression`/`LongProgression`/`CharProgression` (and the
    /// unsigned `UInt`/`ULong` variants) value — a counted loop whose increment is the progression's
    /// `step` (which may be negative, e.g. from `downTo`), so the loop direction is decided at runtime.
    /// The progression's `getLast()` is already the exact final element (the stdlib computed it via
    /// `getProgressionLastElement`, including any `step`), so the `i == last` break lands precisely and
    /// guards against overflowing past a `MAX`/`MIN` bound:
    ///   i = p.getFirst(); last = p.getLast(); step = p.getStep();
    ///   while ((step > 0 && i <= last) || (step < 0 && i >= last)) { x = i; …; if (i == last) break; i += step }
    /// An unsigned progression compares its (sign-bit-spanning) elements via `compareUnsigned`; its
    /// `step` stays signed, so the direction test is an ordinary signed comparison.
    fn lower_foreach_progression(
        &mut self,
        name: &str,
        iterable: AstExprId,
        body: AstExprId,
        it_ty: Ty,
        elem: Ty,
        label: Option<String>,
    ) -> Option<u32> {
        let internal = it_ty.obj_internal()?.to_string();
        let depth = self.scope.len();
        let elem_ir = ty_to_ir(elem);
        let is_unsigned = elem.is_unsigned();
        // `Long`/`ULong` counters and steps use 64-bit arithmetic; `Int`/`Char`/`UInt` use 32-bit.
        let wide = matches!(elem, Ty::Long | Ty::ULong);
        // The element's primitive descriptor for the `getFirst`/`getLast` getters (a `Char` reads `C`;
        // unsigned and the rest erase to the signed primitive).
        let prim_desc = match elem {
            Ty::Char => "C",
            _ if wide => "J",
            _ => "I",
        };
        // `step` is a plain signed `Int`/`Long` (never a value class), so its getter is unmangled.
        let step_desc = if wide { "J" } else { "I" };
        // The unsigned progressions' `first`/`last` are mangled inline-class members; signed ones are
        // the plain `getFirst`/`getLast`.
        let (gf_name, gf_desc, gl_name, gl_desc) = if is_unsigned {
            let (gf, gfd) = self.syms.libraries.mangled_member(&internal, "getFirst-")?;
            let (gl, gld) = self.syms.libraries.mangled_member(&internal, "getLast-")?;
            (gf, gfd, gl, gld)
        } else {
            (
                "getFirst".to_string(),
                format!("(){prim_desc}"),
                "getLast".to_string(),
                format!("(){prim_desc}"),
            )
        };
        // Evaluate the progression once; the three getters share the one receiver.
        let rng = self.expr(iterable)?;
        let r_v = self.fresh_value();
        let var_r = self.ir.add_expr(IrExpr::Variable {
            index: r_v,
            ty: ty_to_ir(it_ty),
            init: Some(rng),
        });
        let getter = |this: &mut Self, gname: &str, gdesc: &str| {
            let recv = this.ir.add_expr(IrExpr::GetValue(r_v));
            this.ir.add_expr(IrExpr::Call {
                callee: Callee::Virtual {
                    owner: internal.clone(),
                    name: gname.to_string(),
                    descriptor: gdesc.to_string(),
                    interface: false,
                },
                dispatch_receiver: Some(recv),
                args: vec![],
            })
        };
        // i = p.getFirst()
        let first = getter(self, &gf_name, &gf_desc);
        let i_v = self.fresh_value();
        self.scope.push((name.to_string(), i_v, elem));
        let var_i = self.ir.add_expr(IrExpr::Variable {
            index: i_v,
            ty: elem_ir.clone(),
            init: Some(first),
        });
        // last = p.getLast()  (hoisted)
        let last = getter(self, &gl_name, &gl_desc);
        let n_v = self.fresh_value();
        let var_n = self.ir.add_expr(IrExpr::Variable {
            index: n_v,
            ty: elem_ir.clone(),
            init: Some(last),
        });
        // step = p.getStep()  (hoisted)
        let step = getter(self, "getStep", &format!("(){step_desc}"));
        let s_v = self.fresh_value();
        let var_s = self.ir.add_expr(IrExpr::Variable {
            index: s_v,
            ty: ty_to_ir(if wide { Ty::Long } else { Ty::Int }),
            init: Some(step),
        });
        // The step zero literal, in the step's own (signed) type.
        let zero_step = |this: &mut Self| {
            this.ir.add_expr(IrExpr::Const(if wide {
                IrConst::Long(0)
            } else {
                IrConst::Int(0)
            }))
        };
        // Compare two counter-typed values; an unsigned element orders via `compareUnsigned(a,b) <op> 0`
        // (a signed opcode would misorder values past the sign bit).
        let cmp = |this: &mut Self, op: IrBinOp, a: u32, b: u32| -> u32 {
            let la = this.ir.add_expr(IrExpr::GetValue(a));
            let lb = this.ir.add_expr(IrExpr::GetValue(b));
            if is_unsigned {
                let (owner, prim) = if elem == Ty::UInt {
                    ("java/lang/Integer", "I")
                } else {
                    ("java/lang/Long", "J")
                };
                let call = this.ir.add_expr(IrExpr::Call {
                    callee: Callee::Static {
                        owner: owner.to_string(),
                        name: "compareUnsigned".to_string(),
                        descriptor: format!("({prim}{prim})I"),
                        inline: false,
                        must_inline: false,
                    },
                    dispatch_receiver: None,
                    args: vec![la, lb],
                });
                let zero = this.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                this.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op,
                    lhs: call,
                    rhs: zero,
                })
            } else {
                this.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op,
                    lhs: la,
                    rhs: lb,
                })
            }
        };
        // cond: (step > 0 && i <= last) || (step < 0 && i >= last). A constant step folds to one branch
        // at the bytecode level; iterating is correct for either direction.
        let sg1 = self.ir.add_expr(IrExpr::GetValue(s_v));
        let z1 = zero_step(self);
        let step_pos = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Gt,
            lhs: sg1,
            rhs: z1,
        });
        let i_le = cmp(self, IrBinOp::Le, i_v, n_v);
        let asc = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::And,
            lhs: step_pos,
            rhs: i_le,
        });
        let sg2 = self.ir.add_expr(IrExpr::GetValue(s_v));
        let z2 = zero_step(self);
        let step_neg = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Lt,
            lhs: sg2,
            rhs: z2,
        });
        let i_ge = cmp(self, IrBinOp::Ge, i_v, n_v);
        let desc = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::And,
            lhs: step_neg,
            rhs: i_ge,
        });
        let cond = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Or,
            lhs: asc,
            rhs: desc,
        });
        // body (the loop variable is the counter `i`)
        let mut out = Vec::new();
        if self.append_body_stmts(body, &mut out).is_none() {
            self.scope.truncate(depth);
            return None;
        }
        // update (the `continue` target): break exactly at `last`, then `i += step`. Breaking before the
        // increment keeps a progression ending at `MAX_VALUE`/`MIN_VALUE` from wrapping past it.
        let ic = self.ir.add_expr(IrExpr::GetValue(i_v));
        let ec = self.ir.add_expr(IrExpr::GetValue(n_v));
        let at_end = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Eq,
            lhs: ic,
            rhs: ec,
        });
        let brk = self.ir.add_expr(IrExpr::Break { label: None });
        let if_break = self.ir.add_expr(IrExpr::When {
            branches: vec![(Some(at_end), brk)],
        });
        let gi2 = self.ir.add_expr(IrExpr::GetValue(i_v));
        let gs = self.ir.add_expr(IrExpr::GetValue(s_v));
        let inc = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Add,
            lhs: gi2,
            rhs: gs,
        });
        let incs = self.ir.add_expr(IrExpr::SetValue {
            var: i_v,
            value: inc,
        });
        let update = self.ir.add_expr(IrExpr::Block {
            stmts: vec![if_break, incs],
            value: None,
        });
        let wbody = self.ir.add_expr(IrExpr::Block {
            stmts: out,
            value: None,
        });
        let wh = self.ir.add_expr(IrExpr::While {
            cond,
            body: wbody,
            update: Some(update),
            post_test: false,
            label,
        });
        self.scope.truncate(depth);
        Some(self.ir.add_expr(IrExpr::Block {
            stmts: vec![var_r, var_i, var_n, var_s, wh],
            value: None,
        }))
    }

    fn lower_foreach_iterator(
        &mut self,
        name: &str,
        iterable: AstExprId,
        body: AstExprId,
        it_ty: Ty,
        index: Option<&str>,
        label: Option<String>,
    ) -> Option<u32> {
        let internal = it_ty.obj_internal()?;
        // The iterator comes from a member `iterator()` (`List`), or — when there is none — the stdlib
        // `iterator` *extension* (`for (e in map)` uses `Map.iterator()` → `Iterator<Map.Entry<K,V>>`).
        // `iter_ret` is the (possibly parameterized) iterator type; `ext_iter` flags the static call.
        let (iter_ret, iter_desc, iter_owner, iter_ext) = if let Some(m) =
            crate::call_resolver::resolve_instance(&*self.syms.libraries, internal, "iterator", &[])
        {
            (m.ret, m.descriptor, internal.to_string(), false)
        } else if let Some(c) =
            self.syms
                .libraries
                .resolve_callable("iterator", Some(it_ty), &[], &[])
        {
            (c.ret, c.descriptor, c.owner, true)
        } else {
            return None;
        };
        let iter_ty = iter_ret;
        let iter_internal = iter_ty.obj_internal()?.to_string();
        let hasnext_m = crate::call_resolver::resolve_instance(
            &*self.syms.libraries,
            &iter_internal,
            "hasNext",
            &[],
        )?;
        let next_m = crate::call_resolver::resolve_instance(
            &*self.syms.libraries,
            &iter_internal,
            "next",
            &[],
        )?;
        // The element is the iterator's type argument (`Iterator<Map.Entry<K,V>>`), else the iterable's
        // own (`List<Int>` → `Int`), else the type parameter's upper bound (`Any`). The JVM `Object`
        // realization + checkcast are the backend's concern, applied at the Ty→bytecode boundary.
        let elem = iter_ty
            .type_args()
            .first()
            .copied()
            .or_else(|| it_ty.type_args().first().copied())
            .unwrap_or_else(|| Ty::obj("kotlin/Any"));
        let it_iface = self
            .syms
            .libraries
            .resolve_type(internal)
            .map_or(false, |t| t.is_interface());
        let iter_iface = self
            .syms
            .libraries
            .resolve_type(&iter_internal)
            .map_or(false, |t| t.is_interface());
        let depth = self.scope.len();
        // `forEachIndexed`: an `Int` index counter, declared before the loop and bound to the lambda's
        // first parameter, incremented at the end of each iteration.
        let (idx_v, var_idx) = if let Some(iname) = index {
            let v = self.fresh_value();
            self.scope.push((iname.to_string(), v, Ty::Int));
            let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
            (
                Some(v),
                Some(self.ir.add_expr(IrExpr::Variable {
                    index: v,
                    ty: ty_to_ir(Ty::Int),
                    init: Some(zero),
                })),
            )
        } else {
            (None, None)
        };

        // it = iterable.iterator()  (member virtual call, or the extension's static call)
        let recv = self.expr(iterable)?;
        let iter_callee = if iter_ext {
            Callee::Static {
                owner: iter_owner,
                name: "iterator".to_string(),
                descriptor: iter_desc,
                inline: false,
                must_inline: false,
            }
        } else {
            Callee::Virtual {
                owner: iter_owner,
                name: "iterator".to_string(),
                descriptor: iter_desc,
                interface: it_iface,
            }
        };
        let iter_call = self.ir.add_expr(IrExpr::Call {
            callee: iter_callee,
            dispatch_receiver: if iter_ext { None } else { Some(recv) },
            args: if iter_ext { vec![recv] } else { vec![] },
        });
        let it_v = self.fresh_value();
        let var_it = self.ir.add_expr(IrExpr::Variable {
            index: it_v,
            ty: ty_to_ir(iter_ty),
            init: Some(iter_call),
        });

        // cond: it.hasNext()
        let it_g = self.ir.add_expr(IrExpr::GetValue(it_v));
        let cond = self.ir.add_expr(IrExpr::Call {
            callee: Callee::Virtual {
                owner: iter_internal.clone(),
                name: "hasNext".to_string(),
                descriptor: hasnext_m.descriptor,
                interface: iter_iface,
            },
            dispatch_receiver: Some(it_g),
            args: vec![],
        });

        // x = (elem) it.next()  — unbox a primitive element, checkcast a specific reference.
        let it_g2 = self.ir.add_expr(IrExpr::GetValue(it_v));
        let next_call = self.ir.add_expr(IrExpr::Call {
            callee: Callee::Virtual {
                owner: iter_internal.clone(),
                name: "next".to_string(),
                descriptor: next_m.descriptor,
                interface: iter_iface,
            },
            dispatch_receiver: Some(it_g2),
            args: vec![],
        });
        let x_init = if elem.is_unsigned() {
            // The element is a boxed `kotlin/UInt`/`ULong` — checkcast + `unbox-impl`, not the
            // `Integer` unbox a plain `is_primitive` coercion would emit.
            self.unbox_unsigned(next_call, elem)
        } else if elem.is_primitive() {
            self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: next_call,
                type_operand: ty_to_ir(elem),
            })
        } else if elem != Ty::obj("kotlin/Any") {
            self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::Cast,
                arg: next_call,
                type_operand: ty_to_ir(elem),
            })
        } else {
            next_call
        };
        let x_v = self.fresh_value();
        self.scope.push((name.to_string(), x_v, elem));
        let var_x = self.ir.add_expr(IrExpr::Variable {
            index: x_v,
            ty: ty_to_ir(elem),
            init: Some(x_init),
        });

        let mut out = vec![var_x];
        if self.append_body_stmts(body, &mut out).is_none() {
            self.scope.truncate(depth);
            return None;
        }
        // index += 1 (forEachIndexed)
        let update = idx_v.map(|iv| {
            let g = self.ir.add_expr(IrExpr::GetValue(iv));
            let one = self.ir.add_expr(IrExpr::Const(IrConst::Int(1)));
            let inc = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                op: IrBinOp::Add,
                lhs: g,
                rhs: one,
            });
            self.ir.add_expr(IrExpr::SetValue {
                var: iv,
                value: inc,
            })
        });
        let wbody = self.ir.add_expr(IrExpr::Block {
            stmts: out,
            value: None,
        });
        let wh = self.ir.add_expr(IrExpr::While {
            cond,
            body: wbody,
            update,
            post_test: false,
            label,
        });
        self.scope.truncate(depth);
        let mut stmts = Vec::new();
        if let Some(vi) = var_idx {
            stmts.push(vi);
        }
        stmts.push(var_it);
        stmts.push(wh);
        Some(self.ir.add_expr(IrExpr::Block { stmts, value: None }))
    }

    pub(crate) fn lower_arg(&mut self, arg: AstExprId, target: &Ty) -> Option<u32> {
        // A value flowing into a `suspend` function-type parameter. A LAMBDA literal becomes a concrete
        // `SuspendLambda` subclass (`lower_suspend_lambda`); any other value (a suspend function value
        // passed through) needs continuation threading not yet modeled, so it bails (skip the file).
        if let Ty::Fun(s) = target.non_null() {
            if s.suspend {
                let params = &s.params;
                if let Expr::Lambda {
                    params: lparams,
                    body,
                } = self.afile.expr(arg).clone()
                {
                    // Bind names: explicit, or the implicit single `it`, or none (arity 0).
                    let bind_names: Vec<String> = if !lparams.is_empty() {
                        lparams.clone()
                    } else if params.len() == 1 {
                        vec!["it".to_string()]
                    } else if params.is_empty() {
                        vec![]
                    } else {
                        return None;
                    };
                    // Parameter `Ty`s come from the lambda's checked type (the expected suspend type drives
                    // them); fall back to the erased IR param types only if absent.
                    let ty_params: Vec<Ty> = self
                        .info
                        .ty(arg)
                        .fun_params()
                        .map(|p| p.to_vec())
                        .filter(|p| p.len() == params.len())
                        .unwrap_or_else(|| params.iter().map(|_| Ty::obj("kotlin/Any")).collect());
                    if bind_names.len() == params.len() {
                        return self.lower_suspend_lambda(body, &ty_params, bind_names);
                    }
                }
                return None;
            }
        }
        let at = self.info.ty(arg);
        // `emptyArray<T>()` is a reified intrinsic — expand it to a fresh empty array of the *target*
        // element type (the reified `T`), exactly as kotlinc specializes it, rather than calling the
        // throwing stub. Recognized by the call shape (not the erased `Array<Any>` type, which a real
        // `Object[]` value also has); the target supplies the otherwise-erased element.
        if self.is_empty_array_intrinsic(arg) {
            if let Some(elem) = ir_array_element(target) {
                return Some(self.ir.add_expr(IrExpr::Vararg {
                    element_type: elem,
                    elements: vec![],
                }));
            }
        }
        let e = self.expr(arg)?;
        let target_ref = ir_type_is_reference(target);
        // An unsigned value flowing into a reference context (`Any`, a generic, a collection element)
        // boxes via the inline-class `box-impl` factory to a `kotlin/UInt`/`ULong` object.
        if at.is_unsigned() && target_ref {
            return Some(self.box_unsigned(e, at));
        }
        // A reference (`Any`, a smart-cast `is UInt`) flowing into an unsigned target unboxes the
        // `kotlin.UInt`/`ULong` value type — but krusty erases unsigned to `int` and would emit an
        // `Integer` unbox (ClassCastException). Skip rather than miscompile.
        if at.is_reference()
            && matches!(
                target.non_null().obj_internal(),
                Some("kotlin/UInt" | "kotlin/ULong")
            )
        {
            return None;
        }
        if at.is_primitive() && target_ref {
            Some(self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: e,
                type_operand: target.clone(),
            }))
        } else if at.is_reference() && !target_ref && *target != Ty::Unit && *target != Ty::Error {
            Some(self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: e,
                type_operand: target.clone(),
            }))
        } else if at.is_primitive()
            && !target_ref
            && *target != Ty::Error
            && *target != Ty::Unit
            && ty_to_ir(at) != *target
        {
            // Primitive numeric widening/narrowing (`Int` → `Long`, `Double` → `Int`): emit a
            // coercion (the backend does the `i2l`/`d2i`/… conversion).
            Some(self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: e,
                type_operand: target.clone(),
            }))
        } else if at == Ty::obj("kotlin/Any") && target_ref && !ir_type_is_object(target) {
            // A generic type-parameter return is erased to `Object` in the JVM signature; flowing it
            // into a more specific reference target needs a `checkcast` (kotlinc inserts one — the
            // value really is the target type at runtime). `as`-style, but never null here.
            Some(self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::Cast,
                arg: e,
                type_operand: target.clone(),
            }))
        } else {
            Some(e)
        }
    }

    /// A member read of a property declared as a generic type parameter has the erased *physical*
    /// type `pty` (`Object`) on the JVM, but the checker may have substituted a more specific
    /// *logical* type (`Box<Int>().x : Int`). Insert the coercion kotlinc emits on such a read: an
    /// unbox for a primitive target, a checkcast for a more specific reference target. A no-op when
    /// the logical and physical types already agree (every non-generic read).
    /// Insert the unbox/checkcast bridging an erased physical type to a known logical type — the same
    /// coercion as [`coerce_generic_read`] but with both types given directly (for synthesized reads,
    /// e.g. a destructuring `componentN()` call whose erased `Object` becomes the element type).
    /// A typed zero/`null` placeholder for an omitted `$default` parameter — the value is ignored
    /// (the synthetic stub substitutes the real default when the mask bit is set), but its type must
    /// match the descriptor slot: `0` for a primitive, `null` for a reference.
    fn zero_placeholder(&mut self, t: Ty) -> u32 {
        let c = match t {
            Ty::Long => IrConst::Long(0),
            Ty::Double => IrConst::Double(0.0),
            Ty::Float => IrConst::Float(0.0),
            t if t.is_primitive() => IrConst::Int(0), // Int/Short/Byte/Char/Boolean → iconst_0
            _ => IrConst::Null,
        };
        self.ir.add_expr(IrExpr::Const(c))
    }

    fn coerce_erased(&mut self, read: u32, logical: Ty, physical: Ty) -> u32 {
        if logical == physical {
            return read;
        }
        // An unsigned value out of an erased reference: checkcast to the inline-class object, then
        // `unbox-impl` — the wrapper is `kotlin/UInt`, not `Integer`.
        if logical.is_unsigned() && physical.is_reference() {
            return self.unbox_unsigned(read, logical);
        }
        // A primitive flowing out of any erased reference (`Object`, or a type-parameter bound like
        // `Comparable`/`Number` — `maxOrNull(): T`) unboxes; a reference erased to `Object` checkcasts.
        if logical.is_primitive() && physical.is_reference() {
            self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: read,
                type_operand: ty_to_ir(logical),
            })
        } else if logical.is_reference()
            && !matches!(logical, Ty::Null)
            && physical == Ty::obj("kotlin/Any")
        {
            self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::Cast,
                arg: read,
                type_operand: ty_to_ir(logical),
            })
        } else {
            read
        }
    }

    fn coerce_generic_read(&mut self, read: u32, member: AstExprId, pty: Ty) -> u32 {
        let lt = self.info.ty(member);
        if lt == Ty::Error {
            return read;
        }
        self.coerce_erased(read, lt, pty)
    }

    /// Lower a zero-arg member READ `recv.name` on a builtin/library/another-file receiver — the shared
    /// path behind a qualified `recv.name` and a receiver-lambda's implicit `this.name`. The member is
    /// resolved generically through the library/stdlib classpath reader by its own name, its `getX()`
    /// accessor form, or a collection-mapped name (no per-member hardcode — `String.length` resolves as
    /// `java/lang/String.length()` just like `uppercase()`). `recv` is the already-lowered receiver
    /// value. Returns `None` when the type exposes no such member.
    fn lower_member_read_on(&mut self, recv: u32, rt: Ty, name: &str, e: AstExprId) -> Option<u32> {
        // A property on a class defined in ANOTHER file → its public `getX()` accessor (the backing
        // field is private). Resolved from the sibling class's `ClassSig`.
        if let Ty::Obj(i, _) = rt {
            if self.class_of(rt).is_none() {
                if let Some((owner, ret_ty, is_iface)) = self
                    .syms
                    .class_by_internal(i)
                    .filter(|cs| cs.value_field.is_none())
                    .and_then(|cs| {
                        cs.props
                            .iter()
                            .find(|(n, _, _)| n == name)
                            .map(|(_, t, _)| (i.to_string(), *t, cs.is_interface))
                    })
                {
                    return Some(self.ir.add_expr(IrExpr::Call {
                        callee: Callee::CrossFileVirtual {
                            owner,
                            name: getter_name(name),
                            params: vec![],
                            ret: ty_to_ir(ret_ty),
                            interface: is_iface,
                        },
                        dispatch_receiver: Some(recv),
                        args: vec![],
                    }));
                }
            }
        }
        // A Kotlin property is a zero-arg accessor on the JVM. Resolve it from the stdlib/classpath
        // reader by the property's own name (`size()`), its `getX()` form, or a collection-mapped name —
        // a `String` receiver reads its `java.lang.String` members.
        let internal = match rt {
            Ty::String => "java/lang/String".to_string(),
            Ty::Obj(i, _) => i.to_string(),
            _ => return None,
        };
        let mapped = crate::resolve::collection_mapped_accessor(name).map(|s| s.to_string());
        let resolved = [Some(name.to_string()), Some(getter_name(name)), mapped]
            .into_iter()
            .flatten()
            .find_map(|cand| {
                crate::call_resolver::resolve_instance(&*self.syms.libraries, &internal, &cand, &[])
                    .filter(|m| !matches!(m.ret, Ty::Unit | Ty::Error))
                    .map(|m| {
                        let is_iface = self
                            .syms
                            .libraries
                            .resolve_type(&internal)
                            .map_or(false, |t| t.is_interface());
                        (m, is_iface)
                    })
            });
        if let Some((m, is_iface)) = resolved {
            let read = self.ir.add_expr(IrExpr::Call {
                callee: Callee::Virtual {
                    owner: internal,
                    name: m.name.clone(),
                    descriptor: m.descriptor,
                    interface: is_iface,
                },
                dispatch_receiver: Some(recv),
                args: vec![],
            });
            return Some(self.coerce_generic_read(read, e, m.ret));
        }
        // A Kotlin BUILTIN member the classpath `resolve_instance` can't surface — e.g. `String.length`
        // (a property over `java.lang.String.length()`), `List.size`. Resolved generically from the
        // builtins metadata + the kotlin↔JVM class map (owner/descriptor/interface), NOT a hardcode.
        let kotlin_internal = match rt {
            Ty::String => "kotlin/String".to_string(),
            Ty::Obj(i, _) => i.to_string(),
            _ => return None,
        };
        if let Some((owner, descriptor, ret_ty, is_iface)) = self
            .syms
            .libraries
            .builtin_member_call(&kotlin_internal, name, 0)
        {
            let read = self.ir.add_expr(IrExpr::Call {
                callee: Callee::Virtual {
                    owner,
                    name: name.to_string(),
                    descriptor,
                    interface: is_iface,
                },
                dispatch_receiver: Some(recv),
                args: vec![],
            });
            return Some(self.coerce_generic_read(read, e, ret_ty));
        }
        None
    }

    /// Lower a CALL `recv.name(args)` that resolves to a top-level EXTENSION function on `rt` — the
    /// shared path behind a qualified `recv.name(args)` and a receiver-lambda / extension-fn body's
    /// implicit `this.name(args)` (`"ab".run { uppercase() }`, `fun String.shout() = uppercase()`).
    /// `recv_ir` is the already-lowered receiver value. Tries a public library extension first
    /// (`invokestatic facade.name(recv, args)`), then a private `@InlineOnly` extension whose real body
    /// the backend splices (`String.uppercase()` → `toUpperCase(Locale.ROOT)`). `None` when neither
    /// resolves. No stdlib name is hardcoded — owner/descriptor come from the library reader.
    /// Resolve a classpath extension `recv.name(args)`, retrying once with integer-LITERAL `Int`
    /// arguments widened to `Long` — Kotlin adapts an integer literal to a wider expected type, so
    /// `longRange step 3` resolves `LongProgression.step(Long)`. A non-literal `Int` is left as-is
    /// (kotlinc rejects `longRange step intVar`). Mirrors the checker's classpath-extension adaptation.
    fn resolve_ext_lit_widened(
        &self,
        name: &str,
        rt: Ty,
        args: &[AstExprId],
        arg_tys: &[Ty],
    ) -> Option<crate::libraries::LibraryCallable> {
        if let Some(c) = self
            .syms
            .libraries
            .resolve_callable(name, Some(rt), arg_tys, &[])
        {
            return Some(c);
        }
        let widened: Vec<Ty> = arg_tys
            .iter()
            .zip(args.iter())
            .map(|(t, &a)| {
                if *t == Ty::Int && matches!(self.afile.expr(a), Expr::IntLit(_)) {
                    Ty::Long
                } else {
                    *t
                }
            })
            .collect();
        if widened == arg_tys {
            return None;
        }
        self.syms
            .libraries
            .resolve_callable(name, Some(rt), &widened, &[])
    }

    fn lower_ext_call_on(
        &mut self,
        recv_ir: u32,
        rt: Ty,
        name: &str,
        args: &[AstExprId],
        e: AstExprId,
    ) -> Option<u32> {
        let arg_tys: Vec<Ty> = args.iter().map(|&a| self.info.ty(a)).collect();
        let c = self
            .resolve_ext_lit_widened(name, rt, args, &arg_tys)
            .filter(|c| !c.default_call) // a defaulted extension needs the AST receiver expr — bail
            .or_else(|| {
                self.syms
                    .libraries
                    .resolve_scope_inline(name, rt, &arg_tys)
                    .filter(|c| {
                        c.is_inline
                            && self
                                .syms
                                .libraries
                                .can_inline_call(&c.owner, &c.name, &c.descriptor)
                    })
            })?;
        // The first parameter is the extension receiver. Box a primitive receiver flowing into a generic
        // `Object` receiver param; a reference receiver widens to its declared param type for free.
        let p0 = *c.params.first().unwrap_or(&rt);
        let recv = if rt.is_primitive() && p0.is_reference() {
            self.coerce_erased(recv_ir, rt, p0)
        } else {
            recv_ir
        };
        let mut a = vec![recv];
        for (i, &arg) in args.iter().enumerate() {
            match c.params.get(i + 1) {
                Some(p) => a.push(self.lower_arg(arg, &ty_to_ir(*p))?),
                None => a.push(self.expr(arg)?),
            }
        }
        let call = self.ir.add_expr(IrExpr::Call {
            callee: Callee::Static {
                owner: c.owner,
                name: c.name,
                descriptor: c.descriptor,
                inline: c.is_inline,
                must_inline: false,
            },
            dispatch_receiver: None,
            args: a,
        });
        Some(self.coerce_generic_read(call, e, c.physical_ret))
    }

    /// Lower an unqualified CALL `name(args)` against the implicit `this` receiver — the body of a
    /// receiver lambda (`"ab".run { uppercase() }`) or an extension function (`fun String.f() =
    /// uppercase()`), where `cur_class` is cleared and the `this` slot holds the external receiver. The
    /// implicit receiver takes priority over a receiver-less top-level function (Kotlin scoping), so
    /// this runs first. Tries, in order: a user instance method, a builtin/library member, then a stdlib
    /// extension. `None` when none match (the call falls through to top-level resolution).
    fn lower_this_member_call(
        &mut self,
        this_v: u32,
        this_ty: Ty,
        name: &str,
        args: &[AstExprId],
        e: AstExprId,
    ) -> Option<u32> {
        // A user instance method on the receiver's class — `this.m(args)`.
        if let Some(internal) = this_ty.obj_internal() {
            if let Some((class, index, mfid, _)) = self.resolve_method(internal, name) {
                let params = self.ir.functions[mfid as usize].params.clone();
                if args.len() == params.len() {
                    let recv = self.ir.add_expr(IrExpr::GetValue(this_v));
                    let mut a = Vec::new();
                    for (arg, pt) in args.iter().zip(&params) {
                        a.push(self.lower_arg(*arg, pt)?);
                    }
                    return Some(self.ir.add_expr(IrExpr::MethodCall {
                        class,
                        index,
                        receiver: recv,
                        args: a.into_iter().map(Some).collect(),
                    }));
                }
            }
        }
        // A builtin/library member method (`StringBuilder.append`, `String.isEmpty`) — `this.m(args)`.
        let arg_tys: Vec<Ty> = args.iter().map(|&a| self.info.ty(a)).collect();
        let lib_owner = match this_ty {
            Ty::String => Some("java/lang/String".to_string()),
            Ty::Obj(i, _) => Some(i.to_string()),
            _ => None,
        };
        if let Some(internal) = &lib_owner {
            if let Some(m) = crate::call_resolver::resolve_instance(
                &*self.syms.libraries,
                internal,
                name,
                &arg_tys,
            ) {
                if m.params.len() == args.len() {
                    let is_iface = self
                        .syms
                        .libraries
                        .resolve_type(internal)
                        .map_or(false, |ty| ty.is_interface());
                    let recv = self.ir.add_expr(IrExpr::GetValue(this_v));
                    let mut a = Vec::new();
                    for (arg, pt) in args.iter().zip(&m.params) {
                        a.push(self.lower_arg(*arg, &ty_to_ir(*pt))?);
                    }
                    let call = self.ir.add_expr(IrExpr::Call {
                        callee: Callee::Virtual {
                            owner: internal.clone(),
                            name: m.name.clone(),
                            descriptor: m.descriptor,
                            interface: is_iface,
                        },
                        dispatch_receiver: Some(recv),
                        args: a,
                    });
                    return Some(self.coerce_generic_read(call, e, m.ret));
                }
            }
        }
        // A stdlib/library EXTENSION on the receiver (`uppercase`/`reversed`).
        let recv = self.ir.add_expr(IrExpr::GetValue(this_v));
        self.lower_ext_call_on(recv, this_ty, name, args, e)
    }

    /// Inline a receiver-lambda scope call the checker resolved (`x.run { … }`, `x.apply { … }`,
    /// `with(x) { … }`): evaluate the receiver into a fresh slot bound as `this`, lower the lambda body
    /// against it, then yield the body value (`run`/`with`) or the receiver (`apply`/`also`). The
    /// receiver lambda runs in the caller, so `cur_class` is cleared — its members are reached through
    /// the implicit-`this` paths (member/getter/extension), never the enclosing class's private fields.
    fn lower_receiver_lambda(&mut self, rl: crate::resolve::ReceiverLambda) -> Option<u32> {
        let rty = self.info.ty(rl.receiver);
        let recv = self.expr(rl.receiver)?;
        let depth = self.scope.len();
        let p_slot = self.fresh_value();
        let saved_cur = self.cur_class.clone();
        self.cur_class = None;
        self.scope.push(("this".to_string(), p_slot, rty));
        let var_p = self.ir.add_expr(IrExpr::Variable {
            index: p_slot,
            ty: ty_to_ir(rty),
            init: Some(recv),
        });
        let body_val = self.expr(rl.body);
        self.scope.truncate(depth);
        self.cur_class = saved_cur;
        let body_val = body_val?;
        let result = if rl.returns_receiver {
            let recv_read = self.ir.add_expr(IrExpr::GetValue(p_slot));
            self.ir.add_expr(IrExpr::Block {
                stmts: vec![var_p, body_val],
                value: Some(recv_read),
            })
        } else {
            self.ir.add_expr(IrExpr::Block {
                stmts: vec![var_p],
                value: Some(body_val),
            })
        };
        Some(result)
    }

    /// Inline a scope function over an ALREADY-LOWERED receiver value (`recv_val`) — the shared core for
    /// a safe-call scope fn (`s?.let { … }`). Binds `recv_val` to a fresh slot named `pname` (`it` for
    /// `let`/`also`, `this` for `run`/`apply` — which also clears `cur_class`), lowers the body, and
    /// yields the body value or the receiver (`returns_receiver`). Returns the inlined block value.
    fn lower_scope_inline_on(
        &mut self,
        recv_val: u32,
        rty: Ty,
        pname: &str,
        body: AstExprId,
        returns_receiver: bool,
    ) -> Option<u32> {
        // A nullable-primitive receiver (`Int?` = `java/lang/Integer`, from a chained `…?.let { … }`) binds
        // the scope param as the UNBOXED primitive — matching the checker, so `it + 1` is primitive math.
        let (rty, recv_val) = match rty.nullable_primitive() {
            Some(prim) => {
                let unboxed = self.ir.add_expr(IrExpr::TypeOp {
                    op: IrTypeOp::ImplicitCoercion,
                    arg: recv_val,
                    type_operand: ty_to_ir(prim),
                });
                (prim, unboxed)
            }
            None => (rty, recv_val),
        };
        let depth = self.scope.len();
        let p_slot = self.fresh_value();
        let saved_cur = self.cur_class.clone();
        if pname == "this" {
            self.cur_class = None;
        }
        self.scope.push((pname.to_string(), p_slot, rty));
        let var_p = self.ir.add_expr(IrExpr::Variable {
            index: p_slot,
            ty: ty_to_ir(rty),
            init: Some(recv_val),
        });
        let body_val = self.expr(body);
        self.scope.truncate(depth);
        self.cur_class = saved_cur;
        let body_val = body_val?;
        Some(if returns_receiver {
            let recv_read = self.ir.add_expr(IrExpr::GetValue(p_slot));
            self.ir.add_expr(IrExpr::Block {
                stmts: vec![var_p, body_val],
                value: Some(recv_read),
            })
        } else {
            self.ir.add_expr(IrExpr::Block {
                stmts: vec![var_p],
                value: Some(body_val),
            })
        })
    }

    /// The non-null-branch value of a safe-call scope function (`s?.let`/`?.run`/`?.also`/`?.apply`):
    /// inline the scope fn with `recv_val` (the non-null receiver) bound. `None` when `name`/args aren't
    /// a recognized lambda-bearing scope call (the caller falls back to the member-access path).
    fn lower_safe_scope_member(
        &mut self,
        recv_val: u32,
        rty: Ty,
        name: &str,
        args: &Option<Vec<AstExprId>>,
    ) -> Option<u32> {
        let a = args.as_ref()?;
        if a.len() != 1 {
            return None;
        }
        let Expr::Lambda { params, body } = self.afile.expr(a[0]).clone() else {
            return None;
        };
        let (pname, returns_receiver) = match name {
            "let" => (
                params.first().cloned().unwrap_or_else(|| "it".to_string()),
                false,
            ),
            "also" => (
                params.first().cloned().unwrap_or_else(|| "it".to_string()),
                true,
            ),
            "run" if params.is_empty() => ("this".to_string(), false),
            "apply" if params.is_empty() => ("this".to_string(), true),
            _ => return None,
        };
        self.lower_scope_inline_on(recv_val, rty, &pname, body, returns_receiver)
    }

    /// Resolve an `is`/`as` target `TypeRef` to a known **reference** `Ty` (`String` or a class in
    /// this IR); returns `None` to bail for primitives, nullables, or unknown types.
    /// A field's declared type: `ty_of` (file-local classes + built-ins), falling back to the
    /// classpath-aware [`ty_ref`] when `ty_of` erases a CLASSPATH reference type to `Any` (it doesn't
    /// consult imports). Keeps the field decl, constructor parameter and getter agreeing on the real type.
    fn field_ty(&self, file: &ast::File, r: &ast::TypeRef) -> Ty {
        let base = ty_of(file, r);
        if base == Ty::obj("kotlin/Any") {
            // Resolve the NON-nullable form (`ty_ref` bails on a nullable type); the caller re-applies
            // the field's nullability to the IrType, so `Uuid` and `Uuid?` recover the same base type.
            let nn = ast::TypeRef {
                nullable: false,
                ..r.clone()
            };
            if let Some(rt) = self.ty_ref(&nn) {
                if rt != Ty::obj("kotlin/Any") {
                    return rt;
                }
            }
        }
        base
    }

    /// An extension function RECEIVER type: `ty_of` (file-local + built-ins, e.g. `String`), falling back
    /// to the classpath-aware [`ty_ref`] when `ty_of` can't resolve it (a classpath type like
    /// `kotlinx...SerialDescriptor` — `ty_of` yields `Error`/`Any` since it doesn't consult imports). The
    /// registration and body-lowering of the extension must agree with the checker's receiver descriptor.
    fn ext_receiver_ty(&self, file: &ast::File, r: &ast::TypeRef) -> Ty {
        let base = ty_of(file, r);
        if base == Ty::Error || base == Ty::obj("kotlin/Any") {
            let nn = ast::TypeRef {
                nullable: false,
                ..r.clone()
            };
            if let Some(rt) = self.ty_ref(&nn) {
                if rt != Ty::Error && rt != Ty::obj("kotlin/Any") {
                    return rt;
                }
            }
        }
        base
    }

    fn ty_ref(&self, r: &ast::TypeRef) -> Option<Ty> {
        // A reified type parameter (inside an expanded `<reified T>` inline body) resolves to the type
        // bound at the call site — `Array<T>`, `val x: T`, a return `T`, etc. all specialize. The bound
        // type is already concrete (built through `subst_type_ref`), so this recurses at most once.
        if !self.reified_subst.is_empty() {
            let s = self.subst_type_ref(r);
            if s.name != r.name {
                return self.ty_ref(&s);
            }
        }
        if r.nullable {
            return None;
        }
        let t = if let Some(p) = Ty::from_name(&r.name) {
            p
        } else if let Some(elem) = Ty::primitive_array_element(&r.name) {
            // `IntArray`/`CharArray`/… → the primitive array type (`int[]`/…), NOT a same-named classpath
            // class — resolve it here, before the `class_names` fallback, exactly as `resolve_ty` does
            // (the JDK ships an unrelated `sun.jvm.hotspot.utilities.IntArray`).
            Ty::array(elem)
        } else if r.name == "Array" {
            let e = r.arg.as_ref().and_then(|a| self.ty_ref(a))?;
            if !e.is_reference() {
                return None;
            }
            Ty::array(e)
        } else if self.classes.contains_key(&r.name) {
            Ty::obj(&r.name)
        } else if self
            .classes
            .contains_key(&class_internal(self.afile, &r.name))
        {
            // A nested class by source name (`Outer.Inner` → `Outer$Inner`).
            Ty::obj(&class_internal(self.afile, &r.name))
        } else if let Some(cs) = self.syms.classes.get(&r.name) {
            Ty::obj(&cs.internal)
        } else if let Some(internal) = self.syms.class_names.get(&r.name) {
            // A classpath / built-in mapped type (`Number`, `CharSequence`, `Runnable`, a Java class) —
            // the same name→internal map the checker resolves `is`/`as` targets against. `"__ty/<prim>"`
            // is an alias to a primitive, which `is`/`as` here doesn't model (skip).
            if internal.starts_with("__ty/") {
                return None;
            }
            Ty::obj(internal)
        } else {
            return None;
        };
        if t.is_reference() {
            Some(t)
        } else {
            None
        }
    }

    fn lower_body(&mut self, body: &FunBody, ret_ty: &Ty, fid: u32) -> Option<()> {
        self.cur_ret_ty = ret_ty.clone();
        // A named/local function returning `Unit` is a `void` JVM method (only a `() -> Unit` lambda's
        // closure method returns the `Unit` reference) — reset so a nested fun doesn't inherit it.
        self.cur_method_returns_unit_ref = false;
        // Defensive: the stack is push/pop-balanced within a body, but a bail mid-lowering of a previous
        // body must not leak an enclosing `finally` into this one.
        self.try_finally_stack.clear();
        // Local delegated properties are function-scoped — don't leak into the next body.
        self.local_delegated.clear();
        let b = match body {
            FunBody::Expr(e) => {
                let diverges = self.info.ty(*e) == Ty::Nothing;
                let stmts = if *ret_ty == Ty::Unit || diverges {
                    vec![self.expr(*e)?] // Unit, or a diverging expr (it returns/throws on its own — no wrap)
                } else {
                    // Coerce the body to the return type (a generic-erased `Object` return gets the
                    // `checkcast` kotlinc inserts).
                    let ve = self.lower_arg(*e, ret_ty)?;
                    vec![self.ir.add_expr(IrExpr::Return(Some(ve)))]
                };
                self.ir.add_expr(IrExpr::Block { stmts, value: None })
            }
            FunBody::Block(blk) => self.block_as_body(*blk, ret_ty)?,
            FunBody::None => return None,
        };
        self.ir.functions[fid as usize].body = Some(b);
        Some(())
    }

    fn block_as_body(&mut self, block: AstExprId, ret_ty: &Ty) -> Option<u32> {
        let Expr::Block { stmts, trailing } = self.afile.expr(block).clone() else {
            return None;
        };
        let depth = self.scope.len();
        let mut out = Vec::new();
        let mut diverged = false;
        for s in stmts {
            self.append_stmt(s, &mut out)?;
            if self.stmt_diverges(s) {
                diverged = true;
                break;
            }
        }
        // A block that diverges before its trailing value (`{ …; throw X; <unreachable trailing> }`)
        // needs no `return` — the diverging statement already transfers control; the trailing is dead.
        if diverged {
            self.scope.truncate(depth);
            return Some(self.ir.add_expr(IrExpr::Block {
                stmts: out,
                value: None,
            }));
        }
        if let Some(t) = trailing {
            let tt = self.info.ty(t);
            let diverges = tt == Ty::Nothing;
            // A value-less statement (e.g. a no-`else` `when`) can only be a value-returning
            // function's body if it's exhaustive (hence diverging). krusty doesn't prove
            // exhaustiveness, so bail rather than emit `return <no-value>`.
            if *ret_ty != Ty::Unit && !diverges && tt == Ty::Unit {
                self.scope.truncate(depth);
                return None;
            }
            let ve = self.expr(t)?;
            if *ret_ty == Ty::Unit || diverges {
                out.push(ve); // Unit trailing, or a diverging one (returns/throws itself — no wrap)
            } else {
                out.push(self.ir.add_expr(IrExpr::Return(Some(ve))));
            }
        }
        self.scope.truncate(depth);
        Some(self.ir.add_expr(IrExpr::Block {
            stmts: out,
            value: None,
        }))
    }

    /// Lower a `tailrec fun` body: rewrite tail-position self-calls into a `while(true)` loop (reassign
    /// the param slots, then `continue`). Bails (skip file) if a self-call appears in a non-tail position
    /// the transform can't handle — never miscompiles into stack-overflowing recursion.
    fn lower_tailrec_body(
        &mut self,
        f: &ast::FunDecl,
        ret_ty: &Ty,
        fid: u32,
        param_vals: Vec<u32>,
        param_tys: Vec<Ty>,
    ) -> Option<()> {
        self.cur_ret_ty = ret_ty.clone();
        self.cur_method_returns_unit_ref = false;
        self.try_finally_stack.clear();
        self.local_delegated.clear();
        let label = "$tailrec".to_string();
        self.cur_tailrec = Some(TailrecCtx {
            name: f.name.clone(),
            param_vals,
            param_tys,
            label: label.clone(),
        });
        let unit = *ret_ty == Ty::Unit;
        let loop_body = match &f.body {
            // A `Unit` body recurses with a bare expression STATEMENT (`if (c) f(args)`), not
            // `return f(args)` — handled by a tail-statement walk (`lower_tail_unit`); a value body uses
            // the return-driven transform (`lower_tail_expr` for an expr body, `block_as_body` +
            // `Stmt::Return` interception for a block body).
            FunBody::Block(blk) if unit => self.lower_tail_unit(*blk),
            FunBody::Expr(e) if !unit => self.lower_tail_expr(*e, ret_ty),
            FunBody::Block(blk) if !unit => self.block_as_body(*blk, ret_ty),
            _ => None, // a `Unit` expr-body tailrec / no body — not modeled, skip
        };
        self.cur_tailrec = None;
        let loop_body = loop_body?;
        let cond = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
        let whilexpr = self.ir.add_expr(IrExpr::While {
            cond,
            body: loop_body,
            update: None,
            post_test: false,
            label: Some(label),
        });
        let body = self.ir.add_expr(IrExpr::Block {
            stmts: vec![whilexpr],
            value: None,
        });
        self.ir.functions[fid as usize].body = Some(body);
        Some(())
    }

    /// Whether `callee(args)` is a tail self-call to the current `tailrec` function (same unqualified
    /// name, not shadowed by a local, matching arity).
    fn is_tail_self_call(&self, callee: AstExprId, args: &[AstExprId]) -> bool {
        let Some(ctx) = &self.cur_tailrec else {
            return false;
        };
        matches!(self.afile.expr(callee), Expr::Name(n) if *n == ctx.name)
            && self.lookup(&ctx.name).is_none()
            && args.len() == ctx.param_vals.len()
    }

    /// Emit a tail self-call: evaluate the args into temps (so reassignment can't alias), reassign each
    /// parameter slot, then `continue` the wrapping loop.
    fn tail_update_continue(&mut self, args: &[AstExprId]) -> Option<u32> {
        let ctx = self.cur_tailrec.clone()?;
        let mut stmts = Vec::new();
        let mut temps = Vec::new();
        for (a, t) in args.iter().zip(&ctx.param_tys) {
            let v = self.lower_arg(*a, &ty_to_ir(*t))?;
            let tmp = self.fresh_value();
            stmts.push(self.ir.add_expr(IrExpr::Variable {
                index: tmp,
                ty: ty_to_ir(*t),
                init: Some(v),
            }));
            temps.push(tmp);
        }
        for (pv, tmp) in ctx.param_vals.iter().zip(&temps) {
            let read = self.ir.add_expr(IrExpr::GetValue(*tmp));
            stmts.push(self.ir.add_expr(IrExpr::SetValue {
                var: *pv,
                value: read,
            }));
        }
        stmts.push(self.ir.add_expr(IrExpr::Continue {
            label: Some(ctx.label.clone()),
        }));
        Some(self.ir.add_expr(IrExpr::Block { stmts, value: None }))
    }

    /// Lower an expression in TAIL position of a `tailrec` body: an `if` recurses into both branches; a
    /// self-call becomes update+continue; any other expression returns its value (bailing if it still
    /// contains a self-call — that would be non-tail recursion).
    fn lower_tail_expr(&mut self, e: AstExprId, ret_ty: &Ty) -> Option<u32> {
        match self.afile.expr(e).clone() {
            Expr::If {
                cond,
                then_branch,
                else_branch: Some(eb),
            } => {
                let c = self.expr(cond)?;
                let t = self.lower_tail_expr(then_branch, ret_ty)?;
                let el = self.lower_tail_expr(eb, ret_ty)?;
                Some(self.ir.add_expr(IrExpr::When {
                    branches: vec![(Some(c), t), (None, el)],
                }))
            }
            Expr::Call { callee, args } if self.is_tail_self_call(callee, &args) => {
                self.tail_update_continue(&args)
            }
            Expr::Block { .. } => self.block_as_body(e, ret_ty),
            _ => {
                // Base case: a non-recursive value. If it still references the function (a non-tail
                // self-call, e.g. `f(x) + 1`), we can't loop-ize it — skip the file rather than recurse.
                let ctx_name = self.cur_tailrec.as_ref()?.name.clone();
                if crate::resolve::expr_uses_name_pub(self.afile, e, &ctx_name) {
                    return None;
                }
                let v = self.lower_arg(e, ret_ty)?;
                Some(self.ir.add_expr(IrExpr::Return(Some(v))))
            }
        }
    }

    /// Lower a `Unit`-returning `tailrec` body block. A `Unit` body recurses with a bare statement
    /// (`if (c) f(args)` / `{ …; f(args) }`), not `return f(args)`: walk to the tail position and rewrite
    /// its self-calls into update+continue, then exit the loop via a trailing `return` on fall-through.
    /// Bails (skip file) on any self-call outside tail position — never miscompiles into recursion.
    fn lower_tail_unit(&mut self, block: AstExprId) -> Option<u32> {
        let Expr::Block { stmts, trailing } = self.afile.expr(block).clone() else {
            return None;
        };
        if stmts.is_empty() && trailing.is_none() {
            return None;
        }
        let (body, diverges) = self.lower_tail_unit_block(&stmts, trailing)?;
        let mut out = vec![body];
        if !diverges {
            // No tail self-call fired on this path → return `Unit`, exiting the `while(true)` loop.
            out.push(self.ir.add_expr(IrExpr::Return(None)));
        }
        Some(self.ir.add_expr(IrExpr::Block {
            stmts: out,
            value: None,
        }))
    }

    /// Lower a block's statements + optional trailing expression in TAIL position. Only the final
    /// element — the trailing expr, or the last statement when there is none — is the tail. Returns the
    /// block IR and whether every path through it transfers control (`continue`/`return`/`throw`), i.e.
    /// never falls through. A self-call in any non-tail position → bail.
    fn lower_tail_unit_block(
        &mut self,
        stmts: &[crate::ast::StmtId],
        trailing: Option<AstExprId>,
    ) -> Option<(u32, bool)> {
        let name = self.cur_tailrec.as_ref()?.name.clone();
        let depth = self.scope.len();
        let mut out = Vec::new();
        // Leading non-tail statements: all of them when a trailing expr is the tail, else all but the last.
        let non_tail = if trailing.is_some() {
            stmts.len()
        } else {
            stmts.len().saturating_sub(1)
        };
        for &s in &stmts[..non_tail] {
            if self.afile.any_child_stmt(s, &mut |e| {
                crate::resolve::expr_uses_name_pub(self.afile, e, &name)
            }) {
                self.scope.truncate(depth);
                return None;
            }
            if self.append_stmt(s, &mut out).is_none() {
                self.scope.truncate(depth);
                return None;
            }
            if self.stmt_diverges(s) {
                // A non-tail statement that always transfers control makes the tail dead — stop here.
                self.scope.truncate(depth);
                return Some((
                    self.ir.add_expr(IrExpr::Block {
                        stmts: out,
                        value: None,
                    }),
                    true,
                ));
            }
        }
        let tail = match trailing {
            Some(t) => self.lower_tail_unit_expr(t),
            None => self.lower_tail_unit_stmt(*stmts.last()?),
        };
        let Some((ir, diverges)) = tail else {
            self.scope.truncate(depth);
            return None;
        };
        out.push(ir);
        self.scope.truncate(depth);
        Some((
            self.ir.add_expr(IrExpr::Block {
                stmts: out,
                value: None,
            }),
            diverges,
        ))
    }

    /// Lower the TAIL statement of a `Unit` `tailrec` body. Returns `(ir, always_transfers_control)`.
    fn lower_tail_unit_stmt(&mut self, s: crate::ast::StmtId) -> Option<(u32, bool)> {
        if let Stmt::Expr(e) = self.afile.stmt(s).clone() {
            return self.lower_tail_unit_expr(e);
        }
        // A non-`Expr` tail statement (assignment, etc.) is never a tail self-call — bail if it still
        // references the function, else lower it normally (the loop's `return` exits afterward).
        let name = self.cur_tailrec.as_ref()?.name.clone();
        if self.afile.any_child_stmt(s, &mut |e| {
            crate::resolve::expr_uses_name_pub(self.afile, e, &name)
        }) {
            return None;
        }
        let mut tmp = Vec::new();
        self.append_stmt(s, &mut tmp)?;
        let d = self.stmt_diverges(s);
        Some((
            self.ir.add_expr(IrExpr::Block {
                stmts: tmp,
                value: None,
            }),
            d,
        ))
    }

    /// Lower a `Unit`-typed expression in TAIL position: `if` recurses into both branches (a no-`else`
    /// `if` whose condition is false falls through), a self-call becomes update+continue, a `{ … }`
    /// block recurses; anything else runs for effect (bailing on a non-tail self-call).
    fn lower_tail_unit_expr(&mut self, e: AstExprId) -> Option<(u32, bool)> {
        match self.afile.expr(e).clone() {
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let c = self.expr(cond)?;
                let (t, td) = self.lower_tail_unit_expr(then_branch)?;
                match else_branch {
                    Some(eb) => {
                        let (el, ed) = self.lower_tail_unit_expr(eb)?;
                        Some((
                            self.ir.add_expr(IrExpr::When {
                                branches: vec![(Some(c), t), (None, el)],
                            }),
                            td && ed,
                        ))
                    }
                    // No `else`: a false condition falls through past the `when` → never diverges.
                    None => Some((
                        self.ir.add_expr(IrExpr::When {
                            branches: vec![(Some(c), t)],
                        }),
                        false,
                    )),
                }
            }
            Expr::Call { callee, args } if self.is_tail_self_call(callee, &args) => {
                Some((self.tail_update_continue(&args)?, true))
            }
            Expr::Block { stmts, trailing } => self.lower_tail_unit_block(&stmts, trailing),
            _ => {
                // Base case: run for effect. A lingering self-call here is non-tail → bail.
                let name = self.cur_tailrec.as_ref()?.name.clone();
                if crate::resolve::expr_uses_name_pub(self.afile, e, &name) {
                    return None;
                }
                let v = self.expr(e)?;
                Some((v, false))
            }
        }
    }

    /// Lower a compound assignment the checker marked as a user `opAssign` operator call (`plus_assign`):
    /// `target op= rhs` → `target.plusAssign(rhs)` (member `invokevirtual`, or extension `invokestatic`
    /// with the receiver as the first argument). `value` is the parser's desugared `Binary { op, lhs, rhs }`
    /// where `lhs` is the target read.
    fn lower_plus_assign(&mut self, value: AstExprId) -> Option<u32> {
        let Expr::Binary { op, lhs, rhs } = self.afile.expr(value).clone() else {
            return None;
        };
        let aname = match op {
            BinOp::Add => "plusAssign",
            BinOp::Sub => "minusAssign",
            BinOp::Mul => "timesAssign",
            BinOp::Div => "divAssign",
            BinOp::Rem => "remAssign",
            _ => return None,
        };
        let recv_desc = self.recv_ty(lhs).descriptor();
        // Extension operator: `invokestatic owner.plusAssign(recv, arg)` (receiver is the first param).
        if let Some(&fid) = self.ext_fun_ids.get(&(recv_desc, aname.to_string())) {
            let params = self.ir.functions[fid as usize].params.clone();
            if params.len() == 2 {
                let r = self.lower_arg(lhs, &params[0])?;
                let a = self.lower_arg(rhs, &params[1])?;
                return Some(self.ir.add_expr(IrExpr::Call {
                    callee: Callee::Local(fid),
                    dispatch_receiver: None,
                    args: vec![r, a],
                }));
            }
        }
        // Member operator: `recv.plusAssign(arg)`.
        let internal = self.recv_ty(lhs).obj_internal().map(|s| s.to_string())?;
        if let Some((class, index, mfid, _)) = self.resolve_method(&internal, aname) {
            let params = self.ir.functions[mfid as usize].params.clone();
            if params.len() == 1 {
                let r = self.expr(lhs)?;
                let a = self.lower_arg(rhs, &params[0])?;
                return Some(self.ir.add_expr(IrExpr::MethodCall {
                    class,
                    index,
                    receiver: r,
                    args: vec![Some(a)],
                }));
            }
        }
        // Classpath inline `MutableCollection.plusAssign` (`@InlineOnly`): emit an inline
        // `invokestatic owner.plusAssign(recv, arg)` — the bytecode splicer expands its real body
        // (`add`/`addAll`) at the call site (nothing about `add`/`addAll` is hardcoded here).
        let arg_ty = self.info.ty(rhs);
        let c = self
            .syms
            .libraries
            .resolve_scope_inline(aname, self.recv_ty(lhs), &[arg_ty])?;
        if c.params.len() == 2 {
            let r = self.lower_arg(lhs, &ty_to_ir(c.params[0]))?;
            let a = self.lower_arg(rhs, &ty_to_ir(c.params[1]))?;
            return Some(self.ir.add_expr(IrExpr::Call {
                callee: Callee::Static {
                    owner: c.owner,
                    name: c.name,
                    descriptor: c.descriptor,
                    inline: true,
                    must_inline: false,
                },
                dispatch_receiver: None,
                args: vec![r, a],
            }));
        }
        None
    }

    fn stmt(&mut self, s: crate::ast::StmtId) -> Option<u32> {
        let r = self.stmt_inner(s);
        if r.is_none() && lower_bail_reason().starts_with("deep") {
            set_bail(&format!(
                "stmt {}",
                bail_variant(&format!("{:?}", self.afile.stmt(s)))
            ));
        }
        r
    }

    fn stmt_inner(&mut self, s: crate::ast::StmtId) -> Option<u32> {
        // A compound assignment routed to a user `opAssign` operator (checker-marked) — emit the call.
        if self.info.plus_assign.contains(&s) {
            if let Stmt::Assign { value, .. } | Stmt::AssignMember { value, .. } =
                self.afile.stmt(s).clone()
            {
                return self.lower_plus_assign(value);
            }
        }
        match self.afile.stmt(s).clone() {
            Stmt::Expr(e) => self.expr(e),
            Stmt::Return(e, ret_label) => {
                // Inside a `tailrec` function (no spliced-lambda label): a `return f(args)` to the same
                // function is a tail call — reassign the params and `continue` the loop. A `return` whose
                // value otherwise references the function is non-tail recursion the loop can't model →
                // bail (skip file) rather than stack-overflow.
                if ret_label.is_none() && self.cur_tailrec.is_some() {
                    if let Some(ve) = e {
                        if let Expr::Call { callee, args } = self.afile.expr(ve).clone() {
                            if self.is_tail_self_call(callee, &args) {
                                return self.tail_update_continue(&args);
                            }
                        }
                        let name = self.cur_tailrec.as_ref().unwrap().name.clone();
                        if crate::resolve::expr_uses_name_pub(self.afile, ve, &name) {
                            return None;
                        }
                    }
                }
                // A `return@label` matching an active spliced-lambda frame is a LOCAL return from that
                // lambda: break to the lambda's end label (`Unit` result — run any value for effect). A
                // labeled return with no matching frame is a `return@enclosingFn` — fall through to the
                // normal function-return handling below (the label names the enclosing function).
                if let Some(lbl) = &ret_label {
                    if let Some((_, _, brk, _)) = self
                        .inline_lambda_ret
                        .iter()
                        .rev()
                        .find(|(l, ..)| l == lbl)
                        .cloned()
                    {
                        let mut stmts = Vec::new();
                        if let Some(e) = e {
                            stmts.push(self.expr(e)?);
                        }
                        stmts.push(self.ir.add_expr(IrExpr::Break { label: Some(brk) }));
                        return Some(self.ir.add_expr(IrExpr::Block { stmts, value: None }));
                    }
                }
                // Inside an expanded `inline fun` body: `return x` is not a real method return — it
                // assigns the inline result slot and breaks to the body's end label (the `do…while(false)`
                // wrapper). A `try { … } finally { … }` around the return is not yet combined with this
                // transfer, so bail that combination (the file skips — never a miscompile).
                if let Some((slot, label, rty)) = self.inline_return.last().cloned() {
                    if !self.try_finally_stack.is_empty() {
                        return None;
                    }
                    let mut stmts = Vec::new();
                    // A `Unit`-returning inline fn has no result slot — `return`/`return Unit` is a bare
                    // `break`. Otherwise assign the (coerced) value to the result slot, then break.
                    if rty != Ty::Unit {
                        let val = match e {
                            Some(e) if self.info.ty(e) != Ty::Nothing => self.lower_arg(e, &rty)?,
                            Some(e) => self.expr(e)?,
                            None => self.ir.add_expr(IrExpr::UnitInstance),
                        };
                        stmts.push(self.ir.add_expr(IrExpr::SetValue {
                            var: slot,
                            value: val,
                        }));
                    } else if let Some(e) = e {
                        // `return someUnitExpr` — run the expression for its side effects.
                        stmts.push(self.expr(e)?);
                    }
                    stmts.push(self.ir.add_expr(IrExpr::Break { label: Some(label) }));
                    return Some(self.ir.add_expr(IrExpr::Block { stmts, value: None }));
                }
                let v = match e {
                    // Coerce to the enclosing function's return type (generic-erased `Object` → cast).
                    Some(e) if self.cur_ret_ty != Ty::Unit && self.info.ty(e) != Ty::Nothing => {
                        let rt = self.cur_ret_ty.clone();
                        Some(self.lower_arg(e, &rt)?)
                    }
                    Some(e) => Some(self.expr(e)?),
                    // A valueless `return@lambda` in a `() -> Unit` closure method must `areturn` the
                    // `kotlin/Unit` singleton (the method's JVM return is a reference, not `void`).
                    None if self.cur_method_returns_unit_ref => {
                        Some(self.ir.add_expr(IrExpr::UnitInstance))
                    }
                    None => None,
                };
                // Inside one or more `try { … } finally { … }`, a `return` must run each enclosing
                // `finally` (innermost first) before transferring control — `{ val tmp = <value>;
                // <finally>…; return tmp }`. The value is captured into a temp first so a `finally` that
                // mutates state can't change what is returned (Kotlin evaluates the value, then runs the
                // finallys).
                if !self.try_finally_stack.is_empty() {
                    let finallys = self.try_finally_stack.clone();
                    let mut stmts = Vec::new();
                    let ret_val = match v {
                        Some(val) => {
                            let tmp = self.fresh_value();
                            let vty = self.cur_ret_ty.clone();
                            stmts.push(self.ir.add_expr(IrExpr::Variable {
                                index: tmp,
                                ty: vty,
                                init: Some(val),
                            }));
                            Some(tmp)
                        }
                        None => None,
                    };
                    // Inline each finally (innermost first). A `return` *inside* a finally must run
                    // only the finallys that enclose it — never itself — so lower finally `i` with the
                    // stack truncated to its enclosers (`finallys[..i]`, outermost-first). Without this,
                    // a finally whose body returns (e.g. `try { return 0 } finally { return 1 }`) would
                    // re-inline itself at its own `return` and recurse forever.
                    let saved = std::mem::take(&mut self.try_finally_stack);
                    for i in (0..finallys.len()).rev() {
                        self.try_finally_stack = finallys[..i].to_vec();
                        let lowered = self.expr(finallys[i]);
                        let Some(s) = lowered else {
                            self.try_finally_stack = saved;
                            return None;
                        };
                        stmts.push(s);
                    }
                    self.try_finally_stack = saved;
                    let rv = ret_val.map(|tmp| self.ir.add_expr(IrExpr::GetValue(tmp)));
                    stmts.push(self.ir.add_expr(IrExpr::Return(rv)));
                    return Some(self.ir.add_expr(IrExpr::Block { stmts, value: None }));
                }
                Some(self.ir.add_expr(IrExpr::Return(v)))
            }
            Stmt::Local { name, init, ty, .. } => {
                let init_ty = self.info.ty(init);
                // A diverging initializer (`val x = when { … all branches return … }`) never binds
                // anything — emit it for effect (it returns/throws), no slot store.
                if init_ty == Ty::Nothing {
                    return Some(self.expr(init)?);
                }
                // `Unit` as a stored value: kotlinc runs the initializer for effect, then binds the
                // `kotlin.Unit` singleton to the slot (a `kotlin/Unit` reference). `val u = f()` where
                // `f(): Unit` → run `f()`, then store `GETSTATIC kotlin/Unit.INSTANCE`.
                if init_ty == Ty::Unit {
                    let unit_ty = Ty::obj("kotlin/Unit");
                    let side = self.expr(init)?;
                    let unit_val = self.ir.add_expr(IrExpr::UnitInstance);
                    let seq = self.ir.add_expr(IrExpr::Block {
                        stmts: vec![side],
                        value: Some(unit_val),
                    });
                    let v = self.fresh_value();
                    self.scope.push((name.clone(), v, unit_ty));
                    return Some(self.ir.add_expr(IrExpr::Variable {
                        index: v,
                        ty: ty_to_ir(unit_ty),
                        init: Some(seq),
                    }));
                }
                // Use the declared type only when it's a builtin krusty `Ty`; for a user/class type
                // (`val en: En`) `Ty::from_name` is `None`, so fall back to the checker's inferred
                // type — otherwise the local is typed `Error` and e.g. `==` takes the wrong path.
                // Resolve the declared type: a builtin, else a known file class (`A?` → reference
                // `A`, not the `null` initializer's `Ty::Null`), else the checker's inferred type.
                let kty = match ty.as_ref() {
                    // A declared function type (`val f: (C) -> Int`): use the annotation's `Ty::Fun`, not
                    // the initializer's type — a property reference `C::n` is typed `KProperty1`, but the
                    // slot (and any `f(arg)` invoke) must see the function type it was declared as.
                    Some(r) if !r.fun_params.is_empty() || r.name == "<fun>" => {
                        ty_of(self.afile, r)
                    }
                    // A nullable primitive (`Char?`) is `Nullable(prim)`, a reference slot (consistent
                    // with the checker), else a boxed value is stored raw.
                    Some(r)
                        if r.nullable
                            && Ty::from_name(&r.name)
                                .and_then(Ty::nullable_boxed)
                                .is_some() =>
                    {
                        Ty::from_name(&r.name).unwrap().nullable_boxed().unwrap()
                    }
                    Some(r) if Ty::from_name(&r.name).is_some() => Ty::from_name(&r.name).unwrap(),
                    Some(r)
                        if self
                            .classes
                            .contains_key(&class_internal(self.afile, &r.name)) =>
                    {
                        Ty::obj(&class_internal(self.afile, &r.name))
                    }
                    // A library reference type (`Throwable?`, `List<Int>`): use the declared reference
                    // type, not the initializer's. A `null` initializer is `Ty::Null`, which would type
                    // the slot as `top` in frames; the declared reference type keeps the slot a
                    // reference so a later reassign + use across branches verifies.
                    Some(r) if self.catch_internal(&r.name).is_some() => {
                        Ty::obj(&self.catch_internal(&r.name).unwrap())
                    }
                    _ => init_ty,
                };
                // A mutable local captured (and written) by a closure is boxed into a `Ref$XxxRef`:
                // the local holds the holder, reads/writes go through `element`, and the closure
                // captures the shared holder (so its writes are visible here and vice versa).
                if self.info.boxed_vars.contains(&name) {
                    // A `@JvmInline value class` var is represented UNBOXED as its underlying type, so its
                    // `Ref` holder + element must use that underlying type (`var z: Z(Int)` → `Ref.IntRef`,
                    // not `Ref.ObjectRef` of an erased object — that mismatches the unboxed `int` value).
                    let elem_ty = self.value_class_underlying(kty).unwrap_or(kty);
                    let elem = ty_to_ir(elem_ty);
                    let it = self.lower_arg(init, &ty_to_ir(kty))?;
                    let holder = self.fresh_value();
                    let holder_ty = Ty::obj(ref_holder_internal(elem_ty));
                    self.scope.push((name.clone(), holder, holder_ty));
                    self.boxed_elem.insert(name.clone(), elem_ty);
                    // A single `Variable` (no scoping block) so the holder's slot lives in the enclosing
                    // scope — the closure's capture reads it later.
                    let new_ref = self.ir.add_expr(IrExpr::RefNew { elem, init: it });
                    return Some(self.ir.add_expr(IrExpr::Variable {
                        index: holder,
                        ty: ty_to_ir(holder_ty),
                        init: Some(new_ref),
                    }));
                }
                // Coerce the initializer to the declared type (a generic-erased `Object` flowing into a
                // typed `val` gets the `checkcast` kotlinc inserts).
                let it = self.lower_arg(init, &ty_to_ir(kty))?;
                let v = self.fresh_value();
                self.scope.push((name.clone(), v, kty));
                // A nullable-declared local (`val v: X? = null`) carries its nullability into the IrType —
                // `Ty` drops it, but the JVM value-class pass keys boxing (`X(null)` vs `null`) on it.
                let mut var_ty = ty_to_ir(kty);
                // The local is nullable if explicitly declared `?`, OR (when inferred) its initializer is a
                // call to a function with a nullable return — `Ty` drops nullability, so a `val x = zap()`
                // where `zap(): ZN2?` would otherwise type `x` non-null and the JVM value-class pass would
                // treat a boxed `ZN2?` as unboxed.
                let nullable = match ty.as_ref() {
                    Some(r) => r.nullable,
                    None => {
                        if let ast::Expr::Call { callee, .. } = self.afile.expr(init) {
                            match self.afile.expr(*callee) {
                                // A free-function call (`val x = zap()` where `zap(): T?`).
                                ast::Expr::Name(n) => self.afile.decls.iter().any(|&d| matches!(self.afile.decl(d),
                                    ast::Decl::Fun(f) if &f.name == n && f.ret.as_ref().is_some_and(|rr| rr.nullable))),
                                // A method call (`val x = t.foo()` where `foo(): T?`): resolve the method on the
                                // receiver's class and read its (nullability-carrying) IR return type — a nullable
                                // value-class return (`X?`) is a boxed `X`, so the local must stay boxed, not unbox.
                                ast::Expr::Member { receiver, name } => {
                                    let recv_ty = self.info.ty(*receiver);
                                    recv_ty
                                        .obj_internal()
                                        .and_then(|internal| self.resolve_method(internal, name))
                                        .is_some_and(|(_, _, fid, _)| {
                                            self.ir.functions[fid as usize].ret.is_nullable()
                                        })
                                }
                                _ => false,
                            }
                        } else {
                            false
                        }
                    }
                };
                if nullable {
                    var_ty = mark_nullable(var_ty);
                }
                Some(self.ir.add_expr(IrExpr::Variable {
                    index: v,
                    ty: var_ty,
                    init: Some(it),
                }))
            }
            Stmt::LocalDelegate {
                is_var,
                name,
                ty,
                delegate,
            } => {
                let delegate_ty = self.info.ty(delegate);
                let delegate_internal = delegate_ty.obj_internal()?.to_string();
                // Same soundness guards as member delegation: only a concrete (non-value-class, no
                // `provideDelegate`) delegate whose `getValue` return matches the property type.
                let is_value_cls = |s: &str| {
                    self.syms
                        .class_by_internal(s)
                        .is_some_and(|cs| cs.value_field.is_some())
                };
                if is_value_cls(&delegate_internal)
                    || self
                        .syms
                        .method_of(&delegate_internal, "provideDelegate")
                        .is_some()
                    || delegate_getvalue_uses_property(self.afile, &delegate_internal)
                {
                    return None;
                }
                let gv = self.syms.method_of(&delegate_internal, "getValue")?;
                let prop_ty = ty.as_ref().map(|r| ty_of(self.afile, r)).unwrap_or(gv.ret);
                if gv.ret != prop_ty || prop_ty.obj_internal().is_some_and(is_value_cls) {
                    return None;
                }
                let desc_of = |sig: &crate::resolve::Signature| {
                    let mut s = String::from("(");
                    for pt in &sig.params {
                        s.push_str(&pt.descriptor());
                    }
                    s.push(')');
                    s.push_str(&sig.ret.descriptor());
                    s
                };
                let getvalue_desc = desc_of(&gv);
                let setvalue_desc = if is_var {
                    Some(desc_of(
                        &self.syms.method_of(&delegate_internal, "setValue")?,
                    ))
                } else {
                    None
                };
                // Lower the delegate into a fresh `$delegate` local.
                let dv = self.fresh_value();
                let init = self.lower_arg(delegate, &ty_to_ir(delegate_ty))?;
                self.scope
                    .push((format!("{name}$delegate"), dv, delegate_ty));
                self.local_delegated.insert(
                    name.clone(),
                    LocalDelegate {
                        delegate_internal,
                        getvalue_desc,
                        setvalue_desc,
                        name: name.clone(),
                        ret_desc: prop_ty.descriptor(),
                    },
                );
                Some(self.ir.add_expr(IrExpr::Variable {
                    index: dv,
                    ty: ty_to_ir(delegate_ty),
                    init: Some(init),
                }))
            }
            Stmt::Destructure { entries, init } => {
                // A direct `stmt()` call wraps the bindings in a Block; the block builders use
                // `append_stmt` instead so the component locals live in the enclosing scope.
                let mut out = Vec::new();
                self.lower_destructure(&entries, init, &mut out)?;
                Some(self.ir.add_expr(IrExpr::Block {
                    stmts: out,
                    value: None,
                }))
            }
            Stmt::Assign { name, value } => {
                // `field = …` inside a custom setter body writes the property's backing field.
                if name == "field" {
                    if let Some((class_id, fidx, fty)) = self.cur_field.clone() {
                        let val = self.lower_arg(value, &fty)?;
                        let this_e = self.ir.add_expr(IrExpr::GetValue(0));
                        return Some(self.ir.add_expr(IrExpr::SetField {
                            receiver: this_e,
                            class: class_id,
                            index: fidx,
                            value: val,
                        }));
                    }
                }
                // A local delegated `var`: write through the delegate's `setValue(null, propref, value)`.
                if let Some(ld) = self.local_delegated.get(&name).cloned() {
                    let setvalue_desc = ld.setvalue_desc.clone()?;
                    let sv = self.syms.method_of(&ld.delegate_internal, "setValue")?;
                    let value_ty = sv.params.last().copied().unwrap_or(Ty::Error);
                    let val = self.lower_arg(value, &ty_to_ir(value_ty))?;
                    let (dslot, _) = self.lookup(&format!("{name}$delegate"))?;
                    let dele = self.ir.add_expr(IrExpr::GetValue(dslot));
                    let null_a = self.ir.add_expr(IrExpr::Const(IrConst::Null));
                    let pref = self.make_local_propref(&ld);
                    return Some(self.ir.add_expr(IrExpr::Call {
                        callee: crate::ir::Callee::Virtual {
                            owner: ld.delegate_internal.clone(),
                            name: "setValue".to_string(),
                            descriptor: setvalue_desc,
                            interface: false,
                        },
                        dispatch_receiver: Some(dele),
                        args: vec![null_a, pref, val],
                    }));
                }
                // A boxed mutable-capture local: write through its `Ref` holder's `element`.
                if let Some(elem) = self.boxed_elem.get(&name).cloned() {
                    let (holder, _) = self.lookup(&name)?;
                    let hv = self.ir.add_expr(IrExpr::GetValue(holder));
                    let val = self.lower_arg(value, &ty_to_ir(elem))?;
                    return Some(self.ir.add_expr(IrExpr::RefSet {
                        holder: hv,
                        elem: ty_to_ir(elem),
                        value: val,
                    }));
                }
                // A backing field of the enclosing class (`this.<field>`) shadows a same-named top-level
                // property — resolve it BEFORE `statics` (kotlinc: a member's unqualified name binds to the
                // class member first). Requires `this` in scope (a class member, not a top-level function).
                let own_field = self.lookup("this").and_then(|(this_v, _)| {
                    self.cur_class.as_ref().and_then(|c| {
                        // A custom-accessor property writes through `setX`, never the raw field.
                        if self
                            .field_accessor_props
                            .contains(&(c.clone(), name.clone()))
                        {
                            return None;
                        }
                        self.classes.get(c).and_then(|ci| {
                            ci.fields
                                .iter()
                                .position(|(fn_, _)| *fn_ == name)
                                .map(|i| (this_v, ci.id, i as u32, ty_to_ir(ci.fields[i].1)))
                        })
                    })
                });
                if let Some((v, sty)) = self.lookup(&name) {
                    // Coerce to the slot's declared type — a generic-erased `Object` value assigned to a
                    // typed `var` gets the `checkcast` kotlinc inserts (else the slot frame is
                    // inconsistent: `String?` at init vs `Object` after the assignment).
                    let val = self.lower_arg(value, &ty_to_ir(sty))?;
                    Some(self.ir.add_expr(IrExpr::SetValue { var: v, value: val }))
                } else if let Some((this_v, class, idx, field_ty)) = own_field {
                    let recv = self.ir.add_expr(IrExpr::GetValue(this_v));
                    let val = self.lower_arg(value, &field_ty)?;
                    Some(self.ir.add_expr(IrExpr::SetField {
                        receiver: recv,
                        class,
                        index: idx,
                        value: val,
                    }))
                } else if let Some((idx, ty)) = self.statics.get(&name).cloned() {
                    let val = self.lower_arg(value, &ty_to_ir(ty))?;
                    Some(self.ir.add_expr(IrExpr::SetStatic {
                        index: idx,
                        value: val,
                    }))
                } else if let Some((facade, ty, is_var, _)) =
                    self.syms.prop_facades.get(&name).cloned()
                {
                    // A `var` from ANOTHER file → call its facade's `setX(v)` (the field is private).
                    if !is_var {
                        return None;
                    }
                    let val = self.lower_arg(value, &ty_to_ir(ty))?;
                    Some(self.ir.add_expr(IrExpr::Call {
                        callee: Callee::CrossFile {
                            facade,
                            name: setter_name(&name),
                            params: vec![ty_to_ir(ty)],
                            ret: Ty::Unit,
                        },
                        dispatch_receiver: None,
                        args: vec![val],
                    }))
                } else {
                    // `this` is an external receiver (an inlined `apply`/`run` whose backing field is
                    // private) — write through the property setter `setX(v)`.
                    let (this_v, this_ty) = self.lookup("this")?;
                    let recv = self.ir.add_expr(IrExpr::GetValue(this_v));
                    {
                        let internal = this_ty.obj_internal()?.to_string();
                        let (sclass, sindex, sfid, _) =
                            self.resolve_method(&internal, &setter_name(&name))?;
                        let pty = self.ir.functions[sfid as usize]
                            .params
                            .first()
                            .cloned()
                            .unwrap_or_else(|| ty_to_ir(Ty::obj("kotlin/Any")));
                        let val = self.lower_arg(value, &pty)?;
                        Some(self.ir.add_expr(IrExpr::MethodCall {
                            class: sclass,
                            index: sindex,
                            receiver: recv,
                            args: vec![Some(val)],
                        }))
                    }
                }
            }
            // `name++` / `name--` on a local numeric variable → `name = name ± 1`. (In statement
            // position the pre/post distinction is irrelevant — the value isn't observed.) A built-in
            // numeric primitive only; a `var` field/property or a user `operator inc`/`dec` bails.
            Stmt::IncDec { name, dec } => {
                // A boxed mutable-capture local: `x++`/`x--` reads/writes through its `Ref` holder.
                if let Some(elem) = self.boxed_elem.get(&name).cloned() {
                    let (holder, _) = self.lookup(&name)?;
                    let one = match elem {
                        Ty::Int | Ty::Byte | Ty::Short | Ty::Char => IrConst::Int(1),
                        Ty::Long => IrConst::Long(1),
                        Ty::Double => IrConst::Double(1.0),
                        Ty::Float => IrConst::Float(1.0),
                        _ => return None,
                    };
                    let op = if dec { IrBinOp::Sub } else { IrBinOp::Add };
                    let hv = self.ir.add_expr(IrExpr::GetValue(holder));
                    let cur = self.ir.add_expr(IrExpr::RefGet {
                        holder: hv,
                        elem: ty_to_ir(elem),
                    });
                    let one = self.ir.add_expr(IrExpr::Const(one));
                    let sum = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op,
                        lhs: cur,
                        rhs: one,
                    });
                    let nv = if matches!(elem, Ty::Byte | Ty::Short | Ty::Char) {
                        self.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg: sum,
                            type_operand: ty_to_ir(elem),
                        })
                    } else {
                        sum
                    };
                    let hv2 = self.ir.add_expr(IrExpr::GetValue(holder));
                    return Some(self.ir.add_expr(IrExpr::RefSet {
                        holder: hv2,
                        elem: ty_to_ir(elem),
                        value: nv,
                    }));
                }
                // A `var` field of the enclosing class (`this.x++` written bare) inside its own method —
                // `this.x = this.x ± 1` via a direct field read/write. (`obj.x++`/`arr[i]++` were already
                // desugared to a compound assignment by the parser; an external `this`, e.g. an inlined
                // `apply`, isn't handled here.)
                if self.lookup(&name).is_none() {
                    let (this_v, this_ty) = self.lookup("this")?;
                    let internal = this_ty.obj_internal()?.to_string();
                    let (fty, is_var) = self.syms.prop_of(&internal, &name)?;
                    if !is_var {
                        return None;
                    }
                    let one_c = match fty {
                        Ty::Int | Ty::Byte | Ty::Short | Ty::Char => IrConst::Int(1),
                        Ty::Long => IrConst::Long(1),
                        Ty::Double => IrConst::Double(1.0),
                        Ty::Float => IrConst::Float(1.0),
                        _ => return None,
                    };
                    let op = if dec { IrBinOp::Sub } else { IrBinOp::Add };
                    // A field of *this* class is read/written directly; an inherited one (or an external
                    // `this`), or a CUSTOM-accessor property, goes through its getter/setter accessors.
                    let own = self.cur_class.as_ref().and_then(|c| {
                        if self
                            .field_accessor_props
                            .contains(&(c.clone(), name.clone()))
                        {
                            return None;
                        }
                        self.classes.get(c).and_then(|ci| {
                            ci.fields
                                .iter()
                                .position(|(fn_, _)| *fn_ == name)
                                .map(|i| (ci.id, i as u32))
                        })
                    });
                    let recv = self.ir.add_expr(IrExpr::GetValue(this_v));
                    let cur_val = if let Some((class, idx)) = own {
                        self.ir.add_expr(IrExpr::GetField {
                            receiver: recv,
                            class,
                            index: idx,
                        })
                    } else {
                        let (gclass, gindex, _, _) =
                            self.resolve_method(&internal, &getter_name(&name))?;
                        self.ir.add_expr(IrExpr::MethodCall {
                            class: gclass,
                            index: gindex,
                            receiver: recv,
                            args: vec![],
                        })
                    };
                    let one = self.ir.add_expr(IrExpr::Const(one_c));
                    let nv = if matches!(fty, Ty::Byte | Ty::Short | Ty::Char) {
                        let cur_i = self.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg: cur_val,
                            type_operand: ty_to_ir(Ty::Int),
                        });
                        let sum = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op,
                            lhs: cur_i,
                            rhs: one,
                        });
                        self.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg: sum,
                            type_operand: ty_to_ir(fty),
                        })
                    } else {
                        self.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op,
                            lhs: cur_val,
                            rhs: one,
                        })
                    };
                    let recv2 = self.ir.add_expr(IrExpr::GetValue(this_v));
                    return Some(if let Some((class, idx)) = own {
                        self.ir.add_expr(IrExpr::SetField {
                            receiver: recv2,
                            class,
                            index: idx,
                            value: nv,
                        })
                    } else {
                        let (sclass, sindex, _, _) =
                            self.resolve_method(&internal, &setter_name(&name))?;
                        self.ir.add_expr(IrExpr::MethodCall {
                            class: sclass,
                            index: sindex,
                            receiver: recv2,
                            args: vec![Some(nv)],
                        })
                    });
                }
                let (v, ty) = self.lookup(&name)?;
                let one = match ty {
                    Ty::Int | Ty::Byte | Ty::Short | Ty::Char => IrConst::Int(1),
                    Ty::Long => IrConst::Long(1),
                    Ty::Double => IrConst::Double(1.0),
                    Ty::Float => IrConst::Float(1.0),
                    _ => return None,
                };
                let op = if dec { IrBinOp::Sub } else { IrBinOp::Add };
                let one = self.ir.add_expr(IrExpr::Const(one));
                let nv = if matches!(ty, Ty::Byte | Ty::Short | Ty::Char) {
                    // `Byte`/`Short`/`Char` arithmetic widens to `Int`, then narrows back so the value
                    // wraps in its own width (`Byte.MAX_VALUE++` = `Byte.MIN_VALUE`, not 128). The widen
                    // forces an `Int`-typed result so the final narrow actually emits `i2b`/`i2s`/`i2c`.
                    let cur = self.ir.add_expr(IrExpr::GetValue(v));
                    let cur_i = self.ir.add_expr(IrExpr::TypeOp {
                        op: IrTypeOp::ImplicitCoercion,
                        arg: cur,
                        type_operand: ty_to_ir(Ty::Int),
                    });
                    let sum = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op,
                        lhs: cur_i,
                        rhs: one,
                    });
                    self.ir.add_expr(IrExpr::TypeOp {
                        op: IrTypeOp::ImplicitCoercion,
                        arg: sum,
                        type_operand: ty_to_ir(ty),
                    })
                } else {
                    let cur = self.ir.add_expr(IrExpr::GetValue(v));
                    self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op,
                        lhs: cur,
                        rhs: one,
                    })
                };
                Some(self.ir.add_expr(IrExpr::SetValue { var: v, value: nv }))
            }
            // `receiver.field = value` → `IrSetField` (var property of a class in this IR).
            Stmt::AssignMember {
                receiver,
                name,
                value,
            } => {
                let rt = self.info.ty(receiver);
                // A `var` extension property write (`x.name = v`) → its static setter `setName(x, v)`.
                if let Some(&sfid) = self.ext_prop_set_ids.get(&(rt.descriptor(), name.clone())) {
                    let pty = self.ir.functions[sfid as usize]
                        .params
                        .get(1)
                        .cloned()
                        .unwrap_or_else(|| ty_to_ir(self.info.ty(value)));
                    let r = self.expr(receiver)?;
                    let v = self.lower_arg(value, &pty)?;
                    return Some(self.ir.add_expr(IrExpr::Call {
                        callee: Callee::Local(sfid),
                        dispatch_receiver: None,
                        args: vec![r, v],
                    }));
                }
                // A property write on a `var` of a class defined in ANOTHER file → its `setX(v)` accessor
                // (the backing field is private). A cross-file `val` write bails.
                if self.class_of(rt).is_none() {
                    if let Ty::Obj(i, _) = &rt {
                        if let Some((owner, pty, is_var, interface)) = self
                            .syms
                            .class_by_internal(i)
                            .filter(|cs| cs.value_field.is_none())
                            .and_then(|cs| {
                                cs.props
                                    .iter()
                                    .find(|(n, _, _)| n == &name)
                                    .map(|(_, t, v)| (i.to_string(), *t, *v, cs.is_interface))
                            })
                        {
                            if !is_var {
                                return None;
                            }
                            let r = self.expr(receiver)?;
                            let v = self.lower_arg(value, &ty_to_ir(pty))?;
                            return Some(self.ir.add_expr(IrExpr::Call {
                                callee: Callee::CrossFileVirtual {
                                    owner,
                                    name: setter_name(&name),
                                    params: vec![ty_to_ir(pty)],
                                    ret: Ty::Unit,
                                    interface,
                                },
                                dispatch_receiver: Some(r),
                                args: vec![v],
                            }));
                        }
                    }
                }
                let owner_internal = self.class_of(rt)?.internal.clone();
                // The backing field is private; a write from outside the declaring class goes through
                // the public `setX()` accessor (matching kotlinc). Inside the class, write directly.
                if self.cur_class.as_deref() != Some(owner_internal.as_str()) {
                    if let Some((mclass, mindex, mfid, _)) =
                        self.resolve_method(&owner_internal, &setter_name(&name))
                    {
                        let pty = self.ir.functions[mfid as usize].params[0].clone();
                        let r = self.expr(receiver)?;
                        let v = self.lower_arg(value, &pty)?;
                        return Some(self.ir.add_expr(IrExpr::MethodCall {
                            class: mclass,
                            index: mindex,
                            receiver: r,
                            args: vec![Some(v)],
                        }));
                    }
                }
                let (class, idx, field_ty) = {
                    let ci = self.class_of(rt)?;
                    let idx = ci.fields.iter().position(|(fn_, _)| *fn_ == name)? as u32;
                    (ci.id, idx, ty_to_ir(ci.fields[idx as usize].1))
                };
                let r = self.expr(receiver)?;
                // Coerce the value to the field's type (e.g. `Int` literal into a `Long` field).
                let v = self.lower_arg(value, &field_ty)?;
                Some(self.ir.add_expr(IrExpr::SetField {
                    receiver: r,
                    class,
                    index: idx,
                    value: v,
                }))
            }
            Stmt::AssignIndex {
                array,
                index,
                value,
            } => {
                let at = self.info.ty(array);
                // `m[i] = v` on a USER class with an `operator fun set(index, value)` → `m.set(i, v)`
                // (walks supers, so an inherited operator resolves — consistent with the checker).
                if let Ty::Obj(internal, _) = at {
                    if at.array_elem().is_none() {
                        let setm = self
                            .resolve_method(internal, "set")
                            .map(|(class, midx, fid, _)| (class, midx, fid));
                        if let Some((class, midx, fid)) = setm {
                            let ptys = self.ir.functions[fid as usize].params.clone();
                            let ity = ty_to_ir(self.info.ty(index));
                            let vty = ty_to_ir(self.info.ty(value));
                            let a = self.expr(array)?;
                            let i = self.lower_arg(index, ptys.first().unwrap_or(&ity))?;
                            let v = self.lower_arg(value, ptys.get(1).unwrap_or(&vty))?;
                            return Some(self.ir.add_expr(IrExpr::MethodCall {
                                class,
                                index: midx,
                                receiver: a,
                                args: vec![Some(i), Some(v)],
                            }));
                        }
                    }
                }
                // `coll[i] = v` on a library type → its `set(index, value)` operator member, discarding
                // the returned previous element (an array set stays the `kotlin/Array.set` intrinsic).
                if let Ty::Obj(internal, _) = at {
                    if at.array_elem().is_none() {
                        let (it, vt) = (self.info.ty(index), self.info.ty(value));
                        // `MutableList.set(Int, E)`, or `MutableMap.put(K, V)` — Kotlin's `m[k] = v`
                        // operator maps to `put` on a map.
                        let resolved = crate::call_resolver::resolve_instance(
                            &*self.syms.libraries,
                            internal,
                            "set",
                            &[it, vt],
                        )
                        .map(|m| ("set", m))
                        .or_else(|| {
                            crate::call_resolver::resolve_instance(
                                &*self.syms.libraries,
                                internal,
                                "put",
                                &[it, vt],
                            )
                            .map(|m| ("put", m))
                        });
                        if let Some((mname, m)) = resolved {
                            // A narrowing store into a primitive-element collection (`List<Byte>[i] = intVal`)
                            // needs `(value).toByte()` before boxing as `java/lang/Byte` — not yet modeled.
                            // Bail (skip the file) rather than box the wrong wrapper type.
                            if let Some(elem) = self.syms.libraries.member_return(at, "get", &[it])
                            {
                                if elem.is_primitive() && elem != vt {
                                    return None;
                                }
                            }
                            let is_iface = self
                                .syms
                                .libraries
                                .resolve_type(internal)
                                .map_or(false, |t| t.is_interface());
                            let a = self.expr(array)?;
                            let i = self.lower_arg(
                                index,
                                &ty_to_ir(m.params.first().copied().unwrap_or(it)),
                            )?;
                            let v = self.lower_arg(
                                value,
                                &ty_to_ir(m.params.get(1).copied().unwrap_or(vt)),
                            )?;
                            return Some(self.ir.add_expr(IrExpr::Call {
                                callee: Callee::Virtual {
                                    owner: internal.to_string(),
                                    name: mname.to_string(),
                                    descriptor: m.descriptor.clone(),
                                    interface: is_iface,
                                },
                                dispatch_receiver: Some(a),
                                args: vec![i, v],
                            }));
                        }
                    }
                }
                let a = self.expr(array)?;
                let i = self.expr(index)?;
                let v = self.expr(value)?;
                Some(self.ir.add_expr(IrExpr::Call {
                    callee: Callee::External("kotlin/Array.set".to_string()),
                    dispatch_receiver: Some(a),
                    args: vec![i, v],
                }))
            }
            Stmt::While { cond, body, label } => {
                let c = self.expr(cond)?;
                let depth = self.scope.len();
                let mut out = Vec::new();
                self.append_body_stmts(body, &mut out)?;
                self.scope.truncate(depth);
                let b = self.ir.add_expr(IrExpr::Block {
                    stmts: out,
                    value: None,
                });
                Some(self.ir.add_expr(IrExpr::While {
                    cond: c,
                    body: b,
                    update: None,
                    post_test: false,
                    label,
                }))
            }
            Stmt::DoWhile { body, cond, label } => {
                let depth = self.scope.len();
                let mut out = Vec::new();
                self.append_body_stmts(body, &mut out)?;
                self.scope.truncate(depth);
                // The condition is lowered after the body's scope is dropped — a `do…while` condition
                // can't see body-local declarations (Kotlin scopes them to the body).
                let c = self.expr(cond)?;
                let b = self.ir.add_expr(IrExpr::Block {
                    stmts: out,
                    value: None,
                });
                Some(self.ir.add_expr(IrExpr::While {
                    cond: c,
                    body: b,
                    update: None,
                    post_test: true,
                    label,
                }))
            }
            Stmt::Break(label) => Some(self.ir.add_expr(IrExpr::Break { label })),
            Stmt::Continue(label) => Some(self.ir.add_expr(IrExpr::Continue { label })),
            // `for (i in a..b [step s])` over an `Int` range → a counted `while`. The bound is
            // hoisted to a local (evaluated once, per Kotlin); the step defaults to 1.
            Stmt::For {
                name,
                range,
                body,
                label,
            } => {
                use crate::ast::RangeKind;
                let depth = self.scope.len();
                // The counter type is the bound type (`Int`, `Long`, or unsigned `UInt`/`ULong`). A
                // `Byte`/`Short` range widens to an `IntRange`, so the counter is `Int` and the bounds
                // coerce up (matching the checker and `Short.rangeTo(Short): IntRange`).
                let elem = match self.info.ty(range.start) {
                    Ty::Byte | Ty::Short => Ty::Int,
                    t => t,
                };
                let elem_ir = ty_to_ir(elem);
                let one = if matches!(elem, Ty::Long | Ty::ULong) {
                    IrConst::Long(1)
                } else {
                    IrConst::Int(1)
                };
                // loop var = start. The bounds may be erased (`l[0]` → `Object`); coerce them to the
                // counter's primitive type so the value is unboxed before the slot store.
                let start = self.lower_arg(range.start, &elem_ir)?;
                let i_v = self.fresh_value();
                self.scope.push((name.clone(), i_v, elem));
                let var_i = self.ir.add_expr(IrExpr::Variable {
                    index: i_v,
                    ty: elem_ir.clone(),
                    init: Some(start),
                });
                // The bound. kotlinc folds a CONSTANT bound with unit step into a single `i < C` exclusive
                // test — no hoisted local, no overflow guard: `1..10` → `i < 11`, `0 until 10` → `i < 10`.
                // Match that for a literal `Int` bound; every other case hoists the (possibly
                // side-effecting / non-constant) bound into a temp and keeps the overflow-safe shape.
                // The exclusive comparison constant: `until C` → `C` (`i < C`); `..C` → `C+1` (`i < C+1`);
                // `downTo C` → `C-1` (`C-1 < i`, i.e. `i > C-1`). The `±1` folds must not over/underflow.
                let inline_bound: Option<i32> = if elem == Ty::Int {
                    match self.afile.expr(range.end) {
                        Expr::IntLit(v) => {
                            let v = *v;
                            match range.kind {
                                RangeKind::Until if i32::try_from(v).is_ok() => Some(v as i32),
                                RangeKind::Through if v < i32::MAX as i64 => Some(v as i32 + 1),
                                RangeKind::DownTo if v > i32::MIN as i64 => Some(v as i32 - 1),
                                _ => None,
                            }
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                // A bound that is already a plain local (not reassigned in the body) is read directly —
                // kotlinc does not hoist it into a second slot. Only a complex/reassigned bound is hoisted.
                let end_local = if inline_bound.is_none() {
                    self.expr_as_reusable_local(range.end, body)
                } else {
                    None
                };
                let (var_end, end_v) = if inline_bound.is_some() {
                    (None, None)
                } else if let Some(idx) = end_local {
                    (None, Some(idx))
                } else {
                    let end_e = self.lower_arg(range.end, &elem_ir)?;
                    let ev = self.fresh_value();
                    let var = self.ir.add_expr(IrExpr::Variable {
                        index: ev,
                        ty: elem_ir.clone(),
                        init: Some(end_e),
                    });
                    (Some(var), Some(ev))
                };
                // condition. Constant bound: a single `i < C`. Otherwise the form's comparison against the
                // hoisted bound (unsigned via `compareUnsigned(i, end) <op> 0`, since a signed `<=` would
                // misorder values past the sign bit).
                let cmp = match range.kind {
                    RangeKind::Through => IrBinOp::Le,
                    RangeKind::Until => IrBinOp::Lt,
                    RangeKind::DownTo => IrBinOp::Ge,
                };
                let cond = if let Some(b) = inline_bound {
                    let gi = self.ir.add_expr(IrExpr::GetValue(i_v));
                    let c = self.ir.add_expr(IrExpr::Const(IrConst::Int(b)));
                    // Descending compares the constant against the counter (`C-1 < i`) so the emitted
                    // operand order (`iconst C-1; iload i; if_icmpge`) matches kotlinc; ascending is `i < C`.
                    let (lhs, rhs) = if matches!(range.kind, RangeKind::DownTo) {
                        (c, gi)
                    } else {
                        (gi, c)
                    };
                    self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: IrBinOp::Lt,
                        lhs,
                        rhs,
                    })
                } else if elem.is_unsigned() {
                    let gi = self.ir.add_expr(IrExpr::GetValue(i_v));
                    let ge = self.ir.add_expr(IrExpr::GetValue(end_v.unwrap()));
                    let (owner, prim) = if elem == Ty::UInt {
                        ("java/lang/Integer", "I")
                    } else {
                        ("java/lang/Long", "J")
                    };
                    let cmp_call = self.ir.add_expr(IrExpr::Call {
                        callee: Callee::Static {
                            owner: owner.to_string(),
                            name: "compareUnsigned".to_string(),
                            descriptor: format!("({prim}{prim})I"),
                            inline: false,
                            must_inline: false,
                        },
                        dispatch_receiver: None,
                        args: vec![gi, ge],
                    });
                    let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                    self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: cmp,
                        lhs: cmp_call,
                        rhs: zero,
                    })
                } else {
                    let gi = self.ir.add_expr(IrExpr::GetValue(i_v));
                    let ge = self.ir.add_expr(IrExpr::GetValue(end_v.unwrap()));
                    self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: cmp,
                        lhs: gi,
                        rhs: ge,
                    })
                };
                // body + increment
                let mut out = Vec::new();
                if self.append_body_stmts(body, &mut out).is_none() {
                    self.scope.truncate(depth);
                    return None;
                }
                // The counted `Stmt::For` is always unit-step: a range with a `step` (or any trailing
                // infix) is parsed as a progression value and lowered by `lower_foreach_progression`.
                let step = self.ir.add_expr(IrExpr::Const(one));
                let inc_op = if matches!(range.kind, RangeKind::DownTo) {
                    IrBinOp::Sub
                } else {
                    IrBinOp::Add
                };
                let gi2 = self.ir.add_expr(IrExpr::GetValue(i_v));
                let inc_val = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: inc_op,
                    lhs: gi2,
                    rhs: step,
                });
                // The increment is the loop `update` (runs at the `continue` target), not a body stmt —
                // so `continue` advances the counter instead of skipping it.
                let inc = self.ir.add_expr(IrExpr::SetValue {
                    var: i_v,
                    value: inc_val,
                });
                // Non-overflowing loop: break when the counter reaches the (inclusive) bound, *before*
                // the increment — so `0..Int.MAX_VALUE` / `x downTo Int.MIN_VALUE` don't wrap past it and
                // loop forever. The break + increment are the loop `update` (the `continue` target), so a
                // `continue` also hits the bound check instead of skipping to the wrapping increment.
                // Skip the overflow break entirely when the loop can't wrap past its bound: a constant
                // inline bound (folded to `i < C`), or an exclusive `until` (the counter never reaches
                // `end`). kotlinc emits no guard there. Every other form keeps the `i == end` break.
                let no_guard = inline_bound.is_some() || matches!(range.kind, RangeKind::Until);
                let update = if no_guard {
                    inc
                } else {
                    let end_v = end_v.unwrap();
                    let ic = self.ir.add_expr(IrExpr::GetValue(i_v));
                    let ec = self.ir.add_expr(IrExpr::GetValue(end_v));
                    let at_end = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: IrBinOp::Eq,
                        lhs: ic,
                        rhs: ec,
                    });
                    let brk = self.ir.add_expr(IrExpr::Break { label: None });
                    let if_break = self.ir.add_expr(IrExpr::When {
                        branches: vec![(Some(at_end), brk)],
                    });
                    self.ir.add_expr(IrExpr::Block {
                        stmts: vec![if_break, inc],
                        value: None,
                    })
                };
                let wbody = self.ir.add_expr(IrExpr::Block {
                    stmts: out,
                    value: None,
                });
                let wh = self.ir.add_expr(IrExpr::While {
                    cond,
                    body: wbody,
                    update: Some(update),
                    post_test: false,
                    label,
                });
                self.scope.truncate(depth);
                let mut prologue = vec![var_i];
                if let Some(ve) = var_end {
                    prologue.push(ve);
                }
                prologue.push(wh);
                Some(self.ir.add_expr(IrExpr::Block {
                    stmts: prologue,
                    value: None,
                }))
            }
            // `for (x in arr)` over an array → an index loop `i=0; while (i<arr.size) { x=arr[i]; …; i++ }`.
            Stmt::ForEach {
                name,
                iterable,
                body,
                label,
            } => self.lower_for_each(&name, iterable, body, label),
            // A local-function declaration emits no code here — its body is lifted to a separate static
            // method (pass 2'); a call to it routes to that method.
            Stmt::LocalFun(_) => Some(self.ir.add_expr(IrExpr::Block {
                stmts: vec![],
                value: None,
            })),
            // A local class is lowered via its hoisted top-level `Decl::Class`; the in-body statement
            // emits nothing.
            Stmt::LocalClass(_) => Some(self.ir.add_expr(IrExpr::Block {
                stmts: vec![],
                value: None,
            })),
        }
    }

    /// Lower a `for (name in iterable) body` (also the inlined target of `iterable.forEach { … }`):
    /// dispatch to the counted range loop, the array/`String` index loop, or the iterator protocol.
    fn lower_for_each(
        &mut self,
        name: &str,
        iterable: AstExprId,
        body: AstExprId,
        label: Option<String>,
    ) -> Option<u32> {
        let it_ty = self.info.ty(iterable);
        // A primitive range value (`IntRange`/`LongRange`/`CharRange`) iterates as a counted loop over
        // its `getFirst()`/`getLast()` bounds (step +1), matching kotlinc and avoiding per-element boxing.
        if let Some((elem, prim_desc)) = it_ty.obj_internal().and_then(range_counted_elem) {
            return self.lower_foreach_range(name, iterable, body, it_ty, elem, prim_desc, label);
        }
        // A progression value (`IntProgression`/`LongProgression`/…, e.g. from `downTo`/`step`)
        // iterates as a counted loop reading its `getStep()` — the increment may be negative.
        if let Some((elem, _)) = it_ty.obj_internal().and_then(progression_counted_elem) {
            return self.lower_foreach_progression(name, iterable, body, it_ty, elem, label);
        }
        // An array, or a `String` (iterated as its `Char`s), uses an index loop; any other iterable
        // (`List`, `Set`, a progression value, …) uses the iterator protocol.
        let elem = if it_ty == Ty::String {
            Some(Ty::Char)
        } else {
            it_ty.array_elem()
        };
        let Some(elem) = elem else {
            return self.lower_foreach_iterator(name, iterable, body, it_ty, None, label);
        };
        let depth = self.scope.len();
        // Evaluate the array once. When the iterable is ALREADY a plain (non-boxed) local, iterate on
        // that local directly — kotlinc reuses the existing slot rather than storing a redundant copy.
        let (arr_v, var_arr) = if let Some(v) = self.expr_as_reusable_local(iterable, body) {
            (v, None)
        } else {
            let v = self.fresh_value();
            let arr_val = self.expr(iterable)?;
            let var = self.ir.add_expr(IrExpr::Variable {
                index: v,
                ty: ty_to_ir(it_ty),
                init: Some(arr_val),
            });
            (v, Some(var))
        };
        // i = 0
        let i_v = self.fresh_value();
        let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
        let var_i = self.ir.add_expr(IrExpr::Variable {
            index: i_v,
            ty: ty_to_ir(Ty::Int),
            init: Some(zero),
        });
        // n = arr.size (hoisted)
        let n_v = self.fresh_value();
        let arr_g = self.ir.add_expr(IrExpr::GetValue(arr_v));
        let size_fq = if it_ty == Ty::String {
            "kotlin/String.length"
        } else {
            "kotlin/Array.size"
        };
        let size = self.ir.add_expr(IrExpr::Call {
            callee: Callee::External(size_fq.to_string()),
            dispatch_receiver: Some(arr_g),
            args: vec![],
        });
        let var_n = self.ir.add_expr(IrExpr::Variable {
            index: n_v,
            ty: ty_to_ir(Ty::Int),
            init: Some(size),
        });
        // condition: i < n
        let gi = self.ir.add_expr(IrExpr::GetValue(i_v));
        let gn = self.ir.add_expr(IrExpr::GetValue(n_v));
        let cond = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Lt,
            lhs: gi,
            rhs: gn,
        });
        // loop var `x = arr[i]`, bound for the body
        let x_v = self.fresh_value();
        self.scope.push((name.to_string(), x_v, elem));
        let arr_g2 = self.ir.add_expr(IrExpr::GetValue(arr_v));
        let gi2 = self.ir.add_expr(IrExpr::GetValue(i_v));
        let getq = if it_ty == Ty::String {
            "kotlin/String.get"
        } else {
            "kotlin/Array.get"
        };
        let elem_get = self.ir.add_expr(IrExpr::Call {
            callee: Callee::External(getq.to_string()),
            dispatch_receiver: Some(arr_g2),
            args: vec![gi2],
        });
        let var_x = self.ir.add_expr(IrExpr::Variable {
            index: x_v,
            ty: ty_to_ir(elem),
            init: Some(elem_get),
        });
        let mut out = vec![var_x];
        if self.append_body_stmts(body, &mut out).is_none() {
            self.scope.truncate(depth);
            return None;
        }
        let gi3 = self.ir.add_expr(IrExpr::GetValue(i_v));
        let one = self.ir.add_expr(IrExpr::Const(IrConst::Int(1)));
        let inc = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Add,
            lhs: gi3,
            rhs: one,
        });
        let incs = self.ir.add_expr(IrExpr::SetValue {
            var: i_v,
            value: inc,
        });
        let wbody = self.ir.add_expr(IrExpr::Block {
            stmts: out,
            value: None,
        });
        let wh = self.ir.add_expr(IrExpr::While {
            cond,
            body: wbody,
            update: Some(incs),
            post_test: false,
            label,
        });
        self.scope.truncate(depth);
        let mut stmts = Vec::new();
        if let Some(va) = var_arr {
            stmts.push(va);
        }
        stmts.extend([var_i, var_n, wh]);
        Some(self.ir.add_expr(IrExpr::Block { stmts, value: None }))
    }

    /// If `e` is a plain (non-boxed) local variable read that the loop `body` does NOT reassign, its IR
    /// value index — so a loop over it (`for (x in localArr)`, or a counted loop whose bound is a local)
    /// reads the existing local directly instead of snapshotting a copy, matching kotlinc. `None`
    /// otherwise (then it's stored into a fresh temp): kotlinc snapshots a value whose backing `var` is
    /// reassigned in the body so the loop keeps the ORIGINAL — reusing the live local would read the new.
    fn expr_as_reusable_local(&self, e: AstExprId, body: AstExprId) -> Option<u32> {
        if let Expr::Name(n) = self.afile.expr(e) {
            let n = n.clone();
            if self.boxed_elem.contains_key(&n) || self.expr_reassigns_name(body, &n) {
                return None;
            }
            return self.lookup(&n).map(|(v, _)| v);
        }
        None
    }

    /// Whether `name` is reassigned (`name = …`) anywhere in the expression subtree `e`.
    fn expr_reassigns_name(&self, e: AstExprId, name: &str) -> bool {
        self.afile
            .any_child_expr(e, &mut |x| self.expr_reassigns_name(x, name), &mut |s| {
                self.stmt_reassigns_name(s, name)
            })
    }

    /// Whether statement `s` (or a nested expression) reassigns `name`.
    fn stmt_reassigns_name(&self, s: crate::ast::StmtId, name: &str) -> bool {
        if let Stmt::Assign { name: n, .. } = self.afile.stmt(s) {
            if n == name {
                return true;
            }
        }
        self.afile
            .any_child_stmt(s, &mut |x| self.expr_reassigns_name(x, name))
    }

    /// Substitute a reified type-parameter reference (`is T`/`as T`/`T::class` inside an expanded
    /// `<reified T>` inline body) with the concrete type bound at the call site. Nullability is the
    /// union of the reference's and the bound type's. Unchanged when no reified binding is active.
    fn subst_type_ref(&self, tr: &ast::TypeRef) -> ast::TypeRef {
        for frame in self.reified_subst.iter().rev() {
            if let Some(bound) = frame.get(&tr.name) {
                let mut out = bound.clone();
                out.nullable = out.nullable || tr.nullable;
                return out;
            }
        }
        tr.clone()
    }

    /// Expand a call to a user-defined `inline fun`: bind its value parameters to the (once-evaluated)
    /// arguments, register its lambda arguments for inlining at their invoke sites, then lower its body
    /// in place — exactly what kotlinc's inliner does. Returns `None` (the file bails, never miscompiles)
    /// for anything outside the supported subset. `call_id` is the call expression (for reified type args).
    fn lower_inline_fn_call(
        &mut self,
        fname: &str,
        args: &[AstExprId],
        call_id: u32,
        recv: Option<AstExprId>,
    ) -> Option<u32> {
        // Find the matching top-level fn: an extension call wants the decl WITH a receiver, a plain call
        // the one WITHOUT (so an extension and a same-named plain fn don't shadow each other here).
        let f = self
            .afile
            .decls
            .iter()
            .find_map(|&d| match self.afile.decl(d) {
                Decl::Fun(f) if f.name == fname && f.receiver.is_some() == recv.is_some() => {
                    Some(f)
                }
                _ => None,
            })?
            .clone();
        // A non-extension call must hit a non-extension fn and vice versa. An extension binds its receiver
        // as `this` (below). No default/vararg params. Non-reified generic type params are SPECIALIZED
        // from the actual argument types; REIFIED type params are bound to the call's explicit type
        // arguments and substituted into `is T`/`as T`/`T::class` in the body.
        if f.receiver.is_some() != recv.is_some() {
            return None;
        }
        // The extension receiver type (`inline fun String.foo()` → `String`), or `None` for a plain fn.
        // A GENERIC receiver (`<T> T.foo()`) is specialized to the ACTUAL receiver's type at the call site.
        let recv_ty = match (&f.receiver, recv) {
            (Some(r), Some(ra)) => {
                let t = if f.type_params.iter().any(|tp| tp == &r.name) {
                    self.recv_ty(ra)
                } else {
                    ty_of(self.afile, r)
                };
                if t == Ty::Error {
                    return None;
                }
                Some(t)
            }
            (None, None) => None,
            _ => return None,
        };
        // Default parameters ARE modeled (an inline fn substitutes the default expression directly — no
        // `$default` method): an omitted parameter is filled with its default below. A `vararg` IS
        // supported, but only as the LAST parameter of a plain (non-extension) inline fn whose element
        // isn't a type parameter or a function type. The two aren't combined (rare) — bail on the overlap.
        let has_default = f.params.iter().any(|p| p.default.is_some());
        let vararg = f.params.last().is_some_and(|p| p.is_vararg);
        if has_default && vararg {
            return None;
        }
        if f.params.iter().rev().skip(1).any(|p| p.is_vararg) {
            return None; // a non-last vararg (only reachable with trailing named args) — bail
        }
        if vararg {
            let vp = f.params.last().unwrap();
            let is_tparam = f.type_params.iter().any(|tp| tp == &vp.ty.name);
            if recv_ty.is_some() || is_tparam || !vp.ty.fun_params.is_empty() {
                return None;
            }
        }
        let body = match f.body {
            FunBody::Expr(e) | FunBody::Block(e) => e,
            FunBody::None => return None,
        };
        let pnames: Vec<String> = f.params.iter().map(|p| p.name.clone()).collect();
        // A body with `return`s is wrapped in a `do { … } while(false)` and each `return` becomes
        // `result = x; break` (see below). A `return` lexically inside a *nested lambda* of the body
        // would be a non-local return targeting the WRONG inline level — but a lambda argument carrying a
        // return is already pre-bailed at registration, so the only returns that survive are direct.
        let has_return = body_has_return(self.afile, body);
        // Whether every path through the body returns/throws (its checked type is `Nothing`) — then the
        // `do…while` wrapper omits the unreachable fall-through (avoids an unframed dead `goto` tail).
        let body_diverges = self.info.ty(body) == Ty::Nothing;
        // Value-parameter types + return type. An extension is keyed in `ext_funs` by `(receiver
        // descriptor, name)` with value params only; a GENERIC extension isn't registered there (its
        // receiver erased to `Any`), so derive from the decl.
        let (sig_params, sig_ret): (Vec<Ty>, Ty) = if let Some(rt) = &recv_ty {
            if let Some(s) = self
                .syms
                .ext_funs
                .get(&(rt.descriptor(), fname.to_string()))
            {
                (s.params.clone(), s.ret)
            } else {
                let ps = f.params.iter().map(|p| ty_of(self.afile, &p.ty)).collect();
                let r = f.ret.as_ref().map_or(Ty::Unit, |t| ty_of(self.afile, t));
                (ps, r)
            }
        } else {
            let sigs = self.syms.funs.get(fname)?;
            let s = if sigs.len() == 1 {
                sigs[0].clone()
            } else {
                let want: String = f
                    .params
                    .iter()
                    .map(|p| ty_of(self.afile, &p.ty).descriptor())
                    .collect();
                sigs.iter()
                    .find(|s| crate::resolve::erased_params_key(s) == want)?
                    .clone()
            };
            (s.params.clone(), s.ret)
        };
        if sig_params.len() != pnames.len() {
            return None;
        }
        // Fixed (non-vararg) parameter count. A vararg call supplies `>= n_fixed` arguments (the rest pack
        // into the trailing array); a non-vararg call must match exactly (after default-filling).
        let n_fixed = if vararg {
            sig_params.len() - 1
        } else {
            sig_params.len()
        };
        // Effective per-parameter arguments. For a non-vararg call, place named arguments at their declared
        // position and fill each omitted parameter with its default expression (an inline fn substitutes the
        // default directly — no `$default` method). A vararg call keeps the raw list (packed below).
        let eff_storage: Vec<AstExprId>;
        let has_named = self
            .afile
            .call_arg_names
            .get(&call_id)
            .is_some_and(|n| n.iter().any(|x| x.is_some()));
        let args: &[AstExprId] = if vararg {
            if args.len() < n_fixed {
                return None;
            }
            args
        } else if has_default || has_named {
            let names = self
                .afile
                .call_arg_names
                .get(&call_id)
                .cloned()
                .unwrap_or_default();
            let np = f.params.len();
            if args.len() > np {
                return None;
            }
            let mut slot: Vec<Option<AstExprId>> = vec![None; np];
            let mut pos = 0;
            for (i, &arg) in args.iter().enumerate() {
                match names.get(i).and_then(|o| o.as_ref()) {
                    None => {
                        if pos >= np {
                            return None;
                        }
                        slot[pos] = Some(arg);
                        pos += 1;
                    }
                    Some(nm) => {
                        let idx = f.params.iter().position(|p| &p.name == nm)?;
                        if slot[idx].is_some() {
                            return None;
                        }
                        slot[idx] = Some(arg);
                    }
                }
            }
            let mut eff = Vec::with_capacity(np);
            for (k, p) in f.params.iter().enumerate() {
                match slot[k] {
                    Some(a) => eff.push(a),
                    None => match p.default {
                        Some(d) => eff.push(d),
                        None => return None,
                    },
                }
            }
            eff_storage = eff;
            &eff_storage
        } else {
            if sig_params.len() != args.len() {
                return None;
            }
            args
        };
        // A genuinely recursive inline call re-enters the SAME call site, expanding without bound — bail
        // (skip). Source-level NESTING of the same fn (`a { a { 5 } }`) uses DISTINCT call sites, so it is
        // allowed. (kotlinc rejects only genuine recursion.)
        if self.inline_active.contains(&call_id) {
            return None;
        }
        // Specialize non-reified type params from the actual arguments: a parameter declared `T` binds
        // `T` to the call's concrete argument type, so the inlined body (and a lambda parameter typed `T`)
        // uses e.g. `Int` rather than the erased `Any`. Empty for a non-generic inline fn (no change).
        let tparams: std::collections::HashSet<&str> =
            f.type_params.iter().map(String::as_str).collect();
        let mut tbinds: std::collections::HashMap<String, Ty> = std::collections::HashMap::new();
        for (i, p) in f.params.iter().enumerate().take(n_fixed) {
            if tparams.contains(p.ty.name.as_str())
                && !matches!(self.afile.expr(args[i]), Expr::Lambda { .. })
            {
                tbinds
                    .entry(p.ty.name.clone())
                    .or_insert(self.info.ty(args[i]));
            }
        }
        // A generic-RECEIVER extension (`<T> T.foo`): bind the type parameter to the SPECIALIZED receiver
        // type, so a lambda parameter typed by it (`f: (T) -> …`) specializes too. Without this the lambda
        // param slot is typed the erased `Object` while the checker recorded the concrete type in its
        // frames — an inconsistent-frame `VerifyError` (`<T> T.alsoLog { … }` capturing a variable).
        if let (Some(rt), Some(recv_ref)) = (recv_ty, &f.receiver) {
            if tparams.contains(recv_ref.name.as_str()) {
                tbinds.entry(recv_ref.name.clone()).or_insert(rt);
            }
        }
        let active_depth = self.inline_active.len();
        self.inline_active.push(call_id);
        // Bind each reified type parameter to the call's explicit type argument (resolved through any
        // enclosing reified binding, so nested reified inlines compose). A missing arg (e.g. a purely
        // inferred reified type) bails — the file skips, never miscompiles.
        let reif_depth = self.reified_subst.len();
        if !f.reified_type_params.is_empty() {
            let targs = self.afile.call_type_args.get(&call_id);
            let mut map = std::collections::HashMap::new();
            for (i, tp) in f.type_params.iter().enumerate() {
                if f.reified_type_params.contains(tp) {
                    let actual = match targs.and_then(|ts| ts.get(i)) {
                        Some(a) => self.subst_type_ref(a),
                        None => {
                            self.inline_active.truncate(active_depth);
                            return None;
                        }
                    };
                    map.insert(tp.clone(), actual);
                }
            }
            self.reified_subst.push(map);
        }
        let depth = self.scope.len();
        let lam_depth = self.inline_lambdas.len();
        let mut stmts = Vec::new();
        // An extension fn binds its receiver as `this`: evaluate it once into a temp, visible as `this`
        // in the body (so `this`, `this.member`, and implicit-receiver member access all resolve to it).
        if let Some(rt) = &recv_ty {
            let recv_ast = match recv {
                Some(r) => r,
                None => {
                    self.scope.truncate(depth);
                    self.inline_active.truncate(active_depth);
                    self.reified_subst.truncate(reif_depth);
                    return None;
                }
            };
            let rt = *rt;
            let slot = self.fresh_value();
            let val = match self.lower_arg(recv_ast, &ty_to_ir(rt)) {
                Some(v) => v,
                None => {
                    self.scope.truncate(depth);
                    self.inline_active.truncate(active_depth);
                    self.reified_subst.truncate(reif_depth);
                    return None;
                }
            };
            stmts.push(self.ir.add_expr(IrExpr::Variable {
                index: slot,
                ty: ty_to_ir(rt),
                init: Some(val),
            }));
            self.scope.push(("this".to_string(), slot, rt));
        }
        for (i, pty) in sig_params.iter().enumerate() {
            // The trailing `vararg` parameter: pack the remaining arguments into a fresh array bound to it
            // (a `kotlin/Array`/`IntArray`/… local), exactly as the non-inline call site does. The inlined
            // body then iterates it normally.
            if vararg && i == n_fixed {
                let elem_ty = match pty.array_elem() {
                    Some(t) => t,
                    None => {
                        self.scope.truncate(depth);
                        self.inline_lambdas.truncate(lam_depth);
                        self.inline_active.truncate(active_depth);
                        self.reified_subst.truncate(reif_depth);
                        return None;
                    }
                };
                let elem_ir = ty_to_ir(elem_ty);
                let mut elements = Vec::new();
                for &arg in &args[n_fixed..] {
                    if is_branchy(self.afile, arg) {
                        self.scope.truncate(depth);
                        self.inline_lambdas.truncate(lam_depth);
                        self.inline_active.truncate(active_depth);
                        self.reified_subst.truncate(reif_depth);
                        return None;
                    }
                    match self.lower_arg(arg, &elem_ir) {
                        Some(v) => elements.push(v),
                        None => {
                            self.scope.truncate(depth);
                            self.inline_lambdas.truncate(lam_depth);
                            self.inline_active.truncate(active_depth);
                            self.reified_subst.truncate(reif_depth);
                            return None;
                        }
                    }
                }
                let arr = self.ir.add_expr(IrExpr::Vararg {
                    element_type: elem_ir,
                    elements,
                });
                let slot = self.fresh_value();
                let var = self.ir.add_expr(IrExpr::Variable {
                    index: slot,
                    ty: ty_to_ir(*pty),
                    init: Some(arr),
                });
                stmts.push(var);
                self.scope.push((pnames[i].clone(), slot, *pty));
                continue;
            }
            if let Ty::Fun(fnsig) = pty {
                // A lambda parameter is inline-SPLICED (its body expanded at each invoke site) only when it
                // is a literal lambda AND used solely as a callee (`f(args)`) in the body. If the parameter
                // is also used as a VALUE — passed to another call (`a(f)`), stored, or returned — it must
                // be materialized as a `FunctionN` instead (the value-binding branch below); a callable-ref
                // argument is never a body to splice.
                let arg_expr = self.afile.expr(args[i]).clone();
                let splice = matches!(arg_expr, Expr::Lambda { .. })
                    && !name_used_as_value(self.afile, body, &pnames[i]);
                if let (
                    true,
                    Expr::Lambda {
                        params,
                        body: lbody,
                    },
                ) = (splice, arg_expr)
                {
                    // A single-parameter lambda may name its parameter implicitly as `it`.
                    let params = if params.is_empty() && fnsig.params.len() == 1 {
                        vec!["it".to_string()]
                    } else {
                        params
                    };
                    // A bare `return` (non-local) or a `return@other` in the lambda body isn't modeled —
                    // bail. A `return@<thisInlineFn>` IS modeled (a local return from the spliced lambda,
                    // handled by the `inline_lambda_ret` frame set up at the invoke site), so it's allowed.
                    if body_has_disallowed_return(self.afile, lbody, fname)
                        || params.len() != fnsig.params.len()
                    {
                        self.scope.truncate(depth);
                        self.inline_lambdas.truncate(lam_depth);
                        self.inline_active.truncate(active_depth);
                        self.reified_subst.truncate(reif_depth);
                        return None;
                    }
                    // The lambda's parameter types, with the inline fn's type params specialized
                    // (`(T)->T` on a `twice(1){…}` call → `it: Int`, not the erased `Any` of `fnsig`).
                    let lam_param_tys: Vec<Ty> = f.params[i]
                        .ty
                        .fun_params
                        .iter()
                        .map(|fp| {
                            tbinds
                                .get(fp.name.as_str())
                                .copied()
                                .unwrap_or_else(|| ty_of(self.afile, fp))
                        })
                        .collect();
                    let lam_param_tys = if lam_param_tys.len() == fnsig.params.len() {
                        lam_param_tys
                    } else {
                        fnsig.params.clone()
                    };
                    self.inline_lambdas.push((
                        pnames[i].clone(),
                        params,
                        lbody,
                        lam_param_tys,
                        fname.to_string(),
                    ));
                } else {
                    // A function-typed argument that is NOT a lambda literal — a callable reference
                    // (`::g`, `obj::m`), or a function-valued variable. It can't be inline-expanded as a
                    // body, so bind it as a function VALUE (a `FunctionN`): `f(v)` in the inlined body then
                    // invokes it via `.invoke`. Semantically identical (kotlinc inlines the reference too;
                    // the value form is box-OK and verifies) — no FunctionN-drop bookkeeping needed.
                    let slot = self.fresh_value();
                    let val = match self.lower_arg(args[i], &ty_to_ir(*pty)) {
                        Some(v) => v,
                        None => {
                            self.scope.truncate(depth);
                            self.inline_lambdas.truncate(lam_depth);
                            self.inline_active.truncate(active_depth);
                            self.reified_subst.truncate(reif_depth);
                            return None;
                        }
                    };
                    let var = self.ir.add_expr(IrExpr::Variable {
                        index: slot,
                        ty: ty_to_ir(*pty),
                        init: Some(val),
                    });
                    stmts.push(var);
                    self.scope.push((pnames[i].clone(), slot, *pty));
                }
            } else {
                // A value parameter: evaluate once into a temp, visible by name in the body. A parameter
                // declared as a type param uses the call's concrete argument type (specialized), not the
                // erased `Any` — so the inlined body sees `Int` and avoids spurious boxing.
                let spty = tbinds
                    .get(f.params[i].ty.name.as_str())
                    .copied()
                    .unwrap_or(*pty);
                let slot = self.fresh_value();
                let val = match self.lower_arg(args[i], &ty_to_ir(spty)) {
                    Some(v) => v,
                    None => {
                        self.scope.truncate(depth);
                        self.inline_lambdas.truncate(lam_depth);
                        self.inline_active.truncate(active_depth);
                        self.reified_subst.truncate(reif_depth);
                        return None;
                    }
                };
                let var = self.ir.add_expr(IrExpr::Variable {
                    index: slot,
                    ty: ty_to_ir(spty),
                    init: Some(val),
                });
                stmts.push(var);
                self.scope.push((pnames[i].clone(), slot, spty));
            }
        }
        // For a body with `return`s, set up the inline-return target (result slot + end label) so a
        // `return x` lowers to `result = x; break@end`; the body is then wrapped in `do { … } while(false)`
        // labeled `end`. A `Unit` return type needs no result slot (a `return` is a bare `break`).
        let ret_ty = ty_to_ir(sig_ret);
        let target = if has_return {
            let slot = self.fresh_value();
            let label = format!("$inl${slot}");
            self.inline_return
                .push((slot, label.clone(), ret_ty.clone()));
            Some((slot, label))
        } else {
            None
        };
        let body_val = self.expr(body);
        self.scope.truncate(depth);
        self.inline_lambdas.truncate(lam_depth);
        self.inline_active.truncate(active_depth);
        self.reified_subst.truncate(reif_depth);
        if has_return {
            self.inline_return.pop();
        }
        let body_val = body_val?;
        if let Some((slot, label)) = target {
            let unit_ret = ret_ty == Ty::Unit;
            // `while(true) { <body>; [result = fall-through value;] break@end }` — runs the inlined body
            // exactly once (the trailing `break` exits), while any `return` inside breaks early (after
            // setting the result). A standard `while(true)`+`break` shape the frame emitter handles.
            // If the body ALWAYS diverges (every path returns — type `Nothing`), the fall-through assign +
            // break are unreachable dead code AFTER a `goto`, which would lack a stackmap frame — so omit
            // them and emit the body alone (its internal `return`s already break to the end).
            let loop_body = if body_diverges {
                body_val
            } else {
                let brk = self.ir.add_expr(IrExpr::Break {
                    label: Some(label.clone()),
                });
                if unit_ret {
                    self.ir.add_expr(IrExpr::Block {
                        stmts: vec![body_val, brk],
                        value: None,
                    })
                } else {
                    let assign = self.ir.add_expr(IrExpr::SetValue {
                        var: slot,
                        value: body_val,
                    });
                    self.ir.add_expr(IrExpr::Block {
                        stmts: vec![assign, brk],
                        value: None,
                    })
                }
            };
            let cond = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
            let loopw = self.ir.add_expr(IrExpr::While {
                cond,
                body: loop_body,
                update: None,
                post_test: false,
                label: Some(label),
            });
            if !unit_ret {
                // Initialize the result slot to a type default so its frame type is consistent at the
                // loop head (an uninitialized slot is `top` there but the body assigns it → mismatch).
                let init = if ir_type_is_reference(&ret_ty) {
                    IrConst::Null
                } else if let Some(fq_name) = ret_ty.non_null().obj_internal() {
                    match fq_name {
                        "kotlin/Long" => IrConst::Long(0),
                        "kotlin/Float" => IrConst::Float(0.0),
                        "kotlin/Double" => IrConst::Double(0.0),
                        "kotlin/Boolean" => IrConst::Boolean(false),
                        "kotlin/Char" => IrConst::Char('\0'),
                        _ => IrConst::Int(0), // Int/Short/Byte
                    }
                } else {
                    IrConst::Null
                };
                let init = self.ir.add_expr(IrExpr::Const(init));
                stmts.push(self.ir.add_expr(IrExpr::Variable {
                    index: slot,
                    ty: ret_ty,
                    init: Some(init),
                }));
            }
            stmts.push(loopw);
            let value = if unit_ret {
                self.ir.add_expr(IrExpr::UnitInstance)
            } else {
                self.ir.add_expr(IrExpr::GetValue(slot))
            };
            Some(self.ir.add_expr(IrExpr::Block {
                stmts,
                value: Some(value),
            }))
        } else if stmts.is_empty() {
            Some(body_val)
        } else {
            Some(self.ir.add_expr(IrExpr::Block {
                stmts,
                value: Some(body_val),
            }))
        }
    }

    /// Expand a call `param(args)` to an inlined lambda parameter: bind the lambda's parameters to the
    /// (evaluated) arguments, then lower its body in place. The body's value is the call's value.
    fn lower_inline_lambda_invoke(&mut self, idx: usize, args: &[AstExprId]) -> Option<u32> {
        let (_, lam_params, lam_body, lam_param_tys, lam_label) = self.inline_lambdas[idx].clone();
        if args.len() != lam_params.len() || lam_params.len() != lam_param_tys.len() {
            return None;
        }
        let depth = self.scope.len();
        let mut stmts = Vec::new();
        for ((pname, pty), &arg) in lam_params.iter().zip(&lam_param_tys).zip(args) {
            let slot = self.fresh_value();
            let val = match self.lower_arg(arg, &ty_to_ir(*pty)) {
                Some(v) => v,
                None => {
                    self.scope.truncate(depth);
                    return None;
                }
            };
            let var = self.ir.add_expr(IrExpr::Variable {
                index: slot,
                ty: ty_to_ir(*pty),
                init: Some(val),
            });
            stmts.push(var);
            self.scope.push((pname.clone(), slot, *pty));
        }
        // A `return@<inlineFn>` in the lambda body is a LOCAL return from this lambda invocation. Wrap the
        // spliced body in a `while(true){ … break }` labeled `brk` and register a frame so the labeled
        // return lowers to `break@brk` — exactly the inline-fn return mechanism, one level in. Only a
        // `Unit`-result lambda (the `forEach`/`onEach` shape) is modeled; a value-result labeled return
        // (a `let { return@let v }`) is not yet — bail rather than miscompile.
        if body_has_labeled_return(self.afile, lam_body, &lam_label) {
            let lam_ret = self.info.ty(lam_body);
            if lam_ret != Ty::Unit && lam_ret != Ty::Nothing {
                self.scope.truncate(depth);
                return None;
            }
            let brk = format!("$lamret${}", self.fresh_value());
            self.inline_lambda_ret
                .push((lam_label.clone(), 0, brk.clone(), Ty::Unit));
            let body_val = self.expr(lam_body);
            self.inline_lambda_ret.pop();
            self.scope.truncate(depth);
            let body_val = body_val?;
            let brk_stmt = self.ir.add_expr(IrExpr::Break {
                label: Some(brk.clone()),
            });
            let loop_body = self.ir.add_expr(IrExpr::Block {
                stmts: vec![body_val, brk_stmt],
                value: None,
            });
            let cond = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
            let loopw = self.ir.add_expr(IrExpr::While {
                cond,
                body: loop_body,
                update: None,
                post_test: false,
                label: Some(brk),
            });
            stmts.push(loopw);
            let unit = self.ir.add_expr(IrExpr::UnitInstance);
            return Some(self.ir.add_expr(IrExpr::Block {
                stmts,
                value: Some(unit),
            }));
        }
        // A BARE `return` in the spliced lambda body is NON-LOCAL: it returns from the function enclosing
        // the lambda literal, NOT from the inline fn whose call is being expanded. Clear the inline-return
        // stack while lowering the body so such a `return` lowers to the real enclosing-function return
        // (`cur_ret_ty`), not the inline fn's result-slot break. Restored after (the inline fn's own body,
        // lowered elsewhere, keeps its frame).
        let saved_inl_ret = std::mem::take(&mut self.inline_return);
        let body_val = self.expr(lam_body);
        self.inline_return = saved_inl_ret;
        self.scope.truncate(depth);
        let body_val = body_val?;
        if stmts.is_empty() {
            Some(body_val)
        } else {
            Some(self.ir.add_expr(IrExpr::Block {
                stmts,
                value: Some(body_val),
            }))
        }
    }

    /// Build the pieces of a safe property/length access `recv?.<name>` (NO call args) for fusion with a
    /// surrounding `?:` over a PRIMITIVE result: returns `(var, cond, member)` where `var` binds the
    /// receiver once, `cond` is `recv != null`, and `member` is the UNBOXED member value read on the
    /// non-null receiver. Lets `s?.length ?: -1` compile to a `dup`/`ifnull` primitive select with no
    /// boxing (kotlinc's form) instead of boxing the member then unboxing it through the elvis.
    fn lower_safe_prop_member(
        &mut self,
        receiver: AstExprId,
        name: &str,
    ) -> Option<(u32, u32, u32)> {
        let rty = self.info.ty(receiver);
        if !rty.is_reference() {
            return None;
        }
        let internal = if rty == Ty::String {
            "java/lang/String".to_string()
        } else {
            rty.obj_internal()?.to_string()
        };
        let rv = self.expr(receiver)?;
        let v = self.fresh_value();
        let var = self.ir.add_expr(IrExpr::Variable {
            index: v,
            ty: mark_nullable(ty_to_ir(rty)),
            init: Some(rv),
        });
        let get1 = self.ir.add_expr(IrExpr::GetValue(v));
        let nullc = self.ir.add_expr(IrExpr::Const(IrConst::Null));
        let cond = self.ir.add_expr(IrExpr::PrimitiveBinOp {
            op: IrBinOp::Ne,
            lhs: get1,
            rhs: nullc,
        });
        let recv2 = self.ir.add_expr(IrExpr::GetValue(v));
        let is_iface = self
            .syms
            .libraries
            .resolve_type(&internal)
            .is_some_and(|t| t.is_interface());
        let member = if let Some((fclass, idx, _)) = self.resolve_field(&internal, name) {
            let owner_internal = self.ir.classes[fclass as usize].fq_name.clone();
            if self.cur_class.as_deref() != Some(owner_internal.as_str()) {
                if let Some((mclass, mindex, _, _)) =
                    self.resolve_method(&internal, &getter_name(name))
                {
                    self.ir.add_expr(IrExpr::MethodCall {
                        class: mclass,
                        index: mindex,
                        receiver: recv2,
                        args: vec![],
                    })
                } else {
                    self.ir.add_expr(IrExpr::GetField {
                        receiver: recv2,
                        class: fclass,
                        index: idx,
                    })
                }
            } else {
                self.ir.add_expr(IrExpr::GetField {
                    receiver: recv2,
                    class: fclass,
                    index: idx,
                })
            }
        } else if internal == "java/lang/String" && name == "length" {
            self.ir.add_expr(IrExpr::Call {
                callee: Callee::External("kotlin/String.length".to_string()),
                dispatch_receiver: Some(recv2),
                args: vec![],
            })
        } else {
            let mapped = crate::resolve::collection_mapped_accessor(name).map(|s| s.to_string());
            let m = [Some(name.to_string()), Some(getter_name(name)), mapped]
                .into_iter()
                .flatten()
                .find_map(|c| {
                    crate::call_resolver::resolve_instance(
                        &*self.syms.libraries,
                        &internal,
                        &c,
                        &[],
                    )
                    .filter(|m| !matches!(m.ret, Ty::Unit | Ty::Error))
                })?;
            self.ir.add_expr(IrExpr::Call {
                callee: Callee::Virtual {
                    owner: internal.clone(),
                    name: m.name,
                    descriptor: m.descriptor,
                    interface: is_iface,
                },
                dispatch_receiver: Some(recv2),
                args: vec![],
            })
        };
        Some((var, cond, member))
    }

    fn expr(&mut self, e: AstExprId) -> Option<u32> {
        // Guard against a stack overflow on a pathologically deep expression (a stress test with
        // thousands of nested operators): bail past the limit so the file is skipped, not crashed.
        self.expr_depth += 1;
        if self.expr_depth > 500 {
            self.expr_depth -= 1;
            return None;
        }
        let r = self.expr_inner(e);
        self.expr_depth -= 1;
        // Survey diagnostic: tag the innermost unsupported expression that caused the bail (the first
        // `None` to bubble up wins, since deeper frames run first and a tag is only refined from a
        // coarse `deep*` phase). A `Call` is tagged by its callee name — the single most useful signal.
        if r.is_none() && lower_bail_reason().starts_with("deep") {
            let reason = match self.afile.expr(e).clone() {
                Expr::Call { callee, .. } => {
                    let name = match self.afile.expr(callee) {
                        Expr::Name(n) => n.clone(),
                        Expr::Member { name, .. } => format!(".{name}"),
                        o => bail_variant(&format!("{o:?}")).to_string(),
                    };
                    format!("call {name}")
                }
                o => format!("expr {}", bail_variant(&format!("{o:?}"))),
            };
            set_bail(&reason);
        }
        r
    }

    fn expr_inner(&mut self, e: AstExprId) -> Option<u32> {
        Some(match self.afile.expr(e).clone() {
            Expr::IntLit(v) => self.ir.add_expr(IrExpr::Const(IrConst::Int(v as i32))),
            Expr::LongLit(v) => self.ir.add_expr(IrExpr::Const(IrConst::Long(v))),
            // Unsigned literals are the signed int/long bit pattern of their magnitude (`UInt.MAX` =
            // 0xFFFFFFFFu reinterprets to int -1, which is what kotlinc stores).
            Expr::UIntLit(v) => self
                .ir
                .add_expr(IrExpr::Const(IrConst::Int(v as u32 as i32))),
            Expr::ULongLit(v) => self.ir.add_expr(IrExpr::Const(IrConst::Long(v))),
            Expr::DoubleLit(v) => self.ir.add_expr(IrExpr::Const(IrConst::Double(v))),
            Expr::FloatLit(v) => self.ir.add_expr(IrExpr::Const(IrConst::Float(v))),
            Expr::CharLit(c) => self.ir.add_expr(IrExpr::Const(IrConst::Char(c))),
            Expr::BoolLit(b) => self.ir.add_expr(IrExpr::Const(IrConst::Boolean(b))),
            Expr::StringLit(s) => self.ir.add_expr(IrExpr::Const(IrConst::String(s))),
            Expr::NullLit => self.ir.add_expr(IrExpr::Const(IrConst::Null)),
            // `throw e` — throw the exception value; control never returns.
            Expr::Throw { operand } => {
                let v = self.expr(operand)?;
                // Throwing an erased generic value (`throw id(e)`, where `id` returns a type parameter
                // and is physically `Object`) needs the `checkcast Throwable` kotlinc inserts — the JVM
                // `athrow` requires a `Throwable` on the stack.
                let v = if self.info.ty(operand) == Ty::obj("kotlin/Any") {
                    self.ir.add_expr(IrExpr::TypeOp {
                        op: IrTypeOp::Cast,
                        arg: v,
                        type_operand: ty_to_ir(Ty::obj("kotlin/Throwable")),
                    })
                } else {
                    v
                };
                self.ir.add_expr(IrExpr::Throw { operand: v })
            }
            // `return value` in expression position (`x ?: return null`). Only the simple function-return
            // case is modeled here; an enclosing `finally`, an `inline fun` expansion frame, or a label
            // naming a spliced lambda needs the richer `Stmt::Return` handling — skip the file instead.
            Expr::Return { value, label } => {
                if !self.try_finally_stack.is_empty() || !self.inline_return.is_empty() {
                    return None;
                }
                if let Some(lbl) = &label {
                    if self.inline_lambda_ret.iter().any(|(l, ..)| l == lbl) {
                        return None;
                    }
                }
                let v = match value {
                    Some(ve) if self.cur_ret_ty != Ty::Unit && self.info.ty(ve) != Ty::Nothing => {
                        let rt = self.cur_ret_ty.clone();
                        Some(self.lower_arg(ve, &rt)?)
                    }
                    Some(ve) => Some(self.expr(ve)?),
                    None => None,
                };
                self.ir.add_expr(IrExpr::Return(v))
            }
            // `try { … } catch (e: E) { … } … [finally { f }]` (nested try already rejected by checker).
            Expr::Try {
                body,
                catches,
                finally,
            } => {
                // A `finally` is inlined at each exit. A `break`/`continue` that escapes the `try` would
                // need the `finally` run before it (not modeled) — bail. A `return` IS modeled: the
                // `finally` is pushed onto `try_finally_stack` and inlined at each `return` inside the
                // body/catch (`Stmt::Return`); the normal/exception exits are inlined by `emit_try`.
                if finally.is_some()
                    && (body_has_break_continue(self.afile, body)
                        || catches
                            .iter()
                            .any(|c| body_has_break_continue(self.afile, c.body)))
                {
                    return None;
                }
                // A `finally` that declares locals is inlined on several exit paths (normal, each `return`,
                // the exception catch-all); the duplicated locals' slots clash across copies (a verify
                // error) — skip rather than miscompile.
                if let Some(f) = finally {
                    if body_declares_local(self.afile, f) {
                        return None;
                    }
                }
                let result = ty_to_ir(self.info.ty(e));
                if let Some(f) = finally {
                    self.try_finally_stack.push(f);
                }
                let body_ir = self.expr(body);
                let mut ir_catches = Vec::new();
                let mut ok = body_ir.is_some();
                for c in &catches {
                    let exc_internal = match self.catch_internal(&c.ty.name) {
                        Some(x) => x,
                        None => {
                            ok = false;
                            break;
                        }
                    };
                    let v = self.fresh_value();
                    self.scope.push((c.name.clone(), v, Ty::obj(&exc_internal)));
                    let cbody = self.expr(c.body);
                    self.scope.pop();
                    match cbody {
                        Some(cb) => ir_catches.push(crate::ir::IrCatch {
                            var: v,
                            exc_internal,
                            body: cb,
                        }),
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                if finally.is_some() {
                    self.try_finally_stack.pop();
                }
                if !ok {
                    return None;
                }
                let fin = match finally {
                    Some(f) => Some(self.expr(f)?),
                    None => None,
                };
                self.ir.add_expr(IrExpr::Try {
                    body: body_ir?,
                    catches: ir_catches,
                    finally: fin,
                    result,
                })
            }
            // `operand!!` — assert non-null. On a reference, `Intrinsics.checkNotNull` throws if null
            // and yields the value; on a (non-null) primitive it is a no-op.
            Expr::NotNull { operand } => {
                let v = self.expr(operand)?;
                if self.info.ty(operand).is_reference() {
                    let asserted = self.ir.add_expr(IrExpr::NotNullAssert { operand: v });
                    // `Int?!!` narrows to the unboxed primitive — unbox the wrapper after the null check.
                    let result = self.info.ty(e);
                    if result.is_primitive() {
                        self.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg: asserted,
                            type_operand: ty_to_ir(result),
                        })
                    } else {
                        asserted
                    }
                } else {
                    v
                }
            }
            // `r?.m(args)` / `r?.p` → `{ val t = r; if (t != null) t.m(args)/t.p else null }`.
            Expr::SafeCall {
                receiver,
                name,
                args,
            } => {
                let rty = self.info.ty(receiver);
                let result_ty = self.info.ty(e);
                // A primitive receiver can never be null, so `a?.foo(b)` is a vacuous safe call (kotlinc
                // warns "unnecessary safe call") ≡ `a.foo(b)`. Fold an arithmetic operator-method call to
                // the plain primitive op — `var a = 10; a?.plus(10)` works like `a.plus(10)`.
                if !rty.is_reference() {
                    if let Some(args) = &args {
                        if args.len() == 1 {
                            if let Some(r) = self.lower_prim_op_method(receiver, &name, args[0]) {
                                return Some(r);
                            }
                        }
                    }
                }
                // Only reference receiver + reference result are modeled (a nullable-primitive result
                // would need boxing the member value, which the checker rejects anyway).
                if !rty.is_reference() || !result_ty.is_reference() {
                    return None;
                }
                // The receiver internal comes from the NON-null form (`Int?` in a chained `?.let` unwraps
                // to `Int`). A nullable-primitive receiver has no class internal — the scope-fn path
                // (tried first) unboxes it itself, so a placeholder owner suffices for the null-check.
                let nn = rty.non_null();
                let internal = if nn == Ty::String {
                    "java/lang/String".to_string()
                } else if let Some(i) = nn.obj_internal() {
                    i.to_string()
                } else if nn.is_primitive() {
                    // A nullable-PRIMITIVE receiver (`Int?` in a chained `?.let`) has no class internal —
                    // the scope-fn path (tried first) unboxes it; a placeholder owner suffices.
                    "java/lang/Object".to_string()
                } else {
                    // A non-object, non-primitive receiver (e.g. a literal `null?.…`): unsupported — bail.
                    return None;
                };
                let rv = self.expr(receiver)?;
                let v = self.fresh_value();
                // The `?.` receiver is NULLABLE by construction — carry that into the temp's IrType (`Ty`
                // drops it). For a value-class receiver this keeps it BOXED (`MyC?` → `LMyC;`), so storing
                // the boxed `as?`/nullable value doesn't mismatch an unboxed-`int` slot.
                let var = self.ir.add_expr(IrExpr::Variable {
                    index: v,
                    ty: mark_nullable(ty_to_ir(rty)),
                    init: Some(rv),
                });
                let get1 = self.ir.add_expr(IrExpr::GetValue(v));
                let nullc = self.ir.add_expr(IrExpr::Const(IrConst::Null));
                let cond = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: IrBinOp::Ne,
                    lhs: get1,
                    rhs: nullc,
                });
                let recv2 = self.ir.add_expr(IrExpr::GetValue(v));
                let is_iface = self
                    .syms
                    .libraries
                    .resolve_type(&internal)
                    .map_or(false, |t| t.is_interface());
                // A safe-call scope function (`s?.let { it… }`, `s?.run { … }`): inline it with the
                // non-null receiver `recv2`; the surrounding null-check + nullable-wrap below make the
                // whole `s?.…` yield `null` when `s` is null.
                let member = if let Some(m) = self.lower_safe_scope_member(recv2, rty, &name, &args)
                {
                    m
                } else {
                    match args {
                        Some(args) => {
                            if let Some((class, index, fid, _)) =
                                self.resolve_method(&internal, &name)
                            {
                                let params = self.ir.functions[fid as usize].params.clone();
                                if args.len() != params.len() {
                                    return None;
                                }
                                let mut a = Vec::new();
                                for (arg, pt) in args.iter().zip(&params) {
                                    a.push(self.lower_arg(*arg, pt)?);
                                }
                                self.ir.add_expr(IrExpr::MethodCall {
                                    class,
                                    index,
                                    receiver: recv2,
                                    args: a.into_iter().map(Some).collect(),
                                })
                            } else {
                                // A classpath instance method (`s?.substring(1)`).
                                let arg_tys: Vec<Ty> =
                                    args.iter().map(|&a| self.info.ty(a)).collect();
                                if let Some(m) = crate::call_resolver::resolve_instance(
                                    &*self.syms.libraries,
                                    &internal,
                                    &name,
                                    &arg_tys,
                                ) {
                                    let mut a = Vec::new();
                                    for (arg, pt) in args.iter().zip(&m.params) {
                                        a.push(self.lower_arg(*arg, &ty_to_ir(*pt))?);
                                    }
                                    self.ir.add_expr(IrExpr::Call {
                                        callee: Callee::Virtual {
                                            owner: internal.clone(),
                                            name: m.name,
                                            descriptor: m.descriptor,
                                            interface: is_iface,
                                        },
                                        dispatch_receiver: Some(recv2),
                                        args: a,
                                    })
                                } else {
                                    // A stdlib EXTENSION via safe call (`s?.uppercase()`) — inline it on
                                    // the non-null receiver, the same path as the qualified call.
                                    self.lower_ext_call_on(recv2, rty, &name, &args, e)?
                                }
                            }
                        }
                        None => {
                            if let Some((fclass, idx, _)) = self.resolve_field(&internal, &name) {
                                let owner_internal =
                                    self.ir.classes[fclass as usize].fq_name.clone();
                                // External read → `getX()` (the backing field is private); internal → field.
                                if self.cur_class.as_deref() != Some(owner_internal.as_str()) {
                                    if let Some((mclass, mindex, _, _)) =
                                        self.resolve_method(&internal, &getter_name(&name))
                                    {
                                        self.ir.add_expr(IrExpr::MethodCall {
                                            class: mclass,
                                            index: mindex,
                                            receiver: recv2,
                                            args: vec![],
                                        })
                                    } else {
                                        self.ir.add_expr(IrExpr::GetField {
                                            receiver: recv2,
                                            class: fclass,
                                            index: idx,
                                        })
                                    }
                                } else {
                                    self.ir.add_expr(IrExpr::GetField {
                                        receiver: recv2,
                                        class: fclass,
                                        index: idx,
                                    })
                                }
                            } else if internal == "java/lang/String" && name == "length" {
                                self.ir.add_expr(IrExpr::Call {
                                    callee: Callee::External("kotlin/String.length".to_string()),
                                    dispatch_receiver: Some(recv2),
                                    args: vec![],
                                })
                            } else {
                                // A classpath property (`list?.size`) — a zero-arg accessor.
                                let mapped = crate::resolve::collection_mapped_accessor(&name)
                                    .map(|s| s.to_string());
                                let m = [Some(name.clone()), Some(getter_name(&name)), mapped]
                                    .into_iter()
                                    .flatten()
                                    .find_map(|c| {
                                        crate::call_resolver::resolve_instance(
                                            &*self.syms.libraries,
                                            &internal,
                                            &c,
                                            &[],
                                        )
                                        .filter(|m| !matches!(m.ret, Ty::Unit | Ty::Error))
                                    })?;
                                self.ir.add_expr(IrExpr::Call {
                                    callee: Callee::Virtual {
                                        owner: internal.clone(),
                                        name: m.name,
                                        descriptor: m.descriptor,
                                        interface: is_iface,
                                    },
                                    dispatch_receiver: Some(recv2),
                                    args: vec![],
                                })
                            }
                        }
                    }
                };
                // A nullable-primitive result (`s?.length` : `Int?`): box the primitive member value so
                // both `when` branches are the wrapper reference (the other branch is `null`).
                let member = if result_ty.nullable_primitive().is_some() {
                    self.ir.add_expr(IrExpr::TypeOp {
                        op: IrTypeOp::ImplicitCoercion,
                        arg: member,
                        type_operand: ty_to_ir(result_ty),
                    })
                } else if result_ty.obj_internal().is_some_and(|i| {
                    self.syms
                        .classes
                        .get(i.rsplit('/').next().unwrap_or(i))
                        .is_some_and(|c| c.value_field.is_some())
                }) {
                    // A nullable VALUE-CLASS result (`a?.foo()` : `Z?`): the member returns the unboxed
                    // underlying, but the `when` merges it with a `null` branch — coerce to the NULLABLE
                    // value class so the JVM value-class pass `box-impl`s it (both branches then references).
                    self.ir.add_expr(IrExpr::TypeOp {
                        op: IrTypeOp::ImplicitCoercion,
                        arg: member,
                        type_operand: mark_nullable(ty_to_ir(result_ty)),
                    })
                } else {
                    member
                };
                let nullb = self.ir.add_expr(IrExpr::Const(IrConst::Null));
                let when = self.ir.add_expr(IrExpr::When {
                    branches: vec![(Some(cond), member), (None, nullb)],
                });
                self.ir.add_expr(IrExpr::Block {
                    stmts: vec![var],
                    value: Some(when),
                })
            }
            // `a ?: b` → `{ val t = a; if (t != null) t else b }` (t bound once, so `a` runs once).
            Expr::Elvis { lhs, rhs } => {
                let result_ty = self.info.ty(e);
                // Fuse `recv?.prop ?: default` for a PRIMITIVE result: null-check the receiver and select
                // the UNBOXED member or the default — no boxing of the member (kotlinc's `dup;ifnull`
                // form). Only the no-arg property/length safe-access is fused here.
                if result_ty.is_primitive() {
                    if let Expr::SafeCall {
                        receiver,
                        name,
                        args: None,
                    } = self.afile.expr(lhs).clone()
                    {
                        if let Some((var, cond, member)) =
                            self.lower_safe_prop_member(receiver, &name)
                        {
                            // The member is the natural primitive; convert to the elvis result type if it
                            // differs (`s?.length ?: 0L` → `i2l`).
                            let member = self.ir.add_expr(IrExpr::TypeOp {
                                op: IrTypeOp::ImplicitCoercion,
                                arg: member,
                                type_operand: ty_to_ir(result_ty),
                            });
                            let rv = self.lower_arg(rhs, &ty_to_ir(result_ty))?;
                            let when = self.ir.add_expr(IrExpr::When {
                                branches: vec![(Some(cond), member), (None, rv)],
                            });
                            return Some(self.ir.add_expr(IrExpr::Block {
                                stmts: vec![var],
                                value: Some(when),
                            }));
                        }
                    }
                }
                let lty = self.info.ty(lhs);
                // A trivial elvis kotlinc folds at compile time (it warns "left/right operand is never
                // null"/"is always null"): a non-reference (primitive) lhs is never null, so `x ?: d` == `x`
                // (the rhs is dead — drop it, but still emit `x` for its side effects); a statically-`null`
                // lhs makes the elvis always take the rhs, so `null ?: d` == `d`.
                if lty == Ty::Null {
                    return self.lower_arg(rhs, &ty_to_ir(result_ty));
                }
                if !lty.is_reference() {
                    return self.lower_arg(lhs, &ty_to_ir(result_ty));
                }
                let lv = self.expr(lhs)?;
                let v = self.fresh_value();
                let var = self.ir.add_expr(IrExpr::Variable {
                    index: v,
                    ty: ty_to_ir(lty),
                    init: Some(lv),
                });
                let get1 = self.ir.add_expr(IrExpr::GetValue(v));
                let nullc = self.ir.add_expr(IrExpr::Const(IrConst::Null));
                let cond = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op: IrBinOp::Ne,
                    lhs: get1,
                    rhs: nullc,
                });
                // When the elvis result is a primitive (a nullable-primitive lhs, `Int? ?: 0`), the
                // non-null lhs unboxes to the primitive and the rhs coerces to it too.
                let result_ty = self.info.ty(e);
                let mut get2 = self.ir.add_expr(IrExpr::GetValue(v));
                if result_ty.is_primitive() && lty.is_reference() {
                    // Unbox to the wrapper's OWN primitive (`Integer`→`Int`), then numeric-convert to the
                    // result if it differs (`Int? ?: 0.0` → unbox to `Int`, then `i2d` to `Double`) —
                    // unboxing `Integer` straight to `Double` would be an invalid checkcast.
                    if let Some(lp) = lty.nullable_primitive() {
                        get2 = self.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg: get2,
                            type_operand: ty_to_ir(lp),
                        });
                        if lp != result_ty {
                            get2 = self.ir.add_expr(IrExpr::TypeOp {
                                op: IrTypeOp::ImplicitCoercion,
                                arg: get2,
                                type_operand: ty_to_ir(result_ty),
                            });
                        }
                    }
                }
                let rv = self.lower_arg(rhs, &ty_to_ir(result_ty))?;
                let when = self.ir.add_expr(IrExpr::When {
                    branches: vec![(Some(cond), get2), (None, rv)],
                });
                self.ir.add_expr(IrExpr::Block {
                    stmts: vec![var],
                    value: Some(when),
                })
            }
            // A block in expression position: `{ stmt; …; trailing }`; value is the trailing expr.
            Expr::Block { stmts, trailing } => {
                let depth = self.scope.len();
                let mut out = Vec::new();
                let mut diverged = false;
                for &s in &stmts {
                    if self.append_stmt(s, &mut out).is_none() {
                        self.scope.truncate(depth);
                        return None;
                    }
                    if self.stmt_diverges(s) {
                        diverged = true;
                        break;
                    }
                }
                let value = match trailing {
                    Some(t) if !diverged => match self.expr(t) {
                        Some(v) => Some(v),
                        None => {
                            self.scope.truncate(depth);
                            return None;
                        }
                    },
                    _ => None,
                };
                self.scope.truncate(depth);
                self.ir.add_expr(IrExpr::Block { stmts: out, value })
            }
            Expr::Lambda { params, body } => return self.lower_lambda(e, &params, body),
            // Unbound top-level function reference `::foo` → the same `invokedynamic` +
            // `LambdaMetafactory` machinery as a lambda, but the impl method handle points directly at
            // the referenced function (no synthesized body). Bound/object/constructor references bail.
            Expr::CallableRef { receiver, name } => {
                // A property reference (`Type::prop`) is typed as a `KProperty*`, not a function type;
                // handle it before the `Fun` guard below.
                if let Some(recv) = receiver {
                    if let Some(pr) = self.lower_prop_ref(e, recv, &name) {
                        return Some(pr);
                    }
                }
                let Ty::Fun(sig) = self.info.ty(e) else {
                    return None;
                };
                let arity = sig.params.len();
                if let Some(recv) = receiver {
                    // `lower_method_ref` handles Name receivers (bound local/object, unbound class). An
                    // arbitrary expression receiver (`"abc"::get`, `1::foo`) or a bound extension falls
                    // through to `lower_bound_expr_ref`, which evaluates+captures the receiver once.
                    if let Some(r) = self.lower_method_ref(e, recv, &name, &sig.params, sig.ret) {
                        return Some(r);
                    }
                    return self.lower_bound_expr_ref(e, recv, &name, &sig.params, sig.ret);
                }
                // Local function reference `::localFun` (the checker mapped this ref to its decl): a
                // closure over the lifted static method, capturing the same outer locals the method
                // takes as leading params. A `Unit` SAM-return is wrapped (`invoke` yields the `Unit`
                // singleton); `Nothing` stays unmodeled.
                if let Some(&stmt_id) = self.info.local_call_map.get(&e) {
                    if let Some(&fid) = self.local_fun_ids.get(&stmt_id) {
                        if sig.ret == Ty::Nothing {
                            return None;
                        }
                        let caps = self
                            .info
                            .local_fun_captures
                            .get(&stmt_id)
                            .cloned()
                            .unwrap_or_default();
                        let captures: Vec<u32> = caps
                            .iter()
                            .map(|(n, _)| {
                                self.lookup(n)
                                    .map(|(cv, _)| self.ir.add_expr(IrExpr::GetValue(cv)))
                            })
                            .collect::<Option<Vec<_>>>()?;
                        let impl_fn = if sig.ret == Ty::Unit {
                            self.unit_ref_wrapper(fid, e.0)
                        } else {
                            fid
                        };
                        return Some(self.ir.add_expr(IrExpr::Lambda {
                            impl_fn,
                            arity: arity as u8,
                            captures,
                            sam: None,
                            inline_body: None,
                        }));
                    }
                }
                // Constructor reference `::A` (the name is a class, not a function): synthesize a
                // static impl `(ctor params) -> new A(params)` and wrap it in a closure, exactly as a
                // lambda `{ a -> A(a) }` would lower. Only the simple primary-constructor positional
                // case (the closure's arity matches the constructor's field params) is modeled.
                if !self.module_declares(&name) {
                    let ci = self.class_of(sig.ret)?;
                    let class_id = ci.id;
                    let ctor_count = self.ir.classes[class_id as usize].ctor_param_count as usize;
                    let ctor_args = self.ir.classes[class_id as usize].ctor_args.clone();
                    let field_tys: Vec<Ty> = if ctor_args.is_empty() {
                        self.ir.classes[class_id as usize].fields[..ctor_count]
                            .iter()
                            .map(|f| f.ty.clone())
                            .collect()
                    } else {
                        ctor_args.iter().map(|(t, _)| t.clone()).collect()
                    };
                    if field_tys.len() != arity {
                        return None;
                    }
                    let argvals: Vec<u32> = (0..arity as u32)
                        .map(|i| self.ir.add_expr(IrExpr::GetValue(i)))
                        .collect();
                    let new_e = self.ir.add_expr(IrExpr::New {
                        class: class_id,
                        args: argvals,
                        ctor_params: None,
                    });
                    let ret_e = self.ir.add_expr(IrExpr::Return(Some(new_e)));
                    let block = self.ir.add_expr(IrExpr::Block {
                        stmts: vec![ret_e],
                        value: None,
                    });
                    let impl_name = format!("{}$ctorref${}", self.cur_fn_name, self.lambda_seq);
                    self.lambda_seq += 1;
                    let fid = self.ir.add_fun(IrFunction {
                        name: impl_name,
                        params: field_tys,
                        ret: ty_to_ir(sig.ret),
                        body: Some(block),
                        is_static: true,
                        dispatch_receiver: None,
                        param_checks: Vec::new(),
                    });
                    return Some(self.ir.add_expr(IrExpr::Lambda {
                        impl_fn: fid,
                        arity: arity as u8,
                        captures: vec![],
                        sam: None,
                        inline_body: None,
                    }));
                }
                // `::foo` only resolves for a single-overload name (the checker enforces this), so the
                // sole entry for this name is the target.
                let fid = *self
                    .fun_ids
                    .iter()
                    .find(|((n, _), _)| n == &name)
                    .map(|(_, id)| id)?;
                let ret = self.ir.functions[fid as usize].ret.clone();
                let fn_params = self.ir.functions[fid as usize].params.clone();
                // A generic referenced function erases its type parameters — not modeled.
                if fn_params.len() != arity {
                    return None;
                }
                if self
                    .top_fun_decl(&name)
                    .map_or(false, |f| !f.type_params.is_empty())
                {
                    return None;
                }
                if ret == Ty::Nothing {
                    return None;
                }
                // A top-level function reference → a `FunctionReferenceImpl` subclass whose `invoke`
                // calls `invokestatic <facade>.foo(args)` (empty owner = facade). Unbound (an `INSTANCE`
                // singleton), top-level flags = 1. The subclass carries real reference equality.
                return Some(self.make_func_ref(
                    e.0,
                    false,
                    arity as u8,
                    String::new(),
                    name.clone(),
                    1,
                    crate::ir::FrDispatch::Static,
                    String::new(),
                    name.clone(),
                    false,
                    fn_params,
                    ret,
                    None,
                ));
            }
            Expr::Name(n) => {
                // `COROUTINE_SUSPENDED` (a `kotlin.coroutines` intrinsic, recognized via the registry) —
                // read the sentinel through its accessor `IntrinsicsKt.getCOROUTINE_SUSPENDED()`. A local
                // of the same name shadows it (resolved through the scope below).
                if self.lookup(&n).is_none()
                    && self.syms.libraries.coroutine_intrinsic(&n)
                        == Some(crate::libraries::CoroutineIntrinsic::CoroutineSuspended)
                {
                    return Some(self.ir.add_expr(IrExpr::Call {
                        callee: crate::ir::Callee::Static {
                            owner: "kotlin/coroutines/intrinsics/IntrinsicsKt".to_string(),
                            name: "getCOROUTINE_SUSPENDED".to_string(),
                            descriptor: "()Ljava/lang/Object;".to_string(),
                            inline: false,
                            must_inline: false,
                        },
                        dispatch_receiver: None,
                        args: vec![],
                    }));
                }
                // The `field` keyword inside a custom accessor body reads the property's backing field.
                if n == "field" {
                    if let Some((class_id, fidx, _)) = self.cur_field {
                        let this_e = self.ir.add_expr(IrExpr::GetValue(0));
                        return Some(self.ir.add_expr(IrExpr::GetField {
                            receiver: this_e,
                            class: class_id,
                            index: fidx,
                        }));
                    }
                }
                // A CLASSPATH `object` referenced as a value (`EmptyCoroutineContext`) — the checker
                // recorded it; read `getstatic <internal>.INSTANCE`.
                if let Some(internal) = self.info.obj_value_refs.get(&e) {
                    return Some(self.ir.add_expr(IrExpr::ExternalStaticField {
                        owner: internal.clone(),
                        name: "INSTANCE".to_string(),
                        descriptor: format!("L{internal};"),
                    }));
                }
                // A class NAME with a typed `companion object` used as a VALUE (`val c: I = C`): read its
                // companion singleton `getstatic C.Companion:LC$Companion;`. Only classes whose companion
                // declares a supertype get a registered `C$Companion` ClassSig (checked here); a local of
                // the same name shadows it.
                if self.lookup(&n).is_none() {
                    if let Some(cls) = self.syms.classes.get(&n) {
                        let comp_internal = format!("{}$Companion", cls.internal);
                        if self.syms.class_by_internal(&comp_internal).is_some() {
                            return Some(self.ir.add_expr(IrExpr::ExternalStaticField {
                                owner: cls.internal.clone(),
                                name: "Companion".to_string(),
                                descriptor: format!("L{comp_internal};"),
                            }));
                        }
                    }
                }
                // A local delegated property: read through the delegate's `getValue(null, propref)`.
                if let Some(ld) = self.local_delegated.get(&n).cloned() {
                    // Resolve the `$delegate` slot via the CURRENT scope (so a capture-remapped value
                    // space is honored); bail if it isn't reachable here (e.g. captured into a closure we
                    // don't thread the delegate through).
                    let (dslot, _) = self.lookup(&format!("{n}$delegate"))?;
                    let dele = self.ir.add_expr(IrExpr::GetValue(dslot));
                    let null_a = self.ir.add_expr(IrExpr::Const(IrConst::Null));
                    let pref = self.make_local_propref(&ld);
                    return Some(self.ir.add_expr(IrExpr::Call {
                        callee: crate::ir::Callee::Virtual {
                            owner: ld.delegate_internal.clone(),
                            name: "getValue".to_string(),
                            descriptor: ld.getvalue_desc.clone(),
                            interface: false,
                        },
                        dispatch_receiver: Some(dele),
                        args: vec![null_a, pref],
                    }));
                }
                // A boxed mutable-capture local: read through its `Ref` holder's `element`.
                if let Some(elem) = self.boxed_elem.get(&n).cloned() {
                    let (holder, _) = self.lookup(&n)?;
                    let hv = self.ir.add_expr(IrExpr::GetValue(holder));
                    return Some(self.ir.add_expr(IrExpr::RefGet {
                        holder: hv,
                        elem: ty_to_ir(elem),
                    }));
                }
                if let Some((v, slot_ty)) = self.lookup(&n) {
                    let read = self.ir.add_expr(IrExpr::GetValue(v));
                    // Smart-cast: the checker narrowed this read (`if (s is String) s` → `String`) below
                    // the variable's declared slot type. Insert the `checkcast` (a more specific
                    // reference) or unbox (a nullable primitive narrowed to the primitive) kotlinc emits.
                    let narrowed = self.info.ty(e);
                    // A reference (`Any`) smart-cast to `UInt`/`ULong` (`if (x is UInt) … x …`) would
                    // unbox the `kotlin.UInt` value type, but krusty erases unsigned to `int` and would
                    // emit an `Integer` unbox (ClassCastException) — skip rather than miscompile.
                    if matches!(narrowed, Ty::UInt | Ty::ULong) && slot_ty.is_reference() {
                        return None;
                    }
                    if narrowed != slot_ty && narrowed != Ty::Error {
                        if narrowed.is_primitive() && slot_ty.is_reference() {
                            self.ir.add_expr(IrExpr::TypeOp {
                                op: IrTypeOp::ImplicitCoercion,
                                arg: read,
                                type_operand: ty_to_ir(narrowed),
                            })
                        } else if narrowed.is_reference()
                            && slot_ty.is_reference()
                            && !matches!(narrowed, Ty::Null)
                            // A "narrowing" to `kotlin/Any` is a no-op WIDENING to the top type — never a
                            // real smart-cast. It arises when an inline expansion specializes a slot to a
                            // more concrete type than the checker's erased `info.ty` (a generic inline
                            // parameter/`this` bound to the actual argument type); the spurious
                            // `checkcast Object` it would emit erases the value and breaks verification.
                            && narrowed.obj_internal() != Some("kotlin/Any")
                        {
                            self.ir.add_expr(IrExpr::TypeOp {
                                op: IrTypeOp::Cast,
                                arg: read,
                                type_operand: ty_to_ir(narrowed),
                            })
                        } else {
                            read
                        }
                    } else {
                        read
                    }
                } else if let Some(&(fid, _)) = self.computed_props.get(&n) {
                    // A computed top-level property → call its `getX()` accessor.
                    self.ir.add_expr(IrExpr::Call {
                        callee: Callee::Local(fid),
                        dispatch_receiver: None,
                        args: vec![],
                    })
                } else if let Some(c) = self.const_lits.get(&n).cloned() {
                    // A same-file `const val` read → inline its literal (`ldc`), like kotlinc.
                    self.ir.add_expr(IrExpr::Const(c))
                } else if let Some(&(idx, _)) = self.statics.get(&n) {
                    self.ir.add_expr(IrExpr::GetStatic(idx))
                } else if let Some((facade, ty, _, is_const)) =
                    self.syms.prop_facades.get(&n).cloned()
                {
                    if is_const {
                        // A `const val` from another file has a PUBLIC field and NO accessor — read it as
                        // `getstatic <facade>.X` (kotlinc inlines the constant; a field read is equivalent
                        // and avoids a `NoSuchMethodError` on the non-existent `getX`).
                        self.ir.add_expr(IrExpr::ExternalStaticField {
                            owner: facade,
                            name: n.clone(),
                            descriptor: ty.descriptor(),
                        })
                    } else {
                        // A top-level property from ANOTHER file → call its facade's `getX()` (the field is
                        // private), reusing the backend-agnostic cross-file callee.
                        self.ir.add_expr(IrExpr::Call {
                            callee: Callee::CrossFile {
                                facade,
                                name: getter_name(&n),
                                params: vec![],
                                ret: ty_to_ir(ty),
                            },
                            dispatch_receiver: None,
                            args: vec![],
                        })
                    }
                } else if let Some(class) = self
                    .classes
                    .get(&class_internal(self.afile, &n))
                    .filter(|ci| self.ir.classes[ci.id as usize].is_object)
                    .map(|ci| ci.id)
                {
                    // A bare `object` name → its singleton instance.
                    self.ir.add_expr(IrExpr::StaticInstance {
                        owner: class,
                        ty: class,
                        field: "INSTANCE",
                    })
                } else if n == "Unit" {
                    // The `Unit` singleton used as a value → `getstatic kotlin/Unit.INSTANCE`.
                    self.ir.add_expr(IrExpr::UnitInstance)
                } else if let Some((owner, field, cty)) = {
                    // A bare CLASSPATH class with a companion object (`Json` → `Json.Default`): read the
                    // companion-instance static field. Resolve the simple name to its classpath internal
                    // via imports (`class_names`), exactly as the checker did. The checker already typed
                    // this as the companion's type, so member calls on it resolve normally.
                    self.syms
                        .class_names
                        .get(&n)
                        .filter(|i| !i.starts_with("__ty/"))
                        .map(|i| i.to_string())
                        .and_then(|internal| {
                            self.syms
                                .libraries
                                .resolve_type(&internal)
                                .and_then(|lt| lt.companion_object)
                                .map(|(f, t)| (internal, f, t))
                        })
                } {
                    self.ir.add_expr(IrExpr::ExternalStaticInstance {
                        owner,
                        ty: cty,
                        field,
                    })
                } else {
                    // Unqualified member of the enclosing class: a backing field (`this.<field>`), or a
                    // computed property (`this.getX()`).
                    let (this_v, this_ty) = self.lookup("this")?;
                    let recv = self.ir.add_expr(IrExpr::GetValue(this_v));
                    let read = if let Some(cur) = self.cur_class.clone() {
                        // An interface has no backing fields — its properties are abstract getters, so
                        // an unqualified property read in a default method routes through the getter
                        // (`invokeinterface getX`), never a (nonexistent) interface field.
                        let cur_is_iface = self
                            .classes
                            .get(&cur)
                            .is_some_and(|ci| self.ir.classes[ci.id as usize].is_interface);
                        let field = if cur_is_iface
                            || self
                                .field_accessor_props
                                .contains(&(cur.clone(), n.clone()))
                        {
                            // A custom-accessor property reads through `getX`, never the raw field.
                            None
                        } else {
                            self.classes.get(&cur).and_then(|ci| {
                                ci.fields
                                    .iter()
                                    .position(|(fn_, _)| *fn_ == n)
                                    .map(|i| (ci.id, i as u32))
                            })
                        };
                        if let Some((class, idx)) = field {
                            self.ir.add_expr(IrExpr::GetField {
                                receiver: recv,
                                class,
                                index: idx,
                            })
                        } else if let Some((class, index, _, _)) =
                            self.resolve_method(&cur, &getter_name(&n))
                        {
                            self.ir.add_expr(IrExpr::MethodCall {
                                class,
                                index,
                                receiver: recv,
                                args: vec![],
                            })
                        } else {
                            // An inner class reads an enclosing member through `this$0` (its field 0).
                            let cur_id = self.classes.get(&cur)?.id;
                            let outer = match self.ir.classes[cur_id as usize].fields.first() {
                                Some(IrField { name: n0, ty, .. })
                                    if n0 == "this$0" && ty.non_null().obj_internal().is_some() =>
                                {
                                    ty.non_null().obj_internal().unwrap().to_string()
                                }
                                _ => return None,
                            };
                            let this0 = self.ir.add_expr(IrExpr::GetField {
                                receiver: recv,
                                class: cur_id,
                                index: 0,
                            });
                            // The outer backing field is private — read it through its synthesized getter.
                            let (class, index, _, _) =
                                self.resolve_method(&outer, &getter_name(&n))?;
                            self.ir.add_expr(IrExpr::MethodCall {
                                class,
                                index,
                                receiver: this0,
                                args: vec![],
                            })
                        }
                    } else {
                        // An extension/receiver-lambda implicit receiver: `fun A.f() = n` (or
                        // `recv.run { n }`) reads `this.n` from OUTSIDE class A — through the property
                        // getter (the backing field is private) for a user class, the field directly when
                        // one resolves, else the shared builtin/library member read (`String.length`, a
                        // collection accessor, …) — the same path a qualified `this.n` read takes.
                        let internal = this_ty.obj_internal();
                        if let Some(internal) = internal {
                            if let Some((class, index, _, _)) =
                                self.resolve_method(internal, &getter_name(&n))
                            {
                                self.ir.add_expr(IrExpr::MethodCall {
                                    class,
                                    index,
                                    receiver: recv,
                                    args: vec![],
                                })
                            } else if let Some((fclass, idx, _)) = self.resolve_field(internal, &n)
                            {
                                self.ir.add_expr(IrExpr::GetField {
                                    receiver: recv,
                                    class: fclass,
                                    index: idx,
                                })
                            } else {
                                self.lower_member_read_on(recv, this_ty, &n, e)?
                            }
                        } else {
                            self.lower_member_read_on(recv, this_ty, &n, e)?
                        }
                    };
                    // Smart-cast narrowing: a nullable-primitive *field* read narrowed to its primitive
                    // (after `field != null`) must unbox the wrapper, exactly as the local-variable read
                    // path does — else the `Integer` value reaches an `int` context (a verify error).
                    let narrowed = self.info.ty(e);
                    let field_is_ref = this_ty
                        .obj_internal()
                        .and_then(|i| self.syms.prop_of(i, &n))
                        .map_or(false, |(t, _)| t.is_reference());
                    if narrowed.is_primitive() && field_is_ref {
                        self.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg: read,
                            type_operand: ty_to_ir(narrowed),
                        })
                    } else {
                        read
                    }
                }
            }
            // `a[i]` read → an intrinsic; `String[i]` is `kotlin/String.get` (a `Char`), else
            // `kotlin/Array.get` (backend reads element from the receiver type).
            Expr::Index { array, index } => {
                let at = self.info.ty(array);
                // `m[i]` on a USER class with an `operator fun get(index)` → `m.get(i)` (walks supers, so
                // an inherited operator resolves — consistent with the checker's `method_of`).
                if let Ty::Obj(internal, _) = at {
                    if at.array_elem().is_none() {
                        let getm = self
                            .resolve_method(internal, "get")
                            .map(|(class, midx, fid, _)| (class, midx, fid));
                        if let Some((class, midx, fid)) = getm {
                            let pty = self.ir.functions[fid as usize]
                                .params
                                .first()
                                .cloned()
                                .unwrap_or_else(|| ty_to_ir(self.info.ty(index)));
                            let a = self.expr(array)?;
                            let i = self.lower_arg(index, &pty)?;
                            return Some(self.ir.add_expr(IrExpr::MethodCall {
                                class,
                                index: midx,
                                receiver: a,
                                args: vec![Some(i)],
                            }));
                        }
                    }
                }
                // `coll[i]` on a library type (`List`, `Map`) → its `get(index)` operator member.
                if let Ty::Obj(internal, _) = at {
                    if at.array_elem().is_none() {
                        let it = self.info.ty(index);
                        if let Some(m) = crate::call_resolver::resolve_instance(
                            &*self.syms.libraries,
                            internal,
                            "get",
                            &[it],
                        ) {
                            let is_iface = self
                                .syms
                                .libraries
                                .resolve_type(internal)
                                .map_or(false, |t| t.is_interface());
                            let a = self.expr(array)?;
                            let i = self.lower_arg(
                                index,
                                &ty_to_ir(m.params.first().copied().unwrap_or(it)),
                            )?;
                            let read = self.ir.add_expr(IrExpr::Call {
                                callee: Callee::Virtual {
                                    owner: internal.to_string(),
                                    name: "get".to_string(),
                                    descriptor: m.descriptor.clone(),
                                    interface: is_iface,
                                },
                                dispatch_receiver: Some(a),
                                args: vec![i],
                            });
                            return Some(self.coerce_generic_read(read, e, m.ret));
                        }
                    }
                }
                let fq = if at == Ty::String {
                    "kotlin/String.get"
                } else {
                    "kotlin/Array.get"
                };
                let a = self.expr(array)?;
                let i = self.expr(index)?;
                self.ir.add_expr(IrExpr::Call {
                    callee: Callee::External(fq.to_string()),
                    dispatch_receiver: Some(a),
                    args: vec![i],
                })
            }
            Expr::Member { receiver, name } => {
                // A classpath nested singleton object recorded by the checker (`PrimitiveKind.STRING`) →
                // `getstatic <Outer$Nested>.INSTANCE`.
                if let Some(internal) = self.info.obj_value_refs.get(&e) {
                    return Some(self.ir.add_expr(IrExpr::ExternalStaticField {
                        owner: internal.clone(),
                        name: "INSTANCE".to_string(),
                        descriptor: format!("L{internal};"),
                    }));
                }
                // A classpath EXTENSION property recorded by the checker (`d.elementDescriptors`) →
                // `invokestatic <Kt>.get<Name>(recv)`.
                if let Some((owner, method, descriptor)) = self.info.ext_prop_calls.get(&e).cloned()
                {
                    let a = self.expr(receiver)?;
                    return Some(self.ir.add_expr(IrExpr::Call {
                        callee: Callee::Static {
                            owner,
                            name: method,
                            descriptor,
                            inline: false,
                            must_inline: false,
                        },
                        dispatch_receiver: None,
                        args: vec![a],
                    }));
                }
                // Reading an annotation member (`a.x`, `a` typed as the annotation interface): the JVM
                // accessor is the bare member name `x()` (not `getX`), dispatched by `invokeinterface`.
                if let Ty::Obj(internal, _) = self.info.ty(receiver) {
                    if let Some(field) = self
                        .ir
                        .classes
                        .iter()
                        .find(|c| c.fq_name == internal && c.is_annotation)
                        .and_then(|c| c.fields.iter().find(|f| f.name == *name))
                    {
                        let descriptor = format!(
                            "(){}",
                            crate::jvm::ir_emit::ir_ty_to_jvm(&field.ty).descriptor()
                        );
                        let a = self.expr(receiver)?;
                        return Some(self.ir.add_expr(IrExpr::Call {
                            callee: Callee::Virtual {
                                owner: internal.to_string(),
                                name: name.clone(),
                                descriptor,
                                interface: true,
                            },
                            dispatch_receiver: Some(a),
                            args: vec![],
                        }));
                    }
                }
                // A `val` extension property read (`x.doubled`) → its static getter `getDoubled(x)`.
                let rty = self.info.ty(receiver);
                if let Some(&gfid) = self.ext_prop_get_ids.get(&(rty.descriptor(), name.clone())) {
                    let a = self.expr(receiver)?;
                    return Some(self.ir.add_expr(IrExpr::Call {
                        callee: Callee::Local(gfid),
                        dispatch_receiver: None,
                        args: vec![a],
                    }));
                }
                // Primitive companion constant `Int.MAX_VALUE` / `Double.NaN` / … — inline the
                // compile-time value read from the library (kotlinc emits the same `ldc`).
                if let Expr::Name(rn) = self.afile.expr(receiver).clone() {
                    if matches!(
                        rn.as_str(),
                        "Int" | "Long" | "Short" | "Byte" | "Char" | "Double" | "Float" | "Boolean"
                    ) && self.lookup(&rn).is_none()
                    {
                        if let Some(lc) = self.syms.libraries.prim_companion_const(&rn, &name) {
                            let c = match lc {
                                // `Char.MAX_VALUE`/`MIN_VALUE` read back as an integer ConstantValue, but
                                // the constant's type is `Char` — emit a `Char` const so it boxes to
                                // `Character` (not `Integer`) in a vararg/generic position.
                                crate::libraries::LibConst::Int(v) if rn == "Char" => {
                                    IrConst::Char(char::from_u32(v as u32).unwrap_or('\0'))
                                }
                                crate::libraries::LibConst::Int(v) => IrConst::Int(v),
                                crate::libraries::LibConst::Long(v) => IrConst::Long(v),
                                crate::libraries::LibConst::Float(v) => IrConst::Float(v),
                                crate::libraries::LibConst::Double(v) => IrConst::Double(v),
                            };
                            return Some(self.ir.add_expr(IrExpr::Const(c)));
                        }
                    }
                }
                // `EnumClass.ENTRY` — a static enum-constant read.
                if let Expr::Name(rn) = self.afile.expr(receiver).clone() {
                    let internal = class_internal(self.afile, &rn);
                    if let Some(ci) = self.classes.get(&internal) {
                        let cls = ci.id;
                        if let Some(idx) = self.ir.classes[cls as usize]
                            .enum_entries
                            .iter()
                            .position(|(n, _)| *n == name)
                        {
                            return Some(self.ir.add_expr(IrExpr::EnumEntry {
                                class: cls,
                                index: idx as u32,
                            }));
                        }
                    }
                    // `C.X` where `X` is a companion `const val` → `getstatic C.X` (the field lives on the
                    // outer class C; the JVM initializes it from its `ConstantValue` attribute).
                    if let Some(cty) = self.companion_consts.get(&(internal.clone(), name.clone()))
                    {
                        return Some(self.ir.add_expr(IrExpr::ExternalStaticField {
                            owner: internal,
                            name: name.clone(),
                            descriptor: cty.descriptor(),
                        }));
                    }
                }
                let rt = self.recv_ty(receiver);
                // `e.ordinal` / `e.name` on an enum value → `Enum.ordinal()`/`Enum.name()`.
                if matches!(name.as_str(), "ordinal" | "name") {
                    if let Some(ci) = self.class_of(rt) {
                        if !self.ir.classes[ci.id as usize].enum_entries.is_empty() {
                            let recv = self.expr(receiver)?;
                            let fq = format!("java/lang/Enum.{name}");
                            return Some(self.ir.add_expr(IrExpr::Call {
                                callee: Callee::External(fq),
                                dispatch_receiver: Some(recv),
                                args: vec![],
                            }));
                        }
                    }
                }
                if rt == Ty::Char && name == "code" {
                    // `c.code` → the `Char`'s code unit as an `Int` (a no-op coercion on the JVM stack).
                    let c = self.expr(receiver)?;
                    self.ir.add_expr(IrExpr::TypeOp {
                        op: IrTypeOp::ImplicitCoercion,
                        arg: c,
                        type_operand: ty_to_ir(Ty::Int),
                    })
                } else if rt.array_elem().is_some() && name == "size" {
                    let a = self.expr(receiver)?;
                    self.ir.add_expr(IrExpr::Call {
                        callee: Callee::External("kotlin/Array.size".to_string()),
                        dispatch_receiver: Some(a),
                        args: vec![],
                    })
                } else if let Some(rci) = self.class_of(rt) {
                    // Resolve the field through the superclass chain — it may be declared on a base
                    // class (`b.baseField`). `class` is the *owning* class (whose fieldref we emit).
                    let recv_internal = rci.internal.clone();
                    if let Some((class, idx, pty)) = self.resolve_field(&recv_internal, &name) {
                        let owner_internal = self.ir.classes[class as usize].fq_name.clone();
                        // The backing field is private; access from outside the declaring class goes
                        // through the public `getX()` accessor (matching kotlinc). Inside the class,
                        // read the field directly.
                        if self.cur_class.as_deref() != Some(owner_internal.as_str()) {
                            if let Some((mclass, mindex, _, _)) =
                                self.resolve_method(&recv_internal, &getter_name(&name))
                            {
                                let recv = self.expr(receiver)?;
                                let read = self.ir.add_expr(IrExpr::MethodCall {
                                    class: mclass,
                                    index: mindex,
                                    receiver: recv,
                                    args: vec![],
                                });
                                return Some(self.coerce_generic_read(read, e, pty));
                            }
                        }
                        let recv = self.expr(receiver)?;
                        // Smartcast: if the receiver's *slot* type isn't the owning class (e.g. an erased
                        // generic / `Any?` local narrowed by `is`), checkcast it so `getfield` is valid.
                        let needs_cast = matches!(self.afile.expr(receiver), Expr::Name(n)
                            if self.lookup(n).map_or(false, |(_, t)| t != Ty::obj(&owner_internal)));
                        let recv = if needs_cast {
                            self.ir.add_expr(IrExpr::TypeOp {
                                op: IrTypeOp::Cast,
                                arg: recv,
                                type_operand: ty_to_ir(Ty::obj(&owner_internal)),
                            })
                        } else {
                            recv
                        };
                        let read = self.ir.add_expr(IrExpr::GetField {
                            receiver: recv,
                            class,
                            index: idx,
                        });
                        self.coerce_generic_read(read, e, pty)
                    } else if let Some((class, index, _, _)) =
                        self.resolve_method(&recv_internal, &getter_name(&name))
                    {
                        // A computed property → `recv.getX()`.
                        let recv = self.expr(receiver)?;
                        self.ir.add_expr(IrExpr::MethodCall {
                            class,
                            index,
                            receiver: recv,
                            args: vec![],
                        })
                    } else {
                        return None;
                    }
                } else {
                    // A property read on a builtin/library/another-file receiver (`s.length`,
                    // `list.size`, a sibling class's `getX()`): resolved generically through the shared
                    // member-read helper (no per-member name hardcode). The receiver may be smart-cast to
                    // a narrower reference type than its slot (`Any` narrowed by `is String`) — checkcast.
                    let recv = self.expr(receiver)?;
                    let recv = if rt.is_reference()
                        && matches!(self.afile.expr(receiver), Expr::Name(n)
                            if self.lookup(n).map_or(false, |(_, t)| t != rt))
                    {
                        self.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::Cast,
                            arg: recv,
                            type_operand: ty_to_ir(rt),
                        })
                    } else {
                        recv
                    };
                    self.lower_member_read_on(recv, rt, &name, e)?
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                // `&&` / `||` SHORT-CIRCUIT: the right operand must not be evaluated when the left
                // already decides the result (`x != 0 && 10 / x > 0` must not divide when `x == 0`).
                // Lower to a branch — `a && b` → `if (a) b else false`, `a || b` → `if (a) true else b`
                // — never the eager `iand`/`ior` (which evaluates both, a miscompile for a side-effecting
                // or throwing right operand). This is also the control-flow shape kotlinc emits.
                if matches!(op, BinOp::And | BinOp::Or) {
                    // Constant-fold a literal left operand (kotlinc folds these; a `const val` initializer
                    // must stay a constant, not a branch): `false && _` → false, `true && b` → b,
                    // `true || _` → true, `false || b` → b.
                    if let Expr::BoolLit(lv) = self.afile.expr(lhs) {
                        let lv = *lv;
                        return match (op, lv) {
                            (BinOp::And, false) => {
                                Some(self.ir.add_expr(IrExpr::Const(IrConst::Boolean(false))))
                            }
                            (BinOp::Or, true) => {
                                Some(self.ir.add_expr(IrExpr::Const(IrConst::Boolean(true))))
                            }
                            // `true && b` / `false || b` → the right operand.
                            _ => self.expr(rhs),
                        };
                    }
                    // Inside a suspend body the right operand may carry a suspension the CPS flattener
                    // models only at an unconditional position — keep the eager form (`iand`/`ior`) there;
                    // a non-suspend body can't call a suspend fn, so the branch short-circuit is safe.
                    if !self.cur_fn_suspend {
                        let l = self.expr(lhs)?;
                        let r = self.expr(rhs)?;
                        let konst = |this: &mut Self, b: bool| {
                            this.ir.add_expr(IrExpr::Const(IrConst::Boolean(b)))
                        };
                        let (then_e, else_e) = if op == BinOp::And {
                            let f = konst(self, false);
                            (r, f)
                        } else {
                            let t = konst(self, true);
                            (t, r)
                        };
                        return Some(self.ir.add_expr(IrExpr::When {
                            branches: vec![(Some(l), then_e), (None, else_e)],
                        }));
                    }
                }
                // Unsigned `+`/`-`/`*`/`==`/`!=` match the signed two's-complement opcodes, but
                // `/`/`%`/`<`/`>`/`<=`/`>=` need the JDK unsigned intrinsics kotlinc calls:
                // `Integer.{divide,remainder,compare}Unsigned` (`Long.*` for `ULong`). A comparison is
                // `compareUnsigned(l, r) <op> 0`.
                let lty = self.info.ty(lhs);
                if lty.is_unsigned()
                    && matches!(
                        op,
                        BinOp::Div | BinOp::Rem | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
                    )
                {
                    let is_uint = lty == Ty::UInt;
                    let owner = if is_uint {
                        "java/lang/Integer"
                    } else {
                        "java/lang/Long"
                    };
                    let prim = if is_uint { "I" } else { "J" };
                    let l = self.expr(lhs)?;
                    let r = self.expr(rhs)?;
                    let call = |this: &mut Self, name: &str, desc: String, args: Vec<u32>| {
                        this.ir.add_expr(IrExpr::Call {
                            callee: Callee::Static {
                                owner: owner.to_string(),
                                name: name.to_string(),
                                descriptor: desc,
                                inline: false,
                                must_inline: false,
                            },
                            dispatch_receiver: None,
                            args,
                        })
                    };
                    return Some(match op {
                        BinOp::Div => call(
                            self,
                            "divideUnsigned",
                            format!("({prim}{prim}){prim}"),
                            vec![l, r],
                        ),
                        BinOp::Rem => call(
                            self,
                            "remainderUnsigned",
                            format!("({prim}{prim}){prim}"),
                            vec![l, r],
                        ),
                        _ => {
                            let cmp = call(
                                self,
                                "compareUnsigned",
                                format!("({prim}{prim})I"),
                                vec![l, r],
                            );
                            let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                            self.ir.add_expr(IrExpr::PrimitiveBinOp {
                                op: bin_to_ir(op)?,
                                lhs: cmp,
                                rhs: zero,
                            })
                        }
                    });
                }
                // A user `operator fun LhsType.plus(…)` (etc.) extension overrides the builtin operator.
                let op_name = match op {
                    BinOp::Add => Some("plus"),
                    BinOp::Sub => Some("minus"),
                    BinOp::Mul => Some("times"),
                    BinOp::Div => Some("div"),
                    BinOp::Rem => Some("rem"),
                    _ => None,
                };
                if let Some(opn) = op_name {
                    let recv_desc = self.recv_ty(lhs).descriptor();
                    if let Some(&fid) = self.ext_fun_ids.get(&(recv_desc, opn.to_string())) {
                        let params = self.ir.functions[fid as usize].params.clone();
                        if params.len() == 2 {
                            let l = self.lower_arg(lhs, &params[0])?;
                            let r = self.lower_arg(rhs, &params[1])?;
                            return Some(self.ir.add_expr(IrExpr::Call {
                                callee: Callee::Local(fid),
                                dispatch_receiver: None,
                                args: vec![l, r],
                            }));
                        }
                    }
                    // A class MEMBER operator (`operator fun plus(o: V)`): `a + b` → `a.plus(b)`.
                    if let Some(internal) = self.recv_ty(lhs).obj_internal().map(|s| s.to_string())
                    {
                        if let Some((class, index, mfid, _)) = self.resolve_method(&internal, opn) {
                            let params = self.ir.functions[mfid as usize].params.clone();
                            if params.len() == 1 {
                                let l = self.expr(lhs)?;
                                let r = self.lower_arg(rhs, &params[0])?;
                                return Some(self.ir.add_expr(IrExpr::MethodCall {
                                    class,
                                    index,
                                    receiver: l,
                                    args: vec![Some(r)],
                                }));
                            }
                        }
                    }
                }
                // A class `operator fun compareTo(o): Int` drives a comparison: `a < b` →
                // `a.compareTo(b) < 0`.
                if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
                    if let Some(internal) = self.recv_ty(lhs).obj_internal().map(|s| s.to_string())
                    {
                        if let Some((class, index, mfid, _)) =
                            self.resolve_method(&internal, "compareTo")
                        {
                            let params = self.ir.functions[mfid as usize].params.clone();
                            if params.len() == 1 {
                                let l = self.expr(lhs)?;
                                let r = self.lower_arg(rhs, &params[0])?;
                                let cmp = self.ir.add_expr(IrExpr::MethodCall {
                                    class,
                                    index,
                                    receiver: l,
                                    args: vec![Some(r)],
                                });
                                let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                                return Some(self.ir.add_expr(IrExpr::PrimitiveBinOp {
                                    op: bin_to_ir(op)?,
                                    lhs: cmp,
                                    rhs: zero,
                                }));
                            }
                        }
                    }
                }
                // A library operator function on a reference receiver (`list + x` → `CollectionsKt.plus(list,
                // x)`): re-resolve through the library set (most-specific overload) and emit the call,
                // lowering the receiver and argument to the callee's parameter types (a primitive element
                // boxes to `Object`).
                if let Some(opn) = op_name {
                    let lt = self.info.ty(lhs);
                    if lt.is_reference() && self.info.ty(rhs) != Ty::Error {
                        let rt = self.info.ty(rhs);
                        if let Some(c) =
                            self.syms
                                .libraries
                                .resolve_callable(opn, Some(lt), &[rt], &[])
                        {
                            if c.params.len() == 2 {
                                let l = self.lower_arg(lhs, &ty_to_ir(c.params[0]))?;
                                let r = self.lower_arg(rhs, &ty_to_ir(c.params[1]))?;
                                return Some(self.ir.add_expr(IrExpr::Call {
                                    callee: Callee::Static {
                                        owner: c.owner,
                                        name: c.name,
                                        descriptor: c.descriptor,
                                        inline: c.is_inline,
                                        must_inline: false,
                                    },
                                    dispatch_receiver: None,
                                    args: vec![l, r],
                                }));
                            }
                        }
                    }
                }
                if op == BinOp::Add
                    && (self.info.ty(lhs) == Ty::String || self.info.ty(rhs) == Ty::String)
                {
                    // Flatten the left-nested concat chain (`a + b + c + …`) iteratively, then fold —
                    // a deep chain (a stress test with hundreds of `+`) would otherwise recurse through
                    // `expr` once per operator and overflow the stack. The emitted `String.plus` chain
                    // is identical to the recursive lowering.
                    let is_concat = |this: &Self, l: AstExprId, r: AstExprId| {
                        this.info.ty(l) == Ty::String || this.info.ty(r) == Ty::String
                    };
                    let mut operands = vec![rhs];
                    let mut cur = lhs;
                    loop {
                        match self.afile.expr(cur).clone() {
                            Expr::Binary {
                                op: BinOp::Add,
                                lhs: l2,
                                rhs: r2,
                            } if is_concat(self, l2, r2) => {
                                operands.push(r2);
                                cur = l2;
                            }
                            _ => {
                                operands.push(cur);
                                break;
                            }
                        }
                    }
                    operands.reverse();
                    // Lower an operand, converting an unsigned value to its UNSIGNED decimal string
                    // (`String.plus`/`String.valueOf` on the erased int would print the signed value).
                    let lower_concat_operand = |this: &mut Self, oe: AstExprId| -> Option<u32> {
                        let v = this.expr(oe)?;
                        let t = this.info.ty(oe);
                        Some(if t.is_unsigned() {
                            this.unsigned_to_string(v, t)
                        } else {
                            v
                        })
                    };
                    let mut acc = lower_concat_operand(self, operands[0])?;
                    for &op_e in &operands[1..] {
                        let r = lower_concat_operand(self, op_e)?;
                        acc = self.ir.add_expr(IrExpr::Call {
                            callee: Callee::External("kotlin/String.plus".to_string()),
                            dispatch_receiver: Some(acc),
                            args: vec![r],
                        });
                    }
                    acc
                } else {
                    let irop = bin_to_ir(op)?;
                    // A primitive op needs both operands in one type. For mixed numeric operands
                    // (`Double < Int`, `1 + 2L`) coerce both to the promoted type; the backend emits
                    // the conversion. Non-numeric mixes (e.g. involving `Char`) have no promotion → bail.
                    let (lt, rt) = (self.info.ty(lhs), self.info.ty(rhs));
                    let mut l = self.expr(lhs)?;
                    let mut r = self.expr(rhs)?;
                    // `Char` arithmetic (`'a' + 1`, `c - 1`, `c1 - c2`): `Char`/`Int` share the int stack
                    // representation, but there is no numeric *promotion* between them. Do the op on ints
                    // (coerce the `Char` operands to `Int` — a no-op on the stack, but it types the result
                    // as `Int`); a `Char` result then truncates with `i2c` (Kotlin wraps mod 2^16), a
                    // `Char - Char` difference is a plain `Int`. The checker already typed `e` accordingly.
                    if lt == Ty::Char
                        && matches!(op, BinOp::Add | BinOp::Sub)
                        && (rt == Ty::Int || rt == Ty::Char)
                    {
                        let int_ir = ty_to_ir(Ty::Int);
                        let li = self.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg: l,
                            type_operand: int_ir.clone(),
                        });
                        let ri = if rt == Ty::Char {
                            self.ir.add_expr(IrExpr::TypeOp {
                                op: IrTypeOp::ImplicitCoercion,
                                arg: r,
                                type_operand: int_ir,
                            })
                        } else {
                            r
                        };
                        let raw = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op: irop,
                            lhs: li,
                            rhs: ri,
                        });
                        return Some(if self.info.ty(e) == Ty::Char {
                            self.ir.add_expr(IrExpr::TypeOp {
                                op: IrTypeOp::ImplicitCoercion,
                                arg: raw,
                                type_operand: ty_to_ir(Ty::Char),
                            })
                        } else {
                            raw
                        });
                    }
                    if lt.is_primitive() && rt.is_primitive() && lt != rt {
                        let p = Ty::promote(lt, rt)?;
                        let pir = ty_to_ir(p);
                        if lt != p {
                            l = self.ir.add_expr(IrExpr::TypeOp {
                                op: IrTypeOp::ImplicitCoercion,
                                arg: l,
                                type_operand: pir.clone(),
                            });
                        }
                        if rt != p {
                            r = self.ir.add_expr(IrExpr::TypeOp {
                                op: IrTypeOp::ImplicitCoercion,
                                arg: r,
                                type_operand: pir,
                            });
                        }
                    } else if matches!(op, BinOp::Eq | BinOp::Ne)
                        && lt.is_reference() != rt.is_reference()
                    {
                        // A nullable-primitive wrapper (`Int?`) compared with a primitive: match kotlinc's
                        // short-circuit — when the wrapper is null the result is fixed (`!=`→true,
                        // `==`→false) WITHOUT evaluating the primitive side (which may have side effects).
                        // `{ val t = wrapper; if (t == null) <fixed> else t.unbox <op> prim }`.
                        let l_wp = lt.nullable_primitive();
                        let r_wp = rt.nullable_primitive();
                        if let Some(wp) = l_wp.or(r_wp) {
                            let (w_e, w_ty, p_e) = if l_wp.is_some() {
                                (lhs, lt, rhs)
                            } else {
                                (rhs, rt, lhs)
                            };
                            let wv = self.expr(w_e)?;
                            let v = self.fresh_value();
                            let var = self.ir.add_expr(IrExpr::Variable {
                                index: v,
                                ty: ty_to_ir(w_ty),
                                init: Some(wv),
                            });
                            let getn = self.ir.add_expr(IrExpr::GetValue(v));
                            let nullc = self.ir.add_expr(IrExpr::Const(IrConst::Null));
                            let isnull = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                                op: IrBinOp::Eq,
                                lhs: getn,
                                rhs: nullc,
                            });
                            let getw = self.ir.add_expr(IrExpr::GetValue(v));
                            let unboxed = self.ir.add_expr(IrExpr::TypeOp {
                                op: IrTypeOp::ImplicitCoercion,
                                arg: getw,
                                type_operand: ty_to_ir(wp),
                            });
                            let pv = self.lower_arg(p_e, &ty_to_ir(wp))?;
                            let cmp = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                                op: irop,
                                lhs: unboxed,
                                rhs: pv,
                            });
                            let fixed = self
                                .ir
                                .add_expr(IrExpr::Const(IrConst::Boolean(op == BinOp::Ne)));
                            let when = self.ir.add_expr(IrExpr::When {
                                branches: vec![(Some(isnull), fixed), (None, cmp)],
                            });
                            return Some(self.ir.add_expr(IrExpr::Block {
                                stmts: vec![var],
                                value: Some(when),
                            }));
                        }
                        // A general `Any == 5`: box the primitive operand → structural `Intrinsics.areEqual`.
                        let obj = ty_to_ir(Ty::obj("kotlin/Any"));
                        if lt.is_primitive() {
                            l = self.ir.add_expr(IrExpr::TypeOp {
                                op: IrTypeOp::ImplicitCoercion,
                                arg: l,
                                type_operand: obj,
                            });
                        } else {
                            r = self.ir.add_expr(IrExpr::TypeOp {
                                op: IrTypeOp::ImplicitCoercion,
                                arg: r,
                                type_operand: obj,
                            });
                        }
                    }
                    self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: irop,
                        lhs: l,
                        rhs: r,
                    })
                }
            }
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                // Constant-fold a literal-boolean condition (`if (false) { … }`) — emit only the taken
                // branch, like kotlinc's dead-code elimination. (Emitting the dead branch can produce
                // unverifiable frames, e.g. a `try` whose handler slot conflicts in unreachable code.)
                match self.afile.expr(cond) {
                    Expr::BoolLit(true) => return self.expr(then_branch),
                    Expr::BoolLit(false) => {
                        return match else_branch {
                            Some(els) => self.expr(els),
                            // `if (false) {}` with no else is a no-op `Unit` statement.
                            None => Some(self.ir.add_expr(IrExpr::Block {
                                stmts: vec![],
                                value: None,
                            })),
                        };
                    }
                    _ => {}
                }
                let c = self.expr(cond)?;
                // When the `if`'s result type is a reference but a branch is a primitive (`if (c) true else
                // null` → `Boolean?`), the primitive branch must be boxed at the merge so both branches
                // agree on the (reference) stack type — `lower_arg` to the result type inserts the box.
                let res = self.info.ty(e);
                let t = if res.is_reference() {
                    self.lower_arg(then_branch, &ty_to_ir(res))?
                } else {
                    self.expr(then_branch)?
                };
                let branches = match else_branch {
                    Some(els) => {
                        let e2 = if res.is_reference() {
                            self.lower_arg(els, &ty_to_ir(res))?
                        } else {
                            self.expr(els)?
                        };
                        vec![(Some(c), t), (None, e2)]
                    }
                    None => vec![(Some(c), t)],
                };
                self.ir.add_expr(IrExpr::When { branches })
            }
            // `x is T` / `x !is T` / `x as T` → the existing `IrTypeOp` node (no new node).
            Expr::Is {
                operand,
                ty,
                negated,
            } => {
                // A reified type parameter (`x is T` in a `<reified T>` inline body) → the bound type.
                let ty = self.subst_type_ref(&ty);
                // A nullable reference target (`x is A?`): `null` IS an `A?`, but plain `instanceof`
                // yields false for null. Lower to `x == null || x is A` (and the De Morgan dual for
                // `x !is A?` → `x != null && x !is A`), binding the operand to a temp so it runs once.
                if ty.nullable {
                    // `ty_ref` returns `None` for any nullable type; resolve the non-null base reference.
                    let mut base_ref = ty.clone();
                    base_ref.nullable = false;
                    if let Some(target) = self.ty_ref(&base_ref) {
                        let arg = self.expr(operand)?;
                        let v = self.fresh_value();
                        // The temp only feeds `== null` and `instanceof`, so an `Object` slot always
                        // holds it — a precise operand type (or `null`/`Nothing`) could be an invalid
                        // local-variable type.
                        let opnd_ty = ty_to_ir(Ty::obj("kotlin/Any"));
                        let g1 = self.ir.add_expr(IrExpr::GetValue(v));
                        let nullc = self.ir.add_expr(IrExpr::Const(IrConst::Null));
                        let null_test = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op: if negated {
                                IrBinOp::RefNe
                            } else {
                                IrBinOp::RefEq
                            },
                            lhs: g1,
                            rhs: nullc,
                        });
                        let g2 = self.ir.add_expr(IrExpr::GetValue(v));
                        let inst = self.ir.add_expr(IrExpr::TypeOp {
                            op: if negated {
                                IrTypeOp::NotInstanceOf
                            } else {
                                IrTypeOp::InstanceOf
                            },
                            arg: g2,
                            type_operand: ty_to_ir(target),
                        });
                        let combined = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op: if negated { IrBinOp::And } else { IrBinOp::Or },
                            lhs: null_test,
                            rhs: inst,
                        });
                        let temp = self.ir.add_expr(IrExpr::Variable {
                            index: v,
                            ty: opnd_ty,
                            init: Some(arg),
                        });
                        return Some(self.ir.add_expr(IrExpr::Block {
                            stmts: vec![temp],
                            value: Some(combined),
                        }));
                    }
                }
                let arg = self.expr(operand)?;
                let op = if negated {
                    IrTypeOp::NotInstanceOf
                } else {
                    IrTypeOp::InstanceOf
                };
                // A reference target, or a primitive (`x is Int` → `instanceof` the boxed wrapper, which
                // the backend resolves from the primitive type_operand).
                let target = self.ty_ref(&ty).or_else(|| {
                    if ty.nullable {
                        None
                    } else {
                        Ty::from_name(&ty.name)
                            .filter(|t| t.is_primitive() && !matches!(t, Ty::Double | Ty::Float))
                    }
                })?;
                // An unsigned target tests against its inline-class object (`kotlin/UInt`), not the
                // representation's wrapper (`Integer`).
                let type_operand = if target.is_unsigned() {
                    ty_to_ir(Ty::obj(if target == Ty::UInt {
                        "kotlin/UInt"
                    } else {
                        "kotlin/ULong"
                    }))
                } else {
                    ty_to_ir(target)
                };
                self.ir.add_expr(IrExpr::TypeOp {
                    op,
                    arg,
                    type_operand,
                })
            }
            Expr::InRange {
                value,
                start,
                end,
                kind,
                negated,
            } => {
                use crate::ast::RangeKind;
                // Evaluate the bounds then the value once each (source order: start, end, value —
                // matching kotlinc's `start..end` then `.contains(value)`), into temps, then a
                // comparison chain. `!in` uses the De Morgan dual so no logical-not node is needed.
                let s = self.expr(start)?;
                let sv = self.fresh_value();
                let var_s = self.ir.add_expr(IrExpr::Variable {
                    index: sv,
                    ty: ty_to_ir(self.info.ty(start)),
                    init: Some(s),
                });
                let en = self.expr(end)?;
                let ev = self.fresh_value();
                let var_e = self.ir.add_expr(IrExpr::Variable {
                    index: ev,
                    ty: ty_to_ir(self.info.ty(end)),
                    init: Some(en),
                });
                let v = self.expr(value)?;
                let vv = self.fresh_value();
                let var_v = self.ir.add_expr(IrExpr::Variable {
                    index: vv,
                    ty: ty_to_ir(self.info.ty(value)),
                    init: Some(v),
                });
                // `lo`/`hi` are the inclusive low / (in/ex)clusive high bound. `downTo` runs high→low, so
                // membership is `end <= value <= start` — swap the bounds.
                let (lo, hi, hi_strict) = match kind {
                    RangeKind::Through => (sv, ev, false),
                    RangeKind::Until => (sv, ev, true),
                    RangeKind::DownTo => (ev, sv, false),
                };
                // A comparison `a <op> b` on the loaded temps. For an unsigned element type the operands
                // are compared via `Integer/Long.compareUnsigned(a, b) <op> 0` (a signed opcode would
                // misorder values past the sign bit), matching kotlinc's unsigned-range membership.
                let elem = self.info.ty(value);
                let cmp = |this: &mut Self, op: IrBinOp, a: u32, b: u32| -> u32 {
                    let la = this.ir.add_expr(IrExpr::GetValue(a));
                    let lb = this.ir.add_expr(IrExpr::GetValue(b));
                    if elem.is_unsigned() {
                        let (owner, prim) = if elem == Ty::UInt {
                            ("java/lang/Integer", "I")
                        } else {
                            ("java/lang/Long", "J")
                        };
                        let call = this.ir.add_expr(IrExpr::Call {
                            callee: Callee::Static {
                                owner: owner.to_string(),
                                name: "compareUnsigned".to_string(),
                                descriptor: format!("({prim}{prim})I"),
                                inline: false,
                                must_inline: false,
                            },
                            dispatch_receiver: None,
                            args: vec![la, lb],
                        });
                        let zero = this.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                        this.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op,
                            lhs: call,
                            rhs: zero,
                        })
                    } else {
                        this.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op,
                            lhs: la,
                            rhs: lb,
                        })
                    }
                };
                let cond = if negated {
                    // value < lo  ||  value (> | >=) hi
                    let c1 = cmp(self, IrBinOp::Lt, vv, lo);
                    let c2 = cmp(
                        self,
                        if hi_strict { IrBinOp::Ge } else { IrBinOp::Gt },
                        vv,
                        hi,
                    );
                    self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: IrBinOp::Or,
                        lhs: c1,
                        rhs: c2,
                    })
                } else {
                    // lo <= value  &&  value (< | <=) hi
                    let c1 = cmp(self, IrBinOp::Le, lo, vv);
                    let c2 = cmp(
                        self,
                        if hi_strict { IrBinOp::Lt } else { IrBinOp::Le },
                        vv,
                        hi,
                    );
                    self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: IrBinOp::And,
                        lhs: c1,
                        rhs: c2,
                    })
                };
                self.ir.add_expr(IrExpr::Block {
                    stmts: vec![var_s, var_e, var_v],
                    value: Some(cond),
                })
            }
            Expr::RangeTo { lo, hi, kind } => {
                use crate::ast::RangeKind;
                // The operand types select the range class and the element type its operands widen to:
                // `Char..Char` → `CharRange`; the integer family widens (`Byte`/`Short`/`Int` → `IntRange`,
                // anything with a `Long` → `LongRange`), mirroring kotlinc's `rangeTo` overloads. The
                // bounds are coerced to that element type (`Byte`→`Int` is a no-op on the JVM stack).
                let lt = self.info.ty(lo);
                let rt = self.info.ty(hi);
                let small_int = |t: &Ty| matches!(t, Ty::Byte | Ty::Short | Ty::Int);
                let (range_internal, prim_desc, elem) = match (lt, rt) {
                    (Ty::Char, Ty::Char) => ("kotlin/ranges/CharRange", "C", Ty::Char),
                    (Ty::UInt, Ty::UInt) => ("kotlin/ranges/UIntRange", "I", Ty::UInt),
                    (Ty::ULong, Ty::ULong) => ("kotlin/ranges/ULongRange", "J", Ty::ULong),
                    (l, r) if small_int(&l) && small_int(&r) => {
                        ("kotlin/ranges/IntRange", "I", Ty::Int)
                    }
                    (l, r)
                        if (small_int(&l) || l == Ty::Long) && (small_int(&r) || r == Ty::Long) =>
                    {
                        ("kotlin/ranges/LongRange", "J", Ty::Long)
                    }
                    _ => return None,
                };
                let lo_v = self.lower_arg(lo, &ty_to_ir(elem))?;
                let hi_v = self.lower_arg(hi, &ty_to_ir(elem))?;
                match kind {
                    // `a..b` → `new IntRange(a, b)` (kotlinc's intrinsic constructor). The unsigned range
                    // classes' public ctor takes a trailing synthetic `DefaultConstructorMarker` (null).
                    RangeKind::Through if elem.is_unsigned() => {
                        let marker = self.ir.add_expr(IrExpr::Const(IrConst::Null));
                        let ctor_desc = format!("({prim_desc}{prim_desc}Lkotlin/jvm/internal/DefaultConstructorMarker;)V");
                        self.ir.add_expr(IrExpr::NewExternal {
                            internal: range_internal.to_string(),
                            ctor_desc,
                            args: vec![lo_v, hi_v, marker],
                        })
                    }
                    RangeKind::Through => {
                        let ctor_desc = format!("({prim_desc}{prim_desc})V");
                        self.ir.add_expr(IrExpr::NewExternal {
                            internal: range_internal.to_string(),
                            ctor_desc,
                            args: vec![lo_v, hi_v],
                        })
                    }
                    // `a..<b` → `RangesKt.until(a, b)` (the `rangeUntil` operator), returning the range.
                    // (Unsigned `..<` uses a different intrinsic krusty doesn't model yet — skip.)
                    RangeKind::Until if elem.is_unsigned() => return None,
                    RangeKind::Until => {
                        let descriptor = format!("({prim_desc}{prim_desc})L{range_internal};");
                        self.ir.add_expr(IrExpr::Call {
                            callee: Callee::Static {
                                owner: "kotlin/ranges/RangesKt".to_string(),
                                name: "until".to_string(),
                                descriptor,
                                inline: false,
                                must_inline: false,
                            },
                            dispatch_receiver: None,
                            args: vec![lo_v, hi_v],
                        })
                    }
                    // `downTo` never reaches here (it parses as an infix function call, not `RangeTo`).
                    RangeKind::DownTo => return None,
                }
            }
            Expr::IncDec {
                target,
                dec,
                prefix,
            } => {
                // `var++`/`++var` as a value. Only a simple local/captured variable; anything else bails.
                // No temp slot: the update is `i = i ± 1`; the value is the new `i` (prefix) or, for a
                // postfix, the new `i` minus the step (the old value) — valid for every numeric type.
                let Expr::Name(name) = self.afile.expr(target).clone() else {
                    return None;
                };
                // A boxed mutable-capture local: `var++`/`++var` as a value, through its `Ref` holder.
                // (A `Byte`/`Short`/`Char` boxed inc-as-expression is rare — skip it rather than model
                // the narrowing here.)
                if let Some(elem) = self.boxed_elem.get(&name).cloned() {
                    let one_c = match elem {
                        Ty::Int => IrConst::Int(1),
                        Ty::Long => IrConst::Long(1),
                        Ty::Double => IrConst::Double(1.0),
                        Ty::Float => IrConst::Float(1.0),
                        _ => return None,
                    };
                    let (holder, _) = self.lookup(&name)?;
                    let elem_ir = ty_to_ir(elem);
                    let op = if dec { IrBinOp::Sub } else { IrBinOp::Add };
                    let undo = if dec { IrBinOp::Add } else { IrBinOp::Sub };
                    let h1 = self.ir.add_expr(IrExpr::GetValue(holder));
                    let cur = self.ir.add_expr(IrExpr::RefGet {
                        holder: h1,
                        elem: elem_ir.clone(),
                    });
                    let one1 = self.ir.add_expr(IrExpr::Const(one_c.clone()));
                    let nv = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op,
                        lhs: cur,
                        rhs: one1,
                    });
                    let h2 = self.ir.add_expr(IrExpr::GetValue(holder));
                    let set = self.ir.add_expr(IrExpr::RefSet {
                        holder: h2,
                        elem: elem_ir.clone(),
                        value: nv,
                    });
                    let h3 = self.ir.add_expr(IrExpr::GetValue(holder));
                    let read = self.ir.add_expr(IrExpr::RefGet {
                        holder: h3,
                        elem: elem_ir,
                    });
                    let value = if prefix {
                        read
                    } else {
                        let one2 = self.ir.add_expr(IrExpr::Const(one_c));
                        self.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op: undo,
                            lhs: read,
                            rhs: one2,
                        })
                    };
                    return Some(self.ir.add_expr(IrExpr::Block {
                        stmts: vec![set],
                        value: Some(value),
                    }));
                }
                let (v, ty) = self.lookup(&name)?;
                let op = if dec { IrBinOp::Sub } else { IrBinOp::Add };
                if matches!(ty, Ty::Byte | Ty::Short | Ty::Char) {
                    // `Byte`/`Short`/`Char` narrow on update (wrap in their own width). No temp slot (a
                    // `Variable` inside an operand `Block` trips the verifier in a template/argument
                    // position): the postfix value is `narrow(new ∓ 1)`, which wraps back to the old value
                    // even at the boundary (`Byte` 127++: new = narrow(128) = -128; narrow(-128 - 1) = 127).
                    let narrow = |this: &mut Self, val: u32| {
                        this.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg: val,
                            type_operand: ty_to_ir(ty),
                        })
                    };
                    let widen = |this: &mut Self, val: u32| {
                        this.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg: val,
                            type_operand: ty_to_ir(Ty::Int),
                        })
                    };
                    let cur = self.ir.add_expr(IrExpr::GetValue(v));
                    let cur_i = widen(self, cur);
                    let one = self.ir.add_expr(IrExpr::Const(IrConst::Int(1)));
                    let sum = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op,
                        lhs: cur_i,
                        rhs: one,
                    });
                    let narrowed = narrow(self, sum);
                    let set = self.ir.add_expr(IrExpr::SetValue {
                        var: v,
                        value: narrowed,
                    });
                    let value = if prefix {
                        self.ir.add_expr(IrExpr::GetValue(v))
                    } else {
                        let read = self.ir.add_expr(IrExpr::GetValue(v));
                        let read_i = widen(self, read);
                        let one2 = self.ir.add_expr(IrExpr::Const(IrConst::Int(1)));
                        let undo = if dec { IrBinOp::Add } else { IrBinOp::Sub };
                        let back = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op: undo,
                            lhs: read_i,
                            rhs: one2,
                        });
                        narrow(self, back)
                    };
                    return Some(self.ir.add_expr(IrExpr::Block {
                        stmts: vec![set],
                        value: Some(value),
                    }));
                }
                let one = match ty {
                    Ty::Int => IrConst::Int(1),
                    Ty::Long => IrConst::Long(1),
                    Ty::Double => IrConst::Double(1.0),
                    Ty::Float => IrConst::Float(1.0),
                    _ => return None,
                };
                // i = i ± 1 (no temp: wraparound is consistent for Int/Long/Float/Double)
                let cur = self.ir.add_expr(IrExpr::GetValue(v));
                let one1 = self.ir.add_expr(IrExpr::Const(one.clone()));
                let nv = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                    op,
                    lhs: cur,
                    rhs: one1,
                });
                let set = self.ir.add_expr(IrExpr::SetValue { var: v, value: nv });
                // value: new `i` (prefix), or new `i` ∓ 1 = old `i` (postfix).
                let read = self.ir.add_expr(IrExpr::GetValue(v));
                let value = if prefix {
                    read
                } else {
                    let one2 = self.ir.add_expr(IrExpr::Const(one));
                    let undo = if dec { IrBinOp::Add } else { IrBinOp::Sub };
                    self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: undo,
                        lhs: read,
                        rhs: one2,
                    })
                };
                self.ir.add_expr(IrExpr::Block {
                    stmts: vec![set],
                    value: Some(value),
                })
            }
            Expr::As {
                operand,
                ty,
                nullable,
            } => {
                // A reified type parameter (`x as T` in a `<reified T>` inline body) → the bound type.
                let ty = self.subst_type_ref(&ty);
                // `x as? T` (safe cast): `{ val t = x; if (t is T) t as T else null }` — `instanceof`
                // then `checkcast` on the non-null branch, `null` on a mismatch (never throws). The
                // target must be a reference (a primitive `as? Int` yields the boxed `Int?` wrapper —
                // its `instanceof`/`checkcast` already test/keep the wrapper, per the `TypeOp` backend).
                if nullable {
                    let target = self.ty_ref(&ty)?;
                    let target_ir = ty_to_ir(target);
                    let v = self.expr(operand)?;
                    let ov = self.fresh_value();
                    let oty = ty_to_ir(self.info.ty(operand));
                    let var_t = self.ir.add_expr(IrExpr::Variable {
                        index: ov,
                        ty: oty,
                        init: Some(v),
                    });
                    let g1 = self.ir.add_expr(IrExpr::GetValue(ov));
                    let is_t = self.ir.add_expr(IrExpr::TypeOp {
                        op: IrTypeOp::InstanceOf,
                        arg: g1,
                        type_operand: target_ir.clone(),
                    });
                    let g2 = self.ir.add_expr(IrExpr::GetValue(ov));
                    let cast_t = self.ir.add_expr(IrExpr::TypeOp {
                        op: IrTypeOp::Cast,
                        arg: g2,
                        type_operand: target_ir,
                    });
                    let nullc = self.ir.add_expr(IrExpr::Const(IrConst::Null));
                    let when = self.ir.add_expr(IrExpr::When {
                        branches: vec![(Some(is_t), cast_t), (None, nullc)],
                    });
                    return Some(self.ir.add_expr(IrExpr::Block {
                        stmts: vec![var_t],
                        value: Some(when),
                    }));
                }
                let arg = self.expr(operand)?;
                // A PRIMITIVE operand cast to a reference type (`42 as Any`, `'a' as Char?`, `b as
                // Byte?`) is a BOX: the primitive is boxed to its wrapper (which is-a the target), an
                // `ImplicitCoercion` the JVM backend emits as `valueOf`. Handle it before the
                // reference-target paths below (which assume a reference operand + `checkcast`).
                let operand_ty = self.info.ty(operand);
                let target_is_tparam = self.cur_tparams.iter().any(|(n, _, _)| *n == ty.name);
                if operand_ty.is_primitive() && !operand_ty.is_unsigned() && !target_is_tparam {
                    let target = self.info.ty(e);
                    if target.is_reference() {
                        return Some(self.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg,
                            type_operand: ty_to_ir(target),
                        }));
                    }
                }
                // `x as T` — a cast to a type parameter in scope. The IR keeps `T` (with its bound) as
                // the cast target; the JVM backend erases it (checkcast to the bound, `Object` for an
                // unbounded `T`). A non-null `T` (`<T : Any>`, `<T : Foo>`) null-checks like kotlinc
                // (`CastNonNull`); a nullable target (`as T?`, or an unbounded `<T : Any?>`) does not.
                if let Some((name, bound, non_null)) =
                    self.cur_tparams.iter().find(|(n, _, _)| *n == ty.name)
                {
                    let op = if *non_null && !ty.nullable {
                        IrTypeOp::CastNonNull
                    } else {
                        IrTypeOp::Cast
                    };
                    let type_operand = Ty::ty_param(name, *bound);
                    return Some(self.ir.add_expr(IrExpr::TypeOp {
                        op,
                        arg,
                        type_operand,
                    }));
                }
                // `x as Int` (non-null primitive target) is an unbox: `checkcast Integer; intValue()`,
                // emitted by the `ImplicitCoercion` reference→primitive path. `ty_ref` only yields
                // reference types, so handle the primitive case before it.
                if !ty.nullable {
                    if let Some(prim) =
                        Ty::from_name(&ty.name).filter(|t| t.is_primitive() && !t.is_unsigned())
                    {
                        return Some(self.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::ImplicitCoercion,
                            arg,
                            type_operand: ty_to_ir(prim),
                        }));
                    }
                }
                // `x as Foo?` is a plain `checkcast Foo` (a reference target; `null` passes the
                // checkcast). `ty_ref` rejects any nullable `TypeRef`, so resolve the NON-NULL form —
                // the JVM cast target is the same class either way; only the null-throwing behaviour
                // (selected below via `ty.nullable`) differs.
                let non_null_ty = ast::TypeRef {
                    nullable: false,
                    ..ty.clone()
                };
                let target = self.ty_ref(&non_null_ty)?;
                // A nullable VALUE-class cast (`as Str?`) keeps the boxed wrapper; the value-class pass
                // would unbox a `null` (`Str.unbox-impl()` on null → NPE), so skip rather than miscompile.
                if ty.nullable
                    && target
                        .obj_internal()
                        .is_some_and(|i| self.is_value_class(i))
                {
                    return None;
                }
                let type_operand = ty_to_ir(target);
                // `as T` to a non-null reference type throws on `null` (kotlinc null-checks before the
                // `checkcast`); `as T?` and primitive casts are a plain `checkcast`/coercion.
                let op = if !ty.nullable && target.is_reference() {
                    IrTypeOp::CastNonNull
                } else {
                    IrTypeOp::Cast
                };
                self.ir.add_expr(IrExpr::TypeOp {
                    op,
                    arg,
                    type_operand,
                })
            }
            Expr::Unary { op, operand } => {
                use crate::ast::UnOp;
                let v = self.expr(operand)?;
                match op {
                    // `-x` → `0 - x` (zero typed to match); `!x` → `x == false`.
                    UnOp::Neg => {
                        // A negated `Double`/`Float` literal is the negative *constant* — not `0.0 - lit`,
                        // which yields `+0.0` for `-0.0` (losing the sign IEEE-754 comparisons distinguish,
                        // e.g. `Double.compare(0.0, -0.0) == 1`).
                        match self.afile.expr(operand) {
                            Expr::DoubleLit(d) => {
                                return Some(self.ir.add_expr(IrExpr::Const(IrConst::Double(-d))))
                            }
                            Expr::FloatLit(f) => {
                                return Some(self.ir.add_expr(IrExpr::Const(IrConst::Float(-f))))
                            }
                            _ => {}
                        }
                        // `-x` → `0 - x` with the zero typed to the operand so both Sub operands
                        // share one numeric type (Byte/Short/Char negate in the `int` category).
                        let zero = match self.info.ty(operand) {
                            Ty::Long => self.ir.add_expr(IrExpr::Const(IrConst::Long(0))),
                            Ty::Double => self.ir.add_expr(IrExpr::Const(IrConst::Double(0.0))),
                            Ty::Float => self.ir.add_expr(IrExpr::Const(IrConst::Float(0.0))),
                            _ => self.ir.add_expr(IrExpr::Const(IrConst::Int(0))),
                        };
                        self.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op: IrBinOp::Sub,
                            lhs: zero,
                            rhs: v,
                        })
                    }
                    UnOp::Not => {
                        let f = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
                        self.ir.add_expr(IrExpr::PrimitiveBinOp {
                            op: IrBinOp::Eq,
                            lhs: v,
                            rhs: f,
                        })
                    }
                    // Unary `+` is identity on numerics — emit the operand unchanged. A non-numeric
                    // operand (a user `unaryPlus` operator) isn't modeled → skip the file.
                    UnOp::Plus => {
                        if !self.info.ty(operand).is_numeric() {
                            return None;
                        }
                        v
                    }
                }
            }
            // `when` → `IrWhen`. With a subject, each arm becomes `subject == cond` (OR-ed for
            // multiple conditions); the subject is re-evaluated per comparison (correct for the
            // side-effect-free subjects in the core subset). Without a subject, the conditions are
            // boolean tests directly.
            Expr::When { subject, arms } => {
                // Bail on shapes the flat IR can't emit safely: branches that mix `Unit` with real
                // values (only valid as a discarded statement, indistinguishable here from a value
                // use → inconsistent frames), and a subject `==` comparison that mixes a primitive
                // with a reference (e.g. `when (i: Int) { null -> … }` → bad-typed compare).
                let body_tys: Vec<Ty> = arms.iter().map(|a| self.info.ty(a.body)).collect();
                let any_unit = body_tys.iter().any(|t| *t == Ty::Unit);
                if any_unit && !body_tys.iter().all(|t| *t == Ty::Unit) {
                    return None;
                }
                // An unsigned subject compares its arms with unsigned `==` (bit-equal, same as signed),
                // but a `ULong` subject whose magnitude exceeds `Long.MAX` needs care, and the unsigned
                // const-val arms aren't materialized yet — bail rather than risk a mismatch.
                if subject.map_or(false, |s| self.info.ty(s).is_unsigned()) {
                    return None;
                }
                // A no-`else` `when` used as a *value* is only accepted by the checker when it is
                // exhaustive (every enum entry / both booleans / a sealed hierarchy covered). The flat
                // emitter can't *prove* exhaustiveness, but it can rely on it: the last arm is dropped
                // to the `else` (one arm always matches, so the final one is the catch-all). This is
                // behavior-preserving for an exhaustive `when`.
                let has_else = arms.iter().any(|a| a.conditions.is_empty());
                let make_last_else = !has_else && self.info.ty(e) != Ty::Unit && !arms.is_empty();
                if let Some(subj) = subject {
                    let st = self.info.ty(subj);
                    for arm in &arms {
                        for &c in &arm.conditions {
                            // `is`/`in` conditions are complete boolean tests, not `==` comparands —
                            // their type (`Boolean`) needn't match the subject's primitiveness.
                            if is_when_test(self.afile, c) {
                                continue;
                            }
                            if st.is_primitive() != self.info.ty(c).is_primitive() {
                                return None;
                            }
                        }
                    }
                }
                // Any subject other than a bare `Name` is evaluated ONCE into a temp: a branchy subject
                // (`when (when …)`) so it runs on a clean stack, and a side-effecting one (`when (a++)`,
                // a call) so its effect happens exactly once — never zero times (an empty `when`) nor
                // once per comparison. A bare `Name` stays on the cheap re-evaluate path, which is
                // side-effect-free and also correct for a smart-cast local (whose slot type differs
                // from its static type, and which a temp store would mis-frame).
                let subj_tmp = match subject {
                    Some(subj) if !matches!(self.afile.expr(subj), Expr::Name(_)) => {
                        let sv = self.expr(subj)?;
                        let v = self.fresh_value();
                        let var = self.ir.add_expr(IrExpr::Variable {
                            index: v,
                            ty: ty_to_ir(self.info.ty(subj)),
                            init: Some(sv),
                        });
                        Some((v, var))
                    }
                    _ => None,
                };
                let last = arms.len().saturating_sub(1);
                // Like `if`, a `when` whose result is a reference but whose arm is a primitive must box that
                // arm at the merge so every branch agrees on the (reference) stack type.
                let res = self.info.ty(e);
                let mut branches = Vec::new();
                for (ai, arm) in arms.iter().enumerate() {
                    let body = if res.is_reference() {
                        self.lower_arg(arm.body, &ty_to_ir(res))?
                    } else {
                        self.expr(arm.body)?
                    };
                    if arm.conditions.is_empty() || (make_last_else && ai == last) {
                        branches.push((None, body)); // else (real, or the exhaustive last arm)
                    } else {
                        let mut cond: Option<u32> = None;
                        for &c in &arm.conditions {
                            // An `is`/`!is` or `in`/`!in` condition is already a complete boolean test
                            // involving the subject (the parser embeds it) — use it directly rather than
                            // comparing the subject against it with `==`.
                            let test = if is_when_test(self.afile, c) {
                                self.expr(c)?
                            } else {
                                match (subj_tmp, subject) {
                                    (Some((v, _)), _) => {
                                        let s = self.ir.add_expr(IrExpr::GetValue(v));
                                        let cv = self.expr(c)?;
                                        self.ir.add_expr(IrExpr::PrimitiveBinOp {
                                            op: IrBinOp::Eq,
                                            lhs: s,
                                            rhs: cv,
                                        })
                                    }
                                    (None, Some(subj)) => {
                                        let s = self.expr(subj)?;
                                        let cv = self.expr(c)?;
                                        self.ir.add_expr(IrExpr::PrimitiveBinOp {
                                            op: IrBinOp::Eq,
                                            lhs: s,
                                            rhs: cv,
                                        })
                                    }
                                    (None, None) => self.expr(c)?,
                                }
                            };
                            cond = Some(match cond {
                                Some(prev) => self.ir.add_expr(IrExpr::PrimitiveBinOp {
                                    op: IrBinOp::Or,
                                    lhs: prev,
                                    rhs: test,
                                }),
                                None => test,
                            });
                        }
                        branches.push((cond, body));
                    }
                }
                let when = self.ir.add_expr(IrExpr::When { branches });
                // Prepend the subject-temp declaration (if any) so it's evaluated before the arms.
                match subj_tmp {
                    Some((_, var)) => self.ir.add_expr(IrExpr::Block {
                        stmts: vec![var],
                        value: Some(when),
                    }),
                    None => when,
                }
            }
            Expr::Template(parts) => {
                // Build an ordered list of parts, dropping empty string-literal chunks (kotlinc does),
                // and emit ONE `StringConcat` — the backend turns it into kotlinc's single-StringBuilder
                // (or `String.valueOf` for a lone interpolation) shape, not a `String.plus` chain.
                let mut ir_parts = Vec::new();
                for part in parts {
                    match part {
                        TemplatePart::Str(s) if s.is_empty() => {}
                        TemplatePart::Str(s) => {
                            ir_parts.push(self.ir.add_expr(IrExpr::Const(IrConst::String(s))))
                        }
                        TemplatePart::Expr(e) => {
                            let v = self.expr(e)?;
                            // An unsigned interpolated value prints in unsigned decimal.
                            let ety = self.info.ty(e);
                            let v = if ety.is_unsigned() {
                                self.unsigned_to_string(v, ety)
                            } else {
                                v
                            };
                            ir_parts.push(v);
                        }
                    }
                }
                if ir_parts.is_empty() {
                    ir_parts.push(
                        self.ir
                            .add_expr(IrExpr::Const(IrConst::String(String::new()))),
                    );
                }
                self.ir.add_expr(IrExpr::StringConcat(ir_parts))
            }
            // A receiver-lambda scope function the checker resolved (`x.run { … }`, `with(x) { … }`):
            // inline it generically — bind `this` to the receiver, lower the body — driven by the
            // checker's recorded decision, NOT a backend name-match.
            // A classpath value-class COMPANION call (`Result.success(42)`): load the companion singleton
            // (`getstatic <class>.<field>:L<companion>;`) as the receiver, then an inline-splice of the
            // companion's (instance) `inline` method — `success`'s `this` is the singleton, its param the
            // boxed argument. The splicer drops the unused `this` and inlines the arg, like kotlinc.
            // `suspendCoroutineUninterceptedOrReturn { c -> block }` — a `kotlin.coroutines` inline
            // intrinsic (recognized via the registry). The block runs with the enclosing suspend
            // function's own `Continuation` bound as its parameter; kotlinc inlines the block and returns
            // its `Any?` result. The leaf shape `{ COROUTINE_SUSPENDED }` (and any block that does NOT
            // read its continuation parameter) inlines to just the block body. A block that DOES read the
            // continuation needs the (post-CPS) continuation slot threaded in — not modeled here, so it
            // bails (skip the file) rather than binding a wrong slot.
            Expr::Call { callee, args }
                if args.len() == 1
                    && matches!(self.afile.expr(callee), ast::Expr::Name(n)
                        if self.syms.libraries.coroutine_intrinsic(n)
                            == Some(crate::libraries::CoroutineIntrinsic::SuspendCoroutineUninterceptedOrReturn)) =>
            {
                let ast::Expr::Lambda { params, body } = self.afile.expr(args[0]).clone() else {
                    return None;
                };
                let cont_name = params.first().cloned().unwrap_or_else(|| "it".to_string());
                if name_used_as_value(self.afile, body, &cont_name) {
                    return None; // block reads its continuation — not modeled (skip, never miscompile)
                }
                self.expr(body)?
            }
            Expr::Call { .. } if self.info.companion_calls.contains_key(&e) => {
                let cf = self.info.companion_calls[&e].clone();
                let args = match self.afile.expr(e).clone() {
                    Expr::Call { args, .. } => args,
                    _ => return None,
                };
                let recv = self.ir.add_expr(IrExpr::ExternalStaticField {
                    owner: cf.class_internal.clone(),
                    name: cf.companion_field.clone(),
                    descriptor: format!("L{};", cf.companion_internal),
                });
                // A value-class companion fn's params are erased reference types (`success(Object)`,
                // `failure(Throwable)`) — target `Object` so a primitive argument is boxed (`Integer.
                // valueOf`), matching kotlinc; a reference argument passes through unchanged.
                let obj_ty = Ty::nullable(Ty::obj("kotlin/Any"));
                let mut ir_args = Vec::with_capacity(args.len());
                for &a in &args {
                    ir_args.push(self.lower_arg(a, &obj_ty)?);
                }
                Some(self.ir.add_expr(IrExpr::Call {
                    callee: Callee::Static {
                        owner: cf.companion_internal.clone(),
                        name: cf.jvm_name.clone(),
                        descriptor: cf.descriptor.clone(),
                        inline: true,
                        must_inline: true,
                    },
                    dispatch_receiver: Some(recv),
                    args: ir_args,
                }))?
            }
            Expr::Call { .. } if self.info.receiver_lambdas.contains_key(&e) => {
                let rl = self.info.receiver_lambdas[&e];
                self.lower_receiver_lambda(rl)?
            }
            // A call with a spread argument (`foo(*a)`). Only the single-spread-to-a-top-level-vararg
            // form is handled (the array is passed through via `Arrays.copyOf`, like kotlinc); ANY other
            // shape (mixed spreads, fixed args, member/library callee, primitive element, complex spread
            // expr) returns `None` → the file skips, never miscompiles. The guard ensures a spread arg
            // never reaches the normal vararg-packing paths below.
            Expr::Call { callee, args } if args.iter().any(|&a| self.afile.is_spread_arg(a)) => {
                self.lower_single_spread_call(callee, &args)?
            }
            Expr::Call { callee, args } => match self.afile.expr(callee).clone() {
                // Local top-level function, or constructor `C(args)`.
                Expr::Name(fname) => {
                    // NAMED-ARGUMENT call to a CLASSPATH top-level function (`foo(b = …, a = …)`): reorder
                    // the arguments into parameter order (from the callee's `@Metadata` names) so the
                    // positional lowering below sees them positionally. Same-file/module functions have
                    // their own named-arg handling and are skipped here (this fires only for a name that is
                    // neither a local nor module-declared). `None` → leave the args untouched.
                    let args = if self.afile.call_arg_names.contains_key(&e.0)
                        && self.lookup(&fname).is_none()
                        && !self.module_declares(&fname)
                    {
                        self.reorder_classpath_named_args(e, &fname, &args)
                            .unwrap_or(args)
                    } else {
                        args
                    };
                    // Reified free function `serializer<T>()` (kotlinx.serialization.serializer): a
                    // `reified inline` that can't be called directly (throws at runtime) — desugar to
                    // `T.serializer()` for a `@Serializable` T, the way kotlinc's inliner does.
                    if fname == "serializer"
                        && args.is_empty()
                        && self.lookup(&fname).is_none()
                        && !self.module_declares(&fname)
                    {
                        if let Some(c) = self
                            .afile
                            .call_type_args
                            .get(&e.0)
                            .and_then(|ts| ts.first())
                            .and_then(|tr| self.ty_ref(tr))
                            .and_then(|targ| self.serializable_internal(targ))
                        {
                            return Some(self.serializer_crossfile(&c));
                        }
                    }
                    // No-receiver `run { … }` (the stdlib `inline fun <R> run(block: () -> R): R =
                    // block()`): inline the lambda body directly as the value. The receiver scope
                    // functions (`x.let`/`with(x)`) are intercepted similarly; without this, no-receiver
                    // `run` falls to the bytecode splicer, which bails on a branchy body (`run { if … }`).
                    if fname == "run"
                        && args.len() == 1
                        && self.lookup(&fname).is_none()
                        && !self.module_declares(&fname)
                    {
                        if let Expr::Lambda { params, body } = self.afile.expr(args[0]).clone() {
                            if params.is_empty()
                                && !body_has_labeled_return(self.afile, body, "run")
                            {
                                return self.expr(body);
                            }
                        }
                    }
                    // A call to a lifted local function — the checker mapped this call to its decl.
                    // Prepend the captured outer locals (the enclosing scope holds each captured var's
                    // value, or its `Ref` holder when boxed), then the declared arguments.
                    if let Some(&stmt_id) = self.info.local_call_map.get(&e) {
                        if let Some(&fid) = self.local_fun_ids.get(&stmt_id) {
                            let params = self.ir.functions[fid as usize].params.clone();
                            let caps = self
                                .info
                                .local_fun_captures
                                .get(&stmt_id)
                                .cloned()
                                .unwrap_or_default();
                            if args.len() + caps.len() == params.len() {
                                let mut a = Vec::new();
                                for (name, _) in &caps {
                                    let (cv, _) = self.lookup(name)?;
                                    a.push(self.ir.add_expr(IrExpr::GetValue(cv)));
                                }
                                for (arg, pt) in args.iter().zip(&params[caps.len()..]) {
                                    a.push(self.lower_arg(*arg, pt)?);
                                }
                                return Some(self.ir.add_expr(IrExpr::Call {
                                    callee: Callee::Local(fid),
                                    dispatch_receiver: None,
                                    args: a,
                                }));
                            }
                        }
                    }
                    // A call `param(args)` where `param` is a lambda parameter of the `inline fun`
                    // currently being expanded: inline the passed lambda's body in place.
                    if self.lookup(&fname).is_none() {
                        if let Some(idx) =
                            self.inline_lambdas.iter().rposition(|(n, ..)| *n == fname)
                        {
                            return self.lower_inline_lambda_invoke(idx, &args);
                        }
                    }
                    // A user-defined `inline fun foo(...)` — expand it here (kotlinc's inliner): bind its
                    // value parameters to the evaluated arguments, register its lambda arguments, and
                    // lower its body so a lambda capturing a mutable local works (no closure).
                    if self.lookup(&fname).is_none()
                        && self
                            .syms
                            .funs
                            .get(&fname)
                            .map_or(false, |v| v.iter().any(|s| s.is_inline))
                    {
                        return self.lower_inline_fn_call(&fname, &args, e.0, None);
                    }
                    // SAM conversion `Pred { lambda }` — a functional interface built from a lambda;
                    // lower the lambda as a `LambdaMetafactory` instance targeting the interface's
                    // single abstract method (instead of `FunctionN.invoke`).
                    if args.len() == 1
                        && self.lookup(&fname).is_none()
                        && matches!(self.afile.expr(args[0]), Expr::Lambda { .. })
                    {
                        if let Some(internal) = self.info.ty(e).obj_internal() {
                            // A file interface (its single method), or a classpath functional interface
                            // (`Runnable`, …) — its single abstract method from the library set.
                            let target = self
                                .classes
                                .get(internal)
                                .filter(|ci| {
                                    self.ir.classes[ci.id as usize].is_interface
                                        && self.ir.classes[ci.id as usize].methods.len() == 1
                                })
                                .map(|ci| {
                                    let f = &self.ir.functions
                                        [self.ir.classes[ci.id as usize].methods[0] as usize];
                                    (f.name.clone(), f.ret == ty_to_ir(Ty::Unit))
                                })
                                .or_else(|| {
                                    self.syms
                                        .libraries
                                        .sam_method(internal)
                                        .map(|m| (m.name, m.ret == Ty::Unit))
                                });
                            if let Some((method, void)) = target {
                                let iface = internal.to_string();
                                if let Expr::Lambda { params, body } =
                                    self.afile.expr(args[0]).clone()
                                {
                                    return self.lower_lambda_sam(
                                        args[0],
                                        &params,
                                        body,
                                        Some((iface, method, void)),
                                    );
                                }
                            }
                        }
                    }
                    // `f(args)` where `f` is a field/property of the enclosing class (not a local value or
                    // a top-level function) — invoking a function value through a field isn't modeled;
                    // bail rather than miscompile (it would emit a bogus constructor call).
                    if self.lookup(&fname).is_none() && !self.module_declares(&fname) {
                        if let Some(cur) = self.cur_class.clone() {
                            if self
                                .classes
                                .get(&cur)
                                .map_or(false, |ci| ci.fields.iter().any(|(n, _)| *n == fname))
                            {
                                return None;
                            }
                        }
                    }
                    // `f(args)` where `f` is a function-typed local/parameter → invoke through the
                    // `kotlin/jvm/functions/FunctionN.invoke` interface method (args boxed to Object,
                    // the Object result cast/unboxed to the function's return type).
                    if let Some((v, Ty::Fun(sig))) = self.lookup(&fname) {
                        if sig.params.len() != args.len() {
                            return None;
                        }
                        let func = self.ir.add_expr(IrExpr::GetValue(v));
                        let mut a = Vec::new();
                        for arg in &args {
                            a.push(self.expr(*arg)?);
                        }
                        let ret = ty_to_ir(sig.ret);
                        return Some(self.ir.add_expr(IrExpr::InvokeFunction {
                            func,
                            args: a,
                            ret,
                        }));
                    }
                    // `x(args)` where `x` is a TOP-LEVEL property of function type (`val x = ::foo; x()`):
                    // read the property (the facade getter / cross-file read), then invoke it through
                    // `FunctionN.invoke`. Locals are handled above; this is the not-a-local case.
                    if self.lookup(&fname).is_none() {
                        if let Some(Ty::Fun(sig)) = self.syms.props.get(&fname).map(|p| p.0) {
                            if sig.params.len() == args.len() {
                                let func = self.expr(callee)?;
                                let mut a = Vec::new();
                                for arg in &args {
                                    a.push(self.expr(*arg)?);
                                }
                                return Some(self.ir.add_expr(IrExpr::InvokeFunction {
                                    func,
                                    args: a,
                                    ret: ty_to_ir(sig.ret),
                                }));
                            }
                        }
                    }
                    // The array creators (`arrayOf`/`intArrayOf`/…/`IntArray(n)`/`Array(n){}`) are
                    // compiler synthetics — the synthetic registry (priority over the classpath)
                    // supplies their IR body directly. Honor user shadowing first: a user-defined `fun
                    // arrayOf` (or a local of that name) wins, exactly as in kotlinc.
                    let array_intrinsic_ok =
                        self.lookup(&fname).is_none() && !self.module_declares(&fname);
                    if array_intrinsic_ok {
                        if let Some(syn) = crate::synthetics::lookup(&fname) {
                            let call = crate::synthetics::SynthCall {
                                args: &args,
                                call: e,
                            };
                            if let Some(r) = (syn.body)(syn, self, &call) {
                                return Some(r);
                            }
                        }
                    }
                    if let Some((fi, fid)) = {
                        // Select the overload (matching the arg types) through the current module as a
                        // `SymbolSource` (ModuleSymbols), then resolve its method id. Only a function
                        // defined in THIS file (present in `fun_ids`) is handled here; a cross-file
                        // function (in `funs` but not `fun_ids`) falls through to the facade branch below.
                        let arg_tys: Vec<Ty> = args.iter().map(|&a| self.info.ty(a)).collect();
                        crate::module_symbols::ModuleSymbols::new(self.syms)
                            .resolve_top_level(&fname, &arg_tys)
                            .and_then(|fi| {
                                // `erased_params_key` == the params' descriptors concatenated.
                                let key: String =
                                    fi.callable.params.iter().map(|t| t.descriptor()).collect();
                                self.fun_ids
                                    .get(&(fname.clone(), key))
                                    .copied()
                                    .map(|fid| (fi, fid))
                            })
                    } {
                        // A `vararg` function: pack the trailing arguments into a fresh array for the
                        // last (array) parameter. (Spread `*arr` and a branchy element are unsupported.)
                        if fi.call_sig.vararg {
                            let params = self.ir.functions[fid as usize].params.clone();
                            let fixed = params.len() - 1;
                            if args.len() < fixed {
                                return None;
                            }
                            let elem_ty = fi.callable.params[fixed].array_elem()?;
                            let elem_ir = ty_to_ir(elem_ty);
                            let mut a = Vec::new();
                            for (i, &arg) in args.iter().take(fixed).enumerate() {
                                a.push(self.lower_arg(arg, &params[i])?);
                            }
                            // For a GENERIC vararg (`vararg z: T`) the erased element type is `Any`, so a
                            // primitive argument would box to its own wrapper (`-1` → `Integer`). When the
                            // call supplies a primitive type argument (`mk<Long>(-1)`), coerce each element
                            // to that primitive first (`Int`→`Long`), then box — matching kotlinc.
                            let targ_prim = if elem_ty == Ty::obj("kotlin/Any") {
                                self.afile
                                    .call_type_args
                                    .get(&e.0)
                                    .and_then(|ts| ts.first())
                                    .map(|r| ty_of(self.afile, r))
                                    .filter(|t| t.is_primitive())
                            } else {
                                None
                            };
                            let mut elements = Vec::new();
                            for &arg in &args[fixed..] {
                                if is_branchy(self.afile, arg) {
                                    return None;
                                }
                                if let Some(p) = targ_prim {
                                    let v = self.lower_arg(arg, &ty_to_ir(p))?;
                                    elements.push(self.ir.add_expr(IrExpr::TypeOp {
                                        op: IrTypeOp::ImplicitCoercion,
                                        arg: v,
                                        type_operand: elem_ir.clone(),
                                    }));
                                } else {
                                    elements.push(self.lower_arg(arg, &elem_ir)?);
                                }
                            }
                            let arr = self.ir.add_expr(IrExpr::Vararg {
                                element_type: elem_ir,
                                elements,
                            });
                            a.push(arr);
                            return Some(self.ir.add_expr(IrExpr::Call {
                                callee: Callee::Local(fid),
                                dispatch_receiver: None,
                                args: a,
                            }));
                        }
                        let params = self.ir.functions[fid as usize].params.clone();
                        // Omitted trailing args are filled from constant-literal defaults.
                        let meta: Vec<(String, Option<AstExprId>)> = self
                            .top_fun_decl(&fname)
                            .map(|f| {
                                f.params
                                    .iter()
                                    .map(|p| (p.name.clone(), p.default))
                                    .collect()
                            })
                            .unwrap_or_default();
                        if self.erased_generic_call_unmodeled(e, &fname) {
                            return None;
                        }
                        let a = self.lower_args_defaulted(e, &meta, &args, &params)?;
                        let call = self.ir.add_expr(IrExpr::Call {
                            callee: Callee::Local(fid),
                            dispatch_receiver: None,
                            args: a,
                        });
                        let ret = self.ir.functions[fid as usize].ret.clone();
                        self.coerce_erased_call_result(e, call, &ret)
                    } else if let Some(facade) = self.syms.fn_facades.get(&fname).cloned() {
                        // A top-level function defined in ANOTHER file of this multi-file compilation →
                        // a cross-facade `invokestatic`. Only the simple exact-arity case (no vararg /
                        // omitted defaults) is modeled here; anything else bails (skips the file).
                        let arg_tys: Vec<Ty> = args.iter().map(|&a| self.info.ty(a)).collect();
                        let fi = crate::module_symbols::ModuleSymbols::new(self.syms)
                            .resolve_top_level(&fname, &arg_tys)?;
                        let plen = fi.callable.params.len();
                        if fi.call_sig.vararg || fi.call_sig.required != plen || args.len() != plen
                        {
                            return None;
                        }
                        let mut a = Vec::new();
                        for (arg, pt) in args.iter().zip(&fi.callable.params) {
                            a.push(self.lower_arg(*arg, &ty_to_ir(*pt))?);
                        }
                        let call = self.ir.add_expr(IrExpr::Call {
                            callee: Callee::CrossFile {
                                facade,
                                name: fname.clone(),
                                params: fi.callable.params.iter().map(|t| ty_to_ir(*t)).collect(),
                                ret: ty_to_ir(fi.callable.ret),
                            },
                            dispatch_receiver: None,
                            args: a,
                        });
                        // A cross-file `suspend fun` call: record it so the coroutine pass threads the
                        // continuation (the callee, in another file, is absent from this file's
                        // `suspend_funs`). The logical return type is the resolved callable's.
                        if fi.flags.suspend {
                            self.ir
                                .suspend_calls
                                .insert(call, ty_to_ir(fi.callable.ret));
                        }
                        call
                    } else if let Some(r) = {
                        // Inside a receiver lambda / extension-fn body (`cur_class` cleared, `this` is the
                        // external receiver), an unqualified call resolves against the implicit `this`
                        // receiver — a member or a stdlib extension — BEFORE a receiver-less top-level
                        // function, matching Kotlin's scoping (`"ab".run { reversed() }` is
                        // `this.reversed()`, not the top-level `reversed`).
                        if self.cur_class.is_none()
                            && self.lookup(&fname).is_none()
                            && !self.module_declares(&fname)
                        {
                            self.lookup("this").and_then(|(this_v, this_ty)| {
                                self.lower_this_member_call(this_v, this_ty, &fname, &args, e)
                            })
                        } else {
                            None
                        }
                    } {
                        r
                    } else if let Some(c) = {
                        // A receiver-less top-level library function (`listOf(…)`) → `invokestatic
                        // facade.name(args)`. Resolved (vararg-aware) through the library set, so no
                        // stdlib facade or descriptor is hardcoded.
                        let arg_tys: Vec<Ty> = args.iter().map(|&a| self.info.ty(a)).collect();
                        // Forward the call's explicit type arguments (`listOf<Long>(…)`), as the checker
                        // does — they bind the generic vararg's element type for literal adaptation below.
                        let call_targs: Vec<Ty> = self
                            .afile
                            .call_type_args
                            .get(&e.0)
                            .map(|ts| {
                                ts.iter()
                                    .map(|r| {
                                        crate::types::Ty::from_name(&r.name)
                                            .filter(|_| !r.nullable)
                                            .or_else(|| self.ty_ref(r))
                                            .unwrap_or(Ty::Error)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        self.syms
                            .libraries
                            .resolve_callable(&fname, None, &arg_tys, &call_targs)
                    } {
                        // For a spliced top-level `inline fun` (`run { 2 + 3 }`), the body returns the
                        // ERASED `Object` (a generic `R`); coerce it to the logical type so a primitive
                        // result unboxes instead of landing boxed in a primitive slot.
                        let (call_inline, call_log, call_phys) =
                            (c.is_inline, c.ret, c.physical_ret);
                        // Is the callee a `suspend fun`? Ask the resolver (the flag flows uniformly from
                        // the AST for a module/sibling-file fn and from `@Metadata` for a classpath one).
                        // A suspend call is recorded by `ExprId` so the coroutine pass threads the
                        // continuation even when the callee lives in another compilation unit (absent from
                        // this file's `suspend_funs`).
                        let call_suspend = self.resolver().toplevel_is_suspend(&fname);
                        // A sub-`Int` primitive type argument (`listOf<Short>(1, 2)`) erases its
                        // element to `Object`, so a wider literal would box as `Integer` and a later
                        // narrowing read (`map(::shortFoo)`) throws `ClassCastException`. kotlinc boxes
                        // the constant as the narrow type; krusty doesn't track that logical-vs-erased
                        // element type yet, so bail (skip) rather than miscompile.
                        let narrow_targ = self.afile.call_type_args.get(&e.0).map_or(false, |ts| {
                            ts.iter()
                                .any(|r| !r.nullable && matches!(r.name.as_str(), "Short" | "Byte"))
                        });
                        if narrow_targ
                            && args
                                .iter()
                                .any(|&a| matches!(self.info.ty(a), Ty::Int | Ty::Long | Ty::Char))
                        {
                            return None;
                        }
                        // krusty's `Ty` erases `byte`/`short` to `Int`, but a resolved overload's
                        // descriptor keeps `B`/`S` — so for a `byte`/`short` parameter the lowering builds
                        // an `int`/`int[]` that mismatches the callee's `B`/`[B` descriptor (a verify error).
                        // This happens when the precise `Int` overload is private `@InlineOnly` and the
                        // public `Byte` one is mis-selected (`maxOf(3, 7)`). Bail (skip) rather than
                        // miscompile — a genuine `byte`-parameter library call is rare and skips safely.
                        if descriptor_has_byte_or_short_param(&c.descriptor) {
                            return None;
                        }
                        let last_is_array =
                            c.params.last().map_or(false, |p| p.array_elem().is_some());
                        let vararg = !c.params.is_empty()
                            && last_is_array
                            && (c.params.len() != args.len()
                                || self.info.ty(args[args.len() - 1]) != *c.params.last().unwrap());
                        let mut a = Vec::new();
                        if vararg {
                            let fixed = c.params.len() - 1;
                            if args.len() < fixed {
                                return None;
                            }
                            for i in 0..fixed {
                                a.push(self.lower_arg(args[i], &ty_to_ir(c.params[i]))?);
                            }
                            let elem_ir = ty_to_ir(c.params[fixed].array_elem()?);
                            let bound = c.vararg_elem;
                            let mut elements = Vec::new();
                            for &arg in &args[fixed..] {
                                if is_branchy(self.afile, arg) {
                                    return None;
                                }
                                // Literal adaptation: an integer literal whose bound element type is `Long`
                                // (`listOf<Long>(3)`) is the `Long` constant `3L`, boxed as `Long` — kotlinc
                                // adapts the literal at compile time (no runtime `i2l`, and a non-literal
                                // `Int` is a kotlinc error here, so only constant literals adapt).
                                if bound == Some(Ty::Long) {
                                    if let Expr::IntLit(v) = *self.afile.expr(arg) {
                                        let lc = self.ir.add_expr(IrExpr::Const(IrConst::Long(v)));
                                        elements.push(self.ir.add_expr(IrExpr::TypeOp {
                                            op: IrTypeOp::ImplicitCoercion,
                                            arg: lc,
                                            type_operand: elem_ir.clone(),
                                        }));
                                        continue;
                                    }
                                }
                                elements.push(self.lower_arg(arg, &elem_ir)?);
                            }
                            a.push(self.ir.add_expr(IrExpr::Vararg {
                                element_type: elem_ir,
                                elements,
                            }));
                        } else if c.default_call {
                            // A `name$default` call (`assertEquals(a, b)` omits the `message` default):
                            // lower the provided prefix, then append a placeholder per omitted trailing
                            // parameter, an `int` bit-mask (a bit per omitted param), and a `null` marker.
                            // A generic function whose provided parameters share one type variable
                            // (`assertEquals(expected: T, actual: T)`) boxes each argument as its OWN
                            // primitive; mismatched primitives (`assertEquals(0, longVal)`) would compare
                            // `areEqual(Integer, Long)` = false (kotlinc unifies `T` and coerces the
                            // literal, which krusty doesn't model) — skip rather than miscompile.
                            let prim_args: Vec<Ty> = args
                                .iter()
                                .map(|&a| self.info.ty(a))
                                .filter(|t| t.is_primitive())
                                .collect();
                            let generic_provided = c.params.iter().take(args.len()).all(|p| {
                                matches!(
                                    p.obj_internal(),
                                    Some("kotlin/Any") | Some("java/lang/Object")
                                )
                            });
                            if generic_provided && prim_args.windows(2).any(|w| w[0] != w[1]) {
                                return None;
                            }
                            for (i, &arg) in args.iter().enumerate() {
                                a.push(self.lower_arg(arg, &ty_to_ir(c.params[i]))?);
                            }
                            for j in args.len()..c.params.len() {
                                let ph = self.zero_placeholder(c.params[j]);
                                a.push(ph);
                            }
                            let mask: i32 = (args.len()..c.params.len()).map(|j| 1i32 << j).sum();
                            a.push(self.ir.add_expr(IrExpr::Const(IrConst::Int(mask))));
                            a.push(self.ir.add_expr(IrExpr::Const(IrConst::Null)));
                        } else {
                            if c.params.len() != args.len() {
                                return None;
                            }
                            for (i, &arg) in args.iter().enumerate() {
                                a.push(self.lower_arg(arg, &ty_to_ir(c.params[i]))?);
                            }
                        }
                        let call = self.ir.add_expr(IrExpr::Call {
                            callee: Callee::Static {
                                owner: c.owner,
                                name: c.name,
                                descriptor: c.descriptor,
                                inline: c.is_inline,
                                must_inline: c.must_inline,
                            },
                            dispatch_receiver: None,
                            args: a,
                        });
                        if call_suspend {
                            self.ir.suspend_calls.insert(call, ty_to_ir(call_log));
                        }
                        // A spliced inline fn leaves its erased return on the stack — coerce to the logical
                        // type (unbox/checkcast). A no-op when they match (the non-generic common case).
                        if call_inline {
                            self.coerce_erased(call, call_log, call_phys)
                        } else {
                            call
                        }
                    } else if let Some((class, index, mfid, _)) = self
                        .cur_class
                        .clone()
                        .and_then(|cur| self.resolve_method(&cur, &fname))
                    {
                        // Unqualified instance method call inside a class body: `foo()` → `this.foo()`.
                        let params = self.ir.functions[mfid as usize].params.clone();
                        if args.len() != params.len() {
                            return None;
                        }
                        let this = self.ir.add_expr(IrExpr::GetValue(0));
                        let mut a = Vec::new();
                        for (arg, pt) in args.iter().zip(&params) {
                            a.push(self.lower_arg(*arg, pt)?);
                        }
                        self.ir.add_expr(IrExpr::MethodCall {
                            class,
                            index,
                            receiver: this,
                            args: a.into_iter().map(Some).collect(),
                        })
                    } else if let Some((class, index, mfid, cur_id)) =
                        self.inner_outer_method(&fname)
                    {
                        // Unqualified call to an enclosing method from an inner class: `this.this$0.foo()`.
                        let params = self.ir.functions[mfid as usize].params.clone();
                        if args.len() != params.len() {
                            return None;
                        }
                        let this = self.ir.add_expr(IrExpr::GetValue(0));
                        let this0 = self.ir.add_expr(IrExpr::GetField {
                            receiver: this,
                            class: cur_id,
                            index: 0,
                        });
                        let mut a = Vec::new();
                        for (arg, pt) in args.iter().zip(&params) {
                            a.push(self.lower_arg(*arg, pt)?);
                        }
                        self.ir.add_expr(IrExpr::MethodCall {
                            class,
                            index,
                            receiver: this0,
                            args: a.into_iter().map(Some).collect(),
                        })
                    } else if let Some(internal) = self
                        .info
                        .ty(e)
                        .obj_internal()
                        .filter(|i| !self.classes.contains_key(*i))
                    {
                        // Constructing a classpath (non-IR) class — `RuntimeException("x")`,
                        // `StringBuilder()`. The constructor descriptor comes from the classpath.
                        return self.lower_external_new(internal, &args);
                    } else {
                        // Constructor: the call's result type is the class.
                        let ci = self.class_of(self.info.ty(e))?;
                        let class = ci.id;
                        // Constructing an annotation (`A(args)`) builds its synthetic IMPL class — the
                        // annotation INTERFACE itself can't be `new`'d. Redirect to `<A>$annotationImpl`
                        // (same fields/ctor); the result still types as the annotation (the impl IS-A `A`).
                        let class = if self.ir.classes[class as usize].is_annotation {
                            let impl_name = format!(
                                "{}$annotationImpl",
                                self.ir.classes[class as usize].fq_name
                            );
                            self.ir
                                .classes
                                .iter()
                                .position(|c| c.fq_name == impl_name)
                                .map(|p| p as u32)
                                .unwrap_or(class)
                        } else {
                            class
                        };
                        // The IR models only an exact positional match against the primary
                        // constructor's parameter fields. Default arguments and secondary
                        // constructors aren't lowered — bail (skip) rather than emit a call whose
                        // stack shape won't match the constructor descriptor (a VerifyError).
                        let ctor_count = self.ir.classes[class as usize].ctor_param_count as usize;
                        // Coerce each argument to its constructor-parameter type, filling named args +
                        // constant-literal defaults (`LongWrapper(2)`, `C(y = 1)`, `C()`). Use the FULL
                        // ctor-param list (`ctor_args`, property + plain params) when present, else the
                        // leading parameter fields (synthesized classes have empty `ctor_args`).
                        let ctor_args = self.ir.classes[class as usize].ctor_args.clone();
                        let field_tys: Vec<Ty> = if ctor_args.is_empty() {
                            self.ir.classes[class as usize].fields[..ctor_count]
                                .iter()
                                .map(|f| f.ty.clone())
                                .collect()
                        } else {
                            ctor_args.iter().map(|(t, _)| t.clone()).collect()
                        };
                        // Generic constructor with a primitive type argument (`Box<Long>(-1)`): a
                        // type-parameter field gets its argument coerced to the type-argument primitive
                        // (`Int`→`Long`) before boxing — else an `Int` literal boxes as `Integer`, not
                        // `Long`. Only the simple all-property positional case.
                        // A NULLABLE type-param field (`val z: T?`) stays boxed (`Int?`) — only coerce a
                        // non-nullable one (`val value: T`); use an empty (non-matching) name for nullable.
                        let (tparams, prop_tys): (Vec<String>, Vec<String>) = self
                            .class_decl(&fname)
                            .map(|cd| {
                                (
                                    cd.type_params.clone(),
                                    cd.props
                                        .iter()
                                        .filter(|p| p.is_property)
                                        .map(|p| {
                                            if p.ty.nullable {
                                                String::new()
                                            } else {
                                                p.ty.name.clone()
                                            }
                                        })
                                        .collect(),
                                )
                            })
                            .unwrap_or_default();
                        let targs: Vec<Ty> = self
                            .afile
                            .call_type_args
                            .get(&e.0)
                            .map(|ts| ts.iter().map(|r| ty_of(self.afile, r)).collect())
                            .unwrap_or_default();
                        let no_named = self
                            .afile
                            .call_arg_names
                            .get(&e.0)
                            .map_or(true, |ns| ns.iter().all(|n| n.is_none()));
                        let arg_prim = |i: usize| {
                            prop_tys
                                .get(i)
                                .and_then(|pn| tparams.iter().position(|tp| tp == pn))
                                .and_then(|ti| targs.get(ti))
                                .copied()
                                .filter(|t| t.is_primitive())
                        };
                        if !targs.is_empty()
                            && no_named
                            && args.len() == field_tys.len()
                            && prop_tys.len() == field_tys.len()
                            && (0..args.len()).any(|i| arg_prim(i).is_some())
                        {
                            let mut a = Vec::new();
                            for (i, &arg) in args.iter().enumerate() {
                                if let Some(p) = arg_prim(i) {
                                    let v = self.lower_arg(arg, &ty_to_ir(p))?;
                                    a.push(self.ir.add_expr(IrExpr::TypeOp {
                                        op: IrTypeOp::ImplicitCoercion,
                                        arg: v,
                                        type_operand: field_tys[i].clone(),
                                    }));
                                } else {
                                    a.push(self.lower_arg(arg, &field_tys[i])?);
                                }
                            }
                            return Some(self.ir.add_expr(IrExpr::New {
                                class,
                                args: a,
                                ctor_params: None,
                            }));
                        }
                        let meta: Vec<(String, Option<AstExprId>)> = self
                            .class_decl(&fname)
                            .map(|cd| {
                                cd.props
                                    .iter()
                                    .map(|p| (p.name.clone(), p.default))
                                    .collect()
                            })
                            .unwrap_or_default();
                        // A secondary constructor whose parameter types MATCH the arguments is preferred
                        // over a lenient primary match (`Sc("x")` is the `String` secondary, not the
                        // `Int` primary coerced). Compare the argument IR types to each secondary's.
                        let arg_irs: Vec<Ty> =
                            args.iter().map(|a| ty_to_ir(self.info.ty(*a))).collect();
                        let secs = self.ir.classes[class as usize].secondary_ctors.clone();
                        // Whether the PRIMARY constructor can accept the args (each assignable to a field
                        // type). When it can't (`IC("abc")`: a `String` isn't assignable to `List<T>`),
                        // a secondary whose params accept the args is chosen instead of leniently coercing
                        // into the primary — otherwise the call would target `constructor-impl(List)` with
                        // a `String` and fail at runtime.
                        let primary_accepts = arg_irs.len() == field_tys.len()
                            && arg_irs
                                .iter()
                                .zip(&field_tys)
                                .all(|(a, p)| ir_arg_assignable(a, p));
                        // A named-argument call (`C(b = 9)`) references the PRIMARY ctor's parameter
                        // names — Kotlin never considers a secondary for it. Only positional calls pick
                        // a secondary by argument types (otherwise a same-arity secondary that merely
                        // coincides on types would hijack the named primary call → wrong fields).
                        let typed_secondary = no_named
                            .then(|| {
                                secs.iter()
                                    .find(|sc| sc.params == arg_irs)
                                    .or_else(|| {
                                        (!primary_accepts)
                                            .then(|| {
                                                secs.iter().find(|sc| {
                                                    sc.params.len() == arg_irs.len()
                                                        && arg_irs
                                                            .iter()
                                                            .zip(&sc.params)
                                                            .all(|(a, p)| ir_arg_assignable(a, p))
                                                })
                                            })
                                            .flatten()
                                    })
                                    .cloned()
                            })
                            .flatten();
                        // The primary constructor (exact/defaulted positional match), else a secondary
                        // constructor selected by argument count.
                        if let Some(sc) = typed_secondary {
                            let mut a = Vec::new();
                            for (arg, pt) in args.iter().zip(&sc.params) {
                                a.push(self.lower_arg(*arg, pt)?);
                            }
                            self.ir.add_expr(IrExpr::New {
                                class,
                                args: a,
                                ctor_params: Some(sc.params),
                            })
                        } else if let Some(a) =
                            self.lower_args_defaulted(e, &meta, &args, &field_tys)
                        {
                            self.ir.add_expr(IrExpr::New {
                                class,
                                args: a,
                                ctor_params: None,
                            })
                        } else if let Some(sc) = no_named
                            .then(|| {
                                self.ir.classes[class as usize]
                                    .secondary_ctors
                                    .clone()
                                    .into_iter()
                                    .find(|sc| sc.params.len() == args.len())
                            })
                            .flatten()
                        {
                            let mut a = Vec::new();
                            for (arg, pt) in args.iter().zip(&sc.params) {
                                a.push(self.lower_arg(*arg, pt)?);
                            }
                            self.ir.add_expr(IrExpr::New {
                                class,
                                args: a,
                                ctor_params: Some(sc.params),
                            })
                        } else {
                            return None;
                        }
                    }
                }
                // Instance method call `recv.m(args)`, or a stdlib intrinsic method.
                Expr::Member { receiver, name } => {
                    // NAMED-ARGUMENT call to a CLASSPATH instance member (`g.greet(b = …, a = …)`):
                    // reorder the arguments into parameter order (from the member's `@Metadata` names) so
                    // the positional lowering below pairs each argument with its parameter. `None` → leave
                    // the args untouched (module members keep their own named-arg handling).
                    let args = if self.afile.call_arg_names.contains_key(&e.0) {
                        let rt = self.info.ty(receiver);
                        self.reorder_classpath_named_member_args(e, rt, &name, &args)
                            .unwrap_or(args)
                    } else {
                        args
                    };
                    // `recv.startCoroutine(completion)` — a `kotlin.coroutines` extension intrinsic
                    // (recognized via the registry). The suspend-function receiver + completion are passed
                    // to `invokestatic ContinuationKt.startCoroutine(Function1, Continuation)V`.
                    if args.len() == 1
                        && matches!(self.info.ty(receiver), Ty::Fun(s) if s.suspend)
                        && self.syms.libraries.coroutine_intrinsic(&name)
                            == Some(crate::libraries::CoroutineIntrinsic::StartCoroutine)
                    {
                        let recv_v = self.expr(receiver)?;
                        let cont_ir = ty_to_ir(Ty::obj("kotlin/coroutines/Continuation"));
                        let comp_v = self.lower_arg(args[0], &cont_ir)?;
                        return Some(self.ir.add_expr(IrExpr::Call {
                            callee: Callee::Static {
                                owner: "kotlin/coroutines/ContinuationKt".to_string(),
                                name: "startCoroutine".to_string(),
                                descriptor:
                                    "(Lkotlin/jvm/functions/Function1;Lkotlin/coroutines/Continuation;)V"
                                        .to_string(),
                                inline: false,
                                must_inline: false,
                            },
                            dispatch_receiver: None,
                            args: vec![recv_v, comp_v],
                        }));
                    }
                    // `super.method(args)` → a non-virtual `invokespecial` on `this` (value 0) to the base
                    // class's method (the receiver's own override is skipped). The base is the current
                    // class's superclass; the method's signature comes from the super (a user class via
                    // `method_of`, else a classpath class via `resolve_instance`).
                    if matches!(self.afile.expr(receiver), Expr::Name(rn) if rn == "super") {
                        let cur = self.cur_class.clone()?;
                        let sup = self
                            .classes
                            .get(&cur)
                            .and_then(|ci| ci.super_internal.clone())?;
                        let this = self.ir.add_expr(IrExpr::GetValue(0));
                        let arg_tys: Vec<Ty> = args.iter().map(|&a| self.info.ty(a)).collect();
                        let (params, descriptor) =
                            if let Some(sig) = self.syms.method_of(&sup, &name) {
                                (
                                    sig.params.clone(),
                                    crate::jvm::names::method_descriptor(&sig.params, sig.ret),
                                )
                            } else if let Some(m) = crate::call_resolver::resolve_instance(
                                &*self.syms.libraries,
                                &sup,
                                &name,
                                &arg_tys,
                            ) {
                                (m.params.clone(), m.descriptor.clone())
                            } else {
                                return None;
                            };
                        if params.len() != args.len() {
                            return None;
                        }
                        let mut a = Vec::new();
                        for (arg, pt) in args.iter().zip(&params) {
                            a.push(self.lower_arg(*arg, &ty_to_ir(*pt))?);
                        }
                        return Some(self.ir.add_expr(IrExpr::Call {
                            callee: Callee::Special {
                                owner: sup,
                                name: name.clone(),
                                descriptor,
                            },
                            dispatch_receiver: Some(this),
                            args: a,
                        }));
                    }
                    // Reified kotlinx.serialization round-trip: `fmt.encodeToString(x)` /
                    // `fmt.decodeFromString<C>(s)` are `reified inline` (uncallable directly) — desugar to
                    // the 2-arg member with a synthesized `C.serializer()`.
                    if let Some(call) = self.try_reified_serial(receiver, &name, &args, e) {
                        return Some(call);
                    }
                    // An arithmetic operator member called by name on a primitive numeric receiver
                    // (`a.plus(b)` ≡ `a + b`) → the same `PrimitiveBinOp` lowering as the operator form.
                    if args.len() == 1 && self.info.ty(receiver).is_primitive() {
                        if let Some(r) = self.lower_prim_op_method(receiver, &name, args[0]) {
                            return Some(r);
                        }
                    }
                    // Array `isEmpty()`/`isNotEmpty()`/`count()` (stdlib extensions) → the `arraylength`
                    // intrinsic: `size == 0` / `size != 0` / `size`.
                    if self.info.ty(receiver).array_elem().is_some() && args.is_empty() {
                        let cmp = match name.as_str() {
                            "isEmpty" => Some(Some(IrBinOp::Eq)),
                            "isNotEmpty" => Some(Some(IrBinOp::Ne)),
                            "count" => Some(None),
                            _ => None,
                        };
                        if let Some(op) = cmp {
                            let a = self.expr(receiver)?;
                            let size = self.ir.add_expr(IrExpr::Call {
                                callee: Callee::External("kotlin/Array.size".to_string()),
                                dispatch_receiver: Some(a),
                                args: vec![],
                            });
                            return Some(match op {
                                Some(c) => {
                                    let z = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                                    self.ir.add_expr(IrExpr::PrimitiveBinOp {
                                        op: c,
                                        lhs: size,
                                        rhs: z,
                                    })
                                }
                                None => size,
                            });
                        }
                    }
                    // Inner-class construction `outerInstance.Inner(args)` → `new Outer$Inner(outer,
                    // args)`: the checker typed the call as the inner class (whose first field is the
                    // synthetic `this$0`), so pass the receiver as the leading constructor argument.
                    if let Some(class_id) = self
                        .info
                        .ty(e)
                        .obj_internal()
                        .and_then(|i| self.classes.get(i))
                        .map(|ci| ci.id)
                        .filter(|&id| {
                            let c = &self.ir.classes[id as usize];
                            // The inner's `this$0` field type must match the receiver's type (the outer
                            // instance) — guards against a same-named method returning an inner-typed value.
                            let this0_outer = match c.fields.first() {
                                Some(IrField { name: n0, ty, .. }) if n0 == "this$0" => {
                                    ty.non_null().obj_internal()
                                }
                                _ => None,
                            };
                            c.fq_name.ends_with(&format!("${name}"))
                                && this0_outer == self.info.ty(receiver).obj_internal()
                        })
                    {
                        let field_tys: Vec<Ty> = self.ir.classes[class_id as usize]
                            .ctor_args
                            .iter()
                            .map(|(t, _)| t.clone())
                            .collect();
                        if field_tys.len() == args.len() + 1 {
                            let recv = self.expr(receiver)?;
                            let mut a = vec![recv];
                            for (arg, pt) in args.iter().zip(&field_tys[1..]) {
                                a.push(self.lower_arg(*arg, pt)?);
                            }
                            return Some(self.ir.add_expr(IrExpr::New {
                                class: class_id,
                                args: a,
                                ctor_params: None,
                            }));
                        }
                    }
                    // `iterable.forEach { x -> body }` is the stdlib `inline fun` whose body is
                    // `for (x in this) body` — inline it to a for-each loop (no closure), so a mutable
                    // capture in the lambda works, exactly as kotlinc's inlining does. Gated on the
                    // receiver being iterable (so a user `forEach` on a non-iterable falls through).
                    if name == "forEach" && args.len() == 1 {
                        if let Expr::Lambda {
                            params,
                            body: lbody,
                        } = self.afile.expr(args[0]).clone()
                        {
                            let rty = self.info.ty(receiver);
                            // An array, a `String`, or an `Obj` iterable (List/Set/Iterable) — all handled
                            // by `lower_for_each` (and the checker element-types the lambda parameter).
                            let iterable = rty.array_elem().is_some()
                                || rty == Ty::String
                                || rty.obj_internal().map_or(false, |i| {
                                    range_counted_elem(i).is_some()
                                        || crate::call_resolver::resolve_instance(
                                            &*self.syms.libraries,
                                            i,
                                            "iterator",
                                            &[],
                                        )
                                        .is_some()
                                        || self
                                            .syms
                                            .libraries
                                            .resolve_callable("iterator", Some(rty), &[], &[])
                                            .is_some()
                                });
                            if iterable {
                                let param =
                                    params.first().cloned().unwrap_or_else(|| "it".to_string());
                                return self.lower_for_each(&param, receiver, lbody, None);
                            }
                        }
                    }
                    // `iterable.forEachIndexed { i, x -> body }` — the inline `forEachIndexed`, whose
                    // body is `var i = 0; for (x in this) { action(i, x); i++ }`. Inline it via the
                    // iterator path with an index counter (Obj iterables only, same as `forEach`).
                    if name == "forEachIndexed" && args.len() == 1 {
                        if let Expr::Lambda {
                            params,
                            body: lbody,
                        } = self.afile.expr(args[0]).clone()
                        {
                            let rty = self.info.ty(receiver);
                            let iterable = rty.obj_internal().map_or(false, |i| {
                                crate::call_resolver::resolve_instance(
                                    &*self.syms.libraries,
                                    i,
                                    "iterator",
                                    &[],
                                )
                                .is_some()
                                    || self
                                        .syms
                                        .libraries
                                        .resolve_callable("iterator", Some(rty), &[], &[])
                                        .is_some()
                            });
                            if iterable && params.len() == 2 {
                                let idx = params[0].clone();
                                let elem = params[1].clone();
                                return self.lower_foreach_iterator(
                                    &elem,
                                    receiver,
                                    lbody,
                                    rty,
                                    Some(&idx),
                                    None,
                                );
                            }
                        }
                    }
                    // Metadata-driven inline route: any library `inline fun` taking a single lambda whose
                    // body the platform can splice (`let`/`also`/…) is inlined from its REAL stdlib
                    // bytecode — no per-function desugar, no hardcoded name list. The route self-gates on
                    // the resolved callee's `is_inline` + spliceability, so non-spliceable inline fns
                    // (`map`/`filter`, branchy) and user methods simply fall through.
                    // `run`/`apply` are receiver lambdas (the lambda's `this` is the receiver); the
                    // bytecode splice routes them as ordinary value-lambdas, mishandling that receiver, so
                    // they go to the receiver-aware fallback below instead.
                    if args.len() == 1
                        && !matches!(name.as_str(), "run" | "apply")
                        && matches!(self.afile.expr(args[0]), Expr::Lambda { .. })
                    {
                        if let Some(call) = self.try_route_lambda_inline(
                            &name,
                            receiver,
                            args[0],
                            self.info.ty(receiver),
                        ) {
                            return Some(call);
                        }
                    }
                    // FALLBACK for the cases the route can't splice — a lambda capturing `this`/fields (no
                    // closure form, so no `IrExpr::Lambda` to inline); it inlines the body directly.
                    // (Removing it costs ~13 box tests until this-capturing lambdas are modelled; the
                    // common closure-form cases already inline from real bytecode via the route above.)
                    // `let`/`also` bind the receiver to the lambda's value parameter (`it`); `run`/`apply`
                    // are receiver lambdas — the receiver is `this`, so member access in the body resolves
                    // against its class (bind `this` and set `cur_class` to the receiver's user class; a
                    // library receiver, whose members krusty can't reach through a bare `this`, falls
                    // through). `let`/`run` yield the body value; `also`/`apply` yield the receiver.
                    let is_recv_lambda = matches!(name.as_str(), "run" | "apply");
                    let scope_fn = matches!(name.as_str(), "let" | "also") || is_recv_lambda;
                    if scope_fn && args.len() == 1 {
                        if let Expr::Lambda {
                            params,
                            body: lbody,
                        } = self.afile.expr(args[0]).clone()
                        {
                            let rty = self.info.ty(receiver);
                            // A receiver lambda binds the receiver as `this`; member access in the body
                            // resolves against its type through the implicit-`this` paths (own class
                            // field/getter, or a builtin/library accessor for `String`/collections/…).
                            let recv = self.expr(receiver)?;
                            let depth = self.scope.len();
                            let p_slot = self.fresh_value();
                            let pname = if is_recv_lambda {
                                "this".to_string()
                            } else {
                                params.first().cloned().unwrap_or_else(|| "it".to_string())
                            };
                            // An inlined receiver lambda runs in the *caller's* method, so the receiver's
                            // members are accessed externally (getter/setter), never as the enclosing
                            // class's own private fields — clear `cur_class` for the body.
                            let saved_cur = self.cur_class.clone();
                            if is_recv_lambda {
                                self.cur_class = None;
                            }
                            self.scope.push((pname, p_slot, rty));
                            let var_p = self.ir.add_expr(IrExpr::Variable {
                                index: p_slot,
                                ty: ty_to_ir(rty),
                                init: Some(recv),
                            });
                            let body_val = self.expr(lbody);
                            self.scope.truncate(depth);
                            self.cur_class = saved_cur;
                            let body_val = body_val?;
                            let returns_receiver = matches!(name.as_str(), "also" | "apply");
                            let result = if !returns_receiver {
                                self.ir.add_expr(IrExpr::Block {
                                    stmts: vec![var_p],
                                    value: Some(body_val),
                                })
                            } else {
                                let recv_read = self.ir.add_expr(IrExpr::GetValue(p_slot));
                                self.ir.add_expr(IrExpr::Block {
                                    stmts: vec![var_p, body_val],
                                    value: Some(recv_read),
                                })
                            };
                            return Some(result);
                        }
                    }
                    // Nested-class construction `Outer.Inner(args)` — the receiver is a class name and
                    // the call's result type is the nested class. Emit `new Outer$Inner(args)`.
                    if let Expr::Name(root) = self.afile.expr(receiver).clone() {
                        if self.lookup(&root).is_none() {
                            let qname = format!("{root}.{name}");
                            if let Some(ci) = self.classes.get(&class_internal(self.afile, &qname))
                            {
                                let class = ci.id;
                                let ctor_count =
                                    self.ir.classes[class as usize].ctor_param_count as usize;
                                let field_tys: Vec<Ty> = self.ir.classes[class as usize].fields
                                    [..ctor_count]
                                    .iter()
                                    .map(|f| f.ty.clone())
                                    .collect();
                                let meta: Vec<(String, Option<AstExprId>)> = self
                                    .class_decl(&qname)
                                    .map(|cd| {
                                        cd.props
                                            .iter()
                                            .map(|p| (p.name.clone(), p.default))
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                if let Some(a) =
                                    self.lower_args_defaulted(e, &meta, &args, &field_tys)
                                {
                                    return Some(self.ir.add_expr(IrExpr::New {
                                        class,
                                        args: a,
                                        ctor_params: None,
                                    }));
                                }
                                return None;
                            }
                        }
                    }
                    // `C.serializer()` on a `@Serializable` class — the serialization plugin synthesizes
                    // a `static serializer(): KSerializer<C>` on `C`, but only at the BACKEND phase
                    // (after this lowering). Emit the call BY SIGNATURE now (`invokestatic C.serializer
                    // ()…`, via the backend-agnostic CrossFile callee); the plugin supplies the method
                    // body before emit. Scoped to the plugin's synthetic static so it can't shadow a
                    // companion method (which lowers via the `C$Companion` instance, not a static).
                    if name == "serializer" {
                        if let Expr::Name(cls) = self.afile.expr(receiver).clone() {
                            let is_serializable = self.class_decl(&cls).is_some_and(|cd| {
                                // Same simple-name detection as the checker + the plugin.
                                cd.annotations
                                    .iter()
                                    .any(|a| a.rsplit(['/', '.']).next() == Some("Serializable"))
                            });
                            if is_serializable && self.lookup(&cls).is_none() {
                                if let Some(internal) = self
                                    .classes
                                    .get(&class_internal(self.afile, &cls))
                                    .map(|ci| self.ir.classes[ci.id as usize].fq_name.clone())
                                {
                                    let ret = ty_to_ir(Ty::obj_args(
                                        "kotlinx/serialization/KSerializer",
                                        &[Ty::obj(&internal)],
                                    ));
                                    // A generic `C<T…>` takes one `KSerializer` argument per type
                                    // parameter (`C.serializer(KSerializer<T0>, …)`); a non-generic class
                                    // takes none. Each argument lowers to an erased `KSerializer`.
                                    let kser =
                                        ty_to_ir(Ty::obj("kotlinx/serialization/KSerializer"));
                                    let mut a = Vec::new();
                                    for arg in &args {
                                        a.push(self.lower_arg(*arg, &kser)?);
                                    }
                                    return Some(self.ir.add_expr(IrExpr::Call {
                                        callee: Callee::CrossFile {
                                            facade: internal,
                                            name: "serializer".to_string(),
                                            params: vec![kser; args.len()],
                                            ret,
                                        },
                                        dispatch_receiver: None,
                                        args: a,
                                    }));
                                }
                            }
                        }
                    }
                    // A user `inline fun <recv>.name(args)` — expand it here (kotlinc's inliner) with the
                    // receiver bound as `this`, instead of a real static call.
                    {
                        let recv_desc = self.recv_ty(receiver).descriptor();
                        let is_inline_ext = self.afile.decls.iter().any(|&d| {
                            matches!(self.afile.decl(d), Decl::Fun(f)
                                if f.name == name && f.is_inline
                                && f.receiver.as_ref().is_some_and(|r|
                                    f.type_params.iter().any(|tp| tp == &r.name)
                                    || ty_of(self.afile, r).descriptor() == recv_desc))
                        });
                        if is_inline_ext {
                            if let Some(r) =
                                self.lower_inline_fn_call(&name, &args, e.0, Some(receiver))
                            {
                                return Some(r);
                            }
                        }
                    }
                    // A top-level extension function `recv.name(args)` → a static call whose first
                    // argument is the receiver (matching how the extension was registered/emitted).
                    {
                        let recv_desc = self.recv_ty(receiver).descriptor();
                        if let Some(&fid) = self.ext_fun_ids.get(&(recv_desc, name.clone())) {
                            let params = self.ir.functions[fid as usize].params.clone();
                            if params.len() == args.len() + 1 {
                                let recv = self.lower_arg(receiver, &params[0])?;
                                let mut a = vec![recv];
                                for (arg, pt) in args.iter().zip(&params[1..]) {
                                    a.push(self.lower_arg(*arg, pt)?);
                                }
                                return Some(self.ir.add_expr(IrExpr::Call {
                                    callee: Callee::Local(fid),
                                    dispatch_receiver: None,
                                    args: a,
                                }));
                            }
                        }
                    }
                    // Unsigned conversions. `UInt`/`Int` and `ULong`/`Long` share a JVM representation,
                    // so a conversion that doesn't change the representation is a no-op reinterpret;
                    // `UInt.toLong()`/`toULong()` zero-extend (`Integer.toUnsignedLong`, NOT the
                    // sign-extending `i2l`); `ULong.toInt()` truncates (`l2i`); `inc`/`dec` are ±1.
                    {
                        let rty = self.info.ty(receiver);
                        if args.is_empty()
                            && (rty.is_unsigned() || matches!(name.as_str(), "toUInt" | "toULong"))
                        {
                            let repr = |t: Ty| t.unsigned_repr().unwrap_or(t);
                            if rty.is_unsigned() && name == "toString" {
                                let r = self.expr(receiver)?;
                                return Some(self.unsigned_to_string(r, rty));
                            }
                            if rty.is_unsigned() && matches!(name.as_str(), "inc" | "dec") {
                                let one = if rty == Ty::ULong {
                                    IrConst::Long(1)
                                } else {
                                    IrConst::Int(1)
                                };
                                let r = self.expr(receiver)?;
                                let o = self.ir.add_expr(IrExpr::Const(one));
                                let op = if name == "dec" {
                                    IrBinOp::Sub
                                } else {
                                    IrBinOp::Add
                                };
                                return Some(self.ir.add_expr(IrExpr::PrimitiveBinOp {
                                    op,
                                    lhs: r,
                                    rhs: o,
                                }));
                            }
                            if let Some(target) = crate::resolve::conversion_target(&name) {
                                let r = self.expr(receiver)?;
                                if rty == Ty::UInt && matches!(target, Ty::Long | Ty::ULong) {
                                    // zero-extend the 32-bit unsigned value into a long
                                    return Some(self.ir.add_expr(IrExpr::Call {
                                        callee: Callee::Static {
                                            owner: "java/lang/Integer".to_string(),
                                            name: "toUnsignedLong".to_string(),
                                            descriptor: "(I)J".to_string(),
                                            inline: false,
                                            must_inline: false,
                                        },
                                        dispatch_receiver: None,
                                        args: vec![r],
                                    }));
                                }
                                if repr(rty) == repr(target) {
                                    return Some(r); // identity reinterpret (UInt↔Int, ULong↔Long, UInt→UInt)
                                }
                                if repr(rty).is_primitive() && repr(target).is_primitive() {
                                    return Some(self.ir.add_expr(IrExpr::TypeOp {
                                        op: IrTypeOp::ImplicitCoercion,
                                        arg: r,
                                        type_operand: ty_to_ir(repr(target)),
                                    }));
                                }
                            }
                            // Any other unsigned conversion (e.g. unsigned→float) isn't modeled — bail.
                            return None;
                        }
                    }
                    // Primitive numeric/`Char` conversions (`n.toLong()`, `c.toInt()`, `i.toChar()`, …)
                    // are coercions — the backend emits the `i2l`/`l2i`/`i2c`/… opcode.
                    {
                        let rty = self.info.ty(receiver);
                        if matches!(
                            rty,
                            Ty::Int
                                | Ty::Long
                                | Ty::Byte
                                | Ty::Short
                                | Ty::Char
                                | Ty::Double
                                | Ty::Float
                        ) {
                            if let Some(target) = crate::resolve::conversion_target(&name) {
                                if args.is_empty() {
                                    let r = self.expr(receiver)?;
                                    return Some(self.ir.add_expr(IrExpr::TypeOp {
                                        op: IrTypeOp::ImplicitCoercion,
                                        arg: r,
                                        type_operand: ty_to_ir(target),
                                    }));
                                }
                            }
                        }
                    }
                    // `a.compareTo(b)` on numeric primitives → `{Integer,Long,Float,Double}.compare(a, b)`
                    // (returns -1/0/1), after promoting both operands to their common type — so a mixed
                    // comparison like `1.compareTo(1.1)` becomes `Double.compare(1.0, 1.1)`. `Byte`/`Short`/
                    // `Char` compare in the `int` category (`Integer.compare`). A user `operator compareTo`
                    // has a reference receiver and is handled elsewhere; this is the builtin intrinsic.
                    {
                        let rty = self.info.ty(receiver);
                        if name == "compareTo" && args.len() == 1 && rty.is_primitive() {
                            let at = self.info.ty(args[0]);
                            if let Some(p) = Ty::promote(rty, at)
                                .filter(|p| p.is_primitive() && *p != Ty::Boolean)
                            {
                                let pir = ty_to_ir(p);
                                let l = self.lower_arg(receiver, &pir)?;
                                let r = self.lower_arg(args[0], &pir)?;
                                let (owner, prim) = match p {
                                    Ty::Long => ("java/lang/Long", "J"),
                                    Ty::Float => ("java/lang/Float", "F"),
                                    Ty::Double => ("java/lang/Double", "D"),
                                    _ => ("java/lang/Integer", "I"), // Int/Byte/Short/Char compare as int
                                };
                                return Some(self.ir.add_expr(IrExpr::Call {
                                    callee: Callee::Static {
                                        owner: owner.to_string(),
                                        name: "compare".to_string(),
                                        descriptor: format!("({prim}{prim})I"),
                                        inline: false,
                                        must_inline: false,
                                    },
                                    dispatch_receiver: None,
                                    args: vec![l, r],
                                }));
                            }
                        }
                    }
                    // `Int`/`Long` bitwise/shift members are stdlib functions the compiler treats as
                    // intrinsics (kotlinc maps `Int.and` → `iand`, etc.). This is an ordinary method
                    // call `a.and(b)` — `a and b` is just its infix spelling, already desugared by the
                    // parser — so both spellings land here. Recognize them by (receiver type, name).
                    {
                        let rty = self.info.ty(receiver);
                        if matches!(rty, Ty::Int | Ty::Long) {
                            let shift = matches!(name.as_str(), "shl" | "shr" | "ushr");
                            let bop = match name.as_str() {
                                "and" => Some(IrBinOp::BitAnd),
                                "or" => Some(IrBinOp::BitOr),
                                "xor" => Some(IrBinOp::BitXor),
                                "shl" => Some(IrBinOp::Shl),
                                "shr" => Some(IrBinOp::Shr),
                                "ushr" => Some(IrBinOp::Ushr),
                                _ => None,
                            };
                            if let (Some(op), 1) = (bop, args.len()) {
                                let l = self.expr(receiver)?;
                                let rt = if shift { Ty::Int } else { rty };
                                let r = self.lower_arg(args[0], &ty_to_ir(rt))?;
                                return Some(self.ir.add_expr(IrExpr::PrimitiveBinOp {
                                    op,
                                    lhs: l,
                                    rhs: r,
                                }));
                            }
                            if name == "inv" && args.is_empty() {
                                let l = self.expr(receiver)?;
                                let neg1 = self.ir.add_expr(IrExpr::Const(if rty == Ty::Long {
                                    IrConst::Long(-1)
                                } else {
                                    IrConst::Int(-1)
                                }));
                                return Some(self.ir.add_expr(IrExpr::PrimitiveBinOp {
                                    op: IrBinOp::BitXor,
                                    lhs: l,
                                    rhs: neg1,
                                }));
                            }
                        }
                    }
                    // `EnumClass.values()` / `EnumClass.valueOf(s)` — static enum methods.
                    if let Expr::Name(rn) = self.afile.expr(receiver).clone() {
                        let internal = class_internal(self.afile, &rn);
                        if let Some(ci) = self.classes.get(&internal) {
                            let cls = ci.id;
                            if !self.ir.classes[cls as usize].enum_entries.is_empty() {
                                if name == "values" && args.is_empty() {
                                    return Some(
                                        self.ir.add_expr(IrExpr::EnumValues { class: cls }),
                                    );
                                }
                                if name == "valueOf" && args.len() == 1 {
                                    let a = self.expr(args[0])?;
                                    return Some(
                                        self.ir
                                            .add_expr(IrExpr::EnumValueOf { class: cls, arg: a }),
                                    );
                                }
                            }
                        }
                    }
                    // `C.foo(args)` — a companion-object method → `getstatic C.Companion; invokevirtual`.
                    if let Expr::Name(rn) = self.afile.expr(receiver).clone() {
                        let internal = class_internal(self.afile, &rn);
                        if let Some(comp_fq) = self.companions.get(&internal).cloned() {
                            if let Some((class, index, fid, _)) =
                                self.resolve_method(&comp_fq, &name)
                            {
                                let params = self.ir.functions[fid as usize].params.clone();
                                if args.len() != params.len() {
                                    return None;
                                }
                                let outer_id = self.classes[&internal].id;
                                let comp_id = self.classes[&comp_fq].id;
                                let recv = self.ir.add_expr(IrExpr::StaticInstance {
                                    owner: outer_id,
                                    ty: comp_id,
                                    field: "Companion",
                                });
                                let mut a = Vec::new();
                                for (arg, pt) in args.iter().zip(&params) {
                                    a.push(self.lower_arg(*arg, pt)?);
                                }
                                return Some(self.ir.add_expr(IrExpr::MethodCall {
                                    class,
                                    index,
                                    receiver: recv,
                                    args: a.into_iter().map(Some).collect(),
                                }));
                            }
                        }
                    }
                    // A call to a method with parameter defaults, possibly with named/omitted args. Map
                    // each provided argument to its parameter position; omitted positions stay `None` (a
                    // call with holes). The backend fills the holes (JVM: `$default` stub + mask).
                    if let Some(internal) = self
                        .class_of(self.recv_ty(receiver))
                        .map(|ci| ci.internal.clone())
                    {
                        if let Some((class, index, fid, _)) = self
                            .resolve_method(&internal, &name)
                            .filter(|(_, _, fid, _)| {
                                // Prefer the override unless it lacks defaults yet the call omits args —
                                // then the default is inherited from a super-interface (resolved below).
                                self.ir.fn_param_defaults.contains_key(fid)
                                    || args.len() == self.ir.functions[*fid as usize].params.len()
                            })
                            .or_else(|| self.resolve_defaulted_iface_method(&internal, &name))
                        {
                            if self.ir.fn_param_defaults.contains_key(&fid) {
                                let params = self.ir.functions[fid as usize].params.clone();
                                let n = params.len();
                                let param_names = self
                                    .ir
                                    .fn_param_names
                                    .get(&fid)
                                    .cloned()
                                    .unwrap_or_default();
                                let names = self.afile.call_arg_names.get(&e.0).cloned();
                                let mut provided: Vec<Option<u32>> = vec![None; n];
                                let mut next_pos = 0usize;
                                let mut ok = param_names.len() == n;
                                for (ai, arg) in args.iter().enumerate() {
                                    let nm =
                                        names.as_ref().and_then(|v| v.get(ai).cloned().flatten());
                                    let pos = match nm {
                                        Some(s) => param_names.iter().position(|f| *f == s),
                                        None => {
                                            let p = next_pos;
                                            next_pos += 1;
                                            Some(p)
                                        }
                                    };
                                    match pos {
                                        Some(p) if p < n => {
                                            let l = self.lower_arg(*arg, &params[p])?;
                                            provided[p] = Some(l);
                                        }
                                        _ => {
                                            ok = false;
                                            break;
                                        }
                                    }
                                }
                                if ok {
                                    let recv = self.expr(receiver)?;
                                    return Some(self.ir.add_expr(IrExpr::MethodCall {
                                        class,
                                        index,
                                        receiver: recv,
                                        args: provided,
                                    }));
                                }
                            }
                        }
                    }
                    let rt = self.recv_ty(receiver);
                    if let Some((class, index, fid, _)) = self
                        .class_of(rt)
                        .map(|ci| ci.internal.clone())
                        .and_then(|i| self.resolve_method(&i, &name))
                    {
                        // The method may be inherited — `class` is the *owning* class. Virtual dispatch
                        // (`invokevirtual`) still reaches an override on the receiver's actual class.
                        let params = self.ir.functions[fid as usize].params.clone();
                        // Default/omitted arguments aren't modeled (would underflow the descriptor).
                        if args.len() != params.len() {
                            return None;
                        }
                        let recv = self.expr(receiver)?;
                        // Coerce each argument to its parameter type (numeric widening, boxing, …).
                        let mut a = Vec::new();
                        for (arg, pt) in args.iter().zip(&params) {
                            a.push(self.lower_arg(*arg, pt)?);
                        }
                        self.ir.add_expr(IrExpr::MethodCall {
                            class,
                            index,
                            receiver: recv,
                            args: a.into_iter().map(Some).collect(),
                        })
                    } else if let Some((owner, sig_params, sig_ret, interface)) = match &rt {
                        // An instance method on a class defined in ANOTHER file → `CrossFileVirtual`
                        // (own methods only; inherited/defaulted/vararg cross-file calls bail).
                        Ty::Obj(i, _) if self.class_of(rt).is_none() => self
                            .syms
                            .class_by_internal(i)
                            .filter(|cs| cs.value_field.is_none())
                            .and_then(|cs| {
                                cs.methods
                                    .get(&name)
                                    .filter(|s| !s.vararg && s.required == s.params.len())
                                    .map(|s| {
                                        (i.to_string(), s.params.clone(), s.ret, cs.is_interface)
                                    })
                            }),
                        _ => None,
                    } {
                        if args.len() != sig_params.len() {
                            return None;
                        }
                        let recv = self.expr(receiver)?;
                        let mut a = Vec::new();
                        for (arg, pt) in args.iter().zip(&sig_params) {
                            a.push(self.lower_arg(*arg, &ty_to_ir(*pt))?);
                        }
                        self.ir.add_expr(IrExpr::Call {
                            callee: Callee::CrossFileVirtual {
                                owner,
                                name: name.clone(),
                                params: sig_params.iter().map(|t| ty_to_ir(*t)).collect(),
                                ret: ty_to_ir(sig_ret),
                                interface,
                            },
                            dispatch_receiver: Some(recv),
                            args: a,
                        })
                    } else if name == "toString" && args.is_empty() {
                        // `x.toString()` → stdlib intrinsic, `String`.
                        let recv = self.expr(receiver)?;
                        self.ir.add_expr(IrExpr::Call {
                            callee: Callee::External("kotlin/Any.toString".to_string()),
                            dispatch_receiver: Some(recv),
                            args: vec![],
                        })
                    } else if name == "hashCode" && args.is_empty() {
                        // `x.hashCode()` → the `Any.hashCode` virtual (dispatches to any override),
                        // not a by-index member — so it needs no class-side method table entry.
                        let recv = self.expr(receiver)?;
                        self.ir.add_expr(IrExpr::Call {
                            callee: Callee::External("kotlin/Any.hashCode".to_string()),
                            dispatch_receiver: Some(recv),
                            args: vec![],
                        })
                    } else if let Some((internal, desc, is_iface, mparams, mret)) = {
                        // A classpath *instance* method `recv.name(args)` → `invokevirtual`/
                        // `invokeinterface recvType.name:descriptor` (descriptor from the classpath; no
                        // hardcoded names). Enables stdlib member calls (iterators, collections, …).
                        let arg_tys: Vec<Ty> = args.iter().map(|&a| self.info.ty(a)).collect();
                        self.class_of(rt)
                            .map(|ci| ci.internal.clone())
                            .or_else(|| {
                                if let Ty::Obj(i, _) = rt {
                                    Some(i.to_string())
                                } else {
                                    None
                                }
                            })
                            // A `String` receiver resolves its `java.lang.String` members (`isEmpty()`,
                            // `isBlank()`, …) — a member wins over a same-named extension, as in kotlinc
                            // (and a private `@InlineOnly` extension like `StringsKt.isEmpty` can't be called).
                            .or_else(|| {
                                if rt == Ty::String {
                                    Some("java/lang/String".to_string())
                                } else {
                                    None
                                }
                            })
                            .and_then(|internal| {
                                crate::call_resolver::resolve_instance(
                                    &*self.syms.libraries,
                                    &internal,
                                    &name,
                                    &arg_tys,
                                )
                                .map(|m| {
                                    let is_iface = self
                                        .syms
                                        .libraries
                                        .resolve_type(&internal)
                                        .map_or(false, |t| t.is_interface());
                                    (internal, m.descriptor, is_iface, m.params, m.ret)
                                })
                            })
                    } {
                        let recv = self.expr(receiver)?;
                        // The receiver's PRIMITIVE element type (`ArrayList<Byte>` → `Byte`,
                        // `ArrayList<Long>` → `Long`). `coll.add(0)` must box the value as THAT wrapper
                        // (`Byte`/`Long`/…), not the literal's own `Integer` — else iterating the element
                        // (`checkcast Byte`/`Long`) throws `ClassCastException`. Coerced only when the
                        // argument's primitive type actually differs from the element (below).
                        let elem_prim = if let Ty::Obj(_, targs) = &rt {
                            targs.first().copied().filter(|t| t.is_primitive())
                        } else {
                            None
                        };
                        // Coerce each argument to the resolved parameter type so a primitive flowing into
                        // an erased `Any` parameter (`List<Int>.add(E)` → `add(Object)`) autoboxes.
                        let mut a = Vec::new();
                        for (i, &arg) in args.iter().enumerate() {
                            match mparams.get(i) {
                                Some(p)
                                    if matches!(
                                        p.obj_internal(),
                                        Some("kotlin/Any") | Some("java/lang/Object")
                                    ) && elem_prim.is_some_and(|e| {
                                        let a = self.info.ty(arg);
                                        a.is_primitive() && a != e
                                    }) =>
                                {
                                    // Coerce to the element primitive (i2b/i2l/…), then box as THAT
                                    // wrapper for the erased `Object` parameter.
                                    let e = elem_prim.unwrap();
                                    let v = self.lower_arg(arg, &ty_to_ir(e))?;
                                    a.push(self.ir.add_expr(IrExpr::TypeOp {
                                        op: IrTypeOp::ImplicitCoercion,
                                        arg: v,
                                        type_operand: ty_to_ir(*p),
                                    }));
                                }
                                Some(p) => a.push(self.lower_arg(arg, &ty_to_ir(*p))?),
                                None => a.push(self.expr(arg)?),
                            }
                        }
                        let call = self.ir.add_expr(IrExpr::Call {
                            callee: Callee::Virtual {
                                owner: internal,
                                name: name.clone(),
                                descriptor: desc,
                                interface: is_iface,
                            },
                            dispatch_receiver: Some(recv),
                            args: a,
                        });
                        // A generic member whose erased return is `Object` but whose substituted type is
                        // more specific (`List<Int>.get` → `Int`) gets the unbox/checkcast kotlinc emits.
                        self.coerce_generic_read(call, e, mret)
                    } else if let Some(c) = {
                        // A library-resolved extension `recv.name(args)` → `invokestatic
                        // facade.name(recv, args)`. Owner + descriptor come from the library
                        // (`resolve_callable` with the receiver), so no stdlib name is hardcoded here.
                        let arg_tys: Vec<Ty> = args.iter().map(|&a| self.info.ty(a)).collect();
                        self.resolve_ext_lit_widened(&name, rt, &args, &arg_tys)
                    } {
                        // Coerce the receiver + arguments to the extension's parameter types so a
                        // primitive flowing into a generic `Object` parameter (`fun <T> T.to(…)`) boxes.
                        let recv =
                            self.lower_arg(receiver, &ty_to_ir(*c.params.first().unwrap_or(&rt)))?;
                        let mut a = vec![recv];
                        // A `$default` call with a TRAILING LAMBDA: the lambda fills the LAST real parameter
                        // (`transform`), the leading args a prefix, the MIDDLE parameters default. Place the
                        // lambda in the last slot, zero-placeholders for the defaulted middle, and a mask
                        // with a bit set for each defaulted middle parameter (not the prefix, not the lambda).
                        let trailing_lambda = c.default_call
                            && args
                                .last()
                                .is_some_and(|&x| matches!(self.info.ty(x), Ty::Fun(_)));
                        if trailing_lambda {
                            let real_count = c.params.len() - 1; // exclude the receiver
                            let prefix_len = args.len() - 1;
                            let last = real_count - 1;
                            for j in 0..real_count {
                                let pj = ty_to_ir(c.params[j + 1]);
                                if j < prefix_len {
                                    a.push(self.lower_arg(args[j], &pj)?);
                                } else if j == last {
                                    a.push(self.lower_arg(args[prefix_len], &pj)?);
                                // the trailing lambda
                                } else {
                                    a.push(self.zero_placeholder(c.params[j + 1]));
                                }
                            }
                            let mask: i32 = (prefix_len..last).map(|j| 1i32 << j).sum();
                            a.push(self.ir.add_expr(IrExpr::Const(IrConst::Int(mask))));
                            a.push(self.ir.add_expr(IrExpr::Const(IrConst::Null)));
                        } else {
                            for (i, &arg) in args.iter().enumerate() {
                                match c.params.get(i + 1) {
                                    Some(p) => a.push(self.lower_arg(arg, &ty_to_ir(*p))?),
                                    None => a.push(self.expr(arg)?),
                                }
                            }
                            // A `name$default` call appends a placeholder per omitted trailing parameter,
                            // an `int` bit-mask (a bit per omitted parameter), and a `null` marker.
                            if c.default_call {
                                let real_count = c.params.len() - 1; // exclude the receiver
                                for j in args.len()..real_count {
                                    let ph = self.zero_placeholder(c.params[j + 1]);
                                    a.push(ph);
                                }
                                let mask: i32 = (args.len()..real_count).map(|j| 1i32 << j).sum();
                                a.push(self.ir.add_expr(IrExpr::Const(IrConst::Int(mask))));
                                a.push(self.ir.add_expr(IrExpr::Const(IrConst::Null)));
                            }
                        }
                        let call = self.ir.add_expr(IrExpr::Call {
                            callee: Callee::Static {
                                owner: c.owner,
                                name: c.name,
                                descriptor: c.descriptor,
                                inline: c.is_inline,
                                must_inline: false,
                            },
                            dispatch_receiver: None,
                            args: a,
                        });
                        self.coerce_generic_read(call, e, c.physical_ret)
                    } else if let Some(c) = {
                        // A call selected by lambda RETURN type (`recv.sumOf { it * 2 }`): resolve the
                        // `@JvmName`-mangled `@InlineOnly` method (`sumOfInt`) matching the lambda's return,
                        // then splice it (its body is a fold loop). The lambda return comes from the typed
                        // lambda arg.
                        let arg_tys: Vec<Ty> = args.iter().map(|&a| self.info.ty(a)).collect();
                        arg_tys
                            .iter()
                            .find_map(|t| {
                                if let Ty::Fun(s) = t {
                                    Some(s.ret)
                                } else {
                                    None
                                }
                            })
                            .and_then(|lam_ret| {
                                self.resolver()
                                    .resolve_lambda_return_overload(rt, &name, lam_ret, &arg_tys)
                            })
                    } {
                        let (c, is_member) = c;
                        let phys = c.physical_ret;
                        let call = if is_member {
                            // Instance MEMBER (`recv.foo { … }`): the receiver is the DISPATCH receiver
                            // (`invokevirtual`/`invokeinterface`), NOT an argument. `c.params` are the
                            // value parameters only (no receiver). Emitting it static would leave the
                            // receiver on the operand stack → `VerifyError`.
                            let interface = self
                                .syms
                                .libraries
                                .resolve_type(&c.owner)
                                .is_some_and(|t| t.is_interface());
                            let recv = self.expr(receiver)?;
                            let mut a = Vec::new();
                            for (i, &arg) in args.iter().enumerate() {
                                match c.params.get(i) {
                                    Some(p) => a.push(self.lower_arg(arg, &ty_to_ir(*p))?),
                                    None => a.push(self.expr(arg)?),
                                }
                            }
                            self.ir.add_expr(IrExpr::Call {
                                callee: Callee::Virtual {
                                    owner: c.owner,
                                    name: c.name,
                                    descriptor: c.descriptor,
                                    interface,
                                },
                                dispatch_receiver: Some(recv),
                                args: a,
                            })
                        } else {
                            // Extension: a static method whose receiver is the FIRST argument.
                            let recv = self
                                .lower_arg(receiver, &ty_to_ir(*c.params.first().unwrap_or(&rt)))?;
                            let mut a = vec![recv];
                            for (i, &arg) in args.iter().enumerate() {
                                match c.params.get(i + 1) {
                                    Some(p) => a.push(self.lower_arg(arg, &ty_to_ir(*p))?),
                                    None => a.push(self.expr(arg)?),
                                }
                            }
                            self.ir.add_expr(IrExpr::Call {
                                callee: Callee::Static {
                                    owner: c.owner,
                                    name: c.name,
                                    descriptor: c.descriptor,
                                    inline: true,
                                    must_inline: c.must_inline,
                                },
                                dispatch_receiver: None,
                                args: a,
                            })
                        };
                        self.coerce_generic_read(call, e, phys)
                    } else if let Some(c) = {
                        // A private `@InlineOnly` extension (`String.uppercase()` → inlines
                        // `toUpperCase(Locale.ROOT)`): resolve via the inline-only path and emit an inline
                        // `Callee::Static` so the backend splices its REAL body (no call to the
                        // package-private method is emitted). Gated on `can_inline_call`, which DRY-RUNS the
                        // actual splice — so a body the emitter couldn't splice (and would fall back to an
                        // `invokestatic` on the private method) is never routed; the call simply skips.
                        let arg_tys: Vec<Ty> = args.iter().map(|&a| self.info.ty(a)).collect();
                        self.syms
                            .libraries
                            .resolve_scope_inline(&name, rt, &arg_tys)
                            .filter(|c| {
                                // The `@Metadata` `inline` flag is keyed by the Kotlin name; a
                                // `@JvmName`-mangled method (`sumOf` → `sumOfInt`) loses it, reading back
                                // `is_inline=false`. A PRIVATE (`must_inline`) extension has no callable
                                // method regardless, so it MUST be spliced — gate on `can_inline_call`
                                // (a real splice dry-run), which is the actual correctness condition.
                                (c.is_inline || c.must_inline)
                                    && self.syms.libraries.can_inline_call(
                                        &c.owner,
                                        &c.name,
                                        &c.descriptor,
                                    )
                            })
                    } {
                        let recv =
                            self.lower_arg(receiver, &ty_to_ir(*c.params.first().unwrap_or(&rt)))?;
                        let mut a = vec![recv];
                        for (i, &arg) in args.iter().enumerate() {
                            match c.params.get(i + 1) {
                                Some(p) => a.push(self.lower_arg(arg, &ty_to_ir(*p))?),
                                None => a.push(self.expr(arg)?),
                            }
                        }
                        let must_inline = c.must_inline;
                        let call = self.ir.add_expr(IrExpr::Call {
                            callee: Callee::Static {
                                owner: c.owner,
                                name: c.name,
                                descriptor: c.descriptor,
                                inline: true,
                                must_inline,
                            },
                            dispatch_receiver: None,
                            args: a,
                        });
                        self.coerce_generic_read(call, e, c.physical_ret)
                    } else {
                        return None;
                    }
                }
                // The callee is an arbitrary expression that evaluates to a function value — an
                // immediately-invoked call result (`mk()()`), an indexed/selected function, etc. Lower it
                // to the `FunctionN` and invoke it through `FunctionN.invoke` (same path as a function-typed
                // local `f(args)`).
                _ => {
                    if let Ty::Fun(sig) = self.info.ty(callee) {
                        if sig.params.len() == args.len() {
                            let func = self.expr(callee)?;
                            let mut a = Vec::new();
                            for arg in &args {
                                a.push(self.expr(*arg)?);
                            }
                            return Some(self.ir.add_expr(IrExpr::InvokeFunction {
                                func,
                                args: a,
                                ret: ty_to_ir(sig.ret),
                            }));
                        }
                    }
                    return None;
                }
            },
        })
    }
}

/// Whether `e` emits as a branch (a conditional that materializes via jumps + merge frames). Such an
/// expression can't be safely emitted while other operands sit on the stack (the merge frame would
/// omit them). Primitive `==`/`<`… and `if`/`when`/elvis are branchy; reference `==`
/// (`Intrinsics.areEqual`) and plain calls are not.
/// Does the expression (or any nested statement/expression) contain a `return`? Inlining a body or
/// lambda that returns non-locally isn't modeled, so such an `inline fun` is bailed (file skipped).
/// Whether `name` is referenced as a VALUE within `e` — anywhere except as the direct callee of a call.
/// An inline lambda parameter used only as `f(args)` can be spliced in place; one used as a value (passed
/// to another function, stored, or returned) must be materialized as a `FunctionN` instead.
fn name_used_as_value(file: &ast::File, e: AstExprId, name: &str) -> bool {
    match file.expr(e) {
        Expr::Name(n) => n == name,
        Expr::Call { callee, args } => {
            let callee_is_target = matches!(file.expr(*callee), Expr::Name(n) if n == name);
            (!callee_is_target && name_used_as_value(file, *callee, name))
                || args.iter().any(|&a| name_used_as_value(file, a, name))
        }
        _ => file.any_child_expr(e, &mut |c| name_used_as_value(file, c, name), &mut |s| {
            file.any_child_stmt(s, &mut |c| name_used_as_value(file, c, name))
        }),
    }
}

/// Does `e` contain a BARE `return` (no label — a non-local return from the enclosing function), at ANY
/// nesting depth INCLUDING inside nested lambdas? A bare return is non-local, so every lambda that
/// (transitively) encloses it must be inlined — a nested lambda's bare return surfaces in the outer
/// lambda's body once spliced, making the outer impl method invalid too. A labeled `return@x` (a local
/// return) is excluded. (Kotlin forbids a bare return inside a non-inline lambda, so descending is sound.)
fn body_has_bare_return(file: &ast::File, e: AstExprId) -> bool {
    fn stmt_bare(file: &ast::File, s: ast::StmtId) -> bool {
        match file.stmt(s) {
            Stmt::Return(_, None) => true,
            Stmt::Return(_, Some(_)) => false,
            _ => file.any_child_stmt(s, &mut |x| body_has_bare_return(file, x)),
        }
    }
    file.any_child_expr(e, &mut |x| body_has_bare_return(file, x), &mut |s| {
        stmt_bare(file, s)
    })
}

fn body_has_return(file: &ast::File, e: AstExprId) -> bool {
    file.any_child_expr(e, &mut |x| body_has_return(file, x), &mut |s| {
        stmt_has_return(file, s)
    })
}

fn stmt_has_return(file: &ast::File, s: ast::StmtId) -> bool {
    matches!(file.stmt(s), Stmt::Return(..))
        || file.any_child_stmt(s, &mut |x| body_has_return(file, x))
}

/// Does `e` contain a `return@label` matching `label` (not descending into nested lambdas)?
fn body_has_labeled_return(file: &ast::File, e: AstExprId, label: &str) -> bool {
    fn stmt_has(file: &ast::File, s: ast::StmtId, lbl: &str) -> bool {
        match file.stmt(s) {
            Stmt::Return(_, l) => l.as_deref() == Some(lbl),
            _ => file.any_child_stmt(s, &mut |x| expr_has(file, x, lbl)),
        }
    }
    fn expr_has(file: &ast::File, e: AstExprId, lbl: &str) -> bool {
        if matches!(file.expr(e), Expr::Lambda { .. }) {
            return false;
        }
        file.any_child_expr(e, &mut |x| expr_has(file, x, lbl), &mut |s| {
            stmt_has(file, s, lbl)
        })
    }
    expr_has(file, e, label)
}

/// Does `e` contain a `return` the inline-lambda expander can't model — a `return@other` (labeled to some
/// OTHER inline fn, not the one being expanded)? A BARE `return` (non-local to the enclosing function) IS
/// modeled now: `lower_inline_lambda_invoke` clears the inline-return stack while lowering the body, so the
/// bare `return` targets the real enclosing function's return. A `return@own_label` (local to this lambda)
/// is also modeled (the `inline_lambda_ret` frame). Nested lambdas keep their own return scope.
fn body_has_disallowed_return(file: &ast::File, e: AstExprId, own_label: &str) -> bool {
    fn stmt_bad(file: &ast::File, s: ast::StmtId, own: &str) -> bool {
        match file.stmt(s) {
            // `return@other` — labeled to a different inline fn — isn't modeled; bail. Bare / `return@own`
            // are fine (handled by the real-return clear / the `inline_lambda_ret` frame respectively).
            Stmt::Return(_, Some(l)) if l != own => true,
            _ => file.any_child_stmt(s, &mut |x| expr_bad(file, x, own)),
        }
    }
    fn expr_bad(file: &ast::File, e: AstExprId, own: &str) -> bool {
        // A nested lambda has its own return scope — don't descend into it.
        if matches!(file.expr(e), Expr::Lambda { .. }) {
            return false;
        }
        file.any_child_expr(e, &mut |x| expr_bad(file, x, own), &mut |s| {
            stmt_bad(file, s, own)
        })
    }
    expr_bad(file, e, own_label)
}

fn is_branchy(file: &ast::File, e: AstExprId) -> bool {
    match file.expr(e) {
        Expr::If { .. } | Expr::When { .. } | Expr::Elvis { .. } => true,
        // A safe call `recv?.m()` lowers to a null-check branch (a stackmap frame), so it is not safe to
        // splice mid-sequence (e.g. as an array-literal element) — treat it as branchy so callers bail.
        Expr::SafeCall { .. } => true,
        // A `try`/`catch` expression emits exception-handler merge frames; as a mid-`Vararg`-fill element
        // those frames land inside the element-store sequence and fail the verifier — bail (skip).
        Expr::Try { .. } => true,
        Expr::Binary { op, lhs, .. } => {
            use ast::BinOp::*;
            matches!(op, Lt | Le | Gt | Ge | And | Or)
                || (matches!(op, Eq | Ne) && file_expr_is_primitive(file, *lhs))
        }
        Expr::Unary {
            op: ast::UnOp::Not, ..
        } => true,
        _ => false,
    }
}

/// Deep check: does `e` contain any branch-producing construct (`if`/`when`/elvis/safe-call/`try`/`&&`/
/// `||`/loop) anywhere within it? The branchless lambda-splice (the `let`/`also` inline route) can't
/// relocate the stackmap frames such a body produces, so a branchy lambda body must fall back to the
/// per-function desugar (which inlines the body through normal branchy lowering).
fn body_contains_branch(file: &ast::File, e: AstExprId) -> bool {
    match file.expr(e) {
        Expr::If { .. }
        | Expr::When { .. }
        | Expr::Elvis { .. }
        | Expr::SafeCall { .. }
        | Expr::Try { .. } => true,
        Expr::Binary {
            op: ast::BinOp::And | ast::BinOp::Or,
            ..
        } => true,
        Expr::Lambda { .. } => false, // a nested lambda is its own method body
        _ => file.any_child_expr(e, &mut |c| body_contains_branch(file, c), &mut |s| {
            stmt_contains_branch(file, s)
        }),
    }
}

fn stmt_contains_branch(file: &ast::File, s: ast::StmtId) -> bool {
    match file.stmt(s) {
        Stmt::While { .. } | Stmt::DoWhile { .. } | Stmt::For { .. } | Stmt::ForEach { .. } => true,
        _ => file.any_child_stmt(s, &mut |c| body_contains_branch(file, c)),
    }
}

/// Whether a JVM method descriptor `(params)ret` has a top-level `byte` (`B`) or `short` (`S`) parameter
/// (including a `byte[]`/`short[]` array). krusty's `Ty` erases these to `Int`, so it can't build a
/// matching argument/array — a call to such an overload would fail the verifier; the caller bails.
fn descriptor_has_byte_or_short_param(desc: &str) -> bool {
    let Some(end) = desc.find(')') else {
        return false;
    };
    let bytes = desc[1..end].as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'[' => {
                i += 1;
            } // array prefix — fall through to the element type
            b'L' => {
                while i < bytes.len() && bytes[i] != b';' {
                    i += 1;
                }
                i += 1;
            } // skip `Lname;`
            b'B' | b'S' => return true,
            _ => {
                i += 1;
            }
        }
    }
    false
}

/// A `when (subject) { … }` condition that is a complete boolean test of the subject (`is`/`!is`,
/// `in`/`!in` range) — built by the parser as a structural `Is`/`InRange` node, so it is used directly,
/// not compared with `subject == cond`.
fn is_when_test(file: &ast::File, e: AstExprId) -> bool {
    matches!(file.expr(e), Expr::Is { .. } | Expr::InRange { .. })
}

/// Const-fold an annotation argument expression to a String: a string literal, a `const val` name, or a
/// string template whose interpolations are themselves const-foldable (`"$prefix.bar"` with
/// `const val prefix = "foo"` → `"foo.bar"`). `None` for anything not statically a string.
fn const_string_value(file: &ast::File, e: AstExprId) -> Option<String> {
    const_string_value_d(file, e, 0)
}

/// `depth` bounds the recursion through `const val` references so a cyclic chain
/// (`const val a = b; const val b = a`) terminates with `None` instead of overflowing the stack.
fn const_string_value_d(file: &ast::File, e: AstExprId, depth: u32) -> Option<String> {
    if depth > 32 {
        return None;
    }
    match file.expr(e) {
        Expr::StringLit(s) => Some(s.clone()),
        Expr::Name(n) => top_level_const_string_d(file, n, depth + 1),
        Expr::Template(parts) => {
            let mut out = String::new();
            for p in parts {
                match p {
                    TemplatePart::Str(s) => out.push_str(s),
                    TemplatePart::Expr(x) => {
                        out.push_str(&const_string_value_d(file, *x, depth + 1)?)
                    }
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// The string value of a top-level property `name` whose initializer const-folds to a string
/// (`const val prefix = "foo"`), or `None`. (krusty's parser doesn't currently retain the `const`
/// modifier on a top-level `val`, so this matches any top-level property with a foldable string init —
/// safe, since only literals/foldable templates fold.)
fn top_level_const_string_d(file: &ast::File, name: &str, depth: u32) -> Option<String> {
    if depth > 32 {
        return None;
    }
    file.decls.iter().find_map(|&d| match file.decl(d) {
        Decl::Property(p) if p.name == name => p
            .init
            .and_then(|i| const_string_value_d(file, i, depth + 1)),
        _ => None,
    })
}

/// `(property_name, serial_name)` for each primary-constructor property carrying `@SerialName("…")`
/// (const-folded). Empty when none — the serialization extension reads this to name descriptor elements.
/// Const-fold a primary-constructor default-value expression to an [`IrConst`] for the serialization
/// extension's `isOptional` element handling. Only obvious compile-time literals (the common
/// `= null`/`= 5`/`= "x"`/`= true` defaults); `None` for any non-literal default (the field is then
/// treated as non-optional — never miscompiled).
fn const_default_of(file: &ast::File, e: AstExprId) -> Option<crate::ir::IrConst> {
    use crate::ir::IrConst;
    Some(match file.expr(e) {
        Expr::NullLit => IrConst::Null,
        Expr::IntLit(v) => IrConst::Int(*v as i32),
        Expr::LongLit(v) => IrConst::Long(*v),
        Expr::BoolLit(v) => IrConst::Boolean(*v),
        Expr::DoubleLit(v) => IrConst::Double(*v),
        Expr::FloatLit(v) => IrConst::Float(*v),
        Expr::CharLit(v) => IrConst::Char(*v),
        Expr::StringLit(s) => IrConst::String(s.clone()),
        _ => return None,
    })
}

/// Widen a const-folded default literal to the field's declared numeric type so its JVM slot
/// width/kind matches the field local (`val x: Long = 5` folds `5` as `Int` but must store as `Long`;
/// likewise `Float`/`Double`). Non-numeric or already-matching consts pass through unchanged.
fn widen_const_to(c: crate::ir::IrConst, t: Ty) -> crate::ir::IrConst {
    use crate::ir::IrConst;
    match (t, c) {
        (Ty::Long, IrConst::Int(v)) => IrConst::Long(v as i64),
        (Ty::Long, IrConst::Byte(v)) => IrConst::Long(v as i64),
        (Ty::Long, IrConst::Short(v)) => IrConst::Long(v as i64),
        (Ty::Double, IrConst::Int(v)) => IrConst::Double(v as f64),
        (Ty::Double, IrConst::Long(v)) => IrConst::Double(v as f64),
        (Ty::Double, IrConst::Float(v)) => IrConst::Double(v as f64),
        (Ty::Float, IrConst::Int(v)) => IrConst::Float(v as f32),
        (Ty::Float, IrConst::Long(v)) => IrConst::Float(v as f32),
        (_, c) => c,
    }
}

fn serial_names_of(file: &ast::File, c: &ast::ClassDecl) -> Vec<(String, String)> {
    c.props
        .iter()
        .filter_map(|p| {
            let i = p.annotations.iter().position(|a| a == "SerialName")?;
            let arg = p.annotation_args.get(i).and_then(|args| args.first())?;
            Some((p.name.clone(), const_string_value(file, *arg)?))
        })
        .collect()
}

fn is_const_literal(file: &ast::File, e: AstExprId) -> bool {
    matches!(
        file.expr(e),
        Expr::IntLit(_)
            | Expr::LongLit(_)
            | Expr::UIntLit(_)
            | Expr::ULongLit(_)
            | Expr::DoubleLit(_)
            | Expr::FloatLit(_)
            | Expr::BoolLit(_)
            | Expr::CharLit(_)
            | Expr::StringLit(_)
            | Expr::NullLit
    )
}

/// Best-effort: is the literal/operand a primitive (so `==` would use a numeric branch, not
/// `Intrinsics.areEqual`)? Conservative — only obvious primitive literals count.
fn file_expr_is_primitive(file: &ast::File, e: AstExprId) -> bool {
    matches!(
        file.expr(e),
        Expr::IntLit(_)
            | Expr::LongLit(_)
            | Expr::UIntLit(_)
            | Expr::ULongLit(_)
            | Expr::DoubleLit(_)
            | Expr::FloatLit(_)
            | Expr::BoolLit(_)
            | Expr::CharLit(_)
    )
}

/// The IR parameter type for a captured local lifted into a local function: a boxed (closure-written)
/// var is passed as its `Ref$XxxRef` holder reference; an ordinary captured local by its own value.
fn captured_param_ir(name: &str, ty: Ty, boxed: &std::collections::HashSet<String>) -> Ty {
    if boxed.contains(name) {
        ty_to_ir(Ty::obj(ref_holder_internal(ty)))
    } else {
        ty_to_ir(ty)
    }
}

/// The `kotlin/jvm/internal/Ref$XxxRef` holder class for a boxed mutable local of (erased) type `t`.
fn ref_holder_internal(t: Ty) -> &'static str {
    match t {
        // Unboxed unsigned types ARE their signed primitive on the JVM (`UInt`=int, `ULong`=long), so
        // they share the signed `Ref` holder — matching `ty_to_ir`'s erasure used by the emitter.
        Ty::Int | Ty::UInt => "kotlin/jvm/internal/Ref$IntRef",
        Ty::Long | Ty::ULong => "kotlin/jvm/internal/Ref$LongRef",
        Ty::Float => "kotlin/jvm/internal/Ref$FloatRef",
        Ty::Double => "kotlin/jvm/internal/Ref$DoubleRef",
        Ty::Boolean => "kotlin/jvm/internal/Ref$BooleanRef",
        Ty::Char => "kotlin/jvm/internal/Ref$CharRef",
        Ty::Byte => "kotlin/jvm/internal/Ref$ByteRef",
        Ty::Short => "kotlin/jvm/internal/Ref$ShortRef",
        _ => "kotlin/jvm/internal/Ref$ObjectRef",
    }
}

/// Collect the simple (`Name`-callee) function-call names anywhere in `e`'s subtree — used to decide
/// whether a lambda body calls a `suspend` function (same-file or classpath).
fn collect_call_names(file: &ast::File, e: AstExprId, out: &std::cell::RefCell<Vec<String>>) {
    if let ast::Expr::Call { callee, .. } = file.expr(e) {
        if let ast::Expr::Name(n) = file.expr(*callee) {
            out.borrow_mut().push(n.clone());
        }
    }
    file.any_child_expr(
        e,
        &mut |c| {
            collect_call_names(file, c, out);
            false
        },
        &mut |s| {
            file.any_child_stmt(s, &mut |c| {
                collect_call_names(file, c, out);
                false
            });
            false
        },
    );
}

fn class_internal(file: &ast::File, name: &str) -> String {
    let mangled = name.replace('.', "$");
    match &file.package {
        Some(p) if !p.is_empty() => format!("{}/{}", p.replace('.', "/"), mangled),
        _ => mangled,
    }
}

/// Resolve a written type to a `Ty`: a builtin (`Int`, `String`, …), else a class declared in this
/// file (`A` → its internal name), else an erased reference (generic type param / external type →
/// `Object`). Without this, class-typed fields resolve to `Error` and emit a bad descriptor.
/// For an array-typed IR field, the `java.util.Arrays.toString` array-parameter
/// descriptor (`[Z`, `[Ljava/lang/Object;` for a reference array); `None` if the field isn't an array.
/// A `data class` renders/compares/hashes array properties through `java.util.Arrays`, like kotlinc.
fn data_array_param(t: &Ty) -> Option<&'static str> {
    let Some(fq_name) = t.non_null().obj_internal() else {
        return None;
    };
    Some(match fq_name {
        "kotlin/BooleanArray" => "[Z",
        "kotlin/CharArray" => "[C",
        "kotlin/ByteArray" => "[B",
        "kotlin/ShortArray" => "[S",
        "kotlin/IntArray" => "[I",
        "kotlin/LongArray" => "[J",
        "kotlin/FloatArray" => "[F",
        "kotlin/DoubleArray" => "[D",
        "kotlin/Array" => "[Ljava/lang/Object;",
        _ => return None,
    })
}

/// Extract the BACKEND-AGNOSTIC generic-signature shape of a generic class (`class Box<T>`), or `None`
/// for a non-generic class / one with explicit supertypes (whose generic args aren't modeled yet) / an
/// unsupported bound. The `jvm` backend formats this into the class `Signature` attribute.
fn class_generic_sig(c: &ast::ClassDecl) -> Option<crate::ir::IrGenericSig> {
    if c.type_params.is_empty() || c.base_class.is_some() || !c.supertypes.is_empty() {
        return None;
    }
    Some(crate::ir::IrGenericSig {
        type_params: type_param_bounds_ir(&c.type_params, &c.type_param_bounds)?,
        param_tparams: Vec::new(),
        ret_tparam: None,
    })
}

/// For a generic class, list `(field name, type-parameter name)` for each property whose declared type
/// is a bare type-parameter reference (`class Pair<A, B>(val a: A, val b: B)` → `[("a","A"),("b","B")]`).
/// The JVM backend emits a field `Signature` (`TA;`) for each. Empty for a non-generic class.
fn class_field_tparams(c: &ast::ClassDecl) -> Vec<(String, String)> {
    if c.type_params.is_empty() {
        return Vec::new();
    }
    let tps = &c.type_params;
    let mut out = Vec::new();
    // Constructor `val`/`var` properties (backing fields), then class-body backing-field properties.
    for p in c.props.iter().filter(|p| p.is_property) {
        if ref_is_bare_tparam(&p.ty, tps) {
            out.push((p.name.clone(), p.ty.name.clone()));
        }
    }
    for p in c.body_props.iter().filter(|p| is_backing_field_prop(p)) {
        if let Some(ty) = &p.ty {
            if ref_is_bare_tparam(ty, tps) {
                out.push((p.name.clone(), ty.name.clone()));
            }
        }
    }
    out
}

/// Extract the backend-agnostic generic-signature shape of a type-parameterized function, or `None` for
/// a non-generic function / an unmodeled shape (a type parameter inside a generic argument like
/// `List<T>`, a vararg, an unsupported bound). Concrete parameter/return types are left to the backend
/// (which reads them from the `IrFunction`); only the bare-type-parameter positions are recorded.
fn fn_generic_sig(f: &ast::FunDecl) -> Option<crate::ir::IrGenericSig> {
    if f.type_params.is_empty() {
        return None;
    }
    let tps = &f.type_params;
    let mut param_tparams = Vec::with_capacity(f.params.len());
    for p in &f.params {
        if p.is_vararg {
            return None;
        }
        if ref_is_bare_tparam(&p.ty, tps) {
            param_tparams.push(Some(p.ty.name.clone()));
        } else if ref_uses_tparam(&p.ty, tps) {
            return None;
        } else {
            param_tparams.push(None);
        }
    }
    let ret_tparam = match f.ret.as_ref() {
        Some(r) if ref_is_bare_tparam(r, tps) => Some(r.name.clone()),
        Some(r) if ref_uses_tparam(r, tps) => return None,
        _ => None,
    };
    Some(crate::ir::IrGenericSig {
        type_params: type_param_bounds_ir(tps, &f.type_param_bounds)?,
        param_tparams,
        ret_tparam,
    })
}

/// Pair each type-parameter name with its upper bound as a Kotlin `IrType` (`kotlin/Any` when none / an
/// `Any` bound). `None` if a bound is a non-`Any`, non-primitive type (not modeled yet → omit the whole
/// signature). Backend-agnostic: the bound is a Kotlin type; the JVM backend maps a primitive to its wrapper.
fn type_param_bounds_ir(
    names: &[String],
    bounds: &[(String, ast::TypeRef)],
) -> Option<Vec<(String, Ty)>> {
    let any = || Ty::obj("kotlin/Any");
    let mut out = Vec::with_capacity(names.len());
    for tp in names {
        let bound = match bounds.iter().find(|(n, _)| n == tp) {
            None => any(),
            Some((_, b)) if b.name == "Any" && !b.nullable => any(),
            Some((_, b)) => match Ty::from_name(&b.name).filter(|t| t.is_primitive()) {
                Some(t) => ty_to_ir(t),
                None => return None,
            },
        };
        out.push((tp.clone(), bound));
    }
    Some(out)
}

fn ref_is_bare_tparam(r: &ast::TypeRef, tps: &[String]) -> bool {
    // A nullable type-parameter ref (`T?`) is still `T<name>;` in the JVM generic signature — nullability
    // is not represented there (kotlinc drops it; the erased descriptor is `Object` either way).
    r.fun_params.is_empty() && r.targs.is_empty() && tps.iter().any(|t| t == &r.name)
}

fn ref_uses_tparam(r: &ast::TypeRef, tps: &[String]) -> bool {
    tps.iter().any(|t| t == &r.name)
        || r.targs.iter().any(|a| ref_uses_tparam(a, tps))
        || r.fun_params.iter().any(|a| ref_uses_tparam(a, tps))
        || r.arg.as_ref().is_some_and(|a| ref_uses_tparam(a, tps))
}

/// An IR field type that PRESERVES generic type arguments at every depth (`List<List<String>>` keeps both
/// the outer and inner element types). `ty_of`/`ty_to_ir` erase a general `Obj`'s args; this rebuilds them
/// recursively from the source `TypeRef` so the serialization extension can derive a nested element
/// serializer (`ListSerializer(ListSerializer(StringSerializer))`). Additive metadata on the type only.
fn field_ty_with_args(file: &ast::File, tr: &ast::TypeRef) -> Ty {
    let base = ty_to_ir(ty_of(file, tr));
    // Rebuild the non-null type with recursively-preserved type arguments, then re-apply the source `?`
    // from the `TypeRef` (`ty_of` strips it from a reference type) — so a nullable element `String?` keeps
    // its nullability, which the element serializer needs for a `.nullable` wrapper.
    let resolved = match base.non_null().obj_internal() {
        Some(fq) if !tr.targs.is_empty() => {
            let targs: Vec<Ty> = tr
                .targs
                .iter()
                .map(|a| field_ty_with_args(file, a))
                .collect();
            Ty::obj_args(fq, &targs)
        }
        _ => base.non_null(),
    };
    if tr.nullable {
        Ty::nullable(resolved)
    } else {
        resolved
    }
}

fn ty_of(file: &ast::File, r: &ast::TypeRef) -> Ty {
    // Function type `(A, B) -> R` (parsed with `fun_params` non-empty / `name == "<fun>"`).
    if !r.fun_params.is_empty() || r.name == "<fun>" {
        let params: Vec<Ty> = r.fun_params.iter().map(|p| ty_of(file, p)).collect();
        let ret = r.arg.as_ref().map(|a| ty_of(file, a)).unwrap_or(Ty::Unit);
        return if r.fun_suspend {
            Ty::fun_suspend(params, ret)
        } else {
            Ty::fun(params, ret)
        };
    }
    if let Some(t) = Ty::from_name(&r.name) {
        // A nullable primitive is `Nullable(prim)`, a reference slot consistent with the checker —
        // otherwise a boxed value would be stored in a primitive field and unboxed wrong.
        if r.nullable && !t.is_reference() {
            if let Some(nb) = t.nullable_boxed() {
                return nb;
            }
        }
        return t;
    }
    // A specialized primitive array (`IntArray` → `int[]`), or a reference `Array<T>` (element from the
    // type argument). Without this an array-typed field/param would erase to `Object`.
    if let Some(elem) = Ty::primitive_array_element(&r.name) {
        return Ty::array(elem);
    }
    if r.name == "Array" {
        let elem = r
            .arg
            .as_ref()
            .map(|a| ty_of(file, a))
            .unwrap_or_else(|| Ty::obj("kotlin/Any"));
        if elem.is_reference() {
            return Ty::array(elem);
        }
    }
    let is_class = file
        .decls
        .iter()
        .any(|&d| matches!(file.decl(d), Decl::Class(c) if c.name == r.name));
    if is_class {
        Ty::obj(&class_internal(file, &r.name))
    } else if let Some(internal) = crate::jvm::jvm_class_map::kotlin_builtin_to_internal(&r.name) {
        // A built-in collection/reference type resolves to its FRONT-END Kotlin name — a collection keeps
        // `kotlin/collections/{List,MutableList,…}` (read-only vs mutable; emit erases to `java/util/List`),
        // other built-ins keep their JVM identity. `Any` stays `kotlin/Any`, not `java/lang/Object`.
        if internal == "java/lang/Object" {
            Ty::obj("kotlin/Any")
        } else {
            Ty::obj(internal)
        }
    } else {
        Ty::obj("kotlin/Any")
    }
}

/// Whether an `IrType` is a reference type (anything except a primitive class FqName / Unit).
fn ir_type_is_reference(t: &Ty) -> bool {
    if matches!(t.non_null(), Ty::Fun(_)) {
        return true;
    }
    match t.non_null().obj_internal() {
        Some(fq_name) => !matches!(
            fq_name,
            "kotlin/Int"
                | "kotlin/Long"
                | "kotlin/Short"
                | "kotlin/Byte"
                | "kotlin/Boolean"
                | "kotlin/Char"
                | "kotlin/Double"
                | "kotlin/Float"
        ),
        None => false,
    }
}

/// Whether `t` is exactly `java/lang/Object` / `kotlin/Any` (the erased top type — no `checkcast` to it).
fn ir_type_is_object(t: &Ty) -> bool {
    matches!(
        t.non_null().obj_internal(),
        Some("java/lang/Any" | "kotlin/Any" | "java/lang/Object")
    )
}

/// Conservative "is `arg` assignable to `param`" for constructor-overload selection: an exact match,
/// or any reference flowing into an erased `Object`/`Any` param (an erased generic type parameter).
/// Deliberately strict otherwise (no class-hierarchy data here) — used only to tell whether the PRIMARY
/// constructor can accept the args before falling back to a secondary (`IC("abc")`: a `String` is NOT
/// assignable to the primary's `List<T>` param, but IS to the secondary's erased `T`).
fn ir_arg_assignable(arg: &Ty, param: &Ty) -> bool {
    arg == param || (ir_type_is_object(param) && ir_type_is_reference(arg))
}

/// Whether `e` contains a `return` (always exits the function), or a `break`/`continue` that targets a
/// loop *outside* `e` (i.e. at loop-depth 0 here) — control transfers that would skip an enclosing
/// `finally`. Does not descend into lambdas (their control flow is separate).
fn body_has_nonlocal_exit(file: &ast::File, e: AstExprId) -> bool {
    body_has_exit(file, e, true)
}

/// Whether `e` declares a local `val`/`var` (a `Stmt::Local`/`Destructure`), not descending into a
/// nested lambda. A `finally` that declares locals can't be inlined on the several exit paths krusty's
/// `emit_try` needs without the duplicated locals' slots clashing across copies — so such a `try` skips.
fn body_declares_local(file: &ast::File, e: AstExprId) -> bool {
    fn ex(file: &ast::File, e: AstExprId) -> bool {
        match file.expr(e) {
            Expr::Lambda { .. } | Expr::CallableRef { .. } => false,
            _ => file.any_child_expr(e, &mut |c| ex(file, c), &mut |s| st(file, s)),
        }
    }
    fn st(file: &ast::File, s: crate::ast::StmtId) -> bool {
        match file.stmt(s) {
            Stmt::Local { .. } | Stmt::Destructure { .. } => true,
            Stmt::Expr(e)
            | Stmt::Assign { value: e, .. }
            | Stmt::While { body: e, .. }
            | Stmt::DoWhile { body: e, .. }
            | Stmt::For { body: e, .. }
            | Stmt::ForEach { body: e, .. } => ex(file, *e),
            _ => false,
        }
    }
    ex(file, e)
}

/// Whether `e` contains a `break`/`continue` that escapes past this region (a loop-local one is fine).
/// `with_return` also flags a `return` — used where a `return` can't be modeled; `false` where the
/// lowerer handles `return` itself (inlining enclosing `finally`s) and only `break`/`continue` must bail.
fn body_has_break_continue(file: &ast::File, e: AstExprId) -> bool {
    body_has_exit(file, e, false)
}

fn body_has_exit(file: &ast::File, e: AstExprId, with_return: bool) -> bool {
    fn ex(file: &ast::File, e: AstExprId, ld: u32, wr: bool) -> bool {
        match file.expr(e) {
            // A lambda's control flow is separate; a callable-ref receiver carries no return/break.
            Expr::Lambda { .. } | Expr::CallableRef { .. } => false,
            _ => file.any_child_expr(e, &mut |c| ex(file, c, ld, wr), &mut |s| {
                st(file, s, ld, wr)
            }),
        }
    }
    fn st(file: &ast::File, s: crate::ast::StmtId, ld: u32, wr: bool) -> bool {
        match file.stmt(s) {
            Stmt::Return(..) => wr,
            Stmt::Break(_) | Stmt::Continue(_) => ld == 0,
            Stmt::Expr(e)
            | Stmt::Local { init: e, .. }
            | Stmt::Assign { value: e, .. }
            | Stmt::Destructure { init: e, .. } => ex(file, *e, ld, wr),
            // A loop's body raises the loop depth, so its `break`/`continue` are loop-local.
            Stmt::While { cond, body, .. } => {
                ex(file, *cond, ld, wr) || ex(file, *body, ld + 1, wr)
            }
            Stmt::DoWhile { body, cond, .. } => {
                ex(file, *body, ld + 1, wr) || ex(file, *cond, ld, wr)
            }
            Stmt::For { body, .. } | Stmt::ForEach { body, .. } => ex(file, *body, ld + 1, wr),
            _ => false,
        }
    }
    ex(file, e, 0, with_return)
}

/// The element type of a primitive-array constructor name (`IntArray` → `Int`).
/// A primitive range class iterated by a counted loop: its (unboxed) element type and the JVM
/// primitive descriptor of its `getFirst`/`getLast` getters. Only the step-+1 *range* classes
/// (not the general progressions) use the counted loop; `Char` ranges fall to the iterator path.
fn range_counted_elem(internal: &str) -> Option<(Ty, &'static str)> {
    match internal {
        "kotlin/ranges/IntRange" => Some((Ty::Int, "I")),
        "kotlin/ranges/LongRange" => Some((Ty::Long, "J")),
        // Unsigned ranges erase to the signed primitive; the counted loop uses unsigned comparison.
        "kotlin/ranges/UIntRange" => Some((Ty::UInt, "I")),
        "kotlin/ranges/ULongRange" => Some((Ty::ULong, "J")),
        _ => None,
    }
}

/// The element type + primitive descriptor for a progression iterated via `getStep()`. A `…Range` is
/// a `…Progression` subtype, but ranges keep the cheaper unit-step path above; only a bare
/// progression (the result of `downTo`/`step`/`reversed`) reaches here. Unsigned progressions are
/// handled too — their elements compare via `compareUnsigned` (see `lower_foreach_progression`).
fn progression_counted_elem(internal: &str) -> Option<(Ty, &'static str)> {
    match internal {
        "kotlin/ranges/IntProgression" => Some((Ty::Int, "I")),
        "kotlin/ranges/LongProgression" => Some((Ty::Long, "J")),
        // A `CharProgression`'s counter is a `Char` but its `step` is an `Int`; char arithmetic is
        // int-based on the JVM, so the counted loop works with an `Int` increment.
        "kotlin/ranges/CharProgression" => Some((Ty::Char, "C")),
        // Unsigned progressions erase to the signed primitive; the counted loop compares unsigned.
        "kotlin/ranges/UIntProgression" => Some((Ty::UInt, "I")),
        "kotlin/ranges/ULongProgression" => Some((Ty::ULong, "J")),
        _ => None,
    }
}

/// Carry a declared `?` into a field/underlying type. The JVM value-class pass keys unboxed-vs-boxed
/// representation on a value class's underlying nullability.
fn mark_nullable(t: Ty) -> Ty {
    Ty::nullable(t)
}

pub(crate) fn ty_to_ir(t: Ty) -> Ty {
    let fq = match t {
        Ty::Int => "kotlin/Int",
        Ty::Long => "kotlin/Long",
        // Unboxed unsigned types ARE their signed primitive on the JVM (`UInt` = int, `ULong` = long);
        // unsigned-specific operations are selected earlier from the checker `Ty`, not here.
        Ty::UInt => "kotlin/Int",
        Ty::ULong => "kotlin/Long",
        Ty::Short => "kotlin/Short",
        Ty::Byte => "kotlin/Byte",
        Ty::Boolean => "kotlin/Boolean",
        Ty::Char => "kotlin/Char",
        Ty::Double => "kotlin/Double",
        Ty::Float => "kotlin/Float",
        Ty::String => "kotlin/String",
        Ty::Unit => return Ty::Unit,
        Ty::Nothing => return Ty::Nothing,
        // (see `ir_array_element` below for the inverse — extracting an array IrType's element.)
        // A reference `Array<T>` keeps its element as a type argument (the JVM backend boxes a
        // primitive `T` when it lays out the array; the front end keeps the logical element).
        Ty::Obj("kotlin/Array", args) => {
            let targs: Vec<Ty> = args.iter().map(|t| ty_to_ir(*t)).collect();
            return Ty::obj_args("kotlin/Array", &targs);
        }
        Ty::Obj(n, _) => return Ty::obj(n),
        // A Kotlin function type `(A,…) -> R` is kept structural so each backend picks its own
        // representation (the JVM maps it to `kotlin/jvm/functions/FunctionN`, JS to a closure, …).
        Ty::Fun(s) => {
            let params: Vec<Ty> = s.params.iter().map(|t| ty_to_ir(*t)).collect();
            let ret = ty_to_ir(s.ret);
            return if s.suspend {
                Ty::fun_suspend(params, ret)
            } else {
                Ty::fun(params, ret)
            };
        }
        // An array is a regular class type (`kotlin/IntArray`, `kotlin/Array<T>`); the backend lowers
        // its representation. Primitive arrays encode the element in the class name.
        Ty::Array(e) => {
            let fq = match *e {
                Ty::Int => "kotlin/IntArray",
                Ty::Long => "kotlin/LongArray",
                Ty::Double => "kotlin/DoubleArray",
                Ty::Float => "kotlin/FloatArray",
                Ty::Boolean => "kotlin/BooleanArray",
                Ty::Char => "kotlin/CharArray",
                Ty::Byte => "kotlin/ByteArray",
                Ty::Short => "kotlin/ShortArray",
                // An unsigned array (e.g. a `vararg x: UInt` → `UIntArray`) is the unboxed underlying
                // primitive array (`[I`/`[J`), NOT a boxed `kotlin/Array`. Keep it primitive so it
                // doesn't collide with a boxed `Array<Int>` at the `kotlin/Array` element-boxing step.
                Ty::UInt => "kotlin/IntArray",
                Ty::ULong => "kotlin/LongArray",
                _ => return Ty::obj_args("kotlin/Array", &[ty_to_ir(*e)]),
            };
            return Ty::obj(fq);
        }
        // A nullable type. A nullable PRIMITIVE is a boxed wrapper reference in the IR (the wrapper
        // class) — matching the representation from before nullable primitives became `Ty::Nullable`,
        // so the codegen for a boxed `Int?` is unchanged (a reference, never an unboxed `int` slot). A
        // nullable REFERENCE keeps its reference form. Without this arm a nullable primitive fell to
        // `Ty::Error` and miscompiled.
        Ty::Nullable(inner) => {
            return match crate::jvm::jvm_class_map::wrapper_internal(*inner) {
                Some(wrapper) => Ty::obj(wrapper),
                None => ty_to_ir(*inner),
            };
        }
        // A type parameter `T` erases to its declared bound in the IR (JVM erasure). When generic
        // substitution lands this will instead carry the `TyParam` so the backend erases at emit; for
        // now erasing here keeps codegen identical to the pre-`TyParam` representation.
        Ty::TyParam(_, bound) => return ty_to_ir(*bound),
        _ => return Ty::Error,
    };
    Ty::obj(fq)
}

/// The element `IrType` of an array `IrType` target — a reference `Array<E>` (its type argument) or a
/// primitive specialized array (`kotlin/IntArray` → `kotlin/Int`). `None` for a non-array type. Used
/// to materialize an empty array (`emptyArray<T>()`) of the target's element type.
fn ir_array_element(t: &Ty) -> Option<Ty> {
    let Some(fq_name) = t.non_null().obj_internal() else {
        return None;
    };
    if fq_name == "kotlin/Array" {
        return t.non_null().type_args().first().copied();
    }
    let prim = match fq_name {
        "kotlin/IntArray" => "kotlin/Int",
        "kotlin/LongArray" => "kotlin/Long",
        "kotlin/DoubleArray" => "kotlin/Double",
        "kotlin/FloatArray" => "kotlin/Float",
        "kotlin/BooleanArray" => "kotlin/Boolean",
        "kotlin/CharArray" => "kotlin/Char",
        "kotlin/ByteArray" => "kotlin/Byte",
        "kotlin/ShortArray" => "kotlin/Short",
        _ => return None,
    };
    Some(Ty::obj(prim))
}

/// Per-parameter `Some(name)` when a non-null assertion (`Intrinsics.checkNotNullParameter`) should
/// guard it: a non-null reference parameter of a visible (non-`private`) function — matching kotlinc.
/// Primitives, nullable params (`String?`), and generic type parameters (`T`, which may be nullable)
/// are skipped. Conservative: when the parameter lists don't line up (e.g. an extension receiver
/// shifts them) no guards are emitted.
fn param_checks_for(f: &ast::FunDecl, param_tys: &[Ty]) -> Vec<Option<String>> {
    if f.is_private || f.receiver.is_some() || f.params.len() != param_tys.len() {
        return vec![None; param_tys.len()];
    }
    f.params
        .iter()
        .zip(param_tys)
        .map(|(p, ty)| {
            let is_type_param = f.type_params.contains(&p.ty.name);
            // A value-class parameter is erased to its underlying type; the null-check applies to that
            // (a primitive underlying gets none — the param is a primitive local, not a reference).
            if !p.ty.nullable && !is_type_param && ty.is_reference() {
                Some(p.name.clone())
            } else {
                None
            }
        })
        .collect()
}

fn bin_to_ir(op: BinOp) -> Option<IrBinOp> {
    Some(match op {
        BinOp::Add => IrBinOp::Add,
        BinOp::Sub => IrBinOp::Sub,
        BinOp::Mul => IrBinOp::Mul,
        BinOp::Div => IrBinOp::Div,
        BinOp::Rem => IrBinOp::Rem,
        BinOp::Lt => IrBinOp::Lt,
        BinOp::Le => IrBinOp::Le,
        BinOp::Gt => IrBinOp::Gt,
        BinOp::Ge => IrBinOp::Ge,
        BinOp::Eq => IrBinOp::Eq,
        BinOp::Ne => IrBinOp::Ne,
        BinOp::RefEq => IrBinOp::RefEq,
        BinOp::RefNe => IrBinOp::RefNe,
        BinOp::And => IrBinOp::And,
        BinOp::Or => IrBinOp::Or,
    })
}
