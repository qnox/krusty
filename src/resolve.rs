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
    /// True if the last parameter is `vararg` (its `Ty` is the array type; callers pack trailing args).
    pub vararg: bool,
}

/// Everything a caller needs about a declared Kotlin class: its JVM internal name, its
/// primary-constructor properties (in order), and its member-function signatures.
#[derive(Clone, Debug)]
pub struct ClassSig {
    pub internal: String,
    pub props: Vec<(String, Ty, bool)>, // backing-field properties (name, type, is_var)
    /// Full primary-constructor parameter types in order (includes non-property params).
    pub ctor_params: Vec<Ty>,
    pub methods: HashMap<String, Signature>,
    /// True if this is an `interface` (calls dispatch via `invokeinterface`).
    pub is_interface: bool,
    /// True if declared `sealed` — all subclasses are known in this module, enabling exhaustive
    /// `when` without an `else`.
    pub is_sealed: bool,
    /// `companion object` functions, emitted as `static` methods and called as `ClassName.fn(...)`.
    pub static_methods: HashMap<String, Signature>,
    /// `companion object` properties, emitted as `static final` fields read as `ClassName.PROP`.
    pub static_props: HashMap<String, Ty>,
    /// Names of `lateinit` properties (instance and companion) — reads emit a null-check that throws.
    pub lateinit_props: std::collections::HashSet<String>,
    /// Internal names of interfaces this type implements (for subtyping).
    pub interfaces: Vec<String>,
    /// Internal name of the base class (`: Base(..)`), if any.
    pub super_internal: Option<String>,
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
    /// Simple names declared as `object` singletons (accessed via `Name.member`).
    pub objects: std::collections::HashSet<String>,
    /// Declared `enum` types (simple name → entry names), accessed via `Name.ENTRY`.
    pub enums: HashMap<String, Vec<String>>,
    /// Classpath for resolving Java/JDK references (empty unless the driver sets `-classpath`).
    pub classpath: Classpath,
}

impl SymbolTable {
    /// Resolve a class reference type `Ty::Obj` back to its declaration (by internal name).
    pub fn class_by_internal(&self, internal: &str) -> Option<&ClassSig> {
        self.classes.values().find(|c| c.internal == internal)
    }

    /// A method (own or inherited up the base-class chain) on a class internal name.
    pub fn method_of(&self, internal: &str, name: &str) -> Option<Signature> {
        let c = self.class_by_internal(internal)?;
        if let Some(sig) = c.methods.get(name) {
            return Some(sig.clone());
        }
        let s = c.super_internal.clone()?;
        self.method_of(&s, name)
    }

    /// All method signatures inherited from declared supertypes (base-class chain + interfaces,
    /// recursively) as `(name, signature)`. Used to detect overrides that would need a JVM bridge
    /// method (covariant/generic return), which krusty does not synthesize.
    pub fn supertype_methods(&self, internal: &str) -> Vec<(String, Signature)> {
        let mut out = Vec::new();
        self.collect_super_methods(internal, &mut out);
        out
    }
    fn collect_super_methods(&self, internal: &str, out: &mut Vec<(String, Signature)>) {
        let Some(c) = self.class_by_internal(internal) else { return };
        let mut parents: Vec<String> = Vec::new();
        if let Some(s) = &c.super_internal {
            parents.push(s.clone());
        }
        parents.extend(c.interfaces.iter().cloned());
        for p in parents {
            if let Some(pc) = self.class_by_internal(&p) {
                for (n, sig) in &pc.methods {
                    out.push((n.clone(), sig.clone()));
                }
            }
            self.collect_super_methods(&p, out);
        }
    }

    /// All declared supertypes (base-class chain + interfaces, transitively) of `internal`.
    pub fn supertype_internals(&self, internal: &str) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_super_internals(internal, &mut out);
        out
    }
    fn collect_super_internals(&self, internal: &str, out: &mut Vec<String>) {
        let Some(c) = self.class_by_internal(internal) else { return };
        let mut parents: Vec<String> = Vec::new();
        if let Some(s) = &c.super_internal {
            parents.push(s.clone());
        }
        parents.extend(c.interfaces.iter().cloned());
        for p in parents {
            if !out.contains(&p) {
                out.push(p.clone());
                self.collect_super_internals(&p, out);
            }
        }
    }

    /// Internal names of declared classes whose direct base class is `internal`.
    pub fn subclasses_of(&self, internal: &str) -> Vec<String> {
        self.classes
            .values()
            .filter(|c| c.super_internal.as_deref() == Some(internal))
            .map(|c| c.internal.clone())
            .collect()
    }

    /// A property (own or inherited) on a class internal name. Returns `(type, is_var)`.
    pub fn prop_of(&self, internal: &str, name: &str) -> Option<(Ty, bool)> {
        let c = self.class_by_internal(internal)?;
        if let Some(p) = c.prop(name) {
            return Some(p);
        }
        let s = c.super_internal.clone()?;
        self.prop_of(&s, name)
    }
}

/// The JVM internal name of a common JDK exception/error referenced by simple name (Kotlin's
/// auto-imported `kotlin.*` exception aliases map onto these `java.lang` types). Used so
/// `throw RuntimeException("…")` resolves without an explicit import.
pub fn builtin_exception(name: &str) -> Option<&'static str> {
    Some(match name {
        "Throwable" => "java/lang/Throwable",
        "Exception" => "java/lang/Exception",
        "RuntimeException" => "java/lang/RuntimeException",
        "Error" => "java/lang/Error",
        "IllegalStateException" => "java/lang/IllegalStateException",
        "IllegalArgumentException" => "java/lang/IllegalArgumentException",
        "NullPointerException" => "java/lang/NullPointerException",
        "IndexOutOfBoundsException" => "java/lang/IndexOutOfBoundsException",
        "UnsupportedOperationException" => "java/lang/UnsupportedOperationException",
        "ArithmeticException" => "java/lang/ArithmeticException",
        "ClassCastException" => "java/lang/ClassCastException",
        "NumberFormatException" => "java/lang/NumberFormatException",
        "AssertionError" => "java/lang/AssertionError",
        _ => return None,
    })
}

/// The target type of a numeric conversion method (`n.toInt()` → `Int`, …).
pub fn conversion_target(name: &str) -> Option<Ty> {
    Some(match name {
        "toInt" => Ty::Int,
        "toByte" => Ty::Byte,
        "toShort" => Ty::Short,
        "toLong" => Ty::Long,
        "toFloat" => Ty::Float,
        "toDouble" => Ty::Double,
        _ => return None,
    })
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

/// Resolve an instance method on `java.lang.StringBuilder` (a curated subset). `append` accepts any
/// primitive/String/reference and returns the builder (chainable); `toString`/`length` as expected.
pub fn resolve_stringbuilder_instance(method: &str, arg_tys: &[Ty]) -> Option<(String, Ty)> {
    let sb = Ty::obj("java/lang/StringBuilder");
    Some(match (method, arg_tys) {
        ("toString", []) => ("()Ljava/lang/String;".to_string(), Ty::String),
        ("length", []) => ("()I".to_string(), Ty::Int),
        ("append", [a]) => {
            let argdesc = match a {
                Ty::Int | Ty::Byte | Ty::Short => "I",
                Ty::Long => "J",
                Ty::Float => "F",
                Ty::Double => "D",
                Ty::Boolean => "Z",
                Ty::Char => "C",
                Ty::String => "Ljava/lang/String;",
                _ => "Ljava/lang/Object;",
            };
            (format!("({argdesc})Ljava/lang/StringBuilder;"), sb)
        }
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

/// Resolve an *instance* method on a classpath Java type by name + exact param descriptors.
/// Returns `(method descriptor, return type)` for `invokevirtual`.
pub fn resolve_java_instance(cp: &Classpath, internal: &str, method: &str, arg_tys: &[Ty]) -> Option<(String, Ty)> {
    let ci = cp.find(internal)?;
    let params: String = arg_tys.iter().map(|t| t.descriptor()).collect();
    let prefix = format!("({params})");
    let m = ci.methods.iter().find(|m| m.name == method && !m.is_static() && m.descriptor.starts_with(&prefix))?;
    let ret = m.descriptor[m.descriptor.find(')').unwrap() + 1..].to_string();
    Some((m.descriptor.clone(), desc_to_ty(&ret)))
}

/// Resolve a constructor on a classpath Java type by argument descriptors. Returns its descriptor.
pub fn resolve_java_ctor(cp: &Classpath, internal: &str, arg_tys: &[Ty]) -> Option<String> {
    let ci = cp.find(internal)?;
    let params: String = arg_tys.iter().map(|t| t.descriptor()).collect();
    let prefix = format!("({params})");
    ci.methods.iter().find(|m| m.name == "<init>" && m.descriptor.starts_with(&prefix)).map(|m| m.descriptor.clone())
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
                    diags.error(c.span, format!("conflicting declarations: {}", c.name));
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
                    let tp: std::collections::HashSet<String> = f.type_params.iter().cloned().collect();
                    // A `vararg` parameter's runtime type is `Array<elem>`.
                    let params: Vec<Ty> = f
                        .params
                        .iter()
                        .map(|p| {
                            let t = ty_of_ref(&p.ty, &class_names, &tp, diags);
                            if p.is_vararg {
                                Ty::array(t)
                            } else {
                                t
                            }
                        })
                        .collect();
                    let ret = match &f.ret {
                        Some(r) => ty_of_ref(r, &class_names, &tp, diags),
                        None => Ty::Unit, // v0: missing return type defaults to Unit
                    };
                    let vararg = f.params.last().map_or(false, |p| p.is_vararg);
                    if table.funs.insert(f.name.clone(), Signature { params, ret, vararg }).is_some() {
                        diags.error(f.span, format!("conflicting declarations: {}", f.name));
                    }
                }
                Decl::Class(c) => {
                    let internal = class_names.get(&c.name).cloned().unwrap_or_else(|| class_internal(file, &c.name));
                    let ctp: std::collections::HashSet<String> = c.type_params.iter().cloned().collect();
                    // All primary-ctor params (in order) define the constructor signature.
                    let ctor_params: Vec<Ty> = c.props.iter().map(|p| ty_of_ref(&p.ty, &class_names, &ctp, diags)).collect();
                    // Only `val`/`var` params (+ body props) are backing-field properties.
                    let mut props: Vec<(String, Ty, bool)> = c
                        .props
                        .iter()
                        .filter(|p| p.is_property)
                        .map(|p| (p.name.clone(), ty_of_ref(&p.ty, &class_names, &ctp, diags), p.is_var))
                        .collect();
                    // Body properties (`class C { val x = … }`) are also fields/accessors.
                    for bp in &c.body_props {
                        let ty = match &bp.ty {
                            Some(r) => ty_of_ref(r, &class_names, &ctp, diags),
                            None => bp.init.map(|i| infer_lit_ty(file, i)).unwrap_or(Ty::Error),
                        };
                        props.push((bp.name.clone(), ty, bp.is_var));
                    }
                    let mut methods: HashMap<String, Signature> = c
                        .methods
                        .iter()
                        .map(|m| {
                            let mut mtp = ctp.clone();
                            mtp.extend(m.type_params.iter().cloned());
                            let params = m.params.iter().map(|p| ty_of_ref(&p.ty, &class_names, &mtp, diags)).collect();
                            let ret = m.ret.as_ref().map(|r| ty_of_ref(r, &class_names, &mtp, diags)).unwrap_or(Ty::Unit);
                            (m.name.clone(), Signature { params, ret, vararg: false })
                        })
                        .collect();
                    // `data class` synthesizes componentN() + copy(props...) callable members.
                    if c.is_data {
                        let self_ty = Ty::obj(&internal);
                        for (i, (_, ty, _)) in props.iter().enumerate() {
                            methods.insert(format!("component{}", i + 1), Signature { params: vec![], ret: *ty, vararg: false });
                        }
                        methods.insert("copy".into(), Signature { params: props.iter().map(|(_, t, _)| *t).collect(), ret: self_ty, vararg: false });
                    }
                    if c.is_object {
                        table.objects.insert(c.name.clone());
                    }
                    if c.is_enum {
                        table.enums.insert(c.name.clone(), c.enum_entries.clone());
                    }
                    // Implemented interfaces, resolved to internal names (for subtyping/dispatch).
                    let interfaces: Vec<String> = c
                        .supertypes
                        .iter()
                        .map(|s| class_names.get(s).cloned().unwrap_or_else(|| s.clone()))
                        .collect();
                    let super_internal = c.base_class.as_ref().map(|b| class_names.get(b).cloned().unwrap_or_else(|| b.clone()));
                    // `companion object` members → static methods/props on this class.
                    let static_methods: HashMap<String, Signature> = c
                        .companion_methods
                        .iter()
                        .map(|m| {
                            let mut mtp = ctp.clone();
                            mtp.extend(m.type_params.iter().cloned());
                            let params = m.params.iter().map(|p| ty_of_ref(&p.ty, &class_names, &mtp, diags)).collect();
                            let ret = m.ret.as_ref().map(|r| ty_of_ref(r, &class_names, &mtp, diags)).unwrap_or(Ty::Unit);
                            (m.name.clone(), Signature { params, ret, vararg: false })
                        })
                        .collect();
                    let static_props: HashMap<String, Ty> = c
                        .companion_props
                        .iter()
                        .map(|p| {
                            let ty = match &p.ty {
                                Some(r) => ty_of_ref(r, &class_names, &ctp, diags),
                                None => p.init.map(|i| infer_lit_ty(file, i)).unwrap_or(Ty::Error),
                            };
                            (p.name.clone(), ty)
                        })
                        .collect();
                    let lateinit_props: std::collections::HashSet<String> = c
                        .body_props
                        .iter()
                        .chain(c.companion_props.iter())
                        .filter(|p| p.is_lateinit)
                        .map(|p| p.name.clone())
                        .collect();
                    table.classes.insert(
                        c.name.clone(),
                        ClassSig { internal, props, ctor_params, methods, is_interface: c.is_interface, is_sealed: c.is_sealed, static_methods, static_props, lateinit_props, interfaces, super_internal },
                    );
                }
                Decl::Property(p) => {
                    // Type from the annotation, else a light inference from a literal initializer.
                    let ty = match &p.ty {
                        Some(r) => ty_of_ref(r, &class_names, &Default::default(), diags),
                        None => p.init.map(|i| infer_lit_ty(file, i)).unwrap_or(Ty::Error),
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

/// Resolve a syntactic type reference to a `Ty`: a primitive/String/Unit, a declared class
/// (→ `Ty::Obj`), or a generic type parameter (erased to `java/lang/Object`).
fn ty_of_ref(r: &TypeRef, classes: &HashMap<String, String>, tparams: &std::collections::HashSet<String>, diags: &mut DiagSink) -> Ty {
    let base = if let Some(t) = Ty::from_name(&r.name) {
        t
    } else if let Some(elem) = Ty::primitive_array_element(&r.name) {
        Ty::array(elem)
    } else if r.name == "Array" {
        match &r.arg {
            Some(a) => {
                let e = ty_of_ref(a, classes, tparams, diags);
                if e.is_reference() {
                    Ty::array(e)
                } else {
                    diags.error(r.span, "krusty: Array of a primitive (use IntArray/…) is not supported".to_string());
                    Ty::Error
                }
            }
            None => {
                diags.error(r.span, "krusty: a raw Array type (no element) is not supported".to_string());
                Ty::Error
            }
        }
    } else if tparams.contains(&r.name) {
        Ty::obj("java/lang/Object") // erased generic type parameter
    } else if let Some(internal) = classes.get(&r.name) {
        Ty::obj(internal)
    } else {
        diags.error(r.span, format!("unresolved reference: {}", r.name));
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
        tparams: Default::default(),
        this_ty: None,
        companion_of: None,
    };
    // Top-level functions that erase to the same JVM signature collide in the facade class.
    let top_funs: Vec<&FunDecl> = file
        .decls
        .iter()
        .filter_map(|&d| if let Decl::Fun(f) = file.decl(d) { Some(f) } else { None })
        .collect();
    c.check_no_erased_clash(&top_funs);

    for &d in &file.decls {
        match file.decl(d) {
            Decl::Fun(f) => {
                c.tparams = f.type_params.iter().cloned().collect();
                c.check_fun(f);
                c.tparams.clear();
            }
            Decl::Class(cl) => {
                // Class type parameters are in scope for all members.
                c.tparams = cl.type_params.iter().cloned().collect();
                // Member functions are checked with the class's properties (resolved in Stage C)
                // visible as an implicit `this` scope.
                let props = syms.classes.get(&cl.name).map(|s| s.props.clone()).unwrap_or_default();
                c.this_ty = syms.classes.get(&cl.name).map(|s| Ty::obj(&s.internal));
                let methods: Vec<&FunDecl> = cl.methods.iter().collect();
                c.check_no_erased_clash(&methods);
                if let Some(internal) = syms.classes.get(&cl.name).map(|s| s.internal.clone()) {
                    c.check_no_bridge_needed(&internal, cl.span);
                    // A `data class` implementing an interface that declares `copy`/`componentN` would
                    // need bridges for its *synthesized* members (which return the class itself, not
                    // the supertype) — krusty doesn't emit those, so reject (cleanly skip).
                    if cl.is_data {
                        let supers = syms.supertype_methods(&internal);
                        if let Some((sn, _)) = supers.iter().find(|(sn, _)| sn == "copy" || sn.starts_with("component")) {
                            c.diags.error(cl.span, format!("krusty: data class overriding synthesized member '{sn}' needs a bridge method (unsupported)"));
                        }
                    }
                }
                for m in &cl.methods {
                    c.check_method(m, &props);
                }
                // Body-property initializers and `init` blocks see the properties (implicit `this`)
                // and the primary-constructor parameters (including non-property ones).
                c.push_scope();
                for (n, t, is_var) in &props {
                    c.declare(n, *t, *is_var);
                }
                for p in &cl.props {
                    let ty = c.resolve_ty(&p.ty);
                    c.declare(&p.name, ty, p.is_var);
                }
                for bp in &cl.body_props {
                    if let Some(init) = bp.init {
                        let it = c.expr(init);
                        if let Some(r) = &bp.ty {
                            let declared = c.resolve_ty(r);
                            c.expect_assignable(declared, it, c.span(init), "property initializer");
                        }
                    }
                    // A computed property's getter body is checked like a method returning the
                    // property type (the implicit-`this` scope of props is already active here).
                    if let Some(getter) = &bp.getter {
                        let prev_ret = c.ret_ty;
                        c.ret_ty = bp.ty.as_ref().map(|r| c.resolve_ty(r)).unwrap_or(Ty::Error);
                        match getter {
                            FunBody::Expr(g) => {
                                let gt = c.expr(*g);
                                c.expect_assignable(c.ret_ty, gt, c.span(*g), "getter body");
                            }
                            FunBody::Block(g) => {
                                let _ = c.expr(*g);
                            }
                            FunBody::None => {}
                        }
                        c.ret_ty = prev_ret;
                    }
                }
                for step in &cl.init_order {
                    if let ClassInit::Block(b) = step {
                        c.expr(*b);
                    }
                }
                c.pop_scope();
                c.this_ty = None;
                // `companion object` members are checked statically, with companion props/methods in
                // scope unqualified.
                if !cl.companion_methods.is_empty() || !cl.companion_props.is_empty() {
                    // krusty emits companion members as statics on the same class, so a companion
                    // member whose name collides with an instance member would duplicate a field/
                    // method (kotlinc separates them via a nested Companion class). Reject (skip).
                    let inst_names: std::collections::HashSet<&str> = cl
                        .props
                        .iter()
                        .filter(|p| p.is_property)
                        .map(|p| p.name.as_str())
                        .chain(cl.body_props.iter().map(|p| p.name.as_str()))
                        .chain(cl.methods.iter().map(|m| m.name.as_str()))
                        .collect();
                    for cp in &cl.companion_props {
                        if inst_names.contains(cp.name.as_str()) {
                            c.diags.error(cl.span, format!("krusty: companion member '{}' collides with an instance member (unsupported)", cp.name));
                        }
                    }
                    for cm in &cl.companion_methods {
                        if inst_names.contains(cm.name.as_str()) {
                            c.diags.error(cl.span, format!("krusty: companion member '{}' collides with an instance member (unsupported)", cm.name));
                        }
                    }
                    c.companion_of = Some(cl.name.clone());
                    for p in &cl.companion_props {
                        if let Some(init) = p.init {
                            let it = c.expr(init);
                            if let Some(r) = &p.ty {
                                let declared = c.resolve_ty(r);
                                c.expect_assignable(declared, it, c.span(init), "companion property");
                            }
                        }
                    }
                    for m in &cl.companion_methods {
                        c.check_fun(m);
                    }
                    c.companion_of = None;
                }
                c.tparams.clear();
            }
            Decl::Property(p) => {
                // A top-level computed property (custom getter) isn't supported — the facade emits a
                // backing field, not the getter, so reject to avoid a miscompile.
                if p.getter.is_some() {
                    c.diags.error(p.span, "krusty: top-level computed properties (custom getter) are not supported".to_string());
                }
                if let Some(init) = p.init {
                    let it = c.expr(init);
                    if let Some((declared, _)) = syms.props.get(&p.name).copied().filter(|(t, _)| *t != Ty::Error) {
                        if p.ty.is_some() {
                            c.expect_assignable(declared, it, c.span(init), "property initializer");
                        }
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
    /// Generic type parameters in scope (erased to `java/lang/Object`).
    tparams: std::collections::HashSet<String>,
    /// The type of `this` when checking class members (`None` at top level).
    this_ty: Option<Ty>,
    /// When checking a `companion object` member, the enclosing class name — its companion
    /// methods/properties are then in scope unqualified.
    companion_of: Option<String>,
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
    /// Nullability doesn't change the `Ty` for reference types (same JVM descriptor), but a nullable
    /// *primitive* (`Char?`, `Int?`, …) would need boxing — rejected (the file is skipped).
    fn resolve_ty(&mut self, r: &TypeRef) -> Ty {
        let base = if let Some(t) = Ty::from_name(&r.name) {
            t
        } else if let Some(elem) = Ty::primitive_array_element(&r.name) {
            Ty::array(elem)
        } else if r.name == "Array" {
            match &r.arg {
                Some(a) => {
                    let e = self.resolve_ty(a);
                    if e.is_reference() {
                        Ty::array(e)
                    } else {
                        Ty::Error
                    }
                }
                None => Ty::Error,
            }
        } else if self.tparams.contains(&r.name) {
            Ty::obj("java/lang/Object") // erased generic type parameter
        } else if let Some(cs) = self.syms.classes.get(&r.name) {
            Ty::obj(&cs.internal)
        } else {
            Ty::Error
        };
        if r.nullable && !base.is_reference() && base != Ty::Error {
            self.diags.error(r.span, format!("nullable primitive type '{}?' is not supported", r.name));
            return Ty::Error;
        }
        base
    }

    /// The erased JVM signature key (`name(paramDescs)`) of a function, using the type parameters
    /// currently in `self.tparams` plus the function's own. Two functions that produce the same key
    /// collide on the JVM after erasure — kotlinc erases each type parameter to its declared *bound*
    /// (keeping bound-distinct overloads separate), which krusty does not model, so we reject the
    /// file rather than emit a `ClassFormatError`-inducing duplicate method.
    fn erased_sig_key(&self, f: &FunDecl) -> String {
        let (params, _) = self.erased_param_ret(f);
        format!("{}({})", f.name, params)
    }

    /// The erased JVM parameter descriptors (concatenated) and return descriptor of a function,
    /// using the type parameters in scope plus the function's own (each → `Object`).
    fn erased_param_ret(&self, f: &FunDecl) -> (String, String) {
        let extra: std::collections::HashSet<&str> = f.type_params.iter().map(|s| s.as_str()).collect();
        let descr = |name: &str| -> String {
            if let Some(t) = Ty::from_name(name) {
                t.descriptor()
            } else if self.tparams.contains(name) || extra.contains(name) {
                "Ljava/lang/Object;".to_string()
            } else if let Some(cs) = self.syms.classes.get(name) {
                Ty::obj(&cs.internal).descriptor()
            } else {
                "Ljava/lang/Object;".to_string()
            }
        };
        let params: String = f.params.iter().map(|p| descr(&p.ty.name)).collect();
        let ret = f.ret.as_ref().map(|r| descr(&r.name)).unwrap_or_else(|| "V".to_string());
        (params, ret)
    }

    /// Reject classes whose *effective* implementation of a supertype method has the same erased
    /// parameters but a different return descriptor (covariant or generic return) — including
    /// *fake overrides*, where the implementation is inherited from a base class while the differing
    /// signature comes from an interface. The JVM resolves such a call via the supertype's descriptor
    /// and would need a synthetic bridge method, which krusty does not emit — so the file is cleanly
    /// skipped rather than throwing `AbstractMethodError` at runtime.
    fn check_no_bridge_needed(&mut self, internal: &str, span: Span) {
        let supers = self.syms.supertype_methods(internal);
        for (name, ssig) in &supers {
            let Some(impl_sig) = self.syms.method_of(internal, name) else { continue };
            let sp: String = ssig.params.iter().map(|t| t.descriptor()).collect();
            let ip: String = impl_sig.params.iter().map(|t| t.descriptor()).collect();
            if sp == ip && ssig.ret.descriptor() != impl_sig.ret.descriptor() {
                self.diags.error(
                    span,
                    format!("krusty: method '{name}' needs a bridge method (covariant/generic return override is not supported)"),
                );
                return;
            }
        }
        // Property getters need the same check: a supertype property whose (erased) type differs from
        // the class's own property type (e.g. a generic interface `val x: T` → `Object`, overridden
        // with a concrete type) would need a bridge `getX`, which krusty does not synthesize.
        for sup in self.syms.supertype_internals(internal) {
            let Some(sc) = self.syms.class_by_internal(&sup) else { continue };
            for (pname, sty, _) in sc.props.clone() {
                if let Some((own_ty, _)) = self.syms.prop_of(internal, &pname) {
                    if sty.descriptor() != own_ty.descriptor() {
                        self.diags.error(
                            span,
                            format!("krusty: property '{pname}' needs a bridge getter (covariant/generic override is not supported)"),
                        );
                        return;
                    }
                }
            }
        }
    }

    /// Report (and thereby skip the file for) functions whose erased signatures collide.
    fn check_no_erased_clash(&mut self, funs: &[&FunDecl]) {
        let mut seen: HashMap<String, Span> = HashMap::new();
        for f in funs {
            let key = self.erased_sig_key(f);
            if seen.contains_key(&key) {
                self.diags.error(
                    f.span,
                    format!("conflicting overloads: function '{}' has the same JVM signature as another after type erasure", f.name),
                );
            } else {
                seen.insert(key, f.span);
            }
        }
    }

    /// True if a subject `when` is exhaustive because its subject is a `sealed` class and every
    /// declared subclass is matched by a positive `is` arm. Conservative: anything it can't prove
    /// (non-sealed subject, an uncovered subclass, a nested sealed subclass) returns false.
    fn when_sealed_exhaustive(&self, subj_ty: Option<Ty>, arms: &[WhenArm]) -> bool {
        let Some(Ty::Obj(internal)) = subj_ty else { return false };
        let Some(cs) = self.syms.class_by_internal(internal) else { return false };
        if !cs.is_sealed {
            return false;
        }
        let subs = self.syms.subclasses_of(internal);
        if subs.is_empty() {
            return false;
        }
        let mut covered: std::collections::HashSet<String> = std::collections::HashSet::new();
        for arm in arms {
            for &c in &arm.conditions {
                if let Expr::Is { ty, negated: false, .. } = self.file.expr(c) {
                    if let Ty::Obj(n) = self.resolve_ty_no_diag(ty) {
                        covered.insert(n.to_string());
                    }
                }
            }
        }
        subs.iter().all(|d| covered.contains(d))
    }

    /// True if evaluating `e` always transfers control away (a `return`, or a block/if whose every
    /// exit does). Used to detect early-return guards for smart-casting the rest of a block.
    fn expr_diverges(&self, e: ExprId) -> bool {
        match self.file.expr(e) {
            Expr::Throw { .. } => true,
            Expr::Block { stmts, trailing } => {
                if let Some(te) = trailing {
                    self.expr_diverges(*te)
                } else if let Some(&last) = stmts.last() {
                    matches!(self.file.stmt(last), Stmt::Return(_))
                } else {
                    false
                }
            }
            Expr::If { then_branch, else_branch: Some(eb), .. } => {
                self.expr_diverges(*then_branch) && self.expr_diverges(*eb)
            }
            _ => false,
        }
    }

    /// The JVM internal name of a `catch` clause's exception type: a common JDK exception, an
    /// imported class, or a declared class. `None` if krusty can't resolve it to a concrete class.
    fn catch_internal(&self, name: &str) -> Option<String> {
        builtin_exception(name)
            .map(|s| s.to_string())
            .or_else(|| self.imports.get(name).cloned())
            .or_else(|| self.syms.classes.get(name).map(|c| c.internal.clone()))
    }

    /// Resolve a type without emitting diagnostics (used for speculative smart-cast narrowing).
    fn resolve_ty_no_diag(&self, r: &TypeRef) -> Ty {
        if let Some(t) = Ty::from_name(&r.name) {
            t
        } else if self.tparams.contains(&r.name) {
            Ty::obj("java/lang/Object")
        } else if let Some(cs) = self.syms.classes.get(&r.name) {
            Ty::obj(&cs.internal)
        } else {
            Ty::Error
        }
    }

    /// If `cond` is `x is T` (or `x !is T` when `for_else`) and `x` is a stable local/parameter and
    /// `T` a non-nullable known reference type, return the smart-cast binding `(x, T)`.
    fn smartcast_binding(&self, cond: ExprId, for_else: bool) -> Option<(String, Ty)> {
        let Expr::Is { operand, ty, negated } = self.file.expr(cond).clone() else { return None };
        // The then-branch narrows on a positive `is`; the else-branch on a negative `!is`.
        if negated != for_else {
            return None;
        }
        if ty.nullable {
            return None;
        }
        let Expr::Name(n) = self.file.expr(operand).clone() else { return None };
        // Only stable values (val/parameter) smart-cast soundly — a `var` could be reassigned.
        if matches!(self.lookup(&n), Some(l) if l.is_var) {
            return None;
        }
        let tt = self.resolve_ty_no_diag(&ty);
        if tt.is_reference() {
            Some((n, tt))
        } else {
            None
        }
    }

    fn check_fun(&mut self, f: &FunDecl) {
        // Use the collected signature's return type; for a companion method (not in `funs`) fall back
        // to the declared return type.
        self.ret_ty = match self.syms.funs.get(&f.name).map(|s| s.ret) {
            Some(r) => r,
            None => f.ret.as_ref().map(|r| self.resolve_ty(r)).unwrap_or(Ty::Unit),
        };
        self.push_scope();
        for p in &f.params {
            let ty = self.resolve_ty(&p.ty);
            let ty = if p.is_vararg { Ty::array(ty) } else { ty };
            self.declare(&p.name, ty, false);
        }
        self.check_fun_body(f);
        self.pop_scope();
    }

    /// Check an instance method: the class properties are visible (implicit `this`), then the
    /// method's own parameters shadow them.
    fn check_method(&mut self, f: &FunDecl, props: &[(String, Ty, bool)]) {
        let added: Vec<String> = f.type_params.iter().filter(|t| self.tparams.insert((*t).clone())).cloned().collect();
        self.ret_ty = f.ret.as_ref().map(|r| self.resolve_ty(r)).unwrap_or(Ty::Unit);
        self.push_scope(); // implicit-this scope (properties)
        for (n, t, is_var) in props {
            self.declare(n, *t, *is_var);
        }
        self.push_scope(); // parameter scope
        for p in &f.params {
            let ty = self.resolve_ty(&p.ty);
            let ty = if p.is_vararg { Ty::array(ty) } else { ty };
            self.declare(&p.name, ty, false);
        }
        self.check_fun_body(f);
        self.pop_scope();
        self.pop_scope();
        for t in added {
            self.tparams.remove(&t);
        }
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

    /// Is `sub` a subtype of `sup`? Reflexive, through implemented interfaces, and up the base-class
    /// chain.
    fn obj_is_subtype(&self, sub: &str, sup: &str) -> bool {
        if sub == sup {
            return true;
        }
        if let Some(c) = self.syms.class_by_internal(sub) {
            if c.interfaces.iter().any(|i| i == sup) {
                return true;
            }
            if let Some(s) = &c.super_internal {
                return self.obj_is_subtype(s, sup);
            }
        }
        false
    }

    /// Resolve a method (own or inherited from the base-class chain) on a class internal name.
    fn lookup_method(&self, internal: &str, name: &str) -> Option<Signature> {
        let c = self.syms.class_by_internal(internal)?;
        if let Some(sig) = c.methods.get(name) {
            return Some(sig.clone());
        }
        let s = c.super_internal.clone()?;
        self.lookup_method(&s, name)
    }

    /// Resolve a property (own or inherited) on a class internal name.
    fn lookup_prop(&self, internal: &str, name: &str) -> Option<(Ty, bool)> {
        let c = self.syms.class_by_internal(internal)?;
        if let Some(p) = c.prop(name) {
            return Some(p);
        }
        let s = c.super_internal.clone()?;
        self.lookup_prop(&s, name)
    }

    fn expect_assignable(&mut self, expected: Ty, actual: Ty, span: Span, ctx: &str) {
        if expected == Ty::Error || actual == Ty::Error {
            return;
        }
        // `Nothing` (a `throw`) is the bottom type: assignable to anything.
        if actual == Ty::Nothing {
            return;
        }
        // `null` is assignable to any reference type (krusty is permissive about nullability).
        if actual == Ty::Null && expected.is_reference() {
            return;
        }
        // An `Int` (typically a constant) is assignable to `Byte`/`Short` (Kotlin narrows integer
        // literals); codegen emits `i2b`/`i2s`. `Byte`/`Short` are interchangeable with `Int` here.
        if matches!(expected, Ty::Byte | Ty::Short) && matches!(actual, Ty::Int | Ty::Byte | Ty::Short) {
            return;
        }
        // Any reference type is assignable to `Any`/`Object` (e.g. an erased generic parameter).
        if matches!(expected, Ty::Obj("java/lang/Object")) && actual.is_reference() {
            return;
        }
        // A class value is assignable to an interface (supertype) it implements.
        if let (Ty::Obj(e), Ty::Obj(a)) = (expected, actual) {
            if self.obj_is_subtype(a, e) {
                return;
            }
        }
        if expected != actual {
            let _ = ctx;
            self.diags.error(span, format!("type mismatch: inferred type is {} but {} was expected", actual.name(), expected.name()));
        }
    }

    fn expr(&mut self, e: ExprId) -> Ty {
        let t = match self.file.expr(e).clone() {
            Expr::IntLit(_) => Ty::Int,
            Expr::LongLit(_) => Ty::Long,
            Expr::DoubleLit(_) => Ty::Double,
            Expr::FloatLit(_) => Ty::Float,
            Expr::BoolLit(_) => Ty::Boolean,
            Expr::StringLit(_) => Ty::String,
            Expr::CharLit(_) => Ty::Char,
            Expr::NullLit => Ty::Null,
            Expr::NotNull { operand } => self.expr(operand), // value with the same (non-null) type
            Expr::Throw { operand } => {
                self.expr(operand); // any reference (a Throwable) — krusty doesn't model the hierarchy
                Ty::Nothing
            }
            Expr::Index { array, index } => {
                let at = self.expr(array);
                let it = self.expr(index);
                self.expect_assignable(Ty::Int, it, self.span(index), "array index");
                match at.array_elem() {
                    Some(elem) => elem,
                    None => {
                        if at != Ty::Error {
                            self.diags.error(self.span(e), format!("'{}' is not an array (cannot index)", at.name()));
                        }
                        Ty::Error
                    }
                }
            }
            Expr::Try { body, catches, finally } => {
                let bt = self.expr(body);
                if let Some(f) = finally {
                    self.expr(f); // finally runs for effect; its value is discarded
                }
                let mut result = bt;
                for c in &catches {
                    let cty = match self.catch_internal(&c.ty.name) {
                        Some(i) => Ty::obj(&i),
                        None => {
                            self.diags.error(c.ty.span, "krusty: catch type is not a known exception class".to_string());
                            Ty::Error
                        }
                    };
                    self.push_scope();
                    self.declare(&c.name, cty, false);
                    let ht = self.expr(c.body);
                    self.pop_scope();
                    // A `try` used as a statement needn't have body/catch agree; merge leniently
                    // (mismatch → `Unit`) so only an expression use that needs a value is constrained.
                    result = if result == ht {
                        result
                    } else if result == Ty::Nothing {
                        ht
                    } else if ht == Ty::Nothing {
                        result
                    } else {
                        Ty::Unit
                    };
                }
                result
            }
            Expr::Is { operand, ty, negated: _ } => {
                let ot = self.expr(operand);
                let tt = self.resolve_ty(&ty);
                // `instanceof` needs a reference operand and a *known* reference target. An unresolved
                // target (`Number`, a value class, `Nothing`, …) must not silently become `Object`
                // (which would make the test always true) — reject so the file is cleanly skipped.
                // A *nullable* target (`x is T?`) is also rejected: `null is T?` is true, but plain
                // `instanceof` yields false, so it would miscompile.
                if !tt.is_reference() || ty.nullable || (!ot.is_reference() && ot != Ty::Error) {
                    self.diags.error(self.span(e), "krusty: 'is' on this type is not supported".to_string());
                    return Ty::Error;
                }
                Ty::Boolean
            }
            Expr::As { operand, ty, nullable: _ } => {
                let ot = self.expr(operand);
                let tt = self.resolve_ty(&ty);
                // `checkcast` needs a reference operand and a *known* reference target (an unresolved
                // target would erase to `Object`, a no-op cast — reject instead of miscompiling).
                if !tt.is_reference() || (!ot.is_reference() && ot != Ty::Error) {
                    self.diags.error(self.span(e), "krusty: 'as' with this type is not supported".to_string());
                    return Ty::Error;
                }
                tt
            }
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
            Expr::Template(parts) => {
                for p in &parts {
                    if let TemplatePart::Expr(pe) = p {
                        self.expr(*pe);
                    }
                }
                Ty::String
            }
            Expr::SafeCall { receiver, name, args } => {
                let rt = self.expr(receiver);
                if rt == Ty::Error {
                    return Ty::Error;
                }
                let result = match &args {
                    None => self.check_member(rt, &name, self.span(e)),
                    Some(a) => {
                        let arg_tys: Vec<Ty> = a.iter().map(|x| self.expr(*x)).collect();
                        if let ("toString", []) = (name.as_str(), arg_tys.as_slice()) {
                            Ty::String
                        } else if rt == Ty::String {
                            resolve_string_instance(&name, &arg_tys).map(|(_, r)| r).unwrap_or(Ty::Error)
                        } else if let Ty::Obj(internal) = rt {
                            self.lookup_method(internal, &name).map(|s| s.ret)
                                .or_else(|| resolve_java_instance(&self.syms.classpath, internal, &name, &arg_tys).map(|(_, r)| r))
                                .unwrap_or(Ty::Error)
                        } else {
                            Ty::Error
                        }
                    }
                };
                // The safe-call result is nullable; krusty needs it to be a reference type (no boxing).
                if !result.is_reference() && result != Ty::Error {
                    self.diags.error(self.span(e), "krusty: safe call (?.) with a non-reference result is not supported".to_string());
                    return Ty::Error;
                }
                result
            }
            Expr::Name(n) if n == "this" => match self.this_ty {
                Some(t) => t,
                None => {
                    self.diags.error(self.span(e), "'this' is not available outside a class member".to_string());
                    Ty::Error
                }
            },
            Expr::Name(n) => match self.lookup(&n) {
                Some(l) => l.ty,
                None => {
                    // Unqualified companion property inside a companion member.
                    if let Some(cls) = &self.companion_of {
                        if let Some(&ty) = self.syms.classes.get(cls).and_then(|c| c.static_props.get(&n)) {
                            return self.set(e, ty);
                        }
                        // A top-level property accessed from a companion member would target the wrong
                        // class in codegen (the facade, not this class) — reject (skip).
                        if self.syms.props.contains_key(&n) {
                            self.diags.error(self.span(e), "krusty: top-level property access from a companion member is not supported".to_string());
                            return self.set(e, Ty::Error);
                        }
                    }
                    match self.syms.props.get(&n) {
                        Some(&(ty, _)) => ty, // top-level property
                        None => {
                            self.diags.error(self.span(e), format!("unresolved reference: {n}"));
                            Ty::Error
                        }
                    }
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
                // `EnumName.ENTRY` — a static enum entry access (receiver is the enum type name).
                if let Expr::Name(en) = self.file.expr(receiver).clone() {
                    if self.lookup(&en).is_none() {
                        if let Some(entries) = self.syms.enums.get(&en) {
                            if entries.iter().any(|e| e == &name) {
                                let internal = self.syms.classes.get(&en).map(|c| c.internal.clone()).unwrap_or(en.clone());
                                return self.set(e, Ty::obj(&internal));
                            }
                        }
                        // `ClassName.PROP` — a companion (static) property read.
                        if let Some(cs) = self.syms.classes.get(&en) {
                            if let Some(&ty) = cs.static_props.get(&name) {
                                return self.set(e, ty);
                            }
                        }
                        // `ObjectName.prop` — a property on a singleton `object`.
                        if self.syms.objects.contains(&en) {
                            if let Some((ty, _)) = self.syms.classes.get(&en).and_then(|c| c.prop(&name)) {
                                return self.set(e, ty);
                            }
                        }
                    }
                }
                let rt = self.expr(receiver);
                self.check_member(rt, &name, self.span(e))
            }
            Expr::Call { callee, args } => self.check_call(callee, &args, self.span(e)),
            Expr::If { cond, then_branch, else_branch } => {
                let ct = self.expr(cond);
                self.expect_assignable(Ty::Boolean, ct, self.span(cond), "if condition");
                // Smart-cast: `if (x is T)` narrows a stable `x` to `T` in the then-branch (and
                // `if (x !is T) … else` narrows it in the else-branch).
                let then_cast = self.smartcast_binding(cond, false);
                self.push_scope();
                if let Some((n, t)) = &then_cast {
                    self.declare(n, *t, false);
                }
                let tt = self.expr(then_branch);
                self.pop_scope();
                match else_branch {
                    Some(eb) => {
                        let else_cast = self.smartcast_binding(cond, true);
                        self.push_scope();
                        if let Some((n, t)) = &else_cast {
                            self.declare(n, *t, false);
                        }
                        let et = self.expr(eb);
                        self.pop_scope();
                        self.join(tt, et, self.span(e))
                    }
                    None => Ty::Unit,
                }
            }
            Expr::Block { stmts, trailing } => {
                self.push_scope();
                for s in &stmts {
                    self.stmt(*s);
                    // Early-return guard: `if (x !is T) return …` (a diverging then, no else) narrows
                    // a stable `x` to `T` for the remaining statements of this block.
                    if let Stmt::Expr(ie) = self.file.stmt(*s).clone() {
                        if let Expr::If { cond, then_branch, else_branch: None } = self.file.expr(ie).clone() {
                            if self.expr_diverges(then_branch) {
                                if let Some((n, t)) = self.smartcast_binding(cond, true) {
                                    self.declare(&n, t, false);
                                }
                            }
                        }
                    }
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
                        let is_type_test = matches!(self.file.expr(cnd), Expr::Is { .. });
                        let ct = self.expr(cnd);
                        match subj_ty {
                            // A type-test arm (`is T`) compares by `instanceof`, not `==` — no
                            // comparability constraint (it already validated its own operand/target).
                            _ if is_type_test => {}
                            // subject form: condition must be comparable to the subject
                            Some(st) if st != Ty::Error && ct != Ty::Error && st != ct && Ty::promote(st, ct).is_none() => {
                                self.diags.error(self.span(cnd), format!("when condition type '{}' is not comparable to subject '{}'", ct.name(), st.name()));
                            }
                            // subjectless form: condition must be Boolean
                            None => self.expect_assignable(Ty::Boolean, ct, self.span(cnd), "when condition"),
                            _ => {}
                        }
                    }
                    // Smart-cast the body of a single positive `is T` arm (subject is a stable name).
                    let arm_cast = match arm.conditions.as_slice() {
                        [cnd] => self.smartcast_binding(*cnd, false),
                        _ => None,
                    };
                    self.push_scope();
                    if let Some((n, t)) = &arm_cast {
                        self.declare(n, *t, false);
                    }
                    let bt = self.expr(arm.body);
                    self.pop_scope();
                    result = Some(match result {
                        Some(r) => self.join(r, bt, self.span(arm.body)),
                        None => bt,
                    });
                }
                // A `when` carries a value only when it is exhaustive: it has an `else`, or its
                // subject is a `sealed` type whose every subclass is matched by an `is` arm.
                let exhaustive = has_else || self.when_sealed_exhaustive(subj_ty, &arms);
                if exhaustive {
                    result.unwrap_or(Ty::Unit)
                } else {
                    Ty::Unit
                }
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
                if Ty::promote(lt, rt).is_some() || (lt == Ty::Char && rt == Ty::Char) {
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

    /// Recognize array-creation builtins: `intArrayOf(…)`/`charArrayOf(…)`/… and `arrayOf(…)`
    /// (element = the common reference type of the arguments), and the size constructors
    /// `IntArray(n)`/`CharArray(n)`/… Returns the array `Ty`, or `None` if `fname` isn't one of these.
    fn check_array_builtin(&mut self, fname: &str, args: &[ExprId], arg_tys: &[Ty], span: Span) -> Option<Ty> {
        let primitive_of = |f: &str| match f {
            "intArrayOf" => Some(Ty::Int),
            "longArrayOf" => Some(Ty::Long),
            "doubleArrayOf" => Some(Ty::Double),
            "booleanArrayOf" => Some(Ty::Boolean),
            "charArrayOf" => Some(Ty::Char),
            _ => None,
        };
        if let Some(elem) = primitive_of(fname) {
            for (i, t) in arg_tys.iter().enumerate() {
                self.expect_assignable(elem, *t, self.span(args[i]), "array element");
            }
            return Some(Ty::array(elem));
        }
        if fname == "arrayOf" {
            // The element type is the common type of the arguments; it must be a reference type
            // (an array of a primitive would require boxing — use `intArrayOf` etc.).
            let mut elem: Option<Ty> = None;
            for &t in arg_tys {
                elem = Some(match elem {
                    Some(prev) => self.join(prev, t, span),
                    None => t,
                });
            }
            match elem {
                Some(e) if e.is_reference() => return Some(Ty::array(e)),
                Some(_) => {
                    self.diags.error(span, "krusty: arrayOf of a primitive (use intArrayOf/…) is not supported".to_string());
                    return Some(Ty::Error);
                }
                None => {
                    self.diags.error(span, "krusty: empty arrayOf() needs an explicit type (unsupported)".to_string());
                    return Some(Ty::Error);
                }
            }
        }
        // Size constructor: `IntArray(n)` etc.
        if let Some(elem) = Ty::primitive_array_element(fname) {
            if arg_tys.len() == 1 {
                self.expect_assignable(Ty::Int, arg_tys[0], self.span(args[0]), "array size");
                return Some(Ty::array(elem));
            }
        }
        None
    }

    /// Recognize stdlib precondition intrinsics: `require`/`check`/`assert(cond)` (→ `Unit`),
    /// `error(msg)` (→ `Nothing`), and `TODO()`/`TODO(msg)` (→ `Nothing`). Returns the result type,
    /// or `None` if `fname` isn't one of these.
    fn check_precondition_intrinsic(&mut self, fname: &str, args: &[ExprId], arg_tys: &[Ty], span: Span) -> Option<Ty> {
        if self.syms.funs.contains_key(fname) {
            return None; // a user-defined function of the same name takes precedence
        }
        match (fname, arg_tys) {
            ("require" | "check" | "assert", [cond]) => {
                self.expect_assignable(Ty::Boolean, *cond, self.span(args[0]), "condition");
                Some(Ty::Unit)
            }
            ("error", [_]) => Some(Ty::Nothing),
            ("TODO", []) => Some(Ty::Nothing),
            ("TODO", [_]) => Some(Ty::Nothing),
            ("require" | "check" | "assert" | "error" | "TODO", _) => {
                self.diags.error(span, format!("krusty: unsupported form of '{fname}'"));
                Some(Ty::Error)
            }
            _ => None,
        }
    }

    fn check_member(&mut self, rt: Ty, name: &str, span: Span) -> Ty {
        if rt == Ty::Error {
            return Ty::Error;
        }
        if let (Ty::String, "length") = (rt, name) {
            return Ty::Int;
        }
        if let (Ty::Array(_), "size") = (rt, name) {
            return Ty::Int;
        }
        if rt == Ty::obj("java/lang/StringBuilder") && name == "length" {
            return Ty::Int; // `sb.length` property → length()
        }
        // Property read on a class value: `p.prop` (own or inherited).
        if let Ty::Obj(internal) = rt {
            if let Some((ty, _)) = self.lookup_prop(internal, name) {
                return ty;
            }
            // `java.lang.Enum` members (`name`, `ordinal`) available on any enum value.
            let is_enum_val = self.syms.enums.keys().any(|en| self.syms.classes.get(en).map_or(false, |c| c.internal == internal));
            if is_enum_val {
                match name {
                    "name" => return Ty::String,
                    "ordinal" => return Ty::Int,
                    _ => {}
                }
            }
        }
        self.diags.error(span, format!("unresolved member '{name}' on '{}'", rt.name()));
        Ty::Error
    }

    fn check_call(&mut self, callee: ExprId, args: &[ExprId], span: Span) -> Ty {
        match self.file.expr(callee).clone() {
            // method call: recv.method(args)
            Expr::Member { receiver, name } => {
                // `super.method(args)` — dispatch to the base class's method (non-virtual).
                if matches!(self.file.expr(receiver), Expr::Name(r) if r == "super") {
                    let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                    if let Some(Ty::Obj(internal)) = self.this_ty {
                        let sup = self.syms.class_by_internal(internal).and_then(|c| c.super_internal.clone());
                        if let Some(sup) = sup {
                            if let Some(sig) = self.syms.method_of(&sup, &name) {
                                for (i, (p, a)) in sig.params.iter().zip(&arg_tys).enumerate() {
                                    self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                                }
                                return sig.ret;
                            }
                        }
                    }
                    self.diags.error(span, format!("krusty: unresolved super method '{name}'"));
                    return Ty::Error;
                }
                // Java static call: `ClassName.method(args)` where ClassName is an imported class
                // (not a local/param) resolvable on the classpath.
                if let Expr::Name(cls) = self.file.expr(receiver).clone() {
                    if self.lookup(&cls).is_none() {
                        // `ClassName.fn(args)` — a companion (static) method call.
                        if let Some(sig) = self.syms.classes.get(&cls).and_then(|c| c.static_methods.get(&name)).cloned() {
                            let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                            if sig.params.len() != arg_tys.len() {
                                self.diags.error(span, format!("static method '{cls}.{name}' expects {} args, got {}", sig.params.len(), arg_tys.len()));
                            } else {
                                for (i, (p, a)) in sig.params.iter().zip(&arg_tys).enumerate() {
                                    self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                                }
                            }
                            return sig.ret;
                        }
                        // `Object.member(args)` — a singleton member call.
                        if self.syms.objects.contains(&cls) {
                            let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                            return match self.syms.classes.get(&cls).and_then(|c| c.methods.get(&name)).cloned() {
                                Some(sig) => {
                                    for (i, (p, a)) in sig.params.iter().zip(&arg_tys).enumerate() {
                                        self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                                    }
                                    sig.ret
                                }
                                None => {
                                    self.diags.error(span, format!("unresolved reference: {name}"));
                                    Ty::Error
                                }
                            };
                        }
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
                // Numeric conversion intrinsics: `n.toInt()`/`toLong()`/`toFloat()`/`toDouble()`.
                if rt.is_numeric() && arg_tys.is_empty() {
                    if let Some(target) = conversion_target(&name) {
                        return target;
                    }
                }
                // Curated `java.lang.StringBuilder` instance methods (append/toString/length).
                if rt == Ty::obj("java/lang/StringBuilder") {
                    if let Some((_, ret)) = resolve_stringbuilder_instance(&name, &arg_tys) {
                        return ret;
                    }
                }
                // Instance method call on a class value: `p.method(args)` (own or inherited).
                if let Ty::Obj(internal) = rt {
                    if let Some(sig) = self.lookup_method(internal, &name) {
                        if sig.params.len() != arg_tys.len() {
                            self.diags.error(span, format!("method '{name}' expects {} args, got {}", sig.params.len(), arg_tys.len()));
                        } else {
                            for (i, (p, a)) in sig.params.iter().zip(&arg_tys).enumerate() {
                                self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                            }
                        }
                        return sig.ret;
                    }
                    // A classpath Java object: resolve the instance method via the `.class` reader.
                    if let Some((_, ret)) = resolve_java_instance(&self.syms.classpath, internal, &name, &arg_tys) {
                        return ret;
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
                if self.lookup(&fname).is_none() {
                    if let Some(t) = self.check_array_builtin(&fname, args, &arg_tys, span) {
                        return t;
                    }
                    if let Some(t) = self.check_precondition_intrinsic(&fname, args, &arg_tys, span) {
                        return t;
                    }
                    // Unqualified companion (static) method call inside a companion member.
                    if let Some(cls) = self.companion_of.clone() {
                        if let Some(sig) = self.syms.classes.get(&cls).and_then(|c| c.static_methods.get(&fname)).cloned() {
                            for (i, (p, a)) in sig.params.iter().zip(&arg_tys).enumerate() {
                                self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                            }
                            return sig.ret;
                        }
                    }
                }
                // Constructor call: `ClassName(args)` (when not shadowed by a local).
                if self.lookup(&fname).is_none() {
                    if let Some(cls) = self.syms.classes.get(&fname).cloned() {
                        let ctor_params: Vec<Ty> = cls.ctor_params.clone();
                        if ctor_params.len() != arg_tys.len() {
                            self.diags.error(span, format!("constructor '{fname}' expects {} args, got {}", ctor_params.len(), arg_tys.len()));
                        } else {
                            for (i, (p, a)) in ctor_params.iter().zip(&arg_tys).enumerate() {
                                self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                            }
                        }
                        return Ty::obj(&cls.internal);
                    }
                    // Constructing a classpath Java type: `Calc()` where `Calc` is imported.
                    if let Some(internal) = self.imports.get(&fname).cloned() {
                        if resolve_java_ctor(&self.syms.classpath, &internal, &arg_tys).is_some() {
                            return Ty::obj(&internal);
                        }
                    }
                    // A common JDK exception by simple name (`throw RuntimeException("msg")`): krusty
                    // accepts the no-arg and single-`String` constructors.
                    if let Some(internal) = builtin_exception(&fname) {
                        let ok = matches!(arg_tys.as_slice(), [] | [Ty::String]);
                        if ok {
                            return Ty::obj(internal);
                        }
                    }
                    // `StringBuilder()` / `StringBuilder("init")` / `StringBuilder(capacity)`.
                    if fname == "StringBuilder" && matches!(arg_tys.as_slice(), [] | [Ty::String] | [Ty::Int]) {
                        return Ty::obj("java/lang/StringBuilder");
                    }
                }
                match self.syms.funs.get(&fname) {
                    Some(sig) => {
                        let sig = sig.clone();
                        if sig.vararg {
                            // Fixed params (all but the last) match by position; remaining args must
                            // match the vararg element type (krusty doesn't support `*spread`).
                            let fixed = sig.params.len() - 1;
                            if arg_tys.len() < fixed {
                                self.diags.error(span, format!("function '{fname}' expects at least {fixed} args, got {}", arg_tys.len()));
                            } else {
                                for i in 0..fixed {
                                    self.expect_assignable(sig.params[i], arg_tys[i], self.span(args[i]), "argument");
                                }
                                let elem = sig.params[fixed].array_elem().unwrap_or(Ty::Error);
                                for i in fixed..arg_tys.len() {
                                    self.expect_assignable(elem, arg_tys[i], self.span(args[i]), "vararg argument");
                                }
                            }
                        } else if sig.params.len() != arg_tys.len() {
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
        // `Nothing` is the bottom type: a diverging branch contributes no value, so the join is the
        // other branch (`if (c) x else throw e` has the type of `x`).
        if a == Ty::Nothing {
            return b;
        }
        if b == Ty::Nothing {
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
                            self.diags.error(self.file.stmt_spans[s.0 as usize], format!("val cannot be reassigned"));
                        }
                        self.expect_assignable(lty, vt, self.file.stmt_spans[s.0 as usize], "assignment");
                    }
                    None if self.companion_of.is_some() && self.syms.props.contains_key(&name) => {
                        // A top-level property write from a companion member targets the wrong class.
                        self.diags.error(self.file.stmt_spans[s.0 as usize], "krusty: top-level property access from a companion member is not supported".to_string());
                    }
                    None => match self.syms.props.get(&name).copied() {
                        Some((lty, is_var)) => {
                            if !is_var {
                                self.diags.error(self.file.stmt_spans[s.0 as usize], format!("val cannot be reassigned"));
                            }
                            self.expect_assignable(lty, vt, self.file.stmt_spans[s.0 as usize], "assignment");
                        }
                        None => {
                            self.diags.error(self.file.stmt_spans[s.0 as usize], format!("unresolved reference: {name}"));
                        }
                    },
                }
            }
            Stmt::AssignMember { receiver, name, value } => {
                let rt = self.expr(receiver);
                let vt = self.expr(value);
                let span = self.file.stmt_spans[s.0 as usize];
                match rt {
                    Ty::Error => {}
                    Ty::Obj(internal) => match self.syms.prop_of(internal, &name) {
                        Some((lty, is_var)) => {
                            if !is_var {
                                self.diags.error(span, "val cannot be reassigned".to_string());
                            }
                            self.expect_assignable(lty, vt, span, "assignment");
                        }
                        None => {
                            self.diags.error(span, format!("unresolved member '{name}' on '{}'", rt.name()));
                        }
                    },
                    _ => self.diags.error(span, format!("cannot assign to a member of '{}'", rt.name())),
                }
            }
            Stmt::AssignIndex { array, index, value } => {
                let at = self.expr(array);
                let it = self.expr(index);
                let vt = self.expr(value);
                let span = self.file.stmt_spans[s.0 as usize];
                self.expect_assignable(Ty::Int, it, span, "array index");
                match at.array_elem() {
                    Some(elem) => self.expect_assignable(elem, vt, span, "array element assignment"),
                    None => {
                        if at != Ty::Error {
                            self.diags.error(span, format!("'{}' is not an array (cannot index-assign)", at.name()));
                        }
                    }
                }
            }
            Stmt::Break | Stmt::Continue => {} // loop control — validated structurally at codegen
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
            Stmt::ForEach { name, iterable, body } => {
                let it = self.expr(iterable);
                let elem = match it {
                    Ty::Array(_) => it.array_elem().unwrap_or(Ty::Error),
                    Ty::String => Ty::Char, // iterating a String yields its chars
                    Ty::Error => Ty::Error,
                    _ => {
                        self.diags.error(self.span(iterable), format!("krusty: 'for' over '{}' is not supported (only arrays and String)", it.name()));
                        Ty::Error
                    }
                };
                self.push_scope();
                self.declare(&name, elem, false);
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
        err_contains("fun f(a: Int): String = a", "type mismatch: inferred type is Int but String was expected");
    }

    #[test]
    fn unresolved_reference() {
        err_contains("fun f(): Int = q", "unresolved reference: q");
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
        err_contains("fun a(x: Int): Int = x\nfun b(): Int = a(\"s\")", "type mismatch: inferred type is String but Int was expected");
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
        err_contains("fun f(p: Widget): Int = 0", "unresolved reference: Widget");
    }

    #[test]
    fn string_method_table() {
        assert_eq!(resolve_string_instance("substring", &[Ty::Int]), Some(("(I)Ljava/lang/String;", Ty::String)));
        assert_eq!(resolve_string_instance("indexOf", &[Ty::String]), Some(("(Ljava/lang/String;)I", Ty::Int)));
        assert_eq!(resolve_string_instance("substring", &[Ty::String]), None);
    }
}
