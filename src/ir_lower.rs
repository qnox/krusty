//! AST → `krusty-ir` lowering for the core language.
//!
//! Runs after the front end (parse + type-check). Produces a backend-agnostic [`IrFile`] that any
//! backend (JVM, JS) lowers to its target — proving the FE/BE boundary is real. Only the core
//! subset is lowered today (top-level functions: const/param/local, primitive arithmetic &
//! comparison, calls to top-level functions, `if`/`when`, `return`, blocks, local `val`/`var`).
//! Anything outside the subset makes lowering return `None`, so the caller keeps using the direct
//! JVM emitter for those files — the IR path grows one construct at a time, each conformance-checked.

use std::collections::HashMap;

use crate::ast::{self, BinOp, Expr, ExprId as AstExprId, FunBody, Stmt};
use crate::ir::{IrBinOp, IrConst, IrExpr, IrFile, IrFunction, IrType};
use crate::resolve::{SymbolTable, TypeInfo};
use crate::types::Ty;

/// Lower a checked file to IR, or `None` if it uses anything outside the core subset.
pub fn lower_file(file: &ast::File, info: &TypeInfo, syms: &SymbolTable) -> Option<IrFile> {
    let mut lo = Lower {
        afile: file,
        info,
        syms,
        ir: IrFile { package: file.package.clone(), ..Default::default() },
        fun_ids: HashMap::new(),
        scope: Vec::new(),
        next_value: 0,
    };
    // Only files that are *entirely* top-level functions (no classes/properties) take the IR path
    // today — keeps the slice honest and the fallback obvious.
    for &d in &file.decls {
        match file.decl(d) {
            ast::Decl::Fun(f) if f.receiver.is_none() && !f.is_inline => {}
            _ => return None,
        }
    }
    // Pass 1: assign a FunId to each top-level function (so calls can resolve to it).
    for &d in &file.decls {
        if let ast::Decl::Fun(f) = file.decl(d) {
            let sig = syms.funs.get(&f.name)?;
            let params: Vec<IrType> = sig.params.iter().map(|t| ty_to_ir(*t)).collect();
            let ret = ty_to_ir(info.fun_ret_overrides.get(&f.name).copied().unwrap_or(sig.ret));
            let id = lo.ir.add_fun(IrFunction { name: f.name.clone(), params, ret, body: None, is_static: true });
            lo.fun_ids.insert(f.name.clone(), id);
        }
    }
    // Pass 2: lower each body.
    let mut idx = 0u32;
    for &d in &file.decls {
        if let ast::Decl::Fun(f) = file.decl(d) {
            let fid = idx;
            idx += 1;
            lo.scope.clear();
            lo.next_value = 0;
            let sig = syms.funs.get(&f.name)?;
            for (p, t) in f.params.iter().zip(&sig.params) {
                let v = lo.fresh_value();
                lo.scope.push((p.name.clone(), v, *t));
            }
            let ret_ty = lo.ir.functions[fid as usize].ret.clone();
            let body = match &f.body {
                FunBody::Expr(e) => {
                    let ve = lo.expr(*e)?;
                    let ret = lo.ir.add_expr(IrExpr::Return(if ret_ty == IrType::Unit { None } else { Some(ve) }));
                    lo.ir.add_expr(IrExpr::Block { stmts: vec![ret], value: None })
                }
                FunBody::Block(b) => lo.block_as_body(*b, &ret_ty)?,
                FunBody::None => return None,
            };
            lo.ir.functions[fid as usize].body = Some(body);
        }
    }
    Some(lo.ir)
}

struct Lower<'a> {
    afile: &'a ast::File,
    info: &'a TypeInfo,
    syms: &'a SymbolTable,
    ir: IrFile,
    fun_ids: HashMap<String, u32>,
    /// In-scope values: (name, value index, Kotlin type). A stack used as block scopes.
    scope: Vec<(String, u32, Ty)>,
    next_value: u32,
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

    /// Lower a `{ … }` block used as a function body, ensuring it ends in a return.
    fn block_as_body(&mut self, block: AstExprId, ret_ty: &IrType) -> Option<u32> {
        let Expr::Block { stmts, trailing } = self.afile.expr(block) else { return None };
        let depth = self.scope.len();
        let mut out = Vec::new();
        for &s in stmts {
            out.push(self.stmt(s)?);
        }
        if let Some(t) = trailing {
            let ve = self.expr(*t)?;
            out.push(self.ir.add_expr(IrExpr::Return(if *ret_ty == IrType::Unit { None } else { Some(ve) })));
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
                let kty = ty.as_ref().map(|r| self.info_ty_of_ref(r)).unwrap_or_else(|| self.info.ty(init));
                let v = self.fresh_value();
                self.scope.push((name.clone(), v, kty));
                Some(self.ir.add_expr(IrExpr::Variable { index: v, ty: ty_to_ir(kty), init: Some(it) }))
            }
            Stmt::Assign { name, value } => {
                let (v, _) = self.lookup(&name)?;
                let val = self.expr(value)?;
                Some(self.ir.add_expr(IrExpr::SetValue { var: v, value: val }))
            }
            _ => None,
        }
    }

    fn expr(&mut self, e: AstExprId) -> Option<u32> {
        Some(match self.afile.expr(e).clone() {
            Expr::IntLit(v) => self.ir.add_expr(IrExpr::Const(IrConst::Int(v as i32))),
            Expr::LongLit(v) => self.ir.add_expr(IrExpr::Const(IrConst::Long(v))),
            Expr::BoolLit(b) => self.ir.add_expr(IrExpr::Const(IrConst::Boolean(b))),
            Expr::StringLit(s) => self.ir.add_expr(IrExpr::Const(IrConst::String(s))),
            Expr::Name(n) => {
                let (v, _) = self.lookup(&n)?;
                self.ir.add_expr(IrExpr::GetValue(v))
            }
            Expr::Binary { op, lhs, rhs } => {
                let irop = bin_to_ir(op)?;
                let l = self.expr(lhs)?;
                let r = self.expr(rhs)?;
                self.ir.add_expr(IrExpr::PrimitiveBinOp { op: irop, lhs: l, rhs: r })
            }
            Expr::If { cond, then_branch, else_branch } => {
                let c = self.expr(cond)?;
                let t = self.expr(then_branch)?;
                let branches = match else_branch {
                    Some(els) => {
                        let e2 = self.expr(els)?;
                        vec![(Some(c), t), (None, e2)]
                    }
                    None => vec![(Some(c), t)],
                };
                self.ir.add_expr(IrExpr::When { branches })
            }
            Expr::Call { callee, args } => {
                let Expr::Name(fname) = self.afile.expr(callee).clone() else { return None };
                let fid = *self.fun_ids.get(&fname)?;
                let mut a = Vec::new();
                for arg in args {
                    a.push(self.expr(arg)?);
                }
                self.ir.add_expr(IrExpr::Call { callee: fid, dispatch_receiver: None, args: a })
            }
            _ => return None,
        })
    }

    fn info_ty_of_ref(&self, r: &ast::TypeRef) -> Ty {
        Ty::from_name(&r.name).unwrap_or(Ty::Error)
    }
}

/// Map a krusty `Ty` to a backend-agnostic `IrType` (a Kotlin FqName). JVM descriptors are *not*
/// produced here — each backend maps the FqName itself.
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
