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
    /// Minimum number of arguments a caller must supply — params beyond this have default values
    /// that the caller fills in. Equals `params.len()` when there are no defaults.
    pub required: usize,
    /// Parameter names, parallel to `params`. Used to map named arguments (`f(x = 1)`) to positions.
    /// Empty for signatures where named-argument calls aren't supported (methods, synthesized members).
    pub param_names: Vec<String>,
    /// For each parameter: if the parameter is a function type `(A, B) -> R`, the inner parameter
    /// types `[A, B]`; otherwise an empty Vec. Used to type-check lambda arguments with the correct
    /// `it` / parameter types. Parallel to `params`.
    pub lambda_param_types: Vec<Vec<Ty>>,
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
    /// `annotation class` — emitted as an interface; instantiation builds a synthetic impl class.
    pub is_annotation: bool,
    /// Default-value expression for each primary-constructor parameter (`None` = required). Used to
    /// fill omitted trailing arguments at a constructor call site (box tests are single-file, so the
    /// `ExprId`s are valid where the constructor is called).
    pub ctor_defaults: Vec<Option<ExprId>>,
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
    /// Top-level *computed* properties (`val g: T get() = …`): a `getG()` static method, no field.
    pub computed_props: std::collections::HashSet<String>,
    /// Simple names declared as `object` singletons (accessed via `Name.member`).
    pub objects: std::collections::HashSet<String>,
    /// Declared `enum` types (simple name → entry names), accessed via `Name.ENTRY`.
    pub enums: HashMap<String, Vec<String>>,
    /// Classpath for resolving Java/JDK references (empty unless the driver sets `-classpath`).
    pub classpath: Classpath,
    /// Top-level extension functions: (receiver_descriptor, method_name) → Signature.
    /// Used to resolve `recv.method(args)` when no instance method matches.
    pub ext_funs: HashMap<(String, String), Signature>,
    /// Top-level extension properties: (receiver_descriptor, prop_name) → (type, is_var). The
    /// getter/setter are emitted as static `getName(Recv)`/`setName(Recv, T)` methods.
    pub ext_props: HashMap<(String, String), (Ty, bool)>,
    /// Simple type name → JVM internal name: every resolvable reference type — user/classpath
    /// classes, classpath `TypeAliasesKt` aliases, and the ported `JavaToKotlinClassMap`
    /// built-ins. The single source of truth for "does this type name resolve, and to what".
    pub class_names: HashMap<String, String>,
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
/// Extract a slash-separated qualified name from a `Name`/`Member` chain (`kotlin.SinceKotlin` →
/// `"kotlin/SinceKotlin"`); `None` if the chain contains a non-name node.
pub fn qualified_path(file: &File, e: ExprId) -> Option<String> {
    match file.expr(e) {
        Expr::Name(n) => Some(n.clone()),
        Expr::Member { receiver, name } => Some(format!("{}/{}", qualified_path(file, *receiver)?, name)),
        _ => None,
    }
}

/// If `internal` names a **classpath annotation** (`ACC_ANNOTATION`), its members `(name, Ty)` read
/// from the no-arg accessor methods. `None` if not an annotation or a member type isn't supported.
pub fn classpath_annotation_members(cp: &Classpath, internal: &str) -> Option<Vec<(String, Ty)>> {
    let ci = cp.find(internal)?;
    if ci.access & 0x2000 == 0 {
        return None; // not ACC_ANNOTATION
    }
    let mut members = Vec::new();
    for m in &ci.methods {
        if m.descriptor.starts_with("()") {
            let ty = desc_to_ty(&m.descriptor[2..]);
            if ty == Ty::Error {
                return None; // a member type we can't model — skip the whole annotation
            }
            members.push((m.name.clone(), ty));
        }
    }
    Some(members)
}

pub fn desc_to_ty(d: &str) -> Ty {
    match d {
        "I" | "B" | "S" => Ty::Int,
        "J" => Ty::Long,
        "F" => Ty::Float,
        "D" => Ty::Double,
        "Z" => Ty::Boolean,
        "C" => Ty::Char,
        "V" => Ty::Unit,
        "Ljava/lang/String;" => Ty::String,
        s if s.starts_with('[') => {
            let elem = desc_to_ty(&s[1..]);
            Ty::array(elem)
        }
        s if s.starts_with('L') && s.ends_with(';') => {
            Ty::obj(&s[1..s.len() - 1])
        }
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
        ("isBlank", []) => ("()Z", Ty::Boolean),
        ("substring", [Ty::Int]) => ("(I)Ljava/lang/String;", Ty::String),
        ("substring", [Ty::Int, Ty::Int]) => ("(II)Ljava/lang/String;", Ty::String),
        ("indexOf", [Ty::String]) => ("(Ljava/lang/String;)I", Ty::Int),
        ("indexOf", [Ty::Char]) => ("(I)I", Ty::Int),
        ("lastIndexOf", [Ty::String]) => ("(Ljava/lang/String;)I", Ty::Int),
        ("lastIndexOf", [Ty::Char]) => ("(I)I", Ty::Int),
        ("contains", [Ty::String]) => ("(Ljava/lang/CharSequence;)Z", Ty::Boolean),
        ("startsWith", [Ty::String]) => ("(Ljava/lang/String;)Z", Ty::Boolean),
        ("endsWith", [Ty::String]) => ("(Ljava/lang/String;)Z", Ty::Boolean),
        ("concat", [Ty::String]) => ("(Ljava/lang/String;)Ljava/lang/String;", Ty::String),
        ("replace", [Ty::String, Ty::String]) => ("(Ljava/lang/CharSequence;Ljava/lang/CharSequence;)Ljava/lang/String;", Ty::String),
        ("uppercase", []) | ("toUpperCase", []) => ("()Ljava/lang/String;", Ty::String),
        ("lowercase", []) | ("toLowerCase", []) => ("()Ljava/lang/String;", Ty::String),
        ("trim", []) => ("()Ljava/lang/String;", Ty::String),
        ("toString", []) => ("()Ljava/lang/String;", Ty::String),
        ("toCharArray", []) => ("()[C", Ty::array(Ty::Char)),
        _ => return None,
    })
}

/// Map a Kotlin String method name to its JVM `java/lang/String` method name.
/// Most are identical; Kotlin 1.5 introduced `uppercase`/`lowercase` as aliases.
pub fn string_kotlin_to_jvm(kotlin_name: &str) -> &'static str {
    match kotlin_name {
        "uppercase" | "toUpperCase" => "toUpperCase",
        "lowercase" | "toLowerCase" => "toLowerCase",
        other => crate::types::intern(other),
    }
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
        // `appendLine(x)` / `appendLine()` (Kotlin extension) — emitted as append(x) + append('\n').
        ("appendLine", [_] | []) => ("()Ljava/lang/StringBuilder;".to_string(), sb),
        _ => return None,
    })
}

/// Resolve a static call `Class.method(args)` against the classpath by exact param-descriptor
/// match. Returns `(owner internal name, method descriptor, return type)`.
pub fn resolve_java_static(cp: &Classpath, internal: &str, method: &str, arg_tys: &[Ty]) -> Option<(String, String, Ty)> {
    let ci = cp.find(internal)?;
    // Only a public method on a public class is callable from generated code (a non-public target
    // → `IllegalAccessError`); otherwise leave it unresolved so the call is rejected, not miscompiled.
    if !ci.is_public() {
        return None;
    }
    let params: String = arg_tys.iter().map(|t| t.descriptor()).collect();
    let prefix = format!("({params})");
    let m = ci.methods.iter().find(|m| m.name == method && m.is_static() && m.is_public() && m.descriptor.starts_with(&prefix))?;
    let ret = m.descriptor[m.descriptor.find(')').unwrap() + 1..].to_string();
    Some((internal.to_string(), m.descriptor.clone(), desc_to_ty(&ret)))
}

/// Resolve an *instance* method on a classpath Java type by name + exact param descriptors.
/// Returns `(method descriptor, return type)` for `invokevirtual`.
pub fn resolve_java_instance(cp: &Classpath, internal: &str, method: &str, arg_tys: &[Ty]) -> Option<(String, Ty)> {
    let ci = cp.find(internal)?;
    if !ci.is_public() {
        return None;
    }
    let params: String = arg_tys.iter().map(|t| t.descriptor()).collect();
    let prefix = format!("({params})");
    let m = ci.methods.iter().find(|m| m.name == method && !m.is_static() && m.is_public() && m.descriptor.starts_with(&prefix))?;
    let ret = m.descriptor[m.descriptor.find(')').unwrap() + 1..].to_string();
    Some((m.descriptor.clone(), desc_to_ty(&ret)))
}

/// Resolve a constructor on a classpath Java type by argument descriptors. Returns its descriptor.
pub fn resolve_java_ctor(cp: &Classpath, internal: &str, arg_tys: &[Ty]) -> Option<String> {
    let ci = cp.find(internal)?;
    let params: String = arg_tys.iter().map(|t| t.descriptor()).collect();
    let exact = format!("({params})V");
    if let Some(m) = ci.methods.iter().find(|m| m.name == "<init>" && m.is_public() && m.descriptor == exact) {
        return Some(m.descriptor.clone());
    }
    // Widening fallback: replace each reference-type arg with Object (e.g. String → Object).
    // Needed because e.g. AssertionError has no public (String) ctor, only public (Object).
    let widened: String = arg_tys.iter().map(|t| match t {
        Ty::String | Ty::Obj(_) | Ty::Array(_) | Ty::Null | Ty::Fun(_) => "Ljava/lang/Object;".to_string(),
        _ => t.descriptor(),
    }).collect();
    let widened_exact = format!("({widened})V");
    ci.methods.iter().find(|m| m.name == "<init>" && m.is_public() && m.descriptor == widened_exact).map(|m| m.descriptor.clone())
}

/// Resolve a Kotlin (or Java) extension / static method where `receiver` is passed as the first
/// argument. Searches the classpath extension index for a static method named `method` on any
/// class, whose first parameter descriptor matches `receiver`'s type and whose remaining params
/// match `arg_tys`.
///
/// Returns `(owner_internal, jvm_method_name, descriptor, return_ty)` or `None` if not found.
pub fn resolve_extension(
    cp: &Classpath,
    receiver: Ty,
    method: &str,
    arg_tys: &[Ty],
) -> Option<(String, String, String, Ty)> {
    let recv_desc = receiver.descriptor();
    let rest_params: String = arg_tys.iter().map(|t| t.descriptor()).collect();
    let full_prefix = format!("({recv_desc}{rest_params})");
    let candidates = cp.find_extensions(&recv_desc, method);
    for c in &candidates {
        if c.descriptor.starts_with(&full_prefix) {
            let ret = desc_to_ty(&c.ret_desc);
            return Some((c.owner.clone(), c.name.clone(), c.descriptor.clone(), ret));
        }
    }
    None
}

fn class_internal(file: &File, name: &str) -> String {
    // A nested class's source name `Outer.Inner` maps to the JVM internal name `Outer$Inner`.
    let mangled = name.replace('.', "$");
    match &file.package {
        Some(pkg) if !pkg.is_empty() => format!("{}/{}", pkg.replace('.', "/"), mangled),
        _ => mangled,
    }
}

/// Stage C: collect top-level function + class signatures across all files. Two passes so that a
/// class type can be referenced before its declaration (and across files).
/// Convenience wrapper — uses an empty classpath (no stdlib type scanning).
pub fn collect_signatures(files: &[File], diags: &mut DiagSink) -> SymbolTable {
    collect_signatures_with_cp(files, Classpath::empty(), diags)
}

/// Like `collect_signatures` but also scans `cp` for class names and type aliases from the
/// classpath (e.g. kotlin-stdlib.jar), eliminating the need for any hardcoded type lists.
pub fn collect_signatures_with_cp(files: &[File], cp: Classpath, diags: &mut DiagSink) -> SymbolTable {
    // Scan classpath to get types and type aliases (e.g. StringBuilder → java/lang/StringBuilder).
    let type_idx = cp.scan_types();

    // Pass 1: every class simple-name -> internal name (no bodies, just the type universe).
    // Pre-seed from the classpath type index so imports/stdlib types are visible.
    let mut class_names: HashMap<String, String> = type_idx.class_names.clone();
    // A user-declared top-level class *shadows* any classpath/JDK type of the same simple name
    // (legal Kotlin — the JDK one would need an explicit import). Only a duplicate among the
    // user's own declarations is a conflict, so track which names the user has defined.
    let mut user_defined: std::collections::HashSet<String> = std::collections::HashSet::new();
    for file in files {
        for &d in &file.decls {
            if let Decl::Class(c) = file.decl(d) {
                let internal = class_internal(file, &c.name);
                if !user_defined.insert(c.name.clone()) {
                    diags.error(c.span, format!("conflicting declarations: {}", c.name));
                }
                class_names.insert(c.name.clone(), internal);
            }
        }
    }
    // Seed the Kotlin built-in → JVM class mapping (ported from the reference compiler's
    // `JavaToKotlinClassMap`; see `jvm::jvm_class_map`). These are intrinsic mapped types
    // (`Comparable`, `Throwable`, `List`, …) — not stdlib `.class` files — so they are always
    // available. User/classpath declarations above take precedence (`or_insert`).
    for name in crate::jvm::jvm_class_map::BUILTIN_MAPPED_NAMES {
        if let Some(internal) = crate::jvm::jvm_class_map::kotlin_builtin_to_jvm(name) {
            class_names.entry(name.to_string()).or_insert_with(|| internal.to_string());
        }
    }

    // Expand type aliases into class_names.
    // `typealias A = B` where B is a user-defined class → A resolves to the same internal name.
    // `typealias A = Primitive` → A maps to `"__ty/<PrimName>"` (decoded in ty_of_ref).
    // `typealias A = java.lang.Foo` → A resolves to the JVM internal name `java/lang/Foo`.
    // Multiple passes handle chains: A = B, B = C.
    //
    // Seed from classpath type aliases (read from @kotlin.Metadata in *TypeAliasesKt.class files)
    // then from any user-defined typealiases in the input files.
    let mut alias_map: HashMap<String, String> = type_idx.type_aliases.into_iter().collect();
    for file in files {
        for (alias, target) in &file.type_aliases {
            alias_map.insert(alias.clone(), target.clone());
        }
    }
    for _ in 0..8 {
        let mut changed = false;
        for (alias, target) in &alias_map {
            if class_names.contains_key(alias.as_str()) {
                continue;
            }
            if let Some(internal) = class_names.get(target.as_str()).cloned() {
                class_names.insert(alias.clone(), internal);
                changed = true;
            } else if Ty::from_name(target).is_some() {
                class_names.insert(alias.clone(), format!("__ty/{target}"));
                changed = true;
            } else if target.contains('/') {
                // Already a JVM internal name (e.g. a classpath `TypeAliasesKt` alias whose
                // expanded type was read straight from `@Metadata` as `kotlin/Exception` →
                // `java/lang/Exception`).
                class_names.insert(alias.clone(), target.clone());
                changed = true;
            } else if target.contains('.') {
                // Fully-qualified class name (e.g. java.lang.Exception) → JVM internal name.
                class_names.insert(alias.clone(), target.replace('.', "/"));
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Top-level function return types (explicit annotations only), collected first so a property
    // initializer `val v = f()` can infer its type from `f`'s return type regardless of decl order.
    let mut fun_rets: HashMap<String, Ty> = HashMap::new();
    for file in files {
        for &d in &file.decls {
            if let Decl::Fun(f) = file.decl(d) {
                if f.receiver.is_none() {
                    if let Some(r) = &f.ret {
                        let tp: std::collections::HashSet<String> = f.type_params.iter().cloned().collect();
                        fun_rets.insert(f.name.clone(), ty_of_ref(r, &class_names, &tp, diags));
                    }
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
                        None => {
                            // For expression-body functions, try to infer the return type from
                            // the body literal (handles `fun f() = "literal"` etc.).  Falls back
                            // to Unit; check_fun will do a deeper inference pass and record any
                            // non-Unit result in TypeInfo::fun_ret_overrides for codegen.
                            if let FunBody::Expr(e) = &f.body {
                                let t = infer_lit_ty(file, *e, &class_names, &fun_rets);
                                if t != Ty::Error {
                                    t
                                } else if let Expr::Name(n) = file.expr(*e) {
                                    // Body is a bare parameter name (`fun f(x: T) = x`): infer T.
                                    f.params.iter().find(|p| &p.name == n)
                                        .map(|p| ty_of_ref(&p.ty, &class_names, &tp, diags))
                                        .unwrap_or(Ty::Unit)
                                } else {
                                    Ty::Unit
                                }
                            } else {
                                Ty::Unit
                            }
                        }
                    };
                    let vararg = f.params.last().map_or(false, |p| p.is_vararg);
                    // Trailing params with defaults may be omitted by callers (positional only).
                    let trailing_defaults = if vararg {
                        0
                    } else {
                        f.params.iter().rev().take_while(|p| p.default.is_some()).count()
                    };
                    let required = f.params.len() - trailing_defaults;
                    // Call-site substitution copies the default expression to the caller, so a default
                    // that reads another parameter can't be reproduced there — reject such functions.
                    let pnames: std::collections::HashSet<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
                    for p in &f.params {
                        if let Some(dx) = p.default {
                            if expr_refs_param(file, dx, &pnames) {
                                diags.error(f.span, "krusty: a default argument that references another parameter is not supported");
                            }
                        }
                    }
                    let lambda_param_types: Vec<Vec<Ty>> = f.params.iter().map(|p| {
                        if !p.ty.fun_params.is_empty() || p.ty.name == "<fun>" {
                            p.ty.fun_params.iter().map(|r| ty_of_ref(r, &class_names, &tp, diags)).collect()
                        } else {
                            Vec::new()
                        }
                    }).collect();
                    let sig = Signature { params, ret, vararg, required, param_names: f.params.iter().map(|p| p.name.clone()).collect(), lambda_param_types };
                    if let Some(recv_ref) = &f.receiver {
                        // Extension function: index by (receiver_descriptor, method_name).
                        let recv_ty = ty_of_ref(recv_ref, &class_names, &tp, diags);
                        // Nullable reference receivers (`fun String?.foo()`) are not supported: the
                        // receiver descriptor is the same as the non-null form so krusty can't
                        // distinguish them at the call site, leading to silent miscompiles or
                        // infinite recursion when the body uses the same operator internally.
                        if recv_ref.nullable && recv_ty.is_reference() {
                            diags.error(f.span, "krusty: extension functions on nullable reference types are not supported".to_string());
                        } else {
                            table.ext_funs.insert((recv_ty.descriptor(), f.name.clone()), sig);
                        }
                    } else if table.funs.insert(f.name.clone(), sig).is_some() {
                        diags.error(f.span, format!("conflicting declarations: {}", f.name));
                    }
                }
                Decl::Class(c) => {
                    let internal = class_names.get(&c.name).cloned().unwrap_or_else(|| class_internal(file, &c.name));
                    let ctp: std::collections::HashSet<String> = c.type_params.iter().cloned().collect();
                    // An `init` block that calls an own member method *before* a later property
                    // initializer runs has subtle init-order semantics (cf. KT-73355) krusty doesn't
                    // model — the helper may observe/overwrite a not-yet-initialized field. Reject it.
                    let own_methods: std::collections::HashSet<&str> = c.methods.iter().map(|m| m.name.as_str()).collect();
                    let is_own_call = |ce: ExprId| matches!(file.expr(ce), Expr::Call { callee, .. } if matches!(file.expr(*callee), Expr::Name(n) if own_methods.contains(n.as_str())));
                    if let Some(last_prop) = c.init_order.iter().rposition(|i| matches!(i, ClassInit::PropInit(_))) {
                        for (pos, init) in c.init_order.iter().enumerate() {
                            if let (true, ClassInit::Block(b)) = (pos < last_prop, init) {
                                if let Expr::Block { stmts, trailing } = file.expr(*b) {
                                    let calls_own = trailing.map_or(false, |t| is_own_call(t))
                                        || stmts.iter().any(|&st| matches!(file.stmt(st), Stmt::Expr(ce) if is_own_call(*ce)));
                                    if calls_own {
                                        diags.error(c.span, "krusty: an init block that calls a member method before a later property initializer is not supported (init order)".to_string());
                                    }
                                }
                            }
                        }
                    }
                    // All primary-ctor params (in order) define the constructor signature.
                    let ctor_params: Vec<Ty> = c.props.iter().map(|p| ty_of_ref(&p.ty, &class_names, &ctp, diags)).collect();
                    let ctor_defaults: Vec<Option<ExprId>> = c.props.iter().map(|p| p.default).collect();
                    // Only `val`/`var` params (+ body props) are backing-field properties.
                    let mut props: Vec<(String, Ty, bool)> = c
                        .props
                        .iter()
                        .filter(|p| p.is_property)
                        .map(|p| (p.name.clone(), ty_of_ref(&p.ty, &class_names, &ctp, diags), p.is_var))
                        .collect();
                    // Body properties (`class C { val x = … }`) are also fields/accessors. A computed
                    // property (custom getter, no annotation) infers its type from the getter body.
                    for bp in &c.body_props {
                        let ty = match (&bp.ty, &bp.getter) {
                            (Some(r), _) => ty_of_ref(r, &class_names, &ctp, diags),
                            (None, Some(FunBody::Expr(g))) => {
                                let locals: HashMap<&str, Ty> = props.iter().map(|(n, t, _)| (n.as_str(), *t)).collect();
                                infer_getter_ty(file, *g, &locals)
                            }
                            (None, _) => bp.init.map(|i| infer_lit_ty(file, i, &class_names, &fun_rets)).unwrap_or(Ty::Error),
                        };
                        if ty == Ty::Error && bp.init.is_some() && bp.ty.is_none() {
                            diags.error(bp.span, format!("krusty: cannot infer the type of property '{}'; add an explicit type", bp.name));
                        }
                        props.push((bp.name.clone(), ty, bp.is_var));
                    }
                    let mut methods: HashMap<String, Signature> = c
                        .methods
                        .iter()
                        .map(|m| {
                            let mut mtp = ctp.clone();
                            mtp.extend(m.type_params.iter().cloned());
                            let params: Vec<Ty> = m.params.iter().map(|p| ty_of_ref(&p.ty, &class_names, &mtp, diags)).collect();
                            let ret = m.ret.as_ref().map(|r| ty_of_ref(r, &class_names, &mtp, diags)).unwrap_or_else(|| {
                                if let FunBody::Expr(e) = &m.body {
                                    let t = infer_lit_ty(file, *e, &class_names, &fun_rets);
                                    if t != Ty::Error { t } else { Ty::Unit }
                                } else { Ty::Unit }
                            });
                            (m.name.clone(), {
                                let n = params.len();
                                let lambda_param_types: Vec<Vec<Ty>> = m.params.iter().map(|p| {
                                    if !p.ty.fun_params.is_empty() || p.ty.name == "<fun>" {
                                        p.ty.fun_params.iter().map(|r| ty_of_ref(r, &class_names, &mtp, diags)).collect()
                                    } else { Vec::new() }
                                }).collect();
                                Signature { params, ret, vararg: false, required: n, param_names: m.params.iter().map(|p| p.name.clone()).collect(), lambda_param_types }
                            })
                        })
                        .collect();
                    // `data class` synthesizes componentN() + copy(props...) callable members.
                    if c.is_data {
                        let self_ty = Ty::obj(&internal);
                        for (i, (_, ty, _)) in props.iter().enumerate() {
                            methods.insert(format!("component{}", i + 1), Signature { params: vec![], ret: *ty, vararg: false, required: 0, param_names: Vec::new(), lambda_param_types: Vec::new() });
                        }
                        methods.insert(
                            "copy".into(),
                            Signature { params: props.iter().map(|(_, t, _)| *t).collect(), ret: self_ty, vararg: false, required: props.len(), param_names: Vec::new(), lambda_param_types: Vec::new() },
                        );
                    }
                    if c.is_object {
                        table.objects.insert(c.name.clone());
                    }
                    if c.is_enum {
                        table.enums.insert(c.name.clone(), c.enum_entries.clone());
                    }
                    // Resolve each supertype to a JVM internal name via `class_names` (user/classpath
                    // classes, stdlib aliases, mapped built-ins). A supertype that resolves to none
                    // of those would be emitted as a bare default-package name → `NoClassDefFound`
                    // at load; reject (skip) instead — never emit an unresolved supertype.
                    let mut resolve_super = |s: &String| -> String {
                        match class_names.get(s) {
                            Some(internal) => internal.clone(),
                            None if ctp.contains(s) => s.clone(), // erased type parameter (degenerate)
                            None => {
                                diags.error(c.span, format!("krusty: supertype '{s}' could not be resolved (provide it on the classpath)"));
                                s.clone()
                            }
                        }
                    };
                    let interfaces: Vec<String> = c.supertypes.iter().map(&mut resolve_super).collect();
                    let super_internal = c.base_class.as_ref().map(|b| resolve_super(b));
                    // `companion object` members → static methods/props on this class.
                    let static_methods: HashMap<String, Signature> = c
                        .companion_methods
                        .iter()
                        .map(|m| {
                            let mut mtp = ctp.clone();
                            mtp.extend(m.type_params.iter().cloned());
                            let params: Vec<Ty> = m.params.iter().map(|p| ty_of_ref(&p.ty, &class_names, &mtp, diags)).collect();
                            let ret = m.ret.as_ref().map(|r| ty_of_ref(r, &class_names, &mtp, diags)).unwrap_or_else(|| {
                                if let FunBody::Expr(e) = &m.body {
                                    let t = infer_lit_ty(file, *e, &class_names, &fun_rets);
                                    if t != Ty::Error { t } else { Ty::Unit }
                                } else { Ty::Unit }
                            });
                            (m.name.clone(), {
                                let n = params.len();
                                let lambda_param_types: Vec<Vec<Ty>> = m.params.iter().map(|p| {
                                    if !p.ty.fun_params.is_empty() || p.ty.name == "<fun>" {
                                        p.ty.fun_params.iter().map(|r| ty_of_ref(r, &class_names, &mtp, diags)).collect()
                                    } else { Vec::new() }
                                }).collect();
                                Signature { params, ret, vararg: false, required: n, param_names: m.params.iter().map(|p| p.name.clone()).collect(), lambda_param_types }
                            })
                        })
                        .collect();
                    let static_props: HashMap<String, Ty> = c
                        .companion_props
                        .iter()
                        .map(|p| {
                            let ty = match &p.ty {
                                Some(r) => ty_of_ref(r, &class_names, &ctp, diags),
                                None => p.init.map(|i| infer_lit_ty(file, i, &class_names, &fun_rets)).unwrap_or(Ty::Error),
                            };
                            if ty == Ty::Error && p.init.is_some() && p.ty.is_none() {
                                diags.error(p.span, format!("krusty: cannot infer the type of property '{}'; add an explicit type", p.name));
                            }
                            // Custom accessors on a `companion object` property are emitted as the
                            // default static getter/setter (the body is ignored) — reject rather
                            // than miscompile.
                            if p.getter.is_some() || p.setter.is_some() {
                                diags.error(p.span, "krusty: companion-object property custom accessors are not supported".to_string());
                            }
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
                        ClassSig { internal, props, ctor_params, methods, is_interface: c.is_interface, is_sealed: c.is_sealed, static_methods, static_props, lateinit_props, interfaces, super_internal, is_annotation: c.is_annotation, ctor_defaults },
                    );
                }
                Decl::Property(p) => {
                    // Extension property `val Recv.name: T get() = …`: register by (receiver
                    // descriptor, name); emitted as a static `getName(Recv)`/`setName(Recv, T)`.
                    if let Some(recv_ref) = &p.receiver {
                        let recv_ty = ty_of_ref(recv_ref, &class_names, &Default::default(), diags);
                        let ty = p.ty.as_ref().map(|r| ty_of_ref(r, &class_names, &Default::default(), diags))
                            .or_else(|| match &p.getter {
                                Some(FunBody::Expr(g)) => Some(infer_lit_ty(file, *g, &class_names, &fun_rets)),
                                _ => None,
                            })
                            .unwrap_or(Ty::Error);
                        if recv_ty != Ty::Error && ty != Ty::Error {
                            let key = (recv_ty.descriptor(), p.name.clone());
                            // Two extension properties that erase to the same `(receiver, name)` (e.g.
                            // generic overloads `C<T: Any?>.p` and `C<T: Any>.p`) would emit duplicate
                            // `getName` methods → `ClassFormatError`. Reject (skip), never miscompile.
                            if table.ext_props.contains_key(&key) {
                                diags.error(p.span, format!("krusty: conflicting extension property '{}' (same erased receiver)", p.name));
                            }
                            table.ext_props.insert(key, (ty, p.is_var));
                        }
                        continue;
                    }
                    // A top-level *computed* property (custom getter, no initializer) — needs a type
                    // annotation (no getter-return inference at top level yet); emitted as `getX()`.
                    let is_computed = p.getter.is_some() && p.init.is_none();
                    // Top-level custom accessors over a *backing field* (a getter with an initializer,
                    // or any setter) aren't emitted yet — the facade would silently use the default
                    // accessor. Reject rather than miscompile (member properties are supported).
                    if (p.getter.is_some() && p.init.is_some()) || p.setter.is_some() {
                        diags.error(p.span, "krusty: top-level property custom accessors are not supported".to_string());
                    }
                    // Type from the annotation, else a light inference from a literal initializer (or,
                    // for a computed property, from its expression getter body).
                    let ty = match (&p.ty, &p.getter) {
                        (Some(r), _) => ty_of_ref(r, &class_names, &Default::default(), diags),
                        (None, Some(FunBody::Expr(g))) if is_computed => infer_lit_ty(file, *g, &class_names, &fun_rets),
                        (None, _) => p.init.map(|i| infer_lit_ty(file, i, &class_names, &fun_rets)).unwrap_or(Ty::Error),
                    };
                    if ty == Ty::Error && (p.init.is_some() || is_computed) && p.ty.is_none() {
                        diags.error(p.span, format!("krusty: cannot infer the type of property '{}'; add an explicit type", p.name));
                    }
                    if is_computed {
                        table.computed_props.insert(p.name.clone());
                    }
                    table.props.insert(p.name.clone(), (ty, p.is_var));
                }
            }
        }
    }

    // Add ClassSig aliases so that `typealias Bar = Foo` allows `Bar(...)` constructor calls.
    for (alias, target) in &alias_map {
        if !table.classes.contains_key(alias.as_str()) {
            if let Some(cs) = table.classes.get(target.as_str()).cloned() {
                table.classes.insert(alias.clone(), cs);
            }
        }
    }

    table.classpath = cp;
    table.class_names = class_names;
    table
}

/// Light type inference for an unannotated computed-property getter body (`val x get() = expr`),
/// against the class's already-collected properties (`locals`). Handles literals, property/`this.x`
/// references, `.size`/`.length`, unary, and binary ops; anything else is `Error` (the file skips).
/// Map a call's source-order arguments (with optional `name =` labels) onto positional parameter
/// slots. Returns a vector of length `param_names.len()`: each slot holds the supplied argument or
/// `None` (the parameter falls back to its default). Errors describe the first problem found
/// (unknown/duplicate name, positional-after-named, arity, or a missing required argument).
pub fn map_call_args(
    args: &[ExprId],
    names: Option<&[Option<String>]>,
    param_names: &[String],
    required: usize,
) -> Result<Vec<Option<ExprId>>, String> {
    let n = param_names.len();
    let mut slots: Vec<Option<ExprId>> = vec![None; n];
    let mut pos = 0usize;
    let mut seen_named = false;
    for (i, &a) in args.iter().enumerate() {
        match names.and_then(|ns| ns.get(i)).and_then(|o| o.as_ref()) {
            Some(nm) => {
                seen_named = true;
                let idx = param_names.iter().position(|p| p == nm).ok_or_else(|| format!("no parameter named '{nm}'"))?;
                if slots[idx].is_some() {
                    return Err(format!("an argument is already passed for '{nm}'"));
                }
                slots[idx] = Some(a);
            }
            None => {
                if seen_named {
                    return Err("a positional argument cannot follow a named argument".to_string());
                }
                if pos >= n {
                    return Err(format!("too many arguments: expected at most {n}"));
                }
                slots[pos] = Some(a);
                pos += 1;
            }
        }
    }
    for (i, slot) in slots.iter().enumerate().take(required) {
        if slot.is_none() {
            return Err(format!("no value passed for required parameter '{}'", param_names.get(i).map(|s| s.as_str()).unwrap_or("?")));
        }
    }
    Ok(slots)
}

/// Does the default-argument expression `e` read any of `names` (the function's own parameters)?
/// Statement-bearing expressions (blocks, lambdas, try, when) are conservatively treated as a
/// reference so we never silently mis-substitute a default we can't fully analyse.
fn expr_uses_name(file: &File, e: ExprId, name: &str) -> bool {
    let set: std::collections::HashSet<&str> = std::iter::once(name).collect();
    expr_refs_param(file, e, &set)
}

pub fn expr_uses_name_pub(file: &File, e: ExprId, name: &str) -> bool {
    expr_uses_name(file, e, name)
}

fn stmt_refs_param(file: &File, s: StmtId, names: &std::collections::HashSet<&str>) -> bool {
    let r = |x: ExprId| expr_refs_param(file, x, names);
    match file.stmt(s) {
        Stmt::Local { init, .. } => r(*init),
        Stmt::Destructure { init, .. } => r(*init),
        Stmt::IncDec { name, .. } => names.contains(name.as_str()),
        Stmt::Assign { value, .. } => r(*value),
        Stmt::AssignMember { receiver, value, .. } => r(*receiver) || r(*value),
        Stmt::AssignIndex { array, index, value } => r(*array) || r(*index) || r(*value),
        Stmt::Return(Some(e)) => r(*e),
        Stmt::Return(None) | Stmt::Break | Stmt::Continue => false,
        Stmt::While { cond, body } => r(*cond) || r(*body),
        Stmt::For { range, body, .. } => r(range.start) || r(range.end) || range.step.map_or(false, |s| r(s)) || r(*body),
        Stmt::ForEach { iterable, body, .. } => r(*iterable) || r(*body),
        Stmt::Expr(e) => r(*e),
        Stmt::LocalFun(_) => false,
    }
}

/// Whether `e`'s subtree contains a `try` expression (used to reject *nested* try/catch, which hits
/// a StackMapTable frame bug in codegen).
fn expr_has_try(file: &File, e: ExprId) -> bool {
    let r = |x: ExprId| expr_has_try(file, x);
    match file.expr(e) {
        Expr::Try { .. } => true,
        Expr::Name(_) | Expr::IntLit(_) | Expr::LongLit(_) | Expr::DoubleLit(_) | Expr::FloatLit(_)
        | Expr::BoolLit(_) | Expr::StringLit(_) | Expr::CharLit(_) | Expr::NullLit
        | Expr::Lambda { .. } | Expr::CallableRef { .. } => false,
        Expr::NotNull { operand } | Expr::Throw { operand } | Expr::Unary { operand, .. }
        | Expr::Is { operand, .. } | Expr::As { operand, .. } => r(*operand),
        Expr::Elvis { lhs, rhs } | Expr::Binary { lhs, rhs, .. } => r(*lhs) || r(*rhs),
        Expr::Member { receiver, .. } => r(*receiver),
        Expr::Index { array, index } => r(*array) || r(*index),
        Expr::Call { callee, args } => r(*callee) || args.iter().any(|&a| r(a)),
        Expr::SafeCall { receiver, args, .. } => r(*receiver) || args.as_ref().map_or(false, |a| a.iter().any(|&x| r(x))),
        Expr::Template(parts) => parts.iter().any(|p| matches!(p, TemplatePart::Expr(x) if r(*x))),
        Expr::If { cond, then_branch, else_branch } => r(*cond) || r(*then_branch) || else_branch.map_or(false, |x| r(x)),
        Expr::Block { stmts, trailing } => stmts.iter().any(|&s| stmt_has_try(file, s)) || trailing.map_or(false, |t| r(t)),
        Expr::When { subject, arms } => subject.map_or(false, |s| r(s)) || arms.iter().any(|a| a.conditions.iter().any(|&c| r(c)) || r(a.body)),
    }
}

fn stmt_has_try(file: &File, s: StmtId) -> bool {
    match file.stmt(s) {
        Stmt::Expr(e) => expr_has_try(file, *e),
        Stmt::Return(Some(e)) => expr_has_try(file, *e),
        Stmt::Local { init, .. } => expr_has_try(file, *init),
        _ => false,
    }
}

fn expr_refs_param(file: &File, e: ExprId, names: &std::collections::HashSet<&str>) -> bool {
    let r = |x: ExprId| expr_refs_param(file, x, names);
    match file.expr(e) {
        Expr::Name(n) => names.contains(n.as_str()),
        Expr::IntLit(_) | Expr::LongLit(_) | Expr::DoubleLit(_) | Expr::FloatLit(_) | Expr::BoolLit(_) | Expr::StringLit(_) | Expr::CharLit(_) | Expr::NullLit => false,
        Expr::NotNull { operand } | Expr::Throw { operand } | Expr::Unary { operand, .. } => r(*operand),
        Expr::Is { operand, .. } | Expr::As { operand, .. } => r(*operand),
        Expr::Elvis { lhs, rhs } | Expr::Binary { lhs, rhs, .. } => r(*lhs) || r(*rhs),
        Expr::Member { receiver, .. } => r(*receiver),
        Expr::Index { array, index } => r(*array) || r(*index),
        Expr::Call { callee, args } => r(*callee) || args.iter().any(|&a| r(a)),
        Expr::SafeCall { receiver, args, .. } => r(*receiver) || args.as_ref().map_or(false, |a| a.iter().any(|&x| r(x))),
        Expr::Template(parts) => parts.iter().any(|p| matches!(p, TemplatePart::Expr(x) if r(*x))),
        Expr::If { cond, then_branch, else_branch } => r(*cond) || r(*then_branch) || else_branch.map_or(false, |x| r(x)),
        // Blocks, try, and when recurse; Lambda introduces a new `it` scope so stop here.
        Expr::Block { stmts, trailing } => stmts.iter().any(|&s| stmt_refs_param(file, s, names)) || trailing.map_or(false, |t| r(t)),
        Expr::Try { body, catches, finally } => r(*body) || catches.iter().any(|c| r(c.body)) || finally.map_or(false, |f| r(f)),
        Expr::When { subject, arms } => subject.map_or(false, |s| r(s)) || arms.iter().any(|a| a.conditions.iter().any(|&c| r(c)) || r(a.body)),
        Expr::Lambda { .. } | Expr::CallableRef { .. } => false,
    }
}

/// Returns true if the expression subtree (or any statement within it) references a name from
/// `outer`. Used to detect captures in local function bodies before allowing lift-to-static.
fn local_fun_body_uses_any(file: &File, e: ExprId, outer: &std::collections::HashSet<String>) -> bool {
    fn check_e(file: &File, e: ExprId, outer: &std::collections::HashSet<String>) -> bool {
        let r = |x: ExprId| check_e(file, x, outer);
        let rs = |x: StmtId| check_s(file, x, outer);
        match file.expr(e) {
            Expr::Name(n) => outer.contains(n),
            Expr::IntLit(_)|Expr::LongLit(_)|Expr::DoubleLit(_)|Expr::FloatLit(_)
            |Expr::BoolLit(_)|Expr::StringLit(_)|Expr::CharLit(_)|Expr::NullLit => false,
            Expr::NotNull{operand}|Expr::Throw{operand}|Expr::Unary{operand,..} => r(*operand),
            Expr::Is{operand,..}|Expr::As{operand,..} => r(*operand),
            Expr::Elvis{lhs,rhs}|Expr::Binary{lhs,rhs,..} => r(*lhs)||r(*rhs),
            Expr::Member{receiver,..} => r(*receiver),
            Expr::Index{array,index} => r(*array)||r(*index),
            Expr::Call{callee,args} => r(*callee)||args.iter().any(|&a|r(a)),
            Expr::SafeCall{receiver,args,..} => r(*receiver)||args.as_ref().map_or(false,|a|a.iter().any(|&x|r(x))),
            Expr::Template(parts) => parts.iter().any(|p|matches!(p,TemplatePart::Expr(x) if r(*x))),
            Expr::If{cond,then_branch,else_branch} => r(*cond)||r(*then_branch)||else_branch.map_or(false,|x|r(x)),
            Expr::Lambda{body,..} => r(*body),
            Expr::Try{body,catches,finally} => r(*body)||catches.iter().any(|c|r(c.body))||finally.map_or(false,|f|r(f)),
            Expr::When{subject,arms} => subject.map_or(false,|s|r(s))||arms.iter().any(|a|a.conditions.iter().any(|&c|r(c))||r(a.body)),
            Expr::Block{stmts,trailing} => stmts.iter().any(|&s|rs(s))||trailing.map_or(false,|t|r(t)),
            Expr::CallableRef{receiver,..} => receiver.map_or(false,|r2|r(r2)),
        }
    }
    fn check_s(file: &File, s: StmtId, outer: &std::collections::HashSet<String>) -> bool {
        let r = |x: ExprId| check_e(file, x, outer);
        match file.stmt(s) {
            Stmt::Local{init,..} => r(*init),
            Stmt::Destructure{init,..} => r(*init),
            Stmt::IncDec{name,..} => outer.contains(name),
            Stmt::Assign{value,..} => r(*value),
            Stmt::AssignMember{receiver,value,..} => r(*receiver)||r(*value),
            Stmt::AssignIndex{array,index,value} => r(*array)||r(*index)||r(*value),
            Stmt::Return(Some(e)) => r(*e),
            Stmt::Return(None)|Stmt::Break|Stmt::Continue => false,
            Stmt::While{cond,body} => r(*cond)||r(*body),
            Stmt::For{range,body,..} => r(range.start)||r(range.end)||range.step.map_or(false,|s|r(s))||r(*body),
            Stmt::ForEach{iterable,body,..} => r(*iterable)||r(*body),
            Stmt::Expr(e) => r(*e),
            Stmt::LocalFun(_) => false, // nested local funs have their own capture check
        }
    }
    check_e(file, e, outer)
}

/// Returns `true` if an expression subtree contains a `Stmt::Assign` (or `+=`-style via
/// `AssignMember` on a Name) whose target is a `Name` that appears in `outer_names`.
/// Used to detect mutable captures in non-inlined lambda bodies.
fn lambda_body_writes_outer(file: &File, e: ExprId, outer_names: &std::collections::HashSet<String>) -> bool {
    fn check_e(file: &File, e: ExprId, outer_names: &std::collections::HashSet<String>) -> bool {
        let r = |x: ExprId| check_e(file, x, outer_names);
        let rs = |x: StmtId| check_s(file, x, outer_names);
        match file.expr(e) {
            Expr::Block { stmts, trailing } => stmts.iter().any(|&s| rs(s)) || trailing.map_or(false, |t| r(t)),
            Expr::If { cond, then_branch, else_branch } => r(*cond) || r(*then_branch) || else_branch.map_or(false, |x| r(x)),
            Expr::Try { body, catches, finally } => r(*body) || catches.iter().any(|c| r(c.body)) || finally.map_or(false, |f| r(f)),
            Expr::When { subject, arms } => subject.map_or(false, |s| r(s)) || arms.iter().any(|a| a.conditions.iter().any(|&c| r(c)) || r(a.body)),
            // Don't recurse into nested lambdas — they have their own scope.
            Expr::Lambda { .. } => false,
            _ => false,
        }
    }
    fn check_s(file: &File, s: StmtId, outer_names: &std::collections::HashSet<String>) -> bool {
        let r = |x: ExprId| check_e(file, x, outer_names);
        match file.stmt(s) {
            Stmt::IncDec { name, .. } => outer_names.contains(name),
            Stmt::Assign { name, value } => {
                // `x = expr` or `x += expr` — check if target is an outer var.
                outer_names.contains(name) || r(*value)
            }
            Stmt::Local { init, .. } => r(*init),
            Stmt::Destructure { init, .. } => r(*init),
            Stmt::AssignMember { receiver, value, .. } => r(*receiver) || r(*value),
            Stmt::AssignIndex { array, index, value } => r(*array) || r(*index) || r(*value),
            Stmt::Return(Some(e)) => r(*e),
            Stmt::Return(None) | Stmt::Break | Stmt::Continue => false,
            Stmt::While { cond, body } => r(*cond) || r(*body),
            Stmt::For { range, body, .. } => r(range.start) || r(range.end) || range.step.map_or(false, |s| r(s)) || r(*body),
            Stmt::ForEach { iterable, body, .. } => r(*iterable) || r(*body),
            Stmt::Expr(e) => r(*e),
            Stmt::LocalFun(_) => false,
        }
    }
    check_e(file, e, outer_names)
}

fn infer_getter_ty(file: &File, e: ExprId, locals: &HashMap<&str, Ty>) -> Ty {
    match file.expr(e) {
        Expr::IntLit(_) => Ty::Int,
        Expr::LongLit(_) => Ty::Long,
        Expr::DoubleLit(_) => Ty::Double,
        Expr::FloatLit(_) => Ty::Float,
        Expr::BoolLit(_) => Ty::Boolean,
        Expr::CharLit(_) => Ty::Char,
        Expr::StringLit(_) | Expr::Template(_) => Ty::String,
        Expr::Name(n) => locals.get(n.as_str()).copied().unwrap_or(Ty::Error),
        Expr::Member { receiver, name } => {
            if matches!(file.expr(*receiver), Expr::Name(r) if r == "this") {
                locals.get(name.as_str()).copied().unwrap_or(Ty::Error)
            } else if name == "size" || name == "length" {
                Ty::Int
            } else {
                Ty::Error
            }
        }
        Expr::Unary { op, operand } => match op {
            UnOp::Not => Ty::Boolean,
            UnOp::Neg => infer_getter_ty(file, *operand, locals),
        },
        Expr::Binary { op, lhs, rhs } => {
            let lt = infer_getter_ty(file, *lhs, locals);
            let rt = infer_getter_ty(file, *rhs, locals);
            match op {
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne | BinOp::And | BinOp::Or | BinOp::RefEq | BinOp::RefNe => Ty::Boolean,
                BinOp::Add if lt == Ty::String || rt == Ty::String => Ty::String,
                _ => Ty::promote(lt, rt).unwrap_or(Ty::Error),
            }
        }
        _ => Ty::Error,
    }
}

/// Type of a primitive-type companion constant: `Int.MAX_VALUE`, `Long.MIN_VALUE`, etc.
fn prim_companion_ty(prim: &str, field: &str) -> Option<Ty> {
    match (prim, field) {
        ("Int", "MAX_VALUE" | "MIN_VALUE" | "SIZE_BITS" | "SIZE_BYTES") => Some(Ty::Int),
        ("Long", "MAX_VALUE" | "MIN_VALUE") => Some(Ty::Long),
        ("Long", "SIZE_BITS" | "SIZE_BYTES") => Some(Ty::Int),
        ("Short", "MAX_VALUE" | "MIN_VALUE") => Some(Ty::Short),
        ("Byte", "MAX_VALUE" | "MIN_VALUE") => Some(Ty::Byte),
        ("Char", "MAX_VALUE" | "MIN_VALUE") => Some(Ty::Char),
        ("Float", "MAX_VALUE" | "MIN_VALUE" | "NaN" | "POSITIVE_INFINITY" | "NEGATIVE_INFINITY") => Some(Ty::Float),
        ("Double", "MAX_VALUE" | "MIN_VALUE" | "NaN" | "POSITIVE_INFINITY" | "NEGATIVE_INFINITY") => Some(Ty::Double),
        _ => None,
    }
}

/// Best-effort type of a simple literal initializer (for an unannotated top-level property).
/// Names of Kotlin's primitive operator/bitwise/conversion-overloadable methods. An explicit call
/// of one of these on a primitive receiver binds to the builtin operator, not a user extension.
fn is_builtin_operator_method(name: &str) -> bool {
    matches!(
        name,
        "plus" | "minus" | "times" | "div" | "rem" | "mod"
            | "inc" | "dec" | "unaryPlus" | "unaryMinus"
            | "and" | "or" | "xor" | "inv" | "shl" | "shr" | "ushr"
            | "compareTo" | "rangeTo"
    )
}

fn infer_lit_ty(file: &File, e: ExprId, class_names: &HashMap<String, String>, fun_rets: &HashMap<String, Ty>) -> Ty {
    match file.expr(e) {
        Expr::IntLit(_) => Ty::Int,
        Expr::LongLit(_) => Ty::Long,
        Expr::DoubleLit(_) => Ty::Double,
        Expr::FloatLit(_) => Ty::Float,
        Expr::BoolLit(_) => Ty::Boolean,
        Expr::CharLit(_) => Ty::Char,
        Expr::StringLit(_) | Expr::Template(_) => Ty::String,
        Expr::Member { receiver, name } => {
            if let Expr::Name(prim) = file.expr(*receiver) {
                prim_companion_ty(prim, name).unwrap_or(Ty::Error)
            } else {
                Ty::Error
            }
        }
        Expr::Unary { op, operand } => match op {
            UnOp::Not => Ty::Boolean,
            UnOp::Neg => infer_lit_ty(file, *operand, class_names, fun_rets),
        },
        Expr::Binary { op, lhs, rhs } => {
            let (lt, rt) = (infer_lit_ty(file, *lhs, class_names, fun_rets), infer_lit_ty(file, *rhs, class_names, fun_rets));
            match op {
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne | BinOp::And | BinOp::Or | BinOp::RefEq | BinOp::RefNe => Ty::Boolean,
                BinOp::Add if lt == Ty::String || rt == Ty::String => Ty::String,
                _ => Ty::promote(lt, rt).unwrap_or(Ty::Error),
            }
        }
        // Constructor call `Foo(args)` — infer type from callee name via class_names (seeded from
        // classpath scan + user-defined classes).
        Expr::Call { callee, .. } => {
            if let Expr::Name(n) = file.expr(*callee) {
                // A call to a top-level function with a known return type (`val v = mk()`).
                if let Some(ret) = fun_rets.get(n.as_str()) {
                    return *ret;
                }
                // A JDK/classpath type resolvable by simple name (`val sb = StringBuilder()`).
                if let Some(internal) = class_names.get(n.as_str()) {
                    return Ty::obj(internal);
                }
            }
            Ty::Error
        }
        _ => Ty::Error,
    }
}

/// Resolve a syntactic type reference to a `Ty`: a primitive/String/Unit, a declared class
/// (→ `Ty::Obj`), or a generic type parameter (erased to `java/lang/Object`).
fn ty_of_ref(r: &TypeRef, classes: &HashMap<String, String>, tparams: &std::collections::HashSet<String>, diags: &mut DiagSink) -> Ty {
    // Function type: `(A, B) -> R` — parsed with `fun_params` non-empty.
    if !r.fun_params.is_empty() || r.name == "<fun>" {
        let params: Vec<Ty> = r.fun_params.iter().map(|p| ty_of_ref(p, classes, tparams, diags)).collect();
        let ret = r.arg.as_ref().map(|a| ty_of_ref(a, classes, tparams, diags)).unwrap_or(Ty::Unit);
        return Ty::fun(params, ret);
    }
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
    } else if r.name == "KClass" {
        // `KClass<*>` is modeled as `java.lang.Class` (the JVM annotation representation, and what
        // `X::class` lowers to here). Enough for class-literal storage/identity, not full reflection.
        Ty::obj("java/lang/Class")
    } else if tparams.contains(&r.name) {
        Ty::obj("java/lang/Object") // erased generic type parameter
    } else if let Some(internal) = classes.get(&r.name) {
        // `"__ty/<PrimName>"` encodes a type-alias → primitive/builtin mapping.
        if let Some(prim) = internal.strip_prefix("__ty/") {
            Ty::from_name(prim).unwrap_or(Ty::Error)
        } else {
            Ty::obj(internal)
        }
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
    /// Maps the `StmtId` of each `Stmt::LocalFun` to its (mangled JVM method name, signature).
    pub local_fun_sigs: HashMap<StmtId, (String, Signature)>,
    /// Maps a call `ExprId` to the `StmtId` of the local function it dispatches to.
    pub local_call_map: HashMap<ExprId, StmtId>,
    /// Inferred return types for expression-body functions that lacked an explicit return annotation.
    /// Codegen overrides the pre-collected `Ty::Unit` default with this when present.
    pub fun_ret_overrides: HashMap<String, Ty>,
    /// Extension / static method calls resolved from the classpath:
    /// `call_expr_id → (owner_internal, jvm_method_name, jvm_descriptor)`.
    /// Emitter emits `invokestatic owner.name descriptor` (receiver is the first arg).
    pub ext_calls: HashMap<ExprId, (String, String, String)>,
    /// Synthetic bridge methods a class must emit so a supertype's erased call dispatches to the
    /// class's concrete override (`class_internal → [bridge]`).
    pub bridges: HashMap<String, Vec<BridgeSpec>>,
}

/// A bridge method: erased signature (the supertype's descriptor, what callers invoke) delegating to
/// the class's concrete method (`name(concrete_params)concrete_ret`).
#[derive(Clone, Debug)]
pub struct BridgeSpec {
    pub name: String,
    pub erased_params: Vec<Ty>,
    pub erased_ret: Ty,
    pub concrete_params: Vec<Ty>,
    pub concrete_ret: Ty,
}

impl TypeInfo {
    pub fn ty(&self, e: ExprId) -> Ty {
        self.expr_types[e.0 as usize]
    }
    /// If call `e` was resolved to a local function, return its mangled name and signature.
    pub fn local_fun_for_call(&self, e: ExprId) -> Option<(&str, &Signature)> {
        let stmt_id = self.local_call_map.get(&e)?;
        let (name, sig) = self.local_fun_sigs.get(stmt_id)?;
        Some((name.as_str(), sig))
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
        field_ty: None,
        companion_of: None,
        local_funs: Vec::new(),
        local_fun_sigs: HashMap::new(),
        local_call_map: HashMap::new(),
        fun_ret_overrides: HashMap::new(),
        ext_calls: HashMap::new(),
        bridges: HashMap::new(),
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
                // Secondary constructors are parsed (real grammar), but krusty doesn't yet emit the
                // extra `<init>` overloads + delegation — a call to one would resolve to a missing
                // constructor. Reject the class until emission lands, rather than miscompile.
                if !cl.secondary_ctors.is_empty() {
                    c.diags.error(cl.span, "krusty: secondary constructors are not supported".to_string());
                }
                // `@JvmInline value class` needs kotlinc's unboxed `-impl`/`box-impl`/`unbox-impl`
                // codegen + use-site name mangling; compiling it as a normal class miscompiles
                // inline-class equality/identity (verified FAILs). Skip until that lands.
                if cl.is_value {
                    c.diags.error(cl.span, "krusty: value/inline classes are not supported".to_string());
                }
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
                // Base class constructor args are evaluated before the body and may reference ctor params.
                for arg in &cl.base_args {
                    c.expr(*arg);
                }
                for bp in &cl.body_props {
                    if let Some(init) = bp.init {
                        let it = c.expr(init);
                        if let Some(r) = &bp.ty {
                            let declared = c.resolve_ty(r);
                            c.expect_assignable(declared, it, c.span(init), "property initializer");
                        }
                    }
                    // A property's accessor bodies are checked like methods, with `field` bound to
                    // the backing-field type (the implicit-`this` scope of props is already active).
                    let prop_ty = bp.ty.as_ref().map(|r| c.resolve_ty(r))
                        .or_else(|| c.syms.classes.get(&cl.name)
                            .and_then(|cs| cs.props.iter().find(|(n, _, _)| n == &bp.name).map(|(_, t, _)| *t)))
                        .unwrap_or(Ty::Error);
                    if let Some(getter) = &bp.getter {
                        let prev_ret = c.ret_ty;
                        let prev_field = c.field_ty;
                        c.ret_ty = prop_ty;
                        c.field_ty = Some(prop_ty);
                        match getter {
                            FunBody::Expr(g) => {
                                let gt = c.expr(*g);
                                c.expect_assignable(c.ret_ty, gt, c.span(*g), "getter body");
                            }
                            FunBody::Block(g) => { let _ = c.expr(*g); }
                            FunBody::None => {}
                        }
                        c.ret_ty = prev_ret;
                        c.field_ty = prev_field;
                    }
                    if let Some(setter) = &bp.setter {
                        if let Some(body) = &setter.body {
                            let prev_ret = c.ret_ty;
                            let prev_field = c.field_ty;
                            c.ret_ty = Ty::Unit;
                            c.field_ty = Some(prop_ty);
                            c.push_scope();
                            let pname = setter.param.clone().unwrap_or_else(|| "value".to_string());
                            c.declare(&pname, prop_ty, true);
                            match body {
                                FunBody::Expr(g) | FunBody::Block(g) => { let _ = c.expr(*g); }
                                FunBody::None => {}
                            }
                            c.pop_scope();
                            c.ret_ty = prev_ret;
                            c.field_ty = prev_field;
                        }
                    }
                }
                for step in &cl.init_order {
                    if let ClassInit::Block(b) = step {
                        c.expr(*b);
                    }
                }
                c.pop_scope();
                c.this_ty = None;
                // Enum entry constructor arguments (e.g. `RED(0xff0000)`) are type-checked in a
                // fresh scope — they're emitted in the static `<clinit>` and cannot access `this`.
                if cl.is_enum {
                    let ctor_tys: Vec<Ty> = cl.props.iter().map(|p| c.resolve_ty(&p.ty)).collect();
                    for args in &cl.enum_entry_args {
                        for (a, expected_ty) in args.iter().zip(&ctor_tys) {
                            let at = c.expr(*a);
                            c.expect_assignable(*expected_ty, at, c.span(*a), "enum entry argument");
                        }
                    }
                }
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
                // For an extension property (`val Recv.name: T get() = …`), `this` inside the
                // accessors is the receiver.
                let prev_this = c.this_ty;
                let recv_ty = p.receiver.as_ref().map(|r| c.resolve_ty(r));
                if let Some(rt) = recv_ty {
                    c.this_ty = Some(rt);
                }
                let prop_ty = p.ty.as_ref().map(|r| c.resolve_ty(r))
                    .or_else(|| p.receiver.as_ref().map(|r| c.resolve_ty(r)).and_then(|rt|
                        c.syms.ext_props.get(&(rt.descriptor(), p.name.clone())).map(|(t, _)| *t)))
                    .unwrap_or(Ty::Error);
                // A top-level computed property (`val g: T get() = …`) emits a `getG()` static method
                // (Phase: top-level computed). Type-check the getter body against the declared type.
                if let Some(g) = &p.getter {
                    let prev = c.ret_ty;
                    c.ret_ty = prop_ty;
                    match g {
                        FunBody::Expr(e) => { let gt = c.expr(*e); c.expect_assignable(c.ret_ty, gt, c.span(*e), "getter body"); }
                        FunBody::Block(b) => { let _ = c.expr(*b); }
                        FunBody::None => {}
                    }
                    c.ret_ty = prev;
                }
                // Extension-property setter body: bind the parameter and check with `this` = receiver.
                if p.receiver.is_some() {
                    if let Some(setter) = &p.setter {
                        if let Some(body) = &setter.body {
                            let prev = c.ret_ty;
                            c.ret_ty = Ty::Unit;
                            c.push_scope();
                            c.declare(setter.param.as_deref().unwrap_or("value"), prop_ty, true);
                            match body { FunBody::Expr(g) | FunBody::Block(g) => { let _ = c.expr(*g); } FunBody::None => {} }
                            c.pop_scope();
                            c.ret_ty = prev;
                        }
                    }
                }
                c.this_ty = prev_this;
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
    TypeInfo { expr_types: c.expr_types, local_fun_sigs: c.local_fun_sigs, local_call_map: c.local_call_map, fun_ret_overrides: c.fun_ret_overrides, ext_calls: c.ext_calls, bridges: c.bridges }
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
    /// The backing-field type while checking a property accessor body — makes the `field`
    /// soft-keyword resolve to the property's backing field. `None` outside an accessor.
    field_ty: Option<Ty>,
    /// When checking a `companion object` member, the enclosing class name — its companion
    /// methods/properties are then in scope unqualified.
    companion_of: Option<String>,
    /// Stack of frames for local-function scopes; each frame maps name → (StmtId, Signature).
    /// Pushed when entering a function, popped on exit; each `Stmt::LocalFun` registers into the
    /// innermost frame so that sibling local-function calls resolve correctly.
    local_funs: Vec<HashMap<String, (StmtId, Signature)>>,
    /// Accumulated output maps (moved into TypeInfo at the end of `check_file`).
    local_fun_sigs: HashMap<StmtId, (String, Signature)>,
    local_call_map: HashMap<ExprId, StmtId>,
    fun_ret_overrides: HashMap<String, Ty>,
    ext_calls: HashMap<ExprId, (String, String, String)>,
    bridges: HashMap<String, Vec<BridgeSpec>>,
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

    fn push_local_funs(&mut self) { self.local_funs.push(HashMap::new()); }
    fn pop_local_funs(&mut self) { self.local_funs.pop(); }
    fn lookup_local_fun(&self, name: &str) -> Option<(StmtId, Signature)> {
        self.local_funs.iter().rev().find_map(|f| f.get(name).cloned())
    }
    fn register_local_fun(&mut self, name: &str, stmt_id: StmtId, sig: Signature) {
        if let Some(frame) = self.local_funs.last_mut() {
            frame.insert(name.to_string(), (stmt_id, sig));
        }
    }

    /// Resolve a syntactic type to a `Ty`, including declared class types (→ `Ty::Obj`).
    /// Nullability doesn't change the `Ty` for reference types (same JVM descriptor), but a nullable
    /// *primitive* (`Char?`, `Int?`, …) would need boxing — rejected (the file is skipped).
    fn resolve_ty(&mut self, r: &TypeRef) -> Ty {
        // Function type: `(A, B) -> R` — parsed with `fun_params` non-empty.
        if !r.fun_params.is_empty() || r.name == "<fun>" {
            let params: Vec<Ty> = r.fun_params.iter().map(|p| self.resolve_ty(p)).collect();
            let ret = r.arg.as_ref().map(|a| self.resolve_ty(a)).unwrap_or(Ty::Unit);
            return Ty::fun(params, ret);
        }
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
        } else if let Some(internal) = self.syms.class_names.get(&r.name) {
            // Built-in mapped types (`Number`, `Comparable`, `List`, …), classpath classes, and
            // type aliases — the *same* map emit resolves against, so the checker and codegen agree
            // (otherwise a leniently-`Error` type here becomes a real `Obj` in emit → VerifyError).
            // `"__ty/<Prim>"` encodes an alias to a primitive/builtin.
            match internal.strip_prefix("__ty/") {
                Some(prim) => Ty::from_name(prim).unwrap_or(Ty::Error),
                None => Ty::obj(internal),
            }
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
        let obj = Ty::obj("java/lang/Object");
        for (name, ssig) in &supers {
            let Some(impl_sig) = self.syms.method_of(internal, name) else { continue };
            let sp: String = ssig.params.iter().map(|t| t.descriptor()).collect();
            let ip: String = impl_sig.params.iter().map(|t| t.descriptor()).collect();
            let params_differ = sp != ip;
            let ret_differs = ssig.ret.descriptor() != impl_sig.ret.descriptor();
            // Each erased param must either equal the concrete one (passes through) or be `Object`
            // (the generic-erasure case — the bridge checkcasts a reference or unboxes a primitive).
            // A non-`Object` erased param that differs means `method_of` resolved the wrong overload.
            let params_bridgeable = ssig.params.len() == impl_sig.params.len()
                && ssig.params.iter().zip(&impl_sig.params).all(|(e, c)| e == c || *e == obj);
            if (params_differ || ret_differs)
                && params_bridgeable
                // A differing *return* that is primitive would need boxing in the bridge — skip.
                && !(ret_differs && impl_sig.ret.is_primitive())
            {
                // Record a synthetic bridge `name(erased)` that downcasts its args and delegates to
                // the concrete `name(impl)`. (Primitive params would need (un)boxing in the bridge —
                // left out of this pass.)
                self.bridges.entry(internal.to_string()).or_default().push(BridgeSpec {
                    name: name.clone(),
                    erased_params: ssig.params.clone(),
                    erased_ret: ssig.ret,
                    concrete_params: impl_sig.params.clone(),
                    concrete_ret: impl_sig.ret,
                });
            } else if params_differ {
                self.diags.error(span, format!("krusty: method '{name}' needs a bridge method (generic parameter override is not supported)"));
                return;
            } else if ret_differs {
                self.diags.error(span, format!("krusty: method '{name}' needs a bridge method (covariant/generic return override is not supported)"));
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
        // Two methods with the same name but DIFFERENT erased param-type signatures are
        // unresolvable by krusty's per-name symbol table — call sites will use the wrong overload.
        // Detect and skip rather than miscompile.
        let mut by_name: HashMap<String, String> = HashMap::new(); // name → first erased key
        let mut seen: HashMap<String, Span> = HashMap::new();
        for f in funs {
            let key = self.erased_sig_key(f);
            if seen.contains_key(&key) {
                self.diags.error(
                    f.span,
                    format!("conflicting overloads: function '{}' has the same JVM signature as another after type erasure", f.name),
                );
            } else {
                match by_name.entry(f.name.clone()) {
                    std::collections::hash_map::Entry::Occupied(e) if e.get() != &key => {
                        self.diags.error(
                            f.span,
                            format!("krusty: function '{}' has multiple overloads with different erased signatures (overload dispatch not supported)", f.name),
                        );
                    }
                    std::collections::hash_map::Entry::Vacant(e) => { e.insert(key.clone()); }
                    _ => {}
                }
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

    /// True if a subject `when` is exhaustive because the subject is an enum type and every
    /// declared entry is matched by a `EnumName.ENTRY` arm condition.
    fn when_enum_exhaustive(&self, subj_ty: Option<Ty>, arms: &[WhenArm]) -> bool {
        let Some(Ty::Obj(internal)) = subj_ty else { return false };
        // Find the enum's simple name (key in self.syms.enums) matching this internal name.
        let Some((_, entries)) = self.syms.enums.iter()
            .find(|(name, _)| self.syms.classes.get(*name).map_or(false, |c| c.internal == internal))
        else { return false };
        if entries.is_empty() { return false; }
        let mut covered: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for arm in arms {
            for &cnd in &arm.conditions {
                // Arm condition must be `EnumClass.ENTRY` — a member access on the enum class.
                if let Expr::Member { receiver, name: entry } = self.file.expr(cnd) {
                    if let Expr::Name(en) = self.file.expr(*receiver) {
                        if self.syms.classes.get(en).map_or(false, |c| c.internal == internal) {
                            covered.insert(entry);
                        }
                    }
                }
            }
        }
        entries.iter().all(|e| covered.contains(e.as_str()))
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
                    matches!(self.file.stmt(last), Stmt::Return(_) | Stmt::Break | Stmt::Continue)
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
        self.imports.get(name).cloned()
            .or_else(|| self.syms.classes.get(name).map(|c| c.internal.clone()))
            // Exception types resolve from the classpath: stdlib `TypeAliasesKt` aliases
            // (`Exception`, `RuntimeException`, …) and the ported `JavaToKotlinClassMap`
            // built-ins (`Throwable`) are both folded into `class_names`.
            .or_else(|| self.syms.class_names.get(name).cloned())
    }

    /// Resolve a type without emitting diagnostics (used for speculative smart-cast narrowing).
    fn resolve_ty_no_diag(&self, r: &TypeRef) -> Ty {
        if !r.fun_params.is_empty() || r.name == "<fun>" {
            let params: Vec<Ty> = r.fun_params.iter().map(|p| self.resolve_ty_no_diag(p)).collect();
            let ret = r.arg.as_ref().map(|a| self.resolve_ty_no_diag(a)).unwrap_or(Ty::Unit);
            return Ty::fun(params, ret);
        }
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
        if f.is_inline {
            self.diags.error(f.span, "krusty: inline functions are not supported");
            return;
        }
        // Extension function: look up in ext_funs table; set this_ty to the receiver type.
        let prev_this = self.this_ty;
        if let Some(recv_ref) = &f.receiver {
            let recv_ty = self.resolve_ty(recv_ref);
            self.this_ty = Some(recv_ty);
            let recv_desc = recv_ty.descriptor();
            self.ret_ty = self.syms.ext_funs.get(&(recv_desc, f.name.clone())).map(|s| s.ret)
                .or_else(|| f.ret.as_ref().map(|r| self.resolve_ty(r)))
                .unwrap_or(Ty::Unit);
        } else {
            // Use the collected signature's return type; for a companion method (not in `funs`) fall back
            // to the declared return type.
            self.ret_ty = match self.syms.funs.get(&f.name).map(|s| s.ret) {
                Some(r) => r,
                None => f.ret.as_ref().map(|r| self.resolve_ty(r)).unwrap_or(Ty::Unit),
            };
        }
        // For expression-body functions with no explicit return type, infer the return type from the
        // body expression and record it as an override (so codegen uses the right JVM descriptor).
        let infer_ret = f.ret.is_none() && self.ret_ty == Ty::Unit && matches!(&f.body, FunBody::Expr(_));
        // Default arguments are evaluated in the caller's context (they may not read other params —
        // enforced in collect_signatures), so check each in a fresh scope and populate its types.
        self.push_scope();
        for p in &f.params {
            if let Some(dx) = p.default {
                let pty = self.resolve_ty(&p.ty);
                let dty = self.expr(dx);
                self.expect_assignable(pty, dty, self.span(dx), "default argument");
            }
        }
        self.pop_scope();
        self.push_local_funs();
        self.push_scope();
        for p in &f.params {
            let ty = self.resolve_ty(&p.ty);
            let ty = if p.is_vararg { Ty::array(ty) } else { ty };
            self.declare(&p.name, ty, false);
        }
        if infer_ret {
            if let FunBody::Expr(e) = &f.body {
                let inferred = self.expr(*e);
                if inferred != Ty::Unit && inferred != Ty::Error {
                    self.ret_ty = inferred;
                    self.fun_ret_overrides.insert(f.name.clone(), inferred);
                }
            }
        } else {
            self.check_fun_body(f);
        }
        self.pop_scope();
        self.pop_local_funs();
        self.this_ty = prev_this;
    }

    /// Check an instance method: the class properties are visible (implicit `this`), then the
    /// method's own parameters shadow them.
    fn check_method(&mut self, f: &FunDecl, props: &[(String, Ty, bool)]) {
        if f.is_inline {
            self.diags.error(f.span, "krusty: inline functions are not supported");
            return;
        }
        let added: Vec<String> = f.type_params.iter().filter(|t| self.tparams.insert((*t).clone())).cloned().collect();
        self.ret_ty = f.ret.as_ref().map(|r| self.resolve_ty(r)).unwrap_or_else(|| {
            // For a method without an explicit return type (e.g. `override fun foo() = "Z"`),
            // use the return type that collect_signatures already inferred from the method body.
            if let Some(Ty::Obj(internal)) = self.this_ty {
                if let Some(sig) = self.syms.class_by_internal(internal).and_then(|c| c.methods.get(&f.name)) {
                    return sig.ret;
                }
            }
            Ty::Unit
        });
        self.push_local_funs();
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
        self.pop_local_funs();
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
        // `emptyArray()` is typed `Array<Null>` (bottom array) — assignable to any reference array;
        // the emitter materializes it with the target's element type.
        if let (Ty::Array(ae), Ty::Array(ee)) = (actual, expected) {
            if *ae == Ty::Null && ee.is_reference() {
                return;
            }
        }
        // An `Int` (typically a constant) is assignable to `Byte`/`Short` (Kotlin narrows integer
        // literals); codegen emits `i2b`/`i2s`. `Byte`/`Short` are interchangeable with `Int` here.
        if matches!(expected, Ty::Byte | Ty::Short) && matches!(actual, Ty::Int | Ty::Byte | Ty::Short) {
            return;
        }
        // Int/Byte/Short/Char are assignable to Long (integer widening); codegen emits i2l.
        if expected == Ty::Long && matches!(actual, Ty::Int | Ty::Byte | Ty::Short | Ty::Char) {
            return;
        }
        // Int/Byte/Short/Char/Long are assignable to Float/Double (widening); codegen emits i2f etc.
        if matches!(expected, Ty::Float | Ty::Double) && matches!(actual, Ty::Int | Ty::Long | Ty::Byte | Ty::Short | Ty::Char | Ty::Float) {
            return;
        }
        // In Kotlin every type is a subtype of `Any`/`Object`, and the top type narrows back to a
        // specific type by an unchecked cast. Both directions are assignable; the primitive-vs-boxed
        // *representation* (and any box/unbox or checkcast) is the backend's concern, decided at the
        // emit coercion site — not the type checker's. (`Unit` is excluded: it has no JVM value here.)
        if expected == Ty::obj("java/lang/Object") && actual != Ty::Unit {
            return;
        }
        if actual == Ty::obj("java/lang/Object") && expected != Ty::Unit {
            return;
        }
        // A class value is assignable to an interface (supertype) it implements.
        if let (Ty::Obj(e), Ty::Obj(a)) = (expected, actual) {
            if self.obj_is_subtype(a, e) {
                return;
            }
        }
        // Function types are assignable by arity — both lower to the same `FunctionN`; parameter and
        // return variance is handled by erasure/boxing at the JVM level (the call still recovers the
        // declared return type from `expected`).
        if let (Ty::Fun(e), Ty::Fun(a)) = (expected, actual) {
            if e.params.len() == a.params.len() {
                return;
            }
        }
        // Known classpath supertypes of `String` (`String : CharSequence, Comparable, Serializable`).
        if actual == Ty::String {
            if let Some(ei) = expected.obj_internal() {
                if matches!(ei, "java/lang/CharSequence" | "java/lang/Comparable" | "java/io/Serializable") {
                    return;
                }
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
            Expr::Lambda { params, body } => {
                // A lambda literal `{ a, b -> body }` — type is `Fun(arity)`. With no explicit
                // parameters but a body referencing `it`, bind the implicit single parameter.
                let bind_names: Vec<String> = if !params.is_empty() {
                    params.clone()
                } else if expr_uses_name(self.file, body, "it") {
                    vec!["it".to_string()]
                } else {
                    vec![]
                };
                let arity = bind_names.len() as u8;
                // Lambdas that are not inlined (let/also/run/apply) become closure classes and
                // cannot mutate the outer function's local variables. Detect this and reject.
                let outer_names: std::collections::HashSet<String> = self.scopes.iter()
                    .flat_map(|s| s.keys().cloned())
                    .collect();
                if !outer_names.is_empty() && lambda_body_writes_outer(self.file, body, &outer_names) {
                    self.diags.error(self.file.expr_spans[e.0 as usize],
                        "krusty: lambda captures a mutable local variable — not supported".to_string());
                    return Ty::fun(vec![Ty::obj("java/lang/Object"); arity as usize], Ty::Unit);
                }
                self.push_scope();
                for name in &bind_names {
                    self.declare(name, Ty::obj("java/lang/Object"), false);
                }
                // `field` does not propagate into a (non-inlined) lambda closure — krusty can't
                // emit a backing-field read from the lambda class. Clear it so `field` inside a
                // lambda body is unresolved (→ the property skips) rather than miscompiled.
                let saved_field = self.field_ty.take();
                let lc_before = self.local_call_map.len();
                let bret = self.expr(body);
                // A non-inlined lambda that calls a local function would dispatch it on the lambda
                // class (the local fun lives on the enclosing facade/class) — reject rather than
                // miscompile (the recursive nested-closure case).
                if self.local_call_map.len() > lc_before {
                    self.diags.error(self.file.expr_spans[e.0 as usize],
                        "krusty: a lambda that calls a local function is not supported".to_string());
                }
                self.field_ty = saved_field;
                self.pop_scope();
                // Params unknown here (no annotation) → erased `Object`; return type comes from the body.
                Ty::fun(vec![Ty::obj("java/lang/Object"); arity as usize], bret)
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
                // Nested try/catch trips a StackMapTable frame bug in codegen — skip rather than
                // emit a VerifyError.
                if expr_has_try(self.file, body) || catches.iter().any(|c| expr_has_try(self.file, c.body)) {
                    self.diags.error(self.span(e), "krusty: nested try/catch is not supported".to_string());
                }
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
                // A `Unit`-coerced elvis (`x ?: someUnitExpr`) trips a StackMapTable mismatch in
                // codegen (the branches push incompatible stack shapes) — skip rather than VerifyError.
                if rt == Ty::Unit {
                    self.diags.error(self.span(e), "krusty: elvis with a Unit right-hand side is not supported".to_string());
                }
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
                // User-defined extension on a non-nullable primitive receiver: safe call is a no-op
                // (primitives can never be null), so emit as a direct static call.
                if !rt.is_reference() {
                    let recv_desc = rt.descriptor();
                    if let Some(sig) = self.syms.ext_funs.get(&(recv_desc.clone(), name.clone())).cloned() {
                        let arg_tys: Vec<Ty> = match &args {
                            Some(a) => a.iter().map(|x| self.expr(*x)).collect(),
                            None => vec![],
                        };
                        if sig.params.len() != arg_tys.len() {
                            self.diags.error(self.span(e), format!("extension '{name}' expects {} args, got {}", sig.params.len(), arg_tys.len()));
                        }
                        let pdesc: String = sig.params.iter().map(|t| t.descriptor()).collect();
                        let desc = format!("({recv_desc}{pdesc}){}", sig.ret.descriptor());
                        self.ext_calls.insert(e, ("$local".to_string(), name.clone(), desc));
                        return self.set(e, sig.ret);
                    }
                }
                let result = match &args {
                    None => self.check_member(rt, &name, self.span(e)),
                    Some(a) => {
                        let arg_tys: Vec<Ty> = a.iter().map(|x| self.expr(*x)).collect();
                        if let ("toString", []) = (name.as_str(), arg_tys.as_slice()) {
                            Ty::String
                        } else if let ("hashCode", []) = (name.as_str(), arg_tys.as_slice()) {
                            Ty::Int // Int (not a reference), so safe-call rejection fires below
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
                // `field` inside an accessor body → the property's backing field. `field` is a soft
                // keyword: it only has this meaning when an accessor is being checked (and a real
                // local named `field` would have been found by `lookup` above).
                None if n == "field" && self.field_ty.is_some() => self.field_ty.unwrap(),
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
                    // Unqualified property of the implicit/extension receiver: `fun Box.f() = v`
                    // means `this.v` (sibling method calls already resolve via `this_ty`).
                    if let Some(Ty::Obj(internal)) = self.this_ty {
                        if let Some((ty, _)) = self.lookup_prop(internal, &n) {
                            return self.set(e, ty);
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
                // User-defined extension operator on a primitive receiver overrides built-in arithmetic.
                // Only applies to primitive receivers (reference receivers can't distinguish nullable vs
                // non-null at the krusty type level, risking infinite self-recursion in the body).
                if lt != Ty::Error && rt != Ty::Error && !lt.is_reference() {
                    let op_name = match op {
                        BinOp::Add => Some("plus"),
                        BinOp::Sub => Some("minus"),
                        BinOp::Mul => Some("times"),
                        BinOp::Div => Some("div"),
                        BinOp::Rem => Some("rem"),
                        _ => None,
                    };
                    if let Some(fname) = op_name {
                        let recv_desc = lt.descriptor();
                        if let Some(sig) = self.syms.ext_funs.get(&(recv_desc.clone(), fname.to_string())).cloned() {
                            if sig.params.len() == 1 {
                                let pdesc: String = sig.params.iter().map(|t| t.descriptor()).collect();
                                let desc = format!("({recv_desc}{pdesc}){}", sig.ret.descriptor());
                                self.ext_calls.insert(e, ("$local".to_string(), fname.to_string(), desc));
                                return self.set(e, sig.ret);
                            }
                        }
                    }
                }
                self.check_binary(op, lt, rt, self.span(e))
            }
            Expr::Member { receiver, name } => {
                // Primitive companion constants: `Int.MAX_VALUE`, `Long.MIN_VALUE`, etc.
                if let Expr::Name(prim) = self.file.expr(receiver).clone() {
                    if self.lookup(&prim).is_none() {
                        if let Some(ty) = prim_companion_ty(&prim, &name) {
                            return self.set(e, ty);
                        }
                    }
                }
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
            Expr::Call { callee, args } => self.check_call(e, callee, &args, self.span(e)),
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
                    None => {
                        // A block whose last statement always transfers control (break/continue/return)
                        // has type Nothing — it never produces a value or falls through.
                        if let Some(&last) = stmts.last() {
                            if matches!(self.file.stmt(last), Stmt::Return(_) | Stmt::Break | Stmt::Continue) {
                                Ty::Nothing
                            } else {
                                Ty::Unit
                            }
                        } else {
                            Ty::Unit
                        }
                    }
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
                            // subject form: condition must be comparable to the subject.
                            // `null` is always a valid condition (the branch simply never matches
                            // for non-nullable subjects; it may match for nullable ones).
                            Some(st) if ct != Ty::Null && st != Ty::Error && ct != Ty::Error && st != ct && Ty::promote(st, ct).is_none() => {
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
                // subject is a `sealed` type whose every subclass is matched by an `is` arm, or
                // its subject is an enum and every entry is covered by an `EnumName.ENTRY` arm.
                let exhaustive = has_else
                    || self.when_sealed_exhaustive(subj_ty, &arms)
                    || self.when_enum_exhaustive(subj_ty, &arms);
                if exhaustive {
                    result.unwrap_or(Ty::Unit)
                } else {
                    Ty::Unit
                }
            }
            Expr::CallableRef { receiver, name } => {
                // Class literal `UserType::class` → a `java.lang.Class`. Restricted to a declared
                // class name: primitive `Int::class` (needs `Integer.TYPE`) and bound `obj::class`
                // (needs `getClass()`) aren't modeled — skip them rather than emit a bad `ldc`.
                if name == "class" {
                    let is_user_type = matches!(receiver.map(|r| self.file.expr(r)), Some(Expr::Name(n)) if self.syms.classes.contains_key(n) && self.lookup(n).is_none());
                    if is_user_type {
                        return self.set(e, Ty::obj("java/lang/Class"));
                    }
                    self.diags.error(self.span(e), "krusty: this class-literal form is not supported".to_string());
                    return Ty::Error;
                }
                // Object-method callable references (`Any::equals`, `obj::toString`). A receiver that
                // names a value is *bound* (captures it, arity = method args); one that names a type
                // is *unbound* (the receiver becomes the first parameter).
                let obj = Ty::obj("java/lang/Object");
                if matches!(name.as_str(), "equals" | "hashCode" | "toString") {
                    let bound = match receiver {
                        Some(r) => matches!(self.file.expr(r), Expr::Name(n) if self.lookup(n).is_some()),
                        None => false,
                    };
                    if let Some(r) = receiver {
                        if bound { self.expr(r); } // type-check the captured receiver
                    }
                    let (margs, ret): (u8, Ty) = match name.as_str() {
                        "equals" => (1, Ty::Boolean),
                        "hashCode" => (0, Ty::Int),
                        _ => (0, Ty::String),
                    };
                    let arity = if bound { margs } else { margs + 1 };
                    return self.set(e, Ty::fun(vec![obj; arity as usize], ret));
                }
                // Top-level function reference `::foo` → `Fun(params, ret)` of that function.
                if receiver.is_none() {
                    if let Some(sig) = self.syms.funs.get(&name).cloned() {
                        if !sig.vararg && sig.params.len() == sig.required {
                            return self.set(e, Ty::fun(sig.params.clone(), sig.ret));
                        }
                    }
                    // Constructor reference `::ClassName` → `Fun(ctor_params, ClassName)`.
                    if !self.syms.objects.contains(&name) {
                        if let Some(cls) = self.syms.classes.get(&name).cloned() {
                            if !cls.is_annotation {
                                return self.set(e, Ty::fun(cls.ctor_params.clone(), Ty::obj(&cls.internal)));
                            }
                        }
                    }
                }
                // Method references on a user class: bound `obj::m` (receiver is a value, captured →
                // arity = method args) or unbound `Type::m` (receiver is the class → first parameter).
                if let Some(r) = receiver {
                    if let Expr::Name(rn) = self.file.expr(r).clone() {
                        // bound: `obj::m` where `obj` is an in-scope value
                        if let Some(loc) = self.lookup(&rn) {
                            if let Some(internal) = loc.ty.obj_internal() {
                                if let Some(sig) = self.syms.method_of(internal, &name) {
                                    if !sig.vararg && sig.params.len() == sig.required {
                                        self.expr(r); // capture the receiver
                                        return self.set(e, Ty::fun(sig.params.clone(), sig.ret));
                                    }
                                }
                            }
                        }
                        // unbound `Type::m` (skip objects: `O::m` is bound to the singleton, which
                        // emit doesn't model — it would be miscompiled as unbound).
                        if self.lookup(&rn).is_none() && !self.syms.objects.contains(&rn) {
                            if let Some(cls) = self.syms.classes.get(&rn).cloned() {
                                if let Some(sig) = cls.methods.get(&name).cloned() {
                                    if !sig.vararg && sig.params.len() == sig.required {
                                        let mut params = vec![Ty::obj(&cls.internal)];
                                        params.extend(sig.params.iter().copied());
                                        return self.set(e, Ty::fun(params, sig.ret));
                                    }
                                }
                            }
                        }
                    }
                }
                if let Some(recv) = receiver {
                    self.expr(recv);
                }
                self.diags.error(self.span(e), "krusty: callable references are not supported");
                Ty::Error
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
            BinOp::RefEq | BinOp::RefNe => Ty::Boolean,
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
        if fname == "emptyArray" && args.is_empty() {
            // `emptyArray<T>()` → a bottom reference array (`Array<Null>`), assignable to any reference
            // array; the emitter materializes it with the *target* element type.
            return Some(Ty::array(Ty::Null));
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
        // Size constructor: `IntArray(n)`, or `IntArray(n) { i -> elem }` with an init lambda whose
        // parameter is the index (`Int`).
        if let Some(elem) = Ty::primitive_array_element(fname) {
            if arg_tys.len() == 1 {
                self.expect_assignable(Ty::Int, arg_tys[0], self.span(args[0]), "array size");
                return Some(Ty::array(elem));
            }
            if arg_tys.len() == 2 && matches!(self.file.expr(args[1]), Expr::Lambda { .. }) {
                // `IntArray(n) { i -> … }` — index lambda inlined into a fill loop by the backend.
                self.expect_assignable(Ty::Int, arg_tys[0], self.span(args[0]), "array size");
                let _ = self.check_lambda_with_types(args[1], &[Ty::Int]); // `it`/index : Int
                return Some(Ty::array(elem));
            }
        }
        // `Array(n) { i -> elem }` — a reference array; its element type is the lambda's return
        // (boxed when primitive: `Array<Int>` is `Integer[]`).
        if fname == "Array" && arg_tys.len() == 2 && matches!(self.file.expr(args[1]), Expr::Lambda { .. }) {
            self.expect_assignable(Ty::Int, arg_tys[0], self.span(args[0]), "array size");
            let lam = self.check_lambda_with_types(args[1], &[Ty::Int]);
            let elem = lam.fun_ret().unwrap_or_else(|| Ty::obj("java/lang/Object"));
            // A nested-array element (`Array(n) { DoubleArray(m) }`) trips the loop-fill's
            // StackMapTable interaction with surrounding loops — skip rather than VerifyError.
            if matches!(elem, Ty::Array(_)) {
                self.diags.error(span, "krusty: Array(n) {…} with an array element is not supported".to_string());
                return Some(Ty::Error);
            }
            let ref_elem = match elem {
                Ty::Int => Ty::obj("java/lang/Integer"),
                Ty::Long => Ty::obj("java/lang/Long"),
                Ty::Double => Ty::obj("java/lang/Double"),
                Ty::Float => Ty::obj("java/lang/Float"),
                Ty::Boolean => Ty::obj("java/lang/Boolean"),
                Ty::Char => Ty::obj("java/lang/Character"),
                Ty::Byte => Ty::obj("java/lang/Byte"),
                Ty::Short => Ty::obj("java/lang/Short"),
                e => e,
            };
            return Some(Ty::array(ref_elem));
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
            ("TODO", [] | [_]) => {
                // `TODO()` throws `kotlin.NotImplementedError`; require it to be resolvable from the
                // classpath (stdlib), else emitting it would `NoClassDefFound` at runtime.
                if !self.syms.class_names.contains_key("NotImplementedError") {
                    self.diags.error(span, "krusty: 'TODO' requires the kotlin stdlib on the classpath".to_string());
                    return Some(Ty::Error);
                }
                Some(Ty::Nothing)
            }
            // kotlin.test assertions. assertEquals(expected, actual[, msg]); assertTrue/assertFalse
            // (cond[, msg]). All evaluate to Unit; an optional trailing message must be a String.
            ("assertEquals", [a, b] | [a, b, _]) => {
                // The two compared values must be a valid `==` pair (same numeric tower or both refs).
                let comparable = Ty::promote(*a, *b).is_some() || (a.is_reference() && b.is_reference());
                if !comparable {
                    self.diags.error(span, format!("krusty: assertEquals on incomparable types {a:?} and {b:?}"));
                }
                if let [_, _, msg] = arg_tys {
                    self.expect_assignable(Ty::String, *msg, self.span(args[2]), "message");
                }
                Some(Ty::Unit)
            }
            ("assertTrue" | "assertFalse", [cond] | [cond, _]) => {
                self.expect_assignable(Ty::Boolean, *cond, self.span(args[0]), "condition");
                if let [_, msg] = arg_tys {
                    self.expect_assignable(Ty::String, *msg, self.span(args[1]), "message");
                }
                Some(Ty::Unit)
            }
            ("require" | "check" | "assert" | "error" | "TODO" | "assertEquals" | "assertTrue" | "assertFalse", _) => {
                self.diags.error(span, format!("krusty: unsupported form of '{fname}'"));
                Some(Ty::Error)
            }
            _ => None,
        }
    }

    /// Type-check a `run`/`with`/`apply` lambda body with `recv` as its implicit receiver: `this` is
    /// `recv`, and the receiver's properties resolve unqualified. Returns the body's type.
    fn check_with_receiver(&mut self, recv: Ty, body: ExprId, span: Span) -> Ty {
        let Ty::Obj(internal) = recv else {
            if recv != Ty::Error {
                self.diags.error(span, "krusty: run/with/apply receiver must be a class instance".to_string());
            }
            return Ty::Error;
        };
        let prev_this = self.this_ty;
        self.this_ty = Some(recv);
        self.push_scope();
        // The receiver's properties are visible unqualified inside the body.
        if let Some(cs) = self.syms.class_by_internal(internal) {
            for (n, t, is_var) in cs.props.clone() {
                self.declare(&n, t, is_var);
            }
        }
        let bt = self.expr(body);
        self.pop_scope();
        self.this_ty = prev_this;
        bt
    }

    /// Check a lambda expression with explicit parameter types (for type-directed inference).
    /// When calling `f({ it.method() })` and `f`'s param is `(String) -> R`, this lets `it` have
    /// type `String` instead of the default `Object`.
    fn check_lambda_with_types(&mut self, e: ExprId, param_types: &[Ty]) -> Ty {
        if let Expr::Lambda { params, body } = self.file.expr(e).clone() {
            let bind_names: Vec<String> = if !params.is_empty() {
                params.clone()
            } else if !param_types.is_empty() || expr_uses_name(self.file, body, "it") {
                vec!["it".to_string()]
            } else {
                vec![]
            };
            self.push_scope();
            for (i, name) in bind_names.iter().enumerate() {
                let pty = param_types.get(i).copied().unwrap_or(Ty::obj("java/lang/Object"));
                self.declare(name, pty, false);
            }
            // `field` cannot be read from inside a lambda closure (see the `Expr::Lambda` arm).
            let saved_field = self.field_ty.take();
            let bret = self.expr(body);
            self.field_ty = saved_field;
            self.pop_scope();
            // Carry the declared parameter types and the inferred body return type.
            let ty = Ty::fun(param_types.to_vec(), bret);
            return self.set(e, ty);
        }
        self.expr(e)
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
        // Extension property: `recv.name` resolved by (receiver descriptor, name).
        if let Some((ty, _)) = self.syms.ext_props.get(&(rt.descriptor(), name.to_string())) {
            return *ty;
        }
        self.diags.error(span, format!("unresolved member '{name}' on '{}'", rt.name()));
        Ty::Error
    }

    fn check_call(&mut self, call: ExprId, callee: ExprId, args: &[ExprId], span: Span) -> Ty {
        // Named arguments (`f(x = 1)`) are only reordered for top-level function calls. Anywhere else
        // (methods, constructors, builtins) the labels would be silently ignored — reject instead.
        let arg_names = self.file.call_arg_names.get(&call.0).cloned();
        if arg_names.is_some() {
            let to_free_fn = matches!(self.file.expr(callee), Expr::Name(n) if self.syms.funs.contains_key(n));
            if !to_free_fn {
                for &a in args {
                    self.expr(a);
                }
                self.diags.error(span, "krusty: named arguments are only supported for top-level function calls".to_string());
                return Ty::Error;
            }
        }
        match self.file.expr(callee).clone() {
            // method call: recv.method(args)
            Expr::Member { receiver, name } => {
                // Qualified-name instantiation of a **classpath annotation**: `kotlin.SinceKotlin(…)`.
                // The whole callee is a dotted path naming an `@interface` on the classpath.
                if let Expr::Name(root) = self.file.expr(receiver).clone() {
                    if self.lookup(&root).is_none() {
                        if let Some(internal) = qualified_path(self.file, callee) {
                            if let Some(members) = classpath_annotation_members(&self.syms.classpath, &internal) {
                                for (i, a) in args.iter().enumerate() {
                                    let at = self.expr(*a);
                                    if let Some((_, pt)) = members.get(i) {
                                        self.expect_assignable(*pt, at, self.span(*a), "argument");
                                    }
                                }
                                return Ty::obj(&internal);
                            }
                        }
                    }
                }
                // Nested-class constructor `Outer.Inner(args)` (when `Outer` isn't a local).
                if let Expr::Name(outer) = self.file.expr(receiver).clone() {
                    if self.lookup(&outer).is_none() {
                        let qualified = format!("{outer}.{name}");
                        if let Some(cls) = self.syms.classes.get(&qualified).cloned() {
                            let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                            if cls.ctor_params.len() != arg_tys.len() {
                                self.diags.error(span, format!("constructor '{qualified}' expects {} args, got {}", cls.ctor_params.len(), arg_tys.len()));
                            } else {
                                for (i, (p, a)) in cls.ctor_params.iter().zip(&arg_tys).enumerate() {
                                    self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                                }
                            }
                            return Ty::obj(&cls.internal);
                        }
                    }
                }
                // Inlined scope functions `recv.let { … }` / `recv.also { … }`: bind the lambda's
                // parameter (default `it`) to the receiver; `let` yields the body, `also` the receiver.
                if matches!(name.as_str(), "let" | "also") && args.len() == 1 {
                    if let Expr::Lambda { params, body } = self.file.expr(args[0]).clone() {
                        let rt = self.expr(receiver);
                        self.push_scope();
                        self.declare(params.first().map(|s| s.as_str()).unwrap_or("it"), rt, false);
                        let bt = self.expr(body);
                        self.pop_scope();
                        return if name == "let" { bt } else { rt };
                    }
                }
                // `recv.run { … }` / `recv.apply { … }`: the lambda body has `recv` as its implicit
                // receiver (`this`); `run` yields the body, `apply` the receiver.
                if matches!(name.as_str(), "run" | "apply") && args.len() == 1 {
                    if let Expr::Lambda { params, body } = self.file.expr(args[0]).clone() {
                      if params.is_empty() {
                        let rt = self.expr(receiver);
                        let bt = self.check_with_receiver(rt, body, self.span(args[0]));
                        return if name == "run" { bt } else { rt };
                      }
                    }
                }
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
                                    // Default arguments on object/companion methods aren't filled by the
                                    // emitter yet, so the call must supply exactly the declared params.
                                    if sig.params.len() != arg_tys.len() {
                                        self.diags.error(span, format!("method '{cls}.{name}' expects {} args, got {}", sig.params.len(), arg_tys.len()));
                                    }
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
                // For a class method with function-type parameters, type lambda arguments against the
                // method's `lambda_param_types` (so `it` resolves), mirroring the free-function path.
                let method_sig = match rt {
                    Ty::Obj(internal) => self.lookup_method(internal, &name),
                    _ => None,
                };
                let arg_tys: Vec<Ty> = args.iter().enumerate().map(|(i, &a)| {
                    if let Some(ref sig) = method_sig {
                        if i < sig.lambda_param_types.len() && !sig.lambda_param_types[i].is_empty()
                            && matches!(self.file.expr(a), Expr::Lambda { .. }) {
                            let pt = sig.lambda_param_types[i].clone();
                            return self.check_lambda_with_types(a, &pt);
                        }
                    }
                    self.expr(a)
                }).collect();
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
                    // `trimIndent()`/`trimMargin()` — stdlib extensions; krusty folds them at compile
                    // time on a string-literal receiver (codegen rejects a non-literal receiver).
                    if matches!(name.as_str(), "trimIndent" | "trimMargin") && arg_tys.is_empty() {
                        return Ty::String;
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
                // Builtin bitwise/shift infix methods on `Int`/`Long` (`a shl b`, `a and b`, `a.inv()`).
                // These have no operator symbol in Kotlin (only the named form), so there's no
                // shadowing concern — resolve to the receiver's type. Shifts take an `Int` amount;
                // `and`/`or`/`xor` take the same type; `inv` is unary.
                if matches!(rt, Ty::Int | Ty::Long) {
                    if name == "inv" && arg_tys.is_empty() {
                        return rt;
                    }
                    if matches!(name.as_str(), "shl" | "shr" | "ushr" | "and" | "or" | "xor") && arg_tys.len() == 1 {
                        let expected = if matches!(name.as_str(), "shl" | "shr" | "ushr") { Ty::Int } else { rt };
                        self.expect_assignable(expected, arg_tys[0], self.span(args[0]), "argument");
                        return rt;
                    }
                }
                // A builtin operator-method on a primitive (`5.rem(2)`, `5.plus(2)`) binds to the
                // primitive operator, which *beats* any same-named user extension (in Kotlin a
                // member/builtin wins over an extension). krusty doesn't emit primitive
                // operator-methods, so reject rather than dispatch to the extension — which would
                // miscompile (e.g. `5.rem(2)` returning the extension's value instead of `1`).
                if rt.is_primitive() && is_builtin_operator_method(&name) {
                    self.diags.error(span, format!("krusty: builtin operator method '{name}' on a primitive is not supported"));
                    return Ty::Error;
                }
                // Extension / static method from any classpath library (e.g. Kotlin stdlib).
                // Receiver type is passed as the first argument (invokestatic at the JVM level).
                if let Some((owner, jvm_name, desc, ret)) = resolve_extension(&self.syms.classpath, rt, &name, &arg_tys) {
                    self.ext_calls.insert(call, (owner, jvm_name, desc));
                    return ret;
                }
                // User-defined extension function in this file (invokestatic on the file facade).
                {
                    let recv_desc = rt.descriptor();
                    if let Some(sig) = self.syms.ext_funs.get(&(recv_desc.clone(), name.clone())).cloned() {
                        if sig.params.len() != arg_tys.len() {
                            self.diags.error(span, format!("extension '{name}' expects {} args, got {}", sig.params.len(), arg_tys.len()));
                        } else {
                            for (i, (p, a)) in sig.params.iter().zip(&arg_tys).enumerate() {
                                self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                            }
                        }
                        // Full static descriptor: (receiver_desc + param_descs)ret_desc
                        let pdesc: String = sig.params.iter().map(|t| t.descriptor()).collect();
                        let desc = format!("({recv_desc}{pdesc}){}", sig.ret.descriptor());
                        self.ext_calls.insert(call, ("$local".to_string(), name.clone(), desc));
                        return sig.ret;
                    }
                }
                // `hashCode`/`toString`/`equals` are inherited from `Object` by every reference type.
                // (Function/lambda receivers excluded: their identity semantics need lambda-singleton
                // codegen krusty doesn't do yet — skip rather than miscompile a `.hashCode()` on one.)
                if rt.is_reference() && !matches!(rt, Ty::Fun(_)) {
                    match (name.as_str(), arg_tys.len()) {
                        ("hashCode", 0) => return Ty::Int,
                        ("toString", 0) => return Ty::String,
                        ("equals", 1) => return Ty::Boolean,
                        _ => {}
                    }
                }
                // `a.contentEquals(b)` / `a.contentHashCode()` / `a.isEmpty()` on arrays.
                if let Ty::Array(_) = rt {
                    match (name.as_str(), arg_tys.len()) {
                        ("contentEquals", 1) => return Ty::Boolean,
                        ("contentHashCode", 0) => return Ty::Int,
                        _ => {}
                    }
                }
                self.diags.error(span, format!("unresolved method '{name}' on '{}'", rt.name()));
                Ty::Error
            }
            // free function call: name(args)
            Expr::Name(fname) => {
                // Calling a local variable of function type: `val f: () -> String = { "OK" }; f()`.
                if let Some(local) = self.lookup(&fname) {
                    if let Ty::Fun(s) = local.ty {
                        let ret = s.ret;
                        let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                        let _ = arg_tys;
                        // The call yields the function type's real return type (erased to `Object` only
                        // at the JVM level by `FunctionN.invoke`, which the backend unboxes/casts).
                        return ret;
                    }
                }
                // Local function call — resolved before top-level funs and constructors.
                if let Some((stmt_id, sig)) = self.lookup_local_fun(&fname) {
                    let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                    if arg_tys.len() != sig.params.len() {
                        self.diags.error(span, format!("local function '{fname}' expects {} args, got {}", sig.params.len(), arg_tys.len()));
                    } else {
                        for (i, (p, a)) in sig.params.iter().zip(&arg_tys).enumerate() {
                            self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                        }
                    }
                    let ret = sig.ret;
                    self.local_call_map.insert(call, stmt_id);
                    return ret;
                }
                // `with(x) { … }` — `x` is the lambda body's implicit receiver (intercept before the
                // args are evaluated, since the trailing lambda isn't a normal value).
                if fname == "with" && args.len() == 2 && !self.syms.funs.contains_key(&fname) {
                    if let Expr::Lambda { params, body } = self.file.expr(args[1]).clone() {
                      if params.is_empty() {
                        let rt = self.expr(args[0]);
                        return self.check_with_receiver(rt, body, self.span(args[1]));
                      }
                    }
                }
                // Standalone `run { … }` — runs the (no-param) lambda body inline, yielding its value.
                if fname == "run" && args.len() == 1 && !self.syms.funs.contains_key(&fname) {
                    if let Expr::Lambda { params, body } = self.file.expr(args[0]).clone() {
                      if params.is_empty() {
                        self.push_scope();
                        let bt = self.expr(body);
                        self.pop_scope();
                        return bt;
                      }
                    }
                }
                // Type-directed lambda checking: if we know the target function's signature and a
                // parameter is a function type with known inner param types, check lambda args with
                // the correct `it` type instead of always using Object.
                let known_sig = self.syms.funs.get(&fname).cloned();
                // An array init constructor `IntArray(n) { i -> … }` / `Array(n) { i -> … }` types its
                // lambda's parameter (the index) as `Int`.
                let array_init_lambda = (Ty::primitive_array_element(&fname).is_some() || fname == "Array")
                    && args.len() == 2 && matches!(self.file.expr(args[1]), Expr::Lambda { .. });
                let arg_tys: Vec<Ty> = args.iter().enumerate().map(|(i, &a)| {
                    if array_init_lambda && i == 1 {
                        return self.check_lambda_with_types(a, &[Ty::Int]);
                    }
                    if let Some(ref sig) = known_sig {
                        if i < sig.lambda_param_types.len() && !sig.lambda_param_types[i].is_empty() {
                            if matches!(self.file.expr(a), Expr::Lambda { .. }) {
                                let pt = sig.lambda_param_types[i].clone();
                                return self.check_lambda_with_types(a, &pt);
                            }
                        }
                    }
                    self.expr(a)
                }).collect();
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
                        // Omitted trailing arguments are allowed when those parameters have a default
                        // that is a *simple literal of the parameter's exact type* — the call site can
                        // emit it directly. Adapting defaults (`Long = 0`) or complex defaults
                        // (anonymous objects, `emptyArray()`) aren't modeled yet → skip those.
                        let got = arg_tys.len();
                        let ok_arity = got <= ctor_params.len()
                            && (got..ctor_params.len()).all(|i| match cls.ctor_defaults.get(i).copied().flatten() {
                                Some(dx) => {
                                    let pt = ctor_params[i];
                                    match self.file.expr(dx) {
                                        Expr::IntLit(_) => matches!(pt, Ty::Int | Ty::Byte | Ty::Short | Ty::Char),
                                        Expr::LongLit(_) => pt == Ty::Long,
                                        Expr::DoubleLit(_) => pt == Ty::Double,
                                        Expr::FloatLit(_) => pt == Ty::Float,
                                        Expr::BoolLit(_) => pt == Ty::Boolean,
                                        Expr::CharLit(_) => pt == Ty::Char,
                                        Expr::StringLit(_) => pt == Ty::String,
                                        Expr::NullLit => pt.is_reference(),
                                        _ => false,
                                    }
                                }
                                None => false,
                            });
                        if !ok_arity {
                            self.diags.error(span, format!("constructor '{fname}' expects {} args, got {}", ctor_params.len(), got));
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
                    // An exception type by simple name (`throw RuntimeException("msg")`): resolved
                    // from the classpath (stdlib `TypeAliasesKt` alias / mapped `Throwable`), not a
                    // hardcoded list. Every JDK `Throwable` has both a no-arg and a single-`String`
                    // constructor, so those two arg shapes are accepted for a throwable-shaped type.
                    if let Some(internal) = self.syms.class_names.get(&fname).cloned() {
                        if crate::jvm::jvm_class_map::is_throwable_internal(&internal) && matches!(arg_tys.as_slice(), [] | [Ty::String]) {
                            return Ty::obj(&internal);
                        }
                    }
                    // `StringBuilder()` / `StringBuilder("init")` / `StringBuilder(capacity)`.
                    if fname == "StringBuilder" && matches!(arg_tys.as_slice(), [] | [Ty::String] | [Ty::Int]) {
                        return Ty::obj("java/lang/StringBuilder");
                    }
                    // `Any()` constructs java.lang.Object (Kotlin's root type).
                    if fname == "Any" && arg_tys.is_empty() {
                        return Ty::obj("java/lang/Object");
                    }
                }
                // Unqualified call to a sibling instance method: `foo()` → `this.foo()`.
                if !self.syms.funs.contains_key(&fname) {
                    if let Some(Ty::Obj(internal)) = self.this_ty {
                        if let Some(sig) = self.lookup_method(internal, &fname) {
                            for (i, (p, a)) in sig.params.iter().zip(&arg_tys).enumerate() {
                                self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                            }
                            return sig.ret;
                        }
                    }
                }
                match self.syms.funs.get(&fname) {
                    Some(sig) => {
                        let mut sig = sig.clone();
                        // Use the inferred return type from the checker's inference pass if the
                        // signature defaulted to Unit (no explicit return type annotation).
                        if let Some(&inferred) = self.fun_ret_overrides.get(&fname) {
                            sig.ret = inferred;
                        }
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
                        } else if let Some(names) = &arg_names {
                            // Named arguments: map onto positional slots, then type-check each slot.
                            match map_call_args(args, Some(names), &sig.param_names, sig.required) {
                                Ok(slots) => {
                                    for (i, slot) in slots.iter().enumerate() {
                                        if let Some(a) = slot {
                                            let aty = self.expr_types[a.0 as usize];
                                            self.expect_assignable(sig.params[i], aty, self.span(*a), "argument");
                                        }
                                    }
                                }
                                Err(msg) => self.diags.error(span, format!("call to '{fname}': {msg}")),
                            }
                        } else if arg_tys.len() < sig.required || arg_tys.len() > sig.params.len() {
                            let want = if sig.required == sig.params.len() {
                                format!("{}", sig.params.len())
                            } else {
                                format!("{} to {}", sig.required, sig.params.len())
                            };
                            self.diags.error(span, format!("function '{fname}' expects {want} args, got {}", arg_tys.len()));
                        } else {
                            // Supplied args match by position; omitted trailing params use their defaults.
                            for (i, a) in arg_tys.iter().enumerate() {
                                self.expect_assignable(sig.params[i], *a, self.span(args[i]), "argument");
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
                // Check if callee is a Fun type (any expression, e.g. a local variable).
                let callee_ty = self.expr(callee);
                let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                let _ = arg_tys;
                if let Ty::Fun(s) = callee_ty {
                    return s.ret;
                }
                if callee_ty != Ty::Error {
                    self.diags.error(span, "expression is not callable");
                }
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
        // `Unit` joins with `null` (or any other type in a discard context) as Unit.
        // This handles `if (cond) unitExpr else null` used as a statement.
        if a == Ty::Unit || b == Ty::Unit {
            return Ty::Unit;
        }
        // Numeric widening: Int is joinable with Long (result is Long).
        if matches!((a, b), (Ty::Int | Ty::Long, Ty::Int | Ty::Long)) {
            return Ty::Long;
        }
        self.diags.error(span, format!("incompatible if branches: '{}' and '{}'", a.name(), b.name()));
        Ty::Error
    }

    fn stmt(&mut self, s: StmtId) {
        match self.file.stmt(s).clone() {
            Stmt::Local { is_var, name, ty, init } => {
                // A local that shadows an in-scope name would alias the outer variable's slot in the
                // emitter (block exit doesn't restore shadowed slot mappings), so reject shadowing.
                if self.lookup(&name).is_some() {
                    self.diags.error(self.file.stmt_spans[s.0 as usize], format!("krusty: local '{name}' shadows an existing variable (not supported)"));
                }
                let declared = ty.as_ref().map(|r| self.resolve_ty(r));
                // A lambda initializer with a declared function type takes its parameter types from
                // the annotation, so `val f: (Int) -> Int = { it * 2 }` types `it`/`x` as `Int`
                // (not the erased `Object`). HOF *arguments* already do this.
                let it = match (declared, matches!(self.file.expr(init), Expr::Lambda { .. })) {
                    (Some(Ty::Fun(s)), true) => self.check_lambda_with_types(init, &s.params),
                    _ => self.expr(init),
                };
                let bind = match declared {
                    Some(d) => {
                        self.expect_assignable(d, it, self.span(init), "initializer");
                        d
                    }
                    None => it,
                };
                self.declare(&name, bind, is_var);
            }
            Stmt::Destructure { entries, init } => {
                let it = self.expr(init);
                let span = self.file.stmt_spans[s.0 as usize];
                // Destructuring requires the initializer to be a known reference type whose class
                // declares `component1..N` (e.g. a krusty `data class`). Anything else is rejected,
                // never miscompiled.
                let internal = it.obj_internal();
                for (idx, (name, is_var)) in entries.iter().enumerate() {
                    if name == "_" { continue; } // `_` skips this component (no binding, no call)
                    if self.lookup(name).is_some() {
                        self.diags.error(span, format!("krusty: local '{name}' shadows an existing variable (not supported)"));
                    }
                    let comp = format!("component{}", idx + 1);
                    let ty = internal.and_then(|i| self.syms.method_of(i, &comp)).map(|sig| sig.ret);
                    match ty {
                        Some(t) => self.declare(name, t, *is_var),
                        None => {
                            self.diags.error(span, format!("krusty: cannot destructure this type (no operator '{comp}')"));
                            self.declare(name, Ty::Error, *is_var);
                        }
                    }
                }
            }
            Stmt::IncDec { name, .. } => {
                // `inc`/`dec` are overloadable operators; krusty only models the built-in numeric
                // ones. The target must be a mutable numeric variable — a non-numeric type would
                // need a user `inc`/`dec` operator krusty doesn't support (reject, never miscompile).
                let span = self.file.stmt_spans[s.0 as usize];
                let found = self.lookup(&name).map(|l| (l.ty, l.is_var))
                    .or_else(|| self.syms.props.get(&name).copied());
                match found {
                    Some((ty, is_var)) => {
                        if !is_var {
                            self.diags.error(span, "val cannot be reassigned".to_string());
                        }
                        if !ty.is_numeric() {
                            self.diags.error(span, "krusty: '++'/'--' is only supported on a numeric variable".to_string());
                        }
                    }
                    None => self.diags.error(span, format!("unresolved reference: {name}")),
                }
            }
            Stmt::Assign { name, value } => {
                let vt = self.expr(value);
                // `field = …` inside a setter writes the backing field.
                if name == "field" && self.lookup(&name).is_none() && self.field_ty.is_some() {
                    let fty = self.field_ty.unwrap();
                    self.expect_assignable(fty, vt, self.file.stmt_spans[s.0 as usize], "assignment");
                } else { match self.lookup(&name) {
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
                } }
            }
            Stmt::AssignMember { receiver, name, value } => {
                let rt = self.expr(receiver);
                let vt = self.expr(value);
                let span = self.file.stmt_spans[s.0 as usize];
                // Extension-property write: `recv.name = value` for a `var` extension property.
                if let Some((lty, is_var)) = self.syms.ext_props.get(&(rt.descriptor(), name.clone())).copied() {
                    if !is_var {
                        self.diags.error(span, "val cannot be reassigned".to_string());
                    }
                    self.expect_assignable(lty, vt, span, "assignment");
                } else {
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
            Stmt::LocalFun(f) => {
                self.check_local_fun(&f.clone(), s);
            }
        }
    }

    /// Type-check a local function declaration (`fun` inside a function body). Non-capturing local
    /// functions are lifted to private static methods; captures are rejected.
    fn check_local_fun(&mut self, f: &FunDecl, stmt_id: StmtId) {
        let span = f.span;
        if !f.type_params.is_empty() {
            self.diags.error(span, "krusty: generic local functions are not supported".to_string());
            return;
        }
        // Collect outer local names (everything currently in scope that isn't one of f's params).
        let own_params: std::collections::HashSet<String> = f.params.iter().map(|p| p.name.clone()).collect();
        let outer_names: std::collections::HashSet<String> = self
            .scopes
            .iter()
            .flat_map(|s| s.keys())
            .filter(|n| !own_params.contains(*n))
            .cloned()
            .collect();

        // Detect captures by walking the body.
        if !outer_names.is_empty() {
            let captures = match &f.body {
                FunBody::Expr(e) | FunBody::Block(e) => local_fun_body_uses_any(self.file, *e, &outer_names),
                FunBody::None => false,
            };
            if captures {
                self.diags.error(span, "krusty: local functions that capture outer variables are not supported".to_string());
                return;
            }
        }

        // Add the local function's own type parameters (erased to Object, same as top-level funs).
        let added_tparams: Vec<String> = f.type_params.iter()
            .filter(|t| self.tparams.insert((*t).clone()))
            .cloned()
            .collect();

        // Resolve parameter types.
        let params: Vec<Ty> = f.params.iter().map(|p| {
            let t = self.resolve_ty(&p.ty);
            if p.is_vararg { Ty::array(t) } else { t }
        }).collect();

        // Resolve return type: explicit annotation, else infer from expression body.
        let ret_ty = if let Some(r) = &f.ret {
            self.resolve_ty(r)
        } else {
            match &f.body {
                FunBody::Expr(e) => {
                    // Check expression in isolation to infer return type (before registering sig).
                    self.push_local_funs();
                    self.push_scope();
                    for (p, &ty) in f.params.iter().zip(&params) {
                        self.declare(&p.name, ty, false);
                    }
                    let inferred = self.expr(*e);
                    self.pop_scope();
                    self.pop_local_funs();
                    inferred
                }
                _ => Ty::Unit,
            }
        };

        // Unique mangled JVM method name (StmtId is file-unique).
        let mangled = format!("$local${}", stmt_id.0);
        let sig = Signature {
            params: params.clone(),
            ret: ret_ty,
            vararg: f.params.last().map_or(false, |p| p.is_vararg),
            required: params.len(),
            param_names: f.params.iter().map(|p| p.name.clone()).collect(),
            lambda_param_types: Vec::new(),
        };

        // Register in current local-funs frame and in the TypeInfo maps.
        self.register_local_fun(&f.name, stmt_id, sig.clone());
        self.local_fun_sigs.insert(stmt_id, (mangled, sig.clone()));

        // Check the body (for a block body or when return type was already inferred above for expr).
        let prev_ret = self.ret_ty;
        self.ret_ty = ret_ty;
        self.push_local_funs();
        self.push_scope();
        for (p, &ty) in f.params.iter().zip(&params) {
            self.declare(&p.name, ty, false);
        }
        match &f.body.clone() {
            FunBody::Expr(e) => {
                // Already checked above for inference; re-check to fill in expr_types.
                let t = self.expr(*e);
                self.expect_assignable(ret_ty, t, self.span(*e), "local function body");
            }
            FunBody::Block(b) => {
                let _ = self.expr(*b);
            }
            FunBody::None => {}
        }
        self.pop_scope();
        self.pop_local_funs();
        self.ret_ty = prev_ret;
        for t in added_tparams {
            self.tparams.remove(&t);
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
    fn kotlin_test_assertions() {
        ok("import kotlin.test.*\nfun box(): String { assertEquals(4, 2+2); assertTrue(1<2); assertFalse(2<1); return \"OK\" }");
        ok("import kotlin.test.assertEquals\nfun box(): String { assertEquals(\"a\", \"a\", \"msg\"); return \"OK\" }");
        err_contains("import kotlin.test.*\nfun box(): String { assertTrue(5); return \"OK\" }", "Boolean was expected");
    }

    #[test]
    fn rejects_latent_miscompiles() {
        // Local shadowing (slot aliasing in the emitter).
        err_contains("fun box(): String { var x = 1; if (1>0) { var x = 2 }; return \"OK\" }", "shadows");
        // Init block that calls a member method before a later property initializer (init order).
        err_contains(
            "class Foo(v: Int) { init { set(v) }\n fun set(x: Int) { field = x }\n var field: Int = 0 }\nfun box(): String = \"OK\"",
            "init order",
        );
    }

    #[test]
    fn named_arguments() {
        // Accepted: named (any order), and named combined with an omitted default.
        ok("fun f(a: Int, b: Int): Int = a - b\nfun g(): Int = f(b = 2, a = 5)");
        ok("fun f(a: Int, b: Int = 10): Int = a + b\nfun g(): Int = f(a = 1)");
        // Rejected: unknown parameter name, and named args on a non-free-function call.
        err_contains("fun f(a: Int): Int = a\nfun g(): Int = f(z = 1)", "no parameter named 'z'");
        err_contains(
            "class C { fun m(a: Int): Int = a }\nfun g(): Int = C().m(a = 3)",
            "named arguments are only supported",
        );
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
