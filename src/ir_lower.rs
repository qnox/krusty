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
}

/// Lower a checked file to IR, or `None` if it uses anything outside the core subset.
pub fn lower_file(file: &ast::File, info: &TypeInfo, syms: &SymbolTable) -> Option<IrFile> {
    let mut lo = Lower {
        afile: file,
        info,
        ir: IrFile { package: file.package.clone(), ..Default::default() },
        fun_ids: HashMap::new(),
        classes: HashMap::new(),
        scope: Vec::new(),
        next_value: 0,
        cur_class: None,
    };

    // Only files of top-level functions + *simple* classes take the IR path.
    for &d in &file.decls {
        match file.decl(d) {
            Decl::Fun(f) if f.receiver.is_none() && !f.is_inline => {}
            Decl::Class(c) if is_simple_class(c) => {}
            _ => return None,
        }
    }

    // Pass 1a: register classes (id, fields) and reserve method FunIds.
    for &d in &file.decls {
        if let Decl::Class(c) = file.decl(d) {
            let internal = class_internal(file, &c.name);
            let fields: Vec<(String, Ty)> = c.props.iter().filter(|p| p.is_property)
                .map(|p| (p.name.clone(), ty_of(syms, &p.ty))).collect();
            let class_ty = IrType::Class { fq_name: internal.clone(), type_args: vec![], nullable: false };
            let id = lo.ir.add_class(IrClass {
                fq_name: internal.clone(),
                supertypes: vec![],
                fields: fields.iter().map(|(n, t)| (n.clone(), ty_to_ir(*t))).collect(),
                methods: vec![],
                is_interface: false,
            });
            let mut methods = HashMap::new();
            let mut method_fids = Vec::new();
            for (mi, m) in c.methods.iter().enumerate() {
                let sig = syms.classes.get(&c.name)?.methods.get(&m.name)?;
                let ret = sig.ret;
                let params: Vec<IrType> = sig.params.iter().map(|t| ty_to_ir(*t)).collect();
                let fid = lo.ir.add_fun(IrFunction {
                    name: m.name.clone(),
                    params,
                    ret: ty_to_ir(ret),
                    body: None,
                    is_static: false,
                    dispatch_receiver: Some(internal.clone()),
                });
                methods.insert(m.name.clone(), (mi as u32, fid, ret));
                method_fids.push(fid);
            }
            lo.ir.classes[id as usize].methods = method_fids;
            let _ = class_ty;
            lo.classes.insert(internal.clone(), ClassInfo { id, internal, fields, methods });
        }
    }
    // Pass 1b: register top-level functions.
    for &d in &file.decls {
        if let Decl::Fun(f) = file.decl(d) {
            let sig = syms.funs.get(&f.name)?;
            let params: Vec<IrType> = sig.params.iter().map(|t| ty_to_ir(*t)).collect();
            let ret = ty_to_ir(info.fun_ret_overrides.get(&f.name).copied().unwrap_or(sig.ret));
            let id = lo.ir.add_fun(IrFunction { name: f.name.clone(), params, ret, body: None, is_static: true, dispatch_receiver: None });
            lo.fun_ids.insert(f.name.clone(), id);
        }
    }

    // Pass 2: lower bodies.
    for &d in &file.decls {
        match file.decl(d) {
            Decl::Fun(f) => {
                let fid = lo.fun_ids[&f.name];
                lo.scope.clear();
                lo.next_value = 0;
                lo.cur_class = None;
                let sig = syms.funs.get(&f.name)?;
                for (p, t) in f.params.iter().zip(&sig.params) {
                    let v = lo.fresh_value();
                    lo.scope.push((p.name.clone(), v, *t));
                }
                let ret_ty = lo.ir.functions[fid as usize].ret.clone();
                lo.lower_body(&f.body, &ret_ty, fid)?;
            }
            Decl::Class(c) => {
                let internal = class_internal(file, &c.name);
                for m in &c.methods {
                    let (_, fid, _) = lo.classes[&internal].methods[&m.name];
                    lo.scope.clear();
                    lo.next_value = 0;
                    lo.cur_class = Some(internal.clone());
                    // `this` is value 0.
                    let this_v = lo.fresh_value();
                    lo.scope.push(("this".to_string(), this_v, Ty::obj(&internal)));
                    let sig = syms.classes.get(&c.name)?.methods.get(&m.name)?;
                    for (p, t) in m.params.iter().zip(&sig.params) {
                        let v = lo.fresh_value();
                        lo.scope.push((p.name.clone(), v, *t));
                    }
                    let ret_ty = lo.ir.functions[fid as usize].ret.clone();
                    lo.lower_body(&m.body, &ret_ty, fid)?;
                }
            }
            _ => {}
        }
    }
    Some(lo.ir)
}

/// A class is in the IR subset if: a primary constructor of only `val`/`var` properties, no base
/// class/interfaces, no body properties, no companion/secondary/init, and only expr-body methods.
fn is_simple_class(c: &ast::ClassDecl) -> bool {
    !c.is_data && !c.is_object && !c.is_enum && !c.is_interface && !c.is_abstract && !c.is_open
        && c.base_class.is_none() && c.supertypes.is_empty()
        && c.body_props.is_empty() && c.companion_methods.is_empty() && c.secondary_ctors.is_empty()
        && c.init_order.is_empty()
        && c.props.iter().all(|p| p.is_property)
        && c.methods.iter().all(|m| m.receiver.is_none() && matches!(m.body, FunBody::Expr(_)))
}

struct Lower<'a> {
    afile: &'a ast::File,
    info: &'a TypeInfo,
    ir: IrFile,
    fun_ids: HashMap<String, u32>,
    classes: HashMap<String, ClassInfo>,
    scope: Vec<(String, u32, Ty)>,
    next_value: u32,
    cur_class: Option<String>,
}

impl<'a> Lower<'a> {
    fn fresh_value(&mut self) -> u32 {
        let v = self.next_value;
        self.next_value += 1;
        v
    }

    fn lookup(&self, name: &str) -> Option<(u32, Ty)> {
        self.scope.iter().rev().find(|(n, _, _)| n == name).map(|(_, v, t)| (*v, *t))
    }

    fn class_of(&self, ty: Ty) -> Option<&ClassInfo> {
        ty.obj_internal().and_then(|i| self.classes.get(i))
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
        let b = match body {
            FunBody::Expr(e) => {
                let ve = self.expr(*e)?;
                let stmts = if *ret_ty == IrType::Unit {
                    vec![ve] // run for effect; the backend appends the single `return`
                } else {
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
            let ve = self.expr(*t)?;
            if *ret_ty == IrType::Unit {
                out.push(ve); // run the trailing for effect; the backend appends the single `return`
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
                    Some(e) => Some(self.expr(e)?),
                    None => None,
                };
                Some(self.ir.add_expr(IrExpr::Return(v)))
            }
            Stmt::Local { name, init, ty, .. } => {
                let it = self.expr(init)?;
                let kty = ty.as_ref().map(|r| Ty::from_name(&r.name).unwrap_or(Ty::Error)).unwrap_or_else(|| self.info.ty(init));
                let v = self.fresh_value();
                self.scope.push((name.clone(), v, kty));
                Some(self.ir.add_expr(IrExpr::Variable { index: v, ty: ty_to_ir(kty), init: Some(it) }))
            }
            Stmt::Assign { name, value } => {
                let (v, _) = self.lookup(&name)?;
                let val = self.expr(value)?;
                Some(self.ir.add_expr(IrExpr::SetValue { var: v, value: val }))
            }
            // `receiver.field = value` → `IrSetField` (var property of a class in this IR).
            Stmt::AssignMember { receiver, name, value } => {
                let rt = self.info.ty(receiver);
                let ci = self.class_of(rt)?;
                let idx = ci.fields.iter().position(|(fn_, _)| *fn_ == name)? as u32;
                let class = ci.id;
                let r = self.expr(receiver)?;
                let v = self.expr(value)?;
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
                let Expr::Block { stmts, trailing: None } = self.afile.expr(body).clone() else { return None };
                let depth = self.scope.len();
                let mut out = Vec::new();
                for s in stmts {
                    out.push(self.stmt(s)?);
                }
                self.scope.truncate(depth);
                let b = self.ir.add_expr(IrExpr::Block { stmts: out, value: None });
                Some(self.ir.add_expr(IrExpr::While { cond: c, body: b }))
            }
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
                let Expr::Block { stmts, trailing: None } = self.afile.expr(body).clone() else { self.scope.truncate(depth); return None };
                let mut out = Vec::new();
                for s in stmts {
                    out.push(self.stmt(s)?);
                }
                let step = match range.step { Some(e) => self.expr(e)?, None => self.ir.add_expr(IrExpr::Const(IrConst::Int(1))) };
                let inc_op = if matches!(range.kind, RangeKind::DownTo) { IrBinOp::Sub } else { IrBinOp::Add };
                let gi2 = self.ir.add_expr(IrExpr::GetValue(i_v));
                let inc_val = self.ir.add_expr(IrExpr::PrimitiveBinOp { op: inc_op, lhs: gi2, rhs: step });
                out.push(self.ir.add_expr(IrExpr::SetValue { var: i_v, value: inc_val }));
                let wbody = self.ir.add_expr(IrExpr::Block { stmts: out, value: None });
                let wh = self.ir.add_expr(IrExpr::While { cond, body: wbody });
                self.scope.truncate(depth);
                Some(self.ir.add_expr(IrExpr::Block { stmts: vec![var_i, var_end, wh], value: None }))
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
            Expr::Name(n) => {
                if let Some((v, _)) = self.lookup(&n) {
                    self.ir.add_expr(IrExpr::GetValue(v))
                } else {
                    // Unqualified field of the enclosing class (`this.<field>`).
                    let (this_v, _) = self.lookup("this")?;
                    let ci = self.cur_class.as_ref().and_then(|c| self.classes.get(c))?;
                    let idx = ci.fields.iter().position(|(fn_, _)| *fn_ == n)? as u32;
                    let class = ci.id;
                    let recv = self.ir.add_expr(IrExpr::GetValue(this_v));
                    self.ir.add_expr(IrExpr::GetField { receiver: recv, class, index: idx })
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
                let rt = self.info.ty(receiver);
                if rt.array_elem().is_some() && name == "size" {
                    let a = self.expr(receiver)?;
                    self.ir.add_expr(IrExpr::Call { callee: Callee::External("kotlin/Array.size".to_string()), dispatch_receiver: Some(a), args: vec![] })
                } else if let Some(ci) = self.class_of(rt) {
                    let idx = ci.fields.iter().position(|(fn_, _)| *fn_ == name)? as u32;
                    let class = ci.id;
                    let recv = self.expr(receiver)?;
                    self.ir.add_expr(IrExpr::GetField { receiver: recv, class, index: idx })
                } else if rt == Ty::String && name == "length" {
                    // `s.length` → stdlib intrinsic (0-arg), `Int`.
                    let recv = self.expr(receiver)?;
                    self.ir.add_expr(IrExpr::Call { callee: Callee::External("kotlin/String.length".to_string()), dispatch_receiver: Some(recv), args: vec![] })
                } else {
                    return None;
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                if op == BinOp::Add && (self.info.ty(lhs) == Ty::String || self.info.ty(rhs) == Ty::String) {
                    let l = self.expr(lhs)?;
                    let r = self.expr(rhs)?;
                    self.ir.add_expr(IrExpr::Call { callee: Callee::External("kotlin/String.plus".to_string()), dispatch_receiver: Some(l), args: vec![r] })
                } else {
                    let irop = bin_to_ir(op)?;
                    let l = self.expr(lhs)?;
                    let r = self.expr(rhs)?;
                    self.ir.add_expr(IrExpr::PrimitiveBinOp { op: irop, lhs: l, rhs: r })
                }
            }
            Expr::If { cond, then_branch, else_branch } => {
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
            Expr::As { operand, ty, nullable: _ } => {
                let arg = self.expr(operand)?;
                let type_operand = ty_to_ir(self.ty_ref(&ty)?);
                self.ir.add_expr(IrExpr::TypeOp { op: IrTypeOp::Cast, arg, type_operand })
            }
            Expr::Unary { op, operand } => {
                use crate::ast::UnOp;
                let v = self.expr(operand)?;
                match op {
                    // `-x` → `0 - x` (zero typed to match); `!x` → `x == false`.
                    UnOp::Neg => {
                        let zero = match self.info.ty(operand) {
                            Ty::Long => self.ir.add_expr(IrExpr::Const(IrConst::Long(0))),
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
                let mut branches = Vec::new();
                for arm in &arms {
                    let body = self.expr(arm.body)?;
                    if arm.conditions.is_empty() {
                        branches.push((None, body)); // else
                    } else {
                        let mut cond: Option<u32> = None;
                        for &c in &arm.conditions {
                            let test = match subject {
                                Some(subj) => {
                                    let s = self.expr(subj)?;
                                    let cv = self.expr(c)?;
                                    self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::Eq, lhs: s, rhs: cv })
                                }
                                None => self.expr(c)?,
                            };
                            cond = Some(match cond {
                                Some(prev) => self.ir.add_expr(IrExpr::PrimitiveBinOp { op: IrBinOp::Or, lhs: prev, rhs: test }),
                                None => test,
                            });
                        }
                        branches.push((cond, body));
                    }
                }
                self.ir.add_expr(IrExpr::When { branches })
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
                    // Primitive-array size constructor `IntArray(n)` → a per-element intrinsic that
                    // encodes the element type (so the backend picks the right allocation).
                    if prim_array_elem(&fname).is_some() && args.len() == 1 {
                        let size = self.expr(args[0])?;
                        return Some(self.ir.add_expr(IrExpr::Call { callee: Callee::External(format!("kotlin/{fname}.<init>")), dispatch_receiver: None, args: vec![size] }));
                    }
                    if let Some(&fid) = self.fun_ids.get(&fname) {
                        let params = self.ir.functions[fid as usize].params.clone();
                        let mut a = Vec::new();
                        for (k, arg) in args.iter().enumerate() {
                            let target = params.get(k).cloned().unwrap_or(IrType::Error);
                            a.push(self.lower_arg(*arg, &target)?);
                        }
                        self.ir.add_expr(IrExpr::Call { callee: Callee::Local(fid), dispatch_receiver: None, args: a })
                    } else {
                        // Constructor: the call's result type is the class.
                        let ci = self.class_of(self.info.ty(e))?;
                        let class = ci.id;
                        let mut a = Vec::new();
                        for arg in args { a.push(self.expr(arg)?); }
                        self.ir.add_expr(IrExpr::New { class, args: a })
                    }
                }
                // Instance method call `recv.m(args)`, or a stdlib intrinsic method.
                Expr::Member { receiver, name } => {
                    let rt = self.info.ty(receiver);
                    if let Some((index, _, _)) = self.class_of(rt).and_then(|ci| ci.methods.get(&name).copied()) {
                        let class = self.class_of(rt)?.id;
                        let recv = self.expr(receiver)?;
                        let mut a = Vec::new();
                        for arg in args { a.push(self.expr(arg)?); }
                        self.ir.add_expr(IrExpr::MethodCall { class, index, receiver: recv, args: a })
                    } else if name == "toString" && args.is_empty() {
                        // `x.toString()` → stdlib intrinsic, `String`.
                        let recv = self.expr(receiver)?;
                        self.ir.add_expr(IrExpr::Call { callee: Callee::External("kotlin/Any.toString".to_string()), dispatch_receiver: Some(recv), args: vec![] })
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

fn class_internal(file: &ast::File, name: &str) -> String {
    let mangled = name.replace('.', "$");
    match &file.package {
        Some(p) if !p.is_empty() => format!("{}/{}", p.replace('.', "/"), mangled),
        _ => mangled,
    }
}

fn ty_of(_syms: &SymbolTable, r: &ast::TypeRef) -> Ty {
    Ty::from_name(&r.name).unwrap_or(Ty::Error)
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
