//! Phase 4: lower a typechecked file to a `FileKt`-style class.
//!
//! v0 slice implemented here: numeric arithmetic, unary, free-function calls, `toString()` +
//! string concat (via `StringBuilder`, the JVM-8 strategy), `println`, locals, and `return` in
//! expression bodies. Branches (`if`/`while`/comparisons/`&&`/`||`) require `StackMapTable` and are
//! emitted in Phase 4c.

use std::collections::HashMap;

use crate::ast::*;
use crate::codegen::classfile::*;
use crate::diag::DiagSink;
use crate::resolve::{SymbolTable, TypeInfo};
use crate::types::Ty;

/// Class name kotlinc derives for top-level decls: `<File>Kt` (capitalized). For a file `foo.kt`
/// the class is `FooKt`. With a package, the internal name is `pkg/path/FooKt`.
pub fn file_class_name(file_stem: &str, package: Option<&str>) -> String {
    let mut base = String::new();
    let mut chars = file_stem.chars();
    if let Some(c) = chars.next() {
        base.extend(c.to_uppercase());
    }
    base.push_str(chars.as_str());
    base.push_str("Kt");
    match package {
        Some(p) if !p.is_empty() => format!("{}/{}", p.replace('.', "/"), base),
        _ => base,
    }
}

pub fn method_descriptor(params: &[Ty], ret: Ty) -> String {
    let mut s = String::from("(");
    for p in params {
        s.push_str(p.descriptor());
    }
    s.push(')');
    s.push_str(ret.descriptor());
    s
}

/// Lower `file` into class bytes. `internal_name` is e.g. `demo/FooKt`.
pub fn emit_file(
    file: &File,
    info: &TypeInfo,
    syms: &SymbolTable,
    internal_name: &str,
    diags: &mut DiagSink,
) -> Vec<u8> {
    let mut cw = ClassWriter::new(internal_name, "java/lang/Object");
    for &d in &file.decls {
        let Decl::Fun(f) = file.decl(d);
        let mut e = MethodEmitter::new(file, info, syms, internal_name, diags);
        e.emit_fun(f, &mut cw);
    }
    cw.finish()
}

struct MethodEmitter<'a> {
    file: &'a File,
    info: &'a TypeInfo,
    syms: &'a SymbolTable,
    class: String,
    diags: &'a mut DiagSink,
    slots: HashMap<String, (u16, Ty)>,
    next_slot: u16,
}

impl<'a> MethodEmitter<'a> {
    fn new(file: &'a File, info: &'a TypeInfo, syms: &'a SymbolTable, class: &str, diags: &'a mut DiagSink) -> Self {
        MethodEmitter { file, info, syms, class: class.to_string(), diags, slots: HashMap::new(), next_slot: 0 }
    }

    fn alloc_slot(&mut self, name: &str, ty: Ty) -> u16 {
        let slot = self.next_slot;
        self.next_slot += slot_words(ty);
        self.slots.insert(name.to_string(), (slot, ty));
        slot
    }

    fn emit_fun(&mut self, f: &FunDecl, cw: &mut ClassWriter) {
        let sig = match self.syms.funs.get(&f.name) {
            Some(s) => s.clone(),
            None => return,
        };
        for (p, ty) in f.params.iter().zip(&sig.params) {
            self.alloc_slot(&p.name, *ty);
        }
        let mut code = CodeBuilder::new(self.next_slot);
        match &f.body {
            FunBody::Expr(e) => {
                self.emit_expr_as(*e, sig.ret, &mut code, cw);
                self.emit_return(sig.ret, &mut code);
            }
            FunBody::Block(_) => {
                // Phase 4c: block bodies / branches. Emit a stub `return` so the class still
                // verifies; real lowering lands with StackMapTable support.
                self.diags.error(f.span, format!("krust v0: block-body functions not yet emitted ('{}')", f.name));
                self.emit_default_return(sig.ret, &mut code, cw);
            }
            FunBody::None => self.emit_default_return(sig.ret, &mut code, cw),
        }
        cw.add_method(ACC_PUBLIC | ACC_STATIC | ACC_FINAL, &f.name, &method_descriptor(&sig.params, sig.ret), &code);
    }

    fn emit_return(&mut self, ret: Ty, code: &mut CodeBuilder) {
        match ret {
            Ty::Int | Ty::Boolean => code.ireturn(),
            Ty::Long => code.lreturn(),
            Ty::Double => code.dreturn(),
            Ty::String => code.areturn(),
            Ty::Unit => code.ret_void(),
            Ty::Error => code.ret_void(),
        }
    }

    fn emit_default_return(&mut self, ret: Ty, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        match ret {
            Ty::Int | Ty::Boolean => { code.push_int(0, cw); code.ireturn(); }
            Ty::Long => { code.push_long(0, cw); code.lreturn(); }
            Ty::Double => { code.push_double(0.0, cw); code.dreturn(); }
            Ty::String => { code.push_string("", cw); code.areturn(); }
            _ => code.ret_void(),
        }
    }

    /// Emit `e`, then widen its value to `target` if numeric.
    fn emit_expr_as(&mut self, e: ExprId, target: Ty, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let from = self.info.ty(e);
        self.emit_expr(e, code, cw);
        if from.is_numeric() && target.is_numeric() {
            code.widen(from, target);
        }
    }

    fn emit_expr(&mut self, e: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        match self.file.expr(e).clone() {
            Expr::IntLit(v) => code.push_int(v as i32, cw),
            Expr::LongLit(v) => code.push_long(v, cw),
            Expr::DoubleLit(v) => code.push_double(v, cw),
            Expr::BoolLit(b) => code.push_int(if b { 1 } else { 0 }, cw),
            Expr::StringLit(s) => code.push_string(&s, cw),
            Expr::Name(n) => {
                if let Some(&(slot, ty)) = self.slots.get(&n) {
                    match ty {
                        Ty::Int | Ty::Boolean => code.iload(slot),
                        Ty::Long => code.lload(slot),
                        Ty::Double => code.dload(slot),
                        Ty::String => code.aload(slot),
                        _ => code.aload(slot),
                    }
                } else {
                    self.diags.error(self.file.expr_spans[e.0 as usize], format!("krust: unbound local '{n}' in codegen"));
                }
            }
            Expr::Unary { op, operand } => {
                let t = self.info.ty(e);
                self.emit_expr(operand, code, cw);
                match op {
                    UnOp::Neg => match t {
                        Ty::Int => code.ineg(),
                        Ty::Long => code.lneg(),
                        Ty::Double => code.dneg(),
                        _ => {}
                    },
                    UnOp::Not => {
                        code.push_int(1, cw);
                        code.ixor();
                    }
                }
            }
            Expr::Binary { op, lhs, rhs } => self.emit_binary(e, op, lhs, rhs, code, cw),
            Expr::Call { callee, args } => self.emit_call(e, callee, &args, code, cw),
            Expr::Member { receiver, name } => {
                if name == "length" {
                    self.emit_expr(receiver, code, cw);
                    let m = cw.methodref("java/lang/String", "length", "()I");
                    code.invokevirtual(m, 0, 1);
                } else {
                    self.diags.error(self.file.expr_spans[e.0 as usize], format!("krust v0: member '{name}' not emittable"));
                }
            }
            Expr::If { .. } | Expr::Block { .. } => {
                self.diags.error(self.file.expr_spans[e.0 as usize], "krust v0: if/block expressions need branch support (Phase 4c)");
            }
        }
    }

    fn emit_binary(&mut self, e: ExprId, op: BinOp, lhs: ExprId, rhs: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let result = self.info.ty(e);
        match op {
            BinOp::Add if result == Ty::String => self.emit_concat(lhs, rhs, code, cw),
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                self.emit_expr_as(lhs, result, code, cw);
                self.emit_expr_as(rhs, result, code, cw);
                self.emit_arith(op, result, code);
            }
            _ => {
                self.diags.error(self.file.expr_spans[e.0 as usize], "krust v0: comparison/logic needs branch support (Phase 4c)");
            }
        }
    }

    fn emit_arith(&mut self, op: BinOp, t: Ty, code: &mut CodeBuilder) {
        match (op, t) {
            (BinOp::Add, Ty::Int) => code.iadd(),
            (BinOp::Sub, Ty::Int) => code.isub(),
            (BinOp::Mul, Ty::Int) => code.imul(),
            (BinOp::Div, Ty::Int) => code.idiv(),
            (BinOp::Rem, Ty::Int) => code.irem(),
            (BinOp::Add, Ty::Long) => code.ladd(),
            (BinOp::Sub, Ty::Long) => code.lsub(),
            (BinOp::Mul, Ty::Long) => code.lmul(),
            (BinOp::Div, Ty::Long) => code.ldiv(),
            (BinOp::Rem, Ty::Long) => code.lrem(),
            (BinOp::Add, Ty::Double) => code.dadd(),
            (BinOp::Sub, Ty::Double) => code.dsub(),
            (BinOp::Mul, Ty::Double) => code.dmul(),
            (BinOp::Div, Ty::Double) => code.ddiv(),
            (BinOp::Rem, Ty::Double) => code.drem(),
            _ => {}
        }
    }

    /// `a + b` where the result is String: `new StringBuilder().append(a).append(b).toString()`.
    fn emit_concat(&mut self, lhs: ExprId, rhs: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let sb = cw.class_ref("java/lang/StringBuilder");
        let ctor = cw.methodref("java/lang/StringBuilder", "<init>", "()V");
        code.new_obj(sb);
        code.dup();
        code.invokespecial(ctor, 0, 0);
        self.emit_append(lhs, code, cw);
        self.emit_append(rhs, code, cw);
        let to_s = cw.methodref("java/lang/StringBuilder", "toString", "()Ljava/lang/String;");
        code.invokevirtual(to_s, 0, 1);
    }

    fn emit_append(&mut self, e: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let t = self.info.ty(e);
        self.emit_expr(e, code, cw);
        // StringBuilder.append has overloads per primitive + String/Object.
        let (desc, words) = match t {
            Ty::Int | Ty::Boolean => ("(I)Ljava/lang/StringBuilder;", 1),
            Ty::Long => ("(J)Ljava/lang/StringBuilder;", 2),
            Ty::Double => ("(D)Ljava/lang/StringBuilder;", 2),
            Ty::String => ("(Ljava/lang/String;)Ljava/lang/StringBuilder;", 1),
            _ => ("(Ljava/lang/Object;)Ljava/lang/StringBuilder;", 1),
        };
        let append = cw.methodref("java/lang/StringBuilder", "append", desc);
        code.invokevirtual(append, words, 1);
    }

    fn emit_call(&mut self, e: ExprId, callee: ExprId, args: &[ExprId], code: &mut CodeBuilder, cw: &mut ClassWriter) {
        match self.file.expr(callee).clone() {
            Expr::Member { receiver, name } if name == "toString" && args.is_empty() => {
                // primitive .toString() -> String.valueOf(x); String.toString() -> identity.
                let rt = self.info.ty(receiver);
                self.emit_expr(receiver, code, cw);
                match rt {
                    Ty::String => {} // already a String
                    Ty::Int | Ty::Boolean => {
                        let m = cw.methodref("java/lang/String", "valueOf", "(I)Ljava/lang/String;");
                        code.invokestatic(m, 1, 1);
                    }
                    Ty::Long => {
                        let m = cw.methodref("java/lang/String", "valueOf", "(J)Ljava/lang/String;");
                        code.invokestatic(m, 2, 1);
                    }
                    Ty::Double => {
                        let m = cw.methodref("java/lang/String", "valueOf", "(D)Ljava/lang/String;");
                        code.invokestatic(m, 2, 1);
                    }
                    _ => {}
                }
            }
            Expr::Name(fname) if fname == "println" => {
                let out = cw.fieldref("java/lang/System", "out", "Ljava/io/PrintStream;");
                code.getstatic(out, 1);
                let at = args.first().map(|a| self.info.ty(*a)).unwrap_or(Ty::Unit);
                if let Some(a) = args.first() {
                    self.emit_expr(*a, code, cw);
                }
                let (desc, words) = match at {
                    Ty::Int | Ty::Boolean => ("(I)V", 1),
                    Ty::Long => ("(J)V", 2),
                    Ty::Double => ("(D)V", 2),
                    Ty::String => ("(Ljava/lang/String;)V", 1),
                    _ => ("()V", 0),
                };
                let m = cw.methodref("java/io/PrintStream", "println", desc);
                code.invokevirtual(m, words, 0);
            }
            Expr::Name(fname) => {
                let sig = match self.syms.funs.get(&fname) {
                    Some(s) => s.clone(),
                    None => return,
                };
                for (a, pty) in args.iter().zip(&sig.params) {
                    self.emit_expr_as(*a, *pty, code, cw);
                }
                let arg_words: i32 = sig.params.iter().map(|t| slot_words(*t) as i32).sum();
                let ret_words = slot_words(sig.ret) as i32;
                let m = cw.methodref(&self.class.clone(), &fname, &method_descriptor(&sig.params, sig.ret));
                code.invokestatic(m, arg_words, ret_words);
            }
            _ => {
                self.diags.error(self.file.expr_spans[e.0 as usize], "krust v0: unsupported call form");
            }
        }
    }
}

/// JVM stack/local words for a type: long/double are 2, everything else 1; Unit is 0 (no value).
fn slot_words(t: Ty) -> u16 {
    match t {
        Ty::Long | Ty::Double => 2,
        Ty::Unit => 0,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_name_capitalization() {
        assert_eq!(file_class_name("foo", None), "FooKt");
        assert_eq!(file_class_name("file_1", None), "File_1Kt");
        assert_eq!(file_class_name("foo", Some("a.b")), "a/b/FooKt");
    }

    #[test]
    fn descriptors() {
        assert_eq!(method_descriptor(&[Ty::Int, Ty::Int], Ty::Int), "(II)I");
        assert_eq!(method_descriptor(&[Ty::Int, Ty::String], Ty::String), "(ILjava/lang/String;)Ljava/lang/String;");
        assert_eq!(method_descriptor(&[Ty::Double, Ty::Long], Ty::Double), "(DJ)D");
    }
}
