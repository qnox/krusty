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
    Callee, ClassId, ExprId, IrBinOp, IrClass, IrConst, IrExpr, IrFile, IrFunction, IrType,
    IrTypeOp,
};
use crate::resolve::{SymbolTable, TypeInfo};
use crate::types::Ty;

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
        classes: HashMap::new(),
        statics: HashMap::new(),
        scope: Vec::new(),
        next_value: 0,
        cur_class: None,
        cur_fn_name: String::new(),
        lambda_seq: 0,
        boxed_elem: HashMap::new(),
        local_fun_ids: HashMap::new(),
        cur_ret_ty: IrType::Unit,
        try_finally_stack: Vec::new(),
        companions: HashMap::new(),
        computed_props: HashMap::new(),
        expr_depth: 0,
        inline_lambdas: Vec::new(),
        inline_active: Vec::new(),
        reified_subst: Vec::new(),
        inline_return: Vec::new(),
    };

    // Only files of top-level functions + *simple* classes take the IR path.
    for &d in &file.decls {
        match file.decl(d) {
            Decl::Fun(_) => {} // top-level function, extension function, or `inline fun` (expanded at call sites)
            Decl::Class(c) if is_simple_class(c) => {}
            Decl::Class(c) if c.is_enum && is_simple_enum(c) => {}
            Decl::Class(c) if c.is_interface && is_simple_interface(c) => {}
            Decl::Class(c) if c.is_object && is_simple_object(c) => {}
            Decl::Property(p) if is_plain_body_prop(p) || is_computed_prop(p) => {}
            _ => return None,
        }
    }

    // Pass 1a: register classes (id, fields) and reserve method FunIds.
    for &d in &file.decls {
        if let Decl::Class(c) = file.decl(d) {
            let internal = class_internal(file, &c.name);
            // An `inner class` captures the enclosing instance: a synthetic `this$0` field of the outer
            // type, prepended as the first constructor-parameter field.
            let inner_outer: Option<String> = c.inner_of.as_ref().map(|o| class_internal(file, o));
            // Constructor-parameter fields, then class-body-property fields (initialized in `init_body`).
            let mut ctor_fields: Vec<(String, Ty)> = c
                .props
                .iter()
                .filter(|p| p.is_property)
                .map(|p| (p.name.clone(), ty_of(file, &p.ty)))
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
                    let ty =
                        p.ty.as_ref()
                            .map(|r| ty_of(file, r))
                            .unwrap_or_else(|| info.ty(p.init.unwrap()));
                    (p.name.clone(), ty)
                })
                .collect();
            let fields: Vec<(String, Ty)> = ctor_fields.into_iter().chain(body_fields).collect();
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
            let class_ty = IrType::Class {
                fq_name: internal.clone(),
                type_args: vec![],
                nullable: false,
            };
            // Resolve a base class (`: A(args)`): only a non-interface class declared in this file is
            // supported; extending a classpath/Java type isn't modeled yet → bail.
            let super_internal: Option<String> = match &c.base_class {
                Some(base) => {
                    let is_file_class = file.decls.iter().any(|&d| matches!(file.decl(d), Decl::Class(bc) if bc.name == *base && !bc.is_interface));
                    if !is_file_class {
                        return None;
                    }
                    Some(class_internal(file, base))
                }
                None => None,
            };
            let superclass = if c.is_enum {
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
                let is_file_iface = file.decls.iter().any(|&d| matches!(file.decl(d), Decl::Class(ic) if ic.name == *st && ic.is_interface));
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
                    .map_or(false, |t| t.is_interface)
                {
                    iface_internals.push(resolved);
                } else {
                    return None;
                }
            }
            let id = lo.ir.add_class(IrClass {
                fq_name: internal.clone(),
                is_value: c.is_value,
                type_param_bounds: c
                    .type_param_bounds
                    .iter()
                    .map(|(n, tr)| {
                        let bt = ty_to_ir(ty_of(file, tr));
                        (n.clone(), if tr.nullable { mark_nullable(bt) } else { bt })
                    })
                    .collect(),
                field_type_params,
                supertypes: vec![],
                fields: fields
                    .iter()
                    .map(|(n, t)| {
                        let ir = ty_to_ir(*t);
                        // A field carries its declared nullability into the IrType (`Ty` drops it). The
                        // JVM value-class pass keys boxing + null-check elision on a value class's
                        // underlying `?` (`X(val v: Int?)` → nullable `Integer`).
                        let ir = if c.props.iter().any(|p| p.name == *n && p.ty.nullable) {
                            mark_nullable(ir)
                        } else {
                            ir
                        };
                        (n.clone(), ir)
                    })
                    .collect(),
                ctor_param_count,
                // All primary-ctor params in declaration order; `is_field` = it's a `val`/`var` property.
                // An inner class's synthetic `this$0` (the outer instance) is the first field param.
                ctor_args: inner_outer
                    .iter()
                    .map(|o| (ty_to_ir(Ty::obj(o)), true))
                    .chain(c.props.iter().map(|p| {
                        // Carry a declared `?` into the ctor-param IrType (like the field), so a nullable
                        // value-class parameter erases to the boxed `X` consistently with its getter/field.
                        let t = ty_to_ir(ty_of(file, &p.ty));
                        let t = if p.ty.nullable { mark_nullable(t) } else { t };
                        (t, p.is_property)
                    }))
                    .collect(),
                init_body: None,
                methods: vec![],
                is_interface: c.is_interface,
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
                bridges: Vec::new(),
                interfaces: iface_internals,
                is_object: c.is_object,
                ctor_param_checks,
                is_companion: false,
                companion_class: None,
                field_final: inner_outer
                    .iter()
                    .map(|_| true) // `this$0` is final
                    .chain(c.props.iter().filter(|p| p.is_property).map(|p| !p.is_var))
                    .chain(
                        c.body_props
                            .iter()
                            .filter(|p| is_backing_field_prop(p))
                            .map(|p| !p.is_var),
                    )
                    .collect(),
                secondary_ctors: vec![],
                has_primary_ctor: c.has_primary_ctor,
            });
            let mut methods = HashMap::new();
            let mut method_fids = Vec::new();
            for (mi, m) in c.methods.iter().enumerate() {
                let sig = syms.classes.get(&c.name)?.methods.get(&m.name)?;
                let ret = sig.ret;
                let params: Vec<IrType> = sig.params.iter().map(|t| ty_to_ir(*t)).collect();
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
                // pass 2 and overwrite this marker. Interface defaults (kotlinc routes those through a
                // `$DefaultImpls` class) and >31 parameters (kotlinc's multi-`int` mask) aren't modeled —
                // leaving them unmarked makes an omitted-arg call bail, so the file is skipped, not wrong.
                if m.params.iter().any(|p| p.default.is_some())
                    && !c.is_interface
                    && m.params.len() <= 31
                {
                    lo.ir.fn_param_defaults.insert(fid, Vec::new());
                    lo.ir
                        .fn_param_names
                        .insert(fid, m.params.iter().map(|p| p.name.clone()).collect());
                }
                methods.insert(m.name.clone(), (mi as u32, fid, ret));
                method_fids.push(fid);
            }
            // Computed body properties → `getX()` instance methods (no backing field).
            for p in c.body_props.iter().filter(|p| is_computed_prop(p)) {
                let ty = p.ty.as_ref().map(|r| ty_of(file, r)).unwrap();
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
            // Abstract properties (`abstract val x: T`, and every interface property) → an abstract
            // `getX()` (and `setX()` for a `var`) the implementing class overrides with its field
            // accessor.
            {
                for p in c
                    .body_props
                    .iter()
                    .filter(|p| p.is_abstract || (c.is_interface && !is_computed_prop(p)))
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
                                ret: IrType::Unit,
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
            if !c.is_interface && !c.is_enum {
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
                for (pi, (pname, is_var)) in field_props.iter().enumerate() {
                    let fidx = pi + field_offset;
                    let fty = fields[fidx].1;
                    // Use the class field's IrType (carries declared `?` via `mark_nullable`), not the
                    // bare `Ty` — so a nullable value-class property getter erases consistently with the
                    // field (`z: Z1?` → `LZ1;`, not the collapsed final underlying).
                    let fty_ir = lo.ir.classes[id as usize].fields[fidx].1.clone();
                    let gname = getter_name(pname);
                    if !methods.contains_key(&gname) {
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
                                ret: IrType::Unit,
                                body: Some(body),
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
            // `companion object` with methods → a synthesized `C$Companion` class (private ctor, the
            // companion methods as instance methods) + a `Companion` field on the outer class.
            // Companion properties aren't modeled yet (their backing fields live on the outer class).
            if !c.companion_methods.is_empty() && c.companion_props.is_empty() {
                let comp_fq = format!("{internal}$Companion");
                let comp_id = lo.ir.add_class(IrClass {
                    fq_name: comp_fq.clone(),
                    is_value: false,
                    type_param_bounds: vec![],
                    field_type_params: vec![],
                    supertypes: vec![],
                    fields: vec![],
                    ctor_param_count: 0,
                    ctor_args: vec![],
                    init_body: None,
                    methods: vec![],
                    is_interface: false,
                    superclass: "kotlin/Any".to_string(),
                    super_args: vec![],
                    enum_entries: vec![],
                    enum_entry_subclass: vec![],
                    enum_entry_of: None,
                    prop_ref: None,
                    bridges: vec![],
                    interfaces: vec![],
                    is_object: false,
                    ctor_param_checks: vec![],
                    is_companion: true,
                    companion_class: None,
                    field_final: vec![],
                    secondary_ctors: vec![],
                    has_primary_ctor: true,
                });
                let csig = syms.classes.get(&c.name)?;
                let mut cmethods = HashMap::new();
                let mut cmethod_fids = Vec::new();
                for (mi, m) in c.companion_methods.iter().enumerate() {
                    let sig = csig.static_methods.get(&m.name)?;
                    let ret = sig.ret;
                    let params: Vec<IrType> = sig.params.iter().map(|t| ty_to_ir(*t)).collect();
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
            if !c.delegations.is_empty() {
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
                let recv_ty = ty_of(file, recv_ref);
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
                let params: Vec<IrType> = sig
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
                let mut params: Vec<IrType> = Vec::new();
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
            let ty =
                p.ty.as_ref()
                    .map(|r| ty_of(file, r))
                    .unwrap_or_else(|| info.ty(p.init.unwrap()));
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
            }
        }
    }

    // Pass 2: lower bodies.
    for &d in &file.decls {
        match file.decl(d) {
            Decl::Fun(f) if f.is_inline => {} // inline functions are expanded at call sites, not emitted
            Decl::Fun(f) => {
                lo.scope.clear();
                lo.next_value = 0;
                lo.cur_class = None;
                lo.cur_fn_name = f.name.clone();
                lo.lambda_seq = 0;
                let (fid, sig) = if let Some(recv_ref) = &f.receiver {
                    // Extension body: `this` is the receiver (parameter 0), then the declared params.
                    let recv_ty = ty_of(file, recv_ref);
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
                for (p, t) in f.params.iter().zip(&sig.params) {
                    let v = lo.fresh_value();
                    lo.scope.push((p.name.clone(), v, *t));
                }
                let ret_ty = lo.ir.functions[fid as usize].ret.clone();
                lo.lower_body(&f.body, &ret_ty, fid)?;
            }
            Decl::Class(c) => {
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
                if !c.is_interface {
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
                if !c.is_interface {
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
                                    let ip: Vec<IrType> =
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
                // An interface's methods are abstract — leave their bodies `None`; nothing to lower.
                if c.is_interface {
                    continue;
                }
                for m in &c.methods {
                    // An abstract method has no body — leave its `IrFunction.body` as `None`.
                    if matches!(m.body, FunBody::None) {
                        continue;
                    }
                    let (_, fid, _) = lo.classes[&internal].methods[&m.name];
                    lo.scope.clear();
                    lo.next_value = 0;
                    lo.cur_class = Some(internal.clone());
                    lo.cur_fn_name = m.name.clone();
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
                        && !c.is_interface
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
                // where `A(val x = …)` has defaulted parameters) — krusty doesn't fill super default
                // arguments, so the `super(…)` call shape wouldn't match. Bail rather than miscompile.
                if let Some(sup) = lo.classes[&internal]
                    .super_internal
                    .clone()
                    .and_then(|s| lo.classes.get(&s))
                {
                    let sup_params = lo.ir.classes[sup.id as usize].ctor_param_count as usize;
                    // For a class with NO primary constructor the base arguments are supplied by each
                    // secondary ctor's `super(…)` (validated in the secondary-ctor lowering), not by a
                    // supertype-list `: Base(args)` — so this `base_args` arity check doesn't apply.
                    if sup_params > c.base_args.len() && c.has_primary_ctor {
                        return None;
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
                    let super_field_tys: Vec<IrType> = lo.classes[&internal]
                        .super_internal
                        .clone()
                        .and_then(|s| lo.classes.get(&s).map(|sup| sup.id))
                        .map(|sid| {
                            let sup = &lo.ir.classes[sid as usize];
                            if sup.ctor_args.is_empty() {
                                let n = sup.ctor_param_count as usize;
                                sup.fields[..n].iter().map(|(_, t)| t.clone()).collect()
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
                if !c.init_order.is_empty() && c.has_primary_ctor {
                    let class_id = lo.classes[&internal].id;
                    let ctor_count = lo.ir.classes[class_id as usize].ctor_param_count;
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
                                    .1
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
                    let primary_param_tys: Vec<IrType> = {
                        let n = lo.ir.classes[class_id as usize].ctor_param_count as usize;
                        lo.ir.classes[class_id as usize].fields[..n]
                            .iter()
                            .map(|(_, t)| t.clone())
                            .collect()
                    };
                    // The IR param types of every secondary ctor (for resolving a sibling `this(…)`).
                    let sec_param_tys: Vec<Vec<IrType>> = c
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
                            Vec<IrType>,
                            bool,
                        ) = match &sc.delegation {
                            ast::CtorDelegation::This(args) => {
                                let target = if c.has_primary_ctor {
                                    primary_param_tys.clone()
                                } else {
                                    // Pick the sibling secondary ctor this `this(…)` targets: prefer the
                                    // one whose parameter types accept the arguments, else the unique
                                    // same-arity ctor. (Ambiguity type-matching can't resolve bails.)
                                    let arg_irs: Vec<IrType> =
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
                                            let same: Vec<&Vec<IrType>> = sec_param_tys
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
                if c.is_enum {
                    let class_id = lo.classes[&internal].id;
                    let ctor_count = lo.ir.classes[class_id as usize].ctor_param_count as usize;
                    let field_tys: Vec<IrType> = lo.ir.classes[class_id as usize].fields
                        [..ctor_count]
                        .iter()
                        .map(|(_, t)| t.clone())
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
                        if body.is_empty() {
                            continue;
                        }
                        let entry_name = &c.enum_entries[ei];
                        let sub_fq = format!("{internal}${entry_name}");
                        let sub_id = lo.ir.add_class(IrClass {
                            fq_name: sub_fq.clone(),
                            is_value: false,
                            type_param_bounds: vec![],
                            field_type_params: vec![],
                            supertypes: vec![],
                            fields: vec![],
                            ctor_param_count: 0,
                            ctor_args: vec![],
                            init_body: None,
                            methods: vec![],
                            is_interface: false,
                            superclass: internal.clone(),
                            super_args: vec![],
                            enum_entries: vec![],
                            enum_entry_subclass: vec![],
                            enum_entry_of: Some(field_tys.clone()),
                            prop_ref: None,
                            bridges: vec![],
                            interfaces: vec![],
                            is_object: false,
                            ctor_param_checks: vec![],
                            is_companion: false,
                            companion_class: None,
                            field_final: vec![],
                            secondary_ctors: vec![],
                            has_primary_ctor: true,
                        });
                        let mut mfids = Vec::new();
                        for bm in body {
                            // The override conforms to the abstract member it overrides — use that
                            // signature for the emitted descriptor (kotlinc's erased override shape).
                            let sig = syms.classes.get(&c.name)?.methods.get(&bm.name)?.clone();
                            let params: Vec<IrType> =
                                sig.params.iter().map(|t| ty_to_ir(*t)).collect();
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
                            lo.cur_class = Some(internal.clone());
                            lo.cur_fn_name = bm.name.clone();
                            lo.lambda_seq = 0;
                            let this_v = lo.fresh_value();
                            lo.scope
                                .push(("this".to_string(), this_v, Ty::obj(&internal)));
                            for (p, t) in bm.params.iter().zip(&sig.params) {
                                let v = lo.fresh_value();
                                lo.scope.push((p.name.clone(), v, *t));
                            }
                            let ret_ty = ty_to_ir(sig.ret);
                            lo.lower_body(&bm.body, &ret_ty, fid)?;
                            mfids.push(fid);
                        }
                        lo.ir.classes[sub_id as usize].methods = mfids;
                        lo.ir.classes[class_id as usize].enum_entry_subclass[ei] = Some(sub_fq);
                    }
                }
            }
            Decl::Property(p) => {
                lo.scope.clear();
                lo.next_value = 0;
                lo.cur_class = None;
                if let Some(&(fid, ty)) = lo.computed_props.get(&p.name) {
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
        c.bridges.iter().any(|b| {
            matches!(&b.concrete_ret, IrType::Class { fq_name, .. } if fq_name == "java/lang/Void")
        })
    });
    if nothing_bridge {
        return None;
    }
    Some(lo.ir)
}

/// A class is in the IR subset if: a primary constructor of only `val`/`var` properties, no base
/// class/interfaces, no body properties, no companion/secondary/init, and methods (expr- or
/// block-bodied) without an extension receiver.
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
    // A `companion object` with only methods is supported (synthesized `C$Companion` class); a
    // companion with properties (`val`/`const val`) is not yet.
    !c.is_object && !c.is_enum && !c.is_interface && c.companion_props.is_empty()
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
        && c.body_props.iter().all(|p| is_plain_body_prop(p) || is_computed_prop(p) || p.is_abstract || is_deferred_val_prop(p))
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
    c.is_enum
        && c.companion_methods.is_empty() && c.companion_props.is_empty()
        && c.secondary_ctors.is_empty() && c.supertypes.is_empty()
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
    c.is_object
        && c.base_class.is_none() && c.supertypes.is_empty()
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
    c.is_interface
        && c.companion_methods.is_empty() && c.companion_props.is_empty()
        && c.props.is_empty()
        // Abstract properties (`val x: T`, no initializer/getter) become abstract `getX()`/`setX()`;
        // a property with an initializer or custom getter (an interface can't have a backing field)
        // isn't modeled.
        && c.body_props.iter().all(|p| p.init.is_none() && p.getter.is_none() && p.ty.is_some())
        && c.methods.iter().all(|m| m.receiver.is_none() && matches!(m.body, FunBody::None))
}

/// A class-body property that is a plain backing field: a normal (non-extension) `val`/`var` with an
/// initializer and no custom getter/setter and not `lateinit`.
fn is_plain_body_prop(p: &ast::PropDecl) -> bool {
    p.receiver.is_none()
        && !p.is_lateinit
        && p.getter.is_none()
        && p.setter.is_none()
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

/// A computed property `val x: T get() = expr` — a custom getter, no backing field (no initializer),
/// immutable (no setter). Compiled to a `getX()` accessor; reads call it.
fn is_computed_prop(p: &ast::PropDecl) -> bool {
    p.receiver.is_none()
        && !p.is_lateinit
        && !p.is_var
        && p.init.is_none()
        && p.getter.is_some()
        && p.setter.is_none()
        && p.ty.is_some()
}

/// A body property with a real backing field — neither a computed property (custom getter, no field)
/// nor an `abstract` one (emitted as an abstract `getX()`, the field lives on the subclass).
fn is_backing_field_prop(p: &ast::PropDecl) -> bool {
    !is_computed_prop(p) && !p.is_abstract
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
    classes: HashMap<String, ClassInfo>,
    /// Top-level property name → (index into `ir.statics`, type).
    statics: HashMap<String, (u32, Ty)>,
    scope: Vec<(String, u32, Ty)>,
    next_value: u32,
    cur_class: Option<String>,
    /// Name of the enclosing function/method being lowered — used to name synthesized lambda impl
    /// methods `<enclosing>$lambda$<n>` (matching kotlinc).
    cur_fn_name: String,
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
    cur_ret_ty: IrType,
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
    /// Current expression-lowering recursion depth — guards against a stack overflow on a pathologically
    /// deep expression (a stress test with thousands of nested operators): past the limit, lowering
    /// bails (the file is skipped, never miscompiled or crashed).
    expr_depth: u32,
    /// Active inlined-lambda parameters while expanding an `inline fun` body, as a stack so nested
    /// inline calls compose. Each entry is `(param name, lambda parameter names, lambda body, lambda
    /// parameter types)`: a call `param(args)` in the inline body inlines the lambda body in place.
    inline_lambdas: Vec<(String, Vec<String>, AstExprId, Vec<Ty>)>,
    /// Names of `inline fun`s currently being expanded — a (self- or mutually-) recursive inline call
    /// would expand forever, so re-entering an active name bails (the file is skipped).
    inline_active: Vec<String>,
    /// Active reified type-parameter bindings while expanding a `<reified T>` inline fn: `T` → the
    /// call's actual type argument. Consulted by `subst_type_ref` so `is T`/`as T`/`T::class` in the
    /// inlined body specialize to the concrete type. A stack — nested reified inline calls compose.
    reified_subst: Vec<std::collections::HashMap<String, ast::TypeRef>>,
    /// Active inline-fn return targets while expanding an `inline fun` whose body has `return`: each is
    /// `(result slot, end label, return type)`. A `return x` in the inlined body lowers to `result = x;
    /// break@end` (the body is wrapped in a `do { … } while(false)` labeled `end`), turning the function
    /// return into a jump to the body's end. Innermost (`.last()`) is the enclosing inline fn; lambda
    /// args with returns are pre-bailed, so a `return` always belongs to the innermost inline body.
    inline_return: Vec<(u32, String, IrType)>,
}

impl<'a> Lower<'a> {
    fn fresh_value(&mut self) -> u32 {
        let v = self.next_value;
        self.next_value += 1;
        v
    }

    /// The parameter types of a class's superclass constructor (`super(args)` targets these). Empty for
    /// `java/lang/Object` or a base whose IR class isn't in this file (then we can't model the call).
    fn super_ctor_param_tys(&self, internal: &str) -> Vec<IrType> {
        self.classes[internal]
            .super_internal
            .clone()
            .and_then(|s| self.classes.get(&s).map(|sup| sup.id))
            .map(|sid| {
                let sup = &self.ir.classes[sid as usize];
                if sup.ctor_args.is_empty() {
                    let n = sup.ctor_param_count as usize;
                    sup.fields[..n].iter().map(|(_, t)| t.clone()).collect()
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
                        .1
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
                let params: Vec<IrType> = cs.ctor_params.iter().map(|t| ty_to_ir(*t)).collect();
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
            crate::libraries::resolve_constructor(&*self.syms.libraries, internal, &arg_tys)?;
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
            Stmt::Return(_) | Stmt::Break(_) | Stmt::Continue(_) => true,
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
            let (call, log_ty) =
                if let Some((class, index, _, _)) = self.resolve_method(&internal, &comp) {
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
                } else if let Some(m) =
                    crate::libraries::resolve_instance(&*self.syms.libraries, &internal, &comp, &[])
                {
                    let is_iface = self
                        .syms
                        .libraries
                        .resolve_type(&internal)
                        .map_or(false, |t| t.is_interface);
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
                    let m = crate::libraries::resolve_instance(
                        &*self.syms.libraries,
                        &internal,
                        "get",
                        &[Ty::Int],
                    )?;
                    let is_iface = self
                        .syms
                        .libraries
                        .resolve_type(&internal)
                        .map_or(false, |t| t.is_interface);
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
            let v = self.fresh_value();
            self.scope.push((name.clone(), v, log_ty));
            out.push(self.ir.add_expr(IrExpr::Variable {
                index: v,
                ty: ty_to_ir(log_ty),
                init: Some(call),
            }));
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
        let arity = sig.params.len();
        // A lambda inside a class method could capture `this`/fields — not modeled yet.
        if self.cur_class.is_some() {
            return None;
        }
        // A `Nothing`-returning lambda (every path throws) isn't modeled — bail.
        if sig.ret == Ty::Nothing {
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
        let ve = self.expr(body);
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
        let mut params_ir: Vec<IrType> = captures.iter().map(|(_, _, t)| ty_to_ir(*t)).collect();
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
        Some(self.ir.add_expr(IrExpr::Lambda {
            impl_fn: fid,
            arity: arity as u8,
            captures: capture_vals,
            sam: sam.map(|(i, m, _)| (i, m)),
            inline_body: Some(inline_body),
        }))
    }

    /// Register a synthesized instance method (a real `IrFunction` with an IR body) on a class, so
    /// it resolves like any other method and the generic emitter handles it — no backend special-case.
    fn add_synth_method(
        &mut self,
        internal: &str,
        class_id: ClassId,
        name: &str,
        params: Vec<IrType>,
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
        for (iface_name, delegate) in &c.delegations {
            let delegate_idx = self
                .classes
                .get(internal)?
                .fields
                .iter()
                .position(|(n, _)| n == delegate)? as u32;
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
                let params_ir: Vec<IrType> = params.iter().map(|t| ty_to_ir(*t)).collect();
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
        matches!(
            self.ir.classes[class_id as usize].fields[i].1,
            IrType::Class { nullable: true, .. }
        )
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
            let params: Vec<IrType> = fields.iter().map(|(_, t)| ty_to_ir(*t)).collect();
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
                    data_array_param(&self.ir.classes[class_id as usize].fields[i].1)
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

    fn class_decl(&self, name: &str) -> Option<&ast::ClassDecl> {
        self.afile
            .decls
            .iter()
            .find_map(|&d| match self.afile.decl(d) {
                Decl::Class(c) if c.name == name => Some(c),
                _ => None,
            })
    }

    /// Lower a call's arguments, filling omitted trailing parameters from their **constant-literal**
    /// defaults (`fun f(x: Int = 5)` called `f()`). A non-literal default (one referencing other
    /// params or `this`) needs the `$default` synthetic method krusty doesn't emit yet → `None`.
    fn lower_args_defaulted(
        &mut self,
        call: AstExprId,
        param_meta: &[(String, Option<AstExprId>)],
        args: &[AstExprId],
        ir_params: &[IrType],
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
        for (i, &arg) in args.iter().enumerate() {
            match names.get(i).and_then(|o| o.as_ref()) {
                None => {
                    if pos >= n {
                        return None;
                    }
                    slot[pos] = Some(arg);
                    pos += 1;
                }
                Some(nm) => {
                    let idx = param_meta.iter().position(|(name, _)| name == nm)?;
                    if idx >= n || slot[idx].is_some() {
                        return None;
                    }
                    slot[idx] = Some(arg);
                }
            }
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
            let idx = cls.fields.iter().position(|(n, _)| n == name)?;
            cls.fields[idx].1.clone()
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
            is_value: false,
            type_param_bounds: vec![],
            field_type_params: vec![],
            supertypes: vec![],
            fields: vec![],
            ctor_param_count: 0,
            ctor_args: vec![],
            init_body: None,
            methods: vec![],
            is_interface: false,
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
            bridges: vec![],
            interfaces: vec![],
            is_object: false,
            ctor_param_checks: vec![],
            is_companion: false,
            companion_class: None,
            field_final: vec![],
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
    fn lower_method_ref(
        &mut self,
        recv: AstExprId,
        name: &str,
        params: &[Ty],
        ret: Ty,
    ) -> Option<u32> {
        if ret == Ty::Unit || ret == Ty::Nothing {
            return None;
        }
        let Expr::Name(rn) = self.afile.expr(recv).clone() else {
            return None;
        };
        // Bound `obj::m`: `rn` is an in-scope value, captured into the closure. Unbound `Type::m`:
        // `rn` is a class; the receiver is the reference's first parameter.
        let (bound_capture, recv_ty): (Option<u32>, Ty) = match self.lookup(&rn) {
            Some((v, ty)) => (Some(v), ty),
            None => {
                let internal = class_internal(self.afile, &rn);
                if !self.classes.contains_key(&internal) {
                    return None;
                }
                (None, *params.first()?)
            }
        };
        let internal = recv_ty.obj_internal()?.to_string();
        let (class_id, index, _fid, _mret) = self.resolve_method(&internal, name)?;
        // The method's own parameter types: all of `params` for a bound ref (the receiver is captured),
        // `params[1..]` for an unbound ref (the receiver is the first parameter).
        let method_params: Vec<Ty> = if bound_capture.is_some() {
            params.to_vec()
        } else {
            params[1..].to_vec()
        };
        // Impl value layout: value 0 = receiver, values 1.. = the method arguments.
        let recv_v = self.ir.add_expr(IrExpr::GetValue(0));
        let arg_vs: Vec<Option<u32>> = (0..method_params.len() as u32)
            .map(|i| Some(self.ir.add_expr(IrExpr::GetValue(i + 1))))
            .collect();
        let mc = self.ir.add_expr(IrExpr::MethodCall {
            class: class_id,
            index,
            receiver: recv_v,
            args: arg_vs,
        });
        let ret_e = self.ir.add_expr(IrExpr::Return(Some(mc)));
        let block = self.ir.add_expr(IrExpr::Block {
            stmts: vec![ret_e],
            value: None,
        });
        let impl_name = format!("{}$methodref${}", self.cur_fn_name, self.lambda_seq);
        self.lambda_seq += 1;
        let mut impl_params: Vec<IrType> = vec![ty_to_ir(recv_ty)];
        impl_params.extend(method_params.iter().map(|t| ty_to_ir(*t)));
        let fid = self.ir.add_fun(IrFunction {
            name: impl_name,
            params: impl_params,
            ret: ty_to_ir(ret),
            body: Some(block),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        let captures = match bound_capture {
            Some(v) => vec![self.ir.add_expr(IrExpr::GetValue(v))],
            None => vec![],
        };
        Some(self.ir.add_expr(IrExpr::Lambda {
            impl_fn: fid,
            arity: params.len() as u8,
            captures,
            sam: None,
            inline_body: None,
        }))
    }

    /// For an unqualified call inside an inner class, resolve `name` as an ENCLOSING method (reached
    /// through `this$0`). Returns `(method_class, method_index, method_fid, inner_class_id)`.
    fn inner_outer_method(&self, name: &str) -> Option<(ClassId, u32, u32, ClassId)> {
        let cur = self.cur_class.as_ref()?;
        let cur_id = self.classes.get(cur)?.id;
        let outer = match self.ir.classes[cur_id as usize].fields.first() {
            Some((n0, IrType::Class { fq_name, .. })) if n0 == "this$0" => fq_name.clone(),
            _ => return None,
        };
        let (c, i, f, _) = self.resolve_method(&outer, name)?;
        Some((c, i, f, cur_id))
    }

    fn resolve_method(&self, internal: &str, name: &str) -> Option<(ClassId, u32, u32, Ty)> {
        let mut cur = Some(internal.to_string());
        while let Some(ci_name) = cur {
            let ci = self.classes.get(&ci_name)?;
            if let Some(&(idx, fid, ret)) = ci.methods.get(name) {
                return Some((ci.id, idx, fid, ret));
            }
            cur = ci.super_internal.clone();
        }
        None
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
                        && !self.syms.funs.contains_key(n);
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
            crate::libraries::resolve_instance(&*self.syms.libraries, internal, "iterator", &[])
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
        let hasnext_m = crate::libraries::resolve_instance(
            &*self.syms.libraries,
            &iter_internal,
            "hasNext",
            &[],
        )?;
        let next_m =
            crate::libraries::resolve_instance(&*self.syms.libraries, &iter_internal, "next", &[])?;
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
            .map_or(false, |t| t.is_interface);
        let iter_iface = self
            .syms
            .libraries
            .resolve_type(&iter_internal)
            .map_or(false, |t| t.is_interface);
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

    pub(crate) fn lower_arg(&mut self, arg: AstExprId, target: &IrType) -> Option<u32> {
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
            && matches!(target, IrType::Class { fq_name, .. } if fq_name == "kotlin/UInt" || fq_name == "kotlin/ULong")
        {
            return None;
        }
        if at.is_primitive() && target_ref {
            Some(self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: e,
                type_operand: target.clone(),
            }))
        } else if at.is_reference()
            && !target_ref
            && *target != IrType::Unit
            && *target != IrType::Error
        {
            Some(self.ir.add_expr(IrExpr::TypeOp {
                op: IrTypeOp::ImplicitCoercion,
                arg: e,
                type_operand: target.clone(),
            }))
        } else if at.is_primitive()
            && !target_ref
            && *target != IrType::Error
            && *target != IrType::Unit
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

    /// Resolve an `is`/`as` target `TypeRef` to a known **reference** `Ty` (`String` or a class in
    /// this IR); returns `None` to bail for primitives, nullables, or unknown types.
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

    fn lower_body(&mut self, body: &FunBody, ret_ty: &IrType, fid: u32) -> Option<()> {
        self.cur_ret_ty = ret_ty.clone();
        // Defensive: the stack is push/pop-balanced within a body, but a bail mid-lowering of a previous
        // body must not leak an enclosing `finally` into this one.
        self.try_finally_stack.clear();
        let b = match body {
            FunBody::Expr(e) => {
                let diverges = self.info.ty(*e) == Ty::Nothing;
                let stmts = if *ret_ty == IrType::Unit || diverges {
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

    fn block_as_body(&mut self, block: AstExprId, ret_ty: &IrType) -> Option<u32> {
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
            if *ret_ty != IrType::Unit && !diverges && tt == Ty::Unit {
                self.scope.truncate(depth);
                return None;
            }
            let ve = self.expr(t)?;
            if *ret_ty == IrType::Unit || diverges {
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
            Stmt::Return(e) => {
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
                    if rty != IrType::Unit {
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
                    Some(e)
                        if self.cur_ret_ty != IrType::Unit && self.info.ty(e) != Ty::Nothing =>
                    {
                        let rt = self.cur_ret_ty.clone();
                        Some(self.lower_arg(e, &rt)?)
                    }
                    Some(e) => Some(self.expr(e)?),
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
                    // A nullable primitive (`Char?`) is its boxed wrapper, not the primitive — keep the
                    // slot a reference (consistent with the checker), else a boxed value is stored raw.
                    Some(r)
                        if Ty::from_name(&r.name).map_or(false, |t| {
                            r.nullable
                                && !t.is_reference()
                                && crate::resolve::nullable_prim_wrapper(t).is_some()
                        }) =>
                    {
                        Ty::obj(
                            crate::resolve::nullable_prim_wrapper(Ty::from_name(&r.name).unwrap())
                                .unwrap(),
                        )
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
                                            matches!(self.ir.functions[fid as usize].ret, IrType::Class { nullable: true, .. })
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
                    self.cur_class
                        .as_ref()
                        .and_then(|c| self.classes.get(c))
                        .and_then(|ci| {
                            ci.fields
                                .iter()
                                .position(|(fn_, _)| *fn_ == name)
                                .map(|i| (this_v, ci.id, i as u32, ty_to_ir(ci.fields[i].1)))
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
                } else if let Some((facade, ty, is_var)) =
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
                            ret: crate::ir::IrType::Unit,
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
                    // `this`) goes through its getter/setter accessors.
                    let own = self
                        .cur_class
                        .as_ref()
                        .and_then(|c| self.classes.get(c))
                        .and_then(|ci| {
                            ci.fields
                                .iter()
                                .position(|(fn_, _)| *fn_ == name)
                                .map(|i| (ci.id, i as u32))
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
                                    ret: crate::ir::IrType::Unit,
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
                // `coll[i] = v` on a library type → its `set(index, value)` operator member, discarding
                // the returned previous element (an array set stays the `kotlin/Array.set` intrinsic).
                if let Ty::Obj(internal, _) = at {
                    if at.array_elem().is_none() {
                        let (it, vt) = (self.info.ty(index), self.info.ty(value));
                        // `MutableList.set(Int, E)`, or `MutableMap.put(K, V)` — Kotlin's `m[k] = v`
                        // operator maps to `put` on a map.
                        let resolved = crate::libraries::resolve_instance(
                            &*self.syms.libraries,
                            internal,
                            "set",
                            &[it, vt],
                        )
                        .map(|m| ("set", m))
                        .or_else(|| {
                            crate::libraries::resolve_instance(
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
                                .map_or(false, |t| t.is_interface);
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
                // hoisted bound
                let end_e = self.lower_arg(range.end, &elem_ir)?;
                let end_v = self.fresh_value();
                let var_end = self.ir.add_expr(IrExpr::Variable {
                    index: end_v,
                    ty: elem_ir.clone(),
                    init: Some(end_e),
                });
                // condition. Signed/Long use the direct comparison opcode; unsigned compares via the JDK
                // `compareUnsigned(i, end) <op> 0` (a signed `<=` would misorder values past the sign bit).
                let cmp = match range.kind {
                    RangeKind::Through => IrBinOp::Le,
                    RangeKind::Until => IrBinOp::Lt,
                    RangeKind::DownTo => IrBinOp::Ge,
                };
                let gi = self.ir.add_expr(IrExpr::GetValue(i_v));
                let ge = self.ir.add_expr(IrExpr::GetValue(end_v));
                let cond = if elem.is_unsigned() {
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
                // The step is evaluated ONCE (after the bounds, before the loop), not per iteration — a
                // side-effecting `step` (`a step logged(2)`) must run a single time. Hoist it to a temp.
                let var_step = match range.step {
                    Some(e) => {
                        // Coerce the step to the counter's type — `0L..n step 3` adapts the `Int` step `3`
                        // to `Long`, else an `int` would be stored into a `long` slot (a verify error).
                        let sv = self.lower_arg(e, &elem_ir)?;
                        let step_v = self.fresh_value();
                        Some((
                            self.ir.add_expr(IrExpr::Variable {
                                index: step_v,
                                ty: elem_ir.clone(),
                                init: Some(sv),
                            }),
                            step_v,
                        ))
                    }
                    None => None,
                };
                let step = match var_step {
                    Some((_, step_v)) => self.ir.add_expr(IrExpr::GetValue(step_v)),
                    None => self.ir.add_expr(IrExpr::Const(one)),
                };
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
                // For an exclusive `until` the counter never equals `end`, and a non-1 `step` may skip it
                // — harmless either way (the `cond` ends the loop).
                // A non-unit `step` on a signed `Int`/`Long`-family range may never land exactly on `end`,
                // so the `i == end` break can't fire and `i ± step` would wrap PAST `end` and loop forever
                // (e.g. `MaxI-5..MaxI step 3`). Break when the NEXT value would pass `end` OR wraps around
                // (`next < i` when ascending / `next > i` when descending detects the overflow) — the
                // overflow-safe shape, matching kotlinc's `getProgressionLastElement` semantics, without
                // needing a wider accumulator (so it works for `Long` too).
                let signed_int_family =
                    matches!(elem, Ty::Int | Ty::Long | Ty::Char | Ty::Short | Ty::Byte);
                let at_end = if range.step.is_some() && signed_int_family {
                    let step_e = |this: &mut Self| match var_step {
                        Some((_, step_v)) => this.ir.add_expr(IrExpr::GetValue(step_v)),
                        None => this.ir.add_expr(IrExpr::Const(IrConst::Int(1))),
                    };
                    // next = i ± step  (passes `end`?)
                    let i1 = self.ir.add_expr(IrExpr::GetValue(i_v));
                    let s1 = step_e(self);
                    let next1 = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: inc_op,
                        lhs: i1,
                        rhs: s1,
                    });
                    let ec = self.ir.add_expr(IrExpr::GetValue(end_v));
                    // Through → next > end; Until → next >= end; DownTo → next < end.
                    let past_op = match range.kind {
                        RangeKind::Through => IrBinOp::Gt,
                        RangeKind::Until => IrBinOp::Ge,
                        RangeKind::DownTo => IrBinOp::Lt,
                    };
                    let past_end = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: past_op,
                        lhs: next1,
                        rhs: ec,
                    });
                    // wrap-around: ascending `next < i`, descending `next > i`.
                    let i2 = self.ir.add_expr(IrExpr::GetValue(i_v));
                    let s2 = step_e(self);
                    let next2 = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: inc_op,
                        lhs: i2,
                        rhs: s2,
                    });
                    let i3 = self.ir.add_expr(IrExpr::GetValue(i_v));
                    let wrap_op = if matches!(range.kind, RangeKind::DownTo) {
                        IrBinOp::Gt
                    } else {
                        IrBinOp::Lt
                    };
                    let wrapped = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: wrap_op,
                        lhs: next2,
                        rhs: i3,
                    });
                    self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: IrBinOp::Or,
                        lhs: past_end,
                        rhs: wrapped,
                    })
                } else {
                    let ic = self.ir.add_expr(IrExpr::GetValue(i_v));
                    let ec = self.ir.add_expr(IrExpr::GetValue(end_v));
                    self.ir.add_expr(IrExpr::PrimitiveBinOp {
                        op: IrBinOp::Eq,
                        lhs: ic,
                        rhs: ec,
                    })
                };
                let brk = self.ir.add_expr(IrExpr::Break { label: None });
                let if_break = self.ir.add_expr(IrExpr::When {
                    branches: vec![(Some(at_end), brk)],
                });
                let update = self.ir.add_expr(IrExpr::Block {
                    stmts: vec![if_break, inc],
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
                let mut prologue = vec![var_i, var_end];
                if let Some((vs, _)) = var_step {
                    prologue.push(vs);
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
        // Evaluate the array once into a temp.
        let arr_v = self.fresh_value();
        let arr_val = self.expr(iterable)?;
        let var_arr = self.ir.add_expr(IrExpr::Variable {
            index: arr_v,
            ty: ty_to_ir(it_ty),
            init: Some(arr_val),
        });
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
        Some(self.ir.add_expr(IrExpr::Block {
            stmts: vec![var_arr, var_i, var_n, wh],
            value: None,
        }))
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
        if f.params.iter().any(|p| p.default.is_some() || p.is_vararg) {
            return None;
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
        if sig_params.len() != args.len() || sig_params.len() != pnames.len() {
            return None;
        }
        // A (self- or mutually-) recursive inline call would expand forever — bail.
        if self.inline_active.iter().any(|n| n == fname) {
            return None;
        }
        // Specialize non-reified type params from the actual arguments: a parameter declared `T` binds
        // `T` to the call's concrete argument type, so the inlined body (and a lambda parameter typed `T`)
        // uses e.g. `Int` rather than the erased `Any`. Empty for a non-generic inline fn (no change).
        let tparams: std::collections::HashSet<&str> =
            f.type_params.iter().map(String::as_str).collect();
        let mut tbinds: std::collections::HashMap<String, Ty> = std::collections::HashMap::new();
        for (i, p) in f.params.iter().enumerate() {
            if tparams.contains(p.ty.name.as_str())
                && !matches!(self.afile.expr(args[i]), Expr::Lambda { .. })
            {
                tbinds
                    .entry(p.ty.name.clone())
                    .or_insert(self.info.ty(args[i]));
            }
        }
        let active_depth = self.inline_active.len();
        self.inline_active.push(fname.to_string());
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
            if let Ty::Fun(fnsig) = pty {
                // A lambda parameter: require a literal lambda argument with no non-local return.
                if let Expr::Lambda {
                    params,
                    body: lbody,
                } = self.afile.expr(args[i]).clone()
                {
                    // A single-parameter lambda may name its parameter implicitly as `it`.
                    let params = if params.is_empty() && fnsig.params.len() == 1 {
                        vec!["it".to_string()]
                    } else {
                        params
                    };
                    if body_has_return(self.afile, lbody) || params.len() != fnsig.params.len() {
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
                    self.inline_lambdas
                        .push((pnames[i].clone(), params, lbody, lam_param_tys));
                } else {
                    self.scope.truncate(depth);
                    self.inline_lambdas.truncate(lam_depth);
                    return None;
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
            let unit_ret = ret_ty == IrType::Unit;
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
                } else if let IrType::Class { fq_name, .. } = &ret_ty {
                    match fq_name.as_str() {
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
        let (_, lam_params, lam_body, lam_param_tys) = self.inline_lambdas[idx].clone();
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
        let body_val = self.expr(lam_body);
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
            .is_some_and(|t| t.is_interface);
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
                    crate::libraries::resolve_instance(&*self.syms.libraries, &internal, &c, &[])
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
                let internal = if rty == Ty::String {
                    "java/lang/String".to_string()
                } else {
                    rty.obj_internal()?.to_string()
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
                    .map_or(false, |t| t.is_interface);
                let member = match args {
                    Some(args) => {
                        if let Some((class, index, fid, _)) = self.resolve_method(&internal, &name)
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
                            let arg_tys: Vec<Ty> = args.iter().map(|&a| self.info.ty(a)).collect();
                            let m = crate::libraries::resolve_instance(
                                &*self.syms.libraries,
                                &internal,
                                &name,
                                &arg_tys,
                            )?;
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
                        }
                    }
                    None => {
                        if let Some((fclass, idx, _)) = self.resolve_field(&internal, &name) {
                            let owner_internal = self.ir.classes[fclass as usize].fq_name.clone();
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
                                    crate::libraries::resolve_instance(
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
                };
                // A nullable-primitive result (`s?.length` : `Int?`): box the primitive member value so
                // both `when` branches are the wrapper reference (the other branch is `null`).
                let member = if result_ty
                    .obj_internal()
                    .and_then(crate::resolve::prim_of_wrapper)
                    .is_some()
                {
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
                    if let Some(lp) = lty.obj_internal().and_then(crate::resolve::prim_of_wrapper) {
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
                    return self.lower_method_ref(recv, &name, &sig.params, sig.ret);
                }
                // Constructor reference `::A` (the name is a class, not a function): synthesize a
                // static impl `(ctor params) -> new A(params)` and wrap it in a closure, exactly as a
                // lambda `{ a -> A(a) }` would lower. Only the simple primary-constructor positional
                // case (the closure's arity matches the constructor's field params) is modeled.
                if !self.syms.funs.contains_key(&name) {
                    let ci = self.class_of(sig.ret)?;
                    let class_id = ci.id;
                    let ctor_count = self.ir.classes[class_id as usize].ctor_param_count as usize;
                    let ctor_args = self.ir.classes[class_id as usize].ctor_args.clone();
                    let field_tys: Vec<IrType> = if ctor_args.is_empty() {
                        self.ir.classes[class_id as usize].fields[..ctor_count]
                            .iter()
                            .map(|(_, t)| t.clone())
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
                // A generic referenced function erases its type parameters — not modeled.
                if self.ir.functions[fid as usize].params.len() != arity {
                    return None;
                }
                if self
                    .top_fun_decl(&name)
                    .map_or(false, |f| !f.type_params.is_empty())
                {
                    return None;
                }
                // A `Unit`-returning function's method handle returns `void`; the SAM's `invoke` must
                // return the `kotlin/Unit` singleton (LambdaMetafactory would adapt `void` to `null`).
                // Synthesize a wrapper `(params) -> { foo(params); return Unit.INSTANCE }`. `Nothing`
                // (every path throws) stays unmodeled.
                if ret == IrType::Nothing {
                    return None;
                }
                if ret == IrType::Unit {
                    let params = self.ir.functions[fid as usize].params.clone();
                    let argvals: Vec<u32> = (0..arity as u32)
                        .map(|i| self.ir.add_expr(IrExpr::GetValue(i)))
                        .collect();
                    let call = self.ir.add_expr(IrExpr::Call {
                        callee: Callee::Local(fid),
                        dispatch_receiver: None,
                        args: argvals,
                    });
                    let unit = self.ir.add_expr(IrExpr::UnitInstance);
                    let ret_e = self.ir.add_expr(IrExpr::Return(Some(unit)));
                    let block = self.ir.add_expr(IrExpr::Block {
                        stmts: vec![call, ret_e],
                        value: None,
                    });
                    let impl_name = format!("{}$funref${}", self.cur_fn_name, self.lambda_seq);
                    self.lambda_seq += 1;
                    let wfid = self.ir.add_fun(IrFunction {
                        name: impl_name,
                        params,
                        ret: ty_to_ir(Ty::obj("kotlin/Unit")),
                        body: Some(block),
                        is_static: true,
                        dispatch_receiver: None,
                        param_checks: Vec::new(),
                    });
                    return Some(self.ir.add_expr(IrExpr::Lambda {
                        impl_fn: wfid,
                        arity: arity as u8,
                        captures: vec![],
                        sam: None,
                        inline_body: None,
                    }));
                }
                return Some(self.ir.add_expr(IrExpr::Lambda {
                    impl_fn: fid,
                    arity: arity as u8,
                    captures: vec![],
                    sam: None,
                    inline_body: None,
                }));
            }
            Expr::Name(n) => {
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
                } else if let Some(&(idx, _)) = self.statics.get(&n) {
                    self.ir.add_expr(IrExpr::GetStatic(idx))
                } else if let Some((facade, ty, _)) = self.syms.prop_facades.get(&n).cloned() {
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
                } else {
                    // Unqualified member of the enclosing class: a backing field (`this.<field>`), or a
                    // computed property (`this.getX()`).
                    let (this_v, this_ty) = self.lookup("this")?;
                    let recv = self.ir.add_expr(IrExpr::GetValue(this_v));
                    let read = if let Some(cur) = self.cur_class.clone() {
                        let field = self.classes.get(&cur).and_then(|ci| {
                            ci.fields
                                .iter()
                                .position(|(fn_, _)| *fn_ == n)
                                .map(|i| (ci.id, i as u32))
                        });
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
                                Some((n0, IrType::Class { fq_name, .. })) if n0 == "this$0" => {
                                    fq_name.clone()
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
                        // An extension-function receiver: `fun A.f() = n` reads `this.n` from OUTSIDE
                        // class A, so go through the property getter (the backing field is private) for a
                        // user class, the field directly when one resolves, else a classpath accessor.
                        let internal = this_ty.obj_internal()?.to_string();
                        if let Some((class, index, _, _)) =
                            self.resolve_method(&internal, &getter_name(&n))
                        {
                            self.ir.add_expr(IrExpr::MethodCall {
                                class,
                                index,
                                receiver: recv,
                                args: vec![],
                            })
                        } else if let Some((fclass, idx, _)) = self.resolve_field(&internal, &n) {
                            self.ir.add_expr(IrExpr::GetField {
                                receiver: recv,
                                class: fclass,
                                index: idx,
                            })
                        } else {
                            let is_iface = self
                                .syms
                                .libraries
                                .resolve_type(&internal)
                                .map_or(false, |t| t.is_interface);
                            let mapped = crate::resolve::collection_mapped_accessor(&n)
                                .map(|s| s.to_string());
                            let m = [Some(n.clone()), Some(getter_name(&n)), mapped]
                                .into_iter()
                                .flatten()
                                .find_map(|c| {
                                    crate::libraries::resolve_instance(
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
                                dispatch_receiver: Some(recv),
                                args: vec![],
                            })
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
                // `coll[i]` on a library type (`List`, `Map`) → its `get(index)` operator member.
                if let Ty::Obj(internal, _) = at {
                    if at.array_elem().is_none() {
                        let it = self.info.ty(index);
                        if let Some(m) = crate::libraries::resolve_instance(
                            &*self.syms.libraries,
                            internal,
                            "get",
                            &[it],
                        ) {
                            let is_iface = self
                                .syms
                                .libraries
                                .resolve_type(internal)
                                .map_or(false, |t| t.is_interface);
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
                } else if rt == Ty::String && name == "length" {
                    // `s.length` → stdlib intrinsic (0-arg), `Int`. The receiver may be a smart-cast
                    // variable whose slot is wider than `String` (`Any` narrowed by `is`) — checkcast it.
                    let recv = self.expr(receiver)?;
                    let needs_cast = matches!(self.afile.expr(receiver), Expr::Name(n)
                        if self.lookup(n).map_or(false, |(_, t)| t != Ty::String));
                    let recv = if needs_cast {
                        self.ir.add_expr(IrExpr::TypeOp {
                            op: IrTypeOp::Cast,
                            arg: recv,
                            type_operand: ty_to_ir(Ty::String),
                        })
                    } else {
                        recv
                    };
                    self.ir.add_expr(IrExpr::Call {
                        callee: Callee::External("kotlin/String.length".to_string()),
                        dispatch_receiver: Some(recv),
                        args: vec![],
                    })
                } else if let Some((owner, ret_ty, is_iface)) = match &rt {
                    // A property read on a class defined in ANOTHER file → its `getX()` accessor (the
                    // backing field is private). Resolved from the sibling class's `ClassSig`.
                    Ty::Obj(i, _) if self.class_of(rt).is_none() => self
                        .syms
                        .class_by_internal(i)
                        .filter(|cs| cs.value_field.is_none())
                        .and_then(|cs| {
                            cs.props
                                .iter()
                                .find(|(n, _, _)| n == &name)
                                .map(|(_, t, _)| (i.to_string(), *t, cs.is_interface))
                        }),
                    _ => None,
                } {
                    let recv = self.expr(receiver)?;
                    self.ir.add_expr(IrExpr::Call {
                        callee: Callee::CrossFileVirtual {
                            owner,
                            name: getter_name(&name),
                            params: vec![],
                            ret: ty_to_ir(ret_ty),
                            interface: is_iface,
                        },
                        dispatch_receiver: Some(recv),
                        args: vec![],
                    })
                } else if let Some((internal, m, is_iface)) = {
                    // A property read on a library type (`list.size`): a Kotlin property is realized as a
                    // zero-arg accessor on the JVM. Try the property's own name (`size()` — collections
                    // map `size` straight to the JVM method) and the `getX()` accessor form.
                    if let Ty::Obj(i, _) = rt {
                        let mapped = crate::resolve::collection_mapped_accessor(&name)
                            .map(|s| s.to_string());
                        [Some(name.clone()), Some(getter_name(&name)), mapped]
                            .into_iter()
                            .flatten()
                            .find_map(|cand| {
                                crate::libraries::resolve_instance(
                                    &*self.syms.libraries,
                                    i,
                                    &cand,
                                    &[],
                                )
                                .filter(|m| !matches!(m.ret, Ty::Unit | Ty::Error))
                                .map(|m| {
                                    let is_iface = self
                                        .syms
                                        .libraries
                                        .resolve_type(i)
                                        .map_or(false, |t| t.is_interface);
                                    (i.to_string(), m, is_iface)
                                })
                            })
                    } else {
                        None
                    }
                } {
                    let recv = self.expr(receiver)?;
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
                    self.coerce_generic_read(read, e, m.ret)
                } else {
                    return None;
                }
            }
            Expr::Binary { op, lhs, rhs } => {
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
                    let mut acc = self.expr(operands[0])?;
                    for &op_e in &operands[1..] {
                        let r = self.expr(op_e)?;
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
                        let l_wp = lt.obj_internal().and_then(crate::resolve::prim_of_wrapper);
                        let r_wp = rt.obj_internal().and_then(crate::resolve::prim_of_wrapper);
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
                let target = self.ty_ref(&ty)?;
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
            Expr::Call { callee, args } => match self.afile.expr(callee).clone() {
                // Local top-level function, or constructor `C(args)`.
                Expr::Name(fname) => {
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
                    // `repeat(count) { i -> body }` — the stdlib inline `repeat`, body
                    // `for (i in 0 until times) action(i)`. Inline to a counted loop (the lambda's
                    // single parameter is the index) so a mutable capture works.
                    if fname == "repeat"
                        && args.len() == 2
                        && self.lookup(&fname).is_none()
                        && !self.syms.funs.contains_key(&fname)
                    {
                        if let Expr::Lambda {
                            params,
                            body: lbody,
                        } = self.afile.expr(args[1]).clone()
                        {
                            let count = self.lower_arg(args[0], &ty_to_ir(Ty::Int))?;
                            let depth = self.scope.len();
                            let i_v = self.fresh_value();
                            let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                            let var_i = self.ir.add_expr(IrExpr::Variable {
                                index: i_v,
                                ty: ty_to_ir(Ty::Int),
                                init: Some(zero),
                            });
                            let n_v = self.fresh_value();
                            let var_n = self.ir.add_expr(IrExpr::Variable {
                                index: n_v,
                                ty: ty_to_ir(Ty::Int),
                                init: Some(count),
                            });
                            let pname = params.first().cloned().unwrap_or_else(|| "it".to_string());
                            self.scope.push((pname, i_v, Ty::Int)); // the index parameter is the counter
                            let gi = self.ir.add_expr(IrExpr::GetValue(i_v));
                            let gn = self.ir.add_expr(IrExpr::GetValue(n_v));
                            let cond = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                                op: IrBinOp::Lt,
                                lhs: gi,
                                rhs: gn,
                            });
                            let mut out = Vec::new();
                            if self.append_body_stmts(lbody, &mut out).is_none() {
                                self.scope.truncate(depth);
                                return None;
                            }
                            let gi2 = self.ir.add_expr(IrExpr::GetValue(i_v));
                            let one = self.ir.add_expr(IrExpr::Const(IrConst::Int(1)));
                            let inc = self.ir.add_expr(IrExpr::PrimitiveBinOp {
                                op: IrBinOp::Add,
                                lhs: gi2,
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
                                label: None,
                            });
                            self.scope.truncate(depth);
                            return Some(self.ir.add_expr(IrExpr::Block {
                                stmts: vec![var_i, var_n, wh],
                                value: None,
                            }));
                        }
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
                    if self.lookup(&fname).is_none() && !self.syms.funs.contains_key(&fname) {
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
                    // The array creators (`arrayOf`/`intArrayOf`/…/`IntArray(n)`/`Array(n){}`) are
                    // compiler synthetics — the synthetic registry (priority over the classpath)
                    // supplies their IR body directly. Honor user shadowing first: a user-defined `fun
                    // arrayOf` (or a local of that name) wins, exactly as in kotlinc.
                    let array_intrinsic_ok =
                        self.lookup(&fname).is_none() && !self.syms.funs.contains_key(&fname);
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
                    if let Some((sig, fid)) = {
                        // Select the overload matching the argument types, then resolve its method id.
                        // Only a function defined in THIS file (present in `fun_ids`) is handled here; a
                        // cross-file function (in `funs` but not `fun_ids`) falls through to the facade
                        // branch below.
                        let arg_tys: Vec<Ty> = args.iter().map(|&a| self.info.ty(a)).collect();
                        self.syms
                            .funs
                            .get(&fname)
                            .and_then(|v| {
                                crate::resolve::pick_overload(v, &arg_tys).map(|i| v[i].clone())
                            })
                            .and_then(|sig| {
                                let key = crate::resolve::erased_params_key(&sig);
                                self.fun_ids
                                    .get(&(fname.clone(), key))
                                    .copied()
                                    .map(|fid| (sig, fid))
                            })
                    } {
                        // A `vararg` function: pack the trailing arguments into a fresh array for the
                        // last (array) parameter. (Spread `*arr` and a branchy element are unsupported.)
                        if sig.vararg {
                            let params = self.ir.functions[fid as usize].params.clone();
                            let fixed = params.len() - 1;
                            if args.len() < fixed {
                                return None;
                            }
                            let elem_ty = sig.params[fixed].array_elem()?;
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
                        let a = self.lower_args_defaulted(e, &meta, &args, &params)?;
                        self.ir.add_expr(IrExpr::Call {
                            callee: Callee::Local(fid),
                            dispatch_receiver: None,
                            args: a,
                        })
                    } else if let Some(facade) = self.syms.fn_facades.get(&fname).cloned() {
                        // A top-level function defined in ANOTHER file of this multi-file compilation →
                        // a cross-facade `invokestatic`. Only the simple exact-arity case (no vararg /
                        // omitted defaults) is modeled here; anything else bails (skips the file).
                        let arg_tys: Vec<Ty> = args.iter().map(|&a| self.info.ty(a)).collect();
                        let sig = self.syms.funs.get(&fname).and_then(|v| {
                            crate::resolve::pick_overload(v, &arg_tys).map(|i| v[i].clone())
                        })?;
                        if sig.vararg
                            || sig.required != sig.params.len()
                            || args.len() != sig.params.len()
                        {
                            return None;
                        }
                        let mut a = Vec::new();
                        for (arg, pt) in args.iter().zip(&sig.params) {
                            a.push(self.lower_arg(*arg, &ty_to_ir(*pt))?);
                        }
                        self.ir.add_expr(IrExpr::Call {
                            callee: Callee::CrossFile {
                                facade,
                                name: fname.clone(),
                                params: sig.params.iter().map(|t| ty_to_ir(*t)).collect(),
                                ret: ty_to_ir(sig.ret),
                            },
                            dispatch_receiver: None,
                            args: a,
                        })
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
                        self.ir.add_expr(IrExpr::Call {
                            callee: Callee::Static {
                                owner: c.owner,
                                name: c.name,
                                descriptor: c.descriptor,
                                inline: c.is_inline,
                                must_inline: c.must_inline,
                            },
                            dispatch_receiver: None,
                            args: a,
                        })
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
                    } else if let Some((class, index, mfid, _)) = {
                        // A bare instance-method call on an external `this` receiver (an inlined
                        // `apply`/`run`, where `cur_class` is cleared and `this` is the receiver slot,
                        // not value 0): `m(args)` → `this.m(args)`.
                        let internal = self
                            .lookup("this")
                            .map(|(_, t)| t)
                            .and_then(|t| t.obj_internal().map(|s| s.to_string()));
                        internal.and_then(|i| self.resolve_method(&i, &fname))
                    } {
                        let (this_v, _) = self.lookup("this")?;
                        let params = self.ir.functions[mfid as usize].params.clone();
                        if args.len() != params.len() {
                            return None;
                        }
                        let recv = self.ir.add_expr(IrExpr::GetValue(this_v));
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
                        let field_tys: Vec<IrType> = if ctor_args.is_empty() {
                            self.ir.classes[class as usize].fields[..ctor_count]
                                .iter()
                                .map(|(_, t)| t.clone())
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
                        let arg_irs: Vec<IrType> =
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
                        let typed_secondary = secs
                            .iter()
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
                            .cloned();
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
                        } else if let Some(sc) = self.ir.classes[class as usize]
                            .secondary_ctors
                            .clone()
                            .into_iter()
                            .find(|sc| sc.params.len() == args.len())
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
                            } else if let Some(m) = crate::libraries::resolve_instance(
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
                                Some((n0, IrType::Class { fq_name, .. })) if n0 == "this$0" => {
                                    Some(fq_name.as_str())
                                }
                                _ => None,
                            };
                            c.fq_name.ends_with(&format!("${name}"))
                                && this0_outer == self.info.ty(receiver).obj_internal()
                        })
                    {
                        let field_tys: Vec<IrType> = self.ir.classes[class_id as usize]
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
                                        || crate::libraries::resolve_instance(
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
                                crate::libraries::resolve_instance(
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
                            // A receiver lambda needs the receiver's user class for `this`-member access.
                            let recv_class = if is_recv_lambda {
                                match rty.obj_internal().filter(|i| self.classes.contains_key(*i)) {
                                    Some(i) => Some(i.to_string()),
                                    None => return None,
                                }
                            } else {
                                None
                            };
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
                            // class's own private fields — clear `cur_class` for the body. (`recv_class`
                            // having resolved confirms the receiver is a reachable user class.)
                            let _ = &recv_class;
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
                                let field_tys: Vec<IrType> = self.ir.classes[class as usize].fields
                                    [..ctor_count]
                                    .iter()
                                    .map(|(_, t)| t.clone())
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
                        if let Some((class, index, fid, _)) = self.resolve_method(&internal, &name)
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
                                crate::libraries::resolve_instance(
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
                                        .map_or(false, |t| t.is_interface);
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
                        self.syms
                            .libraries
                            .resolve_callable(&name, Some(rt), &arg_tys, &[])
                    } {
                        // Coerce the receiver + arguments to the extension's parameter types so a
                        // primitive flowing into a generic `Object` parameter (`fun <T> T.to(…)`) boxes.
                        let recv =
                            self.lower_arg(receiver, &ty_to_ir(*c.params.first().unwrap_or(&rt)))?;
                        let mut a = vec![recv];
                        for (i, &arg) in args.iter().enumerate() {
                            match c.params.get(i + 1) {
                                Some(p) => a.push(self.lower_arg(arg, &ty_to_ir(*p))?),
                                None => a.push(self.expr(arg)?),
                            }
                        }
                        // A `name$default` call appends a placeholder per omitted trailing parameter, an
                        // `int` bit-mask (a bit per omitted parameter), and a `null` marker.
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
                                c.is_inline
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
                        let call = self.ir.add_expr(IrExpr::Call {
                            callee: Callee::Static {
                                owner: c.owner,
                                name: c.name,
                                descriptor: c.descriptor,
                                inline: true,
                                must_inline: false,
                            },
                            dispatch_receiver: None,
                            args: a,
                        });
                        self.coerce_generic_read(call, e, c.physical_ret)
                    } else {
                        return None;
                    }
                }
                _ => return None,
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
fn body_has_return(file: &ast::File, e: AstExprId) -> bool {
    file.any_child_expr(e, &mut |x| body_has_return(file, x), &mut |s| {
        stmt_has_return(file, s)
    })
}

fn stmt_has_return(file: &ast::File, s: ast::StmtId) -> bool {
    matches!(file.stmt(s), Stmt::Return(_))
        || file.any_child_stmt(s, &mut |x| body_has_return(file, x))
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
fn captured_param_ir(name: &str, ty: Ty, boxed: &std::collections::HashSet<String>) -> IrType {
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
fn data_array_param(t: &IrType) -> Option<&'static str> {
    let IrType::Class { fq_name, .. } = t else {
        return None;
    };
    Some(match fq_name.as_str() {
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

fn ty_of(file: &ast::File, r: &ast::TypeRef) -> Ty {
    // Function type `(A, B) -> R` (parsed with `fun_params` non-empty / `name == "<fun>"`).
    if !r.fun_params.is_empty() || r.name == "<fun>" {
        let params: Vec<Ty> = r.fun_params.iter().map(|p| ty_of(file, p)).collect();
        let ret = r.arg.as_ref().map(|a| ty_of(file, a)).unwrap_or(Ty::Unit);
        return Ty::fun(params, ret);
    }
    if let Some(t) = Ty::from_name(&r.name) {
        // A nullable primitive is its boxed wrapper (`Int?` = `java/lang/Integer`), consistent with the
        // checker — otherwise a boxed value would be stored in a primitive field and unboxed wrong.
        if r.nullable && !t.is_reference() {
            if let Some(w) = crate::resolve::nullable_prim_wrapper(t) {
                return Ty::obj(w);
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
fn ir_type_is_reference(t: &IrType) -> bool {
    match t {
        IrType::Class { fq_name, .. } => !matches!(
            fq_name.as_str(),
            "kotlin/Int"
                | "kotlin/Long"
                | "kotlin/Short"
                | "kotlin/Byte"
                | "kotlin/Boolean"
                | "kotlin/Char"
                | "kotlin/Double"
                | "kotlin/Float"
        ),
        IrType::Function { .. } => true,
        _ => false,
    }
}

/// Whether `t` is exactly `java/lang/Object` / `kotlin/Any` (the erased top type — no `checkcast` to it).
fn ir_type_is_object(t: &IrType) -> bool {
    matches!(t, IrType::Class { fq_name, .. } if fq_name == "java/lang/Any" || fq_name == "kotlin/Any" || fq_name == "java/lang/Object")
}

/// Conservative "is `arg` assignable to `param`" for constructor-overload selection: an exact match,
/// or any reference flowing into an erased `Object`/`Any` param (an erased generic type parameter).
/// Deliberately strict otherwise (no class-hierarchy data here) — used only to tell whether the PRIMARY
/// constructor can accept the args before falling back to a secondary (`IC("abc")`: a `String` is NOT
/// assignable to the primary's `List<T>` param, but IS to the secondary's erased `T`).
fn ir_arg_assignable(arg: &IrType, param: &IrType) -> bool {
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
            Stmt::Return(_) => wr,
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

/// Force an `IrType::Class` to be nullable — carry a declared `?` into the IrType (`Ty` drops it). The
/// JVM value-class pass keys unboxed-vs-boxed representation on a value class's underlying nullability.
fn mark_nullable(t: IrType) -> IrType {
    if let IrType::Class {
        fq_name, type_args, ..
    } = t
    {
        IrType::Class {
            fq_name,
            type_args,
            nullable: true,
        }
    } else {
        t
    }
}

pub(crate) fn ty_to_ir(t: Ty) -> IrType {
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
        Ty::Unit => return IrType::Unit,
        Ty::Nothing => return IrType::Nothing,
        // (see `ir_array_element` below for the inverse — extracting an array IrType's element.)
        // A reference `Array<T>` keeps its element as a type argument (the JVM backend boxes a
        // primitive `T` when it lays out the array; the front end keeps the logical element).
        Ty::Obj("kotlin/Array", args) => {
            return IrType::Class {
                fq_name: "kotlin/Array".to_string(),
                type_args: args.iter().map(|t| ty_to_ir(*t)).collect(),
                nullable: false,
            }
        }
        Ty::Obj(n, _) => {
            return IrType::Class {
                fq_name: n.to_string(),
                type_args: vec![],
                nullable: false,
            }
        }
        // A Kotlin function type `(A,…) -> R` is kept structural so each backend picks its own
        // representation (the JVM maps it to `kotlin/jvm/functions/FunctionN`, JS to a closure, …).
        Ty::Fun(s) => {
            return IrType::Function {
                params: s.params.iter().map(|t| ty_to_ir(*t)).collect(),
                ret: Box::new(ty_to_ir(s.ret)),
            }
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
                _ => {
                    return IrType::Class {
                        fq_name: "kotlin/Array".to_string(),
                        type_args: vec![ty_to_ir(*e)],
                        nullable: false,
                    }
                }
            };
            return IrType::Class {
                fq_name: fq.to_string(),
                type_args: vec![],
                nullable: false,
            };
        }
        _ => return IrType::Error,
    };
    IrType::Class {
        fq_name: fq.to_string(),
        type_args: vec![],
        nullable: false,
    }
}

/// The element `IrType` of an array `IrType` target — a reference `Array<E>` (its type argument) or a
/// primitive specialized array (`kotlin/IntArray` → `kotlin/Int`). `None` for a non-array type. Used
/// to materialize an empty array (`emptyArray<T>()`) of the target's element type.
fn ir_array_element(t: &IrType) -> Option<IrType> {
    let IrType::Class {
        fq_name, type_args, ..
    } = t
    else {
        return None;
    };
    if fq_name == "kotlin/Array" {
        return type_args.first().cloned();
    }
    let prim = match fq_name.as_str() {
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
    Some(IrType::Class {
        fq_name: prim.to_string(),
        type_args: vec![],
        nullable: false,
    })
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
