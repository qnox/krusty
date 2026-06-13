//! Phase 4: lower a typechecked file to a `FileKt`-style class.
//!
//! v0 covers: numeric arithmetic + widening, unary, comparisons, `&&`/`||` (short-circuit),
//! `if`/`while`, block bodies with `val`/`var` locals and `return`, free-function calls,
//! `toString()`, string concat (`StringBuilder`), `println`, `.length`. Branchy methods rely on the
//! v50 type-inference verifier (no StackMapTable yet — see classfile.rs).

use std::collections::HashMap;

use crate::ast::*;
use crate::codegen::classfile::*;
use crate::diag::DiagSink;
use crate::resolve::{import_map, resolve_java_static, SymbolTable, TypeInfo};
use crate::types::Ty;

/// Class name kotlinc derives for top-level decls: `<File>Kt` (capitalized).
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
        s.push_str(&p.descriptor());
    }
    s.push(')');
    s.push_str(&ret.descriptor());
    s
}

pub fn emit_file(
    file: &File,
    info: &TypeInfo,
    syms: &SymbolTable,
    internal_name: &str,
    diags: &mut DiagSink,
) -> Vec<u8> {
    let mut cw = ClassWriter::new(internal_name, "java/lang/Object");
    let imports = import_map(file);
    for &d in &file.decls {
        if let Decl::Fun(f) = file.decl(d) {
            let mut e = MethodEmitter::new(file, info, syms, internal_name, &imports, diags);
            e.emit_fun(f, &mut cw);
        }
    }
    // @kotlin.Metadata: describe the file facade so Kotlin consumers see the Kotlin API.
    let funcs: Vec<crate::metadata::builder::FnMeta> = file
        .decls
        .iter()
        .filter_map(|&d| {
            let Decl::Fun(f) = file.decl(d) else { return None };
            let sig = syms.funs.get(&f.name)?;
            Some(crate::metadata::builder::FnMeta {
                name: f.name.clone(),
                params: f.params.iter().zip(&sig.params).map(|(p, t)| (p.name.clone(), *t)).collect(),
                ret: sig.ret,
            })
        })
        .collect();
    let (d1_bytes, d2) = crate::metadata::builder::build_package(&funcs);
    let d1 = crate::metadata::encoding::bytes_to_strings(&d1_bytes);
    cw.set_kotlin_metadata(2, &[1, 9, 0], 48, &d1, &d2);
    cw.finish()
}

/// Resolve a syntactic type to a `Ty`, including declared class types (→ `Ty::Obj`).
fn resolve_ty(r: &TypeRef, syms: &SymbolTable) -> Ty {
    Ty::from_name(&r.name)
        .or_else(|| syms.classes.get(&r.name).map(|c| Ty::obj(&c.internal)))
        .unwrap_or(Ty::Error)
}

/// Capitalize the first character (`x` -> `X`) for Kotlin's `getX`/`setX` accessor naming.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Lower a `class C(val a: T, var b: U)` to a JVM class with private backing fields, a primary
/// constructor that calls `super()` and stores each field, and `getX`/`setX` accessors — matching
/// kotlinc's public ABI for a simple property-holding class.
pub fn emit_class(
    class: &ClassDecl,
    file: &File,
    info: &TypeInfo,
    internal_name: &str,
    syms: &SymbolTable,
    diags: &mut DiagSink,
) -> Vec<u8> {
    let mut cw = ClassWriter::new(internal_name, "java/lang/Object");

    // Resolve property types (primitives/String or declared class reference types).
    let props: Vec<(&PropParam, Ty)> = class
        .props
        .iter()
        .map(|p| (p, resolve_ty(&p.ty, syms)))
        .collect();

    // Backing fields: `private final` for `val`, `private` for `var`.
    for (p, ty) in &props {
        let access = if p.is_var { ACC_PRIVATE } else { ACC_PRIVATE | ACC_FINAL };
        cw.add_field(access, &p.name, &ty.descriptor());
    }

    // Primary constructor: super() then store each parameter into its backing field.
    let ctor_desc = method_descriptor(&props.iter().map(|(_, t)| *t).collect::<Vec<_>>(), Ty::Unit);
    let total_locals: u16 = 1 + props.iter().map(|(_, t)| slot_words(*t)).sum::<u16>();
    let mut code = CodeBuilder::new(total_locals);
    code.aload(0);
    let obj_init = cw.methodref("java/lang/Object", "<init>", "()V");
    code.invokespecial(obj_init, 0, 0);
    let mut slot = 1u16;
    for (p, ty) in &props {
        code.aload(0);
        load_local(*ty, slot, &mut code);
        let f = cw.fieldref(internal_name, &p.name, &ty.descriptor());
        code.putfield(f, slot_words(*ty) as i32);
        slot += slot_words(*ty);
    }
    code.ret_void();
    code.link();
    cw.add_method(ACC_PUBLIC, "<init>", &ctor_desc, &code);

    // Accessors: `public final getX()` for every property; `setX(..)` for `var`.
    for (p, ty) in &props {
        let cap = capitalize(&p.name);
        // getter
        let mut g = CodeBuilder::new(1);
        g.aload(0);
        let f = cw.fieldref(internal_name, &p.name, &ty.descriptor());
        g.getfield(f, slot_words(*ty) as i32);
        emit_typed_return(*ty, &mut g);
        g.link();
        cw.add_method(ACC_PUBLIC | ACC_FINAL, &format!("get{cap}"), &method_descriptor(&[], *ty), &g);
        // setter (var only)
        if p.is_var {
            let mut s = CodeBuilder::new(1 + slot_words(*ty));
            s.aload(0);
            load_local(*ty, 1, &mut s);
            let f = cw.fieldref(internal_name, &p.name, &ty.descriptor());
            s.putfield(f, slot_words(*ty) as i32);
            s.ret_void();
            s.link();
            cw.add_method(ACC_PUBLIC | ACC_FINAL, &format!("set{cap}"), &method_descriptor(&[*ty], Ty::Unit), &s);
        }
    }

    // Member functions → instance methods. Property names resolve to backing-field access.
    let class_props: HashMap<String, Ty> = props.iter().map(|(p, t)| (p.name.clone(), *t)).collect();
    let imports = import_map(file);
    let method_metas: Vec<crate::metadata::class_builder::FnMeta> = class
        .methods
        .iter()
        .map(|m| {
            let params: Vec<Ty> = m.params.iter().map(|p| resolve_ty(&p.ty, syms)).collect();
            let ret = m.ret.as_ref().map(|r| resolve_ty(r, syms)).unwrap_or(Ty::Unit);
            let mut e = MethodEmitter::new_instance(file, info, syms, internal_name, &imports, class_props.clone(), diags);
            e.emit_method(m, &params, ret, &mut cw);
            crate::metadata::class_builder::FnMeta {
                name: m.name.clone(),
                params: m.params.iter().zip(&params).map(|(p, t)| (p.name.clone(), *t)).collect(),
                ret,
            }
        })
        .collect();

    // @kotlin.Metadata (kind=1: class) so a Kotlin consumer sees this as a Kotlin class.
    let ctor_params: Vec<(String, Ty)> = props.iter().map(|(p, t)| (p.name.clone(), *t)).collect();
    let ctor_desc = method_descriptor(&props.iter().map(|(_, t)| *t).collect::<Vec<_>>(), Ty::Unit);
    let prop_metas: Vec<crate::metadata::class_builder::PropMeta> = props
        .iter()
        .map(|(p, t)| {
            let cap = capitalize(&p.name);
            crate::metadata::class_builder::PropMeta {
                name: p.name.clone(),
                ty: *t,
                is_var: p.is_var,
                getter: (format!("get{cap}"), method_descriptor(&[], *t)),
                setter: if p.is_var { Some((format!("set{cap}"), method_descriptor(&[*t], Ty::Unit))) } else { None },
            }
        })
        .collect();
    let (d1_bytes, d2) = crate::metadata::class_builder::build_class(internal_name, &ctor_params, &ctor_desc, &prop_metas, &method_metas);
    let d1 = crate::metadata::encoding::bytes_to_strings(&d1_bytes);
    cw.set_kotlin_metadata(1, &[1, 9, 0], 48, &d1, &d2);

    cw.finish()
}

fn load_local(ty: Ty, slot: u16, code: &mut CodeBuilder) {
    match ty {
        Ty::Int | Ty::Boolean => code.iload(slot),
        Ty::Long => code.lload(slot),
        Ty::Double => code.dload(slot),
        _ => code.aload(slot),
    }
}

fn emit_typed_return(ty: Ty, code: &mut CodeBuilder) {
    match ty {
        Ty::Int | Ty::Boolean => code.ireturn(),
        Ty::Long => code.lreturn(),
        Ty::Double => code.dreturn(),
        _ => code.areturn(),
    }
}

struct MethodEmitter<'a> {
    file: &'a File,
    info: &'a TypeInfo,
    syms: &'a SymbolTable,
    class: String,
    diags: &'a mut DiagSink,
    slots: HashMap<String, (u16, Ty)>,
    next_slot: u16,
    ret_ty: Ty,
    imports: &'a HashMap<String, String>,
    /// Class properties visible to an instance method (implicit `this`); empty for static funcs.
    class_props: HashMap<String, Ty>,
    /// True when emitting an instance method (slot 0 = `this`; bare property names → getfield).
    is_instance: bool,
}

impl<'a> MethodEmitter<'a> {
    fn new(file: &'a File, info: &'a TypeInfo, syms: &'a SymbolTable, class: &str, imports: &'a HashMap<String, String>, diags: &'a mut DiagSink) -> Self {
        MethodEmitter {
            file, info, syms, class: class.to_string(), diags,
            slots: HashMap::new(), next_slot: 0, ret_ty: Ty::Unit, imports,
            class_props: HashMap::new(), is_instance: false,
        }
    }

    fn new_instance(file: &'a File, info: &'a TypeInfo, syms: &'a SymbolTable, class: &str, imports: &'a HashMap<String, String>, class_props: HashMap<String, Ty>, diags: &'a mut DiagSink) -> Self {
        let mut e = MethodEmitter::new(file, info, syms, class, imports, diags);
        e.class_props = class_props;
        e.is_instance = true;
        e.next_slot = 1; // slot 0 reserved for `this`
        e
    }

    /// Emit an instance method: `this` in slot 0, params from slot 1, `public final` (non-static).
    fn emit_method(&mut self, f: &FunDecl, params: &[Ty], ret: Ty, cw: &mut ClassWriter) {
        self.ret_ty = ret;
        for (p, ty) in f.params.iter().zip(params) {
            self.alloc_slot(&p.name, *ty);
        }
        let mut code = CodeBuilder::new(self.next_slot);
        match &f.body {
            FunBody::Expr(e) => {
                self.emit_expr_as(*e, ret, &mut code, cw);
                self.emit_return(ret, &mut code);
            }
            FunBody::Block(b) => self.emit_block_as_body(*b, &mut code, cw),
            FunBody::None => self.emit_default_return(ret, &mut code, cw),
        }
        code.ensure_locals(self.next_slot);
        code.link();
        cw.add_method(ACC_PUBLIC | ACC_FINAL, &f.name, &method_descriptor(params, ret), &code);
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
        self.ret_ty = sig.ret;
        for (p, ty) in f.params.iter().zip(&sig.params) {
            self.alloc_slot(&p.name, *ty);
        }
        let mut code = CodeBuilder::new(self.next_slot);
        match &f.body {
            FunBody::Expr(e) => {
                self.emit_expr_as(*e, sig.ret, &mut code, cw);
                self.emit_return(sig.ret, &mut code);
            }
            FunBody::Block(b) => {
                self.emit_block_as_body(*b, &mut code, cw);
            }
            FunBody::None => self.emit_default_return(sig.ret, &mut code, cw),
        }
        code.ensure_locals(self.next_slot);
        code.link();
        cw.add_method(ACC_PUBLIC | ACC_STATIC | ACC_FINAL, &f.name, &method_descriptor(&sig.params, sig.ret), &code);
    }

    /// A `{ ... }` used directly as a function body: emit statements; a trailing expr (if the fn is
    /// non-Unit) becomes the returned value; otherwise rely on explicit `return`s + a Unit fallthrough.
    fn emit_block_as_body(&mut self, block: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let Expr::Block { stmts, trailing } = self.file.expr(block).clone() else { return };
        for s in &stmts {
            self.emit_stmt(*s, code, cw);
        }
        match trailing {
            Some(te) if self.ret_ty != Ty::Unit => {
                self.emit_expr_as(te, self.ret_ty, code, cw);
                self.emit_return(self.ret_ty, code);
            }
            Some(te) => {
                self.emit_expr(te, code, cw);
                self.discard(self.info.ty(te), code);
                code.ret_void();
            }
            None => {
                if self.ret_ty == Ty::Unit {
                    code.ret_void();
                }
                // non-Unit: explicit `return`s carry control flow (verifier checks completeness)
            }
        }
    }

    fn emit_return(&mut self, ret: Ty, code: &mut CodeBuilder) {
        match ret {
            Ty::Int | Ty::Boolean => code.ireturn(),
            Ty::Long => code.lreturn(),
            Ty::Double => code.dreturn(),
            Ty::String | Ty::Obj(_) => code.areturn(),
            Ty::Unit | Ty::Error => code.ret_void(),
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

    fn discard(&self, t: Ty, code: &mut CodeBuilder) {
        match slot_words(t) {
            1 => code.pop(),
            2 => code.pop2(),
            _ => {}
        }
    }

    // ---- statements ----
    fn emit_stmt(&mut self, s: StmtId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        match self.file.stmt(s).clone() {
            Stmt::Local { name, ty, init, .. } => {
                let lty = ty.as_ref().and_then(|r| Ty::from_name(&r.name)).unwrap_or_else(|| self.info.ty(init));
                self.emit_expr_as(init, lty, code, cw);
                let slot = self.alloc_slot(&name, lty);
                self.store(lty, slot, code);
            }
            Stmt::Assign { name, value } => {
                if let Some(&(slot, ty)) = self.slots.get(&name) {
                    self.emit_expr_as(value, ty, code, cw);
                    self.store(ty, slot, code);
                } else if let Some(&ty) = self.class_props.get(&name).filter(|_| self.is_instance) {
                    // implicit `this.<prop> = value` — write the backing field
                    code.aload(0);
                    self.emit_expr_as(value, ty, code, cw);
                    let f = cw.fieldref(&self.class.clone(), &name, &ty.descriptor());
                    code.putfield(f, slot_words(ty) as i32);
                }
            }
            Stmt::Return(e) => match e {
                Some(ex) => {
                    self.emit_expr_as(ex, self.ret_ty, code, cw);
                    self.emit_return(self.ret_ty, code);
                }
                None => code.ret_void(),
            },
            Stmt::While { cond, body } => {
                let start = code.new_label();
                let end = code.new_label();
                code.bind(start);
                self.emit_cond_jump(cond, end, false, code, cw); // if !cond goto end
                self.emit_block_discard(body, code, cw);
                code.goto(start);
                code.bind(end);
            }
            Stmt::Expr(e) => {
                self.emit_expr(e, code, cw);
                self.discard(self.info.ty(e), code);
            }
        }
    }

    fn store(&self, ty: Ty, slot: u16, code: &mut CodeBuilder) {
        match ty {
            Ty::Int | Ty::Boolean => code.istore(slot),
            Ty::Long => code.lstore(slot),
            Ty::Double => code.dstore(slot),
            Ty::String => code.astore(slot),
            _ => code.astore(slot),
        }
    }

    /// Emit a block for its side effects, discarding any trailing value.
    fn emit_block_discard(&mut self, block: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        if let Expr::Block { stmts, trailing } = self.file.expr(block).clone() {
            for s in &stmts {
                self.emit_stmt(*s, code, cw);
            }
            if let Some(te) = trailing {
                self.emit_expr(te, code, cw);
                self.discard(self.info.ty(te), code);
            }
        } else {
            // a non-block while body
            self.emit_expr(block, code, cw);
            self.discard(self.info.ty(block), code);
        }
    }

    // ---- expressions ----
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
                        _ => code.aload(slot),
                    }
                } else if let Some(&ty) = self.class_props.get(&n).filter(|_| self.is_instance) {
                    // implicit `this.<prop>` — read the backing field
                    code.aload(0);
                    let f = cw.fieldref(&self.class.clone(), &n, &ty.descriptor());
                    code.getfield(f, slot_words(ty) as i32);
                } else {
                    self.diags.error(self.file.expr_spans[e.0 as usize], format!("krusty: unbound local '{n}' in codegen"));
                }
            }
            Expr::Unary { op, operand } => {
                let t = self.info.ty(e);
                match op {
                    UnOp::Neg => {
                        self.emit_expr(operand, code, cw);
                        match t {
                            Ty::Int => code.ineg(),
                            Ty::Long => code.lneg(),
                            Ty::Double => code.dneg(),
                            _ => {}
                        }
                    }
                    UnOp::Not => self.emit_bool(e, code, cw),
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                if is_arith(op) {
                    self.emit_arith_expr(e, op, lhs, rhs, code, cw);
                } else {
                    self.emit_bool(e, code, cw); // comparison / && / ||
                }
            }
            Expr::Call { callee, args } => self.emit_call(e, callee, &args, code, cw),
            Expr::Member { receiver, name } => {
                if name == "length" {
                    self.emit_expr(receiver, code, cw);
                    let m = cw.methodref("java/lang/String", "length", "()I");
                    code.invokevirtual(m, 0, 1);
                } else if let Ty::Obj(internal) = self.info.ty(receiver) {
                    // Property read on a class value: `p.prop` → invokevirtual get<Prop>().
                    let pty = self
                        .syms
                        .class_by_internal(internal)
                        .and_then(|c| c.prop(&name))
                        .map(|(t, _)| t)
                        .unwrap_or(Ty::Error);
                    self.emit_expr(receiver, code, cw);
                    let getter = format!("get{}", capitalize(&name));
                    let m = cw.methodref(internal, &getter, &method_descriptor(&[], pty));
                    code.invokevirtual(m, 0, slot_words(pty) as i32);
                } else {
                    self.diags.error(self.file.expr_spans[e.0 as usize], format!("krusty v0: member '{name}' not emittable"));
                }
            }
            Expr::If { cond, then_branch, else_branch } => {
                let result = self.info.ty(e);
                match else_branch {
                    Some(eb) => {
                        let l_else = code.new_label();
                        let l_end = code.new_label();
                        self.emit_cond_jump(cond, l_else, false, code, cw); // if !cond goto else
                        self.emit_expr_as(then_branch, result, code, cw);
                        code.goto(l_end);
                        code.bind(l_else);
                        self.emit_expr_as(eb, result, code, cw);
                        code.bind(l_end);
                    }
                    None => {
                        // statement-if (Unit value)
                        let l_end = code.new_label();
                        self.emit_cond_jump(cond, l_end, false, code, cw);
                        self.emit_block_discard(then_branch, code, cw);
                        code.bind(l_end);
                    }
                }
            }
            Expr::Block { stmts, trailing } => {
                for s in &stmts {
                    self.emit_stmt(*s, code, cw);
                }
                if let Some(te) = trailing {
                    self.emit_expr(te, code, cw);
                }
            }
        }
    }

    /// Emit `e` as a boolean value (0/1 int on the stack).
    fn emit_bool(&mut self, e: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        match self.file.expr(e).clone() {
            Expr::Binary { op, lhs, rhs } if is_cmp(op) || op == BinOp::And || op == BinOp::Or => {
                let l_true = code.new_label();
                let l_end = code.new_label();
                // jump to l_true when the condition holds, else fall through to push 0
                self.emit_cond_jump(e, l_true, true, code, cw);
                code.push_int(0, cw);
                code.goto(l_end);
                code.bind(l_true);
                code.push_int(1, cw);
                code.bind(l_end);
                let _ = (op, lhs, rhs);
            }
            Expr::Unary { op: UnOp::Not, operand } => {
                self.emit_bool(operand, code, cw);
                code.push_int(1, cw);
                code.ixor();
            }
            _ => self.emit_expr(e, code, cw), // already a 0/1 boolean value (var/call)
        }
    }

    /// Emit code that jumps to `target` when `cond` is `want` (true/false). Short-circuits `&&`/`||`.
    fn emit_cond_jump(&mut self, cond: ExprId, target: Label, want: bool, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        match self.file.expr(cond).clone() {
            Expr::Binary { op: BinOp::And, lhs, rhs } => {
                if want {
                    // jump if (lhs && rhs): if !lhs skip; if rhs jump
                    let skip = code.new_label();
                    self.emit_cond_jump(lhs, skip, false, code, cw);
                    self.emit_cond_jump(rhs, target, true, code, cw);
                    code.bind(skip);
                } else {
                    // jump if !(lhs && rhs) = !lhs || !rhs
                    self.emit_cond_jump(lhs, target, false, code, cw);
                    self.emit_cond_jump(rhs, target, false, code, cw);
                }
            }
            Expr::Binary { op: BinOp::Or, lhs, rhs } => {
                if want {
                    self.emit_cond_jump(lhs, target, true, code, cw);
                    self.emit_cond_jump(rhs, target, true, code, cw);
                } else {
                    let skip = code.new_label();
                    self.emit_cond_jump(lhs, skip, true, code, cw);
                    self.emit_cond_jump(rhs, target, false, code, cw);
                    code.bind(skip);
                }
            }
            Expr::Unary { op: UnOp::Not, operand } => {
                self.emit_cond_jump(operand, target, !want, code, cw);
            }
            Expr::Binary { op, lhs, rhs } if is_cmp(op) => {
                let cmp = if want { op } else { negate_cmp(op) };
                self.emit_compare_jump(cmp, lhs, rhs, target, code, cw);
            }
            _ => {
                // arbitrary boolean value: compare against 0
                self.emit_expr(cond, code, cw);
                if want {
                    code.ifne(target);
                } else {
                    code.ifeq(target);
                }
            }
        }
    }

    fn emit_compare_jump(&mut self, op: BinOp, lhs: ExprId, rhs: ExprId, target: Label, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let lt = self.info.ty(lhs);
        let rt = self.info.ty(rhs);
        let common = Ty::promote(lt, rt).unwrap_or(lt);
        self.emit_expr_as(lhs, common, code, cw);
        self.emit_expr_as(rhs, common, code, cw);
        match common {
            Ty::Int | Ty::Boolean => match op {
                BinOp::Lt => code.if_icmplt(target),
                BinOp::Le => code.if_icmple(target),
                BinOp::Gt => code.if_icmpgt(target),
                BinOp::Ge => code.if_icmpge(target),
                BinOp::Eq => code.if_icmpeq(target),
                BinOp::Ne => code.if_icmpne(target),
                _ => {}
            },
            Ty::Long => {
                code.lcmp();
                self.cmp0(op, target, code);
            }
            Ty::Double => {
                code.dcmpg();
                self.cmp0(op, target, code);
            }
            _ => {
                self.diags.error(self.file.expr_spans[lhs.0 as usize], "krusty v0: unsupported comparison operand type");
            }
        }
    }

    fn cmp0(&mut self, op: BinOp, target: Label, code: &mut CodeBuilder) {
        match op {
            BinOp::Lt => code.iflt(target),
            BinOp::Le => code.ifle(target),
            BinOp::Gt => code.ifgt(target),
            BinOp::Ge => code.ifge(target),
            BinOp::Eq => code.ifeq(target),
            BinOp::Ne => code.ifne(target),
            _ => {}
        }
    }

    fn emit_arith_expr(&mut self, e: ExprId, op: BinOp, lhs: ExprId, rhs: ExprId, code: &mut CodeBuilder, cw: &mut ClassWriter) {
        let result = self.info.ty(e);
        if op == BinOp::Add && result == Ty::String {
            self.emit_concat(lhs, rhs, code, cw);
            return;
        }
        self.emit_expr_as(lhs, result, code, cw);
        self.emit_expr_as(rhs, result, code, cw);
        self.emit_arith(op, result, code);
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
        // Java static call: ClassName.method(args) resolved via the classpath.
        if let Expr::Member { receiver, name } = self.file.expr(callee).clone() {
            if let Expr::Name(cls) = self.file.expr(receiver).clone() {
                if !self.slots.contains_key(&cls) {
                    if let Some(internal) = self.imports.get(&cls).cloned() {
                        let arg_tys: Vec<Ty> = args.iter().map(|a| self.info.ty(*a)).collect();
                        if let Some((owner, desc, ret)) = resolve_java_static(&self.syms.classpath, &internal, &name, &arg_tys) {
                            for a in args {
                                self.emit_expr(*a, code, cw);
                            }
                            let arg_words: i32 = arg_tys.iter().map(|t| slot_words(*t) as i32).sum();
                            let m = cw.methodref(&owner, &name, &desc);
                            code.invokestatic(m, arg_words, slot_words(ret) as i32);
                            return;
                        }
                    }
                }
            }
        }
        match self.file.expr(callee).clone() {
            Expr::Member { receiver, name } if name == "toString" && args.is_empty() => {
                let rt = self.info.ty(receiver);
                self.emit_expr(receiver, code, cw);
                let (desc, words) = match rt {
                    Ty::String => return, // identity
                    Ty::Int | Ty::Boolean => ("(I)Ljava/lang/String;", 1),
                    Ty::Long => ("(J)Ljava/lang/String;", 2),
                    Ty::Double => ("(D)Ljava/lang/String;", 2),
                    _ => return,
                };
                let m = cw.methodref("java/lang/String", "valueOf", desc);
                code.invokestatic(m, words, 1);
            }
            // java.lang.String instance method: recv.method(args) -> invokevirtual
            Expr::Member { receiver, name }
                if self.info.ty(receiver) == Ty::String
                    && crate::resolve::resolve_string_instance(&name, &args.iter().map(|a| self.info.ty(*a)).collect::<Vec<_>>()).is_some() =>
            {
                let arg_tys: Vec<Ty> = args.iter().map(|a| self.info.ty(*a)).collect();
                let (desc, ret) = crate::resolve::resolve_string_instance(&name, &arg_tys).unwrap();
                self.emit_expr(receiver, code, cw);
                for a in args {
                    self.emit_expr(*a, code, cw);
                }
                let arg_words: i32 = arg_tys.iter().map(|t| slot_words(*t) as i32).sum();
                let m = cw.methodref("java/lang/String", &name, desc);
                code.invokevirtual(m, arg_words, slot_words(ret) as i32);
            }
            // Instance method call on a class value: `p.method(args)` → invokevirtual.
            Expr::Member { receiver, name }
                if matches!(self.info.ty(receiver), Ty::Obj(_))
                    && self.syms.class_by_internal(self.info.ty(receiver).obj_internal().unwrap()).map_or(false, |c| c.methods.contains_key(&name)) =>
            {
                let internal = self.info.ty(receiver).obj_internal().unwrap();
                let sig = self.syms.class_by_internal(internal).unwrap().methods.get(&name).unwrap().clone();
                self.emit_expr(receiver, code, cw);
                for (a, pty) in args.iter().zip(&sig.params) {
                    self.emit_expr_as(*a, *pty, code, cw);
                }
                let arg_words: i32 = sig.params.iter().map(|t| slot_words(*t) as i32).sum();
                let m = cw.methodref(internal, &name, &method_descriptor(&sig.params, sig.ret));
                code.invokevirtual(m, arg_words, slot_words(sig.ret) as i32);
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
            // Constructor call: `ClassName(args)` → new + dup + invokespecial <init>.
            Expr::Name(fname) if !self.slots.contains_key(&fname) && self.syms.classes.contains_key(&fname) => {
                let cls = self.syms.classes.get(&fname).unwrap();
                let internal = cls.internal.clone();
                let ctor_tys: Vec<Ty> = cls.props.iter().map(|(_, t, _)| *t).collect();
                let class_idx = cw.class_ref(&internal);
                code.new_obj(class_idx);
                code.dup();
                for (a, pty) in args.iter().zip(&ctor_tys) {
                    self.emit_expr_as(*a, *pty, code, cw);
                }
                let arg_words: i32 = ctor_tys.iter().map(|t| slot_words(*t) as i32).sum();
                let desc = method_descriptor(&ctor_tys, Ty::Unit);
                let m = cw.methodref(&internal, "<init>", &desc);
                code.invokespecial(m, arg_words, 0);
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
                self.diags.error(self.file.expr_spans[e.0 as usize], "krusty v0: unsupported call form");
            }
        }
    }
}

fn is_arith(op: BinOp) -> bool {
    matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem)
}
fn is_cmp(op: BinOp) -> bool {
    matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne)
}
fn negate_cmp(op: BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Ge,
        BinOp::Le => BinOp::Gt,
        BinOp::Gt => BinOp::Le,
        BinOp::Ge => BinOp::Lt,
        BinOp::Eq => BinOp::Ne,
        BinOp::Ne => BinOp::Eq,
        other => other,
    }
}

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
