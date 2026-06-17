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
use crate::ir::{Callee, ClassId, IrBinOp, IrClass, IrConst, IrExpr, IrFile, IrFunction, IrType, IrTypeOp};
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
        ir: IrFile { package: file.package.clone(), ..Default::default() },
        fun_ids: HashMap::new(),
        ext_fun_ids: HashMap::new(),
        classes: HashMap::new(),
        statics: HashMap::new(),
        scope: Vec::new(),
        next_value: 0,
        cur_class: None,
        cur_fn_name: String::new(),
        lambda_seq: 0,
        cur_ret_ty: IrType::Unit,
        companions: HashMap::new(),
        computed_props: HashMap::new(),
    };

    // Only files of top-level functions + *simple* classes take the IR path.
    for &d in &file.decls {
        match file.decl(d) {
            Decl::Fun(f) if !f.is_inline => {} // top-level function or extension function
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
            // Constructor-parameter fields, then class-body-property fields (initialized in `init_body`).
            let ctor_fields: Vec<(String, Ty)> = c.props.iter().filter(|p| p.is_property)
                .map(|p| (p.name.clone(), ty_of(file, &p.ty))).collect();
            let ctor_param_count = ctor_fields.len() as u32;
            // Non-null reference constructor parameters get an `Intrinsics.checkNotNullParameter` guard
            // (kotlinc does); primitives, nullable params, and class type-parameters are skipped.
            let ctor_param_checks: Vec<Option<String>> = c.props.iter().filter(|p| p.is_property).map(|p| {
                let ty = ty_of(file, &p.ty);
                let is_type_param = c.type_params.contains(&p.ty.name);
                if !p.ty.nullable && !is_type_param && ty.is_reference() { Some(p.name.clone()) } else { None }
            }).collect();
            // Computed body properties (custom getter, no backing field) become `getX()` methods, not
            // fields — exclude them here.
            let body_fields: Vec<(String, Ty)> = c.body_props.iter().filter(|p| !is_computed_prop(p))
                .map(|p| {
                    let ty = p.ty.as_ref().map(|r| ty_of(file, r)).unwrap_or_else(|| info.ty(p.init.unwrap()));
                    (p.name.clone(), ty)
                })
                .collect();
            let fields: Vec<(String, Ty)> = ctor_fields.into_iter().chain(body_fields).collect();
            let class_ty = IrType::Class { fq_name: internal.clone(), type_args: vec![], nullable: false };
            // Resolve a base class (`: A(args)`): only a non-interface class declared in this file is
            // supported; extending a classpath/Java type isn't modeled yet → bail.
            let super_internal: Option<String> = match &c.base_class {
                Some(base) => {
                    let is_file_class = file.decls.iter().any(|&d| matches!(file.decl(d), Decl::Class(bc) if bc.name == *base && !bc.is_interface));
                    if !is_file_class { return None; }
                    Some(class_internal(file, base))
                }
                None => None,
            };
            let superclass = if c.is_enum {
                "java/lang/Enum".to_string()
            } else {
                super_internal.clone().unwrap_or_else(|| "java/lang/Object".to_string())
            };
            // Implemented interfaces (`: I, J`): each must be a file interface, else bail.
            let mut iface_internals = Vec::new();
            for st in &c.supertypes {
                let is_file_iface = file.decls.iter().any(|&d| matches!(file.decl(d), Decl::Class(ic) if ic.name == *st && ic.is_interface));
                if !is_file_iface { return None; }
                iface_internals.push(class_internal(file, st));
            }
            let id = lo.ir.add_class(IrClass {
                fq_name: internal.clone(),
                supertypes: vec![],
                fields: fields.iter().map(|(n, t)| (n.clone(), ty_to_ir(*t))).collect(),
                ctor_param_count,
                init_body: None,
                methods: vec![],
                is_interface: c.is_interface,
                superclass,
                super_args: Vec::new(),
                // Entry names now; constructor-arg value-ids are lowered in pass 2.
                enum_entries: c.enum_entries.iter().map(|n| (n.clone(), Vec::new())).collect(),
                bridges: Vec::new(),
                interfaces: iface_internals,
                is_object: c.is_object,
                ctor_param_checks,
                is_companion: false,
                companion_class: None,
                field_final: c.props.iter().filter(|p| p.is_property).map(|p| !p.is_var)
                    .chain(c.body_props.iter().filter(|p| !is_computed_prop(p)).map(|p| !p.is_var))
                    .collect(),
            });
            let mut methods = HashMap::new();
            let mut method_fids = Vec::new();
            for (mi, m) in c.methods.iter().enumerate() {
                let sig = syms.classes.get(&c.name)?.methods.get(&m.name)?;
                let ret = sig.ret;
                let params: Vec<IrType> = sig.params.iter().map(|t| ty_to_ir(*t)).collect();
                let param_checks = param_checks_for(m, &sig.params);
                let fid = lo.ir.add_fun(IrFunction {
                    name: m.name.clone(),
                    params,
                    ret: ty_to_ir(ret),
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
                if m.params.iter().any(|p| p.default.is_some()) && !c.is_interface && m.params.len() <= 31 {
                    lo.ir.fn_param_defaults.insert(fid, Vec::new());
                    lo.ir.fn_param_names.insert(fid, m.params.iter().map(|p| p.name.clone()).collect());
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
                    name: gname.clone(), params: vec![], ret: ty_to_ir(ty),
                    body: None, is_static: false, dispatch_receiver: Some(internal.clone()), param_checks: vec![],
                });
                methods.insert(gname, (mi, fid, ty));
                method_fids.push(fid);
            }
            // Synthesize `getX()`/`setX()` accessors for each backing-field property (kotlinc emits
            // them; the fields are private). Getter returns the field; setter (var only) writes it.
            // Enums keep their existing shape (separate emit path); interfaces have no backing fields.
            if !c.is_interface && !c.is_enum {
                let field_props: Vec<(String, bool)> = c.props.iter().filter(|p| p.is_property).map(|p| (p.name.clone(), p.is_var))
                    .chain(c.body_props.iter().filter(|p| !is_computed_prop(p)).map(|p| (p.name.clone(), p.is_var)))
                    .collect();
                for (fidx, (pname, is_var)) in field_props.iter().enumerate() {
                    let fty = fields[fidx].1;
                    let fty_ir = ty_to_ir(fty);
                    let gname = getter_name(pname);
                    if !methods.contains_key(&gname) {
                        let this_e = lo.ir.add_expr(IrExpr::GetValue(0));
                        let gf = lo.ir.add_expr(IrExpr::GetField { receiver: this_e, class: id, index: fidx as u32 });
                        let ret = lo.ir.add_expr(IrExpr::Return(Some(gf)));
                        let body = lo.ir.add_expr(IrExpr::Block { stmts: vec![ret], value: None });
                        let mi = method_fids.len() as u32;
                        let fid = lo.ir.add_fun(IrFunction { name: gname.clone(), params: vec![], ret: fty_ir.clone(), body: Some(body), is_static: false, dispatch_receiver: Some(internal.clone()), param_checks: vec![] });
                        methods.insert(gname, (mi, fid, fty));
                        method_fids.push(fid);
                    }
                    if *is_var {
                        let sname = setter_name(pname);
                        if !methods.contains_key(&sname) {
                            let this_e = lo.ir.add_expr(IrExpr::GetValue(0));
                            let v = lo.ir.add_expr(IrExpr::GetValue(1));
                            let sf = lo.ir.add_expr(IrExpr::SetField { receiver: this_e, class: id, index: fidx as u32, value: v });
                            let body = lo.ir.add_expr(IrExpr::Block { stmts: vec![sf], value: None });
                            let mi = method_fids.len() as u32;
                            let fid = lo.ir.add_fun(IrFunction { name: sname.clone(), params: vec![fty_ir.clone()], ret: IrType::Unit, body: Some(body), is_static: false, dispatch_receiver: Some(internal.clone()), param_checks: vec![] });
                            methods.insert(sname, (mi, fid, Ty::Unit));
                            method_fids.push(fid);
                        }
                    }
                }
            }
            lo.ir.classes[id as usize].methods = method_fids;
            let _ = class_ty;
            lo.classes.insert(internal.clone(), ClassInfo { id, internal: internal.clone(), fields, methods, super_internal });
            // `companion object` with methods → a synthesized `C$Companion` class (private ctor, the
            // companion methods as instance methods) + a `Companion` field on the outer class.
            // Companion properties aren't modeled yet (their backing fields live on the outer class).
            if !c.companion_methods.is_empty() && c.companion_props.is_empty() {
                let comp_fq = format!("{internal}$Companion");
                let comp_id = lo.ir.add_class(IrClass {
                    fq_name: comp_fq.clone(), supertypes: vec![], fields: vec![], ctor_param_count: 0,
                    init_body: None, methods: vec![], is_interface: false,
                    superclass: "java/lang/Object".to_string(), super_args: vec![],
                    enum_entries: vec![], bridges: vec![], interfaces: vec![],
                    is_object: false, ctor_param_checks: vec![], is_companion: true, companion_class: None,
                    field_final: vec![],
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
                        name: m.name.clone(), params, ret: ty_to_ir(ret), body: None,
                        is_static: false, dispatch_receiver: Some(comp_fq.clone()), param_checks,
                    });
                    cmethods.insert(m.name.clone(), (mi as u32, fid, ret));
                    cmethod_fids.push(fid);
                }
                lo.ir.classes[comp_id as usize].methods = cmethod_fids;
                lo.ir.classes[id as usize].companion_class = Some(comp_fq.clone());
                lo.classes.insert(comp_fq.clone(), ClassInfo { id: comp_id, internal: comp_fq.clone(), fields: vec![], methods: cmethods, super_internal: None });
                lo.companions.insert(internal.clone(), comp_fq);
            }
            // A `data class`'s equals/hashCode/toString/componentN are Kotlin language semantics —
            // synthesize them here as ordinary IR methods (backend-agnostic), registered so calls
            // resolve and the generic method emitter handles them.
            if c.is_data {
                lo.synth_data_members(&internal, id, ctor_param_count as usize);
            }
            // Interface delegation `: I by d` is sugar — synthesize a forwarder for each of `I`'s
            // methods that calls `this.d.method(args)`. Bails the file if a delegate can't be modeled.
            if !c.delegations.is_empty() {
                lo.synth_delegation_forwarders(file, c, &internal, id)?;
            }
        }
    }
    // Pass 1b: register top-level functions and extension functions.
    for &d in &file.decls {
        if let Decl::Fun(f) = file.decl(d) {
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
                let id = lo.ir.add_fun(IrFunction { name: f.name.clone(), params, ret, body: None, is_static: true, dispatch_receiver: None, param_checks: vec![] });
                lo.ext_fun_ids.insert((recv_desc, f.name.clone()), id);
            } else {
                let sig = syms.funs.get(&f.name)?;
                let params: Vec<IrType> = sig.params.iter().map(|t| ty_to_ir(*t)).collect();
                let ret = ty_to_ir(info.fun_ret_overrides.get(&f.name).copied().unwrap_or(sig.ret));
                let param_checks = param_checks_for(f, &sig.params);
                let id = lo.ir.add_fun(IrFunction { name: f.name.clone(), params, ret, body: None, is_static: true, dispatch_receiver: None, param_checks });
                lo.fun_ids.insert(f.name.clone(), id);
            }
        }
    }
    // Pass 1c: assign top-level-property indices (initializers lowered in pass 2). Registered before
    // any body so a function may read a top-level property as `GetStatic`.
    for &d in &file.decls {
        if let Decl::Property(p) = file.decl(d) {
            let ty = p.ty.as_ref().map(|r| ty_of(file, r)).unwrap_or_else(|| info.ty(p.init.unwrap()));
            if is_computed_prop(p) {
                // A computed property: a `getX()` accessor (static on the facade), no backing field.
                let fid = lo.ir.add_fun(IrFunction {
                    name: getter_name(&p.name), params: vec![], ret: ty_to_ir(ty),
                    body: None, is_static: true, dispatch_receiver: None, param_checks: vec![],
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
                    (lo.fun_ids[&f.name], syms.funs.get(&f.name)?)
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
                                    erased_params: bp, erased_ret: br,
                                    concrete_params: op, concrete_ret: or,
                                });
                            }
                        }
                    }
                    // A property redeclared in the subclass (`override val field`) needs getter/setter
                    // virtual dispatch — krusty reads the field directly, which would bypass the
                    // override. Bail when a subclass field name shadows a base field.
                    let own_fields = c.props.iter().filter(|p| p.is_property).map(|p| &p.name)
                        .chain(c.body_props.iter().map(|p| &p.name));
                    for fname in own_fields {
                        if lo.resolve_field(&super_int, fname).is_some() {
                            return None;
                        }
                    }
                }
                // Interface bridges: for each implemented-interface method, if the class's actual
                // implementation (declared or inherited) has a different erased signature than the
                // interface's, add a bridge with the interface's descriptor delegating to the impl.
                if !c.is_interface {
                    let cid = lo.classes[&internal].id;
                    let ifaces = lo.ir.classes[cid as usize].interfaces.clone();
                    let mut seen: std::collections::HashSet<String> =
                        lo.ir.classes[cid as usize].bridges.iter()
                            .map(|b| format!("{}{:?}{:?}", b.name, b.erased_params, b.erased_ret)).collect();
                    for itf in &ifaces {
                        for (mname, ifid) in lo.collect_iface_methods(itf) {
                            if let Some((_, _, impl_fid, _)) = lo.resolve_method(&internal, &mname) {
                                let ip = lo.ir.functions[ifid as usize].params.clone();
                                let ir_ = lo.ir.functions[ifid as usize].ret.clone();
                                let cp = lo.ir.functions[impl_fid as usize].params.clone();
                                let cr = lo.ir.functions[impl_fid as usize].ret.clone();
                                if (ip != cp || ir_ != cr) && seen.insert(format!("{}{:?}{:?}", mname, ip, ir_)) {
                                    lo.ir.classes[cid as usize].bridges.push(crate::ir::Bridge {
                                        name: mname, erased_params: ip, erased_ret: ir_,
                                        concrete_params: cp, concrete_ret: cr,
                                    });
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
                    lo.scope.push(("this".to_string(), this_v, Ty::obj(&internal)));
                    let sig = syms.classes.get(&c.name)?.methods.get(&m.name)?.clone();
                    for (p, t) in m.params.iter().zip(&sig.params) {
                        let v = lo.fresh_value();
                        lo.scope.push((p.name.clone(), v, *t));
                    }
                    // Register parameter defaults (the JVM backend realizes them via the `$default`
                    // stub). Lowered with `this` = value 0 and the params = values 1..=n — the stub's
                    // value layout. `None` for a required parameter. Gated identically to the pass-1
                    // marker (no interface defaults, ≤31 parameters).
                    if m.params.iter().any(|p| p.default.is_some()) && !c.is_interface && m.params.len() <= 31 {
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
                    lo.scope.push(("this".to_string(), this_v, Ty::obj(&internal)));
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
                        lo.scope.push(("this".to_string(), this_v, Ty::obj(&comp_fq)));
                        let sig = syms.classes.get(&c.name)?.static_methods.get(&m.name)?;
                        for (p, t) in m.params.iter().zip(&sig.params) {
                            let v = lo.fresh_value();
                            lo.scope.push((p.name.clone(), v, *t));
                        }
                        let ret_ty = lo.ir.functions[fid as usize].ret.clone();
                        lo.lower_body(&m.body, &ret_ty, fid)?;
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
                    lo.scope.push(("this".to_string(), this_v, Ty::obj(&internal)));
                    for p in c.props.iter().filter(|p| p.is_property) {
                        let v = lo.fresh_value();
                        lo.scope.push((p.name.clone(), v, ty_of(file, &p.ty)));
                    }
                    let super_field_tys: Vec<IrType> = lo.classes[&internal].super_internal.clone()
                        .and_then(|s| lo.classes.get(&s).map(|sup| sup.id))
                        .map(|sid| {
                            let n = lo.ir.classes[sid as usize].ctor_param_count as usize;
                            lo.ir.classes[sid as usize].fields[..n].iter().map(|(_, t)| t.clone()).collect()
                        })
                        .unwrap_or_default();
                    let mut sargs = Vec::new();
                    for (a, ft) in c.base_args.iter().zip(&super_field_tys) {
                        sargs.push(lo.lower_arg(*a, ft)?);
                    }
                    lo.ir.classes[class_id as usize].super_args = sargs;
                }
                // Constructor body: run body-property initializers and `init { … }` blocks in source
                // order, with `this` = value 0 and the constructor params as values 1..=N.
                if !c.init_order.is_empty() {
                    let class_id = lo.classes[&internal].id;
                    let ctor_count = lo.ir.classes[class_id as usize].ctor_param_count;
                    lo.scope.clear();
                    lo.next_value = 0;
                    lo.cur_class = Some(internal.clone());
                    let this_v = lo.fresh_value();
                    lo.scope.push(("this".to_string(), this_v, Ty::obj(&internal)));
                    for p in c.props.iter().filter(|p| p.is_property) {
                        let v = lo.fresh_value();
                        lo.scope.push((p.name.clone(), v, ty_of(file, &p.ty)));
                    }
                    let mut stmts = Vec::new();
                    for step in &c.init_order {
                        match step {
                            ast::ClassInit::PropInit(i) => {
                                // A computed body property has no backing field — nothing to initialize.
                                if is_computed_prop(&c.body_props[*i as usize]) {
                                    continue;
                                }
                                // Computed body properties are not fields, so the field index counts
                                // only the non-computed body properties before this one.
                                let body_offset = c.body_props[..*i as usize].iter().filter(|p| !is_computed_prop(p)).count();
                                let field_idx = ctor_count + body_offset as u32;
                                let field_ty = lo.ir.classes[class_id as usize].fields[field_idx as usize].1.clone();
                                let init_e = c.body_props[*i].init.unwrap();
                                // A branchy body-property initializer (`val k = when { … }`) emits
                                // merge-point frames in the constructor's init context that the flat
                                // emitter doesn't reconcile yet — bail rather than miscompile.
                                if matches!(lo.afile.expr(init_e), Expr::When { .. } | Expr::If { .. } | Expr::Elvis { .. } | Expr::Block { .. } | Expr::Try { .. }) {
                                    return None;
                                }
                                let val = lo.lower_arg(init_e, &field_ty)?;
                                let recv = lo.ir.add_expr(IrExpr::GetValue(this_v));
                                stmts.push(lo.ir.add_expr(IrExpr::SetField { receiver: recv, class: class_id, index: field_idx, value: val }));
                            }
                            ast::ClassInit::Block(e) => {
                                // An `init { … }` block: lower its statements for effect.
                                let Expr::Block { stmts: bs, trailing } = lo.afile.expr(*e).clone() else { return None };
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
                // Enum entries: lower each entry's constructor arguments (constant expressions
                // evaluated in `<clinit>`), coerced to the matching ctor-parameter field type.
                if c.is_enum {
                    let class_id = lo.classes[&internal].id;
                    let ctor_count = lo.ir.classes[class_id as usize].ctor_param_count as usize;
                    let field_tys: Vec<IrType> = lo.ir.classes[class_id as usize].fields[..ctor_count]
                        .iter().map(|(_, t)| t.clone()).collect();
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
                    lo.ir.statics.push(crate::ir::IrStatic { name: p.name.clone(), ty: ir_ty, init });
                }
            }
            _ => {}
        }
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
    // concrete methods normally. `value`/inline classes need unboxing and are excluded.
    // A `companion object` with only methods is supported (synthesized `C$Companion` class); a
    // companion with properties (`val`/`const val`) is not yet.
    !c.is_value && !c.is_object && !c.is_enum && !c.is_interface
        && c.companion_props.is_empty() && c.secondary_ctors.is_empty()
        && c.props.iter().all(|p| p.is_property)
        // Body properties (`class C { val x = … }`) are allowed when they're plain backing fields
        // initialized in the constructor; `init { … }` blocks run there too (see `init_order`).
        && c.body_props.iter().all(|p| is_plain_body_prop(p) || is_computed_prop(p))
        // Methods are non-extension; an abstract method (no body) is allowed on an abstract class
        // (the checker only permits that), and emitted as an `ACC_ABSTRACT` declaration.
        && c.methods.iter().all(|m| m.receiver.is_none())
}

/// An `enum class` the IR can emit: a primary constructor of `val`/`var` props, concrete (non-extension,
/// bodied) methods, plain body-props, and no companion / secondary ctors / supertypes / per-entry
/// bodies (entry bodies would need an anonymous subclass per entry, which the flat AST doesn't carry).
fn is_simple_enum(c: &ast::ClassDecl) -> bool {
    c.is_enum
        && c.companion_methods.is_empty() && c.companion_props.is_empty()
        && c.secondary_ctors.is_empty() && c.supertypes.is_empty()
        && c.props.iter().all(|p| p.is_property)
        && c.body_props.iter().all(is_plain_body_prop)
        && c.methods.iter().all(|m| m.receiver.is_none() && !matches!(m.body, FunBody::None))
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
}

/// An `interface` the IR can emit: only abstract methods (no default/bodied methods, which need a
/// `DefaultImpls` class), no properties (abstract property getters not modeled), no companion.
fn is_simple_interface(c: &ast::ClassDecl) -> bool {
    c.is_interface
        && c.companion_methods.is_empty() && c.companion_props.is_empty()
        && c.props.is_empty() && c.body_props.is_empty()
        && c.methods.iter().all(|m| m.receiver.is_none() && matches!(m.body, FunBody::None))
}

/// A class-body property that is a plain backing field: a normal (non-extension) `val`/`var` with an
/// initializer and no custom getter/setter and not `lateinit`.
fn is_plain_body_prop(p: &ast::PropDecl) -> bool {
    p.receiver.is_none() && !p.is_lateinit && p.getter.is_none() && p.setter.is_none() && p.init.is_some()
}

/// A computed property `val x: T get() = expr` — a custom getter, no backing field (no initializer),
/// immutable (no setter). Compiled to a `getX()` accessor; reads call it.
fn is_computed_prop(p: &ast::PropDecl) -> bool {
    p.receiver.is_none() && !p.is_lateinit && !p.is_var
        && p.init.is_none() && p.getter.is_some() && p.setter.is_none() && p.ty.is_some()
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
    let base = if prop.starts_with("is") && b.len() > 2 && b[2].is_ascii_uppercase() { &prop[2..] } else { prop };
    let mut c = base.chars();
    format!("set{}{}", c.next().unwrap().to_uppercase(), c.as_str())
}

struct Lower<'a> {
    afile: &'a ast::File,
    info: &'a TypeInfo,
    syms: &'a SymbolTable,
    ir: IrFile,
    fun_ids: HashMap<String, u32>,
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
    /// Return type of the function currently being lowered — used to coerce `return` values (e.g. a
    /// generic-erased `Object` return gets the `checkcast` kotlinc inserts).
    cur_ret_ty: IrType,
    /// Outer-class internal name → its `C$Companion` internal name, for routing `C.foo()` calls.
    companions: HashMap<String, String>,
    /// Top-level computed property name → (its synthesized `getX()` `FunId`, property type). A read of
    /// the property compiles to a call to the getter (there is no backing field).
    computed_props: HashMap<String, (u32, Ty)>,
}

impl<'a> Lower<'a> {
    fn fresh_value(&mut self) -> u32 {
        let v = self.next_value;
        self.next_value += 1;
        v
    }

    /// Resolve a `catch` exception type name to its JVM internal name (mirrors the checker): a file
    /// class, a known class-name, or a classpath/stdlib throwable alias.
    fn catch_internal(&self, name: &str) -> Option<String> {
        self.syms.class_names.get(name).cloned()
            .or_else(|| self.syms.classes.get(name).map(|c| c.internal.clone()))
    }

    /// Lower construction of a classpath (non-IR) class — `RuntimeException("x")`, `StringBuilder()`.
    /// The constructor descriptor is resolved from the classpath; arguments are coerced to its
    /// parameter types. Bails when the constructor can't be resolved or arity mismatches.
    fn lower_external_new(&mut self, internal: &str, args: &[AstExprId]) -> Option<u32> {
        let arg_tys: Vec<Ty> = args.iter().map(|a| self.info.ty(*a)).collect();
        let desc = crate::resolve::resolve_java_ctor(&self.syms.classpath, internal, &arg_tys).or_else(|| {
            // Every JDK `Throwable` has a no-arg and a single-`String` constructor; accept those two
            // shapes even when the classpath reader can't see the jimage constructor descriptors.
            if crate::jvm::jvm_class_map::is_throwable_internal(internal) {
                match arg_tys.as_slice() {
                    [] => Some("()V".to_string()),
                    [Ty::String] => Some("(Ljava/lang/String;)V".to_string()),
                    _ => None,
                }
            } else {
                None
            }
        })?;
        let param_descs = split_param_descriptors(&desc)?;
        if param_descs.len() != args.len() {
            return None;
        }
        let mut a = Vec::new();
        for (arg, pd) in args.iter().zip(&param_descs) {
            let pty = ty_to_ir(crate::resolve::desc_to_ty(pd));
            a.push(self.lower_arg(*arg, &pty)?);
        }
        Some(self.ir.add_expr(IrExpr::NewExternal { internal: internal.to_string(), ctor_desc: desc, args: a }))
    }

    /// Lower a lambda literal `{ a, b -> body }` to an `IrExpr::Lambda` (emitted as `invokedynamic` +
    /// `LambdaMetafactory`). The body becomes a synthesized static method `<enclosing>$lambda$<n>`
    /// with the lambda's (real, from the checker) parameter types. Non-capturing only: a body that
    /// reads any enclosing local/parameter, or a lambda inside a class method (which could capture
    /// `this`/fields), bails (`None`) rather than miscompile.
    fn lower_lambda(&mut self, e: AstExprId, params: &[String], body: AstExprId) -> Option<u32> {
        let Ty::Fun(sig) = self.info.ty(e) else { return None };
        let arity = sig.params.len();
        // A lambda inside a class method could capture `this`/fields — not modeled yet.
        if self.cur_class.is_some() {
            return None;
        }
        // A `Unit`/`Nothing`-returning lambda must yield the `kotlin/Unit` singleton from its impl
        // method (so `FunctionN.invoke` returns an Object); krusty doesn't emit that yet — bail.
        if sig.ret == Ty::Unit || sig.ret == Ty::Nothing {
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
        // Non-capturing only: bail if the body reads any enclosing local/parameter that the lambda
        // does not itself bind (capturing it would require a closure with captured fields).
        for (name, _, _) in &self.scope {
            if !bind_names.contains(name) && crate::resolve::expr_uses_name_pub(self.afile, body, name) {
                return None;
            }
        }
        // Lower the body in a fresh value-numbering scope (the impl method's own locals).
        let saved_scope = std::mem::take(&mut self.scope);
        let saved_next = self.next_value;
        self.next_value = 0;
        for (name, pty) in bind_names.iter().zip(sig.params.iter()) {
            let v = self.fresh_value();
            self.scope.push((name.clone(), v, *pty));
        }
        let ve = self.expr(body);
        self.scope = saved_scope;
        self.next_value = saved_next;
        let ve = ve?;
        let ret_ty = ty_to_ir(sig.ret);
        // Wrap the body value in a `Return` unless the lambda is `Unit`-valued or diverges.
        let diverges = self.info.ty(body) == Ty::Nothing;
        let body_expr = if ret_ty == IrType::Unit || diverges {
            ve
        } else {
            self.ir.add_expr(IrExpr::Return(Some(ve)))
        };
        let block = self.ir.add_expr(IrExpr::Block { stmts: vec![body_expr], value: None });
        let impl_name = format!("{}$lambda${}", self.cur_fn_name, self.lambda_seq);
        self.lambda_seq += 1;
        let params_ir: Vec<IrType> = sig.params.iter().map(|t| ty_to_ir(*t)).collect();
        let fid = self.ir.add_fun(IrFunction {
            name: impl_name,
            params: params_ir,
            ret: ret_ty,
            body: Some(block),
            is_static: true,
            dispatch_receiver: None,
            param_checks: Vec::new(),
        });
        Some(self.ir.add_expr(IrExpr::Lambda { impl_fn: fid, arity: arity as u8, captures: vec![] }))
    }

    /// Register a synthesized instance method (a real `IrFunction` with an IR body) on a class, so
    /// it resolves like any other method and the generic emitter handles it — no backend special-case.
    fn add_synth_method(&mut self, internal: &str, class_id: ClassId, name: &str, params: Vec<IrType>, ret: Ty, body: u32) -> Option<u32> {
        if self.classes.get(internal).map_or(false, |ci| ci.methods.contains_key(name)) {
            return None; // a user-defined override exists — don't synthesize over it
        }
        // Don't synthesize a member a superclass already provides (e.g. a `final override fun
        // toString()` on a base class — a data class inherits it instead of regenerating).
        if let Some(s) = self.classes.get(internal).and_then(|ci| ci.super_internal.clone()) {
            if self.resolve_method(&s, name).is_some() {
                return None;
            }
        }
        let fid = self.ir.add_fun(IrFunction {
            name: name.to_string(), params, ret: ty_to_ir(ret), body: Some(body),
            is_static: false, dispatch_receiver: Some(internal.to_string()),
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
    fn synth_delegation_forwarders(&mut self, file: &ast::File, c: &ast::ClassDecl, internal: &str, class_id: ClassId) -> Option<()> {
        for (iface_name, delegate) in &c.delegations {
            let delegate_idx = self.classes.get(internal)?.fields.iter().position(|(n, _)| n == delegate)? as u32;
            let iface_internal = class_internal(file, iface_name);
            let methods: Vec<(String, Vec<Ty>, Ty)> = self.syms.classes.get(iface_name)?
                .methods.iter().map(|(n, s)| (n.clone(), s.params.clone(), s.ret)).collect();
            for (mname, params, ret) in methods {
                let params_ir: Vec<IrType> = params.iter().map(|t| ty_to_ir(*t)).collect();
                let descriptor = format!("({}){}", params.iter().map(|t| t.descriptor()).collect::<String>(), ret.descriptor());
                let field = self.this_field(class_id, delegate_idx);
                let args: Vec<u32> = (0..params.len()).map(|i| self.ir.add_expr(IrExpr::GetValue(i as u32 + 1))).collect();
                let call = self.ir.add_expr(IrExpr::Call {
                    callee: Callee::Virtual { owner: iface_internal.clone(), name: mname.clone(), descriptor, interface: true },
                    dispatch_receiver: Some(field),
                    args,
                });
                let body = if ret == Ty::Unit {
                    self.ir.add_expr(IrExpr::Block { stmts: vec![call], value: None })
                } else {
                    let ret_stmt = self.ir.add_expr(IrExpr::Return(Some(call)));
                    self.ir.add_expr(IrExpr::Block { stmts: vec![ret_stmt], value: None })
                };
                self.add_synth_method(internal, class_id, &mname, params_ir, ret, body);
            }
        }
        Some(())
    }

    fn ir_const_str(&mut self, s: String) -> u32 { self.ir.add_expr(IrExpr::Const(IrConst::String(s))) }
    fn str_plus(&mut self, acc: u32, arg: u32) -> u32 {
        self.ir.add_expr(IrExpr::Call { callee: Callee::External("kotlin/String.plus".to_string()), dispatch_receiver: Some(acc), args: vec![arg] })
    }
    fn this_field(&mut self, class_id: ClassId, i: u32) -> u32 {
        let this = self.ir.add_expr(IrExpr::GetValue(0));
        self.ir.add_expr(IrExpr::GetField { receiver: this, class: class_id, index: i })
    }
    fn static_call(&mut self, fq: &str, args: Vec<u32>) -> u32 {
        self.ir.add_expr(IrExpr::Call { callee: Callee::External(fq.to_string()), dispatch_receiver: None, args })
    }
    /// The `Int` hash of a field value `v` of type `t` (Kotlin's per-field `.hashCode()`).
    fn field_hash(&mut self, v: u32, t: Ty) -> u32 {
        match t {
            Ty::Int | Ty::Short | Ty::Byte | Ty::Char => v,
            Ty::Boolean => self.static_call("java/lang/Boolean.hashCode", vec![v]),
            Ty::Long => self.static_call("java/lang/Long.hashCode", vec![v]),
            Ty::Double => self.static_call("java/lang/Double.hashCode", vec![v]),
            Ty::Float => self.static_call("java/lang/Float.hashCode", vec![v]),
            _ => self.static_call("java/util/Objects.hashCode", vec![v]),
        }
    }
    /// A `Boolean` IR expr testing field *inequality* (IEEE-aware for float/double, structural for
    /// refs) — used to build `equals` as a chain of `if (a != b) return false` early-outs.
    fn field_ne(&mut self, a: u32, b: u32, t: Ty) -> u32 {
        match t {
            Ty::Double | Ty::Float => {
                let fq = if t == Ty::Double { "java/lang/Double.compare" } else { "java/lang/Float.compare" };
                let cmp = self.static_call(fq, vec![a, b]);
                let z = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::Ne, lhs: cmp, rhs: z })
            }
            // Int/Long/… → native compare; reference → `!Intrinsics.areEqual` via the reference Ne path.
            _ => self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::Ne, lhs: a, rhs: b }),
        }
    }
    /// `if (cond) return false` — a no-`else` statement-`when` whose only branch diverges.
    fn guard_return_false(&mut self, cond: u32) -> u32 {
        let f = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
        let ret = self.ir.add_expr(IrExpr::Return(Some(f)));
        let blk = self.ir.add_expr(IrExpr::Block { stmts: vec![ret], value: None });
        self.ir.add_expr(IrExpr::When { branches: vec![(Some(cond), blk)] })
    }

    /// Synthesize a `data class`'s `componentN`/`toString`/`hashCode`/`equals` as IR methods over the
    /// first `n` (primary-constructor) fields.
    fn synth_data_members(&mut self, internal: &str, class_id: ClassId, n: usize) {
        let fields: Vec<(String, Ty)> = self.classes[internal].fields[..n].to_vec();

        // componentN(): `return this.fieldN`.
        for (i, (_, t)) in fields.iter().enumerate() {
            let get = self.this_field(class_id, i as u32);
            let ret = self.ir.add_expr(IrExpr::Return(Some(get)));
            let body = self.ir.add_expr(IrExpr::Block { stmts: vec![ret], value: None });
            self.add_synth_method(internal, class_id, &format!("component{}", i + 1), vec![], *t, body);
        }

        // toString(): `"Simple(f1=" + f1 + ", f2=" + f2 + ")"`.
        {
            let simple = internal.rsplit('/').next().unwrap_or(internal).replace('$', ".");
            let mut acc = self.ir_const_str(format!("{simple}("));
            for (i, (name, _)) in fields.iter().enumerate() {
                let sep = if i == 0 { format!("{name}=") } else { format!(", {name}=") };
                let s = self.ir_const_str(sep);
                acc = self.str_plus(acc, s);
                let fv = self.this_field(class_id, i as u32);
                acc = self.str_plus(acc, fv);
            }
            let close = self.ir_const_str(")".to_string());
            acc = self.str_plus(acc, close);
            let ret = self.ir.add_expr(IrExpr::Return(Some(acc)));
            let body = self.ir.add_expr(IrExpr::Block { stmts: vec![ret], value: None });
            self.add_synth_method(internal, class_id, "toString", vec![], Ty::String, body);
        }

        // hashCode(): `h(f1)`, then `result*31 + h(fN)` (0 for an empty data class).
        {
            let result = if fields.is_empty() {
                self.ir.add_expr(IrExpr::Const(IrConst::Int(0)))
            } else {
                let mut acc: Option<u32> = None;
                for (i, (_, t)) in fields.iter().enumerate() {
                    let fv = self.this_field(class_id, i as u32);
                    let h = self.field_hash(fv, *t);
                    acc = Some(match acc {
                        None => h,
                        Some(prev) => {
                            let c31 = self.ir.add_expr(IrExpr::Const(IrConst::Int(31)));
                            let mul = self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::Mul, lhs: prev, rhs: c31 });
                            self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::Add, lhs: mul, rhs: h })
                        }
                    });
                }
                acc.unwrap()
            };
            let ret = self.ir.add_expr(IrExpr::Return(Some(result)));
            let body = self.ir.add_expr(IrExpr::Block { stmts: vec![ret], value: None });
            self.add_synth_method(internal, class_id, "hashCode", vec![], Ty::Int, body);
        }

        // equals(other): `if (other !is T) return false; if (f1 != o.f1) return false; …; return true`.
        {
            let class_ty = ty_to_ir(Ty::obj(internal));
            let mut stmts = Vec::new();
            let other = self.ir.add_expr(IrExpr::GetValue(1));
            let not_inst = self.ir.add_expr(IrExpr::TypeOp { op: IrTypeOp::NotInstanceOf, arg: other, type_operand: class_ty.clone() });
            let g = self.guard_return_false(not_inst);
            stmts.push(g);
            for (i, (_, t)) in fields.iter().enumerate() {
                let af = self.this_field(class_id, i as u32);
                let other_v = self.ir.add_expr(IrExpr::GetValue(1));
                let ocast = self.ir.add_expr(IrExpr::TypeOp { op: IrTypeOp::Cast, arg: other_v, type_operand: class_ty.clone() });
                let bf = self.ir.add_expr(IrExpr::GetField { receiver: ocast, class: class_id, index: i as u32 });
                let ne = self.field_ne(af, bf, *t);
                let g = self.guard_return_false(ne);
                stmts.push(g);
            }
            let t = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(true)));
            stmts.push(self.ir.add_expr(IrExpr::Return(Some(t))));
            let body = self.ir.add_expr(IrExpr::Block { stmts, value: None });
            let obj = ty_to_ir(Ty::obj("java/lang/Object"));
            self.add_synth_method(internal, class_id, "equals", vec![obj], Ty::Boolean, body);
        }

        // copy(f1, f2, …): `return P(f1, f2, …)`. (A `copy` call with named/omitted arguments — the
        // common form — still needs the `$default` mechanism; this enables the full-positional call.)
        {
            let params: Vec<IrType> = fields.iter().map(|(_, t)| ty_to_ir(*t)).collect();
            let args: Vec<u32> = (0..fields.len()).map(|i| self.ir.add_expr(IrExpr::GetValue(i as u32 + 1))).collect();
            let new = self.ir.add_expr(IrExpr::New { class: class_id, args });
            let ret = self.ir.add_expr(IrExpr::Return(Some(new)));
            let body = self.ir.add_expr(IrExpr::Block { stmts: vec![ret], value: None });
            if let Some(copy_fid) = self.add_synth_method(internal, class_id, "copy", params, Ty::obj(internal), body) {
                // Each `copy` parameter defaults to the corresponding property of the receiver — the
                // backend-agnostic meaning. (The JVM backend realizes this as `copy$default`.) The
                // `$default` mask is one `int`, so it covers ≤31 parameters; a wider data class uses
                // copy() positionally only (the multi-mask form isn't modeled).
                if fields.len() <= 31 {
                    let defaults: Vec<Option<u32>> = (0..fields.len()).map(|i| {
                        let this = self.ir.add_expr(IrExpr::GetValue(0));
                        Some(self.ir.add_expr(IrExpr::GetField { receiver: this, class: class_id, index: i as u32 }))
                    }).collect();
                    self.ir.fn_param_defaults.insert(copy_fid, defaults);
                    // `copy`'s parameters are named after the properties — used to map named args.
                    self.ir.fn_param_names.insert(copy_fid, fields.iter().map(|(n, _)| n.clone()).collect());
                }
            }
        }
    }

    fn lookup(&self, name: &str) -> Option<(u32, Ty)> {
        self.scope.iter().rev().find(|(n, _, _)| n == name).map(|(_, v, t)| (*v, *t))
    }

    fn class_of(&self, ty: Ty) -> Option<&ClassInfo> {
        ty.obj_internal().and_then(|i| self.classes.get(i))
    }

    fn top_fun_decl(&self, name: &str) -> Option<&ast::FunDecl> {
        self.afile.decls.iter().find_map(|&d| match self.afile.decl(d) {
            Decl::Fun(f) if f.name == name => Some(f),
            _ => None,
        })
    }

    fn class_decl(&self, name: &str) -> Option<&ast::ClassDecl> {
        self.afile.decls.iter().find_map(|&d| match self.afile.decl(d) {
            Decl::Class(c) if c.name == name => Some(c),
            _ => None,
        })
    }

    /// Lower a call's arguments, filling omitted trailing parameters from their **constant-literal**
    /// defaults (`fun f(x: Int = 5)` called `f()`). A non-literal default (one referencing other
    /// params or `this`) needs the `$default` synthetic method krusty doesn't emit yet → `None`.
    fn lower_args_defaulted(&mut self, call: AstExprId, param_meta: &[(String, Option<AstExprId>)], args: &[AstExprId], ir_params: &[IrType]) -> Option<Vec<u32>> {
        let n = ir_params.len();
        if args.len() > n {
            return None;
        }
        // Place each argument into its parameter slot: a positional arg fills the next free position;
        // a named arg (`x = …`) fills its named parameter. Unfilled slots take constant-literal
        // defaults. (Arguments are evaluated in slot order — fine for the side-effect-free common case.)
        let names = self.afile.call_arg_names.get(&call.0).cloned().unwrap_or_default();
        let mut slot: Vec<Option<AstExprId>> = vec![None; n];
        let mut pos = 0;
        for (i, &arg) in args.iter().enumerate() {
            match names.get(i).and_then(|o| o.as_ref()) {
                None => {
                    if pos >= n { return None; }
                    slot[pos] = Some(arg);
                    pos += 1;
                }
                Some(nm) => {
                    let idx = param_meta.iter().position(|(name, _)| name == nm)?;
                    if idx >= n || slot[idx].is_some() { return None; }
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
            if self.classes.get(&internal).map_or(false, |ci| self.ir.classes[ci.id as usize].is_object) {
                return Ty::obj(&internal);
            }
        }
        self.info.ty(receiver)
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
            if !seen.insert(i.clone()) { continue; }
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
    fn lower_arg(&mut self, arg: AstExprId, target: &IrType) -> Option<u32> {
        let at = self.info.ty(arg);
        let e = self.expr(arg)?;
        let target_ref = ir_type_is_reference(target);
        if at.is_primitive() && target_ref {
            Some(self.ir.add_expr(IrExpr::TypeOp { op: IrTypeOp::ImplicitCoercion, arg: e, type_operand: target.clone() }))
        } else if at.is_reference() && !target_ref && *target != IrType::Unit && *target != IrType::Error {
            Some(self.ir.add_expr(IrExpr::TypeOp { op: IrTypeOp::ImplicitCoercion, arg: e, type_operand: target.clone() }))
        } else if at.is_primitive() && !target_ref && *target != IrType::Error && *target != IrType::Unit && ty_to_ir(at) != *target {
            // Primitive numeric widening/narrowing (`Int` → `Long`, `Double` → `Int`): emit a
            // coercion (the backend does the `i2l`/`d2i`/… conversion).
            Some(self.ir.add_expr(IrExpr::TypeOp { op: IrTypeOp::ImplicitCoercion, arg: e, type_operand: target.clone() }))
        } else if at == Ty::obj("java/lang/Object") && target_ref && !ir_type_is_object(target) {
            // A generic type-parameter return is erased to `Object` in the JVM signature; flowing it
            // into a more specific reference target needs a `checkcast` (kotlinc inserts one — the
            // value really is the target type at runtime). `as`-style, but never null here.
            Some(self.ir.add_expr(IrExpr::TypeOp { op: IrTypeOp::Cast, arg: e, type_operand: target.clone() }))
        } else {
            Some(e)
        }
    }

    /// Resolve an `is`/`as` target `TypeRef` to a known **reference** `Ty` (`String` or a class in
    /// this IR); returns `None` to bail for primitives, nullables, or unknown types.
    fn ty_ref(&self, r: &ast::TypeRef) -> Option<Ty> {
        if r.nullable {
            return None;
        }
        let t = if let Some(p) = Ty::from_name(&r.name) {
            p
        } else if self.classes.contains_key(&r.name) {
            Ty::obj(&r.name)
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
        let Expr::Block { stmts, trailing } = self.afile.expr(block) else { return None };
        let depth = self.scope.len();
        let mut out = Vec::new();
        for &s in stmts {
            out.push(self.stmt(s)?);
        }
        if let Some(t) = trailing {
            let tt = self.info.ty(*t);
            let diverges = tt == Ty::Nothing;
            // A value-less statement (e.g. a no-`else` `when`) can only be a value-returning
            // function's body if it's exhaustive (hence diverging). krusty doesn't prove
            // exhaustiveness, so bail rather than emit `return <no-value>`.
            if *ret_ty != IrType::Unit && !diverges && tt == Ty::Unit {
                self.scope.truncate(depth);
                return None;
            }
            let ve = self.expr(*t)?;
            if *ret_ty == IrType::Unit || diverges {
                out.push(ve); // Unit trailing, or a diverging one (returns/throws itself — no wrap)
            } else {
                out.push(self.ir.add_expr(IrExpr::Return(Some(ve))));
            }
        }
        self.scope.truncate(depth);
        Some(self.ir.add_expr(IrExpr::Block { stmts: out, value: None }))
    }

    fn stmt(&mut self, s: crate::ast::StmtId) -> Option<u32> {
        match self.afile.stmt(s).clone() {
            Stmt::Expr(e) => self.expr(e),
            Stmt::Return(e) => {
                let v = match e {
                    // Coerce to the enclosing function's return type (generic-erased `Object` → cast).
                    Some(e) if self.cur_ret_ty != IrType::Unit && self.info.ty(e) != Ty::Nothing => {
                        let rt = self.cur_ret_ty.clone();
                        Some(self.lower_arg(e, &rt)?)
                    }
                    Some(e) => Some(self.expr(e)?),
                    None => None,
                };
                Some(self.ir.add_expr(IrExpr::Return(v)))
            }
            Stmt::Local { name, init, ty, .. } => {
                let init_ty = self.info.ty(init);
                // A diverging initializer (`val x = when { … all branches return … }`) never binds
                // anything — emit it for effect (it returns/throws), no slot store.
                if init_ty == Ty::Nothing {
                    return Some(self.expr(init)?);
                }
                // `Unit` as a stored value (the `kotlin.Unit` singleton) isn't modeled yet — bail.
                if init_ty == Ty::Unit {
                    return None;
                }
                // Use the declared type only when it's a builtin krusty `Ty`; for a user/class type
                // (`val en: En`) `Ty::from_name` is `None`, so fall back to the checker's inferred
                // type — otherwise the local is typed `Error` and e.g. `==` takes the wrong path.
                // Resolve the declared type: a builtin, else a known file class (`A?` → reference
                // `A`, not the `null` initializer's `Ty::Null`), else the checker's inferred type.
                let kty = match ty.as_ref() {
                    Some(r) if Ty::from_name(&r.name).is_some() => Ty::from_name(&r.name).unwrap(),
                    Some(r) if self.classes.contains_key(&class_internal(self.afile, &r.name)) => {
                        Ty::obj(&class_internal(self.afile, &r.name))
                    }
                    _ => init_ty,
                };
                // Coerce the initializer to the declared type (a generic-erased `Object` flowing into a
                // typed `val` gets the `checkcast` kotlinc inserts).
                let it = self.lower_arg(init, &ty_to_ir(kty))?;
                let v = self.fresh_value();
                self.scope.push((name.clone(), v, kty));
                Some(self.ir.add_expr(IrExpr::Variable { index: v, ty: ty_to_ir(kty), init: Some(it) }))
            }
            Stmt::Assign { name, value } => {
                if let Some((v, _)) = self.lookup(&name) {
                    let val = self.expr(value)?;
                    Some(self.ir.add_expr(IrExpr::SetValue { var: v, value: val }))
                } else if let Some((idx, ty)) = self.statics.get(&name).cloned() {
                    let val = self.lower_arg(value, &ty_to_ir(ty))?;
                    Some(self.ir.add_expr(IrExpr::SetStatic { index: idx, value: val }))
                } else {
                    // Unqualified write to a `var` field of the enclosing class (`this.<field> = …`).
                    let (this_v, _) = self.lookup("this")?;
                    let (class, idx, field_ty) = {
                        let ci = self.cur_class.as_ref().and_then(|c| self.classes.get(c))?;
                        let idx = ci.fields.iter().position(|(fn_, _)| *fn_ == name)? as u32;
                        (ci.id, idx, ty_to_ir(ci.fields[idx as usize].1))
                    };
                    let val = self.lower_arg(value, &field_ty)?;
                    let recv = self.ir.add_expr(IrExpr::GetValue(this_v));
                    Some(self.ir.add_expr(IrExpr::SetField { receiver: recv, class, index: idx, value: val }))
                }
            }
            // `name++` / `name--` on a local numeric variable → `name = name ± 1`. (In statement
            // position the pre/post distinction is irrelevant — the value isn't observed.) A built-in
            // numeric primitive only; a `var` field/property or a user `operator inc`/`dec` bails.
            Stmt::IncDec { name, dec } => {
                let (v, ty) = self.lookup(&name)?;
                let one = match ty {
                    Ty::Int | Ty::Byte | Ty::Short | Ty::Char => IrConst::Int(1),
                    Ty::Long => IrConst::Long(1),
                    Ty::Double => IrConst::Double(1.0),
                    Ty::Float => IrConst::Float(1.0),
                    _ => return None,
                };
                let cur = self.ir.add_expr(IrExpr::GetValue(v));
                let one = self.ir.add_expr(IrExpr::Const(one));
                let op = if dec { IrBinOp::Sub } else { IrBinOp::Add };
                let nv = self.ir.add_expr(IrExpr::PrimitiveBinOp { op, lhs: cur, rhs: one });
                Some(self.ir.add_expr(IrExpr::SetValue { var: v, value: nv }))
            }
            // `receiver.field = value` → `IrSetField` (var property of a class in this IR).
            Stmt::AssignMember { receiver, name, value } => {
                let rt = self.info.ty(receiver);
                let owner_internal = self.class_of(rt)?.internal.clone();
                // The backing field is private; a write from outside the declaring class goes through
                // the public `setX()` accessor (matching kotlinc). Inside the class, write directly.
                if self.cur_class.as_deref() != Some(owner_internal.as_str()) {
                    if let Some((mclass, mindex, mfid, _)) = self.resolve_method(&owner_internal, &setter_name(&name)) {
                        let pty = self.ir.functions[mfid as usize].params[0].clone();
                        let r = self.expr(receiver)?;
                        let v = self.lower_arg(value, &pty)?;
                        return Some(self.ir.add_expr(IrExpr::MethodCall { class: mclass, index: mindex, receiver: r, args: vec![Some(v)] }));
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
                Some(self.ir.add_expr(IrExpr::SetField { receiver: r, class, index: idx, value: v }))
            }
            Stmt::AssignIndex { array, index, value } => {
                let a = self.expr(array)?;
                let i = self.expr(index)?;
                let v = self.expr(value)?;
                Some(self.ir.add_expr(IrExpr::Call { callee: Callee::External("kotlin/Array.set".to_string()), dispatch_receiver: Some(a), args: vec![i, v] }))
            }
            Stmt::While { cond, body } => {
                let c = self.expr(cond)?;
                let Expr::Block { stmts, trailing } = self.afile.expr(body).clone() else { return None };
                let depth = self.scope.len();
                let mut out = Vec::new();
                for s in stmts {
                    out.push(self.stmt(s)?);
                }
                // A body ending in an expression (`… ; if (c) break`) keeps it as a discarded statement.
                if let Some(t) = trailing {
                    out.push(self.expr(t)?);
                }
                self.scope.truncate(depth);
                let b = self.ir.add_expr(IrExpr::Block { stmts: out, value: None });
                Some(self.ir.add_expr(IrExpr::While { cond: c, body: b, update: None, post_test: false }))
            }
            Stmt::DoWhile { body, cond } => {
                let Expr::Block { stmts, trailing } = self.afile.expr(body).clone() else { return None };
                let depth = self.scope.len();
                let mut out = Vec::new();
                for s in stmts {
                    out.push(self.stmt(s)?);
                }
                if let Some(t) = trailing {
                    out.push(self.expr(t)?);
                }
                self.scope.truncate(depth);
                // The condition is lowered after the body's scope is dropped — a `do…while` condition
                // can't see body-local declarations (Kotlin scopes them to the body).
                let c = self.expr(cond)?;
                let b = self.ir.add_expr(IrExpr::Block { stmts: out, value: None });
                Some(self.ir.add_expr(IrExpr::While { cond: c, body: b, update: None, post_test: true }))
            }
            Stmt::Break => Some(self.ir.add_expr(IrExpr::Break)),
            Stmt::Continue => Some(self.ir.add_expr(IrExpr::Continue)),
            // `for (i in a..b [step s])` over an `Int` range → a counted `while`. The bound is
            // hoisted to a local (evaluated once, per Kotlin); the step defaults to 1.
            Stmt::For { name, range, body } => {
                use crate::ast::RangeKind;
                let depth = self.scope.len();
                // loop var = start
                let start = self.expr(range.start)?;
                let i_v = self.fresh_value();
                self.scope.push((name.clone(), i_v, Ty::Int));
                let var_i = self.ir.add_expr(IrExpr::Variable { index: i_v, ty: ty_to_ir(Ty::Int), init: Some(start) });
                // hoisted bound
                let end_e = self.expr(range.end)?;
                let end_v = self.fresh_value();
                let var_end = self.ir.add_expr(IrExpr::Variable { index: end_v, ty: ty_to_ir(Ty::Int), init: Some(end_e) });
                // condition
                let cmp = match range.kind { RangeKind::Through => IrBinOp::Le, RangeKind::Until => IrBinOp::Lt, RangeKind::DownTo => IrBinOp::Ge };
                let gi = self.ir.add_expr(IrExpr::GetValue(i_v));
                let ge = self.ir.add_expr(IrExpr::GetValue(end_v));
                let cond = self.ir.add_expr(IrExpr::PrimitiveBinOp { op: cmp, lhs: gi, rhs: ge });
                // body + increment
                let Expr::Block { stmts, trailing } = self.afile.expr(body).clone() else { self.scope.truncate(depth); return None };
                let mut out = Vec::new();
                for s in stmts {
                    out.push(self.stmt(s)?);
                }
                if let Some(t) = trailing {
                    out.push(self.expr(t)?);
                }
                let step = match range.step { Some(e) => self.expr(e)?, None => self.ir.add_expr(IrExpr::Const(IrConst::Int(1))) };
                let inc_op = if matches!(range.kind, RangeKind::DownTo) { IrBinOp::Sub } else { IrBinOp::Add };
                let gi2 = self.ir.add_expr(IrExpr::GetValue(i_v));
                let inc_val = self.ir.add_expr(IrExpr::PrimitiveBinOp { op: inc_op, lhs: gi2, rhs: step });
                // The increment is the loop `update` (runs at the `continue` target), not a body stmt —
                // so `continue` advances the counter instead of skipping it.
                let inc = self.ir.add_expr(IrExpr::SetValue { var: i_v, value: inc_val });
                let wbody = self.ir.add_expr(IrExpr::Block { stmts: out, value: None });
                let wh = self.ir.add_expr(IrExpr::While { cond, body: wbody, update: Some(inc), post_test: false });
                self.scope.truncate(depth);
                Some(self.ir.add_expr(IrExpr::Block { stmts: vec![var_i, var_end, wh], value: None }))
            }
            // `for (x in arr)` over an array → an index loop `i=0; while (i<arr.size) { x=arr[i]; …; i++ }`.
            Stmt::ForEach { name, iterable, body } => {
                let it_ty = self.info.ty(iterable);
                let elem = it_ty.array_elem()?; // only array iteration is modeled
                let depth = self.scope.len();
                // Evaluate the array once into a temp.
                let arr_v = self.fresh_value();
                let arr_val = self.expr(iterable)?;
                let var_arr = self.ir.add_expr(IrExpr::Variable { index: arr_v, ty: ty_to_ir(it_ty), init: Some(arr_val) });
                // i = 0
                let i_v = self.fresh_value();
                let zero = self.ir.add_expr(IrExpr::Const(IrConst::Int(0)));
                let var_i = self.ir.add_expr(IrExpr::Variable { index: i_v, ty: ty_to_ir(Ty::Int), init: Some(zero) });
                // n = arr.size (hoisted)
                let n_v = self.fresh_value();
                let arr_g = self.ir.add_expr(IrExpr::GetValue(arr_v));
                let size = self.ir.add_expr(IrExpr::Call { callee: Callee::External("kotlin/Array.size".to_string()), dispatch_receiver: Some(arr_g), args: vec![] });
                let var_n = self.ir.add_expr(IrExpr::Variable { index: n_v, ty: ty_to_ir(Ty::Int), init: Some(size) });
                // condition: i < n
                let gi = self.ir.add_expr(IrExpr::GetValue(i_v));
                let gn = self.ir.add_expr(IrExpr::GetValue(n_v));
                let cond = self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::Lt, lhs: gi, rhs: gn });
                // loop var `x = arr[i]`, bound for the body
                let x_v = self.fresh_value();
                self.scope.push((name.clone(), x_v, elem));
                let arr_g2 = self.ir.add_expr(IrExpr::GetValue(arr_v));
                let gi2 = self.ir.add_expr(IrExpr::GetValue(i_v));
                let getq = if it_ty == Ty::String { "kotlin/String.get" } else { "kotlin/Array.get" };
                let elem_get = self.ir.add_expr(IrExpr::Call { callee: Callee::External(getq.to_string()), dispatch_receiver: Some(arr_g2), args: vec![gi2] });
                let var_x = self.ir.add_expr(IrExpr::Variable { index: x_v, ty: ty_to_ir(elem), init: Some(elem_get) });
                let Expr::Block { stmts, trailing } = self.afile.expr(body).clone() else { self.scope.truncate(depth); return None };
                let mut out = vec![var_x];
                for s in stmts {
                    out.push(self.stmt(s)?);
                }
                if let Some(t) = trailing {
                    out.push(self.expr(t)?);
                }
                let gi3 = self.ir.add_expr(IrExpr::GetValue(i_v));
                let one = self.ir.add_expr(IrExpr::Const(IrConst::Int(1)));
                let inc = self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::Add, lhs: gi3, rhs: one });
                let incs = self.ir.add_expr(IrExpr::SetValue { var: i_v, value: inc });
                let wbody = self.ir.add_expr(IrExpr::Block { stmts: out, value: None });
                let wh = self.ir.add_expr(IrExpr::While { cond, body: wbody, update: Some(incs), post_test: false });
                self.scope.truncate(depth);
                Some(self.ir.add_expr(IrExpr::Block { stmts: vec![var_arr, var_i, var_n, wh], value: None }))
            }
            _ => None,
        }
    }

    fn expr(&mut self, e: AstExprId) -> Option<u32> {
        Some(match self.afile.expr(e).clone() {
            Expr::IntLit(v) => self.ir.add_expr(IrExpr::Const(IrConst::Int(v as i32))),
            Expr::LongLit(v) => self.ir.add_expr(IrExpr::Const(IrConst::Long(v))),
            Expr::DoubleLit(v) => self.ir.add_expr(IrExpr::Const(IrConst::Double(v))),
            Expr::FloatLit(v) => self.ir.add_expr(IrExpr::Const(IrConst::Float(v))),
            Expr::CharLit(c) => self.ir.add_expr(IrExpr::Const(IrConst::Char(c))),
            Expr::BoolLit(b) => self.ir.add_expr(IrExpr::Const(IrConst::Boolean(b))),
            Expr::StringLit(s) => self.ir.add_expr(IrExpr::Const(IrConst::String(s))),
            Expr::NullLit => self.ir.add_expr(IrExpr::Const(IrConst::Null)),
            // `throw e` — throw the exception value; control never returns.
            Expr::Throw { operand } => {
                let v = self.expr(operand)?;
                self.ir.add_expr(IrExpr::Throw { operand: v })
            }
            // `try { … } catch (e: E) { … } … [finally { f }]` (nested try already rejected by checker).
            Expr::Try { body, catches, finally } => {
                // A `finally` is inlined at each exit; a `return`/`break`/`continue` out of the body or
                // a catch would need the `finally` run before it (not modeled) — bail in that case.
                if finally.is_some()
                    && (body_has_nonlocal_exit(self.afile, body)
                        || catches.iter().any(|c| body_has_nonlocal_exit(self.afile, c.body)))
                {
                    return None;
                }
                let result = ty_to_ir(self.info.ty(e));
                let body_ir = self.expr(body)?;
                let mut ir_catches = Vec::new();
                for c in &catches {
                    let exc_internal = self.catch_internal(&c.ty.name)?;
                    let v = self.fresh_value();
                    self.scope.push((c.name.clone(), v, Ty::obj(&exc_internal)));
                    let cbody = self.expr(c.body)?;
                    self.scope.pop();
                    ir_catches.push(crate::ir::IrCatch { var: v, exc_internal, body: cbody });
                }
                let fin = match finally {
                    Some(f) => Some(self.expr(f)?),
                    None => None,
                };
                self.ir.add_expr(IrExpr::Try { body: body_ir, catches: ir_catches, finally: fin, result })
            }
            // `operand!!` — assert non-null. On a reference, `Intrinsics.checkNotNull` throws if null
            // and yields the value; on a (non-null) primitive it is a no-op.
            Expr::NotNull { operand } => {
                let v = self.expr(operand)?;
                if self.info.ty(operand).is_reference() {
                    self.ir.add_expr(IrExpr::NotNullAssert { operand: v })
                } else {
                    v
                }
            }
            // `r?.m(args)` / `r?.p` → `{ val t = r; if (t != null) t.m(args)/t.p else null }`.
            Expr::SafeCall { receiver, name, args } => {
                let rty = self.info.ty(receiver);
                let result_ty = self.info.ty(e);
                // Only reference receiver + reference result are modeled (a nullable-primitive result
                // would need boxing the member value, which the checker rejects anyway).
                if !rty.is_reference() || !result_ty.is_reference() {
                    return None;
                }
                let internal = rty.obj_internal()?.to_string();
                let rv = self.expr(receiver)?;
                let v = self.fresh_value();
                let var = self.ir.add_expr(IrExpr::Variable { index: v, ty: ty_to_ir(rty), init: Some(rv) });
                let get1 = self.ir.add_expr(IrExpr::GetValue(v));
                let nullc = self.ir.add_expr(IrExpr::Const(IrConst::Null));
                let cond = self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::Ne, lhs: get1, rhs: nullc });
                let recv2 = self.ir.add_expr(IrExpr::GetValue(v));
                let member = match args {
                    Some(args) => {
                        let (class, index, fid, _) = self.resolve_method(&internal, &name)?;
                        let params = self.ir.functions[fid as usize].params.clone();
                        if args.len() != params.len() { return None; }
                        let mut a = Vec::new();
                        for (arg, pt) in args.iter().zip(&params) {
                            a.push(self.lower_arg(*arg, pt)?);
                        }
                        self.ir.add_expr(IrExpr::MethodCall { class, index, receiver: recv2, args: a.into_iter().map(Some).collect() })
                    }
                    None => {
                        let (fclass, idx, _) = self.resolve_field(&internal, &name)?;
                        let owner_internal = self.ir.classes[fclass as usize].fq_name.clone();
                        // External read → `getX()` (the backing field is private); internal → field.
                        if self.cur_class.as_deref() != Some(owner_internal.as_str()) {
                            if let Some((mclass, mindex, _, _)) = self.resolve_method(&internal, &getter_name(&name)) {
                                self.ir.add_expr(IrExpr::MethodCall { class: mclass, index: mindex, receiver: recv2, args: vec![] })
                            } else {
                                self.ir.add_expr(IrExpr::GetField { receiver: recv2, class: fclass, index: idx })
                            }
                        } else {
                            self.ir.add_expr(IrExpr::GetField { receiver: recv2, class: fclass, index: idx })
                        }
                    }
                };
                let nullb = self.ir.add_expr(IrExpr::Const(IrConst::Null));
                let when = self.ir.add_expr(IrExpr::When { branches: vec![(Some(cond), member), (None, nullb)] });
                self.ir.add_expr(IrExpr::Block { stmts: vec![var], value: Some(when) })
            }
            // `a ?: b` → `{ val t = a; if (t != null) t else b }` (t bound once, so `a` runs once).
            Expr::Elvis { lhs, rhs } => {
                let lty = self.info.ty(lhs);
                // A statically-null or non-reference lhs isn't a meaningful elvis (and would emit a
                // bad-typed null compare) — bail. The common reference case is handled below.
                if lty == Ty::Null || !lty.is_reference() {
                    return None;
                }
                let lv = self.expr(lhs)?;
                let v = self.fresh_value();
                let var = self.ir.add_expr(IrExpr::Variable { index: v, ty: ty_to_ir(lty), init: Some(lv) });
                let get1 = self.ir.add_expr(IrExpr::GetValue(v));
                let nullc = self.ir.add_expr(IrExpr::Const(IrConst::Null));
                let cond = self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::Ne, lhs: get1, rhs: nullc });
                let get2 = self.ir.add_expr(IrExpr::GetValue(v));
                let rv = self.expr(rhs)?;
                let when = self.ir.add_expr(IrExpr::When { branches: vec![(Some(cond), get2), (None, rv)] });
                self.ir.add_expr(IrExpr::Block { stmts: vec![var], value: Some(when) })
            }
            // A block in expression position: `{ stmt; …; trailing }`; value is the trailing expr.
            Expr::Block { stmts, trailing } => {
                let depth = self.scope.len();
                let mut out = Vec::new();
                for &s in &stmts {
                    match self.stmt(s) {
                        Some(v) => out.push(v),
                        None => { self.scope.truncate(depth); return None; }
                    }
                }
                let value = match trailing {
                    Some(t) => match self.expr(t) {
                        Some(v) => Some(v),
                        None => { self.scope.truncate(depth); return None; }
                    },
                    None => None,
                };
                self.scope.truncate(depth);
                self.ir.add_expr(IrExpr::Block { stmts: out, value })
            }
            Expr::Lambda { params, body } => return self.lower_lambda(e, &params, body),
            // Unbound top-level function reference `::foo` → the same `invokedynamic` +
            // `LambdaMetafactory` machinery as a lambda, but the impl method handle points directly at
            // the referenced function (no synthesized body). Bound/object/constructor references bail.
            Expr::CallableRef { receiver, name } => {
                if receiver.is_some() {
                    return None;
                }
                let Ty::Fun(sig) = self.info.ty(e) else { return None };
                let arity = sig.params.len();
                let fid = *self.fun_ids.get(&name)?;
                // Same guards as lambdas: a `Unit`/`Nothing` return needs the `kotlin/Unit` singleton,
                // and a generic referenced function erases its type parameters.
                let ret = self.ir.functions[fid as usize].ret.clone();
                if ret == IrType::Unit || ret == IrType::Nothing {
                    return None;
                }
                if self.ir.functions[fid as usize].params.len() != arity {
                    return None;
                }
                if self.top_fun_decl(&name).map_or(false, |f| !f.type_params.is_empty()) {
                    return None;
                }
                return Some(self.ir.add_expr(IrExpr::Lambda { impl_fn: fid, arity: arity as u8, captures: vec![] }));
            }
            Expr::Name(n) => {
                if let Some((v, _)) = self.lookup(&n) {
                    self.ir.add_expr(IrExpr::GetValue(v))
                } else if let Some(&(fid, _)) = self.computed_props.get(&n) {
                    // A computed top-level property → call its `getX()` accessor.
                    self.ir.add_expr(IrExpr::Call { callee: Callee::Local(fid), dispatch_receiver: None, args: vec![] })
                } else if let Some(&(idx, _)) = self.statics.get(&n) {
                    self.ir.add_expr(IrExpr::GetStatic(idx))
                } else if let Some(class) = self.classes.get(&class_internal(self.afile, &n)).filter(|ci| self.ir.classes[ci.id as usize].is_object).map(|ci| ci.id) {
                    // A bare `object` name → its singleton instance.
                    self.ir.add_expr(IrExpr::StaticInstance { owner: class, ty: class, field: "INSTANCE" })
                } else {
                    // Unqualified member of the enclosing class: a backing field (`this.<field>`), or a
                    // computed property (`this.getX()`).
                    let (this_v, _) = self.lookup("this")?;
                    let cur = self.cur_class.clone()?;
                    let field = self.classes.get(&cur).and_then(|ci| ci.fields.iter().position(|(fn_, _)| *fn_ == n).map(|i| (ci.id, i as u32)));
                    let recv = self.ir.add_expr(IrExpr::GetValue(this_v));
                    if let Some((class, idx)) = field {
                        self.ir.add_expr(IrExpr::GetField { receiver: recv, class, index: idx })
                    } else {
                        let (class, index, _, _) = self.resolve_method(&cur, &getter_name(&n))?;
                        self.ir.add_expr(IrExpr::MethodCall { class, index, receiver: recv, args: vec![] })
                    }
                }
            }
            // `a[i]` read → an intrinsic; `String[i]` is `kotlin/String.get` (a `Char`), else
            // `kotlin/Array.get` (backend reads element from the receiver type).
            Expr::Index { array, index } => {
                let fq = if self.info.ty(array) == Ty::String { "kotlin/String.get" } else { "kotlin/Array.get" };
                let a = self.expr(array)?;
                let i = self.expr(index)?;
                self.ir.add_expr(IrExpr::Call { callee: Callee::External(fq.to_string()), dispatch_receiver: Some(a), args: vec![i] })
            }
            Expr::Member { receiver, name } => {
                // `EnumClass.ENTRY` — a static enum-constant read.
                if let Expr::Name(rn) = self.afile.expr(receiver).clone() {
                    let internal = class_internal(self.afile, &rn);
                    if let Some(ci) = self.classes.get(&internal) {
                        let cls = ci.id;
                        if let Some(idx) = self.ir.classes[cls as usize].enum_entries.iter().position(|(n, _)| *n == name) {
                            return Some(self.ir.add_expr(IrExpr::EnumEntry { class: cls, index: idx as u32 }));
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
                            return Some(self.ir.add_expr(IrExpr::Call { callee: Callee::External(fq), dispatch_receiver: Some(recv), args: vec![] }));
                        }
                    }
                }
                if rt == Ty::Char && name == "code" {
                    // `c.code` → the `Char`'s code unit as an `Int` (a no-op coercion on the JVM stack).
                    let c = self.expr(receiver)?;
                    self.ir.add_expr(IrExpr::TypeOp { op: IrTypeOp::ImplicitCoercion, arg: c, type_operand: ty_to_ir(Ty::Int) })
                } else if rt.array_elem().is_some() && name == "size" {
                    let a = self.expr(receiver)?;
                    self.ir.add_expr(IrExpr::Call { callee: Callee::External("kotlin/Array.size".to_string()), dispatch_receiver: Some(a), args: vec![] })
                } else if let Some(rci) = self.class_of(rt) {
                    // Resolve the field through the superclass chain — it may be declared on a base
                    // class (`b.baseField`). `class` is the *owning* class (whose fieldref we emit).
                    let recv_internal = rci.internal.clone();
                    if let Some((class, idx, _)) = self.resolve_field(&recv_internal, &name) {
                        let owner_internal = self.ir.classes[class as usize].fq_name.clone();
                        // The backing field is private; access from outside the declaring class goes
                        // through the public `getX()` accessor (matching kotlinc). Inside the class,
                        // read the field directly.
                        if self.cur_class.as_deref() != Some(owner_internal.as_str()) {
                            if let Some((mclass, mindex, _, _)) = self.resolve_method(&recv_internal, &getter_name(&name)) {
                                let recv = self.expr(receiver)?;
                                return Some(self.ir.add_expr(IrExpr::MethodCall { class: mclass, index: mindex, receiver: recv, args: vec![] }));
                            }
                        }
                        let recv = self.expr(receiver)?;
                        // Smartcast: if the receiver's *slot* type isn't the owning class (e.g. an erased
                        // generic / `Any?` local narrowed by `is`), checkcast it so `getfield` is valid.
                        let needs_cast = matches!(self.afile.expr(receiver), Expr::Name(n)
                            if self.lookup(n).map_or(false, |(_, t)| t != Ty::obj(&owner_internal)));
                        let recv = if needs_cast {
                            self.ir.add_expr(IrExpr::TypeOp { op: IrTypeOp::Cast, arg: recv, type_operand: ty_to_ir(Ty::obj(&owner_internal)) })
                        } else { recv };
                        self.ir.add_expr(IrExpr::GetField { receiver: recv, class, index: idx })
                    } else if let Some((class, index, _, _)) = self.resolve_method(&recv_internal, &getter_name(&name)) {
                        // A computed property → `recv.getX()`.
                        let recv = self.expr(receiver)?;
                        self.ir.add_expr(IrExpr::MethodCall { class, index, receiver: recv, args: vec![] })
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
                        self.ir.add_expr(IrExpr::TypeOp { op: IrTypeOp::Cast, arg: recv, type_operand: ty_to_ir(Ty::String) })
                    } else { recv };
                    self.ir.add_expr(IrExpr::Call { callee: Callee::External("kotlin/String.length".to_string()), dispatch_receiver: Some(recv), args: vec![] })
                } else {
                    return None;
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                // A user `operator fun LhsType.plus(…)` (etc.) extension overrides the builtin operator.
                let op_name = match op {
                    BinOp::Add => Some("plus"), BinOp::Sub => Some("minus"), BinOp::Mul => Some("times"),
                    BinOp::Div => Some("div"), BinOp::Rem => Some("rem"), _ => None,
                };
                if let Some(opn) = op_name {
                    let recv_desc = self.recv_ty(lhs).descriptor();
                    if let Some(&fid) = self.ext_fun_ids.get(&(recv_desc, opn.to_string())) {
                        let params = self.ir.functions[fid as usize].params.clone();
                        if params.len() == 2 {
                            let l = self.lower_arg(lhs, &params[0])?;
                            let r = self.lower_arg(rhs, &params[1])?;
                            return Some(self.ir.add_expr(IrExpr::Call { callee: Callee::Local(fid), dispatch_receiver: None, args: vec![l, r] }));
                        }
                    }
                }
                if op == BinOp::Add && (self.info.ty(lhs) == Ty::String || self.info.ty(rhs) == Ty::String) {
                    let l = self.expr(lhs)?;
                    let r = self.expr(rhs)?;
                    self.ir.add_expr(IrExpr::Call { callee: Callee::External("kotlin/String.plus".to_string()), dispatch_receiver: Some(l), args: vec![r] })
                } else {
                    let irop = bin_to_ir(op)?;
                    // A primitive op needs both operands in one type. For mixed numeric operands
                    // (`Double < Int`, `1 + 2L`) coerce both to the promoted type; the backend emits
                    // the conversion. Non-numeric mixes (e.g. involving `Char`) have no promotion → bail.
                    let (lt, rt) = (self.info.ty(lhs), self.info.ty(rhs));
                    let mut l = self.expr(lhs)?;
                    let mut r = self.expr(rhs)?;
                    if lt.is_primitive() && rt.is_primitive() && lt != rt {
                        let p = Ty::promote(lt, rt)?;
                        let pir = ty_to_ir(p);
                        if lt != p { l = self.ir.add_expr(IrExpr::TypeOp { op: IrTypeOp::ImplicitCoercion, arg: l, type_operand: pir.clone() }); }
                        if rt != p { r = self.ir.add_expr(IrExpr::TypeOp { op: IrTypeOp::ImplicitCoercion, arg: r, type_operand: pir }); }
                    }
                    self.ir.add_expr(IrExpr::PrimitiveBinOp { op: irop, lhs: l, rhs: r })
                }
            }
            Expr::If { cond, then_branch, else_branch } => {
                // Constant-fold a literal-boolean condition (`if (false) { … }`) — emit only the taken
                // branch, like kotlinc's dead-code elimination. (Emitting the dead branch can produce
                // unverifiable frames, e.g. a `try` whose handler slot conflicts in unreachable code.)
                match self.afile.expr(cond) {
                    Expr::BoolLit(true) => return self.expr(then_branch),
                    Expr::BoolLit(false) => {
                        return match else_branch {
                            Some(els) => self.expr(els),
                            // `if (false) {}` with no else is a no-op `Unit` statement.
                            None => Some(self.ir.add_expr(IrExpr::Block { stmts: vec![], value: None })),
                        };
                    }
                    _ => {}
                }
                let c = self.expr(cond)?;
                let t = self.expr(then_branch)?;
                let branches = match else_branch {
                    Some(els) => { let e2 = self.expr(els)?; vec![(Some(c), t), (None, e2)] }
                    None => vec![(Some(c), t)],
                };
                self.ir.add_expr(IrExpr::When { branches })
            }
            // `x is T` / `x !is T` / `x as T` → the existing `IrTypeOp` node (no new node).
            Expr::Is { operand, ty, negated } => {
                let arg = self.expr(operand)?;
                let op = if negated { IrTypeOp::NotInstanceOf } else { IrTypeOp::InstanceOf };
                let type_operand = ty_to_ir(self.ty_ref(&ty)?);
                self.ir.add_expr(IrExpr::TypeOp { op, arg, type_operand })
            }
            Expr::InRange { value, start, end, kind, negated } => {
                use crate::ast::RangeKind;
                // Evaluate the bounds then the value once each (source order: start, end, value —
                // matching kotlinc's `start..end` then `.contains(value)`), into temps, then a
                // comparison chain. `!in` uses the De Morgan dual so no logical-not node is needed.
                let s = self.expr(start)?;
                let sv = self.fresh_value();
                let var_s = self.ir.add_expr(IrExpr::Variable { index: sv, ty: ty_to_ir(self.info.ty(start)), init: Some(s) });
                let en = self.expr(end)?;
                let ev = self.fresh_value();
                let var_e = self.ir.add_expr(IrExpr::Variable { index: ev, ty: ty_to_ir(self.info.ty(end)), init: Some(en) });
                let v = self.expr(value)?;
                let vv = self.fresh_value();
                let var_v = self.ir.add_expr(IrExpr::Variable { index: vv, ty: ty_to_ir(self.info.ty(value)), init: Some(v) });
                // `lo`/`hi` are the inclusive low / (in/ex)clusive high bound. `downTo` runs high→low, so
                // membership is `end <= value <= start` — swap the bounds.
                let (lo, hi, hi_strict) = match kind {
                    RangeKind::Through => (sv, ev, false),
                    RangeKind::Until => (sv, ev, true),
                    RangeKind::DownTo => (ev, sv, false),
                };
                let get = |this: &mut Self, idx: u32| this.ir.add_expr(IrExpr::GetValue(idx));
                let cond = if negated {
                    // value < lo  ||  value (> | >=) hi
                    let v1 = get(self, vv); let l1 = get(self, lo);
                    let c1 = self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::Lt, lhs: v1, rhs: l1 });
                    let v2 = get(self, vv); let h2 = get(self, hi);
                    let c2 = self.ir.add_expr(IrExpr::PrimitiveBinOp { op: if hi_strict { IrBinOp::Ge } else { IrBinOp::Gt }, lhs: v2, rhs: h2 });
                    self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::Or, lhs: c1, rhs: c2 })
                } else {
                    // lo <= value  &&  value (< | <=) hi
                    let l1 = get(self, lo); let v1 = get(self, vv);
                    let c1 = self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::Le, lhs: l1, rhs: v1 });
                    let v2 = get(self, vv); let h2 = get(self, hi);
                    let c2 = self.ir.add_expr(IrExpr::PrimitiveBinOp { op: if hi_strict { IrBinOp::Lt } else { IrBinOp::Le }, lhs: v2, rhs: h2 });
                    self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::And, lhs: c1, rhs: c2 })
                };
                self.ir.add_expr(IrExpr::Block { stmts: vec![var_s, var_e, var_v], value: Some(cond) })
            }
            Expr::As { operand, ty, nullable } => {
                // `as?` (safe cast: null on mismatch) isn't modeled — only the throwing `as`.
                if nullable {
                    return None;
                }
                let arg = self.expr(operand)?;
                let target = self.ty_ref(&ty)?;
                let type_operand = ty_to_ir(target);
                // `as T` to a non-null reference type throws on `null` (kotlinc null-checks before the
                // `checkcast`); `as T?` and primitive casts are a plain `checkcast`/coercion.
                let op = if !ty.nullable && target.is_reference() {
                    IrTypeOp::CastNonNull
                } else {
                    IrTypeOp::Cast
                };
                self.ir.add_expr(IrExpr::TypeOp { op, arg, type_operand })
            }
            Expr::Unary { op, operand } => {
                use crate::ast::UnOp;
                let v = self.expr(operand)?;
                match op {
                    // `-x` → `0 - x` (zero typed to match); `!x` → `x == false`.
                    UnOp::Neg => {
                        // `-x` → `0 - x` with the zero typed to the operand so both Sub operands
                        // share one numeric type (Byte/Short/Char negate in the `int` category).
                        let zero = match self.info.ty(operand) {
                            Ty::Long => self.ir.add_expr(IrExpr::Const(IrConst::Long(0))),
                            Ty::Double => self.ir.add_expr(IrExpr::Const(IrConst::Double(0.0))),
                            Ty::Float => self.ir.add_expr(IrExpr::Const(IrConst::Float(0.0))),
                            _ => self.ir.add_expr(IrExpr::Const(IrConst::Int(0))),
                        };
                        self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::Sub, lhs: zero, rhs: v })
                    }
                    UnOp::Not => {
                        let f = self.ir.add_expr(IrExpr::Const(IrConst::Boolean(false)));
                        self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::Eq, lhs: v, rhs: f })
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
                            if st.is_primitive() != self.info.ty(c).is_primitive() {
                                return None;
                            }
                        }
                    }
                }
                // A *branchy* subject (`when (when …)`) is evaluated ONCE into a temp so it runs on a
                // clean stack and isn't re-emitted per condition. A plain subject (a name/field) is
                // cheaply re-evaluated per comparison instead — that path also stays correct for a
                // smart-cast local (whose slot type differs from its static type), which a temp store
                // would mis-frame.
                let subj_tmp = match subject {
                    Some(subj) if is_branchy(self.afile, subj) => {
                        let sv = self.expr(subj)?;
                        let v = self.fresh_value();
                        let var = self.ir.add_expr(IrExpr::Variable { index: v, ty: ty_to_ir(self.info.ty(subj)), init: Some(sv) });
                        Some((v, var))
                    }
                    _ => None,
                };
                let last = arms.len().saturating_sub(1);
                let mut branches = Vec::new();
                for (ai, arm) in arms.iter().enumerate() {
                    let body = self.expr(arm.body)?;
                    if arm.conditions.is_empty() || (make_last_else && ai == last) {
                        branches.push((None, body)); // else (real, or the exhaustive last arm)
                    } else {
                        let mut cond: Option<u32> = None;
                        for &c in &arm.conditions {
                            let test = match (subj_tmp, subject) {
                                (Some((v, _)), _) => {
                                    let s = self.ir.add_expr(IrExpr::GetValue(v));
                                    let cv = self.expr(c)?;
                                    self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::Eq, lhs: s, rhs: cv })
                                }
                                (None, Some(subj)) => {
                                    let s = self.expr(subj)?;
                                    let cv = self.expr(c)?;
                                    self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::Eq, lhs: s, rhs: cv })
                                }
                                (None, None) => self.expr(c)?,
                            };
                            cond = Some(match cond {
                                Some(prev) => self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::Or, lhs: prev, rhs: test }),
                                None => test,
                            });
                        }
                        branches.push((cond, body));
                    }
                }
                let when = self.ir.add_expr(IrExpr::When { branches });
                // Prepend the subject-temp declaration (if any) so it's evaluated before the arms.
                match subj_tmp {
                    Some((_, var)) => self.ir.add_expr(IrExpr::Block { stmts: vec![var], value: Some(when) }),
                    None => when,
                }
            }
            Expr::Template(parts) => {
                let mut iter = parts.iter();
                let mut acc = match iter.clone().next() {
                    Some(TemplatePart::Str(s)) => { iter.next(); self.ir.add_expr(IrExpr::Const(IrConst::String(s.clone()))) }
                    _ => self.ir.add_expr(IrExpr::Const(IrConst::String(String::new()))),
                };
                for part in iter {
                    let rhs = match part {
                        TemplatePart::Str(s) => self.ir.add_expr(IrExpr::Const(IrConst::String(s.clone()))),
                        TemplatePart::Expr(e) => self.expr(*e)?,
                    };
                    acc = self.ir.add_expr(IrExpr::Call { callee: Callee::External("kotlin/String.plus".to_string()), dispatch_receiver: Some(acc), args: vec![rhs] });
                }
                acc
            }
            Expr::Call { callee, args } => match self.afile.expr(callee).clone() {
                // Local top-level function, or constructor `C(args)`.
                Expr::Name(fname) => {
                    // `f(args)` where `f` is a field/property of the enclosing class (not a local value or
                    // a top-level function) — invoking a function value through a field isn't modeled;
                    // bail rather than miscompile (it would emit a bogus constructor call).
                    if self.lookup(&fname).is_none() && !self.fun_ids.contains_key(&fname) {
                        if let Some(cur) = self.cur_class.clone() {
                            if self.classes.get(&cur).map_or(false, |ci| ci.fields.iter().any(|(n, _)| *n == fname)) {
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
                        return Some(self.ir.add_expr(IrExpr::InvokeFunction { func, args: a, ret }));
                    }
                    // Primitive-array size constructor `IntArray(n)` → a per-element intrinsic that
                    // encodes the element type (so the backend picks the right allocation).
                    if prim_array_elem(&fname).is_some() && args.len() == 1 {
                        let size = self.expr(args[0])?;
                        return Some(self.ir.add_expr(IrExpr::Call { callee: Callee::External(format!("kotlin/{fname}.<init>")), dispatch_receiver: None, args: vec![size] }));
                    }
                    if let Some(&fid) = self.fun_ids.get(&fname) {
                        // A `vararg` function: pack the trailing arguments into a fresh array for the
                        // last (array) parameter. (Spread `*arr` and a branchy element are unsupported.)
                        if let Some(sig) = self.syms.funs.get(&fname).filter(|s| s.vararg).cloned() {
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
                            let mut elements = Vec::new();
                            for &arg in &args[fixed..] {
                                if is_branchy(self.afile, arg) {
                                    return None;
                                }
                                elements.push(self.lower_arg(arg, &elem_ir)?);
                            }
                            let arr = self.ir.add_expr(IrExpr::Vararg { element_type: elem_ir, elements });
                            a.push(arr);
                            return Some(self.ir.add_expr(IrExpr::Call { callee: Callee::Local(fid), dispatch_receiver: None, args: a }));
                        }
                        let params = self.ir.functions[fid as usize].params.clone();
                        // Omitted trailing args are filled from constant-literal defaults.
                        let meta: Vec<(String, Option<AstExprId>)> = self.top_fun_decl(&fname)
                            .map(|f| f.params.iter().map(|p| (p.name.clone(), p.default)).collect())
                            .unwrap_or_default();
                        let a = self.lower_args_defaulted(e, &meta, &args, &params)?;
                        self.ir.add_expr(IrExpr::Call { callee: Callee::Local(fid), dispatch_receiver: None, args: a })
                    } else if let Some((class, index, mfid, _)) = self.cur_class.clone().and_then(|cur| self.resolve_method(&cur, &fname)) {
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
                        self.ir.add_expr(IrExpr::MethodCall { class, index, receiver: this, args: a.into_iter().map(Some).collect() })
                    } else if let Some(internal) = self.info.ty(e).obj_internal().filter(|i| !self.classes.contains_key(*i)) {
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
                        // Coerce each argument to its constructor-parameter field type, filling named
                        // args + constant-literal defaults (`LongWrapper(2)`, `C(y = 1)`, `C()`).
                        let field_tys: Vec<IrType> = self.ir.classes[class as usize].fields[..ctor_count]
                            .iter().map(|(_, t)| t.clone()).collect();
                        let meta: Vec<(String, Option<AstExprId>)> = self.class_decl(&fname)
                            .map(|cd| cd.props.iter().map(|p| (p.name.clone(), p.default)).collect())
                            .unwrap_or_default();
                        let a = self.lower_args_defaulted(e, &meta, &args, &field_tys)?;
                        self.ir.add_expr(IrExpr::New { class, args: a })
                    }
                }
                // Instance method call `recv.m(args)`, or a stdlib intrinsic method.
                Expr::Member { receiver, name } => {
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
                                return Some(self.ir.add_expr(IrExpr::Call { callee: Callee::Local(fid), dispatch_receiver: None, args: a }));
                            }
                        }
                    }
                    // Primitive numeric/`Char` conversions (`n.toLong()`, `c.toInt()`, `i.toChar()`, …)
                    // are coercions — the backend emits the `i2l`/`l2i`/`i2c`/… opcode.
                    {
                        let rty = self.info.ty(receiver);
                        if matches!(rty, Ty::Int | Ty::Long | Ty::Byte | Ty::Short | Ty::Char | Ty::Double | Ty::Float) {
                            if let Some(target) = crate::resolve::conversion_target(&name) {
                                if args.is_empty() {
                                    let r = self.expr(receiver)?;
                                    return Some(self.ir.add_expr(IrExpr::TypeOp { op: IrTypeOp::ImplicitCoercion, arg: r, type_operand: ty_to_ir(target) }));
                                }
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
                                "and" => Some(IrBinOp::BitAnd), "or" => Some(IrBinOp::BitOr), "xor" => Some(IrBinOp::BitXor),
                                "shl" => Some(IrBinOp::Shl), "shr" => Some(IrBinOp::Shr), "ushr" => Some(IrBinOp::Ushr),
                                _ => None,
                            };
                            if let (Some(op), 1) = (bop, args.len()) {
                                let l = self.expr(receiver)?;
                                let rt = if shift { Ty::Int } else { rty };
                                let r = self.lower_arg(args[0], &ty_to_ir(rt))?;
                                return Some(self.ir.add_expr(IrExpr::PrimitiveBinOp { op, lhs: l, rhs: r }));
                            }
                            if name == "inv" && args.is_empty() {
                                let l = self.expr(receiver)?;
                                let neg1 = self.ir.add_expr(IrExpr::Const(if rty == Ty::Long { IrConst::Long(-1) } else { IrConst::Int(-1) }));
                                return Some(self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::BitXor, lhs: l, rhs: neg1 }));
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
                                    return Some(self.ir.add_expr(IrExpr::EnumValues { class: cls }));
                                }
                                if name == "valueOf" && args.len() == 1 {
                                    let a = self.expr(args[0])?;
                                    return Some(self.ir.add_expr(IrExpr::EnumValueOf { class: cls, arg: a }));
                                }
                            }
                        }
                    }
                    // `C.foo(args)` — a companion-object method → `getstatic C.Companion; invokevirtual`.
                    if let Expr::Name(rn) = self.afile.expr(receiver).clone() {
                        let internal = class_internal(self.afile, &rn);
                        if let Some(comp_fq) = self.companions.get(&internal).cloned() {
                            if let Some((class, index, fid, _)) = self.resolve_method(&comp_fq, &name) {
                                let params = self.ir.functions[fid as usize].params.clone();
                                if args.len() != params.len() {
                                    return None;
                                }
                                let outer_id = self.classes[&internal].id;
                                let comp_id = self.classes[&comp_fq].id;
                                let recv = self.ir.add_expr(IrExpr::StaticInstance { owner: outer_id, ty: comp_id, field: "Companion" });
                                let mut a = Vec::new();
                                for (arg, pt) in args.iter().zip(&params) {
                                    a.push(self.lower_arg(*arg, pt)?);
                                }
                                return Some(self.ir.add_expr(IrExpr::MethodCall { class, index, receiver: recv, args: a.into_iter().map(Some).collect() }));
                            }
                        }
                    }
                    // A call to a method with parameter defaults, possibly with named/omitted args. Map
                    // each provided argument to its parameter position; omitted positions stay `None` (a
                    // call with holes). The backend fills the holes (JVM: `$default` stub + mask).
                    if let Some(internal) = self.class_of(self.recv_ty(receiver)).map(|ci| ci.internal.clone()) {
                        if let Some((class, index, fid, _)) = self.resolve_method(&internal, &name) {
                            if self.ir.fn_param_defaults.contains_key(&fid) {
                                let params = self.ir.functions[fid as usize].params.clone();
                                let n = params.len();
                                let param_names = self.ir.fn_param_names.get(&fid).cloned().unwrap_or_default();
                                let names = self.afile.call_arg_names.get(&e.0).cloned();
                                let mut provided: Vec<Option<u32>> = vec![None; n];
                                let mut next_pos = 0usize;
                                let mut ok = param_names.len() == n;
                                for (ai, arg) in args.iter().enumerate() {
                                    let nm = names.as_ref().and_then(|v| v.get(ai).cloned().flatten());
                                    let pos = match nm {
                                        Some(s) => param_names.iter().position(|f| *f == s),
                                        None => { let p = next_pos; next_pos += 1; Some(p) }
                                    };
                                    match pos {
                                        Some(p) if p < n => { let l = self.lower_arg(*arg, &params[p])?; provided[p] = Some(l); }
                                        _ => { ok = false; break; }
                                    }
                                }
                                if ok {
                                    let recv = self.expr(receiver)?;
                                    return Some(self.ir.add_expr(IrExpr::MethodCall { class, index, receiver: recv, args: provided }));
                                }
                            }
                        }
                    }
                    let rt = self.recv_ty(receiver);
                    if let Some((class, index, fid, _)) = self.class_of(rt).map(|ci| ci.internal.clone()).and_then(|i| self.resolve_method(&i, &name)) {
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
                        self.ir.add_expr(IrExpr::MethodCall { class, index, receiver: recv, args: a.into_iter().map(Some).collect() })
                    } else if name == "toString" && args.is_empty() {
                        // `x.toString()` → stdlib intrinsic, `String`.
                        let recv = self.expr(receiver)?;
                        self.ir.add_expr(IrExpr::Call { callee: Callee::External("kotlin/Any.toString".to_string()), dispatch_receiver: Some(recv), args: vec![] })
                    } else if let Some((internal, desc, is_iface)) = {
                        // A classpath *instance* method `recv.name(args)` → `invokevirtual`/
                        // `invokeinterface recvType.name:descriptor` (descriptor from the classpath; no
                        // hardcoded names). Enables stdlib member calls (iterators, collections, …).
                        let arg_tys: Vec<Ty> = args.iter().map(|&a| self.info.ty(a)).collect();
                        self.class_of(rt).map(|ci| ci.internal.clone())
                            .or_else(|| if let Ty::Obj(i) = rt { Some(i.to_string()) } else { None })
                            .and_then(|internal| {
                                crate::resolve::resolve_java_instance(&self.syms.classpath, &internal, &name, &arg_tys).map(|(d, _)| {
                                    let is_iface = self.syms.classpath.find(&internal).map_or(false, |c| c.is_interface());
                                    (internal, d, is_iface)
                                })
                            })
                    } {
                        let recv = self.expr(receiver)?;
                        let mut a = Vec::new();
                        for &arg in &args {
                            a.push(self.expr(arg)?);
                        }
                        self.ir.add_expr(IrExpr::Call { callee: Callee::Virtual { owner: internal, name: name.clone(), descriptor: desc, interface: is_iface }, dispatch_receiver: Some(recv), args: a })
                    } else if let Some((owner, jvm_name, desc, _)) = {
                        // A classpath-resolved extension/stdlib function `recv.name(args)` →
                        // `invokestatic facade.name(recv, args)`. Owner + descriptor come from the
                        // classpath (`resolve_extension`), so no stdlib name is hardcoded here.
                        let arg_tys: Vec<Ty> = args.iter().map(|&a| self.info.ty(a)).collect();
                        crate::resolve::resolve_extension(&self.syms.classpath, rt, &name, &arg_tys)
                    } {
                        let recv = self.expr(receiver)?;
                        let mut a = vec![recv];
                        for &arg in &args {
                            a.push(self.expr(arg)?);
                        }
                        self.ir.add_expr(IrExpr::Call { callee: Callee::Static { owner, name: jvm_name, descriptor: desc }, dispatch_receiver: None, args: a })
                    } else {
                        return None;
                    }
                }
                _ => return None,
            },
            _ => return None,
        })
    }
}

/// Whether `e` emits as a branch (a conditional that materializes via jumps + merge frames). Such an
/// expression can't be safely emitted while other operands sit on the stack (the merge frame would
/// omit them). Primitive `==`/`<`… and `if`/`when`/elvis are branchy; reference `==`
/// (`Intrinsics.areEqual`) and plain calls are not.
fn is_branchy(file: &ast::File, e: AstExprId) -> bool {
    match file.expr(e) {
        Expr::If { .. } | Expr::When { .. } | Expr::Elvis { .. } => true,
        Expr::Binary { op, lhs, .. } => {
            use ast::BinOp::*;
            matches!(op, Lt | Le | Gt | Ge | And | Or)
                || (matches!(op, Eq | Ne) && file_expr_is_primitive(file, *lhs))
        }
        Expr::Unary { op: ast::UnOp::Not, .. } => true,
        _ => false,
    }
}

/// Is `e` a compile-time constant literal (an argument-default krusty can inline at the call site)?
fn is_const_literal(file: &ast::File, e: AstExprId) -> bool {
    matches!(file.expr(e),
        Expr::IntLit(_) | Expr::LongLit(_) | Expr::DoubleLit(_) | Expr::FloatLit(_)
        | Expr::BoolLit(_) | Expr::CharLit(_) | Expr::StringLit(_) | Expr::NullLit)
}

/// Best-effort: is the literal/operand a primitive (so `==` would use a numeric branch, not
/// `Intrinsics.areEqual`)? Conservative — only obvious primitive literals count.
fn file_expr_is_primitive(file: &ast::File, e: AstExprId) -> bool {
    matches!(file.expr(e),
        Expr::IntLit(_) | Expr::LongLit(_) | Expr::DoubleLit(_) | Expr::FloatLit(_)
        | Expr::BoolLit(_) | Expr::CharLit(_))
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
fn ty_of(file: &ast::File, r: &ast::TypeRef) -> Ty {
    if let Some(t) = Ty::from_name(&r.name) {
        return t;
    }
    let is_class = file.decls.iter().any(|&d| matches!(file.decl(d), Decl::Class(c) if c.name == r.name));
    if is_class {
        Ty::obj(&class_internal(file, &r.name))
    } else {
        Ty::obj("java/lang/Object")
    }
}

/// Whether an `IrType` is a reference type (anything except a primitive class FqName / Unit).
fn ir_type_is_reference(t: &IrType) -> bool {
    match t {
        IrType::Class { fq_name, .. } => !matches!(
            fq_name.as_str(),
            "kotlin/Int" | "kotlin/Long" | "kotlin/Short" | "kotlin/Byte" | "kotlin/Boolean" | "kotlin/Char" | "kotlin/Double" | "kotlin/Float"
        ),
        IrType::Function { .. } => true,
        _ => false,
    }
}

/// Whether `t` is exactly `java/lang/Object` / `kotlin/Any` (the erased top type — no `checkcast` to it).
fn ir_type_is_object(t: &IrType) -> bool {
    matches!(t, IrType::Class { fq_name, .. } if fq_name == "java/lang/Any" || fq_name == "kotlin/Any" || fq_name == "java/lang/Object")
}

/// Whether `e` contains a `return` (always exits the function), or a `break`/`continue` that targets a
/// loop *outside* `e` (i.e. at loop-depth 0 here) — control transfers that would skip an enclosing
/// `finally`. Does not descend into lambdas (their control flow is separate).
fn body_has_nonlocal_exit(file: &ast::File, e: AstExprId) -> bool {
    fn ex(file: &ast::File, e: AstExprId, ld: u32) -> bool {
        let r = |x: AstExprId| ex(file, x, ld);
        match file.expr(e) {
            Expr::Name(_) | Expr::IntLit(_) | Expr::LongLit(_) | Expr::DoubleLit(_) | Expr::FloatLit(_)
            | Expr::BoolLit(_) | Expr::StringLit(_) | Expr::CharLit(_) | Expr::NullLit
            | Expr::Lambda { .. } | Expr::CallableRef { .. } => false,
            Expr::NotNull { operand } | Expr::Throw { operand } | Expr::Unary { operand, .. }
            | Expr::Is { operand, .. } | Expr::As { operand, .. } => r(*operand),
            Expr::InRange { value, start, end, .. } => r(*value) || r(*start) || r(*end),
            Expr::Elvis { lhs, rhs } | Expr::Binary { lhs, rhs, .. } => r(*lhs) || r(*rhs),
            Expr::Member { receiver, .. } => r(*receiver),
            Expr::Index { array, index } => r(*array) || r(*index),
            Expr::Call { callee, args } => r(*callee) || args.iter().any(|&a| r(a)),
            Expr::SafeCall { receiver, args, .. } => r(*receiver) || args.as_ref().map_or(false, |a| a.iter().any(|&x| r(x))),
            Expr::Template(parts) => parts.iter().any(|p| matches!(p, TemplatePart::Expr(x) if r(*x))),
            Expr::If { cond, then_branch, else_branch } => r(*cond) || r(*then_branch) || else_branch.map_or(false, |x| r(x)),
            Expr::Block { stmts, trailing } => stmts.iter().any(|&s| st(file, s, ld)) || trailing.map_or(false, |t| r(t)),
            Expr::When { subject, arms } => subject.map_or(false, |s| r(s)) || arms.iter().any(|a| a.conditions.iter().any(|&c| r(c)) || r(a.body)),
            Expr::Try { body, catches, finally } => r(*body) || catches.iter().any(|c| r(c.body)) || finally.map_or(false, |f| r(f)),
        }
    }
    fn st(file: &ast::File, s: crate::ast::StmtId, ld: u32) -> bool {
        match file.stmt(s) {
            Stmt::Return(_) => true,
            Stmt::Break | Stmt::Continue => ld == 0,
            Stmt::Expr(e) | Stmt::Local { init: e, .. } | Stmt::Assign { value: e, .. } | Stmt::Destructure { init: e, .. } => ex(file, *e, ld),
            // A loop's body raises the loop depth, so its `break`/`continue` are loop-local.
            Stmt::While { cond, body } => ex(file, *cond, ld) || ex(file, *body, ld + 1),
            Stmt::DoWhile { body, cond } => ex(file, *body, ld + 1) || ex(file, *cond, ld),
            Stmt::For { body, .. } | Stmt::ForEach { body, .. } => ex(file, *body, ld + 1),
            _ => false,
        }
    }
    ex(file, e, 0)
}

/// The element type of a primitive-array constructor name (`IntArray` → `Int`).
fn prim_array_elem(name: &str) -> Option<Ty> {
    Some(match name {
        "IntArray" => Ty::Int,
        "LongArray" => Ty::Long,
        "DoubleArray" => Ty::Double,
        "FloatArray" => Ty::Float,
        "BooleanArray" => Ty::Boolean,
        "CharArray" => Ty::Char,
        "ByteArray" => Ty::Byte,
        "ShortArray" => Ty::Short,
        _ => return None,
    })
}

/// Map a krusty `Ty` to a backend-agnostic `IrType` (a Kotlin FqName).
fn ty_to_ir(t: Ty) -> IrType {
    let fq = match t {
        Ty::Int => "kotlin/Int",
        Ty::Long => "kotlin/Long",
        Ty::Short => "kotlin/Short",
        Ty::Byte => "kotlin/Byte",
        Ty::Boolean => "kotlin/Boolean",
        Ty::Char => "kotlin/Char",
        Ty::Double => "kotlin/Double",
        Ty::Float => "kotlin/Float",
        Ty::String => "kotlin/String",
        Ty::Unit => return IrType::Unit,
        Ty::Nothing => return IrType::Nothing,
        Ty::Obj(n) => return IrType::Class { fq_name: n.to_string(), type_args: vec![], nullable: false },
        // A Kotlin function type `(A,…) -> R` is kept structural so each backend picks its own
        // representation (the JVM maps it to `kotlin/jvm/functions/FunctionN`, JS to a closure, …).
        Ty::Fun(s) => return IrType::Function {
            params: s.params.iter().map(|t| ty_to_ir(*t)).collect(),
            ret: Box::new(ty_to_ir(s.ret)),
        },
        // An array is a regular class type (`kotlin/IntArray`, `kotlin/Array<T>`); the backend lowers
        // its representation. Primitive arrays encode the element in the class name.
        Ty::Array(e) => {
            let fq = match *e {
                Ty::Int => "kotlin/IntArray", Ty::Long => "kotlin/LongArray", Ty::Double => "kotlin/DoubleArray",
                Ty::Float => "kotlin/FloatArray", Ty::Boolean => "kotlin/BooleanArray", Ty::Char => "kotlin/CharArray",
                Ty::Byte => "kotlin/ByteArray", Ty::Short => "kotlin/ShortArray",
                _ => return IrType::Class { fq_name: "kotlin/Array".to_string(), type_args: vec![ty_to_ir(*e)], nullable: false },
            };
            return IrType::Class { fq_name: fq.to_string(), type_args: vec![], nullable: false };
        }
        _ => return IrType::Error,
    };
    IrType::Class { fq_name: fq.to_string(), type_args: vec![], nullable: false }
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
    f.params.iter().zip(param_tys).map(|(p, ty)| {
        let is_type_param = f.type_params.contains(&p.ty.name);
        if !p.ty.nullable && !is_type_param && ty.is_reference() {
            Some(p.name.clone())
        } else {
            None
        }
    }).collect()
}

/// Split the parameter section of a JVM method descriptor (`(Ljava/lang/String;I)V`) into the
/// individual field descriptors (`["Ljava/lang/String;", "I"]`).
fn split_param_descriptors(desc: &str) -> Option<Vec<String>> {
    let inner = desc.strip_prefix('(')?.split(')').next()?;
    let b = inner.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        while b[i] == b'[' {
            i += 1;
            if i >= b.len() { return None; }
        }
        if b[i] == b'L' {
            while i < b.len() && b[i] != b';' { i += 1; }
            i += 1; // include the ';'
        } else {
            i += 1; // a single-char primitive
        }
        if i > b.len() { return None; }
        out.push(inner[start..i].to_string());
    }
    Some(out)
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
        BinOp::And => IrBinOp::And,
        BinOp::Or => IrBinOp::Or,
        _ => return None,
    })
}
