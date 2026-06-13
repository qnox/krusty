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
use crate::jvm::classpath::Classpath;
use crate::types::Ty;

#[derive(Clone, Debug)]
pub struct Signature {
    pub params: Vec<Ty>,
    pub ret: Ty,
}

/// Everything a caller needs about a declared Kotlin class: its JVM internal name, its
/// primary-constructor properties (in order), and its member-function signatures.
#[derive(Clone, Debug)]
pub struct ClassSig {
    pub internal: String,
    pub props: Vec<(String, Ty, bool)>, // (name, type, is_var)
    pub methods: HashMap<String, Signature>,
}

impl ClassSig {
    pub fn prop(&self, name: &str) -> Option<(Ty, bool)> {
        self.props.iter().find(|(n, _, _)| n == name).map(|(_, t, v)| (*t, *v))
    }
}

#[derive(Default)]
pub struct SymbolTable {
    pub funs: HashMap<String, Signature>,
    /// Declared classes by simple name (e.g. `Point`).
    pub classes: HashMap<String, ClassSig>,
    /// Top-level properties (name → type, is_var), backed by static fields on the file facade.
    pub props: HashMap<String, (Ty, bool)>,
    /// Classpath for resolving Java/JDK references (empty unless the driver sets `-classpath`).
    pub classpath: Classpath,
}

impl SymbolTable {
    /// Resolve a class reference type `Ty::Obj` back to its declaration (by internal name).
    pub fn class_by_internal(&self, internal: &str) -> Option<&ClassSig> {
        self.classes.values().find(|c| c.internal == internal)
    }
}

/// Map a file's imports `simple name -> internal name` (e.g. `Calc -> util/Calc`).
pub fn import_map(file: &File) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for fq in &file.imports {
        if let Some(simple) = fq.rsplit('.').next() {
            m.insert(simple.to_string(), fq.replace('.', "/"));
        }
    }
    m
}

/// Map a single JVM field descriptor to a krusty `Ty` (the v0 supported set).
pub fn desc_to_ty(d: &str) -> Ty {
    match d {
        "I" => Ty::Int,
        "J" => Ty::Long,
        "D" => Ty::Double,
        "Z" => Ty::Boolean,
        "V" => Ty::Unit,
        "Ljava/lang/String;" => Ty::String,
        _ => Ty::Error,
    }
}

/// Resolve a `java.lang.String` *instance* method by name + argument types. Returns
/// `(jvm descriptor, return type)` for `invokevirtual java/lang/String`. This is a curated subset
/// of real `java.lang.String` methods (the JDK lives in jimage, which the classpath reader doesn't
/// read yet); each entry matches what kotlinc emits for the same call.
pub fn resolve_string_instance(method: &str, arg_tys: &[Ty]) -> Option<(&'static str, Ty)> {
    Some(match (method, arg_tys) {
        ("length", []) => ("()I", Ty::Int),
        ("isEmpty", []) => ("()Z", Ty::Boolean),
        ("substring", [Ty::Int]) => ("(I)Ljava/lang/String;", Ty::String),
        ("substring", [Ty::Int, Ty::Int]) => ("(II)Ljava/lang/String;", Ty::String),
        ("indexOf", [Ty::String]) => ("(Ljava/lang/String;)I", Ty::Int),
        ("concat", [Ty::String]) => ("(Ljava/lang/String;)Ljava/lang/String;", Ty::String),
        _ => return None,
    })
}

/// Resolve a static call `Class.method(args)` against the classpath by exact param-descriptor
/// match. Returns `(owner internal name, method descriptor, return type)`.
pub fn resolve_java_static(cp: &Classpath, internal: &str, method: &str, arg_tys: &[Ty]) -> Option<(String, String, Ty)> {
    let ci = cp.find(internal)?;
    let params: String = arg_tys.iter().map(|t| t.descriptor()).collect();
    let prefix = format!("({params})");
    let m = ci.methods.iter().find(|m| m.name == method && m.is_static() && m.descriptor.starts_with(&prefix))?;
    let ret = m.descriptor[m.descriptor.find(')').unwrap() + 1..].to_string();
    Some((internal.to_string(), m.descriptor.clone(), desc_to_ty(&ret)))
}

fn class_internal(file: &File, name: &str) -> String {
    match &file.package {
        Some(pkg) if !pkg.is_empty() => format!("{}/{}", pkg.replace('.', "/"), name),
        _ => name.to_string(),
    }
}

/// Stage C: collect top-level function + class signatures across all files. Two passes so that a
/// class type can be referenced before its declaration (and across files).
pub fn collect_signatures(files: &[File], diags: &mut DiagSink) -> SymbolTable {
    // Pass 1: every class simple-name -> internal name (no bodies, just the type universe).
    let mut class_names: HashMap<String, String> = HashMap::new();
    for file in files {
        for &d in &file.decls {
            if let Decl::Class(c) = file.decl(d) {
                let internal = class_internal(file, &c.name);
                if class_names.insert(c.name.clone(), internal).is_some() {
                    diags.error(c.span, format!("conflicting declaration of '{}'", c.name));
                }
            }
        }
    }

    // Pass 2: resolve signatures/properties against the now-complete type universe.
    let mut table = SymbolTable::default();
    for file in files {
        for &d in &file.decls {
            match file.decl(d) {
                Decl::Fun(f) => {
                    let params = f.params.iter().map(|p| ty_of_ref(&p.ty, &class_names, diags)).collect();
                    let ret = match &f.ret {
                        Some(r) => ty_of_ref(r, &class_names, diags),
                        None => Ty::Unit, // v0: missing return type defaults to Unit
                    };
                    if table.funs.insert(f.name.clone(), Signature { params, ret }).is_some() {
                        diags.error(f.span, format!("conflicting declaration of '{}'", f.name));
                    }
                }
                Decl::Class(c) => {
                    let internal = class_names.get(&c.name).cloned().unwrap_or_else(|| class_internal(file, &c.name));
                    let props: Vec<(String, Ty, bool)> = c
                        .props
                        .iter()
                        .map(|p| (p.name.clone(), ty_of_ref(&p.ty, &class_names, diags), p.is_var))
                        .collect();
                    let mut methods: HashMap<String, Signature> = c
                        .methods
                        .iter()
                        .map(|m| {
                            let params = m.params.iter().map(|p| ty_of_ref(&p.ty, &class_names, diags)).collect();
                            let ret = m.ret.as_ref().map(|r| ty_of_ref(r, &class_names, diags)).unwrap_or(Ty::Unit);
                            (m.name.clone(), Signature { params, ret })
                        })
                        .collect();
                    // `data class` synthesizes componentN() + copy(props...) callable members.
                    if c.is_data {
                        let self_ty = Ty::obj(&internal);
                        for (i, (_, ty, _)) in props.iter().enumerate() {
                            methods.insert(format!("component{}", i + 1), Signature { params: vec![], ret: *ty });
                        }
                        methods.insert("copy".into(), Signature { params: props.iter().map(|(_, t, _)| *t).collect(), ret: self_ty });
                    }
                    table.classes.insert(c.name.clone(), ClassSig { internal, props, methods });
                }
                Decl::Property(p) => {
                    // Type from the annotation, else a light inference from a literal initializer.
                    let ty = match &p.ty {
                        Some(r) => ty_of_ref(r, &class_names, diags),
                        None => infer_lit_ty(file, p.init),
                    };
                    table.props.insert(p.name.clone(), (ty, p.is_var));
                }
            }
        }
    }
    table
}

/// Best-effort type of a simple literal initializer (for an unannotated top-level property).
fn infer_lit_ty(file: &File, e: ExprId) -> Ty {
    match file.expr(e) {
        Expr::IntLit(_) => Ty::Int,
        Expr::LongLit(_) => Ty::Long,
        Expr::DoubleLit(_) => Ty::Double,
        Expr::BoolLit(_) => Ty::Boolean,
        Expr::StringLit(_) => Ty::String,
        _ => Ty::Error,
    }
}

/// Resolve a syntactic type reference to a `Ty`: a primitive/String/Unit, or a declared class
/// (→ `Ty::Obj` with the class's internal name).
fn ty_of_ref(r: &TypeRef, classes: &HashMap<String, String>, diags: &mut DiagSink) -> Ty {
    let base = if let Some(t) = Ty::from_name(&r.name) {
        t
    } else if let Some(internal) = classes.get(&r.name) {
        Ty::obj(internal)
    } else {
        diags.error(r.span, format!("unknown type '{}'", r.name));
        Ty::Error
    };
    // Nullable reference types share the non-null JVM descriptor; nullable primitives would need
    // boxing (out of subset) so they are rejected (the file is skipped, never miscompiled).
    if r.nullable && !base.is_reference() && base != Ty::Error {
        diags.error(r.span, format!("nullable primitive type '{}?' is not supported", r.name));
        return Ty::Error;
    }
    base
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
    let imports = import_map(file);
    let mut c = Checker {
        file,
        syms,
        diags,
        expr_types: vec![Ty::Error; file.expr_arena.len()],
        scopes: Vec::new(),
        ret_ty: Ty::Unit,
        imports,
    };
    for &d in &file.decls {
        match file.decl(d) {
            Decl::Fun(f) => c.check_fun(f),
            Decl::Class(cl) => {
                // Member functions are checked with the class's properties (resolved in Stage C)
                // visible as an implicit `this` scope.
                let props = syms.classes.get(&cl.name).map(|s| s.props.clone()).unwrap_or_default();
                for m in &cl.methods {
                    c.check_method(m, &props);
                }
            }
            Decl::Property(p) => {
                let it = c.expr(p.init);
                if let Some((declared, _)) = syms.props.get(&p.name).copied().filter(|(t, _)| *t != Ty::Error) {
                    if p.ty.is_some() {
                        c.expect_assignable(declared, it, c.span(p.init), "property initializer");
                    }
                }
            }
        }
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
    imports: HashMap<String, String>,
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

    /// Resolve a syntactic type to a `Ty`, including declared class types (→ `Ty::Obj`).
    /// Nullability doesn't change the `Ty` for reference types (same JVM descriptor).
    fn resolve_ty(&self, r: &TypeRef) -> Ty {
        if let Some(t) = Ty::from_name(&r.name) {
            return t;
        }
        if let Some(cs) = self.syms.classes.get(&r.name) {
            return Ty::obj(&cs.internal);
        }
        Ty::Error
    }

    fn check_fun(&mut self, f: &FunDecl) {
        let sig = self.syms.funs.get(&f.name);
        self.ret_ty = sig.map(|s| s.ret).unwrap_or(Ty::Unit);
        self.push_scope();
        for p in &f.params {
            let ty = self.resolve_ty(&p.ty);
            self.declare(&p.name, ty, false);
        }
        self.check_fun_body(f);
        self.pop_scope();
    }

    /// Check an instance method: the class properties are visible (implicit `this`), then the
    /// method's own parameters shadow them.
    fn check_method(&mut self, f: &FunDecl, props: &[(String, Ty, bool)]) {
        self.ret_ty = f.ret.as_ref().map(|r| self.resolve_ty(r)).unwrap_or(Ty::Unit);
        self.push_scope(); // implicit-this scope (properties)
        for (n, t, is_var) in props {
            self.declare(n, *t, *is_var);
        }
        self.push_scope(); // parameter scope
        for p in &f.params {
            let ty = self.resolve_ty(&p.ty);
            self.declare(&p.name, ty, false);
        }
        self.check_fun_body(f);
        self.pop_scope();
        self.pop_scope();
    }

    fn check_fun_body(&mut self, f: &FunDecl) {
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
    }

    fn expect_assignable(&mut self, expected: Ty, actual: Ty, span: Span, ctx: &str) {
        if expected == Ty::Error || actual == Ty::Error {
            return;
        }
        // `null` is assignable to any reference type (krusty is permissive about nullability).
        if actual == Ty::Null && expected.is_reference() {
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
            Expr::NullLit => Ty::Null,
            Expr::NotNull { operand } => self.expr(operand), // value with the same (non-null) type
            Expr::Elvis { lhs, rhs } => {
                let lt = self.expr(lhs);
                let rt = self.expr(rhs);
                if lt == Ty::Null {
                    rt
                } else if rt == Ty::Null {
                    lt
                } else {
                    self.join(lt, rt, self.span(e))
                }
            }
            Expr::Name(n) => match self.lookup(&n) {
                Some(l) => l.ty,
                None => match self.syms.props.get(&n) {
                    Some(&(ty, _)) => ty, // top-level property
                    None => {
                        self.diags.error(self.span(e), format!("unresolved reference '{n}'"));
                        Ty::Error
                    }
                },
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
            Expr::When { subject, arms } => {
                let subj_ty = subject.map(|s| self.expr(s));
                let mut result: Option<Ty> = None;
                let mut has_else = false;
                for arm in &arms {
                    if arm.conditions.is_empty() {
                        has_else = true;
                    }
                    for &cnd in &arm.conditions {
                        let ct = self.expr(cnd);
                        match subj_ty {
                            // subject form: condition must be comparable to the subject
                            Some(st) if st != Ty::Error && ct != Ty::Error && st != ct && Ty::promote(st, ct).is_none() => {
                                self.diags.error(self.span(cnd), format!("when condition type '{}' is not comparable to subject '{}'", ct.name(), st.name()));
                            }
                            // subjectless form: condition must be Boolean
                            None => self.expect_assignable(Ty::Boolean, ct, self.span(cnd), "when condition"),
                            _ => {}
                        }
                    }
                    let bt = self.expr(arm.body);
                    result = Some(match result {
                        Some(r) => self.join(r, bt, self.span(arm.body)),
                        None => bt,
                    });
                }
                // A `when` is only an expression (carries a value) when it is exhaustive (has `else`).
                if has_else { result.unwrap_or(Ty::Unit) } else { Ty::Unit }
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
                if lt == rt || Ty::promote(lt, rt).is_some() || (lt.is_reference() && rt.is_reference()) {
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
        if let (Ty::String, "length") = (rt, name) {
            return Ty::Int;
        }
        // Property read on a class value: `p.prop`.
        if let Ty::Obj(internal) = rt {
            if let Some((ty, _)) = self.syms.class_by_internal(internal).and_then(|c| c.prop(name)) {
                return ty;
            }
        }
        self.diags.error(span, format!("unresolved member '{name}' on '{}'", rt.name()));
        Ty::Error
    }

    fn check_call(&mut self, callee: ExprId, args: &[ExprId], span: Span) -> Ty {
        match self.file.expr(callee).clone() {
            // method call: recv.method(args)
            Expr::Member { receiver, name } => {
                // Java static call: `ClassName.method(args)` where ClassName is an imported class
                // (not a local/param) resolvable on the classpath.
                if let Expr::Name(cls) = self.file.expr(receiver).clone() {
                    if self.lookup(&cls).is_none() {
                        if let Some(internal) = self.imports.get(&cls).cloned() {
                            let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                            return match resolve_java_static(&self.syms.classpath, &internal, &name, &arg_tys) {
                                Some((_, _, ret)) => ret,
                                None => {
                                    self.diags.error(span, format!("unresolved Java static '{cls}.{name}' for given argument types"));
                                    Ty::Error
                                }
                            };
                        }
                    }
                }
                let rt = self.expr(receiver);
                let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                if rt == Ty::Error {
                    return Ty::Error;
                }
                if let ("toString", []) = (name.as_str(), arg_tys.as_slice()) {
                    return Ty::String; // intrinsic on any type
                }
                if rt == Ty::String {
                    if let Some((_, ret)) = resolve_string_instance(&name, &arg_tys) {
                        return ret;
                    }
                }
                // Instance method call on a class value: `p.method(args)`.
                if let Ty::Obj(internal) = rt {
                    if let Some(sig) = self.syms.class_by_internal(internal).and_then(|c| c.methods.get(&name)).cloned() {
                        if sig.params.len() != arg_tys.len() {
                            self.diags.error(span, format!("method '{name}' expects {} args, got {}", sig.params.len(), arg_tys.len()));
                        } else {
                            for (i, (p, a)) in sig.params.iter().zip(&arg_tys).enumerate() {
                                self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                            }
                        }
                        return sig.ret;
                    }
                }
                self.diags.error(span, format!("unresolved method '{name}' on '{}'", rt.name()));
                Ty::Error
            }
            // free function call: name(args)
            Expr::Name(fname) => {
                let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                if fname == "println" {
                    return Ty::Unit; // builtin: accepts one value of any type (v0)
                }
                // Constructor call: `ClassName(args)` (when not shadowed by a local).
                if self.lookup(&fname).is_none() {
                    if let Some(cls) = self.syms.classes.get(&fname).cloned() {
                        let ctor_params: Vec<Ty> = cls.props.iter().map(|(_, t, _)| *t).collect();
                        if ctor_params.len() != arg_tys.len() {
                            self.diags.error(span, format!("constructor '{fname}' expects {} args, got {}", ctor_params.len(), arg_tys.len()));
                        } else {
                            for (i, (p, a)) in ctor_params.iter().zip(&arg_tys).enumerate() {
                                self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                            }
                        }
                        return Ty::obj(&cls.internal);
                    }
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
        // `null` joins with any reference type to that (nullable) reference type.
        if a == Ty::Null && b.is_reference() {
            return b;
        }
        if b == Ty::Null && a.is_reference() {
            return a;
        }
        self.diags.error(span, format!("incompatible if branches: '{}' and '{}'", a.name(), b.name()));
        Ty::Error
    }

    fn stmt(&mut self, s: StmtId) {
        match self.file.stmt(s).clone() {
            Stmt::Local { is_var, name, ty, init } => {
                let it = self.expr(init);
                let declared = ty.as_ref().map(|r| self.resolve_ty(r));
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
                    None => match self.syms.props.get(&name).copied() {
                        Some((lty, is_var)) => {
                            if !is_var {
                                self.diags.error(self.file.stmt_spans[s.0 as usize], format!("'val' {name} cannot be reassigned"));
                            }
                            self.expect_assignable(lty, vt, self.file.stmt_spans[s.0 as usize], "assignment");
                        }
                        None => {
                            self.diags.error(self.file.stmt_spans[s.0 as usize], format!("unresolved reference '{name}'"));
                        }
                    },
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
            Stmt::For { name, range, body } => {
                let st = self.expr(range.start);
                self.expect_assignable(Ty::Int, st, self.span(range.start), "range start");
                let et = self.expr(range.end);
                self.expect_assignable(Ty::Int, et, self.span(range.end), "range end");
                if let Some(step) = range.step {
                    let stp = self.expr(step);
                    self.expect_assignable(Ty::Int, stp, self.span(step), "range step");
                }
                self.push_scope();
                self.declare(&name, Ty::Int, true); // loop variable (mutated by the lowering)
                self.expr(body);
                self.pop_scope();
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

    #[test]
    fn string_instance_methods() {
        ok("fun f(s: String): String = s.substring(1)");
        ok("fun f(s: String): String = s.substring(1, 3)");
        ok("fun f(s: String): Int = s.indexOf(\"x\")");
        ok("fun f(s: String): String = s.concat(\"y\")");
        err_contains("fun f(s: String): String = s.substring(\"x\")", "unresolved method");
        err_contains("fun f(a: Int): Int = a.substring(1)", "unresolved method");
    }

    #[test]
    fn reference_types_resolve() {
        // class-typed param + property read + construction + instance call all typecheck.
        ok("class Point(val x: Int, val y: Int)\nfun ox(p: Point): Int = p.x");
        ok("class Point(val x: Int)\nfun mk(): Point = Point(3)");
        ok("class Point(val x: Int) {\n  fun get(): Int = x\n}\nfun use(p: Point): Int = p.get()");
        ok("class Box(val v: Int)\nclass Pair(val a: Box, val b: Box)\nfun first(p: Pair): Int = p.a.v");
        // forward reference: a function can mention a class declared later.
        ok("fun ox(p: Point): Int = p.x\nclass Point(val x: Int)");
    }

    #[test]
    fn reference_type_errors() {
        err_contains("class Point(val x: Int)\nfun f(p: Point): Int = p.z", "unresolved member 'z'");
        err_contains("class Point(val x: Int)\nfun f(): Point = Point()", "expects 1 args");
        err_contains("fun f(p: Widget): Int = 0", "unknown type 'Widget'");
    }

    #[test]
    fn string_method_table() {
        assert_eq!(resolve_string_instance("substring", &[Ty::Int]), Some(("(I)Ljava/lang/String;", Ty::String)));
        assert_eq!(resolve_string_instance("indexOf", &[Ty::String]), Some(("(Ljava/lang/String;)I", Ty::Int)));
        assert_eq!(resolve_string_instance("substring", &[Ty::String]), None);
    }
}
