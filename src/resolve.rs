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
use crate::libraries::{EmptyLibrarySet, LibrarySet};
use crate::symbol_source::SymbolSource;
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
    /// For each parameter: whether it has a default value (so it may be omitted by name or position).
    /// Parallel to `params`; empty when unknown (callers then fall back to the `required` prefix count).
    /// This captures a *non-trailing* default (`f(x: Int = 3, g: () -> Int)`) that `required` cannot.
    pub param_defaults: Vec<bool>,
    /// Parameter names, parallel to `params`. Used to map named arguments (`f(x = 1)`) to positions.
    /// Empty for signatures where named-argument calls aren't supported (methods, synthesized members).
    pub param_names: Vec<String>,
    /// For each parameter: if the parameter is a function type `(A, B) -> R`, the inner parameter
    /// types `[A, B]`; otherwise an empty Vec. Used to type-check lambda arguments with the correct
    /// `it` / parameter types. Parallel to `params`.
    pub lambda_param_types: Vec<Vec<Ty>>,
    /// True for an `inline fun` — the lowerer expands its body at each call site (so a lambda
    /// argument may capture a mutable local), instead of forming a closure.
    pub is_inline: bool,
    /// True for a `final` member (a non-`open` member, or an explicit `final override`). A subclass —
    /// including a `data class` synthesizing `equals`/`hashCode`/`toString` — cannot override it.
    pub is_final: bool,
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
    /// `Some(outer_internal)` for an `inner class` — it captures the enclosing instance (a `this$0`
    /// field of the outer type); constructed as `outerInstance.Inner(...)`.
    pub inner_of: Option<String>,
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
    /// Secondary-constructor parameter-type lists (`constructor(p…) : this(…)`) — a construction call
    /// resolves to one of these when its arguments don't match the primary.
    pub secondary_ctors: Vec<Vec<Ty>>,
    /// This class's own type parameters, in declaration order (`class Box<T, U>` → `["T", "U"]`).
    /// Lets a member read substitute the receiver's type arguments for a property whose declared
    /// type is one of these parameters.
    pub tparam_names: Vec<String>,
    /// Properties whose *declared* type is exactly one of this class's type parameters, mapped to
    /// that parameter's index (`class Box<T>(val x: T)` → `{"x": 0}`). A read of such a property on
    /// `Box<Int>` substitutes the argument at that index (`Int`) for the erased `Object`.
    pub generic_props: HashMap<String, usize>,
    /// For a `@JvmInline value class X(val v: U)` — the sole underlying property's `(name, type U)`.
    /// A value-class value is represented unboxed as `U`; `X` carries static `box-impl`/`unbox-impl`/
    /// `constructor-impl` members for boxed contexts. `None` for an ordinary class.
    pub value_field: Option<(String, Ty)>,
}

impl ClassSig {
    pub fn prop(&self, name: &str) -> Option<(Ty, bool)> {
        self.props
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|(_, t, v)| (*t, *v))
    }
}

pub struct SymbolTable {
    /// Top-level functions by name. A name maps to ALL its overloads (Kotlin allows same-name functions
    /// distinguished by parameter signature); a call selects one via [`pick_overload`]. Most names have
    /// exactly one. Two overloads with the SAME erased parameter descriptors are a real JVM collision and
    /// are rejected at collection.
    pub funs: HashMap<String, Vec<Signature>>,
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
    /// The target's compiled library set — a JVM classpath or a klib (empty unless the driver
    /// supplies one). The front end resolves external references only through this abstraction.
    pub libraries: Box<dyn LibrarySet>,
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
    /// Top-level function name → the facade class it lives on (`helper` → `pkg/AKt`), for the WHOLE
    /// multi-file compilation. Populated only by the multi-file driver (which knows each file's
    /// stem/facade); empty for single-file/in-process callers. Lets `lower_file` emit a call to a
    /// function defined in ANOTHER file as a cross-facade `invokestatic` (`Callee::CrossFile`) instead
    /// of bailing. A function defined in the file being lowered is resolved locally first.
    pub fn_facades: HashMap<String, String>,
    /// Top-level property name → `(facade_internal, type, is_var)` across the WHOLE multi-file
    /// compilation. Populated only by the multi-file driver. A read of a property from ANOTHER file
    /// lowers to `invokestatic <facade>.getX()` (the field is private), a write to `setX(v)`. Empty for
    /// single-file callers; a property in the file being lowered is resolved locally (its static) first.
    pub prop_facades: HashMap<String, (String, Ty, bool)>,
}

impl Default for SymbolTable {
    fn default() -> SymbolTable {
        SymbolTable {
            funs: HashMap::new(),
            classes: HashMap::new(),
            props: HashMap::new(),
            computed_props: std::collections::HashSet::new(),
            objects: std::collections::HashSet::new(),
            enums: HashMap::new(),
            libraries: Box::new(EmptyLibrarySet),
            ext_funs: HashMap::new(),
            ext_props: HashMap::new(),
            class_names: HashMap::new(),
            fn_facades: HashMap::new(),
            prop_facades: HashMap::new(),
        }
    }
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
        let Some(c) = self.class_by_internal(internal) else {
            return;
        };
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
        let Some(c) = self.class_by_internal(internal) else {
            return;
        };
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
/// The erased JVM parameter descriptor of a signature (`(II)`-style, params only) — the key under which
/// two overloads of the same name would collide on the JVM. Used both to reject true duplicates at
/// collection and to find a selected overload's compiled method.
pub fn erased_params_key(sig: &Signature) -> String {
    sig.params.iter().map(|t| t.descriptor()).collect()
}

/// A loose, `self`-free argument-fit test for overload disambiguation: is a value of type `a` plausibly
/// passable where `p` is expected? Exact match, `Error`/`Nothing`/`null`→reference, numeric→numeric, and
/// any value→reference (boxing/upcast) fit; a reference is NOT passable to a primitive. Intentionally
/// permissive between two references (subtype isn't checked here) — it only needs to rank overloads, and
/// the same function runs in the checker and the lowerer so they always agree on the choice.
pub fn arg_assignable_simple(p: Ty, a: Ty) -> bool {
    if p == a || a == Ty::Error || p == Ty::Error || a == Ty::Nothing {
        return true;
    }
    if a == Ty::Null {
        return p.is_reference();
    }
    if p.is_numeric() && a.is_numeric() {
        return true;
    }
    // Any value (incl. a primitive, via boxing) is assignable to a reference; a reference is not
    // assignable to a primitive.
    p.is_reference()
}

/// Select the best-matching overload index among `sigs` for a call with the given argument types — the
/// SAME logic the checker and the lowerer both run, so they always resolve a call to the same function.
/// Filters by arity (respecting varargs and defaults), then scores by argument fit (exact match worth
/// more than a loose fit); a candidate with any non-fitting argument is dropped. Falls back to the first
/// arity-compatible candidate. `None` only if nothing matches the arity at all.
pub fn pick_overload(sigs: &[Signature], arg_tys: &[Ty]) -> Option<usize> {
    if sigs.len() == 1 {
        return Some(0);
    }
    let arity_ok = |s: &Signature| {
        if s.vararg {
            arg_tys.len() + 1 >= s.params.len()
        } else {
            arg_tys.len() >= s.required && arg_tys.len() <= s.params.len()
        }
    };
    let cands: Vec<usize> = (0..sigs.len()).filter(|&i| arity_ok(&sigs[i])).collect();
    if cands.len() <= 1 {
        return cands.first().copied();
    }
    // Soundness guard: krusty erases generics, so a generic value reads as `kotlin/Any`. If an argument
    // is the erased `Any` at a position where the candidates' parameter types DIFFER, krusty cannot
    // reproduce kotlinc's precise-type overload selection (kotlinc may see a concrete type there). Bail
    // (`None`) so the call is left unresolved and the file is skipped rather than dispatched wrongly.
    let any = Ty::obj("kotlin/Any");
    for (i, &a) in arg_tys.iter().enumerate() {
        if a == any {
            let mut params_here = cands.iter().filter_map(|&c| sigs[c].params.get(i));
            if let Some(first) = params_here.next() {
                if params_here.any(|p| p != first) {
                    return None;
                }
            }
        }
    }
    let score = |s: &Signature| -> Option<usize> {
        let mut sc = 0;
        for (&p, &a) in s.params.iter().zip(arg_tys.iter()) {
            if p == a {
                sc += 2;
            } else if arg_assignable_simple(p, a) {
                sc += 1;
            } else {
                return None;
            }
        }
        Some(sc)
    };
    let best = cands
        .iter()
        .filter_map(|&i| score(&sigs[i]).map(|sc| (sc, i)))
        .max_by_key(|&(sc, _)| sc)
        .map(|(_, i)| i);
    best.or_else(|| cands.first().copied())
}

pub fn conversion_target(name: &str) -> Option<Ty> {
    Some(match name {
        "toInt" => Ty::Int,
        "toByte" => Ty::Byte,
        "toShort" => Ty::Short,
        "toLong" => Ty::Long,
        "toFloat" => Ty::Float,
        "toDouble" => Ty::Double,
        "toChar" => Ty::Char,
        "toUInt" => Ty::UInt,
        "toULong" => Ty::ULong,
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
        Expr::Member { receiver, name } => {
            Some(format!("{}/{}", qualified_path(file, *receiver)?, name))
        }
        _ => None,
    }
}

/// Resolve the *result type* of a `kotlin.String` instance method by name + argument types — a
/// curated subset matching what the backend supports. The JVM method/descriptor it lowers to is the
/// backend's concern (the emitter uses Kotlin-named external calls), so only the Kotlin `Ty` lives
/// here; this keeps `java/lang/String` out of the front end.
pub fn resolve_string_instance(method: &str, arg_tys: &[Ty]) -> Option<Ty> {
    Some(match (method, arg_tys) {
        // NOTE: String CLASS members (`length`, `get`/`charAt`, `plus`, `compareTo`, `toString`,
        // `subSequence`, `equals`) are NOT listed here — they're resolved from the builtins declarations
        // (`Classpath::builtin_member_ret` ← `kotlin.kotlin_builtins`). This table is now only the stdlib
        // EXTENSIONS on `String`/`CharSequence` (`StringsKt`), a curated subset pending metadata sourcing.
        ("isEmpty", []) | ("isBlank", []) => Ty::Boolean,
        ("substring", [Ty::Int]) | ("substring", [Ty::Int, Ty::Int]) => Ty::String,
        ("indexOf", [Ty::String]) | ("indexOf", [Ty::Char]) => Ty::Int,
        ("lastIndexOf", [Ty::String]) | ("lastIndexOf", [Ty::Char]) => Ty::Int,
        ("contains", [Ty::String]) => Ty::Boolean,
        ("startsWith", [Ty::String]) | ("endsWith", [Ty::String]) => Ty::Boolean,
        ("concat", [Ty::String]) => Ty::String,
        ("replace", [Ty::String, Ty::String]) => Ty::String,
        ("uppercase", []) | ("toUpperCase", []) => Ty::String,
        ("lowercase", []) | ("toLowerCase", []) => Ty::String,
        ("trim", []) => Ty::String,
        ("toString", []) => Ty::String,
        ("toCharArray", []) => Ty::array(Ty::Char),
        _ => return None,
    })
}

/// Resolve the *result type* of a `kotlin.text.StringBuilder` instance method (a curated subset).
/// `append`/`appendLine` return the builder (chainable); `toString`/`length` as expected.
pub fn resolve_stringbuilder_instance(method: &str, arg_tys: &[Ty]) -> Option<Ty> {
    let sb = Ty::obj("java/lang/StringBuilder");
    Some(match (method, arg_tys) {
        ("toString", []) => Ty::String,
        ("length", []) => Ty::Int,
        ("append", [_]) => sb,
        ("appendLine", [_] | []) => sb,
        _ => return None,
    })
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
    collect_signatures_with_cp(files, Box::new(EmptyLibrarySet), diags)
}

/// Like `collect_signatures` but also seeds class names and type aliases from the target's
/// libraries (a JVM classpath, a klib), eliminating the need for any hardcoded type lists.
pub fn collect_signatures_with_cp(
    files: &[File],
    libraries: Box<dyn LibrarySet>,
    diags: &mut DiagSink,
) -> SymbolTable {
    // The library set's type universe: importable names + type aliases (and intrinsic built-in maps).
    let seed = libraries.seed();

    // Pass 1: every class simple-name -> internal name (no bodies, just the type universe).
    // Pre-seed from the library type index so imports/stdlib types are visible.
    let mut class_names: HashMap<String, String> = seed.class_names;
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
    // (The library set's `seed` already merged the intrinsic Kotlin built-in → target class mapping,
    // e.g. the ported `JavaToKotlinClassMap`, beneath any classpath/user declarations.)

    // Expand type aliases into class_names.
    // `typealias A = B` where B is a user-defined class → A resolves to the same internal name.
    // `typealias A = Primitive` → A maps to `"__ty/<PrimName>"` (decoded in ty_of_ref).
    // `typealias A = java.lang.Foo` → A resolves to the JVM internal name `java/lang/Foo`.
    // Multiple passes handle chains: A = B, B = C.
    //
    // Seed from classpath type aliases (read from @kotlin.Metadata in *TypeAliasesKt.class files)
    // then from any user-defined typealiases in the input files.
    let mut alias_map: HashMap<String, String> = seed.type_aliases;
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
                        let tp: std::collections::HashSet<String> =
                            f.type_params.iter().cloned().collect();
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
                    let tp: std::collections::HashSet<String> =
                        f.type_params.iter().cloned().collect();
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
                                // For an extension function, bind `this` to the receiver type so a body
                                // using it (`fun Int.double() = this * 2`) infers correctly.
                                let this_scope: Vec<(String, Ty, bool)> = f
                                    .receiver
                                    .as_ref()
                                    .map(|r| {
                                        vec![(
                                            "this".to_string(),
                                            ty_of_ref(r, &class_names, &tp, diags),
                                            false,
                                        )]
                                    })
                                    .unwrap_or_default();
                                let t =
                                    infer_lit_ty_p(file, *e, &class_names, &fun_rets, &this_scope);
                                if t != Ty::Error {
                                    t
                                } else if let Expr::Name(n) = file.expr(*e) {
                                    // Body is a bare parameter name (`fun f(x: T) = x`): infer T.
                                    f.params
                                        .iter()
                                        .find(|p| &p.name == n)
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
                        f.params
                            .iter()
                            .rev()
                            .take_while(|p| p.default.is_some())
                            .count()
                    };
                    let required = f.params.len() - trailing_defaults;
                    // Call-site substitution copies the default expression to the caller, so a default
                    // that reads another parameter can't be reproduced there — reject such functions.
                    let pnames: std::collections::HashSet<&str> =
                        f.params.iter().map(|p| p.name.as_str()).collect();
                    for p in &f.params {
                        if let Some(dx) = p.default {
                            if expr_refs_param(file, dx, &pnames) {
                                diags.error(f.span, "krusty: a default argument that references another parameter is not supported");
                            }
                        }
                    }
                    let lambda_param_types: Vec<Vec<Ty>> = f
                        .params
                        .iter()
                        .map(|p| {
                            if !p.ty.fun_params.is_empty() || p.ty.name == "<fun>" {
                                p.ty.fun_params
                                    .iter()
                                    .map(|r| ty_of_ref(r, &class_names, &tp, diags))
                                    .collect()
                            } else {
                                Vec::new()
                            }
                        })
                        .collect();
                    let sig = Signature {
                        params,
                        ret,
                        vararg,
                        required,
                        param_defaults: f.params.iter().map(|p| p.default.is_some()).collect(),
                        param_names: f.params.iter().map(|p| p.name.clone()).collect(),
                        lambda_param_types,
                        is_inline: f.is_inline,
                        is_final: f.is_final,
                    };
                    if let Some(recv_ref) = &f.receiver {
                        // Extension function: index by (receiver_descriptor, method_name).
                        let recv_ty = ty_of_ref(recv_ref, &class_names, &tp, diags);
                        // A nullable reference receiver (`fun String?.foo()`) erases to the same
                        // descriptor as the non-null form, so krusty (whose `Ty` drops nullability)
                        // can't pick between a `String.foo` and a `String?.foo` at the call site. An
                        // ordinary-named lone overload is unambiguous and supported. But an *operator*
                        // name (`String?.plus`) shadows the builtin/member operator: with nullability
                        // erased, krusty would route every `String + …` (even a non-null one) to the
                        // extension, recursing infinitely when the body uses the same operator. kotlinc
                        // resolves member-over-extension by static nullability, which krusty can't — so
                        // reject nullable-reference operator extensions (and any null/non-null collision).
                        let is_operator = is_builtin_operator_method(&f.name)
                            || matches!(
                                f.name.as_str(),
                                "equals"
                                    | "not"
                                    | "get"
                                    | "set"
                                    | "contains"
                                    | "invoke"
                                    | "iterator"
                                    | "getValue"
                                    | "setValue"
                                    | "provideDelegate"
                            );
                        if recv_ref.nullable && recv_ty.is_reference() && is_operator {
                            diags.error(f.span, "krusty: an operator extension on a nullable reference receiver is not supported".to_string());
                        } else if table
                            .ext_funs
                            .insert((recv_ty.descriptor(), f.name.clone()), sig)
                            .is_some()
                        {
                            // Two extensions with the same erased receiver + name (a duplicate, or a
                            // nullable/non-null pair) can't be told apart at the call site under
                            // nullability erasure — reject rather than silently pick one.
                            diags.error(f.span, "krusty: conflicting extension functions with the same erased receiver and name".to_string());
                        }
                    } else {
                        // Overloading: keep ALL same-name functions, keyed by name. Only an EXACT
                        // erased-parameter duplicate (same JVM descriptor) is a real conflict.
                        let key = erased_params_key(&sig);
                        let overloads = table.funs.entry(f.name.clone()).or_default();
                        if overloads.iter().any(|s| erased_params_key(s) == key) {
                            diags.error(f.span, format!("conflicting declarations: {}", f.name));
                        } else {
                            overloads.push(sig);
                        }
                    }
                }
                Decl::Class(c) => {
                    let internal = class_names
                        .get(&c.name)
                        .cloned()
                        .unwrap_or_else(|| class_internal(file, &c.name));
                    let ctp: std::collections::HashSet<String> =
                        c.type_params.iter().cloned().collect();
                    // An `init` block that calls an own member method *before* a later property
                    // initializer runs has subtle init-order semantics (cf. KT-73355) krusty doesn't
                    // model — the helper may observe/overwrite a not-yet-initialized field. Reject it.
                    let own_methods: std::collections::HashSet<&str> =
                        c.methods.iter().map(|m| m.name.as_str()).collect();
                    let is_own_call = |ce: ExprId| matches!(file.expr(ce), Expr::Call { callee, .. } if matches!(file.expr(*callee), Expr::Name(n) if own_methods.contains(n.as_str())));
                    if let Some(last_prop) = c
                        .init_order
                        .iter()
                        .rposition(|i| matches!(i, ClassInit::PropInit(_)))
                    {
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
                    let ctor_params: Vec<Ty> = c
                        .props
                        .iter()
                        .map(|p| ty_of_ref(&p.ty, &class_names, &ctp, diags))
                        .collect();
                    let ctor_defaults: Vec<Option<ExprId>> =
                        c.props.iter().map(|p| p.default).collect();
                    // Only `val`/`var` params (+ body props) are backing-field properties.
                    let mut props: Vec<(String, Ty, bool)> = c
                        .props
                        .iter()
                        .filter(|p| p.is_property)
                        .map(|p| {
                            (
                                p.name.clone(),
                                ty_of_ref(&p.ty, &class_names, &ctp, diags),
                                p.is_var,
                            )
                        })
                        .collect();
                    // Body properties (`class C { val x = … }`) are also fields/accessors. A computed
                    // property (custom getter, no annotation) infers its type from the getter body.
                    // Initializer scope: ALL primary-ctor params (property or not — they're in scope for a
                    // property initializer) plus each preceding body property, so `val y = x*2` sees the
                    // ctor param `x` and `val z = y+1` sees the earlier `y`.
                    let mut init_scope: Vec<(String, Ty, bool)> = c
                        .props
                        .iter()
                        .map(|p| {
                            (
                                p.name.clone(),
                                ty_of_ref(&p.ty, &class_names, &ctp, diags),
                                p.is_var,
                            )
                        })
                        .collect();
                    for bp in &c.body_props {
                        let ty = match (&bp.ty, &bp.getter) {
                            (Some(r), _) => ty_of_ref(r, &class_names, &ctp, diags),
                            (None, Some(FunBody::Expr(g))) => {
                                let locals: HashMap<&str, Ty> = init_scope
                                    .iter()
                                    .map(|(n, t, _)| (n.as_str(), *t))
                                    .collect();
                                infer_getter_ty(file, *g, &locals)
                            }
                            (None, _) => bp
                                .init
                                .map(|i| {
                                    infer_lit_ty_p(file, i, &class_names, &fun_rets, &init_scope)
                                })
                                .unwrap_or(Ty::Error),
                        };
                        if ty == Ty::Error && bp.init.is_some() && bp.ty.is_none() {
                            diags.error(bp.span, format!("krusty: cannot infer the type of property '{}'; add an explicit type", bp.name));
                        }
                        props.push((bp.name.clone(), ty, bp.is_var));
                        init_scope.push((bp.name.clone(), ty, bp.is_var));
                    }
                    // An inner class's methods can read the enclosing instance's properties (via
                    // `this$0`); add the outer class's backing-field properties so an expression-bodied
                    // inner method (`fun box() = s`) infers its return type from them.
                    if let Some(outer) = &c.inner_of {
                        if let Some(oc) = file
                            .decls
                            .iter()
                            .filter_map(|&d| match file.decl(d) {
                                Decl::Class(x) => Some(x),
                                _ => None,
                            })
                            .find(|x| x.name == *outer)
                        {
                            for p in oc.props.iter().filter(|p| p.is_property) {
                                props.push((
                                    p.name.clone(),
                                    ty_of_ref(&p.ty, &class_names, &ctp, diags),
                                    p.is_var,
                                ));
                            }
                        }
                    }
                    // A subclass's expression-bodied methods can reference INHERITED backing-field
                    // properties (`fun f() = x` where `x` is declared in a base class), so add the
                    // superclass chain's properties to the return-type inference scope.
                    let mut sup = c.base_class.clone();
                    let mut guard = 0;
                    while let Some(bn) = sup {
                        guard += 1;
                        if guard > 32 {
                            break;
                        }
                        let Some(bc) = file
                            .decls
                            .iter()
                            .filter_map(|&d| match file.decl(d) {
                                Decl::Class(x) => Some(x),
                                _ => None,
                            })
                            .find(|x| x.name == bn)
                        else {
                            break;
                        };
                        for p in bc.props.iter().filter(|p| p.is_property) {
                            props.push((
                                p.name.clone(),
                                ty_of_ref(&p.ty, &class_names, &ctp, diags),
                                p.is_var,
                            ));
                        }
                        for bp in &bc.body_props {
                            let ty = match &bp.ty {
                                Some(r) => ty_of_ref(r, &class_names, &ctp, diags),
                                None => bp
                                    .init
                                    .map(|i| infer_lit_ty_p(file, i, &class_names, &fun_rets, &[]))
                                    .unwrap_or(Ty::Error),
                            };
                            if ty != Ty::Error {
                                props.push((bp.name.clone(), ty, bp.is_var));
                            }
                        }
                        sup = bc.base_class.clone();
                    }
                    // Sibling/inherited method returns (explicit annotations) so a method with an inferred
                    // expression body can resolve a call to another method of this class or a superclass
                    // (`fun b() = a()` where `a(): Int`). Own methods take precedence over a superclass's.
                    let mut local_rets = fun_rets.clone();
                    let mut sup_m = c.base_class.clone();
                    let mut gm = 0;
                    while let Some(bn) = sup_m {
                        gm += 1;
                        if gm > 32 {
                            break;
                        }
                        let Some(bc) = file
                            .decls
                            .iter()
                            .filter_map(|&d| match file.decl(d) {
                                Decl::Class(x) => Some(x),
                                _ => None,
                            })
                            .find(|x| x.name == bn)
                        else {
                            break;
                        };
                        for m in &bc.methods {
                            if let Some(r) = &m.ret {
                                local_rets.insert(
                                    m.name.clone(),
                                    ty_of_ref(r, &class_names, &ctp, diags),
                                );
                            }
                        }
                        sup_m = bc.base_class.clone();
                    }
                    for m in &c.methods {
                        if let Some(r) = &m.ret {
                            local_rets
                                .insert(m.name.clone(), ty_of_ref(r, &class_names, &ctp, diags));
                        }
                    }
                    let mut methods: HashMap<String, Signature> = c
                        .methods
                        .iter()
                        .map(|m| {
                            let mut mtp = ctp.clone();
                            mtp.extend(m.type_params.iter().cloned());
                            let params: Vec<Ty> = m
                                .params
                                .iter()
                                .map(|p| ty_of_ref(&p.ty, &class_names, &mtp, diags))
                                .collect();
                            let ret = m
                                .ret
                                .as_ref()
                                .map(|r| ty_of_ref(r, &class_names, &mtp, diags))
                                .unwrap_or_else(|| {
                                    if let FunBody::Expr(e) = &m.body {
                                        // The method's own parameters are in scope for its expression body, so
                                        // `fun m(x: Int) = x + 1` infers `Int`. Parameters come FIRST: a
                                        // parameter shadows a class property of the same name in the body (the
                                        // scope lookup returns the first match), matching Kotlin.
                                        let mut scope: Vec<(String, Ty, bool)> = m
                                            .params
                                            .iter()
                                            .map(|p| {
                                                (
                                                    p.name.clone(),
                                                    ty_of_ref(&p.ty, &class_names, &mtp, diags),
                                                    false,
                                                )
                                            })
                                            .collect();
                                        scope.extend(props.iter().cloned());
                                        let t = infer_lit_ty_p(
                                            file,
                                            *e,
                                            &class_names,
                                            &local_rets,
                                            &scope,
                                        );
                                        if t != Ty::Error {
                                            return t;
                                        }
                                    }
                                    // The overridable members `Comparable.compareTo`/`Any.equals`/`hashCode`
                                    // have a Kotlin-CONTRACT return type (`Int`/`Boolean`/`Int`) the body must
                                    // conform to — use it when the body can't be inferred locally (a body like
                                    // `compareTo(o) = v - o.v` references the parameter, which the literal
                                    // inference can't resolve). kotlinc fixes these signatures.
                                    match (m.name.as_str(), m.params.len()) {
                                        ("compareTo", 1) => Ty::Int,
                                        ("equals", 1) => Ty::Boolean,
                                        ("hashCode", 0) => Ty::Int,
                                        _ => Ty::Unit,
                                    }
                                });
                            (m.name.clone(), {
                                let lambda_param_types: Vec<Vec<Ty>> = m
                                    .params
                                    .iter()
                                    .map(|p| {
                                        if !p.ty.fun_params.is_empty() || p.ty.name == "<fun>" {
                                            p.ty.fun_params
                                                .iter()
                                                .map(|r| ty_of_ref(r, &class_names, &mtp, diags))
                                                .collect()
                                        } else {
                                            Vec::new()
                                        }
                                    })
                                    .collect();
                                Signature {
                                    params,
                                    ret,
                                    vararg: false,
                                    required: m
                                        .params
                                        .iter()
                                        .take_while(|p| p.default.is_none())
                                        .count(),
                                    param_defaults: m
                                        .params
                                        .iter()
                                        .map(|p| p.default.is_some())
                                        .collect(),
                                    param_names: m.params.iter().map(|p| p.name.clone()).collect(),
                                    lambda_param_types,
                                    is_inline: false,
                                    is_final: m.is_final,
                                }
                            })
                        })
                        .collect();
                    // `data class` synthesizes componentN() + copy(props...) callable members.
                    if c.is_data {
                        let self_ty = Ty::obj(&internal);
                        for (i, (_, ty, _)) in props.iter().enumerate() {
                            methods.insert(
                                format!("component{}", i + 1),
                                Signature {
                                    params: vec![],
                                    ret: *ty,
                                    vararg: false,
                                    required: 0,
                                    param_defaults: Vec::new(),
                                    param_names: Vec::new(),
                                    lambda_param_types: Vec::new(),
                                    is_inline: false,
                                    is_final: true,
                                },
                            );
                        }
                        // Every `copy` parameter has a default (the receiver's property) — so `required`
                        // is 0 and any subset may be passed, by name or position.
                        methods.insert(
                            "copy".into(),
                            Signature {
                                params: props.iter().map(|(_, t, _)| *t).collect(),
                                ret: self_ty,
                                vararg: false,
                                required: 0,
                                param_defaults: vec![true; props.len()],
                                param_names: props.iter().map(|(n, _, _)| n.clone()).collect(),
                                lambda_param_types: Vec::new(),
                                is_inline: false,
                                is_final: true,
                            },
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
                    let interfaces: Vec<String> =
                        c.supertypes.iter().map(&mut resolve_super).collect();
                    let super_internal = c.base_class.as_ref().map(|b| resolve_super(b));
                    // `companion object` members → static methods/props on this class.
                    let static_methods: HashMap<String, Signature> = c
                        .companion_methods
                        .iter()
                        .map(|m| {
                            let mut mtp = ctp.clone();
                            mtp.extend(m.type_params.iter().cloned());
                            let params: Vec<Ty> = m
                                .params
                                .iter()
                                .map(|p| ty_of_ref(&p.ty, &class_names, &mtp, diags))
                                .collect();
                            let ret = m
                                .ret
                                .as_ref()
                                .map(|r| ty_of_ref(r, &class_names, &mtp, diags))
                                .unwrap_or_else(|| {
                                    if let FunBody::Expr(e) = &m.body {
                                        let t = infer_lit_ty(file, *e, &class_names, &fun_rets);
                                        if t != Ty::Error {
                                            t
                                        } else {
                                            Ty::Unit
                                        }
                                    } else {
                                        Ty::Unit
                                    }
                                });
                            (m.name.clone(), {
                                let lambda_param_types: Vec<Vec<Ty>> = m
                                    .params
                                    .iter()
                                    .map(|p| {
                                        if !p.ty.fun_params.is_empty() || p.ty.name == "<fun>" {
                                            p.ty.fun_params
                                                .iter()
                                                .map(|r| ty_of_ref(r, &class_names, &mtp, diags))
                                                .collect()
                                        } else {
                                            Vec::new()
                                        }
                                    })
                                    .collect();
                                Signature {
                                    params,
                                    ret,
                                    vararg: false,
                                    required: m
                                        .params
                                        .iter()
                                        .take_while(|p| p.default.is_none())
                                        .count(),
                                    param_defaults: m
                                        .params
                                        .iter()
                                        .map(|p| p.default.is_some())
                                        .collect(),
                                    param_names: m.params.iter().map(|p| p.name.clone()).collect(),
                                    lambda_param_types,
                                    is_inline: false,
                                    is_final: m.is_final,
                                }
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
                    // Record which properties are declared as a bare type parameter, so a read on a
                    // generic instantiation can substitute the corresponding type argument. A nullable
                    // parameter (`T?`) is skipped — substituting a primitive there would need boxing.
                    let tparam_names = c.type_params.clone();
                    let tparam_index = |r: &TypeRef| -> Option<usize> {
                        if r.nullable || !r.targs.is_empty() || r.arg.is_some() {
                            return None;
                        }
                        tparam_names.iter().position(|t| *t == r.name)
                    };
                    let mut generic_props: HashMap<String, usize> = HashMap::new();
                    for p in c.props.iter().filter(|p| p.is_property) {
                        if let Some(i) = tparam_index(&p.ty) {
                            generic_props.insert(p.name.clone(), i);
                        }
                    }
                    for bp in &c.body_props {
                        if let Some(r) = &bp.ty {
                            if let Some(i) = tparam_index(r) {
                                generic_props.insert(bp.name.clone(), i);
                            }
                        }
                    }
                    let secondary_ctors: Vec<Vec<Ty>> = c
                        .secondary_ctors
                        .iter()
                        .map(|sc| {
                            sc.params
                                .iter()
                                .map(|p| ty_of_ref(&p.ty, &class_names, &ctp, diags))
                                .collect()
                        })
                        .collect();
                    // An `inner class`'s outer internal name is its own internal minus the trailing
                    // `$Inner` (it was hoisted as `Outer.Inner` → `Outer$Inner`).
                    let inner_of = c
                        .inner_of
                        .as_ref()
                        .and_then(|_| internal.rsplit_once('$').map(|(o, _)| o.to_string()));
                    // A `value class` is represented unboxed as its sole property's type.
                    let value_field = if c.is_value {
                        props.first().map(|(n, t, _)| (n.clone(), *t))
                    } else {
                        None
                    };
                    table.classes.insert(
                        c.name.clone(),
                        ClassSig {
                            internal,
                            props,
                            ctor_params,
                            methods,
                            is_interface: c.is_interface,
                            is_sealed: c.is_sealed,
                            inner_of,
                            static_methods,
                            static_props,
                            lateinit_props,
                            interfaces,
                            super_internal,
                            is_annotation: c.is_annotation,
                            ctor_defaults,
                            secondary_ctors,
                            tparam_names,
                            generic_props,
                            value_field,
                        },
                    );
                }
                Decl::Property(p) => {
                    // Extension property `val Recv.name: T get() = …`: register by (receiver
                    // descriptor, name); emitted as a static `getName(Recv)`/`setName(Recv, T)`.
                    if let Some(recv_ref) = &p.receiver {
                        let recv_ty = ty_of_ref(recv_ref, &class_names, &Default::default(), diags);
                        let ty =
                            p.ty.as_ref()
                                .map(|r| ty_of_ref(r, &class_names, &Default::default(), diags))
                                .or_else(|| match &p.getter {
                                    Some(FunBody::Expr(g)) => {
                                        Some(infer_lit_ty(file, *g, &class_names, &fun_rets))
                                    }
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
                        diags.error(
                            p.span,
                            "krusty: top-level property custom accessors are not supported"
                                .to_string(),
                        );
                    }
                    // Type from the annotation, else a light inference from a literal initializer (or,
                    // for a computed property, from its expression getter body).
                    let ty = match (&p.ty, &p.getter) {
                        (Some(r), _) => ty_of_ref(r, &class_names, &Default::default(), diags),
                        (None, Some(FunBody::Expr(g))) if is_computed => {
                            infer_lit_ty(file, *g, &class_names, &fun_rets)
                        }
                        (None, _) => p
                            .init
                            .map(|i| infer_lit_ty(file, i, &class_names, &fun_rets))
                            .unwrap_or(Ty::Error),
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

    table.libraries = libraries;
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
    param_defaults: &[bool],
) -> Result<Vec<Option<ExprId>>, String> {
    let n = param_names.len();
    let mut slots: Vec<Option<ExprId>> = vec![None; n];
    let mut pos = 0usize;
    let mut seen_named = false;
    for (i, &a) in args.iter().enumerate() {
        match names.and_then(|ns| ns.get(i)).and_then(|o| o.as_ref()) {
            Some(nm) => {
                seen_named = true;
                let idx = param_names
                    .iter()
                    .position(|p| p == nm)
                    .ok_or_else(|| format!("no parameter named '{nm}'"))?;
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
    // A parameter must be supplied unless it has a default. With per-parameter default info, check each
    // slot individually (so a required parameter that FOLLOWS a defaulted one is validated correctly);
    // otherwise fall back to the `required`-prefix count (defaults assumed trailing).
    for (i, slot) in slots.iter().enumerate() {
        let has_default = if param_defaults.is_empty() {
            i >= required
        } else {
            param_defaults.get(i).copied().unwrap_or(false)
        };
        if slot.is_none() && !has_default {
            return Err(format!(
                "no value passed for required parameter '{}'",
                param_names.get(i).map(|s| s.as_str()).unwrap_or("?")
            ));
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
    match file.stmt(s) {
        // `name++`/`name = …` reference `name` (a write-only capture still binds it); a local function
        // is a separate scope (stop). The assigned `value` is still visited via the fall-through below
        // for `Assign`, so a `name = name + 1` is covered too.
        Stmt::IncDec { name, .. } => names.contains(name.as_str()),
        Stmt::Assign { name, value } => {
            names.contains(name.as_str()) || expr_refs_param(file, *value, names)
        }
        Stmt::LocalFun(_) => false,
        _ => file.any_child_stmt(s, &mut |c| expr_refs_param(file, c, names)),
    }
}

/// Whether `e`'s subtree contains a `try` expression (used to reject *nested* try/catch, which hits
/// a StackMapTable frame bug in codegen).
/// Kotlin's built-in read-only/mutable collection types remap a handful of member names onto their JVM
/// `java.util` methods (the compiler's "mapped members"): `Map.keys`→`keySet`, `Map.entries`→`entrySet`.
/// `values`/`size` keep their JVM name, so only the renamed ones need listing. Returns the JVM accessor.
pub(crate) fn collection_mapped_accessor(name: &str) -> Option<&'static str> {
    match name {
        "keys" => Some("keySet"),
        "entries" => Some("entrySet"),
        _ => None,
    }
}

fn expr_has_try(file: &File, e: ExprId) -> bool {
    match file.expr(e) {
        Expr::Try { .. } => true,
        // A `try` inside a lambda body is its own scope — not a *nested* try in the codegen sense.
        Expr::Lambda { .. } => false,
        _ => file.any_child_expr(e, &mut |c| expr_has_try(file, c), &mut |s| {
            stmt_has_try(file, s)
        }),
    }
}

fn stmt_has_try(file: &File, s: StmtId) -> bool {
    file.any_child_stmt(s, &mut |c| expr_has_try(file, c))
}

/// Whether any `try` within `e` (inclusive) carries a `finally`. A `finally` is inlined at each exit of
/// its protected region; combined with nested `try`s (overlapping exception ranges) that duplication
/// trips a verify error, so the checker rejects a nested-try structure that contains any `finally`.
fn expr_has_finally(file: &File, e: ExprId) -> bool {
    match file.expr(e) {
        // `finally.is_some()` already covers a `finally` at this node; recurse only into the bodies that
        // could hold a *deeper* `try`-with-`finally` (the `finally` block itself included via its own try).
        Expr::Try {
            body,
            catches,
            finally,
        } => {
            finally.is_some()
                || expr_has_finally(file, *body)
                || catches.iter().any(|c| expr_has_finally(file, c.body))
        }
        Expr::Lambda { .. } => false,
        _ => file.any_child_expr(e, &mut |c| expr_has_finally(file, c), &mut |s| {
            file.any_child_stmt(s, &mut |c| expr_has_finally(file, c))
        }),
    }
}

/// Whether `break`/`continue` appears in a position krusty's backend can't yet emit: in *value*
/// position (its value would be consumed while operands sit on the stack — an operand-spill the
/// emitter doesn't do), inside a `try` (the jump must cross exception regions / run `finally`), or
/// inside a lambda (a non-local jump). `forbidden` is true once the walk is in such a context.
/// Plain `break`/`continue` *statements* in a loop body or `if`/`when` statement branch are fine.
fn bc_complex_e(file: &File, e: ExprId, forbidden: bool) -> bool {
    let v = |x: ExprId| bc_complex_e(file, x, true);
    match file.expr(e) {
        // Pure leaves (a `CallableRef` receiver can't carry a loop jump) — never complex.
        Expr::Name(_)
        | Expr::IntLit(_)
        | Expr::LongLit(_)
        | Expr::DoubleLit(_)
        | Expr::FloatLit(_)
        | Expr::BoolLit(_)
        | Expr::StringLit(_)
        | Expr::CharLit(_)
        | Expr::NullLit
        | Expr::CallableRef { .. } => false,
        // A lambda body's `break`/`continue` would be a non-local jump (unsupported) — forbid throughout.
        Expr::Lambda { body, .. } => bc_complex_e(file, *body, true),
        // The condition/subject is a value; branches inherit the current context (a value `if`/`when`
        // makes its branches values; a statement `if` keeps them statements).
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            v(*cond)
                || bc_complex_e(file, *then_branch, forbidden)
                || else_branch.map_or(false, |x| bc_complex_e(file, x, forbidden))
        }
        Expr::When { subject, arms } => {
            subject.map_or(false, |s| v(s))
                || arms.iter().any(|a| {
                    a.conditions.iter().any(|&c| v(c)) || bc_complex_e(file, a.body, forbidden)
                })
        }
        Expr::Block { stmts, trailing } => {
            stmts.iter().any(|&s| bc_complex_s(file, s, forbidden))
                || trailing.map_or(false, |t| bc_complex_e(file, t, forbidden))
        }
        // Inside a `try`, any `break`/`continue` must cross the region — forbid throughout.
        Expr::Try {
            body,
            catches,
            finally,
        } => {
            bc_complex_e(file, *body, true)
                || catches.iter().any(|c| bc_complex_e(file, c.body, true))
                || finally.map_or(false, |f| bc_complex_e(file, f, true))
        }
        // Every other expression evaluates its children as *values* (forbidden context).
        _ => file.any_child_expr(e, &mut |c| v(c), &mut |s| bc_complex_s(file, s, true)),
    }
}

fn bc_complex_s(file: &File, s: StmtId, forbidden: bool) -> bool {
    let v = |x: ExprId| bc_complex_e(file, x, true);
    match file.stmt(s) {
        Stmt::Break(_) | Stmt::Continue(_) => forbidden,
        Stmt::Local { init, .. }
        | Stmt::Destructure { init, .. }
        | Stmt::Assign { value: init, .. } => v(*init),
        Stmt::AssignMember {
            receiver, value, ..
        } => v(*receiver) || v(*value),
        Stmt::AssignIndex {
            array,
            index,
            value,
        } => v(*array) || v(*index) || v(*value),
        Stmt::Return(Some(e), _) => v(*e),
        Stmt::Return(None, _) | Stmt::IncDec { .. } => false,
        // A statement's value is discarded — its (possibly `if`/`when`) tree stays in statement position.
        Stmt::Expr(e) => bc_complex_e(file, *e, false),
        Stmt::While { cond, body, .. } | Stmt::DoWhile { cond, body, .. } => {
            v(*cond) || bc_complex_e(file, *body, false)
        }
        Stmt::For { range, body, .. } => {
            v(range.start)
                || v(range.end)
                || range.step.map_or(false, |s| v(s))
                || bc_complex_e(file, *body, false)
        }
        Stmt::ForEach { iterable, body, .. } => v(*iterable) || bc_complex_e(file, *body, false),
        // A local function is a separate body — `break`/`continue` in it would be non-local.
        Stmt::LocalFun(f) => match &f.body {
            FunBody::Expr(e) | FunBody::Block(e) => bc_complex_e(file, *e, true),
            FunBody::None => false,
        },
    }
}

fn expr_refs_param(file: &File, e: ExprId, names: &std::collections::HashSet<&str>) -> bool {
    match file.expr(e) {
        Expr::Name(n) => names.contains(n.as_str()),
        // A lambda introduces a new `it` scope — stop (its captures are handled elsewhere).
        Expr::Lambda { .. } => false,
        _ => file.any_child_expr(e, &mut |c| expr_refs_param(file, c, names), &mut |s| {
            stmt_refs_param(file, s, names)
        }),
    }
}

/// Returns true if the expression subtree (or any statement within it) references a name from
/// `outer`. Used to detect captures in local function bodies before allowing lift-to-static.
fn local_fun_body_uses_any(
    file: &File,
    e: ExprId,
    outer: &std::collections::HashSet<String>,
) -> bool {
    fn check_e(file: &File, e: ExprId, outer: &std::collections::HashSet<String>) -> bool {
        match file.expr(e) {
            Expr::Name(n) => outer.contains(n),
            _ => file.any_child_expr(e, &mut |c| check_e(file, c, outer), &mut |s| {
                check_s(file, s, outer)
            }),
        }
    }
    fn check_s(file: &File, s: StmtId, outer: &std::collections::HashSet<String>) -> bool {
        match file.stmt(s) {
            Stmt::IncDec { name, .. } => outer.contains(name),
            Stmt::LocalFun(_) => false, // nested local funs have their own capture check
            _ => file.any_child_stmt(s, &mut |c| check_e(file, c, outer)),
        }
    }
    check_e(file, e, outer)
}

/// Returns `true` if an expression subtree contains a `Stmt::Assign` (or `+=`-style via
/// `AssignMember` on a Name) whose target is a `Name` that appears in `outer_names`.
/// Used to detect mutable captures in non-inlined lambda bodies.
fn lambda_body_writes_outer(
    file: &File,
    e: ExprId,
    outer_names: &std::collections::HashSet<String>,
) -> bool {
    fn check_e(file: &File, e: ExprId, outer_names: &std::collections::HashSet<String>) -> bool {
        let r = |x: ExprId| check_e(file, x, outer_names);
        let rs = |x: StmtId| check_s(file, x, outer_names);
        match file.expr(e) {
            Expr::Block { stmts, trailing } => {
                stmts.iter().any(|&s| rs(s)) || trailing.map_or(false, |t| r(t))
            }
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => r(*cond) || r(*then_branch) || else_branch.map_or(false, |x| r(x)),
            Expr::Try {
                body,
                catches,
                finally,
            } => r(*body) || catches.iter().any(|c| r(c.body)) || finally.map_or(false, |f| r(f)),
            Expr::When { subject, arms } => {
                subject.map_or(false, |s| r(s))
                    || arms
                        .iter()
                        .any(|a| a.conditions.iter().any(|&c| r(c)) || r(a.body))
            }
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
            Stmt::AssignMember {
                receiver, value, ..
            } => r(*receiver) || r(*value),
            Stmt::AssignIndex {
                array,
                index,
                value,
            } => r(*array) || r(*index) || r(*value),
            Stmt::Return(Some(e), _) => r(*e),
            Stmt::Return(None, _) | Stmt::Break(_) | Stmt::Continue(_) => false,
            Stmt::While { cond, body, .. } | Stmt::DoWhile { cond, body, .. } => {
                r(*cond) || r(*body)
            }
            Stmt::For { range, body, .. } => {
                r(range.start) || r(range.end) || range.step.map_or(false, |s| r(s)) || r(*body)
            }
            Stmt::ForEach { iterable, body, .. } => r(*iterable) || r(*body),
            Stmt::Expr(e) => r(*e),
            Stmt::LocalFun(_) => false,
        }
    }
    check_e(file, e, outer_names)
}

/// Collect the outer-variable names a lambda body writes (assigns / `++`/`--`), so the lowerer can box
/// them. Mirrors [`lambda_body_writes_outer`] but accumulates the names instead of returning a bool;
/// like it, does not descend into nested lambdas (their writes are recorded when they're checked).
fn collect_lambda_outer_writes(
    file: &File,
    e: ExprId,
    outer_names: &std::collections::HashSet<String>,
    out: &mut std::collections::HashSet<String>,
) {
    fn ce(
        file: &File,
        e: ExprId,
        outer: &std::collections::HashSet<String>,
        out: &mut std::collections::HashSet<String>,
    ) {
        match file.expr(e) {
            Expr::Block { stmts, trailing } => {
                for &s in stmts {
                    cs(file, s, outer, out);
                }
                if let Some(t) = trailing {
                    ce(file, *t, outer, out);
                }
            }
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                ce(file, *cond, outer, out);
                ce(file, *then_branch, outer, out);
                if let Some(x) = else_branch {
                    ce(file, *x, outer, out);
                }
            }
            Expr::Try {
                body,
                catches,
                finally,
            } => {
                ce(file, *body, outer, out);
                for c in catches {
                    ce(file, c.body, outer, out);
                }
                if let Some(f) = finally {
                    ce(file, *f, outer, out);
                }
            }
            Expr::When { subject, arms } => {
                if let Some(s) = subject {
                    ce(file, *s, outer, out);
                }
                for a in arms {
                    for &c in &a.conditions {
                        ce(file, c, outer, out);
                    }
                    ce(file, a.body, outer, out);
                }
            }
            Expr::Lambda { .. } => {}
            _ => {}
        }
    }
    fn cs(
        file: &File,
        s: StmtId,
        outer: &std::collections::HashSet<String>,
        out: &mut std::collections::HashSet<String>,
    ) {
        match file.stmt(s) {
            Stmt::IncDec { name, .. } => {
                if outer.contains(name) {
                    out.insert(name.clone());
                }
            }
            Stmt::Assign { name, value } => {
                if outer.contains(name) {
                    out.insert(name.clone());
                }
                ce(file, *value, outer, out);
            }
            Stmt::Local { init, .. } | Stmt::Destructure { init, .. } => {
                ce(file, *init, outer, out)
            }
            Stmt::AssignMember {
                receiver, value, ..
            } => {
                ce(file, *receiver, outer, out);
                ce(file, *value, outer, out);
            }
            Stmt::AssignIndex {
                array,
                index,
                value,
            } => {
                ce(file, *array, outer, out);
                ce(file, *index, outer, out);
                ce(file, *value, outer, out);
            }
            Stmt::Return(Some(e), _) => ce(file, *e, outer, out),
            Stmt::While { cond, body, .. } | Stmt::DoWhile { cond, body, .. } => {
                ce(file, *cond, outer, out);
                ce(file, *body, outer, out);
            }
            Stmt::For { range, body, .. } => {
                ce(file, range.start, outer, out);
                ce(file, range.end, outer, out);
                if let Some(st) = range.step {
                    ce(file, st, outer, out);
                }
                ce(file, *body, outer, out);
            }
            Stmt::ForEach { iterable, body, .. } => {
                ce(file, *iterable, outer, out);
                ce(file, *body, outer, out);
            }
            Stmt::Expr(e) => ce(file, *e, outer, out),
            Stmt::Return(None, _) | Stmt::Break(_) | Stmt::Continue(_) | Stmt::LocalFun(_) => {}
        }
    }
    ce(file, e, outer_names, out);
}

/// Collect every name reassigned (`=`, `+=`-style, `++`/`--`) anywhere in `e`'s subtree — INCLUDING
/// inside nested lambdas and local functions (a `var` reassigned in a sibling closure still needs the
/// box). Used to decide which captured `var`s must be boxed.
fn collect_all_reassigned(file: &File, e: ExprId, out: &mut std::collections::HashSet<String>) {
    // Traverse via `any_child_expr`/`any_child_stmt` (which visit EVERY child, including lambda and
    // local-function bodies) so no expression form can hide a reassignment from the scan. The closures
    // only collect (return `false` to keep visiting); a `RefCell` lets both share the accumulator.
    let cell = std::cell::RefCell::new(std::mem::take(out));
    fn ce(file: &File, e: ExprId, cell: &std::cell::RefCell<std::collections::HashSet<String>>) {
        if let Expr::IncDec { target, .. } = file.expr(e) {
            if let Expr::Name(n) = file.expr(*target) {
                cell.borrow_mut().insert(n.clone());
            }
        }
        file.any_child_expr(
            e,
            &mut |c| {
                ce(file, c, cell);
                false
            },
            &mut |s| {
                cs(file, s, cell);
                false
            },
        );
    }
    fn cs(file: &File, s: StmtId, cell: &std::cell::RefCell<std::collections::HashSet<String>>) {
        if let Stmt::Assign { name, .. } | Stmt::IncDec { name, .. } = file.stmt(s) {
            cell.borrow_mut().insert(name.clone());
        }
        file.any_child_stmt(s, &mut |c| {
            ce(file, c, cell);
            false
        });
    }
    ce(file, e, &cell);
    *out = cell.into_inner();
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
                BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::Eq
                | BinOp::Ne
                | BinOp::And
                | BinOp::Or
                | BinOp::RefEq
                | BinOp::RefNe => Ty::Boolean,
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
        (
            "Float",
            "MAX_VALUE" | "MIN_VALUE" | "NaN" | "POSITIVE_INFINITY" | "NEGATIVE_INFINITY",
        ) => Some(Ty::Float),
        (
            "Double",
            "MAX_VALUE" | "MIN_VALUE" | "NaN" | "POSITIVE_INFINITY" | "NEGATIVE_INFINITY",
        ) => Some(Ty::Double),
        _ => None,
    }
}

/// Best-effort type of a simple literal initializer (for an unannotated top-level property).
/// Names of Kotlin's primitive operator/bitwise/conversion-overloadable methods. An explicit call
/// of one of these on a primitive receiver binds to the builtin operator, not a user extension.
fn is_builtin_operator_method(name: &str) -> bool {
    matches!(
        name,
        "plus"
            | "minus"
            | "times"
            | "div"
            | "rem"
            | "mod"
            | "inc"
            | "dec"
            | "unaryPlus"
            | "unaryMinus"
            | "and"
            | "or"
            | "xor"
            | "inv"
            | "shl"
            | "shr"
            | "ushr"
            | "compareTo"
            | "rangeTo"
    )
}

/// The augmented-assignment operator name for a binary op (`+=` → `plusAssign`, etc.). Only the five
/// arithmetic compound operators have an `…Assign` form.
fn assign_op_name(op: BinOp) -> Option<&'static str> {
    Some(match op {
        BinOp::Add => "plusAssign",
        BinOp::Sub => "minusAssign",
        BinOp::Mul => "timesAssign",
        BinOp::Div => "divAssign",
        BinOp::Rem => "remAssign",
        _ => return None,
    })
}

fn infer_lit_ty(
    file: &File,
    e: ExprId,
    class_names: &HashMap<String, String>,
    fun_rets: &HashMap<String, Ty>,
) -> Ty {
    infer_lit_ty_p(file, e, class_names, fun_rets, &[])
}

/// As [`infer_lit_ty`], but with the enclosing class's properties in scope so an expression-bodied
/// member (`fun get() = v`, where `v` is a constructor property) infers the property's type.
/// The primitive return type of a Kotlin primitive-conversion method (`toByte`/`toInt`/…), or `None`
/// for any other name. The conversions are total and receiver-independent, so a property initializer
/// `2.toByte()` infers to `Byte` without resolving the receiver.
fn prim_conversion_ret(name: &str) -> Option<Ty> {
    Some(match name {
        "toByte" => Ty::Byte,
        "toShort" => Ty::Short,
        "toInt" => Ty::Int,
        "toLong" => Ty::Long,
        "toFloat" => Ty::Float,
        "toDouble" => Ty::Double,
        "toChar" => Ty::Char,
        _ => return None,
    })
}

fn infer_lit_ty_p(
    file: &File,
    e: ExprId,
    class_names: &HashMap<String, String>,
    fun_rets: &HashMap<String, Ty>,
    props: &[(String, Ty, bool)],
) -> Ty {
    match file.expr(e) {
        Expr::IntLit(_) => Ty::Int,
        Expr::LongLit(_) => Ty::Long,
        Expr::DoubleLit(_) => Ty::Double,
        Expr::FloatLit(_) => Ty::Float,
        Expr::BoolLit(_) => Ty::Boolean,
        Expr::CharLit(_) => Ty::Char,
        Expr::StringLit(_) | Expr::Template(_) => Ty::String,
        // A bare name referring to a property (or `this` — the receiver of an expression-bodied
        // extension function `fun Int.double() = this * 2`, supplied as a `"this"` scope entry).
        Expr::Name(n) => props
            .iter()
            .find(|(pn, _, _)| pn == n)
            .map(|(_, t, _)| *t)
            .unwrap_or(Ty::Error),
        Expr::Member { receiver, name } => {
            if let Expr::Name(prim) = file.expr(*receiver) {
                prim_companion_ty(prim, name).unwrap_or(Ty::Error)
            } else {
                Ty::Error
            }
        }
        Expr::Unary { op, operand } => match op {
            UnOp::Not => Ty::Boolean,
            UnOp::Neg => infer_lit_ty_p(file, *operand, class_names, fun_rets, props),
        },
        Expr::Binary { op, lhs, rhs } => {
            let (lt, rt) = (
                infer_lit_ty_p(file, *lhs, class_names, fun_rets, props),
                infer_lit_ty_p(file, *rhs, class_names, fun_rets, props),
            );
            match op {
                BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::Eq
                | BinOp::Ne
                | BinOp::And
                | BinOp::Or
                | BinOp::RefEq
                | BinOp::RefNe => Ty::Boolean,
                BinOp::Add if lt == Ty::String || rt == Ty::String => Ty::String,
                _ => Ty::promote(lt, rt).unwrap_or(Ty::Error),
            }
        }
        // Constructor call `Foo(args)` — infer type from callee name via class_names (seeded from
        // classpath scan + user-defined classes).
        Expr::Call { callee, .. } => {
            match file.expr(*callee) {
                Expr::Name(n) => {
                    // A call to a top-level function with a known return type (`val v = mk()`).
                    if let Some(ret) = fun_rets.get(n.as_str()) {
                        return *ret;
                    }
                    // A JDK/classpath type resolvable by simple name (`val sb = StringBuilder()`).
                    if let Some(internal) = class_names.get(n.as_str()) {
                        return Ty::obj(internal);
                    }
                }
                // A primitive conversion method (`2.toByte()`, `n.toLong()`) returns its named
                // primitive regardless of receiver — Kotlin's `Number`/`Char` conversion family.
                // `toString()` is `String` on any receiver (`Any.toString`).
                Expr::Member { receiver, name } => {
                    if let Some(t) = prim_conversion_ret(name) {
                        return t;
                    }
                    if name == "toString" {
                        return Ty::String;
                    }
                    // `this.method()` — resolve the receiver-`this` method via the rets map (extended
                    // with this class's + superclasses' methods at the method-inference call site).
                    if matches!(file.expr(*receiver), Expr::Name(r) if r == "this") {
                        if let Some(ret) = fun_rets.get(name.as_str()) {
                            return *ret;
                        }
                    }
                }
                _ => {}
            }
            Ty::Error
        }
        _ => Ty::Error,
    }
}

/// The unboxed element type a primitive range / progression iterates as (and the element of its
/// `first`/`last`/`step` members). `None` for any other type. Mirrors kotlinc's specialized
/// `IntIterator`/`LongIterator`/`CharIterator` loops, which yield the primitive without boxing.
fn range_primitive_elem(internal: &str) -> Option<Ty> {
    match internal {
        "kotlin/ranges/IntRange" | "kotlin/ranges/IntProgression" => Some(Ty::Int),
        "kotlin/ranges/LongRange" | "kotlin/ranges/LongProgression" => Some(Ty::Long),
        "kotlin/ranges/CharRange" | "kotlin/ranges/CharProgression" => Some(Ty::Char),
        "kotlin/ranges/UIntRange" | "kotlin/ranges/UIntProgression" => Some(Ty::UInt),
        "kotlin/ranges/ULongRange" | "kotlin/ranges/ULongProgression" => Some(Ty::ULong),
        _ => None,
    }
}

/// Resolve a syntactic type reference to a `Ty`: a primitive/String/Unit, a declared class
/// (→ `Ty::Obj`), or a generic type parameter (erased to `java/lang/Object`).
fn ty_of_ref(
    r: &TypeRef,
    classes: &HashMap<String, String>,
    tparams: &std::collections::HashSet<String>,
    diags: &mut DiagSink,
) -> Ty {
    // Function type: `(A, B) -> R` — parsed with `fun_params` non-empty.
    if !r.fun_params.is_empty() || r.name == "<fun>" {
        let params: Vec<Ty> = r
            .fun_params
            .iter()
            .map(|p| ty_of_ref(p, classes, tparams, diags))
            .collect();
        let ret = r
            .arg
            .as_ref()
            .map(|a| ty_of_ref(a, classes, tparams, diags))
            .unwrap_or(Ty::Unit);
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
                    diags.error(
                        r.span,
                        "krusty: Array of a primitive (use IntArray/…) is not supported"
                            .to_string(),
                    );
                    Ty::Error
                }
            }
            None => {
                diags.error(
                    r.span,
                    "krusty: a raw Array type (no element) is not supported".to_string(),
                );
                Ty::Error
            }
        }
    } else if r.name == "KClass" {
        // `KClass<*>` is modeled as `java.lang.Class` (the JVM annotation representation, and what
        // `X::class` lowers to here). Enough for class-literal storage/identity, not full reflection.
        Ty::obj("java/lang/Class")
    } else if tparams.contains(&r.name) {
        Ty::obj("kotlin/Any") // erased generic type parameter
    } else if let Some(internal) = classes.get(&r.name) {
        // `"__ty/<PrimName>"` encodes a type-alias → primitive/builtin mapping.
        if let Some(prim) = internal.strip_prefix("__ty/") {
            Ty::from_name(prim).unwrap_or(Ty::Error)
        } else if r.targs.is_empty() {
            Ty::obj(internal)
        } else {
            // Generic instantiation `C<A, …>` — carry the resolved arguments (erased in descriptors).
            let args: Vec<Ty> = r
                .targs
                .iter()
                .map(|a| ty_of_ref(a, classes, tparams, diags))
                .collect();
            Ty::obj_args(internal, &args)
        }
    } else {
        diags.error(r.span, format!("unresolved reference '{}'.", r.name));
        Ty::Error
    };
    // A nullable primitive is its boxed wrapper (`Int?` = `java/lang/Integer`); a non-wrappable
    // primitive (unsigned/value) is still rejected (skip, never miscompiled).
    if r.nullable && !base.is_reference() && base != Ty::Error {
        if let Some(w) = nullable_prim_wrapper(base) {
            return Ty::obj(w);
        }
        diags.error(
            r.span,
            format!("nullable primitive type '{}?' is not supported", r.name),
        );
        return Ty::Error;
    }
    base
}

/// The JVM wrapper internal name for a nullable primitive (`Int?` → `java/lang/Integer`). `None` for a
/// non-wrappable primitive (unsigned/value types) — those stay unsupported.
pub(crate) fn nullable_prim_wrapper(t: Ty) -> Option<&'static str> {
    Some(match t {
        Ty::Int => "java/lang/Integer",
        Ty::Long => "java/lang/Long",
        Ty::Double => "java/lang/Double",
        Ty::Float => "java/lang/Float",
        Ty::Boolean => "java/lang/Boolean",
        Ty::Char => "java/lang/Character",
        Ty::Byte => "java/lang/Byte",
        Ty::Short => "java/lang/Short",
        _ => return None,
    })
}

/// The unboxed primitive `Ty` of a JVM wrapper internal name (`java/lang/Integer` → `Int`); inverse of
/// [`nullable_prim_wrapper`]. Used to narrow a nullable primitive (`Int?`) to its primitive after `!!`
/// or a null smart-cast.
pub(crate) fn prim_of_wrapper(internal: &str) -> Option<Ty> {
    Some(match internal {
        "java/lang/Integer" => Ty::Int,
        "java/lang/Long" => Ty::Long,
        "java/lang/Double" => Ty::Double,
        "java/lang/Float" => Ty::Float,
        "java/lang/Boolean" => Ty::Boolean,
        "java/lang/Character" => Ty::Char,
        "java/lang/Byte" => Ty::Byte,
        "java/lang/Short" => Ty::Short,
        _ => return None,
    })
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
    /// A capturing local function's `StmtId` → the enclosing locals it captures, as ordered
    /// `(name, type)`. The lowerer lifts the function with these prepended as extra leading parameters
    /// and passes the captured values at each call site (a written-captured var is also boxed via
    /// `boxed_vars`, so its captured "value" is the shared `Ref` holder).
    pub local_fun_captures: std::collections::HashMap<StmtId, Vec<(String, Ty)>>,
    /// Names of local `var`s that a non-inlined lambda (a closure) writes — they must be boxed into a
    /// `kotlin/jvm/internal/Ref$XxxRef` so the closure and its enclosing scope share the cell. Tracked
    /// by name (a file-wide set); the lowerer boxes any matching `var` it declares (over-boxing an
    /// unrelated same-named `var` is harmless — an extra indirection, never incorrect).
    pub boxed_vars: std::collections::HashSet<String>,
    /// `StmtId`s of compound assignments (`target op= rhs`) the parser desugared to `target = target op
    /// rhs`, but where `target`'s type has a USER-defined `plusAssign`/`minusAssign`/… operator — so the
    /// statement is an in-place operator CALL (`target.plusAssign(rhs)`), legal even on a `val`, NOT a
    /// reassignment. The lowerer resolves the operator and emits the call for these.
    pub plus_assign: std::collections::HashSet<StmtId>,
    /// Calls the checker resolved as a RECEIVER-lambda scope function (`x.run { … }`, `x.apply { … }`,
    /// `with(x) { … }`): the call `ExprId` → how to inline it (the receiver expression, the lambda body,
    /// and whether the call yields the receiver — `apply`/`also` — or the body — `run`/`with`). The
    /// lowerer drives its receiver-lambda inlining off this table, so the decision lives once in the
    /// checker rather than being re-derived (and name-matched) in the backend.
    pub receiver_lambdas: HashMap<ExprId, ReceiverLambda>,
}

/// How to inline a receiver-lambda scope-function call (see [`TypeInfo::receiver_lambdas`]).
#[derive(Clone, Copy, Debug)]
pub struct ReceiverLambda {
    /// The receiver expression — the lambda body's implicit `this`.
    pub receiver: ExprId,
    /// The lambda body expression (lowered with `this` bound to the receiver).
    pub body: ExprId,
    /// `true` for `apply`/`also` (the call yields the receiver), `false` for `run`/`with` (yields body).
    pub returns_receiver: bool,
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
        boxed_vars: std::collections::HashSet::new(),
        plus_assign: std::collections::HashSet::new(),
        receiver_lambdas: HashMap::new(),
        local_fun_captures: std::collections::HashMap::new(),
        fn_reassigned: std::collections::HashSet::new(),
        expr_depth: 0,
        allow_lambda_mutation: false,
        loop_labels: Vec::new(),
    };
    // Top-level functions that erase to the same JVM signature collide in the facade class.
    let top_funs: Vec<&FunDecl> = file
        .decls
        .iter()
        .filter_map(|&d| {
            if let Decl::Fun(f) = file.decl(d) {
                Some(f)
            } else {
                None
            }
        })
        .collect();
    c.check_no_erased_clash(&top_funs, true);

    for &d in &file.decls {
        match file.decl(d) {
            Decl::Fun(f) => {
                c.tparams = f.type_params.iter().cloned().collect();
                c.check_fun(f);
                c.tparams.clear();
            }
            Decl::Class(cl) => {
                // In a class WITH a primary constructor every secondary must delegate to it (`this(…)`);
                // `super(…)`/implicit delegation isn't emitted there, so reject it. A class with NO
                // primary constructor admits `this(…)`/`super(…)`/implicit delegation (each becomes its
                // own `<init>`).
                if cl.has_primary_ctor {
                    for sc in &cl.secondary_ctors {
                        if !matches!(sc.delegation, CtorDelegation::This(_)) {
                            c.diags.error(sc.span, "krusty: a secondary constructor must delegate to the primary (this(…))".to_string());
                        }
                    }
                }
                // `@JvmInline value class` compiles UNBOXED (a value is its underlying type; `X.class`
                // carries the static `-impl` members). The lowering handles construction + sole-property
                // access; uses it can't represent unboxed yet (a value-class-typed local/param/field/
                // return, boxing, equality) make `lower_file` bail so the file skips rather than
                // miscompile. No blanket rejection here.
                // An annotation with an array member needs content-based equals/hashCode
                // (`Arrays.equals`/`Arrays.hashCode`) per the annotation contract — krusty's synthesized
                // members use reference equality, so reject it rather than miscompile equality.
                if cl.is_annotation
                    && cl
                        .props
                        .iter()
                        .any(|p| matches!(c.resolve_ty(&p.ty), Ty::Array(_)))
                {
                    c.diags.error(
                        cl.span,
                        "krusty: an annotation with an array member is not supported".to_string(),
                    );
                }
                // Class type parameters are in scope for all members.
                c.tparams = cl.type_params.iter().cloned().collect();
                // Member functions are checked with the class's properties (resolved in Stage C)
                // visible as an implicit `this` scope.
                let mut props = syms
                    .classes
                    .get(&cl.name)
                    .map(|s| s.props.clone())
                    .unwrap_or_default();
                // An inner class's methods can read the enclosing instance's properties (via `this$0`);
                // make the outer class's backing-field properties resolvable as implicit-`this` members.
                if let Some(outer) = &cl.inner_of {
                    if let Some(os) = syms.classes.get(outer) {
                        props.extend(os.props.clone());
                    }
                }
                c.this_ty = syms.classes.get(&cl.name).map(|s| Ty::obj(&s.internal));
                let methods: Vec<&FunDecl> = cl.methods.iter().collect();
                c.check_no_erased_clash(&methods, false);
                if let Some(internal) = syms.classes.get(&cl.name).map(|s| s.internal.clone()) {
                    c.check_no_bridge_needed(&internal, cl.span);
                    // A `data class` implementing an interface that declares `copy`/`componentN` would
                    // need bridges for its *synthesized* members (which return the class itself, not
                    // the supertype) — krusty doesn't emit those, so reject (cleanly skip).
                    if cl.is_data {
                        let supers = syms.supertype_methods(&internal);
                        if let Some((sn, _)) = supers
                            .iter()
                            .find(|(sn, _)| sn == "copy" || sn.starts_with("component"))
                        {
                            c.diags.error(cl.span, format!("krusty: data class overriding synthesized member '{sn}' needs a bridge method (unsupported)"));
                        }
                    }
                }
                for m in &cl.methods {
                    c.check_method(m, &props);
                }
                // Enum entry bodies (`ENTRY { override fun m() = … }`): each override is checked like
                // a method of the enum — `this` is the enum type, its properties are in scope (so an
                // override can read a constructor `val`), and the return type comes from the abstract
                // member it overrides.
                for body in &cl.enum_entry_bodies {
                    for bm in body {
                        c.check_method(bm, &props);
                    }
                }
                // Secondary constructors: their parameters + the class properties are in scope; the
                // `this(args)` delegation is checked against the primary constructor, then the body.
                let primary_params = c
                    .syms
                    .classes
                    .get(&cl.name)
                    .map(|s| s.ctor_params.clone())
                    .unwrap_or_default();
                // A *deferred* `val` (no initializer/getter/setter) is definitely-assigned once in a
                // constructor body, so it is assignable WITHIN a secondary constructor (kotlinc allows
                // it) — same relaxation the primary-ctor body uses below.
                let sc_deferred_val: std::collections::HashSet<&str> = cl
                    .body_props
                    .iter()
                    .filter(|bp| {
                        !bp.is_var && bp.init.is_none() && bp.getter.is_none() && bp.ty.is_some()
                    })
                    .map(|bp| bp.name.as_str())
                    .collect();
                for sc in &cl.secondary_ctors {
                    c.push_scope();
                    for (n, t, is_var) in &props {
                        c.declare(n, *t, *is_var || sc_deferred_val.contains(n.as_str()));
                    }
                    for p in &sc.params {
                        let ty = c.resolve_ty(&p.ty);
                        c.declare(&p.name, ty, false);
                    }
                    match &sc.delegation {
                        CtorDelegation::This(args) => {
                            let ats: Vec<Ty> = args.iter().map(|a| c.expr(*a)).collect();
                            if cl.has_primary_ctor {
                                if ats.len() != primary_params.len() {
                                    c.diags.error(
                                        sc.span,
                                        format!(
                                            "krusty: this(…) expects {} args, got {}",
                                            primary_params.len(),
                                            ats.len()
                                        ),
                                    );
                                } else {
                                    for (i, (p, a)) in primary_params.iter().zip(&ats).enumerate() {
                                        c.expect_assignable(
                                            *p,
                                            *a,
                                            c.span(args[i]),
                                            "this() argument",
                                        );
                                    }
                                }
                            } else {
                                // No primary ctor: `this(…)` targets a sibling secondary. Best-effort
                                // assignability check against the unique same-arity sibling (lowering bails
                                // if the target is ambiguous), but never reject otherwise-valid code here.
                                let sec_params: Vec<Vec<Ty>> = cl
                                    .secondary_ctors
                                    .iter()
                                    .map(|s| s.params.iter().map(|p| c.resolve_ty(&p.ty)).collect())
                                    .collect();
                                let same: Vec<&Vec<Ty>> =
                                    sec_params.iter().filter(|p| p.len() == ats.len()).collect();
                                if same.len() == 1 {
                                    for (i, (p, a)) in same[0].iter().zip(&ats).enumerate() {
                                        c.expect_assignable(
                                            *p,
                                            *a,
                                            c.span(args[i]),
                                            "this() argument",
                                        );
                                    }
                                }
                            }
                        }
                        // `super(…)`/implicit: evaluate the arguments (records their types for lowering).
                        CtorDelegation::Super(args) => {
                            for a in args {
                                c.expr(*a);
                            }
                        }
                        CtorDelegation::None => {}
                    }
                    if let Some(body) = sc.body {
                        let prev = c.ret_ty;
                        c.ret_ty = Ty::Unit;
                        c.expr(body);
                        c.ret_ty = prev;
                    }
                    c.pop_scope();
                }
                // Body-property initializers and `init` blocks see the properties (implicit `this`)
                // and the primary-constructor parameters (including non-property ones).
                // A *deferred* `val` (declared with no initializer/getter — `val a: Int`) is assigned
                // exactly once in an `init` block, so it is treated as assignable WITHIN the constructor
                // body (kotlinc's definite-assignment allows it; a normal `val` stays immutable).
                let deferred_val: std::collections::HashSet<&str> = cl
                    .body_props
                    .iter()
                    .filter(|bp| {
                        !bp.is_var && bp.init.is_none() && bp.getter.is_none() && bp.ty.is_some()
                    })
                    .map(|bp| bp.name.as_str())
                    .collect();
                c.push_scope();
                for (n, t, is_var) in &props {
                    c.declare(n, *t, *is_var || deferred_val.contains(n.as_str()));
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
                    let prop_ty = bp
                        .ty
                        .as_ref()
                        .map(|r| c.resolve_ty(r))
                        .or_else(|| {
                            c.syms.classes.get(&cl.name).and_then(|cs| {
                                cs.props
                                    .iter()
                                    .find(|(n, _, _)| n == &bp.name)
                                    .map(|(_, t, _)| *t)
                            })
                        })
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
                            FunBody::Block(g) => {
                                let _ = c.expr(*g);
                            }
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
                                FunBody::Expr(g) | FunBody::Block(g) => {
                                    let _ = c.expr(*g);
                                }
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
                            c.expect_assignable(
                                *expected_ty,
                                at,
                                c.span(*a),
                                "enum entry argument",
                            );
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
                                c.expect_assignable(
                                    declared,
                                    it,
                                    c.span(init),
                                    "companion property",
                                );
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
                let prop_ty =
                    p.ty.as_ref()
                        .map(|r| c.resolve_ty(r))
                        .or_else(|| {
                            p.receiver.as_ref().map(|r| c.resolve_ty(r)).and_then(|rt| {
                                c.syms
                                    .ext_props
                                    .get(&(rt.descriptor(), p.name.clone()))
                                    .map(|(t, _)| *t)
                            })
                        })
                        .unwrap_or(Ty::Error);
                // A top-level computed property (`val g: T get() = …`) emits a `getG()` static method
                // (Phase: top-level computed). Type-check the getter body against the declared type.
                if let Some(g) = &p.getter {
                    let prev = c.ret_ty;
                    c.ret_ty = prop_ty;
                    match g {
                        FunBody::Expr(e) => {
                            let gt = c.expr(*e);
                            c.expect_assignable(c.ret_ty, gt, c.span(*e), "getter body");
                        }
                        FunBody::Block(b) => {
                            let _ = c.expr(*b);
                        }
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
                            match body {
                                FunBody::Expr(g) | FunBody::Block(g) => {
                                    let _ = c.expr(*g);
                                }
                                FunBody::None => {}
                            }
                            c.pop_scope();
                            c.ret_ty = prev;
                        }
                    }
                }
                c.this_ty = prev_this;
                if let Some(init) = p.init {
                    let it = c.expr(init);
                    if let Some((declared, _)) = syms
                        .props
                        .get(&p.name)
                        .copied()
                        .filter(|(t, _)| *t != Ty::Error)
                    {
                        if p.ty.is_some() {
                            c.expect_assignable(declared, it, c.span(init), "property initializer");
                        }
                    }
                }
            }
        }
    }
    TypeInfo {
        expr_types: c.expr_types,
        local_fun_sigs: c.local_fun_sigs,
        local_call_map: c.local_call_map,
        fun_ret_overrides: c.fun_ret_overrides,
        ext_calls: c.ext_calls,
        bridges: c.bridges,
        boxed_vars: c.boxed_vars,
        local_fun_captures: c.local_fun_captures,
        plus_assign: c.plus_assign,
        receiver_lambdas: c.receiver_lambdas,
    }
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
    boxed_vars: std::collections::HashSet<String>,
    plus_assign: std::collections::HashSet<StmtId>,
    receiver_lambdas: HashMap<ExprId, ReceiverLambda>,
    local_fun_captures: std::collections::HashMap<StmtId, Vec<(String, Ty)>>,
    /// Names reassigned anywhere in the function body currently being checked (including inside its
    /// closures). A captured `var` is boxed only if it's in here — kotlinc treats a captured-but-never-
    /// reassigned `var` as effectively final (passed by value).
    fn_reassigned: std::collections::HashSet<String>,
    /// Current type-checking recursion depth — guards against a stack overflow on a pathologically
    /// deep expression; past the limit, the expression types as `Error` (the file is skipped).
    expr_depth: u32,
    /// Set while checking the lambda argument of an *inlined* stdlib higher-order function
    /// (`forEach`), where a mutable capture is fine because the lambda body is inlined into the caller
    /// (no closure). Suppresses the mutable-capture rejection for that one lambda.
    allow_lambda_mutation: bool,
    /// In-scope loop labels (`l@ for …`), innermost last. A `break@l`/`continue@l` must name one of
    /// these — an unknown label is rejected (the file skips) rather than silently retargeting a loop.
    loop_labels: Vec<String>,
}

impl<'a> Checker<'a> {
    /// The arg-binding call-resolution layer over this checker's [`LibrarySet`]. Cheap to construct.
    fn resolver(&self) -> crate::call_resolver::CallResolver<'_> {
        crate::call_resolver::CallResolver::new(&*self.syms.libraries)
    }
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
        self.scopes
            .last_mut()
            .unwrap()
            .insert(name.to_string(), Local { ty, is_var });
    }
    fn lookup(&self, name: &str) -> Option<&Local> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }
    /// Whether `name` is already declared in the *innermost* (current) scope — a conflicting
    /// redeclaration (kotlinc rejects it). A declaration in an *outer* scope is legal shadowing.
    fn declared_in_current_scope(&self, name: &str) -> bool {
        self.scopes.last().map_or(false, |s| s.contains_key(name))
    }

    fn push_local_funs(&mut self) {
        self.local_funs.push(HashMap::new());
    }
    fn pop_local_funs(&mut self) {
        self.local_funs.pop();
    }
    fn lookup_local_fun(&self, name: &str) -> Option<(StmtId, Signature)> {
        self.local_funs
            .iter()
            .rev()
            .find_map(|f| f.get(name).cloned())
    }
    fn register_local_fun(&mut self, name: &str, stmt_id: StmtId, sig: Signature) {
        if let Some(frame) = self.local_funs.last_mut() {
            frame.insert(name.to_string(), (stmt_id, sig));
        }
    }

    /// Record the enclosing locals a closure (`body`) captures that must be boxed: a `var` it WRITES,
    /// or a `var` it reads that is REASSIGNED somewhere in the function (`fn_reassigned`) — both need a
    /// shared `Ref$XxxRef` cell. A captured `val`, or a captured `var` never reassigned, stays by value.
    fn record_captured_vars(
        &mut self,
        body: ExprId,
        outer_names: &std::collections::HashSet<String>,
    ) {
        collect_lambda_outer_writes(self.file, body, outer_names, &mut self.boxed_vars);
        for n in outer_names {
            if self.fn_reassigned.contains(n) && self.lookup(n).map_or(false, |l| l.is_var) {
                let single: std::collections::HashSet<String> =
                    std::iter::once(n.clone()).collect();
                if local_fun_body_uses_any(self.file, body, &single) {
                    self.boxed_vars.insert(n.clone());
                }
            }
        }
    }

    /// Build a class reference type, carrying any generic arguments from the syntactic type
    /// (`C<A, …>` → `Ty::obj_args(internal, [A, …])`; raw → `Ty::obj`). Arguments erase in JVM
    /// descriptors but let the front end recover member/element types.
    fn obj_with_targs(&mut self, internal: &str, r: &TypeRef) -> Ty {
        if r.targs.is_empty() {
            Ty::obj(internal)
        } else {
            let args: Vec<Ty> = r.targs.iter().map(|a| self.resolve_ty(a)).collect();
            Ty::obj_args(internal, &args)
        }
    }

    /// Resolve a syntactic type to a `Ty`, including declared class types (→ `Ty::Obj`).
    /// Nullability doesn't change the `Ty` for reference types (same JVM descriptor), but a nullable
    /// *primitive* (`Char?`, `Int?`, …) would need boxing — rejected (the file is skipped).
    fn resolve_ty(&mut self, r: &TypeRef) -> Ty {
        // Function type: `(A, B) -> R` — parsed with `fun_params` non-empty.
        if !r.fun_params.is_empty() || r.name == "<fun>" {
            let params: Vec<Ty> = r.fun_params.iter().map(|p| self.resolve_ty(p)).collect();
            let ret = r
                .arg
                .as_ref()
                .map(|a| self.resolve_ty(a))
                .unwrap_or(Ty::Unit);
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
            Ty::obj("kotlin/Any") // erased generic type parameter
        } else if let Some(cs) = self.syms.classes.get(&r.name) {
            let internal = cs.internal.clone();
            self.obj_with_targs(&internal, r)
        } else if let Some(internal) = self.syms.class_names.get(&r.name).cloned() {
            // Built-in mapped types (`Number`, `Comparable`, `List`, …), classpath classes, and
            // type aliases — the *same* map emit resolves against, so the checker and codegen agree
            // (otherwise a leniently-`Error` type here becomes a real `Obj` in emit → VerifyError).
            // `"__ty/<Prim>"` encodes an alias to a primitive/builtin.
            match internal.strip_prefix("__ty/") {
                Some(prim) => Ty::from_name(prim).unwrap_or(Ty::Error),
                None => self.obj_with_targs(&internal, r),
            }
        } else {
            Ty::Error
        };
        if r.nullable && !base.is_reference() && base != Ty::Error {
            if let Some(w) = nullable_prim_wrapper(base) {
                return Ty::obj(w);
            }
            self.diags.error(
                r.span,
                format!("nullable primitive type '{}?' is not supported", r.name),
            );
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
        // An extension function's receiver is its first JVM parameter — part of the signature, so
        // `Int.foo()` and `String.foo()` don't collide.
        let recv = f
            .receiver
            .as_ref()
            .map(|r| {
                if let Some(t) = Ty::from_name(&r.name) {
                    t.descriptor()
                } else if let Some(cs) = self.syms.classes.get(&r.name) {
                    Ty::obj(&cs.internal).descriptor()
                } else {
                    // Unresolved (type parameter / unknown) — keep the name so distinct receivers get
                    // distinct keys (don't collapse to a single `Object` and hide a real JVM collision).
                    format!("L{};", r.name)
                }
            })
            .unwrap_or_default();
        format!("{}({}{})", f.name, recv, params)
    }

    /// The erased JVM parameter descriptors (concatenated) and return descriptor of a function,
    /// using the type parameters in scope plus the function's own (each → `Object`).
    fn erased_param_ret(&self, f: &FunDecl) -> (String, String) {
        let extra: std::collections::HashSet<&str> =
            f.type_params.iter().map(|s| s.as_str()).collect();
        let descr = |name: &str| -> String {
            if let Some(t) = Ty::from_name(name) {
                t.descriptor()
            } else if self.tparams.contains(name) || extra.contains(name) {
                Ty::obj("kotlin/Any").descriptor()
            } else if let Some(cs) = self.syms.classes.get(name) {
                Ty::obj(&cs.internal).descriptor()
            } else {
                Ty::obj("kotlin/Any").descriptor()
            }
        };
        let params: String = f.params.iter().map(|p| descr(&p.ty.name)).collect();
        let ret = f
            .ret
            .as_ref()
            .map(|r| descr(&r.name))
            .unwrap_or_else(|| "V".to_string());
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
        let obj = Ty::obj("kotlin/Any");
        for (name, ssig) in &supers {
            let Some(impl_sig) = self.syms.method_of(internal, name) else {
                continue;
            };
            let sp: String = ssig.params.iter().map(|t| t.descriptor()).collect();
            let ip: String = impl_sig.params.iter().map(|t| t.descriptor()).collect();
            let params_differ = sp != ip;
            let ret_differs = ssig.ret.descriptor() != impl_sig.ret.descriptor();
            // Each erased param must either equal the concrete one (passes through) or be `Object`
            // (the generic-erasure case — the bridge checkcasts a reference or unboxes a primitive).
            // A non-`Object` erased param that differs means `method_of` resolved the wrong overload.
            let params_bridgeable = ssig.params.len() == impl_sig.params.len()
                && ssig
                    .params
                    .iter()
                    .zip(&impl_sig.params)
                    .all(|(e, c)| e == c || *e == obj);
            if (params_differ || ret_differs) && params_bridgeable {
                // Record a synthetic bridge `name(erased)` that downcasts its args and delegates to
                // the concrete `name(impl)`. (Primitive params would need (un)boxing in the bridge —
                // left out of this pass.)
                self.bridges
                    .entry(internal.to_string())
                    .or_default()
                    .push(BridgeSpec {
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
        // A property overriding a supertype property with a different erased type needs a bridge `getX()`
        // returning the supertype's (erased) type — the lowering synthesizes it (an `ACC_BRIDGE` getter
        // delegating to the concrete one). A primitive own-type would need (un)boxing in that bridge,
        // which the property-bridge path doesn't emit yet — reject only that case.
        // A property overriding a supertype property with a different erased type needs a getter bridge,
        // which the lowering synthesizes (boxing a primitive own type in the bridge as needed).
    }

    /// Report (and thereby skip the file for) functions whose signatures collide. An EXACT erased-signature
    /// duplicate is always a JVM `ClassFormatError` and is rejected. When `allow_overload` is true
    /// (top-level functions), same-name functions with DIFFERENT erased signatures are legal overloads,
    /// dispatched at the call site by argument types ([`pick_overload`]). When false (class members), they
    /// are rejected — member overloading needs erasure/bridge handling krusty doesn't model, so the file
    /// is skipped rather than miscompiled.
    fn check_no_erased_clash(&mut self, funs: &[&FunDecl], allow_overload: bool) {
        let mut by_name: HashMap<String, String> = HashMap::new(); // name → first erased key
        let mut seen: HashMap<String, Span> = HashMap::new();
        for f in funs {
            // `erased_sig_key` includes the name and (for extensions) the receiver, so distinct names and
            // same-named extensions on different receivers don't collide.
            let key = self.erased_sig_key(f);
            if seen.contains_key(&key) {
                self.diags.error(
                    f.span,
                    format!("conflicting overloads: function '{}' has the same JVM signature as another after type erasure", f.name),
                );
            } else {
                if !allow_overload && f.receiver.is_none() {
                    if let std::collections::hash_map::Entry::Occupied(e) =
                        by_name.entry(f.name.clone())
                    {
                        if e.get() != &key {
                            self.diags.error(
                                f.span,
                                format!("krusty: function '{}' has multiple overloads with different erased signatures (overload dispatch not supported)", f.name),
                            );
                        }
                    } else {
                        by_name.insert(f.name.clone(), key.clone());
                    }
                }
                seen.insert(key, f.span);
            }
        }
    }

    /// True if a subject `when` is exhaustive because its subject is a `sealed` class and every
    /// declared subclass is matched by a positive `is` arm. Conservative: anything it can't prove
    /// (non-sealed subject, an uncovered subclass, a nested sealed subclass) returns false.
    fn when_sealed_exhaustive(&self, subj_ty: Option<Ty>, arms: &[WhenArm]) -> bool {
        let Some(Ty::Obj(internal, _)) = subj_ty else {
            return false;
        };
        let Some(cs) = self.syms.class_by_internal(internal) else {
            return false;
        };
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
                if let Expr::Is {
                    ty, negated: false, ..
                } = self.file.expr(c)
                {
                    if let Ty::Obj(n, _) = self.resolve_ty_no_diag(ty) {
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
        let Some(Ty::Obj(internal, _)) = subj_ty else {
            return false;
        };
        // Find the enum's simple name (key in self.syms.enums) matching this internal name.
        let Some((_, entries)) = self.syms.enums.iter().find(|(name, _)| {
            self.syms
                .classes
                .get(*name)
                .map_or(false, |c| c.internal == internal)
        }) else {
            return false;
        };
        if entries.is_empty() {
            return false;
        }
        let mut covered: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for arm in arms {
            for &cnd in &arm.conditions {
                // Arm condition must be `EnumClass.ENTRY` — a member access on the enum class.
                if let Expr::Member {
                    receiver,
                    name: entry,
                } = self.file.expr(cnd)
                {
                    if let Expr::Name(en) = self.file.expr(*receiver) {
                        if self
                            .syms
                            .classes
                            .get(en)
                            .map_or(false, |c| c.internal == internal)
                        {
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
                    matches!(
                        self.file.stmt(last),
                        Stmt::Return(..) | Stmt::Break(_) | Stmt::Continue(_)
                    )
                } else {
                    false
                }
            }
            Expr::If {
                then_branch,
                else_branch: Some(eb),
                ..
            } => self.expr_diverges(*then_branch) && self.expr_diverges(*eb),
            _ => false,
        }
    }

    /// The JVM internal name of a `catch` clause's exception type: a common JDK exception, an
    /// imported class, or a declared class. `None` if krusty can't resolve it to a concrete class.
    fn catch_internal(&self, name: &str) -> Option<String> {
        self.imports
            .get(name)
            .cloned()
            .or_else(|| self.syms.classes.get(name).map(|c| c.internal.clone()))
            // Exception types resolve from the classpath: stdlib `TypeAliasesKt` aliases
            // (`Exception`, `RuntimeException`, …) and the ported `JavaToKotlinClassMap`
            // built-ins (`Throwable`) are both folded into `class_names`.
            .or_else(|| self.syms.class_names.get(name).cloned())
    }

    /// Resolve a type without emitting diagnostics (used for speculative smart-cast narrowing).
    fn resolve_ty_no_diag(&self, r: &TypeRef) -> Ty {
        if !r.fun_params.is_empty() || r.name == "<fun>" {
            let params: Vec<Ty> = r
                .fun_params
                .iter()
                .map(|p| self.resolve_ty_no_diag(p))
                .collect();
            let ret = r
                .arg
                .as_ref()
                .map(|a| self.resolve_ty_no_diag(a))
                .unwrap_or(Ty::Unit);
            return Ty::fun(params, ret);
        }
        if let Some(t) = Ty::from_name(&r.name) {
            t
        } else if self.tparams.contains(&r.name) {
            Ty::obj("kotlin/Any")
        } else if let Some(cs) = self.syms.classes.get(&r.name) {
            Ty::obj(&cs.internal)
        } else {
            Ty::Error
        }
    }

    /// If `cond` is `x is T` (or `x !is T` when `for_else`) and `x` is a stable local/parameter and
    /// `T` a non-nullable known reference type, return the smart-cast binding `(x, T)`.
    fn smartcast_binding(&self, cond: ExprId, for_else: bool) -> Option<(String, Ty)> {
        // `x != null` (then-branch) / `x == null` (else-branch) narrows a nullable-primitive wrapper to
        // its unboxed primitive — the only null-narrowing krusty needs (a nullable reference is already
        // its non-null type here). Only a stable `val`/parameter narrows soundly.
        if let Expr::Binary { op, lhs, rhs } = self.file.expr(cond).clone() {
            if matches!(op, BinOp::Ne | BinOp::Eq) {
                let narrows_then = matches!(op, BinOp::Ne); // `!= null` narrows in the then-branch
                if narrows_then == !for_else {
                    let name = match (self.file.expr(lhs).clone(), self.file.expr(rhs).clone()) {
                        (Expr::Name(n), Expr::NullLit) | (Expr::NullLit, Expr::Name(n)) => Some(n),
                        _ => None,
                    };
                    if let Some(n) = name {
                        if let Some(l) = self.lookup(&n) {
                            if !l.is_var {
                                if let Some(p) = l.ty.obj_internal().and_then(prim_of_wrapper) {
                                    return Some((n, p));
                                }
                            }
                        }
                    }
                }
            }
        }
        let Expr::Is {
            operand,
            ty,
            negated,
        } = self.file.expr(cond).clone()
        else {
            return None;
        };
        // The then-branch narrows on a positive `is`; the else-branch on a negative `!is`.
        if negated != for_else {
            return None;
        }
        if ty.nullable {
            return None;
        }
        let Expr::Name(n) = self.file.expr(operand).clone() else {
            return None;
        };
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
        // The set of locals reassigned anywhere in this function (for captured-`var` boxing).
        self.fn_reassigned.clear();
        if let FunBody::Expr(b) | FunBody::Block(b) = &f.body {
            collect_all_reassigned(self.file, *b, &mut self.fn_reassigned);
        }
        // Inline functions are expanded at each call site by the lowerer (like kotlinc's inliner),
        // so the body is checked here but never emitted standalone. A lambda *parameter* of an
        // inline function may be invoked on a mutable capture (it ends up inlined into the caller),
        // so permit mutation while checking the body.
        let prev_allow = self.allow_lambda_mutation;
        if f.is_inline {
            self.allow_lambda_mutation = true;
        }
        // Extension function: look up in ext_funs table; set this_ty to the receiver type.
        let prev_this = self.this_ty;
        if let Some(recv_ref) = &f.receiver {
            let recv_ty = self.resolve_ty(recv_ref);
            self.this_ty = Some(recv_ty);
            let recv_desc = recv_ty.descriptor();
            self.ret_ty = self
                .syms
                .ext_funs
                .get(&(recv_desc, f.name.clone()))
                .map(|s| s.ret)
                .or_else(|| f.ret.as_ref().map(|r| self.resolve_ty(r)))
                .unwrap_or(Ty::Unit);
        } else {
            // Use this declaration's own collected overload's return type (matched by its erased
            // parameter descriptors when the name is overloaded); for a companion method (not in `funs`)
            // fall back to the declared return type.
            let want: String = f
                .params
                .iter()
                .map(|p| self.resolve_ty(&p.ty).descriptor())
                .collect();
            let own_ret = self.syms.funs.get(&f.name).and_then(|v| {
                if v.len() == 1 {
                    Some(v[0].ret)
                } else {
                    v.iter()
                        .find(|s| erased_params_key(s) == want)
                        .map(|s| s.ret)
                }
            });
            self.ret_ty = own_ret
                .or_else(|| f.ret.as_ref().map(|r| self.resolve_ty(r)))
                .unwrap_or(Ty::Unit);
        }
        // For expression-body functions with no explicit return type, infer the return type from the
        // body expression and record it as an override (so codegen uses the right JVM descriptor).
        let infer_ret =
            f.ret.is_none() && self.ret_ty == Ty::Unit && matches!(&f.body, FunBody::Expr(_));
        // Default arguments are evaluated in the caller's context (they may not read other params —
        // enforced in collect_signatures), so check each in a fresh scope and populate its types.
        self.push_scope();
        for p in &f.params {
            if let Some(dx) = p.default {
                let pty = self.resolve_ty(&p.ty);
                // A default that is a LAMBDA for a function-typed parameter (`g: (Int) -> Int = { it + 1 }`)
                // takes its parameter types from the declared function type, so `it`/named params type
                // concretely (not the erased `Object`) — as for a typed local / HOF argument lambda.
                let dty = if matches!(self.file.expr(dx), Expr::Lambda { .. })
                    && (!p.ty.fun_params.is_empty() || p.ty.name == "<fun>")
                {
                    let lam_pts: Vec<Ty> =
                        p.ty.fun_params.iter().map(|r| self.resolve_ty(r)).collect();
                    self.check_lambda_with_types(dx, &lam_pts)
                } else {
                    self.expr(dx)
                };
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
        self.allow_lambda_mutation = prev_allow;
    }

    /// Check an instance method: the class properties are visible (implicit `this`), then the
    /// method's own parameters shadow them.
    fn check_method(&mut self, f: &FunDecl, props: &[(String, Ty, bool)]) {
        if f.is_inline {
            self.diags
                .error(f.span, "krusty: inline functions are not supported");
            return;
        }
        self.fn_reassigned.clear();
        if let FunBody::Expr(b) | FunBody::Block(b) = &f.body {
            collect_all_reassigned(self.file, *b, &mut self.fn_reassigned);
        }
        let added: Vec<String> = f
            .type_params
            .iter()
            .filter(|t| self.tparams.insert((*t).clone()))
            .cloned()
            .collect();
        self.ret_ty = f
            .ret
            .as_ref()
            .map(|r| self.resolve_ty(r))
            .unwrap_or_else(|| {
                // For a method without an explicit return type (e.g. `override fun foo() = "Z"`),
                // use the return type that collect_signatures already inferred from the method body.
                if let Some(Ty::Obj(internal, _)) = self.this_ty {
                    if let Some(sig) = self
                        .syms
                        .class_by_internal(internal)
                        .and_then(|c| c.methods.get(&f.name))
                    {
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
        if let FunBody::Expr(e) | FunBody::Block(e) = &f.body {
            if bc_complex_e(self.file, *e, false) {
                self.diags.error(f.span, "krusty: 'break'/'continue' in value position, inside 'try', or inside a lambda is not supported".to_string());
                return;
            }
        }
        match &f.body {
            FunBody::Expr(e) => {
                // An expression-body lambda whose declared return type is a function type takes its
                // parameter types from that return type — `fun mk(): (Int) -> Int = { it + 1 }` types `it`
                // as `Int`, not the erased `Object` (the same as a typed local/HOF-argument lambda).
                let t = match (
                    self.ret_ty,
                    matches!(self.file.expr(*e), Expr::Lambda { .. }),
                ) {
                    (Ty::Fun(s), true) => {
                        let params = s.params.clone();
                        self.check_lambda_with_types(*e, &params)
                    }
                    _ => self.expr(*e),
                };
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
        // The Kotlin collection interfaces all erase to one JVM interface; compare on the erased names so
        // `MutableList`/`List` (and a `kotlin/collections/List` vs a platform `java/util/List`) are
        // mutually assignable — the read-only/mutable distinction is enforced only at the `+=` operator,
        // not in general assignability. Also lets a `kotlin/collections/MutableList` reach
        // `java/util/Collection` via the JVM hierarchy walk below.
        let sub = crate::jvm::jvm_class_map::to_jvm_internal(sub);
        let sup = crate::jvm::jvm_class_map::to_jvm_internal(sup);
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
            return false;
        }
        // A classpath type (`java/util/ArrayList`): walk its supertype chain (superclass + interfaces)
        // through the library to see if `sup` (e.g. `java/util/List`) is reachable.
        let mut seen = std::collections::HashSet::new();
        let mut q = std::collections::VecDeque::new();
        q.push_back(sub.to_string());
        while let Some(cur) = q.pop_front() {
            if cur == sup {
                return true;
            }
            if !seen.insert(cur.clone()) {
                continue;
            }
            if let Some(t) = self.syms.libraries.resolve_type(&cur) {
                q.extend(t.supertypes);
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
        // A class provides its implemented interfaces' methods — directly overridden, inherited, or (for
        // `: I by d`) delegated. Resolving them here lets a delegating class's calls type-check.
        for i in c.interfaces.clone() {
            if let Some(sig) = self.lookup_method(&i, name) {
                return Some(sig);
            }
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

    /// Silent (non-erroring) assignability of each argument to a constructor's parameters — used to pick
    /// between a same-arity primary and a secondary constructor (`Sc(Int)` vs `Sc(String)`).
    fn ctor_args_match(&self, params: &[Ty], args: &[Ty]) -> bool {
        params.len() == args.len()
            && params.iter().zip(args).all(|(&p, &a)| {
                p == a
                    || p == Ty::Error
                    || a == Ty::Error
                    || a == Ty::Nothing
                    || (a == Ty::Null && p.is_reference())
                    || p == Ty::obj("kotlin/Any")
                    || a == Ty::obj("kotlin/Any")
                    || matches!((p, a), (Ty::Obj(e, _), Ty::Obj(x, _)) if self.obj_is_subtype(x, e))
                    || (p == Ty::Long && matches!(a, Ty::Int | Ty::Byte | Ty::Short | Ty::Char))
                    || (matches!(p, Ty::Byte | Ty::Short)
                        && matches!(a, Ty::Int | Ty::Byte | Ty::Short))
                    || (matches!(p, Ty::Float | Ty::Double)
                        && matches!(
                            a,
                            Ty::Int | Ty::Long | Ty::Byte | Ty::Short | Ty::Char | Ty::Float
                        ))
            })
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
        // An erased generic reference array (`Array<Any>`, e.g. `emptyArray<T>()` → `Object[]`) is
        // assignable to any specific reference array — `Array` is invariant, but the erased value
        // really is the target type at runtime, so kotlinc inserts a `checkcast` at the use site.
        if let (Ty::Array(ae), Ty::Array(ee)) = (actual, expected) {
            if *ae == Ty::obj("kotlin/Any") && ee.is_reference() {
                return;
            }
        }
        // An `Int` (typically a constant) is assignable to `Byte`/`Short` (Kotlin narrows integer
        // literals); codegen emits `i2b`/`i2s`. `Byte`/`Short` are interchangeable with `Int` here.
        if matches!(expected, Ty::Byte | Ty::Short)
            && matches!(actual, Ty::Int | Ty::Byte | Ty::Short)
        {
            return;
        }
        // Int/Byte/Short/Char are assignable to Long (integer widening); codegen emits i2l.
        if expected == Ty::Long && matches!(actual, Ty::Int | Ty::Byte | Ty::Short | Ty::Char) {
            return;
        }
        // Int/Byte/Short/Char/Long are assignable to Float/Double (widening); codegen emits i2f etc.
        if matches!(expected, Ty::Float | Ty::Double)
            && matches!(
                actual,
                Ty::Int | Ty::Long | Ty::Byte | Ty::Short | Ty::Char | Ty::Float
            )
        {
            return;
        }
        // A primitive is assignable to its boxed wrapper — i.e. to the matching nullable primitive
        // (`Int` → `Int?` = `java/lang/Integer`). The box (`Integer.valueOf`) is the emit site's job.
        if let Ty::Obj(ew, _) = expected {
            if prim_of_wrapper(ew) == Some(actual) {
                return;
            }
        }
        // In Kotlin every type is a subtype of `Any`/`Object`, and the top type narrows back to a
        // specific type by an unchecked cast. Both directions are assignable; the primitive-vs-boxed
        // *representation* (and any box/unbox or checkcast) is the backend's concern, decided at the
        // emit coercion site — not the type checker's. (`Unit` is excluded: it has no JVM value here.)
        if expected == Ty::obj("kotlin/Any") && actual != Ty::Unit {
            return;
        }
        if actual == Ty::obj("kotlin/Any") && expected != Ty::Unit {
            return;
        }
        // A class value is assignable to an interface (supertype) it implements.
        if let (Ty::Obj(e, _), Ty::Obj(a, _)) = (expected, actual) {
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
        // A property reference is a function: `KProperty1<T,R>` IS a `(T)->R` and `KProperty0<R>` a
        // `()->R` (kotlinc's `PropertyReference{1,0}` implements the matching `FunctionN` via
        // `invoke = get`), so it's assignable to a function type of the matching arity.
        if let Ty::Fun(e) = &expected {
            let prop_arity = actual.obj_internal().and_then(|ai| match ai {
                "kotlin/reflect/KProperty1" | "kotlin/reflect/KMutableProperty1" => Some(1),
                "kotlin/reflect/KProperty0" | "kotlin/reflect/KMutableProperty0" => Some(0),
                _ => None,
            });
            if prop_arity == Some(e.params.len()) {
                return;
            }
        }
        // Known classpath supertypes of `String` (`String : CharSequence, Comparable, Serializable`).
        if actual == Ty::String {
            if let Some(ei) = expected.obj_internal() {
                if matches!(
                    ei,
                    "java/lang/CharSequence" | "java/lang/Comparable" | "java/io/Serializable"
                ) {
                    return;
                }
            }
        }
        if expected != actual {
            // Match kotlinc 2.4.0's phrasing. A return position (an expression body or a getter body)
            // reads as "return type mismatch: expected 'T', actual 'U'."; every other context keeps the
            // general inferred-vs-expected wording.
            let msg = if matches!(ctx, "function body" | "getter body" | "local function body") {
                format!(
                    "return type mismatch: expected '{}', actual '{}'.",
                    expected.name(),
                    actual.name()
                )
            } else {
                format!(
                    "type mismatch: inferred type is {} but {} was expected",
                    actual.name(),
                    expected.name()
                )
            };
            self.diags.error(span, msg);
        }
    }

    fn expr(&mut self, e: ExprId) -> Ty {
        // Guard against a stack overflow on a pathologically deep expression: past the limit the
        // expression types as `Error` (the file is skipped, never crashed).
        self.expr_depth += 1;
        if self.expr_depth > 500 {
            self.expr_depth -= 1;
            return self.set(e, Ty::Error);
        }
        let t = self.expr_inner(e);
        self.expr_depth -= 1;
        t
    }

    fn expr_inner(&mut self, e: ExprId) -> Ty {
        let t = match self.file.expr(e).clone() {
            Expr::IntLit(_) => Ty::Int,
            Expr::LongLit(_) => Ty::Long,
            Expr::UIntLit(_) => Ty::UInt,
            Expr::ULongLit(_) => Ty::ULong,
            Expr::DoubleLit(_) => Ty::Double,
            Expr::FloatLit(_) => Ty::Float,
            Expr::BoolLit(_) => Ty::Boolean,
            Expr::StringLit(_) => Ty::String,
            Expr::CharLit(_) => Ty::Char,
            Expr::NullLit => Ty::Null,
            Expr::NotNull { operand } => {
                // The value with its non-null type; `Int?!!` narrows to the unboxed primitive `Int`.
                let t = self.expr(operand);
                t.obj_internal().and_then(prim_of_wrapper).unwrap_or(t)
            }
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
                // A lambda that writes an enclosing local: record those vars so the lowerer boxes them
                // into a `kotlin/jvm/internal/Ref$XxxRef` (a closure can't write a captured-by-value
                // local otherwise). Recorded unconditionally — over-boxing a var that turns out inlined
                // is harmless, and NOT recording one that becomes a closure would miscompile. The body
                // still checks normally below.
                let outer_names: std::collections::HashSet<String> =
                    self.scopes.iter().flat_map(|s| s.keys().cloned()).collect();
                if !outer_names.is_empty() {
                    self.record_captured_vars(body, &outer_names);
                }
                // Type each parameter from its explicit annotation (`{ x: Int -> … }`) if present, so a
                // bare-value lambda checks its body correctly; otherwise the erased `Any` (an expected
                // function type, when there is one, is applied via `check_lambda_with_types` instead).
                let decl_types: Vec<Option<TypeRef>> = self
                    .file
                    .lambda_param_types
                    .get(&e.0)
                    .cloned()
                    .unwrap_or_default();
                self.push_scope();
                for (i, name) in bind_names.iter().enumerate() {
                    let pty = decl_types
                        .get(i)
                        .and_then(|t| t.as_ref())
                        .map(|r| self.resolve_ty(r))
                        .unwrap_or_else(|| Ty::obj("kotlin/Any"));
                    self.declare(name, pty, false);
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
                    self.diags.error(
                        self.file.expr_spans[e.0 as usize],
                        "krusty: a lambda that calls a local function is not supported".to_string(),
                    );
                }
                self.field_ty = saved_field;
                self.pop_scope();
                // Parameter types: an explicit annotation (`{ x: Int -> … }`) drives the function type so a
                // direct call (`f(3)`) type-checks; an unannotated parameter erases to `Object`. The return
                // type comes from the body.
                let fun_params: Vec<Ty> = (0..arity as usize)
                    .map(|i| {
                        decl_types
                            .get(i)
                            .and_then(|t| t.as_ref())
                            .map(|r| self.resolve_ty(r))
                            .unwrap_or_else(|| Ty::obj("kotlin/Any"))
                    })
                    .collect();
                Ty::fun(fun_params, bret)
            }
            Expr::Index { array, index } => {
                let at = self.expr(array);
                let it = self.expr(index);
                if let Some(elem) = at.array_elem() {
                    self.expect_assignable(Ty::Int, it, self.span(index), "array index");
                    return self.set(e, elem);
                }
                // `str[i]` is the `String.get(Int): Char` operator — resolved from the builtins String
                // declarations (then the curated table for anything builtins doesn't declare).
                if at == Ty::String {
                    if let Some(ret) = self
                        .syms
                        .libraries
                        .builtin_member_ret("kotlin/String", "get", &[it])
                        .or_else(|| resolve_string_instance("get", &[it]))
                    {
                        return self.set(e, ret);
                    }
                }
                // `coll[i]` on a library type → the `get(index)` operator member (`List.get(Int)`,
                // `Map.get(K)`); the index type is checked against the member's parameter.
                if let Ty::Obj(internal, _) = at {
                    if let Some(m) = crate::call_resolver::resolve_instance(
                        &*self.syms.libraries,
                        internal,
                        "get",
                        &[it],
                    ) {
                        let ret = self
                            .syms
                            .libraries
                            .member_return(at, "get", &[it])
                            .unwrap_or(m.ret);
                        return self.set(e, ret);
                    }
                }
                if at != Ty::Error {
                    self.diags.error(
                        self.span(e),
                        format!("'{}' is not an array (cannot index)", at.name()),
                    );
                }
                Ty::Error
            }
            Expr::Try {
                body,
                catches,
                finally,
            } => {
                // Nested `try`s are emitted fine on their own, and a flat `try … finally` is fine — but the
                // COMBINATION is not: a `finally` is inlined at each exit of its protected region, so when it
                // sits inside (or wraps) another `try`, the duplicated code lands in overlapping exception
                // ranges and trips a verify error (Bad local variable type). Reject only that combination —
                // a nesting that involves any `finally` — keeping plain nested try/catch and plain finally.
                let nested = expr_has_try(self.file, body)
                    || catches.iter().any(|c| expr_has_try(self.file, c.body))
                    || finally.map_or(false, |f| expr_has_try(self.file, f));
                let any_finally = finally.is_some()
                    || expr_has_finally(self.file, body)
                    || catches.iter().any(|c| expr_has_finally(self.file, c.body))
                    || finally.map_or(false, |f| expr_has_finally(self.file, f));
                if nested && any_finally {
                    self.diags.error(
                        self.span(e),
                        "krusty: a nested try combined with a finally is not supported".to_string(),
                    );
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
                            self.diags.error(
                                c.ty.span,
                                "krusty: catch type is not a known exception class".to_string(),
                            );
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
            Expr::Is {
                operand,
                ty,
                negated: _,
            } => {
                let ot = self.expr(operand);
                let tt = self.resolve_ty(&ty);
                // `instanceof` needs a reference operand and a *known* target. An unresolved target
                // (`Number`, a value class, `Nothing`, …) must not silently become `Object` (which
                // would make the test always true) — reject so the file is cleanly skipped. A primitive
                // target (`x is Int`) is allowed: it tests against the boxed wrapper (`Integer`). A
                // *nullable* target (`x is T?`) is rejected: `null is T?` is true, but plain
                // `instanceof` yields false, so it would miscompile.
                // A floating-point `is` target (`is Double`/`is Float`) would let the file reach
                // boxed `==` whose IEEE-754 semantics (`-0.0`/`NaN`) krusty doesn't model — restrict to
                // integral/boolean/char primitives. `is UInt`/`is ULong` is rejected too: the value is a
                // `kotlin.UInt`/`ULong` value-type box, but krusty erases unsigned to `int`/`long` and a
                // smart-cast *use* of it would unbox as `Integer`/`Long` (ClassCastException), so skip.
                let tt_known = tt.is_reference()
                    || (tt.is_primitive()
                        && !matches!(tt, Ty::Double | Ty::Float | Ty::UInt | Ty::ULong));
                // A nullable target is allowed only for a REFERENCE type (`x is A?` lowers to
                // `x == null || x is A`); a nullable primitive (`x is Int?`) would mix box/unbox
                // semantics krusty doesn't model here, so it stays rejected.
                let bad_nullable = ty.nullable && !tt.is_reference();
                if !tt_known || bad_nullable || (!ot.is_reference() && ot != Ty::Error) {
                    self.diags.error(
                        self.span(e),
                        "krusty: 'is' on this type is not supported".to_string(),
                    );
                    return Ty::Error;
                }
                Ty::Boolean
            }
            Expr::As {
                operand,
                ty,
                nullable: _,
            } => {
                let ot = self.expr(operand);
                let tt = self.resolve_ty(&ty);
                // `checkcast` needs a reference operand. The target is either a *known* reference type (an
                // unresolved one erases to a no-op `Object` cast — rejected), or a non-unsigned primitive:
                // `x as Int` on a reference operand is an unbox (`checkcast Integer; intValue()`).
                let prim_unbox = ot.is_reference() && tt.is_primitive() && !tt.is_unsigned();
                if !(tt.is_reference() || prim_unbox) || (!ot.is_reference() && ot != Ty::Error) {
                    self.diags.error(
                        self.span(e),
                        "krusty: 'as' with this type is not supported".to_string(),
                    );
                    return Ty::Error;
                }
                tt
            }
            Expr::InRange {
                value, start, end, ..
            } => {
                let vt = self.expr(value);
                let st = self.expr(start);
                let et = self.expr(end);
                // Only primitive numeric/char ranges are lowered (to a comparison chain). Any other
                // operand type (a range over user/reference types) is rejected so the file is skipped.
                let prim = |t: &Ty| {
                    matches!(
                        t,
                        Ty::Int
                            | Ty::Long
                            | Ty::Char
                            | Ty::Short
                            | Ty::Byte
                            | Ty::Double
                            | Ty::Float
                            | Ty::UInt
                            | Ty::ULong
                    )
                };
                // Require uniform operand types — the lowering emits direct same-type comparisons, so a
                // mixed range (Int value, Long bounds) would need promotion that isn't modeled yet.
                if prim(&vt) && vt == st && st == et {
                    Ty::Boolean
                } else {
                    self.diags.error(
                        self.span(e),
                        "krusty: 'in' is only supported for primitive numeric ranges".to_string(),
                    );
                    Ty::Error
                }
            }
            Expr::RangeTo { lo, hi, .. } => {
                let lt = self.expr(lo);
                let rt = self.expr(hi);
                // `a..b` / `a..<b` constructs the matching stdlib range object. `Char..Char` is a
                // `CharRange`; the integer family widens like kotlinc's `rangeTo` overloads — any of
                // `Byte`/`Short`/`Int` yields an `IntRange`, and if either operand is `Long` a
                // `LongRange`. Unsigned and floating ranges are not modeled here (the file is skipped).
                let small_int = |t: &Ty| matches!(t, Ty::Byte | Ty::Short | Ty::Int);
                match (lt, rt) {
                    (Ty::Char, Ty::Char) => Ty::obj("kotlin/ranges/CharRange"),
                    // Unsigned ranges are their own stdlib classes (`UIntRange`/`ULongRange`), iterated
                    // with unsigned comparison and mangled inline-class getters.
                    (Ty::UInt, Ty::UInt) => Ty::obj("kotlin/ranges/UIntRange"),
                    (Ty::ULong, Ty::ULong) => Ty::obj("kotlin/ranges/ULongRange"),
                    _ if small_int(&lt) && small_int(&rt) => Ty::obj("kotlin/ranges/IntRange"),
                    _ if (small_int(&lt) || lt == Ty::Long)
                        && (small_int(&rt) || rt == Ty::Long) =>
                    {
                        Ty::obj("kotlin/ranges/LongRange")
                    }
                    _ => {
                        self.diags.error(
                            self.span(e),
                            "krusty: range expression is only supported for Int/Long/Char operands"
                                .to_string(),
                        );
                        Ty::Error
                    }
                }
            }
            Expr::IncDec { target, .. } => {
                // `target++`/`++target` as a value: only a simple mutable numeric/Char variable (the
                // built-in `inc`/`dec`); the result type is the variable's type.
                let tt = self.expr(target);
                if let Expr::Name(name) = self.file.expr(target).clone() {
                    match self
                        .lookup(&name)
                        .map(|l| (l.ty, l.is_var))
                        .or_else(|| self.syms.props.get(&name).copied())
                    {
                        Some((_, is_var)) => {
                            if !is_var {
                                self.diags
                                    .error(self.span(e), "'val' cannot be reassigned.".to_string());
                            }
                            if !tt.is_numeric() && tt != Ty::Char {
                                self.diags.error(
                                    self.span(e),
                                    "krusty: '++'/'--' is only supported on a numeric variable"
                                        .to_string(),
                                );
                            }
                        }
                        None => self
                            .diags
                            .error(self.span(e), format!("unresolved reference '{name}'.")),
                    }
                } else {
                    self.diags.error(
                        self.span(e),
                        "krusty: '++'/'--' as a value is only supported on a simple variable"
                            .to_string(),
                    );
                }
                tt
            }
            Expr::Elvis { lhs, rhs } => {
                let lt0 = self.expr(lhs);
                let rt = self.expr(rhs);
                // The elvis value when lhs is non-null: a nullable-primitive lhs (`Int?`) unwraps to its
                // unboxed primitive, so `intNullable ?: 0` is `Int`.
                let lt = lt0.obj_internal().and_then(prim_of_wrapper).unwrap_or(lt0);
                // A `Unit`-coerced elvis (`x ?: someUnitExpr`) trips a StackMapTable mismatch in
                // codegen (the branches push incompatible stack shapes) — skip rather than VerifyError.
                if rt == Ty::Unit {
                    self.diags.error(
                        self.span(e),
                        "krusty: elvis with a Unit right-hand side is not supported".to_string(),
                    );
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
            Expr::SafeCall {
                receiver,
                name,
                args,
            } => {
                let rt = self.expr(receiver);
                if rt == Ty::Error {
                    return Ty::Error;
                }
                // User-defined extension on a non-nullable primitive receiver: safe call is a no-op
                // (primitives can never be null), so emit as a direct static call.
                if !rt.is_reference() {
                    let recv_desc = rt.descriptor();
                    if let Some(sig) = self
                        .syms
                        .ext_funs
                        .get(&(recv_desc.clone(), name.clone()))
                        .cloned()
                    {
                        let arg_tys: Vec<Ty> = match &args {
                            Some(a) => a.iter().map(|x| self.expr(*x)).collect(),
                            None => vec![],
                        };
                        if sig.params.len() != arg_tys.len() {
                            self.diags.error(
                                self.span(e),
                                format!(
                                    "extension '{name}' expects {} args, got {}",
                                    sig.params.len(),
                                    arg_tys.len()
                                ),
                            );
                        }
                        let pdesc: String = sig.params.iter().map(|t| t.descriptor()).collect();
                        let desc = format!("({recv_desc}{pdesc}){}", sig.ret.descriptor());
                        self.ext_calls
                            .insert(e, ("$local".to_string(), name.clone(), desc));
                        return self.set(e, sig.ret);
                    }
                }
                // A safe-call scope function (`s?.let { it… }`, `s?.run { … }`): the receiver is non-null
                // inside; type it like the non-safe form, then wrap the result nullable below.
                let result = if let Some(t) = self.safe_scope_call_result(rt, &name, &args) {
                    t
                } else {
                    match &args {
                        None => self.check_member(rt, &name, self.span(e)),
                        Some(a) => {
                            let arg_tys: Vec<Ty> = a.iter().map(|x| self.expr(*x)).collect();
                            if let ("toString", []) = (name.as_str(), arg_tys.as_slice()) {
                                Ty::String
                            } else if let ("hashCode", []) = (name.as_str(), arg_tys.as_slice()) {
                                Ty::Int // Int (not a reference), so safe-call rejection fires below
                            } else if rt == Ty::String {
                                self.syms
                                    .libraries
                                    .builtin_member_ret("kotlin/String", &name, &arg_tys)
                                    .or_else(|| resolve_string_instance(&name, &arg_tys))
                                    .unwrap_or(Ty::Error)
                            } else if let Ty::Obj(internal, _) = rt {
                                self.lookup_method(internal, &name)
                                    .map(|s| s.ret)
                                    .or_else(|| {
                                        crate::call_resolver::resolve_instance(
                                            &*self.syms.libraries,
                                            internal,
                                            &name,
                                            &arg_tys,
                                        )
                                        .map(|m| m.ret)
                                    })
                                    .unwrap_or(Ty::Error)
                            } else {
                                Ty::Error
                            }
                        }
                    }
                };
                // The safe-call result is nullable: a primitive member result becomes its boxed wrapper
                // (`s?.length` → `Int?` = `java/lang/Integer`); the member value is boxed (or `null`) in
                // lowering. A non-wrappable primitive (unsigned/value) stays unsupported.
                if !result.is_reference() && result != Ty::Error {
                    if let Some(w) = nullable_prim_wrapper(result) {
                        return self.set(e, Ty::obj(w));
                    }
                    self.diags.error(
                        self.span(e),
                        "krusty: safe call (?.) with a non-reference result is not supported"
                            .to_string(),
                    );
                    return Ty::Error;
                }
                result
            }
            Expr::Name(n) if n == "this" => match self.this_ty {
                Some(t) => t,
                None => {
                    self.diags.error(
                        self.span(e),
                        "'this' is not available outside a class member".to_string(),
                    );
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
                        if let Some(&ty) = self
                            .syms
                            .classes
                            .get(cls)
                            .and_then(|c| c.static_props.get(&n))
                        {
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
                    if let Some(Ty::Obj(internal, _)) = self.this_ty {
                        if let Some((ty, _)) = self.lookup_prop(internal, &n) {
                            return self.set(e, ty);
                        }
                    }
                    // A bare name resolved against the implicit receiver (`this`) of arbitrary type —
                    // e.g. `length` inside `"ab".run { length }` (`this` is `String`). Goes through the
                    // general member read so builtin/library members (`String.length`) resolve too.
                    if let Some(rt) = self.this_ty {
                        if let Some(ty) = self.try_member_read(rt, &n, self.span(e)) {
                            return self.set(e, ty);
                        }
                    }
                    if let Some(&(ty, _)) = self.syms.props.get(&n) {
                        ty // top-level property
                    } else if n == "Unit" {
                        // The `Unit` singleton used as a value (`foo(Unit)`, `val x = Unit`, `return
                        // Unit`) — the `kotlin/Unit` object, read as its `INSTANCE` in lowering. Only a
                        // fallback: any local/property/object named `Unit` was resolved above.
                        Ty::obj("kotlin/Unit")
                    } else {
                        self.diags
                            .error(self.span(e), format!("unresolved reference '{n}'."));
                        Ty::Error
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
                        if let Some(sig) = self
                            .syms
                            .ext_funs
                            .get(&(recv_desc.clone(), fname.to_string()))
                            .cloned()
                        {
                            if sig.params.len() == 1 {
                                let pdesc: String =
                                    sig.params.iter().map(|t| t.descriptor()).collect();
                                let desc = format!("({recv_desc}{pdesc}){}", sig.ret.descriptor());
                                self.ext_calls
                                    .insert(e, ("$local".to_string(), fname.to_string(), desc));
                                return self.set(e, sig.ret);
                            }
                        }
                    }
                }
                // A class MEMBER operator (`operator fun plus(o: V): V` on the receiver class): `a + b` →
                // `a.plus(b)`. The body's own arithmetic is on the field types (no self-recursion). The
                // lowering re-resolves the member, so only the result type is recorded here.
                if let Ty::Obj(internal, _) = &lt {
                    let op_name = match op {
                        BinOp::Add => Some("plus"),
                        BinOp::Sub => Some("minus"),
                        BinOp::Mul => Some("times"),
                        BinOp::Div => Some("div"),
                        BinOp::Rem => Some("rem"),
                        _ => None,
                    };
                    if let Some(fname) = op_name {
                        if let Some(sig) = self.syms.method_of(internal, fname) {
                            if sig.params.len() == 1 && rt != Ty::Error {
                                self.expect_assignable(
                                    sig.params[0],
                                    rt,
                                    self.span(rhs),
                                    "operator argument",
                                );
                                return self.set(e, sig.ret);
                            }
                        }
                    }
                    // A class `operator fun compareTo(o): Int` drives `<`/`<=`/`>`/`>=` (`a < b` →
                    // `a.compareTo(b) < 0`), yielding `Boolean`.
                    if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
                        && rt != Ty::Error
                    {
                        if let Some(sig) = self.syms.method_of(internal, "compareTo") {
                            if sig.params.len() == 1 && sig.ret == Ty::Int {
                                self.expect_assignable(
                                    sig.params[0],
                                    rt,
                                    self.span(rhs),
                                    "operator argument",
                                );
                                return self.set(e, Ty::Boolean);
                            }
                        }
                    }
                    // A library operator function on a reference receiver: `a + b` desugars to `a.plus(b)`,
                    // resolved as a stdlib member/extension (`List + element` → `CollectionsKt.plus`). Use
                    // its (parameterized) return type. The lowering re-resolves to emit the call.
                    let op_name = match op {
                        BinOp::Add => Some("plus"),
                        BinOp::Sub => Some("minus"),
                        BinOp::Mul => Some("times"),
                        BinOp::Div => Some("div"),
                        BinOp::Rem => Some("rem"),
                        _ => None,
                    };
                    // Resolve `a + b` (etc.) as `a.plus(b)` through the library set. Overload selection
                    // picks the most specific candidate (`list + list` → the `Iterable` concat overload,
                    // `list + element` → the element overload), so a reference right operand is fine.
                    if let Some(fname) = op_name {
                        if rt != Ty::Error {
                            if let Some(c) =
                                self.syms
                                    .libraries
                                    .resolve_callable(fname, Some(lt), &[rt], &[])
                            {
                                self.ext_calls.insert(e, (c.owner, c.name, c.descriptor));
                                return self.set(e, c.ret);
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
                                let internal = self
                                    .syms
                                    .classes
                                    .get(&en)
                                    .map(|c| c.internal.clone())
                                    .unwrap_or(en.clone());
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
                            if let Some((ty, _)) =
                                self.syms.classes.get(&en).and_then(|c| c.prop(&name))
                            {
                                return self.set(e, ty);
                            }
                        }
                    }
                }
                let rt = self.expr(receiver);
                self.check_member(rt, &name, self.span(e))
            }
            Expr::Call { callee, args } => self.check_call(e, callee, &args, self.span(e)),
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
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
                        if let Expr::If {
                            cond,
                            then_branch,
                            else_branch: None,
                        } = self.file.expr(ie).clone()
                        {
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
                            if matches!(
                                self.file.stmt(last),
                                Stmt::Return(..) | Stmt::Break(_) | Stmt::Continue(_)
                            ) {
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
                        // An `is T` / `in range` (or `!`-forms) condition is a *boolean test* on the
                        // subject — built by the parser as a structural `Is`/`InRange` node — not a value
                        // to compare with `==`, so it carries no comparability constraint with the subject.
                        let is_type_test =
                            matches!(self.file.expr(cnd), Expr::Is { .. } | Expr::InRange { .. });
                        let ct = self.expr(cnd);
                        match subj_ty {
                            // A type-test arm (`is T`) compares by `instanceof`, not `==` — no
                            // comparability constraint (it already validated its own operand/target).
                            _ if is_type_test => {}
                            // subject form: condition must be comparable to the subject.
                            // `null` is always a valid condition (the branch simply never matches
                            // for non-nullable subjects; it may match for nullable ones).
                            Some(st)
                                if ct != Ty::Null
                                    && st != Ty::Error
                                    && ct != Ty::Error
                                    && st != ct
                                    && Ty::promote(st, ct).is_none() =>
                            {
                                self.diags.error(self.span(cnd), format!("when condition type '{}' is not comparable to subject '{}'", ct.name(), st.name()));
                            }
                            // subjectless form: condition must be Boolean
                            None => self.expect_assignable(
                                Ty::Boolean,
                                ct,
                                self.span(cnd),
                                "when condition",
                            ),
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
                    self.diags.error(
                        self.span(e),
                        "krusty: this class-literal form is not supported".to_string(),
                    );
                    return Ty::Error;
                }
                // Object-method callable references (`Any::equals`, `obj::toString`). A receiver that
                // names a value is *bound* (captures it, arity = method args); one that names a type
                // is *unbound* (the receiver becomes the first parameter).
                let obj = Ty::obj("kotlin/Any");
                if matches!(name.as_str(), "equals" | "hashCode" | "toString") {
                    let bound = match receiver {
                        Some(r) => {
                            matches!(self.file.expr(r), Expr::Name(n) if self.lookup(n).is_some())
                        }
                        None => false,
                    };
                    if let Some(r) = receiver {
                        if bound {
                            self.expr(r);
                        } // type-check the captured receiver
                    }
                    let (margs, ret): (u8, Ty) = match name.as_str() {
                        "equals" => (1, Ty::Boolean),
                        "hashCode" => (0, Ty::Int),
                        _ => (0, Ty::String),
                    };
                    let arity = if bound { margs } else { margs + 1 };
                    return self.set(e, Ty::fun(vec![obj; arity as usize], ret));
                }
                // Top-level function reference `::foo` → `Fun(params, ret)` of that function. Only an
                // UNAMBIGUOUS (single-overload) name resolves here; an overloaded `::foo` needs an
                // expected function type to disambiguate, which krusty doesn't model.
                if receiver.is_none() {
                    if let Some(sig) = self
                        .syms
                        .funs
                        .get(&name)
                        .and_then(|v| (v.len() == 1).then(|| v[0].clone()))
                    {
                        if !sig.vararg && sig.params.len() == sig.required {
                            return self.set(e, Ty::fun(sig.params.clone(), sig.ret));
                        }
                    }
                    // Constructor reference `::ClassName` → `Fun(ctor_params, ClassName)`.
                    if !self.syms.objects.contains(&name) {
                        if let Some(cls) = self.syms.classes.get(&name).cloned() {
                            if !cls.is_annotation {
                                return self.set(
                                    e,
                                    Ty::fun(cls.ctor_params.clone(), Ty::obj(&cls.internal)),
                                );
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
                                // bound property reference `obj::prop` → `KProperty0`/`KMutableProperty0`.
                                let internal = internal.to_string();
                                if let Some(is_var) =
                                    self.syms.class_by_internal(&internal).and_then(|c| {
                                        c.props
                                            .iter()
                                            .find(|(n, _, _)| *n == name)
                                            .map(|(_, _, v)| *v)
                                    })
                                {
                                    self.expr(r); // capture the receiver
                                    let iface = if is_var {
                                        "kotlin/reflect/KMutableProperty0"
                                    } else {
                                        "kotlin/reflect/KProperty0"
                                    };
                                    return self.set(e, Ty::obj(iface));
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
                                // unbound property reference `Type::prop` → `KProperty1<Type, V>`
                                // (`KMutableProperty1` for a `var`), erased to the reflection interface.
                                if let Some(is_var) = cls
                                    .props
                                    .iter()
                                    .find(|(n, _, _)| *n == name)
                                    .map(|(_, _, v)| *v)
                                {
                                    let iface = if is_var {
                                        "kotlin/reflect/KMutableProperty1"
                                    } else {
                                        "kotlin/reflect/KProperty1"
                                    };
                                    return self.set(e, Ty::obj(iface));
                                }
                            }
                        }
                    }
                }
                if let Some(recv) = receiver {
                    self.expr(recv);
                }
                self.diags.error(
                    self.span(e),
                    "krusty: callable references are not supported",
                );
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
                self.diags.error(
                    span,
                    format!("operator cannot be applied to '{}'", ot.name()),
                );
                Ty::Error
            }
        }
    }

    fn check_binary(&mut self, op: BinOp, lt: Ty, rt: Ty, span: Span) -> Ty {
        if lt == Ty::Error || rt == Ty::Error {
            return Ty::Error;
        }
        // Unsigned arithmetic: both operands the same unsigned type (`UInt`/`ULong`). `+`/`-`/`*`/`/`/`%`
        // keep the type; comparisons/equality yield `Boolean`. Mixed signed/unsigned is a type error in
        // Kotlin (explicit conversion required), so it falls through to `bin_err`.
        if lt.is_unsigned() && lt == rt {
            return match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => lt,
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne => {
                    Ty::Boolean
                }
                BinOp::And | BinOp::Or | BinOp::RefEq | BinOp::RefNe => {
                    self.bin_err(op, lt, rt, span)
                }
            };
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
                } else if lt == Ty::Char && rt == Ty::Int {
                    Ty::Char // `Char.plus(Int)` → Char (wraps mod 2^16)
                } else if let Some(t) = Ty::promote(lt, rt) {
                    t
                } else {
                    self.bin_err(op, lt, rt, span)
                }
            }
            BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                // `Char` arithmetic: `Char - Int` → Char, `Char - Char` → Int (Kotlin's only
                // `Char.minus` overloads; there is no `Char + Char`, `Char * …`, etc.).
                if op == BinOp::Sub && lt == Ty::Char {
                    if rt == Ty::Int {
                        return Ty::Char;
                    }
                    if rt == Ty::Char {
                        return Ty::Int;
                    }
                }
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
                // `Any` compares with anything (Kotlin structural equality boxes a primitive operand);
                // otherwise require equal/promotable primitives or two references.
                let any = Ty::obj("kotlin/Any");
                // A nullable-primitive wrapper (`Int?` = `Integer`) compares with its primitive (`a == 5`):
                // the primitive operand is boxed at the emit site for structural equality. Excludes
                // Float/Double — their `0.0 == -0.0` IEEE-754 semantics differ between primitive `==` and
                // boxed `equals`, which needs a dedicated comparison krusty doesn't emit yet.
                let wrapper_vs_prim = |w: Ty, p: Ty| {
                    w.obj_internal()
                        .and_then(prim_of_wrapper)
                        .map_or(false, |pw| pw == p && !matches!(pw, Ty::Float | Ty::Double))
                };
                if lt == rt
                    || Ty::promote(lt, rt).is_some()
                    || (lt.is_reference() && rt.is_reference())
                    || lt == any
                    || rt == any
                    || wrapper_vs_prim(lt, rt)
                    || wrapper_vs_prim(rt, lt)
                {
                    Ty::Boolean
                } else {
                    self.bin_err(op, lt, rt, span)
                }
            }
            BinOp::RefEq | BinOp::RefNe => {
                // Referential identity (`===`/`!==`) compiles to a JVM `if_acmp*` on the two object
                // refs. `String` identity, though, hinges on kotlinc's compile-time folding/interning of
                // `const val`s (a computed const string like `"1234$a"` is folded to one interned
                // literal, so `A.b === B.b`); krusty emits such a const as a runtime concatenation (a
                // fresh object), so it can't reproduce String identity yet — skip rather than miscompile.
                // Object and boxed-primitive identity is unaffected.
                let is_prim_wrapper = |t: Ty| t.obj_internal().and_then(prim_of_wrapper).is_some();
                if lt == Ty::String || rt == Ty::String {
                    self.diags.error(span, "krusty: referential equality (=== / !==) on String operands is not supported".to_string());
                    Ty::Error
                } else if is_prim_wrapper(lt) || is_prim_wrapper(rt) {
                    // A nullable-primitive wrapper (`Int?`/`Double?`) compared with `===`/`!==`: boxed
                    // identity vs the unboxed primitive (and `Double`/`Float`'s `-0.0`/`NaN`) has subtle
                    // semantics krusty doesn't model — skip rather than miscompile (`if_icmp*` on a boxed
                    // operand would be a VerifyError).
                    self.diags.error(span, "krusty: referential equality (=== / !==) on a nullable-primitive operand is not supported".to_string());
                    Ty::Error
                } else {
                    Ty::Boolean
                }
            }
        }
    }

    fn bin_err(&mut self, _op: BinOp, lt: Ty, rt: Ty, span: Span) -> Ty {
        self.diags.error(
            span,
            format!(
                "operator cannot be applied to '{}' and '{}'",
                lt.name(),
                rt.name()
            ),
        );
        Ty::Error
    }

    /// Recognize array-creation builtins: `intArrayOf(…)`/`charArrayOf(…)`/… and `arrayOf(…)`
    /// (element = the common reference type of the arguments), and the size constructors
    /// `IntArray(n)`/`CharArray(n)`/… Returns the array `Ty`, or `None` if `fname` isn't one of these.
    fn check_array_builtin(
        &mut self,
        fname: &str,
        args: &[ExprId],
        arg_tys: &[Ty],
        span: Span,
    ) -> Option<Ty> {
        let primitive_of = |f: &str| match f {
            "intArrayOf" => Some(Ty::Int),
            "longArrayOf" => Some(Ty::Long),
            "doubleArrayOf" => Some(Ty::Double),
            "floatArrayOf" => Some(Ty::Float),
            "booleanArrayOf" => Some(Ty::Boolean),
            "charArrayOf" => Some(Ty::Char),
            "byteArrayOf" => Some(Ty::Byte),
            "shortArrayOf" => Some(Ty::Short),
            _ => None,
        };
        if let Some(elem) = primitive_of(fname) {
            for (i, t) in arg_tys.iter().enumerate() {
                self.expect_assignable(elem, *t, self.span(args[i]), "array element");
            }
            return Some(Ty::array(elem));
        }
        if fname == "emptyArray" && args.is_empty() {
            // `emptyArray<T>()` is a reified intrinsic — an erased reference array (`Array<Any>`,
            // i.e. `Object[]`), assignable to any reference array; codegen specializes the empty array
            // to the *target* element type (the reified `T`) at the use site.
            return Some(Ty::array(Ty::obj("kotlin/Any")));
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
                    self.diags.error(
                        span,
                        "krusty: arrayOf of a primitive (use intArrayOf/…) is not supported"
                            .to_string(),
                    );
                    return Some(Ty::Error);
                }
                None => {
                    self.diags.error(
                        span,
                        "krusty: empty arrayOf() needs an explicit type (unsupported)".to_string(),
                    );
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
        if fname == "Array"
            && arg_tys.len() == 2
            && matches!(self.file.expr(args[1]), Expr::Lambda { .. })
        {
            self.expect_assignable(Ty::Int, arg_tys[0], self.span(args[0]), "array size");
            let lam = self.check_lambda_with_types(args[1], &[Ty::Int]);
            let elem = lam.fun_ret().unwrap_or_else(|| Ty::obj("kotlin/Any"));
            // A nested-array element (`Array(n) { DoubleArray(m) }`) trips the loop-fill's
            // StackMapTable interaction with surrounding loops — skip rather than VerifyError.
            if matches!(elem, Ty::Array(_)) {
                self.diags.error(
                    span,
                    "krusty: Array(n) {…} with an array element is not supported".to_string(),
                );
                return Some(Ty::Error);
            }
            // `Array(n) { … }` is the reference `Array<T>`, distinct from a primitive array. A
            // reference element keeps the existing `Ty::Array` reference representation; a primitive
            // element is the logical `Array<Int>` (`Obj("kotlin/Array", [Int])`) — NOT boxed here, so
            // element reads type as `Int`. The backend boxes to `Integer[]` when it lays out the array.
            return Some(if elem.is_primitive() {
                Ty::obj_args("kotlin/Array", &[elem])
            } else {
                Ty::array(elem)
            });
        }
        None
    }

    /// Recognize stdlib precondition intrinsics: `require`/`check`/`assert(cond)` (→ `Unit`),
    /// `error(msg)` (→ `Nothing`), and `TODO()`/`TODO(msg)` (→ `Nothing`). Returns the result type,
    /// or `None` if `fname` isn't one of these.
    /// Type-check a `run`/`with`/`apply` lambda body with `recv` as its implicit receiver: `this` is
    /// `recv`, and the receiver's properties resolve unqualified. Returns the body's type.
    fn check_with_receiver(&mut self, recv: Ty, body: ExprId, _span: Span) -> Ty {
        if recv == Ty::Error {
            return Ty::Error;
        }
        let prev_this = self.this_ty;
        self.this_ty = Some(recv);
        self.push_scope();
        // A user class receiver's own properties are visible unqualified inside the body; for builtin
        // and library receivers (`String`, `StringBuilder`, …) a bare member resolves through the
        // implicit-`this` member probe in the `Expr::Name`/call arms instead.
        if let Ty::Obj(internal, _) = recv {
            if let Some(cs) = self.syms.class_by_internal(internal) {
                for (n, t, is_var) in cs.props.clone() {
                    self.declare(&n, t, is_var);
                }
            }
        }
        let bt = self.expr(body);
        self.pop_scope();
        self.this_ty = prev_this;
        bt
    }

    /// Type a SAFE-CALL scope function `recv?.name { … }` (`let`/`run`/`also`/`apply`): inside, the
    /// receiver `rt` is non-null; the lambda binds `it`=rt (`let`/`also`) or `this`=rt (`run`/`apply`).
    /// Returns the NON-nullable result (`let`/`run` → the lambda body; `also`/`apply` → the receiver);
    /// the caller wraps it nullable. `None` when it isn't a recognized lambda-bearing scope call.
    fn safe_scope_call_result(
        &mut self,
        rt: Ty,
        name: &str,
        args: &Option<Vec<ExprId>>,
    ) -> Option<Ty> {
        let a = args.as_ref()?;
        if a.len() != 1 {
            return None;
        }
        let Expr::Lambda { params, body } = self.file.expr(a[0]).clone() else {
            return None;
        };
        // Inside the lambda the receiver is NON-null: a nullable-primitive receiver (`Int?` =
        // `java/lang/Integer`, e.g. from a chained `s?.let { … }?.let { it + 1 }`) binds `it`/`this` as
        // the UNBOXED primitive (`Int`), so `it + 1` is primitive arithmetic, not `Integer + Int`.
        let rt = rt.obj_internal().and_then(prim_of_wrapper).unwrap_or(rt);
        match name {
            "run" | "apply" if params.is_empty() => {
                let bt = self.check_with_receiver(rt, body, self.span(a[0]));
                Some(if name == "apply" { rt } else { bt })
            }
            "let" | "also" => {
                let lt = self.check_lambda_with_types(a[0], &[rt]);
                Some(if name == "also" {
                    rt
                } else if let Ty::Fun(s) = lt {
                    s.ret
                } else {
                    Ty::Error
                })
            }
            _ => None,
        }
    }

    /// Resolve an UNQUALIFIED call `name(args)` as a member of the implicit receiver `rt` (`this`) —
    /// the body of a receiver lambda (`StringBuilder().apply { append("x") }` → `this.append("x")`).
    /// Mirrors the qualified `recv.name(args)` member-call typing for builtin/library/user receivers.
    /// Returns `Some(ret)` when it resolves, `None` to let the caller keep searching. Checks arguments.
    fn this_member_call_ret(
        &mut self,
        rt: Ty,
        name: &str,
        arg_tys: &[Ty],
        args: &[ExprId],
    ) -> Option<Ty> {
        if let ("toString", []) = (name, arg_tys) {
            return Some(Ty::String);
        }
        if rt == Ty::String {
            if let Some(ret) = self
                .syms
                .libraries
                .builtin_member_ret("kotlin/String", name, arg_tys)
                .or_else(|| resolve_string_instance(name, arg_tys))
            {
                return Some(ret);
            }
        }
        if rt == Ty::obj("java/lang/StringBuilder") {
            if let Some(ret) = resolve_stringbuilder_instance(name, arg_tys) {
                return Some(ret);
            }
        }
        if let Ty::Obj(internal, _) = rt {
            if let Some(sig) = self.lookup_method(internal, name) {
                if sig.params.len() == arg_tys.len() {
                    for (i, (p, a)) in sig.params.iter().zip(arg_tys).enumerate() {
                        self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                    }
                    return Some(sig.ret);
                }
            }
            if let Some(m) = crate::call_resolver::resolve_instance(
                &*self.syms.libraries,
                internal,
                name,
                arg_tys,
            ) {
                return Some(
                    self.syms
                        .libraries
                        .member_return(rt, name, arg_tys)
                        .unwrap_or(m.ret),
                );
            }
        }
        // A stdlib/library EXTENSION on the receiver (`String.reversed()`, `String.uppercase()`):
        // resolved receiver-aware so the right overload is selected (`CharSequence.reversed`, not the
        // `Iterable.reversed` that a receiver-blind fallthrough would pick). Mirrors the qualified
        // `recv.name(args)` extension typing.
        if let Some(c) = self
            .syms
            .libraries
            .resolve_callable(name, Some(rt), arg_tys, &[])
        {
            return Some(c.ret);
        }
        None
    }

    /// Check a lambda expression with explicit parameter types (for type-directed inference).
    /// For a call to a USER generic function (`inline fun <T> twice(x: T, f: (T)->T): T`): bind its type
    /// parameters from the already-typed non-lambda arguments, then return each argument's lambda parameter
    /// types AND the call's specialized return type — both type-param-substituted. So `twice(1) { it+10 }`
    /// types `it` as `Int` and the call as `Int`, not the erased `Any`. `None` when no matching user
    /// function or it isn't generic. `partial[i]` is `Some(ty)` for a non-lambda arg, `None` for a lambda.
    fn user_generic_call(&mut self, fname: &str, partial: &[Option<Ty>]) -> Option<Vec<Vec<Ty>>> {
        let f = self
            .file
            .decls
            .iter()
            .find_map(|&d| match self.file.decl(d) {
                Decl::Fun(f)
                    if f.name == fname
                        && f.receiver.is_none()
                        && f.params.len() == partial.len() =>
                {
                    Some(f.clone())
                }
                _ => None,
            })?;
        // Only an INLINE function specializes its type params at the call site (the body is spliced with
        // concrete types). A NON-inline generic function runs through the erased `Function1` — its lambda
        // `it` is `Object` at runtime, so typing it concretely would mismatch (and break value-class args).
        if !f.is_inline || f.type_params.is_empty() {
            return None;
        }
        let tparams: std::collections::HashSet<&str> =
            f.type_params.iter().map(String::as_str).collect();
        let mut binds: std::collections::HashMap<String, Ty> = std::collections::HashMap::new();
        for (i, p) in f.params.iter().enumerate() {
            if tparams.contains(p.ty.name.as_str()) {
                if let Some(Some(at)) = partial.get(i) {
                    binds.entry(p.ty.name.clone()).or_insert(*at);
                }
            }
        }
        if binds.is_empty() {
            return None;
        }
        let lam_pts: Vec<Vec<Ty>> = f
            .params
            .iter()
            .map(|p| {
                if p.ty.fun_params.is_empty() {
                    Vec::new()
                } else {
                    p.ty.fun_params
                        .iter()
                        .map(|fp| {
                            binds
                                .get(fp.name.as_str())
                                .copied()
                                .unwrap_or_else(|| self.resolve_ty(fp))
                        })
                        .collect()
                }
            })
            .collect();
        Some(lam_pts)
    }

    /// The specialized return type of a user generic inline HOF, inferred from the FULL argument
    /// types — value args AND lambda args (their parameter and **return** types). For
    /// `applyFn<T, R>(x: T, f: (T) -> R): R`, `applyFn("ab") { it.length }` binds `T=String` from the
    /// value arg and `R=Int` from the lambda's return type, so the call types as `Int` (not erased
    /// `Any`). Must run AFTER the lambda args are typed (unlike [`user_generic_call`], which produces
    /// the lambda parameter types and therefore runs before). `None` when no matching user inline
    /// generic function, or its return type isn't a (now-bound) type parameter.
    fn user_generic_return(&self, fname: &str, arg_tys: &[Ty]) -> Option<Ty> {
        let f = self
            .file
            .decls
            .iter()
            .find_map(|&d| match self.file.decl(d) {
                Decl::Fun(f)
                    if f.name == fname
                        && f.receiver.is_none()
                        && f.is_inline
                        && !f.type_params.is_empty()
                        && f.params.len() == arg_tys.len() =>
                {
                    Some(f)
                }
                _ => None,
            })?;
        let tparams: std::collections::HashSet<&str> =
            f.type_params.iter().map(String::as_str).collect();
        let mut binds: std::collections::HashMap<&str, Ty> = std::collections::HashMap::new();
        for (i, p) in f.params.iter().enumerate() {
            let at = &arg_tys[i];
            if p.ty.fun_params.is_empty() {
                // A plain value parameter typed as a bare type parameter (`x: T`).
                if tparams.contains(p.ty.name.as_str()) {
                    binds.entry(p.ty.name.as_str()).or_insert(*at);
                }
            } else if let Ty::Fun(fsig) = at {
                // A function-typed parameter `(A) -> R`: bind `A` from the lambda's parameter types
                // and `R` from its return type.
                for (decl, actual) in p.ty.fun_params.iter().zip(&fsig.params) {
                    if tparams.contains(decl.name.as_str()) {
                        binds.entry(decl.name.as_str()).or_insert(*actual);
                    }
                }
                if let Some(rret) = &p.ty.arg {
                    if tparams.contains(rret.name.as_str()) {
                        binds.entry(rret.name.as_str()).or_insert(fsig.ret);
                    }
                }
            }
        }
        f.ret
            .as_ref()
            .and_then(|r| binds.get(r.name.as_str()).copied())
    }

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
            // A non-inlined lambda that writes an enclosing local captures it through a `Ref$XxxRef`
            // box (handled in lowering); record those vars. The body still checks normally here.
            if !self.allow_lambda_mutation {
                let outer_names: std::collections::HashSet<String> =
                    self.scopes.iter().flat_map(|s| s.keys().cloned()).collect();
                if !outer_names.is_empty() {
                    self.record_captured_vars(body, &outer_names);
                }
            }
            self.push_scope();
            for (i, name) in bind_names.iter().enumerate() {
                let pty = param_types.get(i).copied().unwrap_or(Ty::obj("kotlin/Any"));
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

    /// The result type of a constructor call `Name<A,…>(…)`: the class instantiated with the call's
    /// explicit type arguments (`ArrayList<Int>()` → `ArrayList<Int>`), so member/element types
    /// resolve. Falls back to the raw class type when there are no explicit type arguments.
    fn ctor_result(&mut self, call: ExprId, internal: &str) -> Ty {
        if let Some(targs) = self.file.call_type_args.get(&call.0).cloned() {
            let args: Vec<Ty> = targs.iter().map(|r| self.resolve_ty(r)).collect();
            if !args.is_empty() {
                return Ty::obj_args(internal, &args);
            }
        }
        Ty::obj(internal)
    }

    fn check_member(&mut self, rt: Ty, name: &str, span: Span) -> Ty {
        if rt == Ty::Error {
            return Ty::Error;
        }
        if let (Ty::String, "length") = (rt, name) {
            return Ty::Int;
        }
        if let (Ty::Char, "code") = (rt, name) {
            return Ty::Int; // `c.code` — the Char's UTF-16 code unit as an `Int`.
        }
        if let (Ty::Array(_), "size") = (rt, name) {
            return Ty::Int;
        }
        if rt == Ty::obj("java/lang/StringBuilder") && name == "length" {
            return Ty::Int; // `sb.length` property → length()
        }
        // Property read on a class value: `p.prop` (own or inherited).
        if let Ty::Obj(internal, args) = rt {
            if let Some((ty, _)) = self.lookup_prop(internal, name) {
                // Generic substitution: if `name` is declared as one of the receiver class's type
                // parameters and the receiver carries that argument (`Box<Int>().x`), report the
                // argument type instead of the erased `Object`. The member-read lowering inserts the
                // checkcast/unbox kotlinc emits on such a read.
                if let Some(cs) = self.syms.class_by_internal(internal) {
                    if let Some(&i) = cs.generic_props.get(name) {
                        if let Some(&arg) = args.get(i) {
                            return arg;
                        }
                    }
                }
                return ty;
            }
            // `java.lang.Enum` members (`name`, `ordinal`) available on any enum value.
            let is_enum_val = self.syms.enums.keys().any(|en| {
                self.syms
                    .classes
                    .get(en)
                    .map_or(false, |c| c.internal == internal)
            });
            if is_enum_val {
                match name {
                    "name" => return Ty::String,
                    "ordinal" => return Ty::Int,
                    _ => {}
                }
            }
        }
        // Extension property: `recv.name` resolved by (receiver descriptor, name).
        if let Some((ty, _)) = self
            .syms
            .ext_props
            .get(&(rt.descriptor(), name.to_string()))
        {
            return *ty;
        }
        // Library-type property read (`list.size`): a Kotlin property is a zero-arg accessor on the
        // JVM — try the property's own name and its `getX()` form.
        if let Ty::Obj(internal, _) = rt {
            // `x` → `getX` (an `isFoo` boolean property keeps its name).
            let getter = if name.starts_with("is")
                && name
                    .as_bytes()
                    .get(2)
                    .map_or(false, |b| b.is_ascii_uppercase())
            {
                name.to_string()
            } else {
                let mut c = name.chars();
                format!(
                    "get{}{}",
                    c.next()
                        .map(|f| f.to_uppercase().to_string())
                        .unwrap_or_default(),
                    c.as_str()
                )
            };
            // Kotlin's built-in collection types remap a few property names to their JVM method (Kotlin
            // `Map.keys`/`entries` → `java.util.Map.keySet()`/`entrySet()`), like kotlinc's mapped members.
            let mapped = collection_mapped_accessor(&name).map(|s| s.to_string());
            for cand in [Some(name.to_string()), Some(getter), mapped]
                .into_iter()
                .flatten()
            {
                if let Some(m) = crate::call_resolver::resolve_instance(
                    &*self.syms.libraries,
                    internal,
                    &cand,
                    &[],
                ) {
                    if !matches!(m.ret, Ty::Unit | Ty::Error) {
                        return m.ret;
                    }
                }
            }
        }
        self.diags.error(
            span,
            format!("unresolved member '{name}' on '{}'", rt.name()),
        );
        Ty::Error
    }

    /// Probe a member read without emitting a diagnostic: returns `Some(ty)` if `recv.name` resolves,
    /// `None` otherwise (rolling back any error [`check_member`] would have reported). Used to resolve a
    /// bare name `length` inside a receiver-lambda body (`this`-relative) for an arbitrary receiver type.
    fn try_member_read(&mut self, rt: Ty, name: &str, span: Span) -> Option<Ty> {
        let n = self.diags.diags.len();
        let t = self.check_member(rt, name, span);
        if self.diags.diags.len() > n || t == Ty::Error {
            self.diags.diags.truncate(n);
            return None;
        }
        Some(t)
    }

    fn check_call(&mut self, call: ExprId, callee: ExprId, args: &[ExprId], span: Span) -> Ty {
        // Named arguments map onto parameter positions for a top-level function or a method whose
        // signature records parameter names (e.g. a data-class `copy`). Elsewhere the labels would be
        // silently ignored — reject instead.
        let arg_names = self.file.call_arg_names.get(&call.0).cloned();
        if arg_names.is_some() {
            let callee_expr = self.file.expr(callee).clone();
            let supports_named = match &callee_expr {
                Expr::Name(n) => self.syms.funs.contains_key(n),
                Expr::Member { receiver, name } => {
                    // A method with default parameters (e.g. data-class `copy`) — `required < params`.
                    let rt = self.expr(*receiver);
                    matches!(rt, Ty::Obj(i, _) if self.lookup_method(i, name).map_or(false, |s| s.required < s.params.len() && !s.param_names.is_empty()))
                }
                _ => false,
            };
            if !supports_named {
                for &a in args {
                    self.expr(a);
                }
                self.diags.error(span, "krusty: named arguments are only supported for top-level functions and methods with named parameters".to_string());
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
                            if let Some(members) = self
                                .syms
                                .libraries
                                .resolve_type(&internal)
                                .and_then(|t| t.annotation_members())
                            {
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
                // Nested-class construction `Outer.Inner(args)` — the source name `Outer.Inner` is a
                // registered class (kotlinc's `Outer$Inner`).
                if let Expr::Name(root) = self.file.expr(receiver).clone() {
                    if self.lookup(&root).is_none() {
                        let qname = format!("{root}.{name}");
                        if let Some(cls) = self.syms.classes.get(&qname).cloned() {
                            let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                            let params = if cls.ctor_params.len() == arg_tys.len() {
                                Some(cls.ctor_params.clone())
                            } else {
                                cls.secondary_ctors
                                    .iter()
                                    .find(|sp| sp.len() == arg_tys.len())
                                    .cloned()
                            };
                            match params {
                                Some(ps) => {
                                    for (i, (p, a)) in ps.iter().zip(&arg_tys).enumerate() {
                                        self.expect_assignable(
                                            *p,
                                            *a,
                                            self.span(args[i]),
                                            "argument",
                                        );
                                    }
                                }
                                None => self.diags.error(
                                    span,
                                    format!(
                                        "constructor '{qname}' expects {} args, got {}",
                                        cls.ctor_params.len(),
                                        arg_tys.len()
                                    ),
                                ),
                            }
                            return self.ctor_result(call, &cls.internal);
                        }
                    }
                }
                // `EnumName.values()` / `EnumName.valueOf(s)` — synthetic static enum methods.
                if let Expr::Name(en) = self.file.expr(receiver).clone() {
                    if self.lookup(&en).is_none() && self.syms.enums.contains_key(&en) {
                        let internal = self
                            .syms
                            .classes
                            .get(&en)
                            .map(|c| c.internal.clone())
                            .unwrap_or(en.clone());
                        if name == "values" && args.is_empty() {
                            return Ty::array(Ty::obj(&internal));
                        }
                        if name == "valueOf" && args.len() == 1 {
                            let at = self.expr(args[0]);
                            self.expect_assignable(Ty::String, at, self.span(args[0]), "argument");
                            return Ty::obj(&internal);
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
                                self.diags.error(
                                    span,
                                    format!(
                                        "constructor '{qualified}' expects {} args, got {}",
                                        cls.ctor_params.len(),
                                        arg_tys.len()
                                    ),
                                );
                            } else {
                                for (i, (p, a)) in cls.ctor_params.iter().zip(&arg_tys).enumerate()
                                {
                                    self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                                }
                            }
                            return Ty::obj(&cls.internal);
                        }
                    }
                }
                // `recv.run { … }` / `recv.apply { … }`: the lambda body has `recv` as its implicit
                // receiver (`this`); `run` yields the body, `apply` the receiver.
                if matches!(name.as_str(), "run" | "apply") && args.len() == 1 {
                    if let Expr::Lambda { params, body } = self.file.expr(args[0]).clone() {
                        if params.is_empty() {
                            let rt = self.expr(receiver);
                            let bt = self.check_with_receiver(rt, body, self.span(args[0]));
                            let returns_receiver = name == "apply";
                            self.receiver_lambdas.insert(
                                call,
                                ReceiverLambda {
                                    receiver,
                                    body,
                                    returns_receiver,
                                },
                            );
                            return self.set(call, if returns_receiver { rt } else { bt });
                        }
                    }
                }
                // `super.method(args)` — dispatch to the base class's method (non-virtual).
                if matches!(self.file.expr(receiver), Expr::Name(r) if r == "super") {
                    let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                    if let Some(Ty::Obj(internal, _)) = self.this_ty {
                        let sup = self
                            .syms
                            .class_by_internal(internal)
                            .and_then(|c| c.super_internal.clone());
                        if let Some(sup) = sup {
                            // A user base-class method.
                            if let Some(sig) = self.syms.method_of(&sup, &name) {
                                for (i, (p, a)) in sig.params.iter().zip(&arg_tys).enumerate() {
                                    self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                                }
                                return sig.ret;
                            }
                            // A classpath base-class method (`class C : ArrayList<…>() { … super.add(x) }`).
                            if let Some(m) = crate::call_resolver::resolve_instance(
                                &*self.syms.libraries,
                                &sup,
                                &name,
                                &arg_tys,
                            ) {
                                return m.ret;
                            }
                        }
                    }
                    self.diags
                        .error(span, format!("krusty: unresolved super method '{name}'"));
                    return Ty::Error;
                }
                // Java static call: `ClassName.method(args)` where ClassName is an imported class
                // (not a local/param) resolvable on the classpath.
                if let Expr::Name(cls) = self.file.expr(receiver).clone() {
                    if self.lookup(&cls).is_none() {
                        // `ClassName.fn(args)` — a companion (static) method call.
                        if let Some(sig) = self
                            .syms
                            .classes
                            .get(&cls)
                            .and_then(|c| c.static_methods.get(&name))
                            .cloned()
                        {
                            let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                            if sig.params.len() != arg_tys.len() {
                                self.diags.error(
                                    span,
                                    format!(
                                        "static method '{cls}.{name}' expects {} args, got {}",
                                        sig.params.len(),
                                        arg_tys.len()
                                    ),
                                );
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
                            return match self
                                .syms
                                .classes
                                .get(&cls)
                                .and_then(|c| c.methods.get(&name))
                                .cloned()
                            {
                                Some(sig) => {
                                    // Default arguments on object/companion methods aren't filled by the
                                    // emitter yet, so the call must supply exactly the declared params.
                                    if sig.params.len() != arg_tys.len() {
                                        self.diags.error(
                                            span,
                                            format!(
                                                "method '{cls}.{name}' expects {} args, got {}",
                                                sig.params.len(),
                                                arg_tys.len()
                                            ),
                                        );
                                    }
                                    for (i, (p, a)) in sig.params.iter().zip(&arg_tys).enumerate() {
                                        self.expect_assignable(
                                            *p,
                                            *a,
                                            self.span(args[i]),
                                            "argument",
                                        );
                                    }
                                    sig.ret
                                }
                                None => {
                                    self.diags
                                        .error(span, format!("unresolved reference '{name}'."));
                                    Ty::Error
                                }
                            };
                        }
                        if let Some(internal) = self.imports.get(&cls).cloned() {
                            let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                            return match crate::call_resolver::resolve_companion(
                                &*self.syms.libraries,
                                &internal,
                                &name,
                                &arg_tys,
                            ) {
                                Some(m) => m.ret,
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
                    Ty::Obj(internal, _) => self.lookup_method(internal, &name),
                    _ => None,
                };
                // A library extension taking a lambda (`list.map { it … }`): the lambda's parameter
                // types are recovered from the extension's generic signature — bound by the receiver's
                // element type and the non-lambda arguments — so the lambda body checks against `Int`
                // rather than the erased `Any`. Type the non-lambda arguments first (the accumulator in
                // `fold(0) { acc, x -> }` binds `R`); lambda positions are `None` until resolved.
                let ext_lambda_pts: Option<Vec<Vec<Ty>>> = if method_sig.is_none()
                    && rt != Ty::Error
                    && args
                        .iter()
                        .any(|&a| matches!(self.file.expr(a), Expr::Lambda { .. }))
                {
                    let partial: Vec<Option<Ty>> = args
                        .iter()
                        .map(|&a| {
                            if matches!(self.file.expr(a), Expr::Lambda { .. }) {
                                None
                            } else {
                                Some(self.expr(a))
                            }
                        })
                        .collect();
                    self.syms
                        .libraries
                        .extension_lambda_param_types(rt, &name, &partial)
                } else {
                    None
                };
                // Array/`String` `forEach`/`forEachIndexed` have no `Obj` generic signature; supply the
                // lambda parameter types directly from the element (the index is `Int`).
                let ext_lambda_pts = ext_lambda_pts.or_else(|| {
                    if matches!(name.as_str(), "forEach" | "forEachIndexed") {
                        let elem = if rt == Ty::String {
                            Some(Ty::Char)
                        } else {
                            rt.array_elem()
                        };
                        if let Some(elem) = elem {
                            return Some(if name == "forEach" {
                                vec![vec![elem]]
                            } else {
                                vec![vec![Ty::Int, elem]]
                            });
                        }
                    }
                    None
                });
                // A USER extension taking a lambda (`inline fun String.withLen(f: (String)->Int)`): its
                // `Signature` carries the lambda parameter types directly. For a GENERIC-receiver
                // extension (keyed under `Any`), specialize the receiver type parameter to `rt` so the
                // lambda's `it` types as the actual receiver, not the erased `Any`.
                let ext_lambda_pts = ext_lambda_pts.or_else(|| {
                    if rt == Ty::Error {
                        return None;
                    }
                    let has_lam =
                        |s: &Signature| s.lambda_param_types.iter().any(|v| !v.is_empty());
                    if let Some(s) = self
                        .syms
                        .ext_funs
                        .get(&(rt.descriptor(), name.clone()))
                        .filter(|s| has_lam(s))
                    {
                        return Some(s.lambda_param_types.clone());
                    }
                    // Generic receiver: the decl's receiver type param → `rt` in the lambda param types.
                    let any_desc = Ty::obj("kotlin/Any").descriptor();
                    let s = self.syms.ext_funs.get(&(any_desc, name.clone()))?;
                    if !has_lam(s) {
                        return None;
                    }
                    let recv_tp = self
                        .file
                        .decls
                        .iter()
                        .find_map(|&d| match self.file.decl(d) {
                            Decl::Fun(fd)
                                if fd.name == name
                                    && fd.receiver.as_ref().is_some_and(|r| {
                                        fd.type_params.iter().any(|tp| tp == &r.name)
                                    }) =>
                            {
                                fd.receiver.as_ref().map(|r| r.name.clone())
                            }
                            _ => None,
                        });
                    let any = Ty::obj("kotlin/Any");
                    Some(
                        s.lambda_param_types
                            .iter()
                            .map(|v| {
                                v.iter()
                                    .map(|t| {
                                        if recv_tp.is_some() && *t == any {
                                            rt
                                        } else {
                                            *t
                                        }
                                    })
                                    .collect()
                            })
                            .collect(),
                    )
                });
                // A call selected by lambda RETURN type (`recv.sumOf { … }`): its source name has no JVM
                // method, so the generic-signature passes above miss it — supply the selector's `it` from
                // the receiver's element type, at the lambda argument's position.
                let ext_lambda_pts = ext_lambda_pts.or_else(|| {
                    let params = self
                        .syms
                        .libraries
                        .lambda_return_overload_param(rt, &name)?;
                    Some(
                        args.iter()
                            .map(|&a| {
                                if matches!(self.file.expr(a), Expr::Lambda { .. }) {
                                    params.clone()
                                } else {
                                    Vec::new()
                                }
                            })
                            .collect(),
                    )
                });
                // A call to an INLINE extension (`forEach`/`let`/`also`/`apply`/… or any user/stdlib inline
                // extension) is spliced at the call site, so a mutable variable its lambda captures is an
                // inline capture (no closure) — permit mutation so the checker doesn't `Ref`-box it. Gated
                // on the extension actually being inline (a non-inline lambda capture must still be boxed).
                // Permit for this call only; the lowering must inline (or bail), never form a closure.
                let prev_allow_mut = self.allow_lambda_mutation;
                self.allow_lambda_mutation =
                    ext_lambda_pts.is_some() && self.resolver().extension_is_inline(rt, &name);
                let arg_tys: Vec<Ty> = args
                    .iter()
                    .enumerate()
                    .map(|(i, &a)| {
                        if let Some(ref sig) = method_sig {
                            if i < sig.lambda_param_types.len()
                                && !sig.lambda_param_types[i].is_empty()
                                && matches!(self.file.expr(a), Expr::Lambda { .. })
                            {
                                let pt = sig.lambda_param_types[i].clone();
                                return self.check_lambda_with_types(a, &pt);
                            }
                        }
                        if let Some(ref pts) = ext_lambda_pts {
                            if pts.get(i).map_or(false, |v| !v.is_empty())
                                && matches!(self.file.expr(a), Expr::Lambda { .. })
                            {
                                let pt = pts[i].clone();
                                return self.check_lambda_with_types(a, &pt);
                            }
                        }
                        self.expr(a)
                    })
                    .collect();
                self.allow_lambda_mutation = prev_allow_mut;
                if rt == Ty::Error {
                    return Ty::Error;
                }
                if let ("toString", []) = (name.as_str(), arg_tys.as_slice()) {
                    return Ty::String; // intrinsic on any type
                }
                if rt == Ty::String {
                    if let Some(ret) = self
                        .syms
                        .libraries
                        .builtin_member_ret("kotlin/String", &name, &arg_tys)
                        .or_else(|| resolve_string_instance(&name, &arg_tys))
                    {
                        return ret;
                    }
                    // `trimIndent()`/`trimMargin()` — stdlib extensions; krusty folds them at compile
                    // time on a string-literal receiver (codegen rejects a non-literal receiver).
                    if matches!(name.as_str(), "trimIndent" | "trimMargin") && arg_tys.is_empty() {
                        return Ty::String;
                    }
                }
                // Numeric/`Char` conversion intrinsics: `n.toInt()`/`toLong()`/`c.toChar()`/….
                if (rt.is_numeric() || rt == Ty::Char || rt.is_unsigned()) && arg_tys.is_empty() {
                    if let Some(target) = conversion_target(&name) {
                        return target;
                    }
                    // `inc`/`dec` on an unsigned value return the same unsigned type.
                    if rt.is_unsigned() && matches!(name.as_str(), "inc" | "dec") {
                        return rt;
                    }
                }
                // Curated `java.lang.StringBuilder` instance methods (append/toString/length).
                if rt == Ty::obj("java/lang/StringBuilder") {
                    if let Some(ret) = resolve_stringbuilder_instance(&name, &arg_tys) {
                        return ret;
                    }
                }
                // Instance method call on a class value: `p.method(args)` (own or inherited).
                if let Ty::Obj(internal, _) = rt {
                    // The user member resolved through the current module as a `SymbolSource`
                    // (`ModuleSymbols`); its DFS member walk matches `lookup_method`, so the first Member
                    // overload is the same one hand-rolled lookup would pick. Collected owned so the
                    // borrow of `syms` ends before the mutating type-checks below.
                    let module_member = crate::module_symbols::ModuleSymbols::new(self.syms)
                        .functions(&name, Some(rt))
                        .overloads
                        .into_iter()
                        .find(|o| o.kind == crate::libraries::FnKind::Member);
                    if let Some(fi) = module_member {
                        let params = fi.callable.params.clone();
                        let cs = &fi.call_sig;
                        // Named or omitted arguments (a method with parameter defaults, e.g. data-class
                        // `copy`): map by name/position via the parameter names, honouring `required`.
                        if (arg_names.is_some() || arg_tys.len() != params.len())
                            && cs.required < params.len()
                            && !cs.param_names.is_empty()
                        {
                            match map_call_args(
                                args,
                                arg_names.as_deref(),
                                &cs.param_names,
                                cs.required,
                                &cs.param_defaults,
                            ) {
                                Ok(slots) => {
                                    for (i, slot) in slots.iter().enumerate() {
                                        if let Some(a) = slot {
                                            self.expect_assignable(
                                                params[i],
                                                self.expr_types[a.0 as usize],
                                                self.span(*a),
                                                "argument",
                                            );
                                        }
                                    }
                                }
                                Err(msg) => {
                                    self.diags.error(span, format!("call to '{name}': {msg}"))
                                }
                            }
                            return fi.callable.ret;
                        }
                        if params.len() != arg_tys.len() {
                            self.diags.error(
                                span,
                                format!(
                                    "method '{name}' expects {} args, got {}",
                                    params.len(),
                                    arg_tys.len()
                                ),
                            );
                        } else {
                            for (i, (p, a)) in params.iter().zip(&arg_tys).enumerate() {
                                self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                            }
                        }
                        return fi.callable.ret;
                    }
                    // A classpath Java object: resolve the instance method via the `.class` reader.
                    if let Some(m) = crate::call_resolver::resolve_instance(
                        &*self.syms.libraries,
                        internal,
                        &name,
                        &arg_tys,
                    ) {
                        // A parameterized receiver (`List<Int>`) recovers the member's substituted
                        // return (`get` → `Int`) from the generic signature; else the erased return.
                        let ret = self
                            .syms
                            .libraries
                            .member_return(rt, &name, &arg_tys)
                            .unwrap_or(m.ret);
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
                    if matches!(name.as_str(), "shl" | "shr" | "ushr" | "and" | "or" | "xor")
                        && arg_tys.len() == 1
                    {
                        let expected = if matches!(name.as_str(), "shl" | "shr" | "ushr") {
                            Ty::Int
                        } else {
                            rt
                        };
                        self.expect_assignable(
                            expected,
                            arg_tys[0],
                            self.span(args[0]),
                            "argument",
                        );
                        return rt;
                    }
                }
                // A builtin operator-method on a primitive (`5.rem(2)`, `5.plus(2)`) binds to the
                // primitive operator, which *beats* any same-named user extension (in Kotlin a
                // member/builtin wins over an extension). The arithmetic/compare/unary forms map
                // directly to the equivalent operator bytecode (see the mirror in `emit_call`); the
                // rest (`mod` floor-semantics, `rangeTo`, `inc`/`dec`) aren't modeled → reject rather
                // than dispatch to a user extension, which would miscompile.
                if rt.is_primitive() && is_builtin_operator_method(&name) {
                    // A user `infix`/`operator` extension with this name shadows the builtin for the
                    // *infix* form (`a rem b`) while the dot form (`a.rem(b)`) keeps the builtin —
                    // but krusty parses both to the same AST, so it can't tell them apart. When such
                    // an extension exists, reject (skip) rather than risk picking the wrong one.
                    let user_ext = self
                        .syms
                        .ext_funs
                        .contains_key(&(rt.descriptor(), name.clone()));
                    if rt.is_numeric() && !user_ext {
                        // Binary arithmetic methods: `a.plus(b)` ≡ `a + b` (same numeric promotion).
                        let bin = match name.as_str() {
                            "plus" => Some(BinOp::Add),
                            "minus" => Some(BinOp::Sub),
                            "times" => Some(BinOp::Mul),
                            "div" => Some(BinOp::Div),
                            "rem" => Some(BinOp::Rem),
                            _ => None,
                        };
                        if let (Some(op), [at]) = (bin, arg_tys.as_slice()) {
                            return self.check_binary(op, rt, *at, span);
                        }
                        // `a.compareTo(b)` → `Int` (emitted via `{Integer,Long,Float,Double}.compare`).
                        if name == "compareTo" {
                            if let [at] = arg_tys.as_slice() {
                                if Ty::promote(rt, *at).is_some() {
                                    return Ty::Int;
                                }
                            }
                        }
                        // Unary `a.unaryMinus()` / `a.unaryPlus()` → the receiver's numeric type.
                        if matches!(name.as_str(), "unaryMinus" | "unaryPlus") && arg_tys.is_empty()
                        {
                            return rt;
                        }
                    }
                    // `Char` arithmetic methods: `c.plus(n): Char`, `c.minus(n): Char`, `c.minus(c2): Int`.
                    // `Char` isn't `is_numeric` (no promotion), but these map to the operator form, which
                    // `check_binary` types with the correct `Char`/`Int` operand rules.
                    if rt == Ty::Char && !user_ext {
                        let bin = match name.as_str() {
                            "plus" => Some(BinOp::Add),
                            "minus" => Some(BinOp::Sub),
                            _ => None,
                        };
                        if let (Some(op), [at]) = (bin, arg_tys.as_slice()) {
                            return self.check_binary(op, rt, *at, span);
                        }
                    }
                    self.diags.error(span, format!("krusty: builtin operator method '{name}' on a primitive is not supported"));
                    return Ty::Error;
                }
                // Extension / static method from any classpath library (e.g. Kotlin stdlib).
                // Receiver type is passed as the first argument (invokestatic at the JVM level).
                let call_targs: Vec<Ty> = self
                    .file
                    .call_type_args
                    .get(&call.0)
                    .cloned()
                    .map(|ts| ts.iter().map(|r| self.resolve_ty(r)).collect())
                    .unwrap_or_default();
                if let Some(c) =
                    self.syms
                        .libraries
                        .resolve_callable(&name, Some(rt), &arg_tys, &call_targs)
                {
                    self.ext_calls.insert(call, (c.owner, c.name, c.descriptor));
                    return c.ret;
                }
                // A call selected by lambda RETURN type (`recv.sumOf { it * 2 }: Int`): the `@JvmName`
                // overload matching the lambda's return is resolved from `@Metadata`; the result is that
                // return type. (Spliced in lowering — no `ext_call` recorded.)
                if let Some(lam_ret) = arg_tys.iter().find_map(|t| {
                    if let Ty::Fun(s) = t {
                        Some(s.ret)
                    } else {
                        None
                    }
                }) {
                    if let Some(c) = self
                        .resolver()
                        .resolve_lambda_return_overload(rt, &name, lam_ret, &arg_tys)
                    {
                        return c.ret;
                    }
                }
                // A non-public (`@InlineOnly`) extension the backend SPLICES (no callable method to call):
                // a lambda-bearing scope fn (`takeIf`/`takeUnless`/…), recovering the receiver-bound return.
                if args.len() == 1 && matches!(self.file.expr(args[0]), Expr::Lambda { .. }) {
                    if let Some(c) = self
                        .syms
                        .libraries
                        .resolve_scope_inline(&name, rt, &arg_tys)
                    {
                        return c.ret;
                    }
                }
                // A NO-lambda `@InlineOnly` extension on a NON-UNSIGNED PRIMITIVE receiver returning a
                // primitive/`String` — `Char.isDigit()`/`isLetter()`/`uppercaseChar()` (inline
                // `Character.isDigit(this)`/`toUpperCase(this)`). Restricted to this shape because the
                // generic no-lambda splice is value-correct only for these simple bodies: a function-typed
                // parameter (`let`/`apply` fallback) → `IllegalAccessError`, and an unsigned/value-class or
                // multi-step reference body (`StringBuilder.appendLine`) → wrong values. Gated further on
                // `can_inline_call` (the body is actually spliceable), so the checker accepts only what the
                // emitter splices correctly. No name match — the receiver/return SHAPE selects it.
                if rt.is_primitive()
                    && !rt.is_unsigned()
                    && (arg_tys.is_empty()
                        || arg_tys.iter().all(|a| a.is_primitive() && !a.is_unsigned()))
                {
                    if let Some(c) = self
                        .syms
                        .libraries
                        .resolve_scope_inline(&name, rt, &arg_tys)
                    {
                        // The KOTLIN return must be a real primitive/`String` — not an unsigned type the
                        // JVM signature erased to a signed primitive (`toUShort(): UShort` reads back as
                        // `Short`), which would splice to a wrong value (krusty's `Ty` can't model it).
                        let ret_ok = ((c.ret.is_primitive() && !c.ret.is_unsigned())
                            || c.ret == Ty::String)
                            && !self
                                .syms
                                .libraries
                                .metadata_return_unsigned(&c.owner, &c.name);
                        let no_fun = !c.descriptor.contains("Lkotlin/jvm/functions/Function");
                        if ret_ok
                            && no_fun
                            && self
                                .syms
                                .libraries
                                .can_inline_call(&c.owner, &c.name, &c.descriptor)
                        {
                            return c.ret;
                        }
                    }
                }
                // User-defined extension function in this file (invokestatic on the file facade), resolved
                // through the current module as a `SymbolSource`. The exact-receiver overload is rung 0;
                // its `callable.params` prepend the receiver and `callable.descriptor` is the full static
                // `(recv + params)ret` the emitter wants.
                {
                    let module_ext = crate::module_symbols::ModuleSymbols::new(self.syms)
                        .functions(&name, Some(rt))
                        .overloads
                        .into_iter()
                        .find(|o| {
                            o.kind == crate::libraries::FnKind::Extension && o.receiver_rank == 0
                        });
                    if let Some(fi) = module_ext {
                        // Logical params (the receiver is `callable.params[0]`; the rest are the args).
                        let logical: Vec<Ty> = fi.callable.params[1..].to_vec();
                        if logical.len() != arg_tys.len() {
                            self.diags.error(
                                span,
                                format!(
                                    "extension '{name}' expects {} args, got {}",
                                    logical.len(),
                                    arg_tys.len()
                                ),
                            );
                        } else {
                            for (i, (p, a)) in logical.iter().zip(&arg_tys).enumerate() {
                                self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                            }
                        }
                        self.ext_calls.insert(
                            call,
                            (
                                "$local".to_string(),
                                name.clone(),
                                fi.callable.descriptor.clone(),
                            ),
                        );
                        return fi.callable.ret;
                    }
                }
                // A user GENERIC-receiver extension `<T> T.foo()` — its receiver erases to `kotlin/Any`,
                // so it's keyed under the `Any` descriptor and matches ANY actual receiver. Specialize the
                // return: a return naming the receiver type param (`T`) → the actual receiver type `rt`;
                // one naming a value-param type param → that argument's type; else the declared return.
                if rt.descriptor() != Ty::obj("kotlin/Any").descriptor() {
                    // The generic-receiver extension keys under the `Any` descriptor — rung 1 in the
                    // module source's extension lookup (rung 0 is the exact receiver, handled above).
                    let module_ext = crate::module_symbols::ModuleSymbols::new(self.syms)
                        .functions(&name, Some(rt))
                        .overloads
                        .into_iter()
                        .find(|o| {
                            o.kind == crate::libraries::FnKind::Extension && o.receiver_rank == 1
                        });
                    if let Some(fi) = module_ext {
                        let logical: Vec<Ty> = fi.callable.params[1..].to_vec();
                        if logical.len() == arg_tys.len() {
                            if let Some(decl) =
                                self.file
                                    .decls
                                    .iter()
                                    .find_map(|&d| match self.file.decl(d) {
                                        // Only INLINE generic-receiver extensions are handled here (the body
                                        // is spliced with `this` specialized to the actual type). A NON-inline
                                        // generic extension needs erased-`Object` boxing at the real call,
                                        // which this path doesn't model — leave it unresolved (skip).
                                        Decl::Fun(fd)
                                            if fd.name == name
                                                && fd.is_inline
                                                && fd.receiver.as_ref().is_some_and(|r| {
                                                    fd.type_params.iter().any(|tp| tp == &r.name)
                                                }) =>
                                        {
                                            Some(fd.clone())
                                        }
                                        _ => None,
                                    })
                            {
                                for (i, (p, a)) in logical.iter().zip(&arg_tys).enumerate() {
                                    self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                                }
                                let recv_tp = decl.receiver.as_ref().map(|r| r.name.clone());
                                let ret = match &decl.ret {
                                    Some(r) if Some(&r.name) == recv_tp.as_ref() => rt,
                                    Some(r) => decl
                                        .params
                                        .iter()
                                        .zip(&arg_tys)
                                        .find(|(p, _)| p.ty.name == r.name)
                                        .map(|(_, a)| *a)
                                        .unwrap_or(fi.callable.ret),
                                    None => Ty::Unit,
                                };
                                // Inline only (the body is spliced — no `ext_call` to emit).
                                return ret;
                            }
                        }
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
                        ("isEmpty", 0) | ("isNotEmpty", 0) => return Ty::Boolean,
                        ("count", 0) => return Ty::Int,
                        _ => {}
                    }
                }
                // Inner-class construction `outerInstance.Inner(args)` → `new Outer$Inner(outer, args)`.
                if let Some(outer_internal) = rt.obj_internal() {
                    let inner_internal = format!("{outer_internal}${name}");
                    if let Some(inner) = self
                        .syms
                        .classes
                        .values()
                        .find(|cs| {
                            cs.internal == inner_internal
                                && cs.inner_of.as_deref() == Some(outer_internal)
                        })
                        .cloned()
                    {
                        if inner.ctor_params.len() == arg_tys.len() {
                            for (i, (p, a)) in inner.ctor_params.iter().zip(&arg_tys).enumerate() {
                                self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                            }
                            return Ty::obj(&inner_internal);
                        }
                    }
                }
                self.diags.error(
                    span,
                    format!("unresolved method '{name}' on '{}'", rt.name()),
                );
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
                        self.diags.error(
                            span,
                            format!(
                                "local function '{fname}' expects {} args, got {}",
                                sig.params.len(),
                                arg_tys.len()
                            ),
                        );
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
                            let bt = self.check_with_receiver(rt, body, self.span(args[1]));
                            self.receiver_lambdas.insert(
                                call,
                                ReceiverLambda {
                                    receiver: args[0],
                                    body,
                                    returns_receiver: false,
                                },
                            );
                            return self.set(call, bt);
                        }
                    }
                }
                // SAM conversion `Pred { lambda }` — a (fun) interface with a single abstract method
                // built from a lambda. Type the lambda from the SAM method's parameters; the result is
                // the interface type.
                if args.len() == 1
                    && matches!(self.file.expr(args[0]), Expr::Lambda { .. })
                    && self.lookup(&fname).is_none()
                {
                    if let Some(cls) = self.syms.classes.get(&fname).cloned() {
                        if cls.is_interface && cls.methods.len() == 1 {
                            let pts = cls.methods.values().next().unwrap().params.clone();
                            self.check_lambda_with_types(args[0], &pts);
                            return self.set(call, Ty::obj(&cls.internal));
                        }
                    }
                    // A classpath functional interface (`Runnable`, `Comparator`, …).
                    if let Some(internal) = self.syms.class_names.get(&fname).cloned() {
                        if let Some(sam) = self.syms.libraries.sam_method(&internal) {
                            self.check_lambda_with_types(args[0], &sam.params);
                            return self.set(call, Ty::obj(&internal));
                        }
                    }
                }
                // Type-directed lambda checking: if we know the target function's signature and a
                // parameter is a function type with known inner param types, check lambda args with
                // the correct `it` type instead of always using Object.
                // For lambda-argument pre-typing we need a single known signature; use it only when the
                // name is unambiguous (one overload). An overloaded call's lambda `it` falls back to the
                // erased type — a minor precision loss, not a miscompile.
                let known_sig = self
                    .syms
                    .funs
                    .get(&fname)
                    .and_then(|v| (v.len() == 1).then(|| v[0].clone()));
                // An array init constructor `IntArray(n) { i -> … }` / `Array(n) { i -> … }` types its
                // lambda's parameter (the index) as `Int`.
                let array_init_lambda = (Ty::primitive_array_element(&fname).is_some()
                    || fname == "Array")
                    && args.len() == 2
                    && matches!(self.file.expr(args[1]), Expr::Lambda { .. });
                // A receiver-less top-level *library* function with a lambda argument (`applyIt(5){ it+1 }`):
                // recover the lambda parameter types from its generic signature so `it` types correctly
                // (the erased `Function1` descriptor hides them), mirroring the extension-call path.
                // Non-lambda argument types, computed once here for a top-level lib fn with a lambda
                // argument (to recover the lambda's parameter types from the fn's generic signature) and
                // reused in the `arg_tys` loop below so they aren't re-typed (no duplicate diagnostics).
                // Non-lambda argument types, computed once for ANY receiver-less call with a lambda argument
                // (a user fn allowed too, so a user generic inline HOF reaches `user_generic_call`).
                let toplevel_partial: Option<Vec<Option<Ty>>> = if self.lookup(&fname).is_none()
                    && !array_init_lambda
                    && args
                        .iter()
                        .any(|&a| matches!(self.file.expr(a), Expr::Lambda { .. }))
                {
                    Some(
                        args.iter()
                            .map(|&a| {
                                if matches!(self.file.expr(a), Expr::Lambda { .. }) {
                                    None
                                } else {
                                    Some(self.expr(a))
                                }
                            })
                            .collect(),
                    )
                } else {
                    None
                };
                // A user generic inline HOF (`twice(1){…}`): bind its type params from the value args to
                // recover the lambda parameter types and the specialized return type.
                let user_generic: Option<Vec<Vec<Ty>>> = toplevel_partial
                    .as_ref()
                    .and_then(|partial| self.user_generic_call(&fname, partial));
                let toplevel_lambda_pts: Option<Vec<Vec<Ty>>> = toplevel_partial
                    .as_ref()
                    // A library top-level function only when no user function shadows it.
                    .filter(|_| known_sig.is_none())
                    .and_then(|partial| {
                        self.syms
                            .libraries
                            .toplevel_lambda_param_types(&fname, partial)
                    })
                    .or_else(|| user_generic.clone());
                // A top-level NON-public (`@InlineOnly`) inline fn (`require`/`check`) inlines its lambda
                // argument (or the file is skipped), so a mutable capture is an inline capture — type the
                // lambda body with mutation allowed (don't `Ref`-box the captured var).
                let toplevel_must_inline = self.lookup(&fname).is_none()
                    && !self.syms.funs.contains_key(&fname)
                    && args
                        .iter()
                        .any(|&a| matches!(self.file.expr(a), Expr::Lambda { .. }))
                    && self.resolver().toplevel_has_must_inline(&fname);
                let arg_tys: Vec<Ty> = args
                    .iter()
                    .enumerate()
                    .map(|(i, &a)| {
                        if array_init_lambda && i == 1 {
                            return self.check_lambda_with_types(a, &[Ty::Int]);
                        }
                        if let Some(ref pts) = toplevel_lambda_pts {
                            if matches!(self.file.expr(a), Expr::Lambda { .. })
                                && i < pts.len()
                                && !pts[i].is_empty()
                            {
                                // A top-level INLINE fn (`repeat`/`run`/…) splices its lambda, so a mutable
                                // variable the lambda captures is an inline capture (no `Ref` box).
                                let prev = self.allow_lambda_mutation;
                                self.allow_lambda_mutation =
                                    self.resolver().toplevel_is_inline(&fname);
                                let t = self.check_lambda_with_types(a, &pts[i]);
                                self.allow_lambda_mutation = prev;
                                return t;
                            }
                        }
                        // A zero-arg lambda to a NON-public (`@InlineOnly`) inline fn (`require(c){m}`):
                        // type its body with mutation allowed (the lambda is spliced, so a mutable capture
                        // is an inline capture, not a `Ref`). After the `repeat`/`pts` branches so those win.
                        if toplevel_must_inline && matches!(self.file.expr(a), Expr::Lambda { .. })
                        {
                            let pt = toplevel_lambda_pts
                                .as_ref()
                                .and_then(|pts| pts.get(i))
                                .cloned()
                                .unwrap_or_default();
                            let prev = self.allow_lambda_mutation;
                            self.allow_lambda_mutation = true;
                            let t = self.check_lambda_with_types(a, &pt);
                            self.allow_lambda_mutation = prev;
                            return t;
                        }
                        // Reuse the already-computed non-lambda argument type (avoid re-typing).
                        if let Some(Some(t)) = toplevel_partial.as_ref().and_then(|p| p.get(i)) {
                            return *t;
                        }
                        if let Some(ref sig) = known_sig {
                            // A lambda argument to a function-typed parameter. For an `inline fun` the lambda
                            // is inlined into the caller, so it may capture a mutable local (like the stdlib
                            // `repeat`/`forEach`). This also covers zero-parameter lambdas (`() -> Unit`),
                            // whose `lambda_param_types[i]` is empty.
                            if i < sig.params.len()
                                && matches!(sig.params[i], Ty::Fun(_))
                                && matches!(self.file.expr(a), Expr::Lambda { .. })
                            {
                                let pt = sig.lambda_param_types.get(i).cloned().unwrap_or_default();
                                let prev = self.allow_lambda_mutation;
                                self.allow_lambda_mutation = sig.is_inline;
                                let t = self.check_lambda_with_types(a, &pt);
                                self.allow_lambda_mutation = prev;
                                return t;
                            }
                        }
                        self.expr(a)
                    })
                    .collect();
                if fname == "println" {
                    return Ty::Unit; // builtin: accepts one value of any type (v0)
                }
                if self.lookup(&fname).is_none() {
                    // The array creators are compiler intrinsics keyed on the resolved stdlib symbol; a
                    // user-defined function of the same name shadows them (as in kotlinc), so only treat
                    // the name as the intrinsic when it isn't a user-declared top-level function.
                    if !self.syms.funs.contains_key(&fname) {
                        // `arrayOfNulls<T>(n): Array<T?>` — a reified intrinsic; the element is the
                        // explicit type argument (a reference; a primitive would need a boxed `Integer[]`,
                        // not modeled → fall through to skip). Codegen allocates `new T[n]` (`b_arr_nulls`).
                        if fname == "arrayOfNulls" && args.len() == 1 {
                            self.expect_assignable(
                                Ty::Int,
                                arg_tys[0],
                                self.span(args[0]),
                                "array size",
                            );
                            let elem = self
                                .file
                                .call_type_args
                                .get(&call.0)
                                .and_then(|ts| ts.first())
                                .map(|r| self.resolve_ty(r))
                                .unwrap_or_else(|| Ty::obj("kotlin/Any"));
                            if elem.is_reference() {
                                return Ty::array(elem);
                            }
                        }
                        if let Some(t) = self.check_array_builtin(&fname, args, &arg_tys, span) {
                            return t;
                        }
                    }
                    // Unqualified companion (static) method call inside a companion member.
                    if let Some(cls) = self.companion_of.clone() {
                        if let Some(sig) = self
                            .syms
                            .classes
                            .get(&cls)
                            .and_then(|c| c.static_methods.get(&fname))
                            .cloned()
                        {
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
                            && (got..ctor_params.len()).all(|i| {
                                match cls.ctor_defaults.get(i).copied().flatten() {
                                    Some(dx) => {
                                        let pt = ctor_params[i];
                                        match self.file.expr(dx) {
                                            Expr::IntLit(_) => matches!(
                                                pt,
                                                Ty::Int | Ty::Byte | Ty::Short | Ty::Char
                                            ),
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
                                }
                            });
                        // The arguments don't match the primary — try a secondary constructor. Prefer one
                        // whose parameter TYPES accept the arguments (`A(123)` is the `Int` secondary, not
                        // the same-arity `String` one); fall back to the first same-arity ctor.
                        if !ok_arity {
                            let chosen = cls
                                .secondary_ctors
                                .iter()
                                .find(|sp| sp.len() == got && self.ctor_args_match(sp, &arg_tys))
                                .or_else(|| cls.secondary_ctors.iter().find(|sp| sp.len() == got));
                            if let Some(sparams) = chosen {
                                for (i, (p, a)) in sparams.iter().zip(&arg_tys).enumerate() {
                                    self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                                }
                                return self.ctor_result(call, &cls.internal);
                            }
                            self.diags.error(
                                span,
                                format!(
                                    "constructor '{fname}' expects {} args, got {}",
                                    ctor_params.len(),
                                    got
                                ),
                            );
                        } else {
                            // Primary arity matches but the argument TYPES may not (a same-arity
                            // secondary, e.g. `Sc(Int)` primary vs `Sc(String)` secondary) — prefer a
                            // secondary whose parameter types accept the arguments.
                            if got == ctor_params.len()
                                && !self.ctor_args_match(&ctor_params, &arg_tys)
                            {
                                if let Some(sparams) = cls
                                    .secondary_ctors
                                    .iter()
                                    .find(|sp| self.ctor_args_match(sp, &arg_tys))
                                {
                                    for (i, (p, a)) in sparams.iter().zip(&arg_tys).enumerate() {
                                        self.expect_assignable(
                                            *p,
                                            *a,
                                            self.span(args[i]),
                                            "argument",
                                        );
                                    }
                                    return self.ctor_result(call, &cls.internal);
                                }
                            }
                            for (i, (p, a)) in ctor_params.iter().zip(&arg_tys).enumerate() {
                                self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                            }
                        }
                        return self.ctor_result(call, &cls.internal);
                    }
                    // Constructing a classpath Java type: `Calc()` where `Calc` is imported.
                    if let Some(internal) = self.imports.get(&fname).cloned() {
                        if crate::call_resolver::resolve_constructor(
                            &*self.syms.libraries,
                            &internal,
                            &arg_tys,
                        )
                        .is_some()
                        {
                            return self.ctor_result(call, &internal);
                        }
                    }
                    // A library type by simple name (`throw RuntimeException("msg")`, a mapped/aliased
                    // type with no explicit import): ask the library to resolve the constructor. The
                    // library owns any target-specific knowledge (e.g. the throwable-ctor shapes the
                    // JVM jimage can't surface) — the resolver no longer special-cases throwables.
                    if let Some(internal) = self.syms.class_names.get(&fname).cloned() {
                        if crate::call_resolver::resolve_constructor(
                            &*self.syms.libraries,
                            &internal,
                            &arg_tys,
                        )
                        .is_some()
                        {
                            return self.ctor_result(call, &internal);
                        }
                    }
                    // `StringBuilder()` / `StringBuilder("init")` / `StringBuilder(capacity)`.
                    if fname == "StringBuilder"
                        && matches!(arg_tys.as_slice(), [] | [Ty::String] | [Ty::Int])
                    {
                        return Ty::obj("java/lang/StringBuilder");
                    }
                    // `Any()` constructs java.lang.Object (Kotlin's root type).
                    if fname == "Any" && arg_tys.is_empty() {
                        return Ty::obj("kotlin/Any");
                    }
                }
                // Unqualified call to a sibling instance method: `foo()` → `this.foo()`. Inside an
                // inner class, an unqualified call may target an enclosing method (`this.this$0.foo()`).
                if !self.syms.funs.contains_key(&fname) {
                    if let Some(Ty::Obj(internal, _)) = self.this_ty {
                        let sig = self.lookup_method(internal, &fname).or_else(|| {
                            self.syms
                                .class_by_internal(internal)
                                .and_then(|c| c.inner_of.clone())
                                .and_then(|outer| self.lookup_method(&outer, &fname))
                        });
                        if let Some(sig) = sig {
                            for (i, (p, a)) in sig.params.iter().zip(&arg_tys).enumerate() {
                                self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                            }
                            return sig.ret;
                        }
                    }
                }
                // Resolve a receiver-less call: a user top-level function shadows everything; otherwise
                // an implicit-receiver member (receiver-lambda body), then a library top-level function.
                // The current module is queried as a `SymbolSource` (ModuleSymbols) and libraries through
                // the classpath set — the federation precedence (module > implicit-receiver > library) made
                // explicit, replacing the scattered `syms.funs.contains_key` guards.
                let user_shadows = self.syms.funs.contains_key(&fname);
                let module_top: Option<crate::libraries::FunctionInfo> = self
                    .syms
                    .funs
                    .get(&fname)
                    .and_then(|sigs| pick_overload(sigs, &arg_tys))
                    .map(|i| {
                        // ModuleSymbols builds overloads in `funs` order, so index `i` aligns.
                        let mut fs = crate::module_symbols::ModuleSymbols::new(self.syms)
                            .functions(&fname, None);
                        fs.overloads.swap_remove(i)
                    });
                if module_top.is_none() && !user_shadows {
                    // Unqualified call to a member of the implicit receiver of a builtin/library type — a
                    // receiver-lambda body (`"ab".run { uppercase() }`, `sb.apply { append(x) }`).
                    if let Some(rt) = self.this_ty {
                        if let Some(ret) = self.this_member_call_ret(rt, &fname, &arg_tys, args) {
                            return ret;
                        }
                    }
                }
                if let Some(fi) = module_top {
                    let params = &fi.callable.params;
                    let cs = &fi.call_sig;
                    let mut ret_ty = fi.callable.ret;
                    // Inferred return (signature defaulted to Unit) from the inference pass.
                    if let Some(&inferred) = self.fun_ret_overrides.get(&fname) {
                        ret_ty = inferred;
                    }
                    // A user generic call whose return is a type parameter: bind from all arguments.
                    if user_generic.is_some() {
                        if let Some(r) = self.user_generic_return(&fname, &arg_tys) {
                            ret_ty = r;
                        }
                    }
                    if cs.vararg {
                        let fixed = params.len() - 1;
                        if arg_tys.len() < fixed {
                            self.diags.error(
                                span,
                                format!(
                                    "function '{fname}' expects at least {fixed} args, got {}",
                                    arg_tys.len()
                                ),
                            );
                        } else {
                            for i in 0..fixed {
                                self.expect_assignable(
                                    params[i],
                                    arg_tys[i],
                                    self.span(args[i]),
                                    "argument",
                                );
                            }
                            let elem = params[fixed].array_elem().unwrap_or(Ty::Error);
                            for i in fixed..arg_tys.len() {
                                self.expect_assignable(
                                    elem,
                                    arg_tys[i],
                                    self.span(args[i]),
                                    "vararg argument",
                                );
                            }
                        }
                    } else if let Some(names) = &arg_names {
                        match map_call_args(
                            args,
                            Some(names),
                            &cs.param_names,
                            cs.required,
                            &cs.param_defaults,
                        ) {
                            Ok(slots) => {
                                for (i, slot) in slots.iter().enumerate() {
                                    if let Some(a) = slot {
                                        let aty = self.expr_types[a.0 as usize];
                                        self.expect_assignable(
                                            params[i],
                                            aty,
                                            self.span(*a),
                                            "argument",
                                        );
                                    }
                                }
                            }
                            Err(msg) => self.diags.error(span, format!("call to '{fname}': {msg}")),
                        }
                    } else if arg_tys.len() < cs.required || arg_tys.len() > params.len() {
                        let want = if cs.required == params.len() {
                            format!("{}", params.len())
                        } else {
                            format!("{} to {}", cs.required, params.len())
                        };
                        self.diags.error(
                            span,
                            format!(
                                "function '{fname}' expects {want} args, got {}",
                                arg_tys.len()
                            ),
                        );
                    } else {
                        for (i, a) in arg_tys.iter().enumerate() {
                            self.expect_assignable(params[i], *a, self.span(args[i]), "argument");
                        }
                    }
                    return ret_ty;
                }
                // A receiver-less top-level library function (`listOf(…)`): resolve it through the
                // library set (vararg-aware), checking each argument against the resolved parameters.
                if !user_shadows {
                    let call_targs: Vec<Ty> = self
                        .file
                        .call_type_args
                        .get(&call.0)
                        .cloned()
                        .map(|ts| ts.iter().map(|r| self.resolve_ty(r)).collect())
                        .unwrap_or_default();
                    if let Some(c) =
                        self.syms
                            .libraries
                            .resolve_callable(&fname, None, &arg_tys, &call_targs)
                    {
                        let vararg = c.params.len() != arg_tys.len()
                            || c.params.last().map_or(false, |p| {
                                p.array_elem().is_some() && arg_tys.last() != Some(p)
                            });
                        if vararg && !c.params.is_empty() {
                            let fixed = c.params.len() - 1;
                            let elem = c.params[fixed].array_elem().unwrap_or(Ty::Error);
                            for i in 0..fixed.min(arg_tys.len()) {
                                self.expect_assignable(
                                    c.params[i],
                                    arg_tys[i],
                                    self.span(args[i]),
                                    "argument",
                                );
                            }
                            for i in fixed..arg_tys.len() {
                                self.expect_assignable(
                                    elem,
                                    arg_tys[i],
                                    self.span(args[i]),
                                    "vararg argument",
                                );
                            }
                        } else {
                            for (i, a) in arg_tys.iter().enumerate() {
                                if matches!(self.file.expr(args[i]), Expr::Lambda { .. }) {
                                    continue;
                                }
                                self.expect_assignable(
                                    c.params[i],
                                    *a,
                                    self.span(args[i]),
                                    "argument",
                                );
                            }
                        }
                        return c.ret;
                    }
                }
                self.diags
                    .error(span, format!("unresolved function '{fname}'"));
                Ty::Error
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
        // A primitive joins with `null` as its boxed (nullable) wrapper — `if (c) true else null` is a
        // `Boolean?` (`java/lang/Boolean`), the primitive branch boxed at the merge.
        if a == Ty::Null {
            if let Some(w) = nullable_prim_wrapper(b) {
                return Ty::obj(w);
            }
        }
        if b == Ty::Null {
            if let Some(w) = nullable_prim_wrapper(a) {
                return Ty::obj(w);
            }
        }
        // Two values of the SAME class join to that class with erased type arguments (`List<C>` and
        // `List<D>` → `List<*>`).
        if let (Ty::Obj(ai, _), Ty::Obj(bi, _)) = (a, b) {
            if ai == bi {
                return Ty::obj(ai);
            }
        }
        // Two values of DIFFERENT reference classes join to their common supertype, which krusty
        // approximates as `Any` (`java/lang/Object`) — the universal upper bound. The emitter writes
        // `Object` for the merge-point frame so each branch's (more specific) value verifies against it.
        // `String`/`Array`/`Fun` are references too, so this also covers `if (c) "s" else SomeObj()`.
        if a.is_reference() && b.is_reference() {
            return Ty::obj("kotlin/Any");
        }
        self.diags.error(
            span,
            format!(
                "incompatible if branches: '{}' and '{}'",
                a.name(),
                b.name()
            ),
        );
        Ty::Error
    }

    /// A compound assignment `target op= rhs` (parser-desugared to `target = target op rhs`, so `value`
    /// is `Binary { op, lhs: <target read>, rhs }`) is an in-place operator call — legal even on a `val`
    /// — when `target`'s type has a USER-defined `op`Assign operator (member, or extension). Detect that,
    /// type-check the argument, and mark the statement for the lowerer (which emits `target.opAssign(rhs)`).
    /// Returns true if handled (the caller must then skip the ordinary reassignment checks). Restricted to
    /// USER operators so a classpath `+=` (e.g. `MutableList`, whose `plusAssign` is `@InlineOnly`) keeps
    /// its existing `target = target + rhs` lowering.
    fn try_user_plus_assign(&mut self, s: StmtId, value: ExprId) -> bool {
        let Expr::Binary { op, lhs, rhs } = self.file.expr(value).clone() else {
            return false;
        };
        let Some(aname) = assign_op_name(op) else {
            return false;
        };
        let recv = self.expr(lhs);
        if recv == Ty::Error {
            return false;
        }
        // Parameter type of the user operator, if one exists (member first, then extension).
        let param = if let Ty::Obj(internal, _) = &recv {
            self.syms
                .method_of(internal, aname)
                .filter(|sig| sig.params.len() == 1)
                .map(|sig| sig.params[0])
        } else {
            None
        }
        .or_else(|| {
            self.syms
                .ext_funs
                .get(&(recv.descriptor(), aname.to_string()))
                .filter(|sig| sig.params.len() == 1)
                .map(|sig| sig.params[0])
        });
        let rt = self.expr(rhs);
        if let Some(param) = param {
            if rt != Ty::Error {
                self.expect_assignable(param, rt, self.span(rhs), "operator argument");
            }
            self.plus_assign.insert(s);
            return true;
        }
        // Otherwise resolve a `plusAssign` operator on the receiver, exactly as kotlinc does: if one is
        // applicable the lowerer splices its (inline) body (`MutableCollection.plusAssign` → `add`/
        // `addAll`). Applicability is Kotlin-type-aware (see `extension_callable`): for a `MutableList`
        // or a concrete `ArrayList` receiver `plusAssign` resolves and `+=` mutates in place; for a
        // read-only `List` it does NOT resolve, so this returns false and `coll += x` lowers as
        // `coll = coll.plus(x)` (reassignment). No mutability predicate — the candidate's Kotlin
        // receiver type decides, like every other operator overload.
        if rt != Ty::Error
            && matches!(recv, Ty::Obj(..))
            && self
                .syms
                .libraries
                .resolve_scope_inline(aname, recv, &[rt])
                .is_some()
        {
            self.plus_assign.insert(s);
            return true;
        }
        false
    }

    fn stmt(&mut self, s: StmtId) {
        match self.file.stmt(s).clone() {
            Stmt::Local {
                is_var,
                name,
                ty,
                init,
            } => {
                // Legal *nested* shadowing (`val x` inside a block, shadowing an outer `val x`) lowers
                // fine — each declaration gets a fresh slot and the lowering's scope is truncated at block
                // exit, restoring the outer mapping (verified). Only a same-scope *redeclaration* is
                // rejected (kotlinc errors on it too — conflicting declarations).
                if self.declared_in_current_scope(&name) {
                    self.diags.error(
                        self.file.stmt_spans[s.0 as usize],
                        format!("krusty: conflicting local declaration '{name}'"),
                    );
                }
                let declared = ty.as_ref().map(|r| self.resolve_ty(r));
                // A lambda initializer with a declared function type takes its parameter types from
                // the annotation, so `val f: (Int) -> Int = { it * 2 }` types `it`/`x` as `Int`
                // (not the erased `Object`). HOF *arguments* already do this.
                let it = match (
                    declared,
                    matches!(self.file.expr(init), Expr::Lambda { .. }),
                ) {
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
                    if name == "_" {
                        continue;
                    } // `_` skips this component (no binding, no call)
                    if self.declared_in_current_scope(name) {
                        self.diags.error(
                            span,
                            format!("krusty: conflicting local declaration '{name}'"),
                        );
                    }
                    let comp = format!("component{}", idx + 1);
                    // A user class's `componentN` (data class), else a library member (`Pair.component1`,
                    // `Map.Entry.component1`) — with the receiver's type arguments substituted into the
                    // result (`Pair<Int, String>.component1()` → `Int`).
                    let ty = internal
                        .and_then(|i| {
                            self.syms
                                .method_of(i, &comp)
                                .map(|sig| sig.ret)
                                .or_else(|| {
                                    crate::call_resolver::resolve_instance(
                                        &*self.syms.libraries,
                                        i,
                                        &comp,
                                        &[],
                                    )
                                    .map(|m| {
                                        self.syms
                                            .libraries
                                            .member_return(it, &comp, &[])
                                            .unwrap_or(m.ret)
                                    })
                                })
                        })
                        // `List.component1()`, … are stdlib *extensions* — try those too.
                        .or_else(|| {
                            self.syms
                                .libraries
                                .resolve_callable(&comp, Some(it), &[], &[])
                                .map(|c| c.ret)
                        })
                        // An indexable type (`List`): `componentN` is the inline `get(N-1)` — use the
                        // element type from `get(Int)` (which kotlinc inlines the component to).
                        .or_else(|| {
                            internal.and_then(|i| {
                                crate::call_resolver::resolve_instance(
                                    &*self.syms.libraries,
                                    i,
                                    "get",
                                    &[Ty::Int],
                                )
                                .map(|m| {
                                    self.syms
                                        .libraries
                                        .member_return(it, "get", &[Ty::Int])
                                        .unwrap_or(m.ret)
                                })
                            })
                        });
                    match ty {
                        Some(t) => self.declare(name, t, *is_var),
                        None => {
                            self.diags.error(
                                span,
                                format!(
                                    "krusty: cannot destructure this type (no operator '{comp}')"
                                ),
                            );
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
                let inherited = || {
                    if let Some(Ty::Obj(internal, _)) = self.this_ty.clone() {
                        self.lookup_prop(&internal, &name)
                    } else {
                        None
                    }
                };
                let found = self
                    .lookup(&name)
                    .map(|l| (l.ty, l.is_var))
                    .or_else(inherited)
                    .or_else(|| self.syms.props.get(&name).copied());
                match found {
                    Some((ty, is_var)) => {
                        if !is_var {
                            self.diags
                                .error(span, "'val' cannot be reassigned.".to_string());
                        }
                        if !ty.is_numeric() && ty != Ty::Char {
                            self.diags.error(
                                span,
                                "krusty: '++'/'--' is only supported on a numeric variable"
                                    .to_string(),
                            );
                        }
                    }
                    None => self
                        .diags
                        .error(span, format!("unresolved reference '{name}'.")),
                }
            }
            Stmt::Assign { name, value } => {
                // `name op= rhs` with a user `opAssign` operator → in-place call (legal on a `val`).
                if self.try_user_plus_assign(s, value) {
                    return;
                }
                let vt = self.expr(value);
                // `field = …` inside a setter writes the backing field.
                if name == "field" && self.lookup(&name).is_none() && self.field_ty.is_some() {
                    let fty = self.field_ty.unwrap();
                    self.expect_assignable(
                        fty,
                        vt,
                        self.file.stmt_spans[s.0 as usize],
                        "assignment",
                    );
                } else {
                    match self.lookup(&name) {
                        Some(l) => {
                            let (lty, is_var) = (l.ty, l.is_var);
                            if !is_var {
                                self.diags.error(
                                    self.file.stmt_spans[s.0 as usize],
                                    format!("'val' cannot be reassigned."),
                                );
                            }
                            self.expect_assignable(
                                lty,
                                vt,
                                self.file.stmt_spans[s.0 as usize],
                                "assignment",
                            );
                        }
                        None if self.companion_of.is_some()
                            && self.syms.props.contains_key(&name) =>
                        {
                            // A top-level property write from a companion member targets the wrong class.
                            self.diags.error(self.file.stmt_spans[s.0 as usize], "krusty: top-level property access from a companion member is not supported".to_string());
                        }
                        None => {
                            let span = self.file.stmt_spans[s.0 as usize];
                            // A bare write to an *inherited* `var` member (`x = …` where `x` is declared in a
                            // superclass): the own properties are in the implicit-`this` scope (found by
                            // `lookup` above), but inherited ones are resolved through `this`'s class chain.
                            let inherited = if let Some(Ty::Obj(internal, _)) = self.this_ty.clone()
                            {
                                self.lookup_prop(&internal, &name)
                            } else {
                                None
                            };
                            match inherited.or_else(|| self.syms.props.get(&name).copied()) {
                                Some((lty, is_var)) => {
                                    if !is_var {
                                        self.diags
                                            .error(span, format!("'val' cannot be reassigned."));
                                    }
                                    self.expect_assignable(lty, vt, span, "assignment");
                                }
                                None => {
                                    self.diags
                                        .error(span, format!("unresolved reference '{name}'."));
                                }
                            }
                        }
                    }
                }
            }
            Stmt::AssignMember {
                receiver,
                name,
                value,
            } => {
                // `recv.prop op= rhs` with a user `opAssign` operator → in-place call (legal on a `val`).
                if self.try_user_plus_assign(s, value) {
                    return;
                }
                let rt = self.expr(receiver);
                let vt = self.expr(value);
                let span = self.file.stmt_spans[s.0 as usize];
                // Extension-property write: `recv.name = value` for a `var` extension property.
                if let Some((lty, is_var)) = self
                    .syms
                    .ext_props
                    .get(&(rt.descriptor(), name.clone()))
                    .copied()
                {
                    if !is_var {
                        self.diags
                            .error(span, "'val' cannot be reassigned.".to_string());
                    }
                    self.expect_assignable(lty, vt, span, "assignment");
                } else {
                    match rt {
                        Ty::Error => {}
                        Ty::Obj(internal, _) => match self.syms.prop_of(internal, &name) {
                            Some((lty, is_var)) => {
                                if !is_var {
                                    self.diags
                                        .error(span, "'val' cannot be reassigned.".to_string());
                                }
                                self.expect_assignable(lty, vt, span, "assignment");
                            }
                            None => {
                                self.diags.error(
                                    span,
                                    format!("unresolved member '{name}' on '{}'", rt.name()),
                                );
                            }
                        },
                        _ => self.diags.error(
                            span,
                            format!("cannot assign to a member of '{}'", rt.name()),
                        ),
                    }
                }
            }
            Stmt::AssignIndex {
                array,
                index,
                value,
            } => {
                let at = self.expr(array);
                let it = self.expr(index);
                let vt = self.expr(value);
                let span = self.file.stmt_spans[s.0 as usize];
                match at.array_elem() {
                    Some(elem) => {
                        self.expect_assignable(Ty::Int, it, span, "array index");
                        self.expect_assignable(elem, vt, span, "array element assignment");
                    }
                    // `coll[i] = v` on a library type → its `set(index, value)` operator member
                    // (`MutableList.set(Int, E)`, `MutableMap.put(K, V)`).
                    None if matches!(at, Ty::Obj(internal, _)
                        if crate::call_resolver::resolve_instance(&*self.syms.libraries, internal, "set", &[it, vt]).is_some()
                            || crate::call_resolver::resolve_instance(&*self.syms.libraries, internal, "put", &[it, vt]).is_some()) =>
                        {}
                    None => {
                        if at != Ty::Error {
                            self.diags.error(
                                span,
                                format!("'{}' is not an array (cannot index-assign)", at.name()),
                            );
                        }
                    }
                }
            }
            Stmt::Break(label) | Stmt::Continue(label) => {
                // A labeled `break@l`/`continue@l` must name an enclosing loop's label (kotlinc rejects
                // an unknown label; krusty must too, else codegen would silently retarget a loop).
                if let Some(l) = label {
                    if !self.loop_labels.iter().any(|x| x.as_str() == l.as_str()) {
                        self.diags.error(
                            self.file.stmt_spans[s.0 as usize],
                            format!("krusty: unresolved loop label '{l}'"),
                        );
                    }
                }
            }
            Stmt::Return(e, label) => {
                // A labeled `return@lbl [expr]` is a *local* return from the lambda carrying `lbl`, not the
                // enclosing function — its value flows to that lambda's call, so it isn't validated against
                // the function's return type. Type-check the expression for its own errors and move on.
                if label.is_some() {
                    if let Some(ex) = e {
                        self.expr(ex);
                    }
                    return;
                }
                let rt = self.ret_ty;
                match e {
                    Some(ex) => {
                        // `return { it + 1 }` in a function returning a function type: the lambda's
                        // parameter types come from the declared return type (as for an expression body).
                        let t = match (rt, matches!(self.file.expr(ex), Expr::Lambda { .. })) {
                            (Ty::Fun(s), true) => {
                                let params = s.params.clone();
                                self.check_lambda_with_types(ex, &params)
                            }
                            _ => self.expr(ex),
                        };
                        self.expect_assignable(rt, t, self.span(ex), "return");
                    }
                    None => {
                        if rt != Ty::Unit {
                            self.diags.error(
                                self.file.stmt_spans[s.0 as usize],
                                format!("missing return value: expected {}", rt.name()),
                            );
                        }
                    }
                }
            }
            Stmt::While { cond, body, label } => {
                let ct = self.expr(cond);
                self.expect_assignable(Ty::Boolean, ct, self.span(cond), "while condition");
                if let Some(l) = &label {
                    self.loop_labels.push(l.clone());
                }
                self.expr(body);
                if label.is_some() {
                    self.loop_labels.pop();
                }
            }
            Stmt::DoWhile { body, cond, label } => {
                if let Some(l) = &label {
                    self.loop_labels.push(l.clone());
                }
                self.expr(body);
                if label.is_some() {
                    self.loop_labels.pop();
                }
                let ct = self.expr(cond);
                self.expect_assignable(Ty::Boolean, ct, self.span(cond), "do-while condition");
            }
            Stmt::For {
                name,
                range,
                body,
                label,
            } => {
                let st = self.expr(range.start);
                let et = self.expr(range.end);
                // The counter type is the (uniform) bound type — `Int`, but also `Long` and the
                // unsigned `UInt`/`ULong` (whose loop the backend emits with unsigned comparison).
                // A `Byte`/`Short` range widens to an `IntRange` (kotlinc's `Short.rangeTo(Short): IntRange`),
                // so the counter is `Int` and the bounds coerce up — exactly like a range *value*.
                let elem = if st == et
                    && matches!(st, Ty::Int | Ty::Long | Ty::UInt | Ty::ULong | Ty::Char)
                {
                    st
                } else if st == et && matches!(st, Ty::Byte | Ty::Short) {
                    Ty::Int
                } else {
                    self.expect_assignable(Ty::Int, st, self.span(range.start), "range start");
                    self.expect_assignable(Ty::Int, et, self.span(range.end), "range end");
                    Ty::Int
                };
                if let Some(step) = range.step {
                    let stp = self.expr(step);
                    // The progression `step` is always `Int` (`Long` for a `Long`/`ULong` progression) —
                    // NOT the element type, so a `Char`/`Byte`/`Short` range steps by an `Int`.
                    let step_ty = if matches!(elem, Ty::Long | Ty::ULong) {
                        Ty::Long
                    } else {
                        Ty::Int
                    };
                    self.expect_assignable(step_ty, stp, self.span(step), "range step");
                }
                self.push_scope();
                self.declare(&name, elem, true); // loop variable (mutated by the lowering)
                if let Some(l) = &label {
                    self.loop_labels.push(l.clone());
                }
                self.expr(body);
                if label.is_some() {
                    self.loop_labels.pop();
                }
                self.pop_scope();
            }
            Stmt::ForEach {
                name,
                iterable,
                body,
                label,
            } => {
                let it = self.expr(iterable);
                let elem = match it {
                    Ty::Array(_) => it.array_elem().unwrap_or(Ty::Error),
                    Ty::String => Ty::Char, // iterating a String yields its chars
                    Ty::Error => Ty::Error,
                    // A primitive range / progression iterates as its (unboxed) primitive element,
                    // matching kotlinc's specialized `IntIterator.nextInt()` loop (no boxing).
                    Ty::Obj(internal, _) if range_primitive_elem(internal).is_some() => {
                        range_primitive_elem(internal).unwrap()
                    }
                    // A collection/range value with an `iterator()` — the iterator protocol. The
                    // element is its generic argument (`List<Int>` → `Int`), erased `Any` if absent.
                    Ty::Obj(internal, args)
                        if crate::call_resolver::resolve_instance(
                            &*self.syms.libraries,
                            internal,
                            "iterator",
                            &[],
                        )
                        .is_some() =>
                    {
                        args.first()
                            .copied()
                            .unwrap_or_else(|| Ty::obj("kotlin/Any"))
                    }
                    // A value with no `iterator()` member but an `iterator` extension (`for (e in map)`
                    // uses `Map.iterator()` → `Iterator<Map.Entry<K,V>>`): the element is that iterator's
                    // type argument.
                    Ty::Obj(..) => {
                        match self
                            .syms
                            .libraries
                            .resolve_callable("iterator", Some(it), &[], &[])
                        {
                            Some(c) => c
                                .ret
                                .type_args()
                                .first()
                                .copied()
                                .unwrap_or_else(|| Ty::obj("kotlin/Any")),
                            None => {
                                self.diags.error(self.span(iterable), format!("krusty: 'for' over '{}' is not supported (only arrays, String, and Iterables)", it.name()));
                                Ty::Error
                            }
                        }
                    }
                    _ => {
                        self.diags.error(self.span(iterable), format!("krusty: 'for' over '{}' is not supported (only arrays, String, and Iterables)", it.name()));
                        Ty::Error
                    }
                };
                self.push_scope();
                self.declare(&name, elem, false);
                if let Some(l) = &label {
                    self.loop_labels.push(l.clone());
                }
                self.expr(body);
                if label.is_some() {
                    self.loop_labels.pop();
                }
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
            self.diags.error(
                span,
                "krusty: generic local functions are not supported".to_string(),
            );
            return;
        }
        // Collect outer local names (everything currently in scope that isn't one of f's params).
        let own_params: std::collections::HashSet<String> =
            f.params.iter().map(|p| p.name.clone()).collect();
        let outer_names: std::collections::HashSet<String> = self
            .scopes
            .iter()
            .flat_map(|s| s.keys())
            .filter(|n| !own_params.contains(*n))
            .cloned()
            .collect();

        // Captured outer locals: lifted to extra leading parameters (recorded as ordered `(name,
        // type)`); a captured var the body *writes* is also boxed (`Ref$XxxRef`) so the lift shares the
        // cell. Determined by walking the body (stops at nested local funs, which capture separately).
        if !outer_names.is_empty() {
            if let FunBody::Expr(e) | FunBody::Block(e) = &f.body {
                let mut captured: Vec<(String, Ty)> = Vec::new();
                for n in &outer_names {
                    let single: std::collections::HashSet<String> =
                        std::iter::once(n.clone()).collect();
                    if local_fun_body_uses_any(self.file, *e, &single) {
                        let ty = self.lookup(n).map(|l| l.ty).unwrap_or(Ty::Error);
                        captured.push((n.clone(), ty));
                    }
                }
                captured.sort_by(|a, b| a.0.cmp(&b.0));
                // Box the captures that need a shared cell (a `var` written here, or a `var` reassigned
                // elsewhere in the function) — `record_captured_vars` adds them to `boxed_vars`; the
                // lift passes each captured holder/value accordingly. A captured `val` (or a never-
                // reassigned `var`) is passed by value.
                self.record_captured_vars(*e, &outer_names);
                if !captured.is_empty() {
                    self.local_fun_captures.insert(stmt_id, captured);
                }
            }
        }

        // Add the local function's own type parameters (erased to Object, same as top-level funs).
        let added_tparams: Vec<String> = f
            .type_params
            .iter()
            .filter(|t| self.tparams.insert((*t).clone()))
            .cloned()
            .collect();

        // Resolve parameter types.
        let params: Vec<Ty> = f
            .params
            .iter()
            .map(|p| {
                let t = self.resolve_ty(&p.ty);
                if p.is_vararg {
                    Ty::array(t)
                } else {
                    t
                }
            })
            .collect();

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
            param_defaults: f.params.iter().map(|p| p.default.is_some()).collect(),
            param_names: f.params.iter().map(|p| p.name.clone()).collect(),
            lambda_param_types: Vec::new(),
            is_inline: false,
            is_final: false,
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
        assert!(
            errs.iter().any(|e| e.contains(needle)),
            "expected error containing {needle:?}, got {errs:?}"
        );
    }

    // NOTE: `require`/`check`/`error`/`TODO`/`assertEquals`/`assertTrue`/`assertFalse` are no longer
    // hardcoded in the checker — they resolve generically from the classpath (a real stdlib / kotlin.test
    // jar) and are validated by the box-conformance + `feature_box_e2e` suites, not here (these unit
    // tests use `EmptyLibrarySet`, so a classpath-resolved call can't be typed).

    #[test]
    fn rejects_latent_miscompiles() {
        // Same-scope redeclaration is rejected (kotlinc errors too); legal nested-scope shadowing
        // (`var x` inside a block) is accepted — each declaration gets its own slot.
        err_contains(
            "fun box(): String { var x = 1; var x = 2; return \"OK\" }",
            "conflicting local declaration",
        );
        ok("fun box(): String { var x = 1; if (1>0) { var x = 2; x.toString() }; return \"OK\" }");
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
        err_contains(
            "fun f(a: Int): Int = a\nfun g(): Int = f(z = 1)",
            "no parameter named 'z'",
        );
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
        err_contains(
            "fun f(a: Int, b: String): Int = if (a > 0) a else b",
            "incompatible if branches",
        );
    }

    #[test]
    fn return_type_mismatch() {
        err_contains(
            "fun f(a: Int): String = a",
            "return type mismatch: expected 'String', actual 'Int'.",
        );
    }

    #[test]
    fn unresolved_reference() {
        err_contains("fun f(): Int = q", "unresolved reference 'q'.");
    }

    #[test]
    fn val_reassign_is_error() {
        err_contains(
            "fun f(): Int {\n val x = 1\n x = 2\n return x\n}",
            "cannot be reassigned",
        );
    }

    #[test]
    fn var_reassign_ok() {
        ok("fun f(): Int {\n var x = 1\n x = 2\n return x\n}");
    }

    #[test]
    fn call_arity_and_types() {
        ok("fun a(x: Int): Int = x\nfun b(): Int = a(1)");
        err_contains(
            "fun a(x: Int): Int = x\nfun b(): Int = a()",
            "expects 1 args",
        );
        err_contains(
            "fun a(x: Int): Int = x\nfun b(): Int = a(\"s\")",
            "type mismatch: inferred type is String but Int was expected",
        );
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
        err_contains(
            "fun f(s: String): String = s.substring(\"x\")",
            "unresolved method",
        );
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
        err_contains(
            "class Point(val x: Int)\nfun f(p: Point): Int = p.z",
            "unresolved member 'z'",
        );
        err_contains(
            "class Point(val x: Int)\nfun f(): Point = Point()",
            "expects 1 args",
        );
        err_contains(
            "fun f(p: Widget): Int = 0",
            "unresolved reference 'Widget'.",
        );
    }

    #[test]
    fn string_method_table() {
        assert_eq!(
            resolve_string_instance("substring", &[Ty::Int]),
            Some(Ty::String)
        );
        assert_eq!(
            resolve_string_instance("indexOf", &[Ty::String]),
            Some(Ty::Int)
        );
        assert_eq!(resolve_string_instance("substring", &[Ty::String]), None);
    }
}
