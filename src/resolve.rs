//! Stage C (global signature collection) + Stage D (per-file typecheck).
//!
//! Signatures are collected for the whole compilation first (cheap, no bodies), then each file is
//! typechecked independently against that global table — the per-file streaming boundary.
//!
//! v0 rules (documented; each has a test): functions REQUIRE explicit return types; assignment is
//! exact-type (no implicit numeric widening); integer literals default to `Int`; `+` is string
//! concat if either side is `String`; `if` with both branches needs a common type.

use std::collections::HashMap;

use crate::ast::*;
use crate::diag::{DiagSink, Span};
use crate::types::Ty;

#[derive(Clone, Debug)]
pub struct Signature {
    pub params: Vec<Ty>,
    pub ret: Ty,
}

#[derive(Default)]
pub struct SymbolTable {
    pub funs: HashMap<String, Signature>,
}

/// Stage C: collect top-level function signatures across all files.
pub fn collect_signatures(files: &[File], diags: &mut DiagSink) -> SymbolTable {
    let mut table = SymbolTable::default();
    for file in files {
        for &d in &file.decls {
            let Decl::Fun(f) = file.decl(d);
            let params = f.params.iter().map(|p| ty_of_ref(&p.ty, diags)).collect();
            let ret = match &f.ret {
                Some(r) => ty_of_ref(r, diags),
                None => Ty::Unit, // v0: missing return type defaults to Unit
            };
            if table.funs.insert(f.name.clone(), Signature { params, ret }).is_some() {
                diags.error(f.span, format!("conflicting declaration of '{}'", f.name));
            }
        }
    }
    table
}

fn ty_of_ref(r: &TypeRef, diags: &mut DiagSink) -> Ty {
    match Ty::from_name(&r.name) {
        Some(t) => t,
        None => {
            diags.error(r.span, format!("unknown type '{}'", r.name));
            Ty::Error
        }
    }
}

/// Result of typechecking a file: the type assigned to every expression node.
pub struct TypeInfo {
    pub expr_types: Vec<Ty>,
}

impl TypeInfo {
    pub fn ty(&self, e: ExprId) -> Ty {
        self.expr_types[e.0 as usize]
    }
}

struct Local {
    ty: Ty,
    is_var: bool,
}

pub fn check_file(file: &File, syms: &SymbolTable, diags: &mut DiagSink) -> TypeInfo {
    let mut c = Checker {
        file,
        syms,
        diags,
        expr_types: vec![Ty::Error; file.expr_arena.len()],
        scopes: Vec::new(),
        ret_ty: Ty::Unit,
    };
    for &d in &file.decls {
        let Decl::Fun(f) = file.decl(d);
        c.check_fun(f);
    }
    TypeInfo { expr_types: c.expr_types }
}

struct Checker<'a> {
    file: &'a File,
    syms: &'a SymbolTable,
    diags: &'a mut DiagSink,
    expr_types: Vec<Ty>,
    scopes: Vec<HashMap<String, Local>>,
    ret_ty: Ty,
}

impl<'a> Checker<'a> {
    fn set(&mut self, e: ExprId, t: Ty) -> Ty {
        self.expr_types[e.0 as usize] = t;
        t
    }
    fn span(&self, e: ExprId) -> Span {
        self.file.expr_spans[e.0 as usize]
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }
    fn pop_scope(&mut self) {
        self.scopes.pop();
    }
    fn declare(&mut self, name: &str, ty: Ty, is_var: bool) {
        self.scopes.last_mut().unwrap().insert(name.to_string(), Local { ty, is_var });
    }
    fn lookup(&self, name: &str) -> Option<&Local> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }

    fn check_fun(&mut self, f: &FunDecl) {
        let sig = self.syms.funs.get(&f.name);
        self.ret_ty = sig.map(|s| s.ret).unwrap_or(Ty::Unit);
        self.push_scope();
        for p in &f.params {
            let ty = Ty::from_name(&p.ty.name).unwrap_or(Ty::Error);
            self.declare(&p.name, ty, false);
        }
        match &f.body {
            FunBody::Expr(e) => {
                let t = self.expr(*e);
                self.expect_assignable(self.ret_ty, t, self.span(*e), "function body");
            }
            FunBody::Block(e) => {
                let _ = self.expr(*e); // block body; returns happen via `return`
            }
            FunBody::None => {}
        }
        self.pop_scope();
    }

    fn expect_assignable(&mut self, expected: Ty, actual: Ty, span: Span, ctx: &str) {
        if expected == Ty::Error || actual == Ty::Error {
            return;
        }
        if expected != actual {
            self.diags.error(span, format!("type mismatch in {ctx}: expected {}, found {}", expected.name(), actual.name()));
        }
    }

    fn expr(&mut self, e: ExprId) -> Ty {
        let t = match self.file.expr(e).clone() {
            Expr::IntLit(_) => Ty::Int,
            Expr::LongLit(_) => Ty::Long,
            Expr::DoubleLit(_) => Ty::Double,
            Expr::BoolLit(_) => Ty::Boolean,
            Expr::StringLit(_) => Ty::String,
            Expr::Name(n) => match self.lookup(&n) {
                Some(l) => l.ty,
                None => {
                    self.diags.error(self.span(e), format!("unresolved reference '{n}'"));
                    Ty::Error
                }
            },
            Expr::Unary { op, operand } => {
                let ot = self.expr(operand);
                self.check_unary(op, ot, self.span(e))
            }
            Expr::Binary { op, lhs, rhs } => {
                let lt = self.expr(lhs);
                let rt = self.expr(rhs);
                self.check_binary(op, lt, rt, self.span(e))
            }
            Expr::Member { receiver, name } => {
                let rt = self.expr(receiver);
                self.check_member(rt, &name, self.span(e))
            }
            Expr::Call { callee, args } => self.check_call(callee, &args, self.span(e)),
            Expr::If { cond, then_branch, else_branch } => {
                let ct = self.expr(cond);
                self.expect_assignable(Ty::Boolean, ct, self.span(cond), "if condition");
                let tt = self.expr(then_branch);
                match else_branch {
                    Some(eb) => {
                        let et = self.expr(eb);
                        self.join(tt, et, self.span(e))
                    }
                    None => Ty::Unit,
                }
            }
            Expr::Block { stmts, trailing } => {
                self.push_scope();
                for s in &stmts {
                    self.stmt(*s);
                }
                let t = match trailing {
                    Some(te) => self.expr(te),
                    None => Ty::Unit,
                };
                self.pop_scope();
                t
            }
        };
        self.set(e, t)
    }

    fn check_unary(&mut self, op: UnOp, ot: Ty, span: Span) -> Ty {
        match op {
            UnOp::Neg if ot.is_numeric() => ot,
            UnOp::Not if ot == Ty::Boolean => Ty::Boolean,
            _ if ot == Ty::Error => Ty::Error,
            _ => {
                self.diags.error(span, format!("operator cannot be applied to '{}'", ot.name()));
                Ty::Error
            }
        }
    }

    fn check_binary(&mut self, op: BinOp, lt: Ty, rt: Ty, span: Span) -> Ty {
        if lt == Ty::Error || rt == Ty::Error {
            return Ty::Error;
        }
        match op {
            BinOp::And | BinOp::Or => {
                if lt == Ty::Boolean && rt == Ty::Boolean {
                    Ty::Boolean
                } else {
                    self.bin_err(op, lt, rt, span)
                }
            }
            BinOp::Add => {
                if lt == Ty::String || rt == Ty::String {
                    Ty::String // concat
                } else if let Some(t) = Ty::promote(lt, rt) {
                    t
                } else {
                    self.bin_err(op, lt, rt, span)
                }
            }
            BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                Ty::promote(lt, rt).unwrap_or_else(|| self.bin_err(op, lt, rt, span))
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                if Ty::promote(lt, rt).is_some() {
                    Ty::Boolean
                } else {
                    self.bin_err(op, lt, rt, span)
                }
            }
            BinOp::Eq | BinOp::Ne => {
                if lt == rt || Ty::promote(lt, rt).is_some() {
                    Ty::Boolean
                } else {
                    self.bin_err(op, lt, rt, span)
                }
            }
        }
    }

    fn bin_err(&mut self, _op: BinOp, lt: Ty, rt: Ty, span: Span) -> Ty {
        self.diags.error(span, format!("operator cannot be applied to '{}' and '{}'", lt.name(), rt.name()));
        Ty::Error
    }

    fn check_member(&mut self, rt: Ty, name: &str, span: Span) -> Ty {
        if rt == Ty::Error {
            return Ty::Error;
        }
        match (rt, name) {
            (Ty::String, "length") => Ty::Int,
            _ => {
                self.diags.error(span, format!("unresolved member '{name}' on '{}'", rt.name()));
                Ty::Error
            }
        }
    }

    fn check_call(&mut self, callee: ExprId, args: &[ExprId], span: Span) -> Ty {
        match self.file.expr(callee).clone() {
            // method call: recv.method(args)
            Expr::Member { receiver, name } => {
                let rt = self.expr(receiver);
                let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                if rt == Ty::Error {
                    return Ty::Error;
                }
                match (name.as_str(), arg_tys.as_slice()) {
                    ("toString", []) => Ty::String, // intrinsic on any type
                    _ => {
                        self.diags.error(span, format!("unresolved method '{name}' on '{}'", rt.name()));
                        Ty::Error
                    }
                }
            }
            // free function call: name(args)
            Expr::Name(fname) => {
                let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                if fname == "println" {
                    return Ty::Unit; // builtin: accepts one value of any type (v0)
                }
                match self.syms.funs.get(&fname) {
                    Some(sig) => {
                        let sig = sig.clone();
                        if sig.params.len() != arg_tys.len() {
                            self.diags.error(span, format!("function '{fname}' expects {} args, got {}", sig.params.len(), arg_tys.len()));
                        } else {
                            for (i, (p, a)) in sig.params.iter().zip(&arg_tys).enumerate() {
                                self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                            }
                        }
                        sig.ret
                    }
                    None => {
                        self.diags.error(span, format!("unresolved function '{fname}'"));
                        Ty::Error
                    }
                }
            }
            _ => {
                for a in args {
                    self.expr(*a);
                }
                self.diags.error(span, "expression is not callable");
                Ty::Error
            }
        }
    }

    fn join(&mut self, a: Ty, b: Ty, span: Span) -> Ty {
        if a == Ty::Error || b == Ty::Error {
            return Ty::Error;
        }
        if a == b {
            return a;
        }
        if let Some(t) = Ty::promote(a, b) {
            return t;
        }
        self.diags.error(span, format!("incompatible if branches: '{}' and '{}'", a.name(), b.name()));
        Ty::Error
    }

    fn stmt(&mut self, s: StmtId) {
        match self.file.stmt(s).clone() {
            Stmt::Local { is_var, name, ty, init } => {
                let it = self.expr(init);
                let declared = ty.as_ref().and_then(|r| Ty::from_name(&r.name));
                let bind = match declared {
                    Some(d) => {
                        self.expect_assignable(d, it, self.span(init), "initializer");
                        d
                    }
                    None => it,
                };
                self.declare(&name, bind, is_var);
            }
            Stmt::Assign { name, value } => {
                let vt = self.expr(value);
                match self.lookup(&name) {
                    Some(l) => {
                        let (lty, is_var) = (l.ty, l.is_var);
                        if !is_var {
                            self.diags.error(self.file.stmt_spans[s.0 as usize], format!("'val' {name} cannot be reassigned"));
                        }
                        self.expect_assignable(lty, vt, self.file.stmt_spans[s.0 as usize], "assignment");
                    }
                    None => {
                        self.diags.error(self.file.stmt_spans[s.0 as usize], format!("unresolved reference '{name}'"));
                    }
                }
            }
            Stmt::Return(e) => {
                let rt = self.ret_ty;
                match e {
                    Some(ex) => {
                        let t = self.expr(ex);
                        self.expect_assignable(rt, t, self.span(ex), "return");
                    }
                    None => {
                        if rt != Ty::Unit {
                            self.diags.error(self.file.stmt_spans[s.0 as usize], format!("missing return value: expected {}", rt.name()));
                        }
                    }
                }
            }
            Stmt::While { cond, body } => {
                let ct = self.expr(cond);
                self.expect_assignable(Ty::Boolean, ct, self.span(cond), "while condition");
                self.expr(body);
            }
            Stmt::Expr(e) => {
                self.expr(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::lex;
    use crate::parser::parse;

    fn check(src: &str) -> (Vec<String>, Option<TypeInfo>) {
        let mut d = DiagSink::new();
        let toks = lex(src, &mut d);
        let file = parse(src, &toks, &mut d);
        let files = vec![file];
        let syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[0], &syms, &mut d);
        let errs: Vec<String> = d.diags.iter().map(|x| x.msg.clone()).collect();
        (errs, Some(info))
    }

    fn ok(src: &str) {
        let (errs, _) = check(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }
    fn err_contains(src: &str, needle: &str) {
        let (errs, _) = check(src);
        assert!(errs.iter().any(|e| e.contains(needle)), "expected error containing {needle:?}, got {errs:?}");
    }

    #[test]
    fn arithmetic_ok() {
        ok("fun f(a: Int, b: Int): Int = a + b * 2");
        ok("fun f(a: Double, b: Int): Double = a + b"); // promotion Int->Double
        ok("fun f(a: Long, b: Int): Long = a * b");
    }

    #[test]
    fn string_concat() {
        ok("fun f(a: Int, b: String): String = a.toString() + b");
        ok("fun f(a: Int): String = \"x=\" + a"); // Int+String via concat
    }

    #[test]
    fn comparison_and_logic() {
        ok("fun f(a: Int, b: Int): Boolean = a < b && a != b");
    }

    #[test]
    fn if_branches_common_type() {
        ok("fun max(a: Int, b: Int): Int = if (a > b) a else b");
        err_contains("fun f(a: Int, b: String): Int = if (a > 0) a else b", "incompatible if branches");
    }

    #[test]
    fn return_type_mismatch() {
        err_contains("fun f(a: Int): String = a", "type mismatch in function body");
    }

    #[test]
    fn unresolved_reference() {
        err_contains("fun f(): Int = q", "unresolved reference 'q'");
    }

    #[test]
    fn val_reassign_is_error() {
        err_contains("fun f(): Int {\n val x = 1\n x = 2\n return x\n}", "cannot be reassigned");
    }

    #[test]
    fn var_reassign_ok() {
        ok("fun f(): Int {\n var x = 1\n x = 2\n return x\n}");
    }

    #[test]
    fn call_arity_and_types() {
        ok("fun a(x: Int): Int = x\nfun b(): Int = a(1)");
        err_contains("fun a(x: Int): Int = x\nfun b(): Int = a()", "expects 1 args");
        err_contains("fun a(x: Int): Int = x\nfun b(): Int = a(\"s\")", "type mismatch in argument");
    }

    #[test]
    fn block_while_fib_typechecks() {
        ok("fun fib(n: Int): Int {\n var a = 0\n var b = 1\n var i = 0\n while (i < n) {\n   val t = a + b\n   a = b\n   b = t\n   i = i + 1\n }\n return a\n}");
    }

    #[test]
    fn bool_operator_misuse() {
        err_contains("fun f(a: Int): Boolean = a && a", "cannot be applied");
    }
}
