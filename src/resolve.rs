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
use crate::libraries::{CompilerPlatform, EmptySymbolSource};
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
    /// For each parameter: `true` when it is a RECEIVER function type `Recv.(A) -> R` (vs a plain
    /// `(Recv, A) -> R`). A lambda passed to such a param binds `lambda_param_types[i][0]` as the
    /// implicit `this` receiver (so a bare member/extension call inside resolves against it). Parallel
    /// to `params`; empty = none are receivers.
    pub lambda_recv: Vec<bool>,
    /// True for an `inline fun` — the lowerer expands its body at each call site (so a lambda
    /// argument may capture a mutable local), instead of forming a closure.
    pub is_inline: bool,
    /// True for a `final` member (a non-`open` member, or an explicit `final override`). A subclass —
    /// including a `data class` synthesizing `equals`/`hashCode`/`toString` — cannot override it.
    pub is_final: bool,
    /// True for a `suspend fun`. Flows to `FnFlags.suspend` so the resolver reports suspend-ness
    /// uniformly for same-file and classpath callees; the coroutine pass keys off it.
    pub is_suspend: bool,
}

/// A primary-constructor parameter's default value, captured in a FILE-INDEPENDENT form — NOT an
/// `ExprId` (which indexes only the defining file's `expr_arena`, so a *different* file filling the
/// default — a subclass/companion in another file, or a multi-file `// WITH_COROUTINES` test — would
/// dereference it against the wrong arena and panic). Only the shapes the default-fill lowering can emit
/// directly are represented; any other default is `None` (the parameter is treated as required, so the
/// call/file skips — never a miscompile).
#[derive(Clone, Debug, PartialEq)]
pub enum CtorDefaultValue {
    Int(i64),
    Long(i64),
    Double(f64),
    Float(f32),
    Bool(bool),
    Char(char),
    Str(String),
    Null,
    /// An `object` singleton default (`= EmptyCoroutineContext`) — read as `getstatic <internal>.INSTANCE`.
    Object(String),
}

/// Capture a primary-constructor parameter default in the file-independent [`CtorDefaultValue`] form, or
/// `None` for an unmodeled default (a non-literal/non-object — kept conservative so behavior matches the
/// previous literal-only handling). Resolves an object-singleton `Name` (`= EmptyCoroutineContext`) to its
/// internal via the type universe, so the default lowers cross-file as `getstatic …INSTANCE`.
fn extract_ctor_default(
    file: &File,
    dx: ExprId,
    class_names: &ClassNames,
    libraries: &dyn SymbolSource,
) -> Option<CtorDefaultValue> {
    Some(match file.expr(dx) {
        Expr::IntLit(v) => CtorDefaultValue::Int(*v),
        Expr::LongLit(v) => CtorDefaultValue::Long(*v),
        Expr::DoubleLit(v) => CtorDefaultValue::Double(*v),
        Expr::FloatLit(v) => CtorDefaultValue::Float(*v),
        Expr::BoolLit(v) => CtorDefaultValue::Bool(*v),
        Expr::CharLit(v) => CtorDefaultValue::Char(*v),
        Expr::StringLit(s) => CtorDefaultValue::Str(s.clone()),
        Expr::NullLit => CtorDefaultValue::Null,
        Expr::Name(n) => {
            let internal = class_names.get(n)?;
            if libraries
                .resolve_type(internal)
                .is_some_and(|t| t.is_object())
            {
                CtorDefaultValue::Object(internal.clone())
            } else {
                return None;
            }
        }
        _ => return None,
    })
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
    /// True if declared `abstract` (or `sealed`, which is abstract) — cannot be instantiated directly.
    pub is_abstract: bool,
    /// True if declared `fun interface` — a single-abstract-method interface eligible for SAM
    /// conversion (a lambda may be passed where this type is expected).
    pub is_fun_interface: bool,
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
    /// Each primary-constructor parameter's default, captured FILE-INDEPENDENTLY (see
    /// [`CtorDefaultValue`]) so a different file can fill it without dereferencing a cross-file `ExprId`.
    /// `None` = required (or an unmodeled default → treated as required → skip).
    pub ctor_defaults: Vec<Option<CtorDefaultValue>>,
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
    /// For a generic higher-order method (`fun <R> map(f: (T) -> R): R`), the un-erased declared shape
    /// needed to substitute the receiver's type arguments into the lambda parameter types and infer the
    /// method's own type parameters from the lambda body. Keyed by method name; only methods whose
    /// substitution actually depends on a type parameter are recorded (ordinary methods are absent).
    pub generic_methods: HashMap<String, GenericMethod>,
}

/// The un-erased declared shape of a generic higher-order method, retained so a call site can
/// substitute the receiver's type arguments and infer the method's own type parameters (mirrors the
/// `GSig` unify/substitute machinery, but built from the source `TypeRef`s of a user-declared method).
/// `TypeRef` is self-contained (owned, not arena-indexed), so cloning it here stays file-independent.
#[derive(Clone, Debug)]
pub struct GenericMethod {
    /// The method's own type parameters (`fun <R>` → `["R"]`), inferred from the argument types.
    pub method_tparams: Vec<String>,
    /// Declared parameter type refs, in order (un-erased).
    pub param_refs: Vec<TypeRef>,
    /// Declared return type ref (un-erased), substituted under the bound type parameters.
    pub ret_ref: TypeRef,
}

/// A generic higher-order member call's substitution plan: the recorded [`GenericMethod`] shape, the
/// class type-parameter → receiver type-argument bindings (`{T: String}`), and — per logical argument —
/// the lambda parameter types with that substitution applied (`[(T) -> R]` → `[[String]]`).
type GenericMemberPlan = (GenericMethod, HashMap<String, Ty>, Vec<Vec<Ty>>);

impl ClassSig {
    pub fn prop(&self, name: &str) -> Option<(Ty, bool)> {
        self.props
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|(_, t, v)| (*t, *v))
    }
}

/// Simple type name → JVM internal name, split into a SHARED read-only base (the library/classpath
/// type universe — tens of thousands of stdlib+JDK names, identical for every file on a classpath) and
/// a small per-file `user` overlay (the file's own classes + type aliases). Lookups check `user` first
/// (a user class shadows a classpath type of the same name), then the shared `base`. This avoids
/// cloning the whole base map per compilation — the dominant `collect_signatures` cost before — by
/// sharing it via `Rc`.
#[derive(Clone, Default)]
pub struct ClassNames {
    base: std::rc::Rc<HashMap<String, String>>,
    user: HashMap<String, String>,
}

impl ClassNames {
    pub fn new(base: std::rc::Rc<HashMap<String, String>>) -> ClassNames {
        ClassNames {
            base,
            user: HashMap::new(),
        }
    }
    pub fn get(&self, k: &str) -> Option<&String> {
        self.user.get(k).or_else(|| self.base.get(k))
    }
    pub fn contains_key(&self, k: &str) -> bool {
        self.user.contains_key(k) || self.base.contains_key(k)
    }
    pub fn insert(&mut self, k: String, v: String) -> Option<String> {
        self.user.insert(k, v)
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
    /// Top-level properties (name → type, is_var, is_const), backed by static fields on the file facade.
    /// `is_const` distinguishes a `const val` (public field, no accessor, cross-file `getstatic`) from a
    /// plain `val`/`var` (private field, read/written through `getX`/`setX`).
    pub props: HashMap<String, (Ty, bool, bool)>,
    /// Top-level *computed* properties (`val g: T get() = …`): a `getG()` static method, no field.
    pub computed_props: std::collections::HashSet<String>,
    /// Simple names declared as `object` singletons (accessed via `Name.member`).
    pub objects: std::collections::HashSet<String>,
    /// Declared `enum` types (simple name → entry names), accessed via `Name.ENTRY`.
    pub enums: HashMap<String, Vec<String>>,
    /// The target's compiled library set — a JVM classpath or a klib (empty unless the driver
    /// supplies one). The front end resolves external references only through this abstraction.
    pub libraries: Box<dyn CompilerPlatform>,
    /// Top-level extension functions: (erased receiver, method_name) → Signature. The receiver is its
    /// [`Ty::erased_recv`] key (nullability/generics/type-params folded). Used to resolve
    /// `recv.method(args)` when no instance method matches.
    pub ext_funs: HashMap<(Ty, String), Signature>,
    /// Top-level extension properties: (erased receiver, prop_name) → (type, is_var). The
    /// getter/setter are emitted as static `getName(Recv)`/`setName(Recv, T)` methods.
    pub ext_props: HashMap<(Ty, String), (Ty, bool)>,
    /// Simple type name → JVM internal name: every resolvable reference type — user/classpath
    /// classes, classpath `TypeAliasesKt` aliases, and the ported `JavaToKotlinClassMap`
    /// built-ins. The single source of truth for "does this type name resolve, and to what".
    pub class_names: ClassNames,
    /// Internal-name canonical aliases for subtype identity checks. This is seeded by the library
    /// source, so the checker does not call back into a backend map while comparing types.
    pub canonical_names: std::rc::Rc<HashMap<String, String>>,
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
    pub prop_facades: HashMap<String, (String, Ty, bool, bool)>,
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
            libraries: Box::new(EmptySymbolSource),
            ext_funs: HashMap::new(),
            ext_props: HashMap::new(),
            class_names: ClassNames::default(),
            canonical_names: std::rc::Rc::new(HashMap::new()),
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

    pub fn class_by_internal_mut(&mut self, internal: &str) -> Option<&mut ClassSig> {
        self.classes.values_mut().find(|c| c.internal == internal)
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

    /// Whether `internal`'s method `name` (or one inherited up the base chain) is `vararg` — a
    /// clone-free probe for the hot call paths, which only need the flag (`method_of` clones the whole
    /// `Signature`, an allocation per call when used merely to read one bool).
    pub fn method_is_vararg(&self, internal: &str, name: &str) -> bool {
        let Some(c) = self.class_by_internal(internal) else {
            return false;
        };
        if let Some(sig) = c.methods.get(name) {
            return sig.vararg;
        }
        c.super_internal
            .as_deref()
            .is_some_and(|s| self.method_is_vararg(s, name))
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ErasedTypeKey {
    Ty(Ty),
    Function(usize),
    Unresolved(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ErasedSigKey {
    name: String,
    receiver: Option<ErasedTypeKey>,
    params: Vec<ErasedTypeKey>,
}

fn erased_key_ty(key: ErasedTypeKey) -> Ty {
    match key {
        ErasedTypeKey::Ty(t) => t,
        ErasedTypeKey::Function(n) => Ty::obj(&format!("kotlin/Function{n}")),
        ErasedTypeKey::Unresolved(n) => Ty::obj(&n),
    }
}

/// Kotlin-level erasure key used by the checker for overload identity. This mirrors the JVM-relevant
/// distinctions without formatting descriptors: generic arguments and nullability are erased, unsigned
/// inline primitives fold to their signed representation, nullable primitives box, and type parameters
/// erase to their bound.
fn erased_type_key(t: Ty) -> ErasedTypeKey {
    let key = match t {
        Ty::UInt => Ty::Int,
        Ty::ULong => Ty::Long,
        Ty::String => Ty::obj("kotlin/String"),
        Ty::Obj(n, _) => Ty::Obj(n, &[]),
        Ty::Null | Ty::Nothing | Ty::Error => Ty::obj("kotlin/Any"),
        Ty::Array(elem) => Ty::array(erased_key_ty(erased_type_key(*elem))),
        Ty::Fun(s) => return ErasedTypeKey::Function(s.params.len() + usize::from(s.suspend)),
        Ty::Nullable(inner) => match *inner {
            Ty::UInt => Ty::obj("kotlin/UInt"),
            Ty::ULong => Ty::obj("kotlin/ULong"),
            other if other.boxed_ref().is_some() => other.boxed_ref().unwrap_or(other),
            other => erased_key_ty(erased_type_key(other)),
        },
        Ty::TyParam(_, bound) => erased_key_ty(erased_type_key(*bound)),
        other => other,
    };
    ErasedTypeKey::Ty(key)
}

fn erased_params_semantic_key(sig: &Signature) -> Vec<ErasedTypeKey> {
    sig.params.iter().copied().map(erased_type_key).collect()
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

/// The return type of a builtin bitwise/shift operator method on an `Int`/`Long` receiver — the named
/// forms of Kotlin's primitive operators (`a shl b`, `a and b`, `a.inv()`), which compile to JVM
/// `ishl`/`iand`/… intrinsics, NOT classpath method calls (so they don't appear in the federated
/// `functions()` index). `inv` is unary; `shl`/`shr`/`ushr`/`and`/`or`/`xor` take one argument. The
/// single source of truth shared by the checker and the signature-inference pre-pass.
pub fn builtin_bitwise_ret(recv: Ty, name: &str, n_args: usize) -> Option<Ty> {
    if !matches!(recv, Ty::Int | Ty::Long) {
        return None;
    }
    match (name, n_args) {
        ("inv", 0) => Some(recv),
        ("shl" | "shr" | "ushr" | "and" | "or" | "xor", 1) => Some(recv),
        _ => None,
    }
}

/// Map a file's EXPLICIT imports `simple name -> internal name` (e.g. `Calc -> util/Calc`). A
/// wildcard import (`a.b.*`) has no simple name — those go to [`import_wildcards`].
pub fn import_map(file: &File) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for fq in &file.imports {
        if fq.ends_with(".*") {
            continue;
        }
        if let Some(simple) = fq.rsplit('.').next() {
            m.insert(simple.to_string(), fq.replace('.', "/"));
        }
    }
    m
}

/// Collect every simple type NAME referenced in a `TypeRef` (recursively through generic arguments,
/// function-type params/return, and the array/return `arg`) into `out`.
fn collect_typeref_names(r: &TypeRef, out: &mut std::collections::HashSet<String>) {
    if !r.name.is_empty() && r.name != "<fun>" {
        out.insert(r.name.clone());
    }
    if let Some(a) = &r.arg {
        collect_typeref_names(a, out);
    }
    for t in &r.targs {
        collect_typeref_names(t, out);
    }
    for p in &r.fun_params {
        collect_typeref_names(p, out);
    }
}

/// Collect every simple type NAME referenced in TYPE positions across a file's declarations (function
/// params/returns/receivers + type-param bounds, property types, class members) — so the signature phase
/// can import-resolve names absent from the global seed (ambiguity-pruned), matching the Checker.
fn collect_file_type_names(file: &File, out: &mut std::collections::HashSet<String>) {
    fn fun_names(f: &FunDecl, file: &File, out: &mut std::collections::HashSet<String>) {
        let _ = file;
        if let Some(r) = &f.receiver {
            collect_typeref_names(r, out);
        }
        for p in &f.params {
            collect_typeref_names(&p.ty, out);
        }
        if let Some(r) = &f.ret {
            collect_typeref_names(r, out);
        }
        for (_, b) in &f.type_param_bounds {
            collect_typeref_names(b, out);
        }
    }
    fn prop_names(p: &PropDecl, out: &mut std::collections::HashSet<String>) {
        if let Some(r) = &p.receiver {
            collect_typeref_names(r, out);
        }
        if let Some(r) = &p.ty {
            collect_typeref_names(r, out);
        }
    }
    // Every bare VALUE reference (`val x = EmptyCoroutineContext` — an object singleton/top-level fun
    // used as a value) is a candidate too: a wildcard/explicit import resolves it no differently from a
    // type. Collecting all `Expr::Name`s over-approximates (locals, params), but a name that matches no
    // import package simply isn't added — harmless. This is what lets a default-import-only seed (no
    // whole-classpath blanket) still resolve imported values.
    for e in &file.expr_arena {
        if let Expr::Name(n) = e {
            out.insert(n.clone());
        }
    }
    for &d in &file.decls {
        match file.decl(d) {
            Decl::Fun(f) => {
                for a in &f.annotations {
                    out.insert(a.clone());
                }
                fun_names(f, file, out)
            }
            Decl::Property(p) => prop_names(p, out),
            Decl::Class(c) => {
                for a in c
                    .annotations
                    .iter()
                    .chain(c.methods.iter().flat_map(|m| &m.annotations))
                {
                    out.insert(a.clone());
                }
                // Supertypes/base/delegated-interface names (`class Done : Continuation<Unit>`) must be
                // candidates too — they are names a wildcard/explicit import resolves, no different from a
                // parameter type. Each interface supertype carries its type args, collected recursively.
                for s in &c.supertypes {
                    collect_typeref_names(s, out);
                }
                if let Some(b) = &c.base_class {
                    out.insert(b.clone());
                }
                for (iface, _, _) in &c.delegations {
                    out.insert(iface.clone());
                }
                for (iface, _) in &c.delegation_exprs {
                    out.insert(iface.clone());
                }
                for (_, b) in &c.type_param_bounds {
                    collect_typeref_names(b, out);
                }
                for pp in &c.props {
                    collect_typeref_names(&pp.ty, out);
                }
                for p in c.body_props.iter().chain(&c.companion_props) {
                    prop_names(p, out);
                }
                for m in c.methods.iter().chain(&c.companion_methods) {
                    fun_names(m, file, out);
                }
            }
        }
    }
}

/// Kotlin's common default imports, in source package syntax. Target-specific additions are supplied by
/// the platform symbol source and composed by [`import_wildcards`].
pub const KOTLIN_DEFAULT_IMPORT_PACKAGES: &[&str] = &[
    "kotlin",
    "kotlin.annotation",
    "kotlin.collections",
    "kotlin.comparisons",
    "kotlin.io",
    "kotlin.ranges",
    "kotlin.sequences",
    "kotlin.text",
];

/// A file's wildcard-import packages as internal names (`import kotlin.coroutines.*` →
/// `"kotlin/coroutines"`), PLUS Kotlin's common default-import packages and the target's documented
/// default additions — so a bare type name resolves through the generic import machinery instead of a
/// global every-class simple-name index (which falsely collides `Continuation` with JDK internals).
pub fn import_wildcards(file: &File, platform_defaults: &[&str]) -> Vec<String> {
    // The file's OWN package is an implicit wildcard: Kotlin makes same-package declarations (including
    // ones compiled separately and read from the classpath) visible without an import. The root package
    // contributes `""`, so a bare name resolves to a top-level classpath class (`RoleId` → `RoleId`).
    let own_package = std::iter::once(match &file.package {
        Some(pkg) => pkg.replace('.', "/"),
        None => String::new(),
    });
    file.imports
        .iter()
        .filter_map(|fq| fq.strip_suffix(".*").map(|p| p.replace('.', "/")))
        .chain(
            KOTLIN_DEFAULT_IMPORT_PACKAGES
                .iter()
                .chain(platform_defaults.iter())
                .map(|s| s.replace('.', "/")),
        )
        .chain(own_package)
        .collect()
}

/// A classpath type candidate from a wildcard package and a simple name. The root package (`pkg == ""`)
/// yields the bare name; a named package yields `pkg/name`.
fn wildcard_candidate(pkg: &str, name: &str) -> String {
    if pkg.is_empty() {
        name.to_string()
    } else {
        format!("{pkg}/{name}")
    }
}

/// Resolve a DOTTED type name written in type position to a classpath internal name, so the SIGNATURE
/// phase (and, via the shared `class_names` map, the checker) accepts it. Two complementary forms:
///   * A fully-qualified package path — `lib.Thing` → `lib/Thing`, `a.b.C` → `a/b/C` — verified via
///     `resolve_type` (so a bogus path stays unresolved rather than becoming a phantom `Obj`).
///   * A nested type under a resolvable outer prefix — `Wrap.Box` → `<pkg>/Wrap$Box`. The longest
///     leading prefix that names a known/importable type is the outer; the rest is the nested path
///     joined with `$` (kotlinc's nesting separator).
///
/// `None` (never a guess) when neither form resolves on the classpath.
/// Resolve `internal` (a dotted name flattened to slashes, e.g. `lib/Outer/Ws`) to the internal that
/// actually EXISTS, treating trailing path segments as NESTED classes (`lib/Outer$Ws`) — convert `/` → `$`
/// from the RIGHT until `resolve_type` finds it. Returns the input unchanged when it already resolves. The
/// signature-phase free-function twin of the checker's `Checker::nested_internal`.
/// Signature-phase twin of the checker's `Checker::object_member_import`: if `name` is imported from a
/// classpath `object` (`import a.b.Obj.name`), the object's internal name — so the light initializer
/// inference can type an unqualified object-member call (`val logger = logger {}`). Recovers a nested
/// owner (`a/b/Outer/Obj` → `a/b/Outer$Obj`) via `resolve_type`, since only [`SymbolSource`] is available
/// here (not the richer `CompilerPlatform` `resolve_nested_internal` needs).
fn object_member_import_sig(file: &File, name: &str, src: &dyn SymbolSource) -> Option<String> {
    let suffix = format!(".{name}");
    let fq = file
        .imports
        .iter()
        .find(|i| !i.ends_with(".*") && i.ends_with(&suffix))?;
    let owner_path = fq[..fq.len() - suffix.len()].replace('.', "/");
    let mut cand = owner_path;
    loop {
        if src.resolve_type(&cand).is_some_and(|t| t.is_object()) {
            return Some(cand);
        }
        match cand.rfind('/') {
            Some(pos) => cand.replace_range(pos..=pos, "$"),
            None => return None,
        }
    }
}

fn resolve_nested_internal(internal: &str, libraries: &dyn CompilerPlatform) -> Option<String> {
    if libraries.resolve_type(internal).is_some() {
        return Some(internal.to_string());
    }
    let mut cand = internal.to_string();
    while let Some(pos) = cand.rfind('/') {
        cand.replace_range(pos..=pos, "$");
        if libraries.resolve_type(&cand).is_some() {
            return Some(cand);
        }
    }
    None
}

fn resolve_dotted_classpath_type(
    name: &str,
    class_names: &ClassNames,
    imap: &HashMap<String, String>,
    wilds: &[String],
    libraries: &dyn CompilerPlatform,
) -> Option<String> {
    if !name.contains('.') {
        return None;
    }
    // (a) Nested type under a resolvable outer prefix FIRST — an in-scope type name shadows a package
    // path (kotlinc resolves the type in scope before treating the qualifier as a package).
    let segs: Vec<&str> = name.split('.').collect();
    for k in 1..segs.len() {
        let outer = segs[..k].join(".");
        let base = class_names
            .get(&outer)
            .filter(|i| !i.starts_with("__ty/"))
            .cloned()
            .or_else(|| {
                imap.get(&outer)
                    .filter(|f| libraries.resolve_type(f).is_some())
                    .cloned()
            })
            .or_else(|| {
                wilds
                    .iter()
                    .map(|p| wildcard_candidate(p, &outer))
                    .find(|c| libraries.resolve_type(c).is_some())
            });
        if let Some(base) = base {
            let cand = format!("{base}${}", segs[k..].join("$"));
            if libraries.resolve_type(&cand).is_some() {
                crate::trace_compiler!("resolve", "dotted nested type {name} -> {cand}");
                return Some(cand);
            }
        }
    }
    // (b) Fully-qualified package path (`lib.Thing` → `lib/Thing`), or a deep FQN whose TAIL names a
    // NESTED type (`a.b.Outer.Inner` → `a/b/Outer$Inner`) when the outer prefix isn't itself imported/in
    // scope (so branch (a) couldn't seed it) — `resolve_nested_internal` tries the flat form then the
    // `/` → `$` variants.
    let fq = name.replace('.', "/");
    let resolved = resolve_nested_internal(&fq, libraries);
    if let Some(r) = &resolved {
        crate::trace_compiler!("resolve", "dotted FQ type {name} -> {r}");
    }
    resolved
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
    collect_signatures_with_cp(files, Box::new(EmptySymbolSource), diags)
}

/// Like `collect_signatures` but also seeds class names and type aliases from the target's
/// libraries (a JVM classpath, a klib), eliminating the need for any hardcoded type lists.
pub fn collect_signatures_with_cp(
    files: &[File],
    libraries: Box<dyn CompilerPlatform>,
    diags: &mut DiagSink,
) -> SymbolTable {
    let platform_default_imports = libraries.platform_default_import_packages();
    // The library set's type universe: importable names + type aliases (and intrinsic built-in maps).
    // The (large) class-name base is shared by `Rc` — NOT cloned per compilation; only the small
    // per-file overlay (user classes + aliases) is owned here.
    let (base_class_names, base_aliases, canonical_names) = libraries.seed_shared();

    // Pass 1: every class simple-name -> internal name (no bodies, just the type universe).
    // Pre-seed from the library type index so imports/stdlib types are visible.
    let mut class_names = ClassNames::new(base_class_names);
    // A user-declared top-level class *shadows* any classpath/JDK type of the same simple name
    // (legal Kotlin — the JDK one would need an explicit import). Only a duplicate among the
    // user's own declarations is a conflict, so track which names the user has defined.
    let mut user_defined: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (i, file) in files.iter().enumerate() {
        diags.set_file(i as u32);
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
    let mut alias_map: HashMap<String, String> = (*base_aliases).clone();
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

    // Explicit imports disambiguate a simple name that classpath ambiguity PRUNED from the global seed
    // (`Encoder` collides with `java.beans.Encoder` once the JDK is on the classpath). For a name ABSENT
    // from the seed (and not user-defined), resolve it to its imported full internal — verified to exist
    // on the classpath — so the SIGNATURE phase's `ty_of_ref` matches the Checker's import-aware
    // resolution. This only ADDS entries for otherwise-unresolved names (never overrides a resolving
    // one), so it cannot regress an accepted file. A name imported INCONSISTENTLY across files (different
    // full internals) is left pruned (ambiguous) rather than guessed.
    {
        let mut from_import: HashMap<String, Option<String>> = HashMap::new();
        for file in files {
            let imap = import_map(file);
            let wilds = import_wildcards(file, platform_default_imports);
            // Candidate simple names: every type referenced in the file (so a WILDCARD import can supply
            // it) plus the explicit-import names themselves.
            let mut names = std::collections::HashSet::new();
            collect_file_type_names(file, &mut names);
            names.extend(imap.keys().cloned());
            for name in names {
                if class_names.contains_key(name.as_str()) || user_defined.contains(&name) {
                    continue;
                }
                // An explicit import wins; else a wildcard package that actually provides the type. The
                // explicit import is resolved through `resolve_nested_internal` so a NESTED-type import
                // (`import lib.Outer.Ws` → flat `lib/Outer/Ws`) registers the real `lib/Outer$Ws` — needed
                // for a TYPE-position use (`fun f(x: Ws)`), which the signature phase resolves via this map.
                let full = imap
                    .get(&name)
                    .and_then(|f| resolve_nested_internal(f, &*libraries))
                    .or_else(|| {
                        wilds
                            .iter()
                            .map(|p| wildcard_candidate(p, &name))
                            .find(|cand| libraries.resolve_type(cand).is_some())
                    })
                    .or_else(|| {
                        // A dotted type name (`lib.Thing`, `Wrap.Box`) — resolve the FQ package path
                        // or a nested type under a resolvable outer prefix.
                        resolve_dotted_classpath_type(
                            &name,
                            &class_names,
                            &imap,
                            &wilds,
                            &*libraries,
                        )
                    });
                if let Some(full) = full {
                    match from_import.get(&name) {
                        None => {
                            from_import.insert(name, Some(full));
                        }
                        Some(Some(prev)) if *prev != full => {
                            from_import.insert(name, None); // conflicting resolutions → leave unresolved
                        }
                        _ => {}
                    }
                }
            }
        }
        for (simple, full) in from_import {
            if let Some(full) = full {
                class_names.insert(simple, full);
            }
        }
    }

    let type_ref_ctx = TypeRefCtx {
        class_literal_ty: libraries.class_literal_type(),
    };
    let ty_of_ref = |r: &TypeRef, classes: &ClassNames, tparams: &TParams, diags: &mut DiagSink| {
        ty_of_ref_with(r, classes, tparams, &type_ref_ctx, diags)
    };

    // Top-level function return types (explicit annotations only), collected first so a property
    // initializer `val v = f()` can infer its type from `f`'s return type regardless of decl order.
    let mut fun_rets: HashMap<String, Ty> = HashMap::new();
    for (i, file) in files.iter().enumerate() {
        diags.set_file(i as u32);
        for &d in &file.decls {
            if let Decl::Fun(f) = file.decl(d) {
                if f.receiver.is_none() {
                    if let Some(r) = &f.ret {
                        let tp =
                            TParams::from_decl_with(&f.type_params, &f.type_param_bounds, &|n| {
                                class_names.get(n).cloned()
                            });
                        fun_rets.insert(f.name.clone(), ty_of_ref(r, &class_names, &tp, diags));
                    }
                }
            }
        }
    }

    // Pass 2: resolve signatures/properties against the now-complete type universe.
    let mut table = SymbolTable::default();
    for (i, file) in files.iter().enumerate() {
        diags.set_file(i as u32);
        for &d in &file.decls {
            match file.decl(d) {
                Decl::Fun(f) => {
                    let tp = TParams::from_decl_with(&f.type_params, &f.type_param_bounds, &|n| {
                        class_names.get(n).cloned()
                    });
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
                            // to Unit; check_fun will do a deeper inference pass and patch the
                            // canonical signature table before lowering.
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
                                let t = infer_lit_ty_p(
                                    file,
                                    *e,
                                    &class_names,
                                    &fun_rets,
                                    &this_scope,
                                    &*libraries,
                                );
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
                    // A default that reads another parameter (`c: Int = a + 1`) is realized inside the
                    // single `foo$default` synthetic (where the parameters are in scope), so it is allowed.
                    // EXCEPT for an OVERLOADED function: its overloads share the name `foo$default` and the
                    // omitted-default routing isn't overload-aware, so a param-referencing default on an
                    // overloaded function is still rejected (skip, never miscompile).
                    // Match the IR-side gate (`ir::toplevel_default_stub_safe`), which counts every
                    // non-member function of this name (a top-level fn AND an extension are both emitted as
                    // facade statics), so the checker and emitter agree on which functions are "overloaded".
                    let overloaded = file
                        .decls
                        .iter()
                        .filter(|&&d| matches!(file.decl(d), Decl::Fun(g) if g.name == f.name))
                        .count()
                        > 1;
                    if overloaded {
                        let pnames: std::collections::HashSet<&str> =
                            f.params.iter().map(|p| p.name.as_str()).collect();
                        for p in &f.params {
                            if let Some(dx) = p.default {
                                if expr_refs_param(file, dx, &pnames) {
                                    diags.error(f.span, "krusty: a default argument that references another parameter is not supported on an overloaded function");
                                }
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
                        lambda_recv: f.params.iter().map(|p| p.ty.fun_has_receiver).collect(),
                        is_inline: f.is_inline,
                        is_final: f.is_final,
                        is_suspend: f.is_suspend,
                    };
                    if let Some(recv_ref) = &f.receiver {
                        // Extension function: index by (erased receiver, method_name).
                        let recv_ty = ty_of_ref(recv_ref, &class_names, &tp, diags);
                        // A nullable reference receiver (`fun String?.foo()`) shares its
                        // [`Ty::erased_recv`] key with the non-null form, so krusty can't pick between a
                        // `String.foo` and a `String?.foo` at the call site (receiver nullability is
                        // folded). An ordinary-named lone overload is unambiguous and supported. But an
                        // *operator*
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
                            .insert((recv_ty.erased_recv(), f.name.clone()), sig)
                            .is_some()
                        {
                            // Two extensions with the same erased receiver + name (a duplicate, or a
                            // nullable/non-null pair) can't be told apart at the call site under
                            // nullability erasure — reject rather than silently pick one.
                            diags.error(f.span, "krusty: conflicting extension functions with the same erased receiver and name".to_string());
                        }
                    } else {
                        // Overloading: keep ALL same-name functions, keyed by name. Only an exact
                        // erased-parameter duplicate is a real conflict; use a Kotlin-level erasure key
                        // here instead of formatting JVM descriptors in the checker.
                        let key = erased_params_semantic_key(&sig);
                        let overloads = table.funs.entry(f.name.clone()).or_default();
                        if overloads
                            .iter()
                            .any(|s| erased_params_semantic_key(s) == key)
                        {
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
                    let ctp = TParams::erased(&c.type_params);
                    // Bring this class's own NESTED types into scope by their SIMPLE name (`Inner` →
                    // `Outer$Inner`), so a member's parameter/return/field type may reference a sibling
                    // nested type unqualified (`fun m(i: Inner)`) — Kotlin's nested-type scoping. A nested
                    // type is a hoisted top-level `Decl::Class` named `Outer.Inner`; map its last segment.
                    let class_names = {
                        let mut ext = class_names.clone();
                        let prefix = format!("{}.", c.name);
                        for &nd in &file.decls {
                            if let Decl::Class(nc) = file.decl(nd) {
                                if let Some(seg) = nc.name.strip_prefix(&prefix) {
                                    // Only bring the nested type into scope when its simple name does NOT
                                    // already resolve to a top-level/imported type — a same-name collision
                                    // (`class Foo; class Outer { class Foo }`) is left to the top-level
                                    // resolution, so the signature checker and the lowerer's `ty_ref`
                                    // (both last-resort on nested) agree (no checker/codegen mismatch).
                                    if !seg.contains('.') && !ext.contains_key(seg) {
                                        let ni = class_names
                                            .get(&nc.name)
                                            .cloned()
                                            .unwrap_or_else(|| class_internal(file, &nc.name));
                                        ext.insert(seg.to_string(), ni);
                                    }
                                }
                            }
                        }
                        ext
                    };
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
                    let ctor_defaults: Vec<Option<CtorDefaultValue>> = c
                        .props
                        .iter()
                        .map(|p| {
                            p.default.and_then(|dx| {
                                extract_ctor_default(file, dx, &class_names, &*libraries)
                            })
                        })
                        .collect();
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
                        let ty = if let Some(de) = bp.delegate {
                            // A delegated member property: type = annotation, else the delegate's
                            // `getValue` return type.
                            match &bp.ty {
                                Some(r) => ty_of_ref(r, &class_names, &ctp, diags),
                                None => infer_lit_ty_p(
                                    file,
                                    de,
                                    &class_names,
                                    &fun_rets,
                                    &init_scope,
                                    &*libraries,
                                )
                                .obj_internal()
                                .and_then(|i| table.method_of(i, "getValue"))
                                .map(|s| s.ret)
                                .unwrap_or(Ty::Error),
                            }
                        } else {
                            match (&bp.ty, &bp.getter) {
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
                                        infer_lit_ty_p(
                                            file,
                                            i,
                                            &class_names,
                                            &fun_rets,
                                            &init_scope,
                                            &*libraries,
                                        )
                                    })
                                    .unwrap_or(Ty::Error),
                            }
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
                            // Resolve the OUTER class's props with the OUTER's own type parameters
                            // (erased) — not this class's `ctp` — so an outer `<T>` used in an outer
                            // property type resolves instead of erroring as an unknown reference here.
                            let octp = TParams::erased(&oc.type_params);
                            for p in oc.props.iter().filter(|p| p.is_property) {
                                props.push((
                                    p.name.clone(),
                                    ty_of_ref(&p.ty, &class_names, &octp, diags),
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
                        // Resolve the BASE class's props with the BASE's own type parameters (erased) —
                        // not the subclass's `ctp`. A base `class A<T> { val some: T }` declares its
                        // member in terms of `T`; resolving it in the subclass's (possibly empty) scope
                        // wrongly reported `T` as an unresolved reference (skipping the whole file).
                        let bctp = TParams::erased(&bc.type_params);
                        for p in bc.props.iter().filter(|p| p.is_property) {
                            props.push((
                                p.name.clone(),
                                ty_of_ref(&p.ty, &class_names, &bctp, diags),
                                p.is_var,
                            ));
                        }
                        for bp in &bc.body_props {
                            let ty = match &bp.ty {
                                Some(r) => ty_of_ref(r, &class_names, &bctp, diags),
                                None => bp
                                    .init
                                    .map(|i| {
                                        infer_lit_ty_p(
                                            file,
                                            i,
                                            &class_names,
                                            &fun_rets,
                                            &[],
                                            &*libraries,
                                        )
                                    })
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
                        // A base method's return type references the BASE class's type parameters
                        // (`abstract fun f(): T` in `A<T>`), NOT the subclass's — resolve under the
                        // base's own params (erased), extended with the method's own (`fun <U> m(): U`).
                        let bctp = TParams::erased(&bc.type_params);
                        for m in &bc.methods {
                            if let Some(r) = &m.ret {
                                let mtp = bctp.extended_with(
                                    &m.type_params,
                                    &m.type_param_bounds,
                                    &|n| class_names.get(n).cloned(),
                                );
                                local_rets.insert(
                                    m.name.clone(),
                                    ty_of_ref(r, &class_names, &mtp, diags),
                                );
                            }
                        }
                        sup_m = bc.base_class.clone();
                    }
                    for m in &c.methods {
                        if let Some(r) = &m.ret {
                            let mtp =
                                ctp.extended_with(&m.type_params, &m.type_param_bounds, &|n| {
                                    class_names.get(n).cloned()
                                });
                            local_rets
                                .insert(m.name.clone(), ty_of_ref(r, &class_names, &mtp, diags));
                        }
                    }
                    let mut methods: HashMap<String, Signature> = c
                        .methods
                        .iter()
                        .map(|m| {
                            let mtp =
                                ctp.extended_with(&m.type_params, &m.type_param_bounds, &|n| {
                                    class_names.get(n).cloned()
                                });
                            // A `vararg` parameter's runtime type is `Array<elem>` (mirrors the
                            // top-level-function path) — without this a member `vararg s: String`
                            // erases to a single `String` and a call passes the element where the
                            // `String[]` is expected (a `ClassCastException`).
                            let ret = m
                                .ret
                                .as_ref()
                                .map(|r| ty_of_ref(r, &class_names, &mtp, diags))
                                .unwrap_or_else(|| {
                                    // These overridable Object/Comparable members have fixed Kotlin
                                    // contract returns. Do not let erased generic body inference widen
                                    // `toString() = privateFun()` to `Any`, which would emit
                                    // `toString(): Object` and fail to override `Object.toString`.
                                    match (m.name.as_str(), m.params.len()) {
                                        ("compareTo", 1) => return Ty::Int,
                                        ("equals", 1) => return Ty::Boolean,
                                        ("hashCode", 0) => return Ty::Int,
                                        ("toString", 0) => return Ty::String,
                                        _ => {}
                                    }
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
                                            &*libraries,
                                        );
                                        if t != Ty::Error {
                                            return t;
                                        }
                                    }
                                    Ty::Unit
                                });
                            (
                                m.name.clone(),
                                member_signature(m, ret, &class_names, &mtp, diags),
                            )
                        })
                        .collect();
                    // Record the un-erased shape of each generic higher-order method (a function-typed
                    // parameter, an explicit return, and a type parameter — the class's or the method's
                    // own — somewhere in its signature), so a call site can substitute the receiver's
                    // type arguments into the lambda parameter types and infer the method's `<R>` from
                    // the lambda body. Plain methods (no type parameter to bind) are skipped.
                    let generic_methods: HashMap<String, GenericMethod> = c
                        .methods
                        .iter()
                        .filter_map(|m| {
                            let has_fun_param = m
                                .params
                                .iter()
                                .any(|p| !p.ty.fun_params.is_empty() || p.ty.name == "<fun>");
                            let ret = m.ret.as_ref()?;
                            if !has_fun_param
                                || (m.type_params.is_empty() && c.type_params.is_empty())
                            {
                                return None;
                            }
                            Some((
                                m.name.clone(),
                                GenericMethod {
                                    method_tparams: m.type_params.clone(),
                                    param_refs: m.params.iter().map(|p| p.ty.clone()).collect(),
                                    ret_ref: ret.clone(),
                                },
                            ))
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
                                    lambda_recv: Vec::new(),
                                    is_inline: false,
                                    is_final: true,
                                    is_suspend: false,
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
                                lambda_recv: Vec::new(),
                                is_inline: false,
                                is_final: true,
                                is_suspend: false,
                            },
                        );
                    }
                    if c.is_object() {
                        table.objects.insert(c.name.clone());
                    }
                    if c.is_enum() {
                        table.enums.insert(
                            c.name.clone(),
                            c.enum_entries.iter().map(|e| e.name.clone()).collect(),
                        );
                    }
                    // Resolve each supertype to a JVM internal name via `class_names` (user/classpath
                    // classes, stdlib aliases, mapped built-ins). A supertype that resolves to none
                    // of those would be emitted as a bare default-package name → `NoClassDefFound`
                    // at load; reject (skip) instead — never emit an unresolved supertype.
                    let mut resolve_super = |s: &str| -> String {
                        match class_names.get(s) {
                            Some(internal) => internal.clone(),
                            None if ctp.contains(s) => s.to_string(), // erased type parameter (degenerate)
                            None => {
                                diags.error(c.span, format!("krusty: supertype '{s}' could not be resolved (provide it on the classpath)"));
                                s.to_string()
                            }
                        }
                    };
                    let interfaces: Vec<String> = c
                        .supertypes
                        .iter()
                        .map(|t| resolve_super(&t.name))
                        .collect();
                    let super_internal = c.base_class.as_deref().map(&mut resolve_super);
                    // A `companion object`'s OWN supertypes, resolved like the class's — so the synthesized
                    // `C$Companion` can be registered as a typed object (`C` used as a value is its
                    // companion, assignable to the companion's supertypes).
                    let companion_interfaces: Vec<String> = c
                        .companion_supertypes
                        .iter()
                        .map(|s| resolve_super(s))
                        .collect();
                    // A `companion object`'s declared base CLASS (`companion object : Base()`) — the
                    // synthesized `C$Companion` extends it, so `C`-as-value is assignable as `Base` (and
                    // transitively its supertypes, e.g. `EmptyContinuation` → `Continuation`). Only a
                    // SAME-FILE non-interface base is registered: that is EXACTLY the shape `ir_lower`
                    // emits (it bails the whole file for any other base). Registering a base `ir_lower`
                    // won't emit would let a *different* file type-check a use of this companion that has
                    // no class file behind it → `NoClassDefFoundError`.
                    let companion_super_internal = c.companion_base.as_ref().and_then(|base| {
                        let is_file_class = file.decls.iter().any(|&fd| {
                            matches!(file.decl(fd), Decl::Class(bc) if bc.name == *base && !bc.is_interface())
                        });
                        is_file_class.then(|| resolve_super(base))
                    });
                    // `companion object` members → static methods/props on this class.
                    let mut static_methods: HashMap<String, Signature> = c
                        .companion_methods
                        .iter()
                        .map(|m| {
                            let mtp =
                                ctp.extended_with(&m.type_params, &m.type_param_bounds, &|n| {
                                    class_names.get(n).cloned()
                                });
                            // A `vararg` parameter's runtime type is `Array<elem>` (mirrors the
                            // top-level-function path) — without this a member `vararg s: String`
                            // erases to a single `String` and a call passes the element where the
                            // `String[]` is expected (a `ClassCastException`).
                            let ret = m
                                .ret
                                .as_ref()
                                .map(|r| ty_of_ref(r, &class_names, &mtp, diags))
                                .unwrap_or_else(|| {
                                    if let FunBody::Expr(e) = &m.body {
                                        let t = infer_lit_ty(
                                            file,
                                            *e,
                                            &class_names,
                                            &fun_rets,
                                            &*libraries,
                                        );
                                        if t != Ty::Error {
                                            t
                                        } else {
                                            Ty::Unit
                                        }
                                    } else {
                                        Ty::Unit
                                    }
                                });
                            (
                                m.name.clone(),
                                member_signature(m, ret, &class_names, &mtp, diags),
                            )
                        })
                        .collect();
                    // PLUGIN SIGNATURE PHASE (kotlinx.serialization): a `@Serializable class C` gains a
                    // synthesized `static serializer(): KSerializer<C>`. The plugin emits its IR body at
                    // the backend phase (after lowering), but its SIGNATURE must be visible to the
                    // type-checker NOW so user references `C.serializer()` resolve. The lowering emits the
                    // call by signature (`invokestatic C.serializer()`); the plugin supplies the method
                    // before emit. Mirrors kotlinc's FIR declaration-generation extension point. The
                    // detection matches the PLUGIN's exactly (simple name of the annotation, fq or not —
                    // `plugins::PluginContext::classes_with_simple("Serializable")`), so the checker
                    // exposes `serializer()` IFF the plugin will emit it (never a missing method at emit).
                    if c.annotations
                        .iter()
                        .any(|a| a.rsplit(['/', '.']).next() == Some("Serializable"))
                    {
                        // A generic `@Serializable class C<T…>` takes one `KSerializer` argument per type
                        // parameter (`C.serializer(KSerializer<T0>, …)`), matching the plugin's generic
                        // `$serializer` constructor; a non-generic class takes none.
                        let n_tp = c.type_params.len();
                        let sparams = vec![Ty::obj("kotlinx/serialization/KSerializer"); n_tp];
                        static_methods.insert(
                            "serializer".to_string(),
                            Signature {
                                params: sparams,
                                ret: Ty::obj_args(
                                    "kotlinx/serialization/KSerializer",
                                    &[Ty::obj(&internal)],
                                ),
                                vararg: false,
                                required: n_tp,
                                param_defaults: vec![],
                                param_names: vec![],
                                lambda_param_types: vec![],
                                lambda_recv: vec![],
                                is_inline: false,
                                is_final: true,
                                is_suspend: false,
                            },
                        );
                    }
                    let static_props: HashMap<String, Ty> = c
                        .companion_props
                        .iter()
                        .map(|p| {
                            let ty = match &p.ty {
                                Some(r) => ty_of_ref(r, &class_names, &ctp, diags),
                                None => p.init.map(|i| infer_lit_ty(file, i, &class_names, &fun_rets, &*libraries)).unwrap_or(Ty::Error),
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
                    // The companion's methods + internal name (for the `C$Companion` typed-object
                    // registration below), captured before `static_methods`/`internal` are moved into the
                    // main class's `ClassSig`.
                    let companion_methods_sigs = static_methods.clone();
                    let comp_internal = format!("{internal}$Companion");
                    table.classes.insert(
                        c.name.clone(),
                        ClassSig {
                            internal,
                            props,
                            ctor_params,
                            methods,
                            is_interface: c.is_interface(),
                            is_abstract: c.is_abstract(),
                            is_fun_interface: c.is_fun_interface,
                            is_sealed: c.is_sealed(),
                            inner_of,
                            static_methods,
                            static_props,
                            lateinit_props,
                            interfaces,
                            super_internal,
                            is_annotation: c.is_annotation(),
                            ctor_defaults,
                            secondary_ctors,
                            tparam_names,
                            generic_props,
                            value_field,
                            generic_methods,
                        },
                    );
                    // Register the synthesized `C$Companion` as a typed object so `C` used as a value (its
                    // companion instance) is assignable to its supertypes and their members resolve. Gated
                    // to exactly what the backend emits: the companion class + `Companion` field only exist
                    // when the companion has methods (`ir_lower` synthesizes it then), and it carries a
                    // declared INTERFACE supertype and/or a base CLASS (`ir_lower` extends the companion to
                    // the base when it's a file class with no-arg / all-default ctor). A companion with no
                    // supertype isn't a first-class value yet (its use as a value skips).
                    if !companion_interfaces.is_empty() || companion_super_internal.is_some() {
                        table.classes.insert(
                            comp_internal.clone(),
                            ClassSig {
                                internal: comp_internal,
                                props: Vec::new(),
                                ctor_params: Vec::new(),
                                methods: companion_methods_sigs,
                                is_interface: false,
                                is_abstract: false,
                                is_fun_interface: false,
                                is_sealed: false,
                                inner_of: None,
                                static_methods: HashMap::new(),
                                static_props: HashMap::new(),
                                lateinit_props: Default::default(),
                                interfaces: companion_interfaces,
                                super_internal: companion_super_internal,
                                is_annotation: false,
                                ctor_defaults: Vec::new(),
                                secondary_ctors: Vec::new(),
                                tparam_names: Vec::new(),
                                generic_props: HashMap::new(),
                                value_field: None,
                                generic_methods: HashMap::new(),
                            },
                        );
                    }
                }
                Decl::Property(p) => {
                    // Extension property `val Recv.name: T get() = …`: register by (erased receiver,
                    // name); emitted as a static `getName(Recv)`/`setName(Recv, T)`.
                    if let Some(recv_ref) = &p.receiver {
                        let recv_ty = ty_of_ref(recv_ref, &class_names, &Default::default(), diags);
                        let ty =
                            p.ty.as_ref()
                                .map(|r| ty_of_ref(r, &class_names, &Default::default(), diags))
                                .or_else(|| match &p.getter {
                                    Some(FunBody::Expr(g)) => Some(infer_lit_ty(
                                        file,
                                        *g,
                                        &class_names,
                                        &fun_rets,
                                        &*libraries,
                                    )),
                                    _ => None,
                                })
                                .unwrap_or(Ty::Error);
                        if recv_ty != Ty::Error && ty != Ty::Error {
                            let key = (recv_ty.erased_recv(), p.name.clone());
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
                    // A top-level backing-field property with a CUSTOM accessor (`val x = init get() =
                    // field`, `var y = init set(v){…}`) is lowered as a facade static + custom
                    // `getX`/`setX` (with `field` bound to the static). Reject only a custom accessor with
                    // NO backing-field initializer — there is nothing for `field` to bind to.
                    let has_custom_accessor = p.getter.is_some() || p.setter.is_some();
                    if has_custom_accessor && p.init.is_none() && !is_computed {
                        diags.error(
                            p.span,
                            "krusty: top-level property custom accessors are not supported"
                                .to_string(),
                        );
                    }
                    // A delegated property `val x: T by Del()`: type is the annotation if present, else the
                    // delegate's `getValue` return type. Resolving the read-type here lets `val a = x`
                    // infer. (The lowering — `x$delegate`/`x$kprop` + `getX()` — is in ir_lower; an
                    // unresolvable delegate type yields `Error` and the file skips.)
                    let ty = if let Some(de) = p.delegate {
                        match &p.ty {
                            Some(r) => ty_of_ref(r, &class_names, &Default::default(), diags),
                            None => infer_lit_ty(file, de, &class_names, &fun_rets, &*libraries)
                                .obj_internal()
                                .and_then(|i| table.method_of(i, "getValue"))
                                .map(|s| s.ret)
                                .unwrap_or(Ty::Error),
                        }
                    } else {
                        // Type from the annotation, else a light inference from a literal initializer (or,
                        // for a computed property, from its expression getter body).
                        match (&p.ty, &p.getter) {
                            (Some(r), _) => ty_of_ref(r, &class_names, &Default::default(), diags),
                            (None, Some(FunBody::Expr(g))) if is_computed => {
                                infer_lit_ty(file, *g, &class_names, &fun_rets, &*libraries)
                            }
                            (None, _) => p
                                .init
                                .map(|i| {
                                    // `val a = other` referencing an already-collected top-level property.
                                    if let Expr::Name(n) = file.expr(i) {
                                        if let Some((t, _, _)) = table.props.get(n) {
                                            return *t;
                                        }
                                    }
                                    infer_lit_ty(file, i, &class_names, &fun_rets, &*libraries)
                                })
                                .unwrap_or(Ty::Error),
                        }
                    };
                    if ty == Ty::Error && (p.init.is_some() || is_computed) && p.ty.is_none() {
                        diags.error(p.span, format!("krusty: cannot infer the type of property '{}'; add an explicit type", p.name));
                    }
                    if is_computed {
                        table.computed_props.insert(p.name.clone());
                    }
                    table
                        .props
                        .insert(p.name.clone(), (ty, p.is_var, p.is_const));
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
    table.canonical_names = canonical_names;
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
                    // A TRAILING LAMBDA is the one positional argument Kotlin allows after named args —
                    // it fills the LAST parameter (a function type). Only the FINAL argument may be such;
                    // any other positional-after-named is an error.
                    if i == args.len() - 1 && n > 0 && slots[n - 1].is_none() {
                        slots[n - 1] = Some(a);
                    } else {
                        return Err(
                            "a positional argument cannot follow a named argument".to_string()
                        );
                    }
                } else {
                    if pos >= n {
                        return Err(format!("too many arguments: expected at most {n}"));
                    }
                    slots[pos] = Some(a);
                    pos += 1;
                }
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

/// Whether any try-with-`finally` in `e` (inclusive) has a `return` in its body/catch that crosses the
/// finally. A `return` inlines its enclosing finallys INTO the body (inside the try's protected range), so
/// if any inlined finally then diverges (a `throw`/`return`/`error()`/`!!` — anything) the enclosing
/// handler re-runs it and the finally runs twice. A `throw` in the BODY is safe (it propagates via the
/// handler, which is outside the range), so only `return` triggers this.
fn expr_try_finally_has_return(file: &File, e: ExprId) -> bool {
    match file.expr(e) {
        Expr::Try {
            body,
            catches,
            finally,
        } => {
            (finally.is_some()
                && (expr_has_return(file, *body)
                    || catches.iter().any(|c| expr_has_return(file, c.body))))
                || expr_try_finally_has_return(file, *body)
                || catches
                    .iter()
                    .any(|c| expr_try_finally_has_return(file, c.body))
                || finally.map_or(false, |f| expr_try_finally_has_return(file, f))
        }
        Expr::Lambda { .. } => false,
        _ => file.any_child_expr(e, &mut |c| expr_try_finally_has_return(file, c), &mut |s| {
            file.any_child_stmt(s, &mut |c| expr_try_finally_has_return(file, c))
        }),
    }
}

/// Whether `e` (inclusive) contains a `return` statement (excludes lambda / local-function bodies, which
/// return from themselves, not the enclosing try).
fn expr_has_return(file: &File, e: ExprId) -> bool {
    match file.expr(e) {
        Expr::Lambda { .. } => false,
        _ => file.any_child_expr(e, &mut |c| expr_has_return(file, c), &mut |s| {
            stmt_has_return(file, s)
        }),
    }
}

fn stmt_has_return(file: &File, s: StmtId) -> bool {
    match file.stmt(s) {
        Stmt::Return(..) => true,
        Stmt::LocalFun(_) => false, // a local function's `return` is its own
        _ => file.any_child_stmt(s, &mut |c| expr_has_return(file, c)),
    }
}

/// Whether any `finally` block in `e` (inclusive) itself contains a `try`. Such a `finally` is inlined at
/// each exit, duplicating the inner `try`'s exception ranges/frames (a verify error) — unsupported.
fn expr_has_finally_with_try(file: &File, e: ExprId) -> bool {
    match file.expr(e) {
        Expr::Try {
            body,
            catches,
            finally,
        } => {
            finally.map_or(false, |f| expr_has_try(file, f))
                || expr_has_finally_with_try(file, *body)
                || catches
                    .iter()
                    .any(|c| expr_has_finally_with_try(file, c.body))
                || finally.map_or(false, |f| expr_has_finally_with_try(file, f))
        }
        Expr::Lambda { .. } => false,
        _ => file.any_child_expr(e, &mut |c| expr_has_finally_with_try(file, c), &mut |s| {
            file.any_child_stmt(s, &mut |c| expr_has_finally_with_try(file, c))
        }),
    }
}

/// Whether `break`/`continue` appears in a position krusty's backend can't yet emit: in *value*
/// position (its value would be consumed while operands sit on the stack — an operand-spill the
/// emitter doesn't do), inside a `try` (the jump must cross exception regions / run `finally`), or
/// inside a lambda (a non-local jump). `forbidden` is true once the walk is in such a context.
/// Plain `break`/`continue` *statements* in a loop body or `if`/`when` statement branch are fine.
// A `break`/`continue` is emit-able only as a loop jump in the CURRENT function reached by a plain goto:
// in statement position or as a direct `if`/`when` BRANCH, and NOT crossing a lambda/`try` boundary to its
// target loop. Two independent flags track the two ways a position is unsupported:
//   `vforbid` — a non-branch VALUE position (`f(break)`, `x + break`); cleared by an `if`/`when` branch.
//   `cross`   — inside a LAMBDA or `try` whose target loop is outside it (a non-local jump); NOT cleared by
//               a branch (a break in an if-branch inside a lambda still crosses it).
// An enclosing LOOP body clears BOTH: a break there targets that (in-scope) loop, so it's a local goto.
fn bc_complex_e(file: &File, e: ExprId, vforbid: bool, cross: bool) -> bool {
    let val = |x: ExprId| bc_complex_e(file, x, true, cross);
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
        // A lambda body's `break`/`continue` targeting an OUTER loop is a non-local jump — set `cross`.
        Expr::Lambda { body, .. } => bc_complex_e(file, *body, true, true),
        // The condition/subject is a value; a branch clears `vforbid` (a direct branch break lowers to a
        // goto, the merge skips the diverging branch) but keeps `cross` (a branch break inside a lambda/try
        // still crosses it).
        Expr::If {
            cond,
            then_branch,
            else_branch,
        } => {
            val(*cond)
                || bc_complex_e(file, *then_branch, false, cross)
                || else_branch.map_or(false, |x| bc_complex_e(file, x, false, cross))
        }
        Expr::When { subject, arms } => {
            subject.map_or(false, &val)
                || arms.iter().any(|a| {
                    a.conditions.iter().any(|&c| val(c)) || bc_complex_e(file, a.body, false, cross)
                })
        }
        Expr::Block { stmts, trailing } => {
            stmts.iter().any(|&s| bc_complex_s(file, s, vforbid, cross))
                || trailing.map_or(false, |t| bc_complex_e(file, t, vforbid, cross))
        }
        // A break/continue inside a `try` would cross its region — set `cross`.
        Expr::Try {
            body,
            catches,
            finally,
        } => {
            bc_complex_e(file, *body, true, true)
                || catches
                    .iter()
                    .any(|c| bc_complex_e(file, c.body, true, true))
                || finally.map_or(false, |f| bc_complex_e(file, f, true, true))
        }
        // Every other expression evaluates its children as *values* (vforbid; `cross` unchanged).
        _ => file.any_child_expr(e, &mut |c| val(c), &mut |s| {
            bc_complex_s(file, s, true, cross)
        }),
    }
}

fn bc_complex_s(file: &File, s: StmtId, vforbid: bool, cross: bool) -> bool {
    let val = |x: ExprId| bc_complex_e(file, x, true, cross);
    // An enclosing loop body: a break/continue there targets THIS loop — a local goto, so clear both flags.
    let loop_body = |x: ExprId| bc_complex_e(file, x, false, false);
    match file.stmt(s) {
        Stmt::Break(_) | Stmt::Continue(_) => vforbid || cross,
        Stmt::Local { init, .. }
        | Stmt::Destructure { init, .. }
        | Stmt::LocalDelegate { delegate: init, .. }
        | Stmt::Assign { value: init, .. } => val(*init),
        Stmt::AssignMember {
            receiver, value, ..
        } => val(*receiver) || val(*value),
        Stmt::AssignIndex {
            array,
            index,
            value,
        } => val(*array) || val(*index) || val(*value),
        Stmt::Return(Some(e), _) => val(*e),
        Stmt::Return(None, _) | Stmt::IncDec { .. } | Stmt::LocalLateinit { .. } => false,
        // A statement's value is discarded — its (possibly `if`/`when`) tree stays in statement position.
        Stmt::Expr(e) => bc_complex_e(file, *e, false, cross),
        Stmt::While { cond, body, .. } | Stmt::DoWhile { cond, body, .. } => {
            val(*cond) || loop_body(*body)
        }
        Stmt::For { range, body, .. } => val(range.start) || val(range.end) || loop_body(*body),
        Stmt::ForEach { iterable, body, .. } => val(*iterable) || loop_body(*body),
        // A local function is a separate body — `break`/`continue` in it would be non-local.
        Stmt::LocalFun(f) => match &f.body {
            FunBody::Expr(e) | FunBody::Block(e) => bc_complex_e(file, *e, true, true),
            FunBody::None => false,
        },
        // A local class is hoisted + checked separately; no enclosing-loop break/continue in its body.
        Stmt::LocalClass(_) => false,
    }
}

fn expr_refs_param(file: &File, e: ExprId, names: &std::collections::HashSet<&str>) -> bool {
    expr_refs_param_inner(file, e, names, false)
}

/// `into_lambdas`: when true, recurse into a NESTED lambda's body to detect a TRANSITIVE capture
/// (`f { g { use(outer) } }` — `outer` is captured by both `f` and `g`). A nested lambda's own bound
/// names (its explicit params, or `it`) shadow the outer ones, so they are removed before recursing.
fn expr_refs_param_inner(
    file: &File,
    e: ExprId,
    names: &std::collections::HashSet<&str>,
    into_lambdas: bool,
) -> bool {
    match file.expr(e) {
        Expr::Name(n) => names.contains(n.as_str()),
        Expr::Lambda { params, body } if into_lambdas => {
            let body = *body;
            let mut shadowed: std::collections::HashSet<&str> =
                params.iter().map(String::as_str).collect();
            if params.is_empty() {
                shadowed.insert("it");
            }
            let remaining: std::collections::HashSet<&str> =
                names.difference(&shadowed).copied().collect();
            !remaining.is_empty() && expr_refs_param_inner(file, body, &remaining, true)
        }
        // A lambda introduces a new `it` scope — stop (its captures are handled elsewhere).
        Expr::Lambda { .. } => false,
        _ => file.any_child_expr(
            e,
            &mut |c| expr_refs_param_inner(file, c, names, into_lambdas),
            &mut |s| stmt_refs_param(file, s, names),
        ),
    }
}

/// Whether `e`'s subtree uses `name`, recursing THROUGH nested lambdas — for CLOSURE-capture detection,
/// where a name used in a nested lambda is also captured by the enclosing one.
pub fn expr_uses_name_deep(file: &File, e: ExprId, name: &str) -> bool {
    let set: std::collections::HashSet<&str> = std::iter::once(name).collect();
    expr_refs_param_inner(file, e, &set, true)
}

/// Returns true if the expression subtree (or any statement within it) references a name from
/// `outer`. Used to detect captures in local function bodies before allowing lift-to-static.
fn local_fun_body_uses_any(
    file: &File,
    e: ExprId,
    outer: &std::collections::HashSet<String>,
) -> bool {
    fn check_e(file: &File, e: ExprId, active: &std::collections::HashSet<String>) -> bool {
        match file.expr(e) {
            Expr::Name(n) => active.contains(n),
            Expr::Block { stmts, trailing } => {
                let mut active = active.clone();
                for &s in stmts {
                    if check_s(file, s, &mut active) {
                        return true;
                    }
                }
                trailing.is_some_and(|t| check_e(file, t, &active))
            }
            Expr::Lambda { params, body } => {
                let mut active = active.clone();
                for p in params {
                    active.remove(p);
                }
                if params.is_empty() {
                    active.remove("it");
                }
                check_e(file, *body, &active)
            }
            Expr::Try {
                body,
                catches,
                finally,
            } => {
                if check_e(file, *body, active) {
                    return true;
                }
                for c in catches {
                    let mut catch_active = active.clone();
                    catch_active.remove(&c.name);
                    if check_e(file, c.body, &catch_active) {
                        return true;
                    }
                }
                finally.is_some_and(|f| check_e(file, f, active))
            }
            _ => file.any_child_expr(e, &mut |c| check_e(file, c, active), &mut |s| {
                let mut active = active.clone();
                check_s(file, s, &mut active)
            }),
        }
    }
    fn check_s(file: &File, s: StmtId, active: &mut std::collections::HashSet<String>) -> bool {
        match file.stmt(s) {
            Stmt::IncDec { name, .. } => active.contains(name),
            Stmt::Local { name, init, .. } => {
                let used = check_e(file, *init, active);
                active.remove(name);
                used
            }
            Stmt::LocalDelegate { name, delegate, .. } => {
                let used = check_e(file, *delegate, active);
                active.remove(name);
                used
            }
            Stmt::Destructure { entries, init } => {
                let used = check_e(file, *init, active);
                for (name, _) in entries {
                    active.remove(name);
                }
                used
            }
            Stmt::For {
                name, range, body, ..
            } => {
                if check_e(file, range.start, active) || check_e(file, range.end, active) {
                    return true;
                }
                let mut body_active = active.clone();
                body_active.remove(name);
                check_e(file, *body, &body_active)
            }
            Stmt::ForEach {
                name,
                iterable,
                body,
                ..
            } => {
                if check_e(file, *iterable, active) {
                    return true;
                }
                let mut body_active = active.clone();
                body_active.remove(name);
                check_e(file, *body, &body_active)
            }
            Stmt::LocalFun(_) => false, // nested local funs have their own capture check
            _ => file.any_child_stmt(s, &mut |c| check_e(file, c, active)),
        }
    }
    check_e(file, e, outer)
}

/// Collect the outer-variable names a lambda body writes (assigns / `++`/`--`), so the lowerer can box
/// them. Does not descend into nested lambdas; their writes are recorded when those lambdas are checked.
fn collect_lambda_outer_writes(
    file: &File,
    e: ExprId,
    outer_names: &std::collections::HashSet<String>,
    out: &mut std::collections::HashSet<String>,
) {
    fn ce(
        file: &File,
        e: ExprId,
        active: &std::collections::HashSet<String>,
        out: &mut std::collections::HashSet<String>,
    ) {
        match file.expr(e) {
            Expr::Block { stmts, trailing } => {
                let mut active = active.clone();
                for &s in stmts {
                    cs(file, s, &mut active, out);
                }
                if let Some(t) = trailing {
                    ce(file, *t, &active, out);
                }
            }
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                ce(file, *cond, active, out);
                ce(file, *then_branch, active, out);
                if let Some(x) = else_branch {
                    ce(file, *x, active, out);
                }
            }
            Expr::Try {
                body,
                catches,
                finally,
            } => {
                ce(file, *body, active, out);
                for c in catches {
                    let mut catch_active = active.clone();
                    catch_active.remove(&c.name);
                    ce(file, c.body, &catch_active, out);
                }
                if let Some(f) = finally {
                    ce(file, *f, active, out);
                }
            }
            Expr::When { subject, arms } => {
                if let Some(s) = subject {
                    ce(file, *s, active, out);
                }
                for a in arms {
                    for &c in &a.conditions {
                        ce(file, c, active, out);
                    }
                    ce(file, a.body, active, out);
                }
            }
            Expr::Lambda { .. } => {}
            _ => {}
        }
    }
    fn cs(
        file: &File,
        s: StmtId,
        active: &mut std::collections::HashSet<String>,
        out: &mut std::collections::HashSet<String>,
    ) {
        match file.stmt(s) {
            Stmt::IncDec { name, .. } => {
                if active.contains(name) {
                    out.insert(name.clone());
                }
            }
            Stmt::Assign { name, value } => {
                if active.contains(name) {
                    out.insert(name.clone());
                }
                ce(file, *value, active, out);
            }
            Stmt::Local { name, init, .. } => {
                ce(file, *init, active, out);
                active.remove(name);
            }
            Stmt::LocalDelegate { name, delegate, .. } => {
                ce(file, *delegate, active, out);
                active.remove(name);
            }
            Stmt::LocalLateinit { name, .. } => {
                active.remove(name);
            }
            Stmt::Destructure { entries, init } => {
                ce(file, *init, active, out);
                for (name, _) in entries {
                    active.remove(name);
                }
            }
            Stmt::AssignMember {
                receiver, value, ..
            } => {
                ce(file, *receiver, active, out);
                ce(file, *value, active, out);
            }
            Stmt::AssignIndex {
                array,
                index,
                value,
            } => {
                ce(file, *array, active, out);
                ce(file, *index, active, out);
                ce(file, *value, active, out);
            }
            Stmt::Return(Some(e), _) => ce(file, *e, active, out),
            Stmt::While { cond, body, .. } | Stmt::DoWhile { cond, body, .. } => {
                ce(file, *cond, active, out);
                ce(file, *body, active, out);
            }
            Stmt::For {
                name, range, body, ..
            } => {
                ce(file, range.start, active, out);
                ce(file, range.end, active, out);
                let mut body_active = active.clone();
                body_active.remove(name);
                ce(file, *body, &body_active, out);
            }
            Stmt::ForEach {
                name,
                iterable,
                body,
                ..
            } => {
                ce(file, *iterable, active, out);
                let mut body_active = active.clone();
                body_active.remove(name);
                ce(file, *body, &body_active, out);
            }
            Stmt::Expr(e) => ce(file, *e, active, out),
            Stmt::Return(None, _)
            | Stmt::Break(_)
            | Stmt::Continue(_)
            | Stmt::LocalFun(_)
            | Stmt::LocalClass(_) => {}
        }
    }
    ce(file, e, outer_names, out);
}

/// Collect every name reassigned (`=`, `+=`-style, `++`/`--`) anywhere in `e`'s subtree — INCLUDING
/// inside nested lambdas and local functions (a `var` reassigned in a sibling closure still needs the
/// box). Used to decide which captured local-function `var`s need a shared mutable cell.
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
        Expr::UIntLit(_) => Ty::UInt,
        Expr::ULongLit(_) => Ty::ULong,
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
            UnOp::Neg | UnOp::Plus => infer_getter_ty(file, *operand, locals),
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

fn companion_const(
    src: &dyn SymbolSource,
    class_names: &ClassNames,
    type_name: &str,
    const_name: &str,
) -> Option<crate::libraries::LibraryConst> {
    let fallback;
    let internal = match class_names.get(type_name) {
        Some(internal) => internal.as_str(),
        None => {
            fallback = format!("kotlin/{type_name}");
            &fallback
        }
    };
    src.resolve_type(internal)
        .and_then(|t| t.companion_consts.get(const_name).copied())
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

const CALLABLE_INVOKE_OPERATOR: &str = "invoke";

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
    class_names: &ClassNames,
    fun_rets: &HashMap<String, Ty>,
    src: &dyn SymbolSource,
) -> Ty {
    infer_lit_ty_p(file, e, class_names, fun_rets, &[], src)
}

/// The common type of two branch values for the lightweight signature inferer: identical types
/// collapse, numeric types widen (`Int`/`Long` → `Long`); anything else is `Error` (so the caller
/// conservatively skips rather than guessing a supertype). Deliberately narrower than the full
/// checker's least-upper-bound — it only needs to be SOUND, never complete.
fn common_lit_ty(a: Ty, b: Ty) -> Ty {
    if a == b {
        a
    } else {
        Ty::promote(a, b).unwrap_or(Ty::Error)
    }
}

fn infer_lit_ty_p(
    file: &File,
    e: ExprId,
    class_names: &ClassNames,
    fun_rets: &HashMap<String, Ty>,
    props: &[(String, Ty, bool)],
    src: &dyn SymbolSource,
) -> Ty {
    // Resolve the (single) return type of `name` applied to `receiver` (a method/extension when
    // `receiver` is `Some`, a top-level function when `None`) through the FEDERATED symbol source — the
    // same classpath/stdlib resolution the full checker uses. Returns a type only when every applicable
    // overload AGREES on the return type (no arg-based overload selection here); otherwise `None`, so the
    // caller falls back to `Error` (skip) rather than guess. NO stdlib symbol names are hardcoded.
    fn resolved_ret(src: &dyn SymbolSource, name: &str, receiver: Option<Ty>) -> Option<Ty> {
        let fs = src.functions(name, receiver);
        let mut ret: Option<Ty> = None;
        for o in &fs.overloads {
            match ret {
                None => ret = Some(o.callable.ret),
                Some(r) if r == o.callable.ret => {}
                Some(_) => return None, // overloads disagree on return type — needs arg selection
            }
        }
        ret
    }
    match file.expr(e) {
        Expr::IntLit(_) => Ty::Int,
        Expr::LongLit(_) => Ty::Long,
        Expr::UIntLit(_) => Ty::UInt,
        Expr::ULongLit(_) => Ty::ULong,
        Expr::DoubleLit(_) => Ty::Double,
        Expr::FloatLit(_) => Ty::Float,
        Expr::BoolLit(_) => Ty::Boolean,
        Expr::CharLit(_) => Ty::Char,
        Expr::StringLit(_) | Expr::Template(_) => Ty::String,
        // A bare name referring to a property (or `this` — the receiver of an expression-bodied
        // extension function `fun Int.double() = this * 2`, supplied as a `"this"` scope entry), or a
        // classpath/stdlib `object` used as a value (`val ctx = EmptyCoroutineContext`): its value is the
        // singleton, of the object's own type. Only an `object` is a value, so the classpath fallback
        // never mistypes a plain class name (which isn't a value and stays `Error` → the file skips).
        Expr::Name(n) => props
            .iter()
            .find(|(pn, _, _)| pn == n)
            .map(|(_, t, _)| *t)
            .or_else(|| {
                class_names
                    .get(n.as_str())
                    .filter(|internal| src.resolve_type(internal).is_some_and(|t| t.is_object()))
                    .map(|internal| Ty::obj(internal))
            })
            .unwrap_or(Ty::Error),
        Expr::Member { receiver, name } => {
            if let Expr::Name(type_name) = file.expr(*receiver) {
                if let Some(c) = companion_const(src, class_names, type_name, name) {
                    return c.ty;
                }
            }
            // Property read (`s.length`, `list.size`, `vc.value`): resolve through the FEDERATED source —
            // the same path the full checker uses, no hardcoded property names.
            let rt = infer_lit_ty_p(file, *receiver, class_names, fun_rets, props, src);
            if let Some(m) = crate::call_resolver::resolve_property_member(src, rt, name) {
                return m.ret;
            }
            Ty::Error
        }
        Expr::Unary { op, operand } => match op {
            UnOp::Not => Ty::Boolean,
            UnOp::Neg | UnOp::Plus => {
                infer_lit_ty_p(file, *operand, class_names, fun_rets, props, src)
            }
        },
        Expr::Binary { op, lhs, rhs } => {
            let (lt, rt) = (
                infer_lit_ty_p(file, *lhs, class_names, fun_rets, props, src),
                infer_lit_ty_p(file, *rhs, class_names, fun_rets, props, src),
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
        Expr::Call { callee, args } => {
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
                    // A top-level library/stdlib function — federated resolution (no hardcoded names).
                    if let Some(t) = resolved_ret(src, n, None) {
                        return t;
                    }
                    // A member of a classpath OBJECT imported unqualified (`import Obj.member`; the
                    // top-level `val logger = logger {}` idiom): resolve the member's return on the
                    // object's singleton type — mirroring the checker's `object_member_import`.
                    if let Some(internal) = object_member_import_sig(file, n, src) {
                        if let Some(t) = resolved_ret(src, n, Some(Ty::obj(&internal))) {
                            return t;
                        }
                    }
                    // A GENERIC top-level function whose return type depends on its arguments
                    // (`arrayOf("a","b")` → `Array<String>`, `mapOf(1 to "x")` → `Map<Int,String>`):
                    // the return-agreement probe above can't decide it (the erased return is the same
                    // for every call), so resolve through the SAME federated `CallResolver` the full
                    // checker uses, binding the type parameters from the inferred argument types. Only
                    // reached when the simpler probe returned `None`, so it never overrides an inference.
                    let arg_tys: Vec<Ty> = args
                        .iter()
                        .map(|a| infer_lit_ty_p(file, *a, class_names, fun_rets, props, src))
                        .collect();
                    if !arg_tys.contains(&Ty::Error) {
                        if let Some(c) = crate::call_resolver::CallResolver::new(src)
                            .resolve_top_level_callable(n, &arg_tys, &[])
                        {
                            return c.ret;
                        }
                    }
                }
                // A method/extension call (`n.toLong()`, `s.uppercase()`, `r shl 8` → `r.shl(8)`,
                // `x.toString()`): resolve the return type through the FEDERATED symbol source (the same
                // classpath/stdlib resolution the full checker uses) — NO stdlib symbol names hardcoded.
                Expr::Member { receiver, name } => {
                    // `this.method()` — a method of the CURRENT module's class; the federated source
                    // doesn't carry the module's own (in-progress) signatures, so use the rets map.
                    if matches!(file.expr(*receiver), Expr::Name(r) if r == "this") {
                        if let Some(ret) = fun_rets.get(name.as_str()) {
                            return *ret;
                        }
                    }
                    let recv_ty =
                        infer_lit_ty_p(file, *receiver, class_names, fun_rets, props, src);
                    if recv_ty != Ty::Error {
                        if let Some(t) = builtin_bitwise_ret(recv_ty, name, args.len()) {
                            return t;
                        }
                        // Everything else (`s.uppercase()`, library members/extensions): federated
                        // classpath/stdlib resolution — no hardcoded symbol names.
                        if let Some(t) = resolved_ret(src, name, Some(recv_ty)) {
                            return t;
                        }
                        // A USER receiver type (a module class — not in the library source, so the call
                        // above found nothing) can still call an `Any`-inherited member (`toString`,
                        // `hashCode`, `equals`): resolve it on `kotlin/Any`, the universal supertype the
                        // source DOES carry. Still real classpath resolution — no member name hardcoded.
                        if let Some(t) = resolved_ret(src, name, Some(Ty::obj("kotlin/Any"))) {
                            return t;
                        }
                    }
                }
                _ => {}
            }
            Ty::Error
        }
        // An `if`/`else` expression body (`fun f(x: Int) = if (x > 0) x else -x`): the common type of
        // the two branches. Needs an `else` to be a value; a branch whose type can't be inferred (e.g. a
        // block with locals) yields `Error`, so the whole `if` does → safe skip, never a wrong type.
        Expr::If {
            then_branch,
            else_branch: Some(eb),
            ..
        } => {
            let t = infer_lit_ty_p(file, *then_branch, class_names, fun_rets, props, src);
            let e = infer_lit_ty_p(file, *eb, class_names, fun_rets, props, src);
            common_lit_ty(t, e)
        }
        // A `when` expression body — the common type of all arm bodies. Requires an explicit `else` arm
        // (provably exhaustive as a value); any arm whose body type can't be inferred → `Error` (skip).
        Expr::When { arms, .. } => {
            if !arms.iter().any(|a| a.conditions.is_empty()) {
                return Ty::Error;
            }
            let mut acc: Option<Ty> = None;
            for a in arms {
                let bt = infer_lit_ty_p(file, a.body, class_names, fun_rets, props, src);
                if bt == Ty::Error {
                    return Ty::Error;
                }
                acc = Some(match acc {
                    None => bt,
                    Some(p) => common_lit_ty(p, bt),
                });
            }
            acc.unwrap_or(Ty::Error)
        }
        // A block expression's value is its trailing expression (`= { … ; value }`). Statements aren't
        // tracked here, so a trailing referring to a local infers `Error` (safe skip).
        Expr::Block {
            trailing: Some(t), ..
        } => infer_lit_ty_p(file, *t, class_names, fun_rets, props, src),
        // A range value (`val r = 1..10`, `0 until n`, `4 downTo 1`) — the matching stdlib range type
        // (mirrors the checker's `RangeTo` typing), so a range-typed property's type infers.
        Expr::RangeTo { lo, hi, .. } => {
            let lt = infer_lit_ty_p(file, *lo, class_names, fun_rets, props, src);
            let rt = infer_lit_ty_p(file, *hi, class_names, fun_rets, props, src);
            match (lt, rt) {
                (Ty::Char, Ty::Char) => Ty::obj("kotlin/ranges/CharRange"),
                (Ty::UInt, Ty::UInt) => Ty::obj("kotlin/ranges/UIntRange"),
                (Ty::ULong, Ty::ULong) => Ty::obj("kotlin/ranges/ULongRange"),
                _ if lt.is_int_range_operand() && rt.is_int_range_operand() => {
                    Ty::obj("kotlin/ranges/IntRange")
                }
                _ if (lt.is_int_range_operand() || lt == Ty::Long)
                    && (rt.is_int_range_operand() || rt == Ty::Long) =>
                {
                    Ty::obj("kotlin/ranges/LongRange")
                }
                _ => Ty::Error,
            }
        }
        // A top-level function reference `::foo` initializing a property (`val x = ::Test`). Its type is
        // the function type `(params) -> ret` of the referenced function. Only a receiver-less,
        // UNAMBIGUOUS (single top-level overload) classpath function resolves here through the federated
        // source — enough for a property's signature; the full checker types same-module/local refs.
        Expr::CallableRef {
            receiver: None,
            name,
        } if name != "class" => {
            let tl: Vec<_> = src
                .functions(name, None)
                .overloads
                .into_iter()
                .filter(|o| o.kind == crate::libraries::FnKind::TopLevel)
                .collect();
            match tl.as_slice() {
                [o] if o.callable.vararg_elem.is_none() => {
                    Ty::fun(o.callable.params.clone(), o.callable.ret)
                }
                _ => Ty::Error,
            }
        }
        _ => Ty::Error,
    }
}

/// Generic type parameters in scope, each with its JVM erasure. A parameter with a wrappable standard
/// primitive upper bound (`<T : Int>`) erases to that PRIMITIVE — kotlinc specializes it (descriptor
/// `(I)I`, not `(Object)Object`); every other parameter erases to `java/lang/Object`.
#[derive(Default, Clone)]
pub struct TParams {
    erasure: std::collections::HashMap<String, Ty>,
}

impl TParams {
    pub fn contains(&self, name: &str) -> bool {
        self.erasure.contains_key(name)
    }

    /// The erasure of type parameter `name` (`Object` if unbounded / reference-bounded).
    pub fn erase(&self, name: &str) -> Ty {
        self.erasure
            .get(name)
            .cloned()
            .unwrap_or_else(|| Ty::obj("kotlin/Any"))
    }

    /// Build directly from explicit name→type bindings — a SUBSTITUTION (concrete types), not an
    /// erasure. Used to map a generic method's type parameters to their receiver-supplied / inferred
    /// concrete types so `ty_of_ref` realizes `T`/`R` to `String`/`Int` rather than the erased `Object`.
    pub fn from_bindings(bindings: impl IntoIterator<Item = (String, Ty)>) -> Self {
        TParams {
            erasure: bindings.into_iter().collect(),
        }
    }

    /// All parameters erased to `Object` (no primitive specialization). Used for CLASS type parameters:
    /// kotlinc specializes a class's primitive-bounded param too, but krusty's value-class pass already
    /// owns class-param bound handling, and naive specialization there breaks the Object/value-class
    /// boundary (VerifyError) — so classes keep the erased model; only FUNCTION params specialize.
    pub fn erased(names: &[String]) -> Self {
        let erasure = names
            .iter()
            .map(|n| (n.clone(), Ty::obj("kotlin/Any")))
            .collect();
        TParams { erasure }
    }

    /// Build from declared names + their upper bounds, resolving a CLASS/interface bound to its JVM
    /// type so member access on `T` resolves and the descriptor erases to the bound (`<T: CharSequence>`
    /// → `java/lang/CharSequence`, not `Object`). `resolve` maps a bound's simple class name to its JVM
    /// internal name (primitive/`String` bounds need no resolver). Without a resolver (`from_decl`) only
    /// primitive bounds are recovered — a reference bound stays `Any`.
    pub fn from_decl_with(
        names: &[String],
        bounds: &[(String, TypeRef)],
        resolve: &dyn Fn(&str) -> Option<String>,
    ) -> Self {
        let erasure = names
            .iter()
            .map(|n| {
                let b = bounds.iter().find(|(bn, _)| bn == n).map(|(_, b)| b);
                (n.clone(), tparam_bound_erasure(b, resolve))
            })
            .collect();
        TParams { erasure }
    }

    pub fn extended_with(
        &self,
        names: &[String],
        bounds: &[(String, TypeRef)],
        resolve: &dyn Fn(&str) -> Option<String>,
    ) -> Self {
        let mut out = self.clone();
        out.erasure
            .extend(TParams::from_decl_with(names, bounds, resolve).erasure);
        out
    }

    pub fn insert_decl_with(
        &mut self,
        names: &[String],
        bounds: &[(String, TypeRef)],
        resolve: &dyn Fn(&str) -> Option<String>,
    ) -> Vec<String> {
        let scoped = TParams::from_decl_with(names, bounds, resolve);
        let mut added = Vec::new();
        for (n, e) in scoped.erasure {
            if !self.erasure.contains_key(&n) {
                self.erasure.insert(n.clone(), e);
                added.push(n);
            }
        }
        added
    }

    pub fn remove(&mut self, name: &str) {
        self.erasure.remove(name);
    }

    pub fn clear(&mut self) {
        self.erasure.clear();
    }
}

/// Bind a method's own type parameters by unifying a declared `TypeRef` against an actual `Ty`
/// (`R` ↔ `Int`; `(T) -> R` ↔ `(String) -> Int` binds `R`; `List<R>` ↔ `List<Int>`). Only names in
/// `tparams` are bound — anything else recurses structurally. The source-`TypeRef` analogue of
/// [`crate::call_resolver::unify_gsig`], for user-declared generic methods.
fn unify_ref(r: &TypeRef, actual: Ty, tparams: &[String], binds: &mut HashMap<String, Ty>) {
    // A function-type ref `(A) -> B` unifies against a lambda's `Ty::Fun`: its parameters bind from the
    // function's parameters, its return from the function's return (where `map`'s `R` is bound).
    if !r.fun_params.is_empty() || r.name == "<fun>" {
        if let Ty::Fun(fsig) = actual {
            for (p, a) in r.fun_params.iter().zip(fsig.params.iter()) {
                unify_ref(p, *a, tparams, binds);
            }
            if let Some(ret) = &r.arg {
                unify_ref(ret, fsig.ret, tparams, binds);
            }
        }
        return;
    }
    if tparams.iter().any(|t| t == &r.name) {
        binds.entry(r.name.clone()).or_insert(actual);
        return;
    }
    // A generic class ref `C<…>` unifies its arguments positionally against the actual's carried args.
    if !r.targs.is_empty() {
        if let Ty::Obj(_, targs) = actual {
            for (a, t) in r.targs.iter().zip(targs.iter()) {
                unify_ref(a, *t, tparams, binds);
            }
        }
    }
}

/// A bound-name → JVM-internal resolver over a `SymbolTable`: a user-declared class by simple name, else
/// the merged class-name map (which already carries the Kotlin built-in → JVM mapping, `CharSequence`
/// → `java/lang/CharSequence`). Borrows only the (copied) `&SymbolTable`, so a caller can hold it while
/// mutating `self.tparams`.
fn class_internal_resolver(syms: &SymbolTable) -> impl Fn(&str) -> Option<String> + '_ {
    move |n: &str| {
        syms.classes
            .get(n)
            .map(|c| c.internal.clone())
            .or_else(|| syms.class_names.get(n).cloned())
    }
}

/// The JVM erasure of a type parameter from its declared upper bound. kotlinc erases a bounded `T` to
/// its bound's type — a specializable integral primitive bound (`<T: Int>`) to that primitive, any other
/// reference bound (`<T: CharSequence>`, `<T: Comparable<T>>`, a user class) to the bound's class — so a
/// value of type `T` accesses the bound's members and the descriptor uses the bound, not `Object`.
/// `Any` when there's no bound, a nullable bound, an unresolved bound, or a non-specializable primitive
/// (`Double`/unsigned/value — those bounds stay rejected on use).
fn tparam_bound_erasure(b: Option<&TypeRef>, resolve: &dyn Fn(&str) -> Option<String>) -> Ty {
    let any = Ty::obj("kotlin/Any");
    let Some(b) = b else { return any };
    // A nullable / generic-instantiated bound name still resolves by its head: `<T: Comparable<T>>`
    // erases to `Comparable` (type args drop on erasure). A nullable bound keeps `Any` (conservative).
    if b.nullable {
        return any;
    }
    if let Some(prim) = Ty::from_name(&b.name) {
        // Integral primitive bound specializes; `String`/`Any` reference bounds erase to themselves;
        // other primitives (`Double`, unsigned, value) are not specialized → `Any`.
        return if prim.is_specializable_bound() || prim.is_reference() {
            prim
        } else {
            any
        };
    }
    match resolve(&b.name) {
        Some(internal) => Ty::obj(&internal),
        None => any,
    }
}

#[derive(Clone, Copy, Default)]
struct TypeRefCtx {
    class_literal_ty: Option<Ty>,
}

/// Build a member method's [`Signature`] from its declaration, given an already-resolved return type
/// `ret` (the two call sites differ only in how they infer `ret`). A `vararg` parameter's runtime type
/// is its `Array<elem>`; the `vararg` flag, defaults, names, and lambda-parameter shapes follow the
/// declaration. Member methods are never `inline`.
fn member_signature(
    m: &FunDecl,
    ret: Ty,
    classes: &ClassNames,
    mtp: &TParams,
    diags: &mut DiagSink,
) -> Signature {
    let params: Vec<Ty> = m
        .params
        .iter()
        .map(|p| {
            let t = ty_of_ref(&p.ty, classes, mtp, diags);
            if p.is_vararg {
                Ty::array(t)
            } else {
                t
            }
        })
        .collect();
    let lambda_param_types: Vec<Vec<Ty>> = m
        .params
        .iter()
        .map(|p| {
            if !p.ty.fun_params.is_empty() || p.ty.name == "<fun>" {
                p.ty.fun_params
                    .iter()
                    .map(|r| ty_of_ref(r, classes, mtp, diags))
                    .collect()
            } else {
                Vec::new()
            }
        })
        .collect();
    Signature {
        params,
        ret,
        vararg: m.params.last().is_some_and(|p| p.is_vararg),
        required: m.params.iter().take_while(|p| p.default.is_none()).count(),
        param_defaults: m.params.iter().map(|p| p.default.is_some()).collect(),
        param_names: m.params.iter().map(|p| p.name.clone()).collect(),
        lambda_param_types,
        lambda_recv: m.params.iter().map(|p| p.ty.fun_has_receiver).collect(),
        is_inline: false,
        is_final: m.is_final,
        is_suspend: m.is_suspend,
    }
}

/// The phase-independent leaf of a `TypeRef`, resolved identically by every type resolver (signature
/// collection's [`ty_of_ref_with`], the checker's `resolve_ty`, and the lowerer's `ty_of`): a function
/// type `(A)->R`, a builtin scalar/`String`/`Unit`, or a primitive array (`IntArray`). Returns `None`
/// for a class / `Array<T>` / type-parameter reference — each phase resolves those against its own class
/// table — and does NOT apply nullability (the boxable-vs-error policy is phase-specific). `recurse`
/// resolves nested refs (a function type's parameters/return) through the caller's own resolver.
pub(crate) fn typeref_leaf(r: &TypeRef, recurse: &mut dyn FnMut(&TypeRef) -> Ty) -> Option<Ty> {
    if !r.fun_params.is_empty() || r.name == "<fun>" {
        let params: Vec<Ty> = r.fun_params.iter().map(&mut *recurse).collect();
        let ret = r.arg.as_ref().map(|a| recurse(a)).unwrap_or(Ty::Unit);
        return Some(if r.fun_suspend {
            Ty::fun_suspend(params, ret)
        } else {
            Ty::fun(params, ret)
        });
    }
    if let Some(t) = Ty::from_name(&r.name) {
        return Some(t);
    }
    Ty::primitive_array_element(&r.name).map(Ty::array)
}

/// Resolve a syntactic type reference to a `Ty`: a primitive/String/Unit, a declared class
/// (→ `Ty::Obj`), or a generic type parameter (erased per `TParams`, normally `Any`).
fn ty_of_ref(r: &TypeRef, classes: &ClassNames, tparams: &TParams, diags: &mut DiagSink) -> Ty {
    ty_of_ref_with(r, classes, tparams, &TypeRefCtx::default(), diags)
}

fn ty_of_ref_with(
    r: &TypeRef,
    classes: &ClassNames,
    tparams: &TParams,
    ctx: &TypeRefCtx,
    diags: &mut DiagSink,
) -> Ty {
    // Function type, builtin scalar, or primitive array — the leaf shared by every type resolver. A
    // function type is reference-typed, so the nullability handling below is a no-op for it.
    let base = if let Some(t) =
        typeref_leaf(r, &mut |x| ty_of_ref_with(x, classes, tparams, ctx, diags))
    {
        t
    } else if r.name == "Array" {
        match &r.arg {
            Some(a) => {
                let e = ty_of_ref_with(a, classes, tparams, ctx, diags);
                if e.is_reference() {
                    Ty::array(e)
                } else if e.boxed_ref().is_some() && !matches!(e, Ty::UInt | Ty::ULong) {
                    // `Array<Int>` is an array of BOXED `Integer` (`[Ljava/lang/Integer;`, distinct from
                    // the unboxed `IntArray` = `[I`). Model it as `Obj("kotlin/Array", [Int])` — the SAME
                    // logical form as `arrayOf(1)`/`Array(n){…}`, element read unboxed (the backend boxes).
                    Ty::obj_args("kotlin/Array", &[e])
                } else {
                    diags.error(
                        r.span,
                        "krusty: Array of this element type is not supported".to_string(),
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
        match ctx.class_literal_ty {
            Some(t) => t,
            None => {
                diags.error(
                    r.span,
                    "krusty: KClass is not available on this target".to_string(),
                );
                Ty::Error
            }
        }
    } else if tparams.contains(&r.name) {
        tparams.erase(&r.name) // erased generic type parameter (primitive if `<T: Int>`)
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
                .map(|a| ty_of_ref_with(a, classes, tparams, ctx, diags))
                .collect();
            Ty::obj_args(internal, &args)
        }
    } else {
        diags.error(r.span, format!("unresolved reference '{}'.", r.name));
        Ty::Error
    };
    // A nullable primitive is `Nullable(prim)` (it boxes to its wrapper at the backend boundary); a
    // non-boxable primitive (unsigned/value) is still rejected (skip, never miscompiled).
    if r.nullable && !base.is_reference() && base != Ty::Error {
        // `Unit?` is a nullable `kotlin/Unit` reference (`Unit.INSTANCE`/`null`), not a primitive.
        if base == Ty::Unit {
            return Ty::nullable(Ty::obj("kotlin/Unit"));
        }
        if let Some(nb) = base.nullable_boxed() {
            return nb;
        }
        diags.error(
            r.span,
            format!("nullable primitive type '{}?' is not supported", r.name),
        );
        return Ty::Error;
    }
    base
}

/// Result of typechecking a file: the type assigned to every expression node.
pub struct TypeInfo {
    pub expr_types: Vec<Ty>,
    /// Selected expression lowerings that cannot be recovered from the expression shape alone: classpath
    /// object value reads and classpath extension-property getter calls.
    pub expr_lowers: HashMap<ExprId, ExprLowering>,
    /// Selected statement lowerings that differ from the parser's generic statement shape.
    pub stmt_lowers: HashMap<StmtId, StmtLowering>,
    /// The RESOLVED type of a `val`/`var` local that carries an explicit type annotation, keyed by the
    /// `Stmt::Local`. The lowerer reuses this instead of re-resolving the annotation — so a library type
    /// the checker resolves through imports (`var res: Result<T>? = null`) keeps its (value-)class type
    /// instead of collapsing to the initializer's type. Absent for an inferred (no-annotation) local.
    pub local_decl_types: HashMap<StmtId, Ty>,
}

/// How to inline a receiver-lambda scope-function call (see [`InlineCall::ReceiverLambda`]).
#[derive(Clone, Copy, Debug)]
pub struct ReceiverLambda {
    /// The receiver expression — the lambda body's implicit `this`.
    pub receiver: ExprId,
    /// The lambda body expression (lowered with `this` bound to the receiver).
    pub body: ExprId,
    /// `true` for `apply`/`also` (the call yields the receiver), `false` for `run`/`with` (yields body).
    pub returns_receiver: bool,
}

/// Call-site selections whose legal lowering is an inline/custom emit form rather than the normal
/// function-call path.
#[derive(Clone, Debug)]
pub enum InlineCall {
    /// `Result.success(args)`: load the companion singleton and inline-splice the selected method.
    ValueCompanion(Box<crate::libraries::CompanionFn>),
    /// `x.run { ... }`, `x.apply { ... }`, or `with(x) { ... }`: evaluate the receiver once and lower
    /// the lambda body in the caller with that receiver bound as implicit `this`.
    ReceiverLambda(ReceiverLambda),
}

#[derive(Clone, Debug)]
pub enum ExprLowering {
    /// A call or function reference resolved to a local function declaration.
    LocalFunction { stmt_id: StmtId },
    /// A call whose selected lowering is an inline/custom emit form rather than the normal function-call
    /// path: value-class companion calls (`Result.success`) or receiver-lambda scope calls.
    InlineCall(InlineCall),
    /// Lambda literal resolution facts: receiver-function closure receiver, if any, and whether capture
    /// collection must stay shallow because the lambda is spliced by an inline call.
    Lambda(LambdaInfo),
    /// A classpath `object` used as a value. Lowering emits `getstatic <internal>.INSTANCE`.
    ObjectValue { internal: String },
    /// A bare-name call `m(args)` resolved to a MEMBER function of a classpath `object` that was imported
    /// unqualified (`import Obj.m; m()`). Kotlin dispatches this on the singleton, so lowering reads
    /// `getstatic <internal>.INSTANCE` as the receiver and invokes the member — the same shape a qualified
    /// `Obj.m(args)` produces (a receiver whose [`ObjectValue`] lowering names `internal`).
    ObjectMemberCall { internal: String },
    /// A `this@Label` that denotes the INNERMOST receiver (the current `this`), so it lowers as a bare
    /// `this`. Only recorded for the innermost match; an outer-receiver label is left unresolved/skipped.
    LabeledThisInner,
    /// A `this@Outer` denoting the IMMEDIATE enclosing class of an `inner class` — one class level up —
    /// so it lowers as the inner class's captured outer instance (`this.this$0`).
    LabeledThisOuter,
    /// A property-read `recv.name` resolved to a classpath extension property getter.
    ExtensionPropertyGet {
        getter: Box<crate::libraries::LibraryCallable>,
    },
    /// A Kotlin invoke-operator call (`a(args)`, equivalently `a.invoke(args)`) selected by the
    /// checker. The one convention covers both a function VALUE receiver (`Ty::Fun`, lowered to a
    /// direct function invocation) and a non-function receiver carrying a member `operator fun invoke`
    /// (lowered to that member call) — distinguished by [`InvokeKind`].
    Invoke {
        receiver: ExprId,
        params: Vec<Ty>,
        kind: InvokeKind,
    },
    /// A class literal `T::class` / `expr::class`. `unbound = Some(ty)` is an UNBOUND literal on a
    /// reference type name (`String::class`) — lowers to `ldc <ty>.class`. `unbound = None` is a BOUND
    /// literal on a value expression (`x::class`, `this::class`) — lowers to `expr.getClass()`. krusty
    /// models the result as `java/lang/Class` (its identity makes `==` agree with kotlinc's `KClass`).
    ClassLiteral { unbound: Option<Ty> },
}

/// How a selected [`ExprLowering::Invoke`] is realized: the receiver is either a function value or an
/// object whose member `invoke` operator is called.
#[derive(Clone, Debug)]
pub enum InvokeKind {
    /// Receiver is a function value (`Ty::Fun`); lowering emits a direct function invocation. `suspend`
    /// is the function type's suspend-ness: a `suspend (A)->R` value implements `Function{N+1}` (the
    /// trailing `Continuation`), so the call must thread the continuation and invoke `Function{N+1}`.
    Function { ret: Ty, suspend: bool },
    /// Receiver carries a member `operator fun invoke`; lowering calls that member.
    Operator { receiver_ty: Ty },
    /// Receiver has an `operator fun Recv.invoke(...)` EXTENSION (`"a"(12)` → `invoke("a", 12)`);
    /// lowering calls the lifted static extension with the receiver as the leading argument.
    ExtensionOperator { receiver_ty: Ty },
}

#[derive(Clone, Debug)]
pub struct LocalFunInfo {
    /// Unique JVM method name for the lifted local function.
    pub mangled: String,
    pub sig: Signature,
    /// Enclosing locals captured by the lifted function, ordered by name. A capture with `shared_cell`
    /// true must observe the same mutable cell as the enclosing scope; otherwise it is passed by value.
    pub captures: Vec<LocalCapture>,
}

#[derive(Clone, Debug)]
pub struct LocalCapture {
    pub name: String,
    pub ty: Ty,
    pub shared_cell: bool,
}

#[derive(Clone, Debug)]
pub enum StmtLowering {
    /// A lifted local function: mangled name, signature, and captures.
    LocalFunction(Box<LocalFunInfo>),
    /// A compound assignment (`target op= rhs`) selected as an in-place `opAssign` operator call.
    PlusAssign,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LambdaInfo {
    pub receiver: Option<Ty>,
    pub capture: LambdaCapture,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LambdaCapture {
    #[default]
    Closure,
    InlineSplice,
}

impl TypeInfo {
    pub fn ty(&self, e: ExprId) -> Ty {
        self.expr_types[e.0 as usize]
    }
    pub fn local_fun(&self, stmt_id: StmtId) -> Option<&LocalFunInfo> {
        match self.stmt_lowers.get(&stmt_id)? {
            StmtLowering::LocalFunction(info) => Some(info.as_ref()),
            _ => None,
        }
    }
}

struct Local {
    ty: Ty,
    is_var: bool,
}

fn is_nothing_ty(t: Ty) -> bool {
    match t {
        Ty::Nothing => true,
        Ty::Obj(n, _) => crate::jvm::jvm_class_map::to_jvm_internal(n) == "java/lang/Void",
        _ => false,
    }
}

fn make_checker<'a>(file: &'a File, syms: &'a SymbolTable, diags: &'a mut DiagSink) -> Checker<'a> {
    let imports = import_map(file);
    let import_wildcards =
        import_wildcards(file, syms.libraries.platform_default_import_packages());
    Checker {
        file,
        syms,
        diags,
        expr_types: vec![Ty::Error; file.expr_arena.len()],
        scopes: Vec::new(),
        ret_ty: Ty::Unit,
        imports,
        import_wildcards,
        tparams: Default::default(),
        this_ty: None,
        this_labels: Vec::new(),
        field_ty: None,
        companion_of: None,
        local_funs: Vec::new(),
        expr_lowers: HashMap::new(),
        inferred_fun_rets: HashMap::new(),
        inferred_ext_fun_rets: HashMap::new(),
        inferred_method_rets: HashMap::new(),
        stmt_lowers: HashMap::new(),
        local_decl_types: HashMap::new(),
        fn_reassigned: std::collections::HashSet::new(),
        expr_depth: 0,
        allow_lambda_mutation: false,
        loop_labels: Vec::new(),
    }
}

pub fn check_file(file: &File, syms: &mut SymbolTable, diags: &mut DiagSink) -> TypeInfo {
    // Pre-infer EXPRESSION-body return types (top-level functions AND class methods) and patch the
    // signature table BEFORE the main check — so a call to `fun m() = f()` resolves to its real return,
    // not the collection default `Unit`. Without this, a method whose return couldn't be inferred at
    // COLLECTION (an inherited-method-calling body in an anonymous object / hoisted local class → `Unit`)
    // is still `Unit` when a SIBLING call resolves it earlier in the same file
    // (`object { fun foo4() = foo3() }.apply { foo4() }`). A body that calls another expr-body method
    // declared LATER (forward reference) needs a second pass, so iterate to a FIXPOINT (bounded — the
    // dependency chain is shallow; an unresolvable case simply stops improving).
    for _pass in 0..8 {
        let mut scratch = DiagSink::new();
        let mut pre = make_checker(file, &*syms, &mut scratch);
        for &d in &file.decls {
            if let Decl::Fun(f) = file.decl(d) {
                if f.ret.is_none() && matches!(f.body, FunBody::Expr(_)) {
                    let resolve = class_internal_resolver(pre.syms);
                    pre.tparams =
                        TParams::from_decl_with(&f.type_params, &f.type_param_bounds, &resolve);
                    pre.check_fun(f);
                    pre.tparams.clear();
                }
            } else if let Decl::Class(cl) = file.decl(d) {
                let Some(internal) = pre.syms.classes.get(&cl.name).map(|s| s.internal.clone())
                else {
                    continue;
                };
                pre.this_ty = Some(Ty::obj(&internal));
                for m in &cl.methods {
                    if m.ret.is_none() && matches!(m.body, FunBody::Expr(_)) {
                        let resolve = class_internal_resolver(pre.syms);
                        pre.tparams =
                            TParams::from_decl_with(&m.type_params, &m.type_param_bounds, &resolve);
                        pre.check_method(m, &[]);
                        pre.tparams.clear();
                    }
                }
                pre.this_ty = None;
            }
        }
        let fun_rets = std::mem::take(&mut pre.inferred_fun_rets);
        let ext_rets = std::mem::take(&mut pre.inferred_ext_fun_rets);
        let method_rets = std::mem::take(&mut pre.inferred_method_rets);
        drop(pre);
        let mut changed = false;
        for ((name, params), ret) in fun_rets {
            if let Some(sig) = syms
                .funs
                .get_mut(&name)
                .and_then(|sigs| sigs.iter_mut().find(|s| s.params == params))
            {
                changed |= sig.ret != ret;
                sig.ret = ret;
            }
        }
        for ((recv, name), ret) in ext_rets {
            if let Some(sig) = syms.ext_funs.get_mut(&(recv, name)) {
                changed |= sig.ret != ret;
                sig.ret = ret;
            }
        }
        for ((internal, name, params), ret) in method_rets {
            if let Some(sig) = syms
                .class_by_internal_mut(&internal)
                .and_then(|c| c.methods.get_mut(&name))
                .filter(|s| s.params == params)
            {
                changed |= sig.ret != ret;
                sig.ret = ret;
            }
        }
        if !changed {
            break;
        }
    }

    let mut c = make_checker(file, &*syms, diags);
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

    // Each top-level declaration is checked in its OWN scope. Reset to the base depth (file-level
    // scope, e.g. top-level properties) before each one so a prior decl's leftover scope can't leak —
    // notably a function's locals must NOT be visible to a hoisted local class (`hoist_local_classes`)
    // checked afterward, or a captured outer name would wrongly resolve instead of skipping the file.
    let base_scope_depth = c.scopes.len();
    for &d in &file.decls {
        c.scopes.truncate(base_scope_depth);
        match file.decl(d) {
            Decl::Fun(f) => {
                let resolve = class_internal_resolver(c.syms);
                c.tparams = TParams::from_decl_with(&f.type_params, &f.type_param_bounds, &resolve);
                c.check_fun(f);
                c.tparams.clear();
            }
            Decl::Class(cl) => {
                // Duplicate primary-constructor parameter names are illegal (kotlinc reports a
                // conflicting declaration). `cl.props` holds every primary-ctor parameter (property
                // and plain) in order.
                {
                    let mut seen = std::collections::HashSet::new();
                    for pp in &cl.props {
                        if !seen.insert(pp.name.as_str()) {
                            c.diags.error(
                                cl.span,
                                format!("conflicting declaration: constructor parameter '{}' is declared more than once", pp.name),
                            );
                        }
                    }
                }
                // A `data class`'s primary-constructor parameters must all be `val`/`var` (kotlinc:
                // "data class primary constructor must have only property (val / var) parameters").
                if cl.is_data {
                    for pp in &cl.props {
                        if !pp.is_property {
                            c.diags.error(
                                cl.span,
                                format!("data class primary-constructor parameter '{}' must be 'val' or 'var'", pp.name),
                            );
                        }
                    }
                }
                // Duplicate class type-parameter names (`class C<T, T>`) are illegal.
                {
                    let mut seen = std::collections::HashSet::new();
                    for tp in &cl.type_params {
                        if !seen.insert(tp.as_str()) {
                            c.diags.error(
                                cl.span,
                                format!("conflicting declaration: type parameter '{tp}' is declared more than once"),
                            );
                        }
                    }
                }
                // Duplicate enum entry names (`enum class E { A, B, A }`) are illegal.
                {
                    let mut seen = std::collections::HashSet::new();
                    for entry in &cl.enum_entries {
                        if !seen.insert(entry.name.as_str()) {
                            c.diags.error(
                                cl.span,
                                format!("conflicting declaration: enum entry '{}' is declared more than once", entry.name),
                            );
                        }
                    }
                }
                // An `abstract` member is only allowed in an abstract class, an interface, or an enum
                // class (whose entries override the abstract member per-entry) — kotlinc rejects it in a
                // final class: "modifier 'abstract' is not applicable inside a final class".
                if !cl.is_abstract() && !cl.is_interface() && !cl.is_enum() {
                    for m in &cl.methods {
                        if m.is_abstract {
                            c.diags.error(
                                m.span,
                                format!(
                                    "abstract member '{}' is not allowed in a non-abstract class",
                                    m.name
                                ),
                            );
                        }
                    }
                }
                // `abstract` modifier consistency (illegal in any class kind): an abstract member has no
                // body, and cannot also be `final` (kotlinc rejects each).
                for m in &cl.methods {
                    if m.is_abstract && !matches!(m.body, FunBody::None) {
                        c.diags.error(
                            m.span,
                            format!("abstract member '{}' cannot have a body", m.name),
                        );
                    }
                    if m.is_abstract && m.is_final {
                        c.diags.error(
                            m.span,
                            format!("member '{}' cannot be both 'abstract' and 'final'", m.name),
                        );
                    }
                }
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
                // An annotation class emits as a JVM annotation interface + a synthetic impl whose
                // `equals`/`hashCode` are CONTENT-based (`Arrays.equals`/`Arrays.hashCode` for array
                // members) per the `java.lang.annotation.Annotation` contract — so an array member is
                // supported (no krusty-synthesized reference-equality member to miscompile).
                // Class type parameters are in scope for all members (erased; only function params
                // specialize — see `TParams::erased`).
                c.tparams = TParams::erased(&cl.type_params);
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
                // Push the enclosing-class labels for the duration of this class's member checks: the
                // OUTER chain first (`this@Outer` for an `inner class`, resolved via `this$0`), then the
                // class's own label (`this@C`) innermost. Walk `inner_of` outward.
                let mut label_depth = 0usize;
                {
                    let mut chain: Vec<(String, Ty)> = Vec::new();
                    let mut outer = cl.inner_of.clone();
                    while let Some(o) = outer {
                        let key = o.rsplit(['/', '$']).next().unwrap_or(&o).to_string();
                        if let Some(s) = syms.classes.get(&key) {
                            chain.push((key, Ty::obj(&s.internal)));
                            outer = s.inner_of.clone();
                        } else {
                            break;
                        }
                    }
                    for (n, ty) in chain.into_iter().rev() {
                        c.this_labels.push((n, ty, true));
                        label_depth += 1;
                    }
                }
                if let Some(ty) = c.this_ty {
                    c.this_labels.push((cl.name.clone(), ty, true));
                    label_depth += 1;
                }
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
                // Enum entry bodies (`ENTRY { val y = … ; override fun m() = y }`): each override is
                // checked like a method of the enum — `this` is the enum type, the enum's properties AND
                // the entry's own body properties are in scope, and the return type comes from the
                // abstract member it overrides.
                for entry in &cl.enum_entries {
                    // Type each entry-body property's initializer, then make it visible (as a member) to
                    // that entry's override methods.
                    let mut entry_props = props.clone();
                    if !entry.props.is_empty() {
                        c.push_scope();
                        for (n, t, v) in &props {
                            c.declare(n, *t, *v);
                        }
                        for bp in &entry.props {
                            let ty = match (&bp.ty, bp.init) {
                                (Some(r), _) => c.resolve_ty(r),
                                (None, Some(init)) => c.expr(init),
                                _ => Ty::Error,
                            };
                            if let (Some(r), Some(init)) = (&bp.ty, bp.init) {
                                let declared = c.resolve_ty(r);
                                let it = c.expr(init);
                                let sp = c.span(init);
                                c.expect_assignable(declared, it, sp, "property initializer");
                            }
                            c.declare(&bp.name, ty, bp.is_var);
                            entry_props.push((bp.name.clone(), ty, bp.is_var));
                        }
                        c.pop_scope();
                    }
                    for bm in &entry.methods {
                        c.check_method(bm, &entry_props);
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
                // Primary-constructor parameter defaults (`class A(val ctx: CoroutineContext =
                // EmptyCoroutineContext)`) are evaluated in the CALLER's context — they may not read other
                // ctor params (enforced in collect) and have no `this` — so check each in a fresh scope.
                // This records each default's sub-expression types and object-value references, which the
                // default-fill lowering needs (a call-site `A()` fill, and the `super(<defaults>)` synthesized
                // for a subclass or a typed `companion object : A()`). Mirrors `check_fun`'s param-default
                // pass so a function-typed parameter's lambda default types concretely.
                c.push_scope();
                for p in &cl.props {
                    if let Some(dx) = p.default {
                        let pty = c.resolve_ty(&p.ty);
                        let dty = if matches!(c.file.expr(dx), Expr::Lambda { .. })
                            && (!p.ty.fun_params.is_empty() || p.ty.name == "<fun>")
                        {
                            let lam_pts: Vec<Ty> =
                                p.ty.fun_params.iter().map(|r| c.resolve_ty(r)).collect();
                            c.check_lambda_with_types(dx, &lam_pts)
                        } else {
                            c.expr(dx)
                        };
                        c.expect_assignable(pty, dty, c.span(dx), "default argument");
                    }
                }
                c.pop_scope();
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
                // Interface-delegation expressions (`: I by mk(x)`) are evaluated in the constructor too,
                // so they're typed here — with the ctor params and `this` in scope.
                for (_iface, e) in &cl.delegation_exprs {
                    c.expr(*e);
                }
                for bp in &cl.body_props {
                    if let Some(init) = bp.init {
                        let it = c.expr(init);
                        if let Some(r) = &bp.ty {
                            let declared = c.resolve_ty(r);
                            c.expect_assignable(declared, it, c.span(init), "property initializer");
                        }
                    }
                    // A delegated member property's delegate expression (`by Del()`) — type-check it so
                    // its (and its sub-expressions') types are recorded for the `x$delegate` field lowering.
                    if let Some(de) = bp.delegate {
                        c.expr(de);
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
                for _ in 0..label_depth {
                    c.this_labels.pop();
                }
                c.this_ty = None;
                // Enum entry constructor arguments (e.g. `RED(0xff0000)`) are type-checked in a
                // fresh scope — they're emitted in the static `<clinit>` and cannot access `this`.
                if cl.is_enum() {
                    let ctor_tys: Vec<Ty> = cl.props.iter().map(|p| c.resolve_ty(&p.ty)).collect();
                    for entry in &cl.enum_entries {
                        for (a, expected_ty) in entry.args.iter().zip(&ctor_tys) {
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
                                    .get(&(rt.erased_recv(), p.name.clone()))
                                    .map(|(t, _)| *t)
                            })
                        })
                        .unwrap_or(Ty::Error);
                // A top-level computed property (`val g: T get() = …`) emits a `getG()` static method
                // (Phase: top-level computed). Type-check the getter body against the declared type. A
                // top-level backing-field property (`val x = init get() = field`) binds `field` to the
                // property type for the accessor body (like a member accessor).
                let has_backing_field = p.receiver.is_none() && p.init.is_some();
                if let Some(g) = &p.getter {
                    let prev = c.ret_ty;
                    let prev_field = c.field_ty;
                    c.ret_ty = prop_ty;
                    if has_backing_field {
                        c.field_ty = Some(prop_ty);
                    }
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
                    c.field_ty = prev_field;
                }
                // A setter body: an extension property's is checked with `this` = receiver; a top-level
                // backing-field property's binds `field` to the property type. Both bind the value param.
                if p.receiver.is_some() || has_backing_field {
                    if let Some(setter) = &p.setter {
                        if let Some(body) = &setter.body {
                            let prev = c.ret_ty;
                            let prev_field = c.field_ty;
                            c.ret_ty = Ty::Unit;
                            if has_backing_field {
                                c.field_ty = Some(prop_ty);
                            }
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
                            c.field_ty = prev_field;
                        }
                    }
                }
                c.this_ty = prev_this;
                // A delegated property's delegate expression (`by Del()`) must be type-checked so its
                // (and its sub-expressions') types are recorded for the lowering of `x$delegate`.
                if let Some(de) = p.delegate {
                    let _ = c.expr(de);
                }
                if let Some(init) = p.init {
                    let it = c.expr(init);
                    if let Some((declared, _, _)) = syms
                        .props
                        .get(&p.name)
                        .copied()
                        .filter(|(t, _, _)| *t != Ty::Error)
                    {
                        if p.ty.is_some() {
                            c.expect_assignable(declared, it, c.span(init), "property initializer");
                        }
                    }
                }
            }
        }
    }
    let Checker {
        expr_types,
        expr_lowers,
        inferred_fun_rets,
        inferred_ext_fun_rets,
        inferred_method_rets,
        stmt_lowers,
        local_decl_types,
        ..
    } = c;
    for ((name, params), ret) in inferred_fun_rets {
        if let Some(sig) = syms
            .funs
            .get_mut(&name)
            .and_then(|sigs| sigs.iter_mut().find(|s| s.params == params))
        {
            sig.ret = ret;
        }
    }
    for ((recv, name), ret) in inferred_ext_fun_rets {
        if let Some(sig) = syms.ext_funs.get_mut(&(recv, name)) {
            sig.ret = ret;
        }
    }
    for ((internal, name, params), ret) in inferred_method_rets {
        if let Some(sig) = syms
            .class_by_internal_mut(&internal)
            .and_then(|c| c.methods.get_mut(&name))
            .filter(|s| s.params == params)
        {
            sig.ret = ret;
        }
    }
    TypeInfo {
        expr_types,
        expr_lowers,
        stmt_lowers,
        local_decl_types,
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
    import_wildcards: Vec<String>,
    /// Generic type parameters in scope (erased to `java/lang/Object`).
    tparams: TParams,
    /// The type of `this` when checking class members (`None` at top level).
    this_ty: Option<Ty>,
    /// Stack of labeled receivers in scope, innermost LAST. Each entry is `(label, ty, is_class)`:
    /// a class body pushes `(ClassName, ty, true)`; a receiver lambda / scope function pushes
    /// `(fnName, receiverTy, false)`. Resolves `this@Label`. The innermost entry is the current `this`
    /// (lowered as a bare `this`); a `this@Label` matching the IMMEDIATE outer CLASS (one class level up,
    /// nothing but classes between) lowers via the inner class's `this$0`. Anything else type-checks but
    /// the lowerer skips it (it can't yet reach a captured / multi-level outer receiver).
    this_labels: Vec<(String, Ty, bool)>,
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
    expr_lowers: HashMap<ExprId, ExprLowering>,
    inferred_fun_rets: HashMap<(String, Vec<Ty>), Ty>,
    inferred_ext_fun_rets: HashMap<(Ty, String), Ty>,
    inferred_method_rets: HashMap<(String, String, Vec<Ty>), Ty>,
    stmt_lowers: HashMap<StmtId, StmtLowering>,
    local_decl_types: HashMap<StmtId, Ty>,
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
    /// The arg-binding call-resolution layer over this checker's [`SymbolSource`]. Cheap to construct.
    fn resolver(&self) -> crate::call_resolver::CallResolver<'_> {
        crate::call_resolver::CallResolver::new(&*self.syms.libraries)
    }
    /// Whether the current module declares a top-level function `name` (shadow-precedence test) — asked
    /// through the module source rather than touching `syms.funs` directly.
    fn module_declares(&self, name: &str) -> bool {
        crate::module_symbols::ModuleSymbols::new(self.syms).declares_top_level(name)
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
    fn update_lambda_info(&mut self, e: ExprId, update: impl FnOnce(&mut LambdaInfo)) {
        let mut info = match self.expr_lowers.remove(&e) {
            Some(ExprLowering::Lambda(info)) => info,
            Some(other) => {
                debug_assert!(
                    false,
                    "lambda metadata collided with a non-lambda expression lowering"
                );
                self.expr_lowers.insert(e, other);
                return;
            }
            None => LambdaInfo::default(),
        };
        update(&mut info);
        self.expr_lowers.insert(e, ExprLowering::Lambda(info));
    }
    fn mark_inline_lambda(&mut self, e: ExprId) {
        self.update_lambda_info(e, |info| info.capture = LambdaCapture::InlineSplice);
    }
    fn mark_receiver_lambda(&mut self, e: ExprId, receiver: Ty) {
        self.update_lambda_info(e, |info| info.receiver = Some(receiver));
    }
    fn mark_local_function_expr(&mut self, e: ExprId, stmt_id: StmtId) {
        self.expr_lowers
            .insert(e, ExprLowering::LocalFunction { stmt_id });
    }
    /// Select the Kotlin invoke-operator convention for `receiver(args)`. One entry point covers both
    /// a function-value receiver (`Ty::Fun`) and a non-function receiver with a member `operator fun
    /// invoke`, recording a single [`ExprLowering::Invoke`] and returning the call's result type.
    /// `None` only when the receiver is neither (the caller reports its own "not callable" error).
    fn record_invoke(
        &mut self,
        call: ExprId,
        receiver: ExprId,
        receiver_ty: Ty,
        args: &[ExprId],
        arg_tys: &[Ty],
        span: Span,
    ) -> Option<Ty> {
        let (params, ret, kind) = match receiver_ty {
            Ty::Fun(sig) => (
                sig.params.clone(),
                sig.ret,
                InvokeKind::Function {
                    ret: sig.ret,
                    suspend: sig.suspend,
                },
            ),
            _ => {
                // A member `operator fun invoke` (user class or a classpath/library type) → `Operator`.
                let member = crate::module_symbols::ModuleSymbols::new(self.syms)
                    .functions(CALLABLE_INVOKE_OPERATOR, Some(receiver_ty))
                    .overloads
                    .into_iter()
                    .find(|o| o.kind == crate::libraries::FnKind::Member)
                    .map(|o| (o.callable.params, o.callable.ret))
                    .or_else(|| {
                        crate::call_resolver::resolve_instance_member(
                            &*self.syms.libraries,
                            receiver_ty,
                            CALLABLE_INVOKE_OPERATOR,
                            arg_tys,
                        )
                        .map(|m| (m.member.params, m.ret))
                    });
                if let Some((params, ret)) = member {
                    (params, ret, InvokeKind::Operator { receiver_ty })
                } else {
                    // An `operator fun Recv.invoke(...)` EXTENSION (`"a"(12)`): logical params are the
                    // extension's own (`callable.params[0]` is the receiver). Lowered as a static call.
                    let fi = crate::module_symbols::ModuleSymbols::new(self.syms)
                        .functions(CALLABLE_INVOKE_OPERATOR, Some(receiver_ty))
                        .overloads
                        .into_iter()
                        .find(|o| {
                            o.kind == crate::libraries::FnKind::Extension
                                && o.receiver_rank == 0
                                // Select by arity (`callable.params[0]` is the receiver) so an
                                // overloaded `invoke()` / `invoke(Int)` picks the right one.
                                && o.callable.params.len() == arg_tys.len() + 1
                                // A `suspend operator fun …invoke` would need continuation threading the
                                // ExtensionOperator lowering doesn't do — leave it unresolved (skip).
                                && !o.flags.suspend
                        })?;
                    let params = fi.callable.params.get(1..).unwrap_or(&[]).to_vec();
                    (
                        params,
                        fi.callable.ret,
                        InvokeKind::ExtensionOperator { receiver_ty },
                    )
                }
            }
        };
        if params.len() != arg_tys.len() {
            self.diags.error(
                span,
                format!(
                    "invoke operator expects {} args, got {}",
                    params.len(),
                    arg_tys.len()
                ),
            );
        } else {
            for (i, (p, a)) in params.iter().zip(arg_tys).enumerate() {
                self.expect_assignable(*p, *a, self.span(args[i]), "argument");
            }
            self.expr_lowers.insert(
                call,
                ExprLowering::Invoke {
                    receiver,
                    params,
                    kind,
                },
            );
        }
        Some(ret)
    }
    fn local_function_expr_count(&self) -> usize {
        self.expr_lowers
            .values()
            .filter(|v| matches!(v, ExprLowering::LocalFunction { .. }))
            .count()
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

    fn local_capture_needs_shared_cell(&self, body: ExprId, name: &str) -> bool {
        let single: std::collections::HashSet<String> = std::iter::once(name.to_string()).collect();
        let written = {
            let mut out = std::collections::HashSet::new();
            collect_lambda_outer_writes(self.file, body, &single, &mut out);
            out.contains(name)
        };
        written
            || (self.fn_reassigned.contains(name)
                && self.lookup(name).map_or(false, |l| l.is_var)
                && local_fun_body_uses_any(self.file, body, &single))
    }

    /// Build a class reference type, carrying any generic arguments from the syntactic type
    /// (`C<A, …>` → `Ty::obj_args(internal, [A, …])`; raw → `Ty::obj`). Arguments erase in JVM
    /// descriptors but let the front end recover member/element types.
    /// If bare `name` resolves (through imports/defaults) to a CLASSPATH Kotlin `object`, its internal
    /// name — so the object can be referenced as a value (`getstatic <internal>.INSTANCE` in lowering).
    /// Resolve a dotted type/qualifier `Outer.Nested` (`Subject.User`, `SlugValidation.Ok`) to a
    /// classpath internal name (`lib/Subject$User`). The longest resolvable prefix names the outer
    /// type; the remaining segments are the nested path joined with `$` (kotlinc's nesting separator).
    /// Existence is verified via `resolve_type` so a bogus `A.B` stays unresolved (never a phantom `Obj`).
    /// A qualified nested-class CONSTRUCTOR call `Outer.Nested(...)` — the receiver names a TYPE (not a
    /// value in scope) and `Outer.Nested` resolves to a classpath nested class. The nested internal, so
    /// named/omitted-default argument mapping can reach the class's `@Metadata`. `None` when the receiver
    /// is a value (a normal `recv.method(...)` call) or the path is not a nested type.
    /// The leftmost simple name of a dotted `Name`/`Member` chain (`a.b.c` → `a`), or `None` if the
    /// chain bottoms out in something other than a bare name. Lets a fully-qualified path be told apart
    /// from a member access on a value by testing whether the root is a value in scope.
    fn dotted_root(&self, e: ExprId) -> Option<String> {
        match self.file.expr(e) {
            Expr::Name(n) => Some(n.clone()),
            Expr::Member { receiver, .. } => self.dotted_root(*receiver),
            _ => None,
        }
    }

    fn qualified_nested_ctor_internal(&self, receiver: ExprId, name: &str) -> Option<String> {
        // The receiver's leftmost segment must be a TYPE/PACKAGE, not a value in scope.
        let root = self.dotted_root(receiver)?;
        if self.lookup(&root).is_some() {
            return None;
        }
        match self.file.expr(receiver) {
            // `Outer.Nested(…)` — a nested type under an in-scope/imported outer type.
            Expr::Name(outer) => self.resolve_qualified_nested(&format!("{outer}.{name}")),
            // `a.b.Ctx(…)` — a FULLY-QUALIFIED constructor via a package PATH: the receiver `a.b` is a
            // package, `Ctx` a top-level class of it (`a/b/Ctx`), verified on the classpath.
            Expr::Member { .. } => {
                let internal = format!("{}/{name}", qualified_path(self.file, receiver)?);
                self.syms
                    .libraries
                    .resolve_type(&internal)
                    .map(|_| internal)
            }
            _ => None,
        }
    }

    fn resolve_qualified_nested(&self, name: &str) -> Option<String> {
        // A nested type under a resolvable outer type FIRST (`Subject.User` → `lib/Subject$User`) — an
        // in-scope type name shadows a package path, as kotlinc resolves it.
        if let Some((outer, rest)) = name.split_once('.') {
            let base = self
                .syms
                .classes
                .get(outer)
                .map(|c| c.internal.clone())
                .or_else(|| self.imported_type_internal(outer))
                .or_else(|| {
                    self.syms
                        .class_names
                        .get(outer)
                        .filter(|i| !i.starts_with("__ty/"))
                        .cloned()
                });
            if let Some(base) = base {
                let candidate = format!("{base}${}", rest.replace('.', "$"));
                if self.syms.libraries.resolve_type(&candidate).is_some() {
                    return Some(candidate);
                }
            }
        }
        // A fully-qualified PACKAGE path (`lib.Thing` → `lib/Thing`): the qualifier is a package, not a
        // type. Verified via `resolve_type`. Handles both a type reference (`x: lib.Thing?`) and a
        // qualified constructor call (`lib.Thing(5)`). `nested_internal` also recovers a DEEP FQN whose
        // tail names a NESTED type (`a.b.Outer.Inner` → `a/b/Outer$Inner`), which the flat slash form misses.
        let fq = name.replace('.', "/");
        self.nested_internal(&fq)
    }

    fn classpath_object_value(&self, name: &str) -> Option<String> {
        let internal = self.imported_type_internal(name)?;
        if self.syms.libraries.resolve_type(&internal)?.is_object() {
            Some(internal)
        } else {
            None
        }
    }

    /// Resolve a bare type `name` through this file's imports to an internal name that actually exists on
    /// the classpath — covering names the global simple-name index dropped as ambiguous (e.g.
    /// `Continuation`). Checks the explicit import first, then each wildcard-imported package. Existence
    /// is verified via the federated source's `resolve_type` (no guessing a non-existent `Obj`).
    fn imported_type_internal(&self, name: &str) -> Option<String> {
        if let Some(internal) = self.imports.get(name) {
            if let Some(resolved) = self.nested_internal(internal) {
                return Some(resolved);
            }
        }
        for pkg in &self.import_wildcards {
            let cand = wildcard_candidate(pkg, name);
            if self.syms.libraries.resolve_type(&cand).is_some() {
                return Some(cand);
            }
        }
        None
    }

    /// Resolve a dotted import flattened to slashes (`import lib.Scope.Ws` → `lib/Scope/Ws`) to the
    /// internal name that actually EXISTS on the classpath, treating trailing path segments as NESTED
    /// classes (`lib/Scope$Ws`). A nested-type import can't be told apart from a package path
    /// syntactically, so convert `/` → `$` from the RIGHT until `resolve_type` finds the class. Returns
    /// the input unchanged when it already resolves (the common package-qualified case).
    fn nested_internal(&self, internal: &str) -> Option<String> {
        resolve_nested_internal(internal, &*self.syms.libraries)
    }

    /// If a bare type name `n` denotes a reference type usable as an UNBOUND class literal `n::class`,
    /// its `Ty`. Resolves the same way [`Self::resolve_ty_no_diag`] does (built-in `from_name` types + user
    /// classes) — deliberately NOT via the global simple-name index, which falsely collides names like
    /// `IntArray` with unrelated JDK classes. A primitive (`Int::class` needs `Integer.TYPE`-style
    /// lowering) or unknown name returns `None` so the literal is skipped rather than miscompiled.
    fn class_literal_unbound_ty(&self, n: &str) -> Option<Ty> {
        if self.tparams.contains(n) {
            return None;
        }
        let ty = Ty::from_name(n)
            .or_else(|| self.syms.classes.get(n).map(|cs| Ty::obj(&cs.internal)))?;
        if ty == Ty::Error {
            return None;
        }
        if ty.is_reference() {
            return Some(ty);
        }
        // A primitive type literal (`Int::class`) is modeled by its boxed wrapper class (`Integer`), so it
        // compares equal to a bound literal on a value of that type (`42::class` → `Integer.getClass()`).
        // (Like the reference case, this is a `Class`-not-`KClass` approximation: `==`/`!=` agree with
        // kotlinc, but `.java.isPrimitive` would observe `Integer` where kotlinc's `KClass<Int>` reports
        // the primitive `int` — no corpus file exercises that, and the gate would flag it as a FAIL.)
        // Unsigned types box to an inline-class wrapper (`kotlin/UInt`), not a plain `java/lang/*` — skip.
        if matches!(ty, Ty::UInt | Ty::ULong) {
            return None;
        }
        ty.boxed_ref()
    }

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
        // Function type, builtin scalar, or primitive array — the leaf shared by every type resolver.
        let base = if let Some(t) = typeref_leaf(r, &mut |x| self.resolve_ty(x)) {
            t
        } else if r.name == "Array" {
            match &r.arg {
                Some(a) => {
                    let e = self.resolve_ty(a);
                    if e.is_reference() {
                        Ty::array(e)
                    } else if e.boxed_ref().is_some() && !matches!(e, Ty::UInt | Ty::ULong) {
                        // A boxed primitive `Array<Int>` = `Integer[]` — the SAME logical form as
                        // `arrayOf(1)`/`Array(n){…}` (`Obj("kotlin/Array", [Int])`, element read unboxed).
                        // Unsigned arrays box to their own inline-class wrapper — left unsupported.
                        Ty::obj_args("kotlin/Array", &[e])
                    } else {
                        Ty::Error
                    }
                }
                None => Ty::Error,
            }
        } else if self.tparams.contains(&r.name) {
            self.tparams.erase(&r.name) // erased generic type parameter (primitive if `<T: Int>`)
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
        } else if let Some(internal) = self.imported_type_internal(&r.name) {
            // An explicit/wildcard import resolves a name whose simple form is ABSENT from the global
            // index — either never registered or pruned because it's ambiguous across the whole classpath
            // (`Continuation` collides with `jdk/internal/vm/Continuation`). The import names the package.
            self.obj_with_targs(&internal, r)
        } else if let Some(internal) = self.resolve_qualified_nested(&r.name) {
            // A dotted CLASSPATH nested type (`Subject.User`, `SlugValidation.Ok`) → `Outer$Nested`.
            self.obj_with_targs(&internal, r)
        } else if let Some(internal) = {
            // An UNQUALIFIED reference to a sibling nested type within the enclosing class body (`Inner`
            // in `class Outer { class Inner }`) → `Outer$Inner` (Kotlin nested-type scoping). Reached only
            // when nothing else resolved, in a checker-only position (`val v: Inner`, `x as Inner`);
            // member SIGNATURE positions are covered by the collect_signatures class-scope extension.
            if let Some(Ty::Obj(outer, _)) = self.this_ty {
                let nested = format!("{outer}${}", r.name);
                self.syms
                    .classes
                    .values()
                    .find(|s| s.internal == nested)
                    .map(|s| s.internal.clone())
            } else {
                None
            }
        } {
            self.obj_with_targs(&internal, r)
        } else {
            Ty::Error
        };
        if r.nullable && !base.is_reference() && base != Ty::Error {
            if base == Ty::Unit {
                return Ty::nullable(Ty::obj("kotlin/Unit"));
            }
            if let Some(nb) = base.nullable_boxed() {
                return nb;
            }
            self.diags.error(
                r.span,
                format!("nullable primitive type '{}?' is not supported", r.name),
            );
            return Ty::Error;
        }
        base
    }

    /// The erased signature key of a function, using the type parameters currently in `self.tparams`
    /// plus the function's own. This is a semantic key, not a JVM descriptor string; JVM descriptor
    /// formatting belongs in the backend.
    fn erased_sig_key(&self, f: &FunDecl) -> ErasedSigKey {
        let extra: std::collections::HashSet<&str> =
            f.type_params.iter().map(|s| s.as_str()).collect();
        let key = |name: &str| -> ErasedTypeKey {
            if let Some(t) = Ty::from_name(name) {
                erased_type_key(t)
            } else if self.tparams.contains(name) || extra.contains(name) {
                erased_type_key(Ty::obj("kotlin/Any"))
            } else if let Some(cs) = self.syms.classes.get(name) {
                erased_type_key(Ty::obj(&cs.internal))
            } else {
                ErasedTypeKey::Unresolved(name.to_string())
            }
        };
        ErasedSigKey {
            name: f.name.clone(),
            receiver: f.receiver.as_ref().map(|r| key(&r.name)),
            params: f.params.iter().map(|p| key(&p.ty.name)).collect(),
        }
    }

    /// The non-null form of a nullable REFERENCE type (`A?` → `A`), for nullability-insensitive
    /// assignability — the checker erases `?` and a nullable reference shares its non-null JVM
    /// representation. A value/inline class (file or classpath, e.g. `kotlin/Result`) is LEFT nullable:
    /// like a primitive it has a distinct boxed-vs-unboxed representation that must stay distinguished.
    fn strip_nullable_ref(&self, t: Ty) -> Ty {
        let nn = t.non_null();
        if nn.is_reference()
            && !self.ty_is_value_class(nn)
            && self.syms.libraries.value_underlying(nn).is_none()
        {
            nn
        } else {
            t
        }
    }

    /// True if `t` is a `@JvmInline value class` reference type (carries a `value_field`).
    fn ty_is_value_class(&self, t: Ty) -> bool {
        matches!(t, Ty::Obj(n, _) if self.syms.class_by_internal(n).is_some_and(|c| c.value_field.is_some()))
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
            // A supertype method wants a `@JvmInline value class` return (unboxed), but the concrete
            // impl is inherited from a generic base and returns the erased `Object` (`fun foo(): T` over
            // `A<IC>`). The bridge would have to unbox `Object` → value class via the class's unbox-impl —
            // codegen krusty can't emit (VerifyError / AbstractMethodError). Skip rather than miscompile.
            if erased_type_key(ssig.ret) != erased_type_key(impl_sig.ret)
                && self.ty_is_value_class(ssig.ret)
                && impl_sig.ret == obj
            {
                self.diags.error(span, format!("krusty: method '{name}' needs a value-class unbox bridge from an erased generic return (unsupported)"));
                return;
            }
            let sp = erased_params_semantic_key(ssig);
            let ip = erased_params_semantic_key(&impl_sig);
            let params_differ = sp != ip;
            let ret_differs = erased_type_key(ssig.ret) != erased_type_key(impl_sig.ret);
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
                // Bridgeable generic/covariant override; IR lowering synthesizes the bridge from the
                // canonical symbol table when it has concrete function ids to delegate to.
            } else if params_differ {
                self.diags.error(span, format!("krusty: method '{name}' needs a bridge method (generic parameter override is not supported)"));
                return;
            } else if ret_differs {
                self.diags.error(span, format!("krusty: method '{name}' needs a bridge method (covariant/generic return override is not supported)"));
                return;
            }
        }
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
        let mut by_name: HashMap<String, ErasedSigKey> = HashMap::new(); // name → first erased key
        let mut seen: HashMap<ErasedSigKey, Span> = HashMap::new();
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
        // Subclasses of the sealed subject: a SAME-MODULE sealed class walks the user-class registry
        // (`subclasses_of`); a CLASSPATH sealed class reads its `@Metadata` `sealedSubclassFqName`
        // (`sealed_subclasses`), so `when (d) { is D.A -> …; is D.B -> … }` over a classpath sealed `D`
        // is proven exhaustive (an expression) the same way a same-module one is.
        let subs = match self.syms.class_by_internal(internal) {
            Some(cs) if cs.is_sealed => self.syms.subclasses_of(internal),
            Some(_) => return false,
            None => self.syms.libraries.sealed_subclasses(internal),
        };
        if subs.is_empty() {
            return false;
        }
        let mut covered: std::collections::HashSet<String> = std::collections::HashSet::new();
        for arm in arms {
            for &c in &arm.conditions {
                match self.file.expr(c) {
                    // `is Sub` — type-test arm.
                    Expr::Is {
                        ty, negated: false, ..
                    } => {
                        if let Ty::Obj(n, _) = self.resolve_ty_no_diag(ty) {
                            covered.insert(n.to_string());
                        }
                    }
                    // `Sub ->` — value arm naming a singleton object subclass (`object A : S`); a bare
                    // name resolving to a known class whose internal is one of the sealed subclasses.
                    Expr::Name(n) => {
                        if let Some(ci) = self.syms.classes.get(n) {
                            if subs.contains(&ci.internal) {
                                covered.insert(ci.internal.clone());
                            }
                        }
                    }
                    _ => {}
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
            Expr::Throw { .. }
            | Expr::Return { .. }
            | Expr::Break { .. }
            | Expr::Continue { .. } => true,
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

    /// Whether statement `s` always transfers control (so subsequent statements in its block are dead).
    fn stmt_diverges(&self, s: StmtId) -> bool {
        match self.file.stmt(s) {
            Stmt::Return(..) | Stmt::Break(_) | Stmt::Continue(_) => true,
            Stmt::Expr(e) => self.expr_diverges(*e),
            _ => false,
        }
    }

    /// Whether the block-body expression `e` transfers control out of the function on every path — a
    /// `return`/`throw`, a `Nothing`-typed call (`error(…)`/`TODO()`/any `Nothing`-returning fn), an
    /// `if` whose both branches do, a `when` whose every arm does, a `try` whose paths all do, or an
    /// infinite `while (true)`. Drives the block-body missing-return check.
    ///
    /// Deliberately conservative TOWARD `true`: a spurious `true` only fails to flag a genuine
    /// missing return (harmless), whereas a spurious `false` would reject a function that DOES return
    /// (a false error). So a subject-exhaustive `when` needs no explicit `else` here, and a
    /// `while (true)` is treated as non-terminating-fallthrough regardless of an inner `break`.
    fn body_terminates(&self, e: ExprId) -> bool {
        // A `Nothing`-typed expression never completes normally (the checker already resolved the
        // result type — no hardcoded intrinsic name list). `is_nothing_ty` recognizes both the
        // `Ty::Nothing` bottom and the `Obj("…/Void")` form the checker uses for a `Nothing` call.
        if is_nothing_ty(self.expr_types[e.0 as usize]) {
            return true;
        }
        match self.file.expr(e) {
            Expr::Return { .. } | Expr::Throw { .. } => true,
            Expr::Block { stmts, trailing } => {
                if stmts.iter().any(|s| self.stmt_terminates(*s)) {
                    return true;
                }
                trailing.is_some_and(|t| self.body_terminates(t))
            }
            Expr::If {
                then_branch,
                else_branch: Some(eb),
                ..
            } => self.body_terminates(*then_branch) && self.body_terminates(*eb),
            Expr::When { arms, .. } => {
                !arms.is_empty() && arms.iter().all(|a| self.body_terminates(a.body))
            }
            Expr::Try {
                body,
                catches,
                finally,
            } => {
                if finally.is_some_and(|f| self.body_terminates(f)) {
                    return true;
                }
                self.body_terminates(*body) && catches.iter().all(|c| self.body_terminates(c.body))
            }
            _ => false,
        }
    }

    /// Whether statement `s` guarantees the function returns/throws (for the missing-return check).
    fn stmt_terminates(&self, s: StmtId) -> bool {
        match self.file.stmt(s) {
            Stmt::Return(..) => true,
            Stmt::Expr(e) => self.body_terminates(*e),
            // `while (true) { … }` / `do { … } while (true)` never fall through (an inner `break`
            // only under-reports here, which is safe — it can't cause a false missing-return error).
            Stmt::While { cond, .. } | Stmt::DoWhile { cond, .. } => {
                matches!(self.file.expr(*cond), Expr::BoolLit(true))
            }
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
            return if r.fun_suspend {
                Ty::fun_suspend(params, ret)
            } else {
                Ty::fun(params, ret)
            };
        }
        if let Some(t) = Ty::from_name(&r.name) {
            t
        } else if self.tparams.contains(&r.name) {
            self.tparams.erase(&r.name)
        } else if let Some(cs) = self.syms.classes.get(&r.name) {
            Ty::obj(&cs.internal)
        } else if let Some(internal) = self
            .imported_type_internal(&r.name)
            .or_else(|| self.resolve_qualified_nested(&r.name))
        {
            // A CLASSPATH type (imported `is Ok`, or a qualified nested `is V.Ok`) — resolved the same way
            // `resolve_ty` resolves it, so an `is`/`as` smart-cast to a classpath sealed/open subclass
            // narrows (`val v: V; if (v is V.Ok) v.v`). Without this the type erased to `Ty::Error`, the
            // narrowing was dropped, and every member access on the smart-cast value failed ("member … on
            // <parent>").
            Ty::obj(&internal)
        } else if let Some(Ty::Obj(outer, _)) = self.this_ty {
            // A sibling nested type unqualified within the enclosing class body (`is Inner` in
            // `class Outer { class Inner }`) → `Outer$Inner`, so a nested-type `is`/`as` smart-cast
            // narrows. Mirrors the same fallback in `resolve_ty`.
            let nested = format!("{outer}${}", r.name);
            self.syms
                .classes
                .values()
                .find(|s| s.internal == nested)
                .map(|s| Ty::obj(&s.internal))
                .unwrap_or(Ty::Error)
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
                                if let Some(p) = l.ty.nullable_primitive() {
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
        let Expr::Name(n) = self.file.expr(operand).clone() else {
            return None;
        };
        // Only stable values (val/parameter) smart-cast soundly — a `var` could be reassigned.
        if matches!(self.lookup(&n), Some(l) if l.is_var) {
            return None;
        }
        let tt = self.resolve_ty_no_diag(&ty);
        // Narrow `x` to the tested type. A non-null `is T` → a reference `T`. A nullable `is T?` is only
        // narrowed for a PRIMITIVE (`is Double?` → `Double?`, which the numeric `==` conform compares;
        // `resolve_ty_no_diag` drops the `?`, so re-wrap with `Ty::nullable`). A nullable REFERENCE
        // (`is String?`) is NOT narrowed: `is String?` is true for `null` too, so asserting the non-null
        // erased type would be unsound for a later use.
        if ty.nullable {
            tt.boxed_ref().is_some().then(|| (n, Ty::nullable(tt)))
        } else if tt.is_reference() {
            Some((n, tt))
        } else {
            // A non-null primitive (`is Int`/`is Double`/`is Char`): narrow to the primitive so a
            // later USE unboxes — the lowerer's `Name` path coerces a reference slot to the narrowed
            // primitive (checkcast wrapper + unbox), and a boxed-FP `==` reached this way conforms
            // with IEEE semantics. Unsigned (`is UInt`) stays unnarrowed: its value-box unbox to the
            // `kotlin.UInt` type isn't modeled (krusty erases unsigned to `int`).
            (tt.is_numeric_or_char() || tt == Ty::Boolean).then_some((n, tt))
        }
    }

    /// Collect every smart-cast narrowing established by a `&&`-chain condition (`a && b && c`), recursing
    /// through nested `&&` so the operand to the right of the whole chain sees ALL of them (`x is Double?
    /// && y is Int? && x == y` narrows BOTH `x` and `y` for the `==`). A non-`&&` leaf contributes its own
    /// (positive) narrowing, if any.
    fn collect_and_narrowings(&self, cond: ExprId, out: &mut Vec<(String, Ty)>) {
        if let Expr::Binary {
            op: BinOp::And,
            lhs,
            rhs,
        } = self.file.expr(cond).clone()
        {
            self.collect_and_narrowings(lhs, out);
            self.collect_and_narrowings(rhs, out);
        } else if let Some(b) = self.smartcast_binding(cond, false) {
            out.push(b);
        }
    }

    fn check_fun(&mut self, f: &FunDecl) {
        // Duplicate parameter names are illegal (kotlinc reports a conflicting declaration). `_` is
        // not a valid function parameter name in Kotlin, so no placeholder exception is needed.
        {
            let mut seen = std::collections::HashSet::new();
            for p in &f.params {
                if !seen.insert(p.name.as_str()) {
                    self.diags.error(
                        f.span,
                        format!(
                            "conflicting declaration: parameter '{}' is declared more than once",
                            p.name
                        ),
                    );
                }
            }
        }
        // Duplicate type-parameter names (`fun <T, T> f()`) are illegal (conflicting declaration).
        {
            let mut seen = std::collections::HashSet::new();
            for tp in &f.type_params {
                if !seen.insert(tp.as_str()) {
                    self.diags.error(
                        f.span,
                        format!("conflicting declaration: type parameter '{tp}' is declared more than once"),
                    );
                }
            }
        }
        // At most one `vararg` parameter is allowed (kotlinc: multiple vararg parameters not allowed).
        if f.params.iter().filter(|p| p.is_vararg).count() > 1 {
            self.diags.error(
                f.span,
                "multiple vararg parameters are not allowed".to_string(),
            );
        }
        // A `reified` type parameter is only allowed on an `inline` function (kotlinc rejects it
        // otherwise — reification needs the body inlined at the call site).
        if !f.reified_type_params.is_empty() && !f.is_inline {
            self.diags.error(
                f.span,
                "'reified' type parameter is only allowed on an 'inline' function".to_string(),
            );
        }
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
            self.ret_ty = self
                .syms
                .ext_funs
                .get(&(recv_ty.erased_recv(), f.name.clone()))
                .map(|s| s.ret)
                .or_else(|| f.ret.as_ref().map(|r| self.resolve_ty(r)))
                .unwrap_or(Ty::Unit);
        } else {
            // Use this declaration's own collected overload's return type (matched by its erased
            // parameter descriptors when the name is overloaded); for a companion method (not in `funs`)
            // fall back to the declared return type.
            let want: Vec<ErasedTypeKey> = f
                .params
                .iter()
                .map(|p| erased_type_key(self.resolve_ty(&p.ty)))
                .collect();
            let own_ret = self.syms.funs.get(&f.name).and_then(|v| {
                if v.len() == 1 {
                    Some(v[0].ret)
                } else {
                    v.iter()
                        .find(|s| erased_params_semantic_key(s) == want)
                        .map(|s| s.ret)
                }
            });
            self.ret_ty = own_ret
                .or_else(|| f.ret.as_ref().map(|r| self.resolve_ty(r)))
                .unwrap_or(Ty::Unit);
        }
        // For expression-body functions with no explicit return type, infer the return type from the
        // body expression and write it back to the canonical signature table. Later call resolution and
        // lowering then consume the same `Signature::ret` field as annotated functions.
        let infer_ret =
            f.ret.is_none() && self.ret_ty == Ty::Unit && matches!(&f.body, FunBody::Expr(_));
        // Default arguments are evaluated in the caller's context (they may not read other params —
        // enforced in collect_signatures), so check each in a fresh scope and populate its types.
        self.push_scope();
        for p in &f.params {
            let pty = self.resolve_ty(&p.ty);
            if let Some(dx) = p.default {
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
            // Declare each parameter as we go, so a LATER parameter's default may reference an EARLIER one
            // (`fun f(a: Int, c: Int = a + 1)`) — Kotlin evaluates defaults left-to-right with preceding
            // parameters in scope. The default is realized inside the `$default` synthetic.
            let decl_ty = if p.is_vararg { Ty::array(pty) } else { pty };
            self.declare(&p.name, decl_ty, false);
        }
        self.pop_scope();
        self.push_local_funs();
        self.push_scope();
        let mut ptys: Vec<Ty> = Vec::with_capacity(f.params.len());
        for p in &f.params {
            let ty = self.resolve_ty(&p.ty);
            let ty = if p.is_vararg { Ty::array(ty) } else { ty };
            ptys.push(ty);
            self.declare(&p.name, ty, false);
        }
        if infer_ret {
            if let FunBody::Expr(e) = &f.body {
                let inferred = self.expr(*e);
                if inferred != Ty::Unit && inferred != Ty::Error {
                    self.ret_ty = inferred;
                    if let Some(recv_ref) = &f.receiver {
                        let recv_ty = self.resolve_ty(recv_ref);
                        self.inferred_ext_fun_rets
                            .insert((recv_ty.erased_recv(), f.name.clone()), inferred);
                    } else {
                        self.inferred_fun_rets
                            .insert((f.name.clone(), ptys.clone()), inferred);
                    }
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
            // A SIMPLE inline member (no type parameters — so no `reified` — and no function-type
            // parameter — so no lambda that must be spliced) is semantically an ordinary method;
            // inlining is only an optimization. Check + emit it as a normal method (member calls become
            // an ordinary invokevirtual). A generic/reified or lambda-taking inline member still needs
            // true call-site splicing, which member methods don't yet have → reject (never miscompile).
            let needs_real_inlining = !f.type_params.is_empty()
                || f.params.iter().any(|p| {
                    p.ty.name == "<fun>" || !p.ty.fun_params.is_empty() || p.ty.fun_suspend
                });
            if needs_real_inlining {
                self.diags
                    .error(f.span, "krusty: inline functions are not supported");
                return;
            }
        }
        self.fn_reassigned.clear();
        if let FunBody::Expr(b) | FunBody::Block(b) = &f.body {
            collect_all_reassigned(self.file, *b, &mut self.fn_reassigned);
        }
        let resolve = class_internal_resolver(self.syms);
        let added = self
            .tparams
            .insert_decl_with(&f.type_params, &f.type_param_bounds, &resolve);
        let object_contract_ret = match (f.name.as_str(), f.params.len()) {
            ("compareTo", 1) => Some(Ty::Int),
            ("equals", 1) => Some(Ty::Boolean),
            ("hashCode", 0) => Some(Ty::Int),
            ("toString", 0) => Some(Ty::String),
            _ => None,
        };
        self.ret_ty = f
            .ret
            .as_ref()
            .map(|r| self.resolve_ty(r))
            .unwrap_or_else(|| {
                if let Some(ret) = object_contract_ret {
                    return ret;
                }
                // For a method without an explicit return type (e.g. `override fun foo() = "Z"`),
                // use the return type that collect_signatures already inferred from the method body, or —
                // for an `override` of an inherited member (a base class OR an implemented interface, e.g.
                // an enum entry overriding an interface method) — that member's declared return type.
                if let Some(Ty::Obj(internal, _)) = self.this_ty {
                    if let Some(sig) = self
                        .syms
                        .class_by_internal(internal)
                        .and_then(|c| c.methods.get(&f.name))
                    {
                        return sig.ret;
                    }
                    if let Some((_, sig)) = self
                        .syms
                        .supertype_methods(internal)
                        .into_iter()
                        .find(|(n, _)| n == &f.name)
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
        let infer_ret =
            f.ret.is_none() && object_contract_ret.is_none() && matches!(&f.body, FunBody::Expr(_));
        if infer_ret {
            if let FunBody::Expr(e) = &f.body {
                let inferred = self.expr(*e);
                if inferred != Ty::Unit && inferred != Ty::Error {
                    self.ret_ty = inferred;
                    if let Some(Ty::Obj(internal, _)) = self.this_ty {
                        let params: Vec<Ty> = f
                            .params
                            .iter()
                            .map(|p| {
                                let ty = self.resolve_ty(&p.ty);
                                if p.is_vararg {
                                    Ty::array(ty)
                                } else {
                                    ty
                                }
                            })
                            .collect();
                        self.inferred_method_rets
                            .insert((internal.to_string(), f.name.clone(), params), inferred);
                    }
                }
            }
        } else {
            self.check_fun_body(f);
        }
        self.pop_scope();
        self.pop_scope();
        self.pop_local_funs();
        for t in added {
            self.tparams.remove(&t);
        }
    }

    fn check_fun_body(&mut self, f: &FunDecl) {
        if let FunBody::Expr(e) | FunBody::Block(e) = &f.body {
            if bc_complex_e(self.file, *e, false, false) {
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
                                       // A block-body function with a non-`Unit` (non-`Nothing`) return type must return a
                                       // value on every path — kotlinc rejects `fun f(): Int { }`. `body_terminates` errs
                                       // toward "returns", so this only fires on a genuinely non-returning body.
                if !matches!(self.ret_ty, Ty::Unit | Ty::Nothing | Ty::Error)
                    && !self.body_terminates(*e)
                {
                    self.diags.error(
                        f.span,
                        "a 'return' expression required in a function with a block body ('{...}')"
                            .to_string(),
                    );
                }
            }
            FunBody::None => {}
        }
    }

    /// Is `sub` a subtype of `sup`? Reflexive, through implemented interfaces, and up the base-class
    /// chain.
    fn obj_is_subtype(&self, sub: &str, sup: &str) -> bool {
        // Cheap exact-match before folding (covers the common same-name case without a map lookup).
        if sub == sup {
            return true;
        }
        // The Kotlin collection interfaces all map to one platform interface; compare on the canonical
        // names so `MutableList`/`List` (and a `kotlin/collections/List` vs a platform `java/util/List`)
        // are mutually assignable — the read-only/mutable distinction is enforced only at the `+=`
        // operator, not in general assignability. Also lets a `kotlin/collections/MutableList` reach
        // `java/util/Collection` via the hierarchy walk below. The aliases are seeded with the type
        // universe, so the checker does not call into the backend's name map here.
        let sub = self
            .syms
            .canonical_names
            .get(sub)
            .map_or(sub, String::as_str);
        let sup = self
            .syms
            .canonical_names
            .get(sup)
            .map_or(sup, String::as_str);
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

    /// Are two reference types comparable as `when`-subject value arms? A `when (s) { A -> … }` over a
    /// sealed subject `s: S` matches the *object* `A` (a subtype of `S`) by `==` — valid in Kotlin
    /// whenever one operand's type is a subtype of the other (the comparison can be non-trivially true).
    /// Only object/array reference types qualify; primitives go through `Ty::promote`.
    fn when_objs_comparable(&self, st: Ty, ct: Ty) -> bool {
        match (st, ct) {
            (Ty::Obj(a, _), Ty::Obj(b, _)) => {
                self.obj_is_subtype(a, b) || self.obj_is_subtype(b, a)
            }
            _ => false,
        }
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

    fn property_ref_ty(&self, arity: usize, mutable: bool) -> Option<Ty> {
        self.syms.libraries.property_reference_type(arity, mutable)
    }

    /// If `name` is a CLASSPATH class with a companion object, the companion instance's type
    /// (`Json` → `Ty::obj("…/Json$Default")`). A bare reference to such a class is its companion
    /// instance; member calls then resolve on the companion's type. `None` for a non-classpath name,
    /// a `__ty/` alias, or a class without a companion.
    /// The first explicit type argument of `call` (`decodeFromString<Foo>(…)` → `Foo`), resolved to a
    /// `Ty`, or `None` if the call carries none. Used to type a reified `<T> T` member's return.
    fn reified_type_arg(&self, call: ExprId) -> Option<Ty> {
        self.file
            .call_type_args
            .get(&call.0)
            .and_then(|ts| ts.first())
            .map(|r| self.resolve_ty_no_diag(r))
    }

    fn classpath_companion_ty(&self, name: &str) -> Option<Ty> {
        let internal = self.syms.class_names.get(name)?;
        if internal.starts_with("__ty/") {
            return None;
        }
        let internal = internal.to_string();
        let lt = self.syms.libraries.resolve_type(&internal)?;
        let (_, companion_ty) = lt.companion_object?;
        Some(Ty::obj(&companion_ty))
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

    /// Whether `internal` is a SAM interface krusty can soundly convert a lambda to: a user
    /// `fun interface` that is NON-generic and whose methods involve no value class. (A generic SAM
    /// erases its method to `Object` — the `LambdaMetafactory` descriptor `lower_lambda_sam` emits
    /// wouldn't match; a value-class method has a mangled name / boxing the path doesn't model; a
    /// library/Kotlin function interface is handled separately at the `Foo { … }` call site.)
    fn simple_fun_interface(&self, internal: &str) -> bool {
        let Some(c) = self.syms.class_by_internal(internal) else {
            return false;
        };
        // A generic fun interface is allowed: its method erases to `Object`, which the SAM descriptor
        // (built from the erased interface method) and the erased lambda parameter types both match. A
        // value-class method is still excluded (mangled name / boxing not modeled).
        c.is_fun_interface
            && c.methods.values().all(|sig| {
                !self.ty_is_value_class(sig.ret)
                    && sig.params.iter().all(|p| !self.ty_is_value_class(*p))
            })
    }

    /// The abstract-method parameter types of a simple fun interface — used to type a lambda being
    /// SAM-converted to it so its parameters resolve concretely (and the lowered impl matches the SAM
    /// descriptor). `None` unless the interface has exactly one method.
    fn fun_interface_sam_params(&self, internal: &str) -> Option<Vec<Ty>> {
        let c = self.syms.class_by_internal(internal)?;
        if c.methods.len() == 1 {
            Some(c.methods.values().next().unwrap().params.clone())
        } else {
            None
        }
    }

    /// Check call arguments against a parameter list. For a `vararg`, the fixed parameters match
    /// positionally and every trailing argument matches the vararg array's ELEMENT type (`f(vararg s:
    /// T)` accepts `f(a, b)` with `a, b: T`); a single array argument is also accepted (a spread). For a
    /// non-`vararg` list the arguments match positionally.
    fn expect_call_args(&mut self, params: &[Ty], vararg: bool, args: &[ExprId], arg_tys: &[Ty]) {
        if vararg && !params.is_empty() {
            let n_fixed = params.len() - 1;
            let array_param = params[n_fixed];
            let elem = array_param.array_elem().unwrap_or(array_param);
            for (i, a) in arg_tys.iter().enumerate() {
                if i >= args.len() {
                    break;
                }
                // A lone argument already OF the array type is a spread/pass-through, not an element.
                let expected = if i < n_fixed {
                    params[i]
                } else if arg_tys.len() - n_fixed == 1 && *a == array_param {
                    array_param
                } else {
                    elem
                };
                self.expect_assignable(expected, *a, self.span(args[i]), "argument");
            }
        } else {
            for (i, (p, a)) in params.iter().zip(arg_tys).enumerate() {
                self.expect_assignable(*p, *a, self.span(args[i]), "argument");
            }
        }
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
        // The checker erases nullability (`resolve_ty` drops the `?`), so most types are already non-null;
        // a nullable REFERENCE that DOES reach here (an `as?` result, a nullable member return) shares the
        // JVM representation of its non-null form, so compare the non-null forms — runtime null-safety is
        // enforced by lowering (`!!`/`?.`), not here. A nullable PRIMITIVE (`Int?`) is NOT stripped: it
        // boxes to a wrapper, a real representation difference the dedicated `nullable_primitive` rule
        // below (and the emit coercion site) must keep distinct from the bare primitive.
        // Only in a RETURN position (an expression body / getter): a body like `= x as? A` yields a
        // nullable reference assignable to the declared non-null-erased return. Elsewhere keep the strict
        // comparison so a genuinely distinct nullable assignment isn't silently accepted.
        let (expected, actual) =
            if matches!(ctx, "function body" | "getter body" | "local function body") {
                (
                    self.strip_nullable_ref(expected),
                    self.strip_nullable_ref(actual),
                )
            } else {
                (expected, actual)
            };
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
        // (`Int` → `Int?`). The box (`Integer.valueOf`) is the emit site's job.
        if expected.nullable_primitive() == Some(actual) {
            return;
        }
        // In Kotlin every type is a subtype of `Any`/`Object`, and the top type narrows back to a
        // specific type by an unchecked cast. Both directions are assignable; the primitive-vs-boxed
        // *representation* (and any box/unbox or checkcast) is the backend's concern, decided at the
        // emit coercion site — not the type checker's. `Unit` IS a subtype of `Any` (`Unit.INSTANCE`);
        // the lowerer's arg/return coercion materializes the singleton.
        if expected == Ty::obj("kotlin/Any") {
            return;
        }
        if actual == Ty::obj("kotlin/Any") && expected != Ty::Unit {
            return;
        }
        // `Unit` (the non-reference modeling) and `kotlin/Unit` (its singleton reference form) are the same
        // type — `fun f(): Unit? = unitExpr` assigns a `Unit` value to a `kotlin/Unit` slot. The lowerer's
        // arg/return coercion materializes `Unit.INSTANCE`.
        if (actual == Ty::Unit && expected == Ty::obj("kotlin/Unit"))
            || (expected == Ty::Unit && actual == Ty::obj("kotlin/Unit"))
        {
            return;
        }
        // A primitive flowing into a reference supertype is checked through its boxed source type; the
        // provider's type hierarchy decides whether that box implements `Number`, `Comparable`, etc.
        if let (Some(Ty::Obj(b, _)), Ty::Obj(e, _)) = (actual.boxed_ref(), expected) {
            if self.obj_is_subtype(b, e) {
                return;
            }
        }
        // String is a reference type with classpath supertypes; ask the same hierarchy walker instead of
        // keeping a local list of platform interfaces.
        if actual == Ty::String {
            if let Ty::Obj(e, _) = expected {
                if self.obj_is_subtype("kotlin/String", e) {
                    return;
                }
            }
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
        if let Ty::Fun(e) = &expected {
            if self.syms.libraries.function_like_arity(actual) == Some(e.params.len()) {
                return;
            }
        }
        // SAM conversion: a function value (lambda) is assignable to a simple `fun interface` — the
        // lowering builds an instance whose single abstract method runs the lambda.
        if matches!(actual, Ty::Fun(_)) {
            if let Some(internal) = expected.obj_internal() {
                if self.simple_fun_interface(internal) {
                    return;
                }
            }
        }
        // `Array<T>` reference-element covariance (JVM: `Integer[]` is-a `Object[]`): `Array<Array<Int>>`
        // → `Array<Array<*>>` (a `*`/`out` element erases to `Any?`). Recurse on the element type.
        if self.array_covariant_assignable(expected, actual) {
            return;
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

    /// `Array<A>` is assignable to `Array<E>` when the elements are — JVM reference arrays are covariant
    /// (`Integer[]` is-a `Object[]`), and a `*`/`out` projection erases the expected element to `Any?`.
    /// Returns `false` for non-array types (the caller's other rules decide those).
    fn array_covariant_assignable(&self, expected: Ty, actual: Ty) -> bool {
        let (Ty::Array(e), Ty::Array(a)) = (expected, actual) else {
            return false;
        };
        self.elem_covariant_assignable(*e, *a)
    }

    /// Element-level covariance for [`array_covariant_assignable`]: equal types, nested arrays, an `Any`/
    /// `Any?` (star) expected element accepting anything, or a reference-subtype element.
    fn elem_covariant_assignable(&self, expected: Ty, actual: Ty) -> bool {
        if expected == actual {
            return true;
        }
        if let (Ty::Array(e), Ty::Array(a)) = (expected, actual) {
            return self.elem_covariant_assignable(*e, *a);
        }
        // A star projection (`*`) or `in`/`out` variance erases the element to `Any?` on whichever side
        // it appears; either way the array assignment is JVM-sound (reference-array covariance), so a
        // `kotlin/Any` element on EITHER side accepts the other.
        let exp = expected.non_null();
        let act = actual.non_null();
        if exp.obj_internal() == Some("kotlin/Any") || act.obj_internal() == Some("kotlin/Any") {
            return true;
        }
        // Otherwise require the actual element to be a reference subtype of the expected element.
        match (
            exp.obj_internal(),
            actual
                .non_null()
                .boxed_ref()
                .and_then(|b| b.obj_internal())
                .or_else(|| actual.non_null().obj_internal()),
        ) {
            (Some(e), Some(a)) => self.obj_is_subtype(a, e),
            _ => false,
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
                // `null!!` (a statically-null operand) ALWAYS throws — its type is `Nothing`, so code
                // after it is dead (otherwise the dead path emits an unframed `aload`/store → VerifyError).
                if t == Ty::Null {
                    return self.set(e, Ty::Nothing);
                }
                t.nullable_primitive().unwrap_or(t)
            }
            Expr::Throw { operand } => {
                self.expr(operand); // any reference (a Throwable) — krusty doesn't model the hierarchy
                Ty::Nothing
            }
            Expr::Return { value, .. } => {
                if let Some(v) = value {
                    self.expr(v);
                }
                Ty::Nothing
            }
            // `break`/`continue` in expression position (`m[k] ?: continue`) — a jump; bottom type
            // `Nothing` (like `return`/`throw`), so it fits any expected type (the elvis result type is
            // the non-null LHS).
            Expr::Break { .. } | Expr::Continue { .. } => Ty::Nothing,
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
                let lc_before = self.local_function_expr_count();
                let bret = self.expr(body);
                // A non-inlined lambda that calls a local function would dispatch it on the lambda
                // class (the local fun lives on the enclosing facade/class) — reject rather than
                // miscompile (the recursive nested-closure case).
                if self.local_function_expr_count() > lc_before {
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
                    if let Some(ret) = crate::call_resolver::resolve_instance_member(
                        &*self.syms.libraries,
                        at,
                        "get",
                        &[it],
                    )
                    .map(|m| m.ret)
                    {
                        return self.set(e, ret);
                    }
                }
                // `m[i]` on a USER class with an `operator fun get(index)` → `m.get(i)`.
                if let Ty::Obj(internal, _) = at {
                    if let Some(sig) = self.syms.method_of(internal, "get") {
                        if sig.params.len() == 1 {
                            self.expect_assignable(sig.params[0], it, self.span(index), "index");
                            return self.set(e, sig.ret);
                        }
                    }
                }
                // `coll[i]` on a library type → the `get(index)` operator member (`List.get(Int)`,
                // `Map.get(K)`); the index type is checked against the member's parameter.
                if let Ty::Obj(..) = at {
                    if let Some(m) = crate::call_resolver::resolve_instance_member(
                        &*self.syms.libraries,
                        at,
                        "get",
                        &[it],
                    ) {
                        return self.set(e, m.ret);
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
                // A nested try + finally only mis-emits when the inlined finally is re-entered: either a
                // finally that diverges (its `throw`/`return` on a return path re-enters the enclosing
                // handler → the finally runs twice) or a `catch` in the nest (the catch's exception path
                // re-runs the inlined finally). Plain nested try + non-diverging finally with no catch
                // emits correctly, so allow it; reject only the re-entrant shapes.
                let reentrant = expr_try_finally_has_return(self.file, e)
                    || expr_has_finally_with_try(self.file, e);
                if nested && any_finally && reentrant {
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
                        // A catch type SHOULD be a `Throwable` subtype, but krusty's exception-hierarchy
                        // walk is incomplete (`NotImplementedError` and other stdlib errors don't chain
                        // to `Throwable`), so enforcing it here false-rejects valid catches. Deferred
                        // until the hierarchy is complete.
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
                    || matches!(
                        tt,
                        Ty::Int
                            | Ty::Byte
                            | Ty::Short
                            | Ty::Long
                            | Ty::Boolean
                            | Ty::Char
                            | Ty::Float
                            | Ty::Double
                    );
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
                nullable,
            } => {
                let ot = self.expr(operand);
                let tt = self.resolve_ty(&ty);
                // `Unit` is the reference type `kotlin/Unit` at the JVM — normalize so a cast to/from it
                // (`println() as Any`, `x as Unit`, `4 as? Unit`, `foo() as? Int`) uses the reference-cast
                // paths below rather than being rejected (`Ty::Unit.is_reference()` is false).
                let ot = if ot == Ty::Unit {
                    Ty::obj("kotlin/Unit")
                } else {
                    ot
                };
                let tt = if tt == Ty::Unit {
                    Ty::obj("kotlin/Unit")
                } else {
                    tt
                };
                // `checkcast` needs a reference operand. The target is either a *known* reference type (an
                // unresolved one erases to a no-op `Object` cast — rejected), or a non-unsigned primitive:
                // `x as Int` on a reference operand is an unbox (`checkcast Integer; intValue()`).
                let prim_unbox = ot.is_reference() && tt.boxed_ref().is_some();
                // A PRIMITIVE operand cast to a CONCRETE reference type (`42 as Any`, `'a' as Char?`,
                // `b as Byte?`) is a BOX — but only when the operand's wrapper is actually assignable
                // to the target (`Any`/`Object`, the wrapper itself, or a supertype like `Number`).
                // An impossible cast (`1 as String`) is NOT boxed — boxing an `Integer` into a `String`
                // slot is a VerifyError; reject it (skip), as kotlinc rejects it at compile time. A
                // type-parameter target (`56 as T`) is excluded too — the boxed value would flow into
                // an erased/bridged generic slot krusty doesn't reconcile.
                let prim_box = ot.boxed_ref().is_some()
                    && tt.is_reference()
                    && !self.tparams.contains(&ty.name)
                    // `'a' as Char?` / `42 as Int?` — box to the operand's own nullable form …
                    && (tt.nullable_primitive() == Some(ot)
                        // … or `42 as Any` / `42 as Number` — box to a supertype of the operand's boxed
                        // class (`Int` → `kotlin/Int`). Subtyping resolves the JVM wrapper hierarchy
                        // (`kotlin/Int` <: `Number` <: `Any`); the wrapper realization is the backend's.
                        || ot.boxed_ref().and_then(|b| b.obj_internal()).is_some_and(|bw| {
                            tt.obj_internal()
                                .is_some_and(|t| {
                                    matches!(t, "kotlin/Any" | "java/lang/Object")
                                        || self.obj_is_subtype(bw, t)
                                })
                        }));
                // A SAFE cast of a PRIMITIVE operand (`1 as? Byte`, `1.0 as? Int`): box the operand to its
                // wrapper, then `instanceof` the target wrapper/class — `null` on a mismatch (an `Int` box
                // is not a `Byte`). Sound for any known target (reference or boxable primitive).
                let prim_operand_safe_cast = nullable
                    && ot.boxed_ref().is_some()
                    // A value/unsigned-class operand (`1U as? Int`) boxes to its OWN wrapper
                    // (`kotlin/UInt`, not `Integer`) — its `instanceof` against the target wrapper
                    // is not the plain-primitive shape the lowerer boxes, so leave it skipped.
                    && !self.syms.libraries.is_unsigned_integer_type(ot)
                    && !self.ty_is_value_class(ot)
                    && (tt.is_reference() || tt.boxed_ref().is_some());
                if (!(tt.is_reference() || prim_unbox)
                    || (!ot.is_reference() && !prim_box && ot != Ty::Error))
                    && !prim_operand_safe_cast
                {
                    self.diags.error(
                        self.span(e),
                        "krusty: 'as' with this type is not supported".to_string(),
                    );
                    return Ty::Error;
                }
                // A safe cast `x as? T` has type `T?` — `null` on a runtime mismatch. For a primitive `T`
                // (`as? Int`) that nullable form is the boxed wrapper (`Int?`). A plain `x as T` keeps `T`.
                // A value/inline-class target keeps `T` (non-null): its nullable boxed form isn't modeled,
                // and a member access on the cast result must see the unboxed value-class type.
                let is_value = self.ty_is_value_class(tt)
                    || self.syms.libraries.value_underlying(tt).is_some();
                if nullable && !is_value {
                    Ty::nullable(tt)
                } else {
                    tt
                }
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
                match (lt, rt) {
                    (Ty::Char, Ty::Char) => Ty::obj("kotlin/ranges/CharRange"),
                    // Unsigned ranges are their own stdlib classes (`UIntRange`/`ULongRange`), iterated
                    // with unsigned comparison and mangled inline-class getters.
                    (Ty::UInt, Ty::UInt) => Ty::obj("kotlin/ranges/UIntRange"),
                    (Ty::ULong, Ty::ULong) => Ty::obj("kotlin/ranges/ULongRange"),
                    _ if lt.is_int_range_operand() && rt.is_int_range_operand() => {
                        Ty::obj("kotlin/ranges/IntRange")
                    }
                    _ if (lt.is_int_range_operand() || lt == Ty::Long)
                        && (rt.is_int_range_operand() || rt == Ty::Long) =>
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
                        .or_else(|| self.syms.props.get(&name).map(|&(t, v, _)| (t, v)))
                    {
                        Some((_, is_var)) => {
                            if !is_var {
                                self.diags
                                    .error(self.span(e), "'val' cannot be reassigned.".to_string());
                            }
                            if !tt.is_numeric_or_char() {
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
                let lt = lt0.nullable_primitive().unwrap_or(lt0);
                // A `Unit`-coerced elvis (`x ?: someUnitExpr`) trips a StackMapTable mismatch in
                // codegen (the branches push incompatible stack shapes) — skip rather than VerifyError.
                if rt == Ty::Unit {
                    self.diags.error(
                        self.span(e),
                        "krusty: elvis with a Unit right-hand side is not supported".to_string(),
                    );
                }
                if rt == Ty::Nothing {
                    match lt0 {
                        Ty::Nullable(inner) => *inner,
                        _ => lt,
                    }
                } else if lt == Ty::Null {
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
                    if let Some(fi) = crate::module_symbols::ModuleSymbols::new(self.syms)
                        .functions(&name, Some(rt))
                        .overloads
                        .into_iter()
                        .find(|o| {
                            o.kind == crate::libraries::FnKind::Extension && o.receiver_rank == 0
                        })
                    {
                        let logical: Vec<Ty> = fi.callable.params[1..].to_vec();
                        let arg_tys: Vec<Ty> = match &args {
                            Some(a) => a.iter().map(|x| self.expr(*x)).collect(),
                            None => vec![],
                        };
                        if logical.len() != arg_tys.len() {
                            self.diags.error(
                                self.span(e),
                                format!(
                                    "extension '{name}' expects {} args, got {}",
                                    logical.len(),
                                    arg_tys.len()
                                ),
                            );
                        }
                        return self.set(e, fi.callable.ret);
                    }
                }
                // A safe-call scope function (`s?.let { it… }`, `s?.run { … }`): the receiver is non-null
                // inside; type it like the non-safe form, then wrap the result nullable below.
                let result = if let Some(t) = self.safe_scope_call_result(rt, &name, &args) {
                    t
                } else {
                    match &args {
                        None => self.check_member(rt, &name, self.span(e), Some(e)),
                        Some(a) => {
                            let arg_tys: Vec<Ty> = a.iter().map(|x| self.expr(*x)).collect();
                            let inline_arg_supported = !a
                                .iter()
                                .any(|x| matches!(self.file.expr(*x), Expr::CallableRef { .. }));
                            if let ("toString", []) = (name.as_str(), arg_tys.as_slice()) {
                                Ty::String
                            } else if let ("hashCode", []) = (name.as_str(), arg_tys.as_slice()) {
                                Ty::Int // Int (not a reference), so safe-call rejection fires below
                            } else if rt == Ty::String {
                                crate::call_resolver::resolve_instance_member(
                                    &*self.syms.libraries,
                                    rt,
                                    &name,
                                    &arg_tys,
                                )
                                .map(|m| m.ret)
                                .or_else(|| self.library_extension_return(&name, rt, &arg_tys, &[]))
                                .or_else(|| {
                                    inline_arg_supported
                                        .then(|| {
                                            self.library_extension_inline_return(
                                                &name, rt, &arg_tys,
                                            )
                                        })
                                        .flatten()
                                })
                                .unwrap_or(Ty::Error)
                            } else if let Ty::Obj(internal, _) = rt {
                                crate::module_symbols::ModuleSymbols::new(self.syms)
                                    .functions(&name, Some(rt))
                                    .overloads
                                    .into_iter()
                                    .find(|o| o.kind == crate::libraries::FnKind::Member)
                                    .map(|fi| fi.callable.ret)
                                    .or_else(|| {
                                        crate::call_resolver::resolve_instance(
                                            &*self.syms.libraries,
                                            internal,
                                            &name,
                                            &arg_tys,
                                        )
                                        .map(|m| m.ret)
                                    })
                                    .or_else(|| {
                                        self.library_extension_return(&name, rt, &arg_tys, &[])
                                    })
                                    .or_else(|| {
                                        inline_arg_supported
                                            .then(|| {
                                                self.library_extension_inline_return(
                                                    &name, rt, &arg_tys,
                                                )
                                            })
                                            .flatten()
                                    })
                                    .unwrap_or(Ty::Error)
                            } else {
                                Ty::Error
                            }
                        }
                    }
                };
                // A same-module EXTENSION via safe call (`s?.id()` where `fun String.id()` is declared in
                // this module): the member/classpath/library lookups above don't see module extensions, so
                // resolve it here on the non-null receiver. The lowerer emits the static extension call.
                let result = if result == Ty::Error {
                    let arg_tys: Vec<Ty> = match &args {
                        Some(a) => a.iter().map(|x| self.expr(*x)).collect(),
                        None => vec![],
                    };
                    crate::module_symbols::ModuleSymbols::new(self.syms)
                        .functions(&name, Some(rt.non_null()))
                        .overloads
                        .into_iter()
                        .find(|o| {
                            o.kind == crate::libraries::FnKind::Extension
                                && o.callable.params.len() == arg_tys.len() + 1
                        })
                        .map(|fi| fi.callable.ret)
                        .unwrap_or(Ty::Error)
                } else {
                    result
                };
                // The safe-call result is nullable: a primitive member result becomes `Int?`
                // (`s?.length`); the member value is boxed (or `null`) in lowering. A non-boxable
                // primitive (unsigned/value) stays unsupported.
                if !result.is_reference() && result != Ty::Error {
                    if result == Ty::Unit {
                        // `x?.let { … }` whose lambda body is `Unit` (a `for`/statement body): the safe call
                        // is `Unit?` — `null` when the receiver is null, else `Unit`. The lowerer runs the
                        // member for effect and yields `Unit.INSTANCE`/`null` (both references).
                        return self.set(e, Ty::Unit);
                    }
                    if let Some(nb) = result.nullable_boxed() {
                        return self.set(e, nb);
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
            // `this@Label` — a labeled receiver. Resolve from the receiver-label stack (innermost last):
            // the matching entry's type is the result. The INNERMOST match (the current `this`) records
            // `LabeledThisInner` (lowered as a bare `this`); a match exactly ONE class level up, with
            // both ends classes, records `LabeledThisOuter` (lowered via the inner class's `this$0`).
            // Any other (captured / multi-level / cross-lambda) match type-checks but the lowerer skips.
            Expr::Name(n) if n.starts_with("this@") => {
                let label = &n["this@".len()..];
                match self.this_labels.iter().rposition(|(l, _, _)| l == label) {
                    Some(idx) => {
                        let ty = self.this_labels[idx].1;
                        let top = self.this_labels.len() - 1;
                        if idx == top {
                            self.expr_lowers.insert(e, ExprLowering::LabeledThisInner);
                        } else if idx + 1 == top
                            && self.this_labels[idx].2
                            && self.this_labels[top].2
                        {
                            self.expr_lowers.insert(e, ExprLowering::LabeledThisOuter);
                        }
                        ty
                    }
                    None => {
                        self.diags
                            .error(self.span(e), format!("unresolved reference '{n}'."));
                        Ty::Error
                    }
                }
            }
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
                    if self.syms.objects.contains(&n) {
                        // A bare `object` name used as a value (`val x = Foo`, or a self-reference
                        // `object Foo { … Foo … }`) — its type is the singleton, read as `Foo.INSTANCE`
                        // by lowering. Resolved here so an object can refer to itself in its own body.
                        if let Some(cls) = self.syms.classes.get(&n) {
                            return self.set(e, Ty::obj(&cls.internal));
                        }
                    }
                    // A class NAME with a typed `companion object` used as a VALUE (`val c: I = C`): its
                    // value is the companion instance (`C.Companion`), typed as `C$Companion` — which the
                    // collect pass registered with the companion's supertypes, so it is assignable to them.
                    // Lowering reads `getstatic C.Companion`. (Only classes whose companion declares a
                    // supertype get a `C$Companion` ClassSig; a plain companion isn't a first-class value.)
                    if !self.syms.objects.contains(&n) {
                        if let Some(cls) = self.syms.classes.get(&n) {
                            let comp_internal = format!("{}$Companion", cls.internal);
                            if self.syms.class_by_internal(&comp_internal).is_some() {
                                return self.set(e, Ty::obj(&comp_internal));
                            }
                        }
                    }
                    if let Some(&(ty, _, _)) = self.syms.props.get(&n) {
                        ty // top-level property
                    } else if crate::libraries::coroutine_intrinsic(&n)
                        == Some(crate::libraries::CoroutineIntrinsic::CoroutineSuspended)
                    {
                        // `COROUTINE_SUSPENDED` — the coroutine suspension sentinel (typed `Any`);
                        // lowering reads `IntrinsicsKt.getCOROUTINE_SUSPENDED()`. A local of the same
                        // name was resolved above and shadows it.
                        Ty::obj("kotlin/Any")
                    } else if n == "Unit" {
                        // The `Unit` singleton used as a value (`foo(Unit)`, `val x = Unit`, `return
                        // Unit`) — the `kotlin/Unit` object, read as its `INSTANCE` in lowering. Only a
                        // fallback: any local/property/object named `Unit` was resolved above.
                        Ty::obj("kotlin/Unit")
                    } else if let Some(ct) = self.classpath_companion_ty(&n) {
                        // A bare reference to a CLASSPATH class with a companion object (`Json` →
                        // `Json.Default`): its value is the companion instance, typed as the companion's
                        // type, so `Json.encodeToString(…)` resolves as an instance method on it.
                        // Lowering emits `getstatic <class>.<field>:LcompanionType;`.
                        self.set(e, ct)
                    } else if let Some(internal) = self.classpath_object_value(&n) {
                        // A CLASSPATH `object` referenced as a value (`EmptyCoroutineContext`): its type is
                        // the object type; lowering reads `getstatic <internal>.INSTANCE`.
                        self.expr_lowers.insert(
                            e,
                            ExprLowering::ObjectValue {
                                internal: internal.clone(),
                            },
                        );
                        Ty::obj(&internal)
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
                // `a && b` / `a || b`: a smart-cast established by `a` holds while checking `b`. In `&&`,
                // the RHS is reached when `a` is TRUE (`x is String && x.length`); in `||`, when `a` is
                // FALSE, so the RHS gets `a`'s NEGATED narrowing (`x !is String || x.length` — reaching
                // the RHS means `x` IS a `String`). Narrow `x` in a scope for the right operand, mirroring
                // the `if`-then/else narrowing.
                if matches!(op, BinOp::And | BinOp::Or) {
                    let for_else = matches!(op, BinOp::Or);
                    let lt = self.expr(lhs);
                    // `&&`: the RHS sees EVERY narrowing from the (possibly compound) left chain
                    // (`x is Double? && y is Int? && x == y`). `||`: a single negated narrowing.
                    let casts: Vec<(String, Ty)> = if op == BinOp::And {
                        let mut v = Vec::new();
                        self.collect_and_narrowings(lhs, &mut v);
                        v
                    } else {
                        self.smartcast_binding(lhs, for_else).into_iter().collect()
                    };
                    self.push_scope();
                    for (n, t) in &casts {
                        // Don't narrow to a VALUE class: it's erased to its underlying type, and a
                        // smart-cast use in the same boolean expr (`x is V && x == …`) would take the
                        // unboxed-equals path the `&&`-narrowing lowering doesn't model — miscompile.
                        let is_value = t
                            .obj_internal()
                            .and_then(|i| self.syms.class_by_internal(i))
                            .is_some_and(|c| c.value_field.is_some());
                        if !is_value {
                            self.declare(n, *t, false);
                        }
                    }
                    let rt = self.expr(rhs);
                    self.pop_scope();
                    // `check_binary` enforces both operands are `Boolean` (and reports the same
                    // "operator cannot be applied" error as the non-`&&` path otherwise).
                    let bt = self.check_binary(op, lt, rt, self.span(e));
                    return self.set(e, bt);
                }
                let lt = self.expr(lhs);
                let rt = self.expr(rhs);
                // User-defined extension operator on a primitive receiver overrides built-in arithmetic.
                // Only applies to primitive receivers (reference receivers can't distinguish nullable vs
                // non-null at the krusty type level, risking infinite self-recursion in the body).
                if lt != Ty::Error && rt != Ty::Error && !lt.is_reference() {
                    let op_name = op.arith_operator_name();
                    if let Some(fname) = op_name {
                        if let Some(fi) = crate::module_symbols::ModuleSymbols::new(self.syms)
                            .functions(fname, Some(lt))
                            .overloads
                            .into_iter()
                            .find(|o| {
                                o.kind == crate::libraries::FnKind::Extension
                                    && o.receiver_rank == 0
                            })
                        {
                            // logical params (receiver is `callable.params[0]`) — operators take one arg.
                            // Only apply the extension when the RIGHT operand actually matches its
                            // parameter type; otherwise this is the builtin (`Int * Int` inside the body
                            // of a `Int.times(V)` extension must NOT re-pick that extension and infer `V`).
                            if fi.callable.params.len() == 2 {
                                // Match the lowerer's guard (ir_lower Binary extension path): an exact
                                // operand/param match, or a reference subtype. No loose cross-numeric
                                // clause — a numeric-param operator on a primitive is the builtin's job
                                // (and `p == rt` already covers a same-type numeric param).
                                let p = fi.callable.params[1];
                                let arg_ok = p == rt
                                    || (p.is_reference()
                                        && rt.is_reference()
                                        && match (p.obj_internal(), rt.obj_internal()) {
                                            (Some(ps), Some(rs)) => self.obj_is_subtype(rs, ps),
                                            _ => true,
                                        });
                                if arg_ok {
                                    return self.set(e, fi.callable.ret);
                                }
                            }
                        }
                    }
                }
                // A class MEMBER operator (`operator fun plus(o: V): V` on the receiver class): `a + b` →
                // `a.plus(b)`. The body's own arithmetic is on the field types (no self-recursion). The
                // lowering re-resolves the member, so only the result type is recorded here.
                if let Ty::Obj(internal, _) = &lt {
                    let op_name = op.arith_operator_name();
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
                        // A CLASSPATH `Comparable` type (`class Money : Comparable<Money>` compiled
                        // separately): its `operator fun compareTo(o): Int` is on the classpath, not in
                        // `method_of`. Resolve it through the library set; lowering re-resolves the call.
                        // Only a REFERENCE right operand: an erased generic `Comparable<Double>.compareTo`
                        // takes `Object`, so a PRIMITIVE argument would need a box the lowering path here
                        // doesn't apply — leave that to the existing generic handling / a sound skip.
                        if rt.is_reference() {
                            if let Some(m) = crate::call_resolver::resolve_instance_member(
                                &*self.syms.libraries,
                                lt,
                                "compareTo",
                                &[rt],
                            ) {
                                if m.ret == Ty::Int {
                                    crate::trace_compiler!(
                                        "resolve",
                                        "classpath compareTo drives comparison on {internal}"
                                    );
                                    return self.set(e, Ty::Boolean);
                                }
                            }
                        }
                    }
                    // A library operator function on a reference receiver: `a + b` desugars to `a.plus(b)`,
                    // resolved as a stdlib member/extension (`List + element` → `CollectionsKt.plus`). Use
                    // its (parameterized) return type. The lowering re-resolves to emit the call.
                    let op_name = op.arith_operator_name();
                    // Resolve `a + b` (etc.) as `a.plus(b)` through the library set. Overload selection
                    // picks the most specific candidate (`list + list` → the `Iterable` concat overload,
                    // `list + element` → the element overload), so a reference right operand is fine.
                    if let Some(fname) = op_name {
                        if rt != Ty::Error {
                            if let Some(ret) =
                                self.record_library_extension_call(fname, lt, &[rt], &[])
                            {
                                return self.set(e, ret);
                            }
                        }
                    }
                }
                self.check_binary(op, lt, rt, self.span(e))
            }
            Expr::Member { receiver, name } => {
                // Library companion constants: `Int.MAX_VALUE`, `Double.NaN`, etc.
                if let Expr::Name(type_name) = self.file.expr(receiver).clone() {
                    if self.lookup(&type_name).is_none() {
                        if let Some(c) = companion_const(
                            &*self.syms.libraries,
                            &self.syms.class_names,
                            &type_name,
                            &name,
                        ) {
                            return self.set(e, c.ty);
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
                        // `Kind.PENDING` on a CLASSPATH enum — a static enum-constant field of the enum's
                        // own type. Lowering emits `getstatic <internal>.ENTRY:L<internal>;`.
                        if let Some(internal) = self
                            .imported_type_internal(&en)
                            .or_else(|| self.syms.class_names.get(&en).cloned())
                        {
                            if self.syms.libraries.is_enum_entry(&internal, &name) {
                                crate::trace_compiler!(
                                    "resolve",
                                    "classpath enum entry {en}.{name} -> {internal}"
                                );
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
                        // `ClasspathClass.NestedObject` — a nested singleton object on the classpath
                        // (`PrimitiveKind.STRING` → `getstatic PrimitiveKind$STRING.INSTANCE`). The value
                        // type is the nested object's type; lowering reads its `INSTANCE`.
                        if let Some(outer) = self.imported_type_internal(&en) {
                            let nested = format!("{outer}${name}");
                            if self
                                .syms
                                .libraries
                                .resolve_type(&nested)
                                .is_some_and(|t| t.is_object())
                            {
                                // Value = `getstatic Outer$Nested.INSTANCE` (lowering reads `nested`), but
                                // TYPE it as the OUTER class — the runtime object is-a Outer, and erased
                                // argument matching wants `Outer` (`PrimitiveSerialDescriptor(_, PrimitiveKind)`
                                // accepts `PrimitiveKind.STRING`), not the narrower nested type.
                                self.expr_lowers
                                    .insert(e, ExprLowering::ObjectValue { internal: nested });
                                return self.set(e, Ty::obj(&outer));
                            }
                        }
                    }
                }
                let rt = self.expr(receiver);
                self.check_member(rt, &name, self.span(e), Some(e))
            }
            Expr::Call { callee, args } => self.check_call(e, callee, &args, self.span(e)),
            Expr::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let ct = self.expr(cond);
                self.expect_assignable(Ty::Boolean, ct, self.span(cond), "if condition");
                // Smart-cast: `if (x is T)` narrows a stable `x` to `T` in the then-branch. An `&&`-chain
                // condition (`if (a is Double && b is Double) a == b`) narrows EVERY operand of the chain,
                // so the then-branch sees both (the `==` then unboxes to an IEEE primitive compare).
                let mut then_casts = Vec::new();
                self.collect_and_narrowings(cond, &mut then_casts);
                let is_and_chain =
                    matches!(self.file.expr(cond), Expr::Binary { op: BinOp::And, .. });
                self.push_scope();
                // Declare innermost-last so a variable narrowed twice in a chain (`x is Comparable<*> &&
                // x is Double`) keeps the LAST (most-specific) narrowing; a duplicate-typed reference +
                // primitive pair would otherwise leave the slot inconsistent.
                let mut seen = std::collections::HashSet::new();
                for (n, t) in then_casts.iter().rev() {
                    if !seen.insert(n.clone()) {
                        continue;
                    }
                    // Don't narrow to a VALUE class (erased to its underlying — the smart-cast use would
                    // take an unmodeled unboxed path; mirrors the `&&`-narrowing guard).
                    let is_value = t
                        .obj_internal()
                        .and_then(|i| self.syms.class_by_internal(i))
                        .is_some_and(|c| c.value_field.is_some());
                    // A `Boolean` narrowed inside an `&&` chain then used as a `compareTo` receiver
                    // mis-lowers (a primitive `Boolean` has no instance `compareTo`); leave it un-narrowed
                    // in the chain case (a single `if (x is Boolean)` is unchanged).
                    if is_value || (is_and_chain && *t == Ty::Boolean) {
                        continue;
                    }
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
                // True once a statement always transfers control (`throw`/`return`/break/continue/a
                // `Nothing` call): everything after it — including the trailing value — is unreachable,
                // so the block's type is `Nothing` (it never falls through). Without this, a `try` whose
                // body throws before its trailing would be typed by the dead trailing, and the lowerer
                // would emit that dead code (an unframed branch target → VerifyError).
                let mut diverged = false;
                for s in &stmts {
                    self.stmt(*s);
                    diverged = diverged || self.stmt_diverges(*s);
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
                    // A trailing after a diverging statement is dead — type the block `Nothing` (still
                    // visit the trailing so its sub-expressions are checked).
                    Some(te) if diverged => {
                        self.expr(te);
                        Ty::Nothing
                    }
                    Some(te) => self.expr(te),
                    None if diverged => Ty::Nothing,
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
                // A `when` may have at most one `else` branch (kotlinc rejects a second one).
                if arms.iter().filter(|a| a.conditions.is_empty()).count() > 1 {
                    self.diags.error(
                        self.span(e),
                        "'when' expression must contain at most one 'else' branch".to_string(),
                    );
                }
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
                                    && Ty::promote(st, ct).is_none()
                                    && !self.when_objs_comparable(st, ct) =>
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
                // Class literal. UNBOUND on a reference type name (`String::class`, `UserType::class`)
                // lowers to `ldc <ty>.class`; BOUND on a value expression (`x::class`, `this::class`)
                // lowers to `expr.getClass()`. A primitive receiver (`Int::class`, `42::class`) needs the
                // `Integer.TYPE`/box-then-getClass form and is not modeled.
                if name == "class" {
                    let unsupported = |s: &mut Self| {
                        s.diags.error(
                            s.span(e),
                            "krusty: this class-literal form is not supported".to_string(),
                        );
                        Ty::Error
                    };
                    let Some(recv) = receiver else {
                        return unsupported(self);
                    };
                    // A bare name that resolves to a reference TYPE is an UNBOUND literal (`String::class`).
                    // Otherwise it's a BOUND literal on a value (`x::class`, `this::class`): type-check the
                    // receiver — a non-reference receiver (a primitive `Int::class`, an unresolved name) is
                    // skipped, not mis-read.
                    let unbound = if let Expr::Name(n) = self.file.expr(recv).clone() {
                        self.class_literal_unbound_ty(&n)
                    } else {
                        None
                    };
                    if unbound.is_none() {
                        // Bound: a reference receiver, or a boxable primitive (boxed then `getClass`).
                        let rt = self.expr(recv);
                        let boxable =
                            !matches!(rt, Ty::UInt | Ty::ULong) && rt.boxed_ref().is_some();
                        if !rt.is_reference() && !boxable {
                            return unsupported(self);
                        }
                    }
                    if let Some(ty) = self.syms.libraries.class_literal_type() {
                        self.expr_lowers
                            .insert(e, ExprLowering::ClassLiteral { unbound });
                        return self.set(e, ty);
                    }
                    return unsupported(self);
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
                    // Local function reference `::localFun` (shadows a same-named top-level fn). Map the
                    // ref to the local fun's decl — the SAME map a local-fun CALL uses — so lowering can
                    // find the lifted static method and prepend its captures.
                    if let Some((stmt_id, sig)) = self.lookup_local_fun(&name) {
                        if !sig.vararg && sig.params.len() == sig.required {
                            self.mark_local_function_expr(e, stmt_id);
                            return self.set(e, Ty::fun(sig.params.clone(), sig.ret));
                        }
                    }
                    // An unqualified `::m` inside a class is a BOUND reference to the enclosing receiver's
                    // member FUNCTION — `this::m`. Resolved before the top-level fallbacks (a member takes
                    // precedence over a same-named top-level decl), exactly matching the lowerer's
                    // `lower_implicit_this_method_ref` (member functions only, non-`Nothing` return — a
                    // member-property implicit ref isn't lowered, so it's NOT resolved here either, to keep
                    // the checker and lowerer in agreement).
                    if let Some(Ty::Obj(internal, _)) = self.this_ty.clone() {
                        if let Some(sig) = self.syms.method_of(&internal, &name) {
                            if !sig.vararg
                                && sig.params.len() == sig.required
                                && sig.ret != Ty::Nothing
                            {
                                return self.set(e, Ty::fun(sig.params.clone(), sig.ret));
                            }
                        }
                    }
                    // Top-level property reference `::foo` keeps its property-reference API (`get`,
                    // `name`) while the provider marks it callable-like for function-typed positions.
                    if let Some((_, is_var, _)) = self.syms.props.get(&name) {
                        if let Some(ty) = self.property_ref_ty(0, *is_var) {
                            return self.set(e, ty);
                        }
                    }
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
                    // A CLASSPATH top-level function reference (`::greet` from a jar/dependency module) —
                    // a single non-vararg, fully-applied `TopLevel` overload. Typed as its function type;
                    // the lowering emits a `FunctionReferenceImpl` whose `invoke` calls it.
                    if !self.syms.classes.contains_key(&name) && self.lookup(&name).is_none() {
                        let tl: Vec<_> = self
                            .syms
                            .libraries
                            .functions(&name, None)
                            .overloads
                            .into_iter()
                            .filter(|o| o.kind == crate::libraries::FnKind::TopLevel)
                            .collect();
                        if let [o] = tl.as_slice() {
                            if o.callable.vararg_elem.is_none()
                                && o.call_sig.required == o.callable.params.len()
                                && o.callable.ret != Ty::Nothing
                            {
                                return self
                                    .set(e, Ty::fun(o.callable.params.clone(), o.callable.ret));
                            }
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
                        // bound `this::m` / `this::prop` — the enclosing receiver (a class member).
                        // `this` isn't a scope local, so resolve via `this_ty` (the lowering already
                        // captures `this` = value 0 in `lower_method_ref`/`lower_prop_ref`).
                        if rn == "this" {
                            if let Some(Ty::Obj(internal, _)) = self.this_ty {
                                if let Some(sig) = self.syms.method_of(internal, &name) {
                                    if !sig.vararg && sig.params.len() == sig.required {
                                        return self.set(e, Ty::fun(sig.params.clone(), sig.ret));
                                    }
                                }
                                if let Some((_, is_var)) = self.lookup_prop(internal, &name) {
                                    if let Some(ty) = self.property_ref_ty(0, is_var) {
                                        return self.set(e, ty);
                                    }
                                }
                            }
                        }
                        // bound: `obj::m` where `obj` is an in-scope value
                        if let Some(loc) = self.lookup(&rn) {
                            if let Some(internal) = loc.ty.obj_internal() {
                                if let Some(sig) = self.syms.method_of(internal, &name) {
                                    if !sig.vararg && sig.params.len() == sig.required {
                                        self.expr(r); // capture the receiver
                                        return self.set(e, Ty::fun(sig.params.clone(), sig.ret));
                                    }
                                }
                                // bound property reference `obj::prop` keeps property-reference APIs.
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
                                    if let Some(ty) = self.property_ref_ty(0, is_var) {
                                        return self.set(e, ty);
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
                                // Unbound reference to a same-module EXTENSION function (`A::foo` where
                                // `fun A.foo()` is top-level): the function type prepends the receiver to
                                // the extension's own args — `(A, ext-args…) -> ext-ret`. (A member of
                                // the same name, checked above, takes precedence.)
                                let recv_ty = Ty::obj(&cls.internal);
                                if let Some(sig) = self
                                    .syms
                                    .ext_funs
                                    .get(&(recv_ty.erased_recv(), name.clone()))
                                    .cloned()
                                {
                                    if !sig.vararg
                                        && sig.params.len() == sig.required
                                        && sig.ret != Ty::Nothing
                                    {
                                        let mut params = vec![recv_ty];
                                        params.extend(sig.params.iter().copied());
                                        return self.set(e, Ty::fun(params, sig.ret));
                                    }
                                }
                                // unbound property reference `Type::prop` keeps property-reference APIs.
                                if let Some(is_var) = cls
                                    .props
                                    .iter()
                                    .find(|(n, _, _)| *n == name)
                                    .map(|(_, _, v)| *v)
                                {
                                    if let Some(ty) = self.property_ref_ty(1, is_var) {
                                        return self.set(e, ty);
                                    }
                                }
                            }
                        }
                        // Object/singleton method reference `O::m` → BOUND to the singleton instance,
                        // so its arity is the method's own args (the receiver is captured, not a param).
                        // The lowering captures `O.INSTANCE`.
                        if self.lookup(&rn).is_none() && self.syms.objects.contains(&rn) {
                            if let Some(cls) = self.syms.classes.get(&rn).cloned() {
                                if let Some(sig) = cls.methods.get(&name).cloned() {
                                    if !sig.vararg && sig.params.len() == sig.required {
                                        return self.set(e, Ty::fun(sig.params.clone(), sig.ret));
                                    }
                                }
                            }
                        }
                    }
                }
                // Bound reference on an arbitrary expression receiver (`"abc"::get`, `1::foo`, `mk()::m`):
                // type-check the receiver (evaluated+captured once by the lowering), then resolve a member
                // method or an extension function (keyed by the receiver's erased descriptor). Typed as
                // `(method/ext args) -> ret` — the receiver is bound, not a parameter.
                if let Some(r) = receiver {
                    let rty = self.expr(r);
                    if let Some(internal) = rty.obj_internal() {
                        if let Some(sig) = self.syms.method_of(internal, &name) {
                            if !sig.vararg && sig.params.len() == sig.required {
                                return self.set(e, Ty::fun(sig.params.clone(), sig.ret));
                            }
                        }
                    }
                    if let Some(sig) = self
                        .syms
                        .ext_funs
                        .get(&(rty.erased_recv(), name.clone()))
                        .cloned()
                    {
                        if !sig.vararg && sig.params.len() == sig.required {
                            return self.set(e, Ty::fun(sig.params.clone(), sig.ret));
                        }
                    }
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
            // Unary `+` is identity on the numeric types (`+x : typeof x`).
            UnOp::Plus if ot.is_numeric() => ot,
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
        // Unsigned arithmetic: both operands the same unsigned library integer type. The source owns which
        // types those are (`UInt`/`ULong` on the JVM stdlib); mixed signed/unsigned falls through to the
        // ordinary type error.
        if self.syms.libraries.is_unsigned_integer_type(lt) && lt == rt {
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
                // Structural equality accepts reference-vs-value comparisons: Kotlin boxes the value
                // operand and calls null-safe equality (`Any? == 5`, `x.getOrNull() == 42`). Ordering and
                // arithmetic remain stricter.
                let any = Ty::obj("kotlin/Any");
                // A nullable-primitive wrapper (`Int?`/`Double?`) compares with its primitive (`a == 5.0`):
                // the lowerer null-checks the wrapper, then UNBOXES it and does a primitive `==` (`dcmp`/
                // `fcmp` for Float/Double — IEEE-754, so `-0.0 == 0.0`, `NaN != NaN`), never boxed `equals`.
                let wrapper_vs_prim =
                    |w: Ty, p: Ty| w.nullable_primitive().map_or(false, |pw| pw == p);
                let is_any_ref = |t: Ty| t.non_null() == Ty::obj("kotlin/Any");
                // `Unit` and its singleton reference `kotlin/Unit` are the same value, comparable with
                // `==`/`!=` (`bar() == Unit`, `h.u != Unit`): the non-reference `Ty::Unit` and the
                // `kotlin/Unit` object the `Unit` literal / a Unit-typed field carry.
                let is_unit =
                    |t: Ty| t.non_null() == Ty::Unit || t.non_null() == Ty::obj("kotlin/Unit");
                let has_boxable_value_equality = |t: Ty| {
                    matches!(
                        t,
                        Ty::Int
                            | Ty::Byte
                            | Ty::Short
                            | Ty::Long
                            | Ty::Boolean
                            | Ty::Char
                            | Ty::UInt
                            | Ty::ULong
                    )
                };
                if lt == rt
                    || Ty::promote(lt, rt).is_some()
                    || (lt.is_reference() && rt.is_reference())
                    || (is_any_ref(lt) && has_boxable_value_equality(rt))
                    || (is_any_ref(rt) && has_boxable_value_equality(lt))
                    || lt == any
                    || rt == any
                    || wrapper_vs_prim(lt, rt)
                    || wrapper_vs_prim(rt, lt)
                    || (is_unit(lt) && is_unit(rt))
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
                let is_prim_wrapper = |t: Ty| t.nullable_primitive().is_some();
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
        explicit_elem: Option<Ty>,
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
            // Unsigned primitive arrays are the unboxed underlying primitive array (`[I`/`[J`) — see
            // `ir_lower`'s `Ty::Array(UInt) → kotlin/IntArray` mapping. The element carries `UInt`/`ULong`
            // so reads and arithmetic select the unsigned semantics (and box to the inline-class wrapper
            // only when used generically).
            "uintArrayOf" => Some(Ty::UInt),
            "ulongArrayOf" => Some(Ty::ULong),
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
            // An explicit type argument (`arrayOf<Byte>(1)`) fixes the element type — the array is
            // `Array<Byte>` (`[Byte`), and the integer-literal args narrow to it. Otherwise infer the
            // element as the common type of the arguments.
            let mut elem: Option<Ty> = explicit_elem;
            if elem.is_none() {
                for &t in arg_tys {
                    elem = Some(match elem {
                        Some(prev) => self.join(prev, t, span),
                        None => t,
                    });
                }
            }
            if let Some(e) = elem {
                for (i, t) in arg_tys.iter().enumerate() {
                    self.expect_assignable(e, *t, self.span(args[i]), "array element");
                }
            }
            match elem {
                Some(e) if e.is_reference() => return Some(Ty::array(e)),
                // `arrayOf(1, 2, 3)` is an `Array<Int>` = `[Ljava/lang/Integer;` (distinct from
                // `intArrayOf(…)` = `[I`). Model it as `Obj("kotlin/Array", [Int])` — the SAME logical form
                // as `Array(n) { … }`, so the element reads as the unboxed primitive `Int` (the backend owns
                // the physical boxed layout, un/boxing at access).
                // Unsigned arrays box to their own inline-class wrapper, not a `java/lang/*` — unsupported,
                // consistent with the other array-type resolvers.
                Some(e) if e.boxed_ref().is_some() && !matches!(e, Ty::UInt | Ty::ULong) => {
                    return Some(Ty::obj_args("kotlin/Array", &[e]))
                }
                Some(_) => {
                    self.diags.error(
                        span,
                        "krusty: arrayOf of this element type is not supported".to_string(),
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
            // `Array(n) { … }` is the reference `Array<T>`, distinct from a primitive array. A reference
            // element keeps the existing `Ty::Array` reference representation; scalar and value-class
            // elements are the logical `Array<T>` (`Obj("kotlin/Array", [T])`) — NOT boxed here, so
            // element reads type as `T`. The backend owns the physical boxed/value-class layout.
            return Some(
                if elem.boxed_ref().is_some()
                    || self.syms.libraries.value_underlying(elem).is_some()
                {
                    Ty::obj_args("kotlin/Array", &[elem])
                } else {
                    Ty::array(elem)
                },
            );
        }
        None
    }

    /// Recognize stdlib precondition intrinsics: `require`/`check`/`assert(cond)` (→ `Unit`),
    /// `error(msg)` (→ `Nothing`), and `TODO()`/`TODO(msg)` (→ `Nothing`). Returns the result type,
    /// or `None` if `fname` isn't one of these.
    /// Type-check a receiver-lambda / scope-function body with `recv` as its implicit receiver: `this`
    /// is `recv`, and the receiver's properties resolve unqualified. `label` (the callee's name, e.g.
    /// `run`/`apply`/`with` or a user HOF) is pushed onto the receiver-label stack so `this@label`
    /// resolves inside the body. Returns the body's type.
    fn check_with_receiver_labeled(&mut self, recv: Ty, body: ExprId, label: Option<&str>) -> Ty {
        if recv == Ty::Error {
            return Ty::Error;
        }
        let prev_this = self.this_ty;
        self.this_ty = Some(recv);
        let pushed = label.map(|l| {
            self.this_labels.push((l.to_string(), recv, false));
        });
        let r = self.check_with_receiver_body(recv, body);
        if pushed.is_some() {
            self.this_labels.pop();
        }
        self.this_ty = prev_this;
        r
    }

    fn check_with_receiver_body(&mut self, recv: Ty, body: ExprId) -> Ty {
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
        let rt = rt.nullable_primitive().unwrap_or(rt);
        match name {
            "run" | "apply" if params.is_empty() => {
                let bt = self.check_with_receiver_labeled(rt, body, Some(name));
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
            if let Some(ret) = crate::call_resolver::resolve_instance_member(
                &*self.syms.libraries,
                rt,
                name,
                arg_tys,
            )
            .map(|m| m.ret)
            {
                return Some(ret);
            }
        }
        if let Ty::Obj(internal, _) = &rt {
            // A `vararg` member (`fun f(vararg s: T)`) accepts any number of trailing `T` arguments,
            // packed into the array parameter — element-type them rather than matching the single array
            // parameter positionally (which would reject `f(x)` as "T but Array<T> expected").
            if self.syms.method_is_vararg(internal, name) {
                if let Some(sig) = self.syms.method_of(internal, name) {
                    let n_fixed = sig.params.len().saturating_sub(1);
                    if arg_tys.len() >= n_fixed {
                        self.expect_call_args(&sig.params, true, args, arg_tys);
                        return Some(
                            self.inferred_member_ret(rt, name, &sig.params)
                                .unwrap_or(sig.ret),
                        );
                    }
                }
            }
        }
        if let Ty::Obj(_, _) = rt {
            let module_member = crate::module_symbols::ModuleSymbols::new(self.syms)
                .functions(name, Some(rt))
                .overloads
                .into_iter()
                .find(|o| o.kind == crate::libraries::FnKind::Member);
            if let Some(fi) = module_member {
                let params = fi.callable.params.clone();
                if params.len() == arg_tys.len() {
                    for (i, (p, a)) in params.iter().zip(arg_tys).enumerate() {
                        self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                    }
                    return Some(
                        self.inferred_member_ret(rt, name, &params)
                            .unwrap_or(fi.callable.ret),
                    );
                }
            }
            if let Some(m) = crate::call_resolver::resolve_instance_member(
                &*self.syms.libraries,
                rt,
                name,
                arg_tys,
            ) {
                return Some(m.ret);
            }
        }
        // A MODULE extension on the receiver (`fun Recv.name(args)` declared in this compilation) — keyed
        // by the receiver's erased key, exactly as a qualified `recv.name(args)` extension call
        // resolves. Lets a bare call inside a receiver lambda reach a same-module extension on `this`.
        if let Some(sig) = self
            .syms
            .ext_funs
            .get(&(rt.erased_recv(), name.to_string()))
            .cloned()
        {
            if !sig.vararg && sig.params.len() == arg_tys.len() {
                for (i, (p, a)) in sig.params.iter().zip(arg_tys).enumerate() {
                    self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                }
                return Some(sig.ret);
            }
        }
        // A stdlib/library EXTENSION on the receiver (`String.reversed()`, `String.uppercase()`):
        // resolved receiver-aware so the right overload is selected (`CharSequence.reversed`, not the
        // `Iterable.reversed` that a receiver-blind fallthrough would pick). Mirrors the qualified
        // `recv.name(args)` extension typing.
        if let Some(ret) = self.library_extension_return(name, rt, arg_tys, &[]) {
            return Some(ret);
        }
        if let Some(ret) = self.library_extension_inline_return(name, rt, arg_tys) {
            return Some(ret);
        }
        None
    }

    fn inferred_member_ret(&self, receiver: Ty, name: &str, params: &[Ty]) -> Option<Ty> {
        let internal = receiver.obj_internal()?;
        self.inferred_method_rets
            .get(&(internal.to_string(), name.to_string(), params.to_vec()))
            .copied()
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
        // concrete types); a NON-inline one materializes its lambda as an erased `Function1` whose `it`
        // arrives as `Object`, but the synthesized invoke `checkcast`s to the bound type — sound for a
        // reference/class binding (a `(Item)->R` lambda's `it.name` verifies after the cast, as a
        // non-generic HOF already does). A VALUE-CLASS binding would need UNBOXING, not a cast, so those
        // stay erased (skipped below).
        if f.type_params.is_empty() {
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
        // A value-class binding (`T` = a `@JvmInline value class` — user OR classpath — or an unsigned
        // type) can't be recovered by a plain `checkcast` on the erased lambda parameter (it needs
        // unboxing); keep such a call erased rather than miscompile it.
        if !f.is_inline
            && binds.values().any(|t| {
                self.ty_is_value_class(*t)
                    || self.syms.libraries.value_underlying(*t).is_some()
                    || self.syms.libraries.is_unsigned_integer_type(*t)
            })
        {
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

    /// A user top-level generic function called with an EXPLICIT type argument (`asSeq<String>(x)`)
    /// whose declared return is a bare type parameter (`fun <T> asSeq(...): T`): the call's result
    /// type is the supplied argument (`String`), so members of the result resolve (`…length`). `None`
    /// when there's no explicit type argument or the return isn't one of the function's type params.
    fn explicit_generic_return(&self, call: ExprId, fname: &str) -> Option<Ty> {
        let targs = self.file.call_type_args.get(&call.0)?;
        let idx = self
            .file
            .decls
            .iter()
            .find_map(|&d| match self.file.decl(d) {
                Decl::Fun(f)
                    if f.name == fname && f.receiver.is_none() && !f.type_params.is_empty() =>
                {
                    let ret = f.ret.as_ref()?;
                    f.type_params.iter().position(|tp| tp == &ret.name)
                }
                _ => None,
            })?;
        // The result IS the supplied type argument (`asSeq<String>(…)` is a `String`). A generic slot
        // is physically a BOXED reference on the JVM, so a primitive argument refines to its boxed
        // wrapper (`<Int>`/`<Int?>` → `Integer`) — the actual runtime representation of the erased
        // result. The wrapper is unboxed to the primitive only where a use site demands it, by krusty's
        // normal nullable-primitive machinery; we never collapse the result to the erased `Any`/`Object`.
        let arg = targs.get(idx)?;
        let t = self.resolve_ty_no_diag(arg);
        let t = if !t.is_reference() {
            t.nullable_boxed()?
        } else {
            t
        };
        (t != Ty::Error).then_some(t)
    }

    /// Resolve and record a classpath/library extension call. This keeps the legacy classpath-call
    /// side-channel in one place while the call-resolution surface is being migrated to
    /// `FunctionSet`/`ResolvedCall`.
    fn record_library_extension_call(
        &mut self,
        name: &str,
        receiver: Ty,
        arg_tys: &[Ty],
        type_args: &[Ty],
    ) -> Option<Ty> {
        let c = self.library_extension_callable(name, receiver, arg_tys, type_args)?;
        Some(c.ret)
    }

    fn library_extension_callable(
        &self,
        name: &str,
        receiver: Ty,
        arg_tys: &[Ty],
        type_args: &[Ty],
    ) -> Option<crate::libraries::LibraryCallable> {
        self.resolver()
            .resolve_extension_callable(name, receiver, arg_tys, type_args)
    }

    fn library_extension_return(
        &self,
        name: &str,
        receiver: Ty,
        arg_tys: &[Ty],
        type_args: &[Ty],
    ) -> Option<Ty> {
        self.library_extension_callable(name, receiver, arg_tys, type_args)
            .map(|c| c.ret)
    }

    fn library_extension_inline_return(
        &self,
        name: &str,
        receiver: Ty,
        arg_tys: &[Ty],
    ) -> Option<Ty> {
        self.resolver()
            .resolve_extension_inline_callable(name, receiver, arg_tys)
            .map(|c| c.ret)
    }

    /// When calling `f({ it.method() })` and `f`'s param is `(String) -> R`, this lets `it` have
    /// type `String` instead of the default `Object`.
    /// Type-check a RECEIVER lambda `{ a -> … }` passed to a `Recv.(value params) -> R` parameter: its
    /// implicit `this` is `recv` (a user receiver's properties resolve unqualified, and a bare member /
    /// extension call inside resolves against it via the implicit-`this` probe), and its explicit
    /// parameters bind `value_types`. Records the lambda expr → `recv` so the backend binds the closure's
    /// first parameter as `this`. Returns the lambda's `Fun` type (the receiver folded back in as the
    /// leading parameter, matching how `lambda_param_types` was built).
    /// `label` (the enclosing HOF's name) is pushed on the receiver-label stack so `this@label`
    /// resolves to this lambda's receiver inside the body.
    fn check_lambda_with_receiver_labeled(
        &mut self,
        e: ExprId,
        recv: Ty,
        value_types: &[Ty],
        label: Option<&str>,
    ) -> Ty {
        if self.allow_lambda_mutation {
            self.mark_inline_lambda(e);
        }
        if let Expr::Lambda { params, body } = self.file.expr(e).clone() {
            let bind_names: Vec<String> = if !params.is_empty() {
                params.clone()
            } else if !value_types.is_empty() || expr_uses_name(self.file, body, "it") {
                vec!["it".to_string()]
            } else {
                vec![]
            };
            self.mark_receiver_lambda(e, recv);
            let prev_this = self.this_ty;
            self.this_ty = Some(recv);
            let pushed = label.map(|l| self.this_labels.push((l.to_string(), recv, false)));
            self.push_scope();
            if let Ty::Obj(internal, _) = recv {
                if let Some(cs) = self.syms.class_by_internal(internal) {
                    for (n, t, is_var) in cs.props.clone() {
                        self.declare(&n, t, is_var);
                    }
                }
            }
            for (i, name) in bind_names.iter().enumerate() {
                let pty = value_types.get(i).copied().unwrap_or(Ty::obj("kotlin/Any"));
                self.declare(name, pty, false);
            }
            let saved_field = self.field_ty.take();
            let bret = self.expr(body);
            self.field_ty = saved_field;
            self.pop_scope();
            if pushed.is_some() {
                self.this_labels.pop();
            }
            self.this_ty = prev_this;
            let mut pts = vec![recv];
            pts.extend_from_slice(value_types);
            let ty = Ty::fun(pts, bret);
            return self.set(e, ty);
        }
        self.expr(e)
    }

    fn check_lambda_with_types(&mut self, e: ExprId, param_types: &[Ty]) -> Ty {
        if self.allow_lambda_mutation {
            self.mark_inline_lambda(e);
        }
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

    /// For a generic higher-order member call (`box.map { it.length }` where `box: Box<String>`),
    /// produce the call's substitution plan: the recorded [`GenericMethod`] shape, the class type
    /// parameter → receiver type argument bindings (`{T: String}`), and — per logical argument — the
    /// lambda parameter types with that substitution applied (`[(T) -> R]` → `[[String]]`, so `it`
    /// types as `String`). `None` when the receiver carries no such generic method.
    fn plan_generic_member(&self, rt: Ty, name: &str) -> Option<GenericMemberPlan> {
        let Ty::Obj(internal, targs) = rt else {
            return None;
        };
        let cs = self.syms.class_by_internal(internal)?;
        let gm = cs.generic_methods.get(name)?.clone();
        // Class type parameters → the receiver's type arguments; a parameter the receiver doesn't
        // supply (a raw type) keeps the erased `Object`, preserving the previous lenient behavior.
        let mut class_binds: HashMap<String, Ty> = cs
            .tparam_names
            .iter()
            .map(|n| (n.clone(), Ty::obj("kotlin/Any")))
            .collect();
        for (n, t) in cs.tparam_names.iter().zip(targs.iter()) {
            class_binds.insert(n.clone(), *t);
        }
        // For resolving the lambda PARAMETER types, the method's own type parameters are still unbound
        // (they bind from the lambda body, below), so erase them to `Object` for this resolution.
        let mut input_subst = class_binds.clone();
        for tp in &gm.method_tparams {
            input_subst
                .entry(tp.clone())
                .or_insert(Ty::obj("kotlin/Any"));
        }
        let tp_in = TParams::from_bindings(input_subst);
        let mut scratch = DiagSink::new();
        let lambda_pts: Vec<Vec<Ty>> = gm
            .param_refs
            .iter()
            .map(|r| {
                if !r.fun_params.is_empty() || r.name == "<fun>" {
                    r.fun_params
                        .iter()
                        .map(|p| ty_of_ref(p, &self.syms.class_names, &tp_in, &mut scratch))
                        .collect()
                } else {
                    Vec::new()
                }
            })
            .collect();
        Some((gm, class_binds, lambda_pts))
    }

    /// Infer the method's own type parameters from the typed arguments (a lambda argument is a
    /// `Ty::Fun` whose return is the body type), then realize the declared return ref under both the
    /// class bindings and the inferred method bindings — `fun <R> map(f: (T) -> R): R` on `Box<String>`
    /// with `{ it.length }` yields `Int`.
    fn generic_member_ret(
        &self,
        gm: &GenericMethod,
        class_binds: &HashMap<String, Ty>,
        arg_tys: &[Ty],
    ) -> Ty {
        let mut binds = class_binds.clone();
        for (i, r) in gm.param_refs.iter().enumerate() {
            if !r.fun_params.is_empty() || r.name == "<fun>" {
                if let Some(a) = arg_tys.get(i) {
                    unify_ref(r, *a, &gm.method_tparams, &mut binds);
                }
            }
        }
        let mut scratch = DiagSink::new();
        ty_of_ref(
            &gm.ret_ref,
            &self.syms.class_names,
            &TParams::from_bindings(binds),
            &mut scratch,
        )
    }

    /// The result type of a constructor call `Name<A,…>(…)`: the class instantiated with the call's
    /// explicit type arguments (`ArrayList<Int>()` → `ArrayList<Int>`), so member/element types
    /// resolve. Falls back to the raw class type when there are no explicit type arguments.
    /// Whether the federated library source can construct `internal` with `arg_tys` — a plain
    /// constructor, or a SYNTHETIC default/marker overload (a value-class-typed parameter, or omitted
    /// defaults). The lowerer (`lower_external_new`) fills the marker/placeholder/bitmask args to match.
    fn library_ctor_resolves(&self, internal: &str, arg_tys: &[Ty]) -> bool {
        crate::call_resolver::resolve_constructor(&*self.syms.libraries, internal, arg_tys)
            .is_some()
            || crate::call_resolver::resolve_synthetic_constructor(
                &*self.syms.libraries,
                internal,
                arg_tys,
            )
            .is_some()
    }

    fn ctor_result(&mut self, call: ExprId, internal: &str) -> Ty {
        // Cannot construct an abstract class / interface directly (kotlinc rejects it; the JVM would
        // throw at `new`). Only fires on a genuine construction call here — a `super(…)` delegation
        // and an `object : I {}` literal reach the backend by other paths, not `ctor_result`.
        if let Some(cls) = self.syms.class_by_internal(internal) {
            if cls.is_interface || cls.is_abstract {
                let kind = if cls.is_interface {
                    "an interface"
                } else {
                    "an abstract class"
                };
                self.diags.error(
                    self.span(call),
                    format!("cannot create an instance of {kind} '{internal}'"),
                );
            }
        }
        if let Some(targs) = self.file.call_type_args.get(&call.0).cloned() {
            let args: Vec<Ty> = targs.iter().map(|r| self.resolve_ty(r)).collect();
            if !args.is_empty() {
                return Ty::obj_args(internal, &args);
            }
        }
        // No explicit `<T>` — INFER a classpath generic type's arguments from the constructor call's
        // argument types (`Pair(1, 2)` → `Pair<Int, Int>`), so members/`componentN` type concretely.
        if let Expr::Call { args, .. } = self.file.expr(call).clone() {
            let arg_tys: Vec<Ty> = args
                .iter()
                .map(|&a| self.expr_types[a.0 as usize])
                .collect();
            if let Some(inferred) = self
                .syms
                .libraries
                .infer_constructor_type_args(internal, &arg_tys)
            {
                if inferred.iter().any(|t| *t != Ty::obj("kotlin/Any")) {
                    return Ty::obj_args(internal, &inferred);
                }
            }
        }
        Ty::obj(internal)
    }

    fn check_member(&mut self, rt: Ty, name: &str, span: Span, mexpr: Option<ExprId>) -> Ty {
        if rt == Ty::Error {
            return Ty::Error;
        }
        if let (Ty::String, "length") = (rt, name) {
            return Ty::Int;
        }
        if let (Ty::Char, "code") = (rt, name) {
            return Ty::Int; // `c.code` — the Char's UTF-16 code unit as an `Int`.
        }
        if name == "size" && rt.array_elem().is_some() {
            // `arr.size` — covers both `Ty::Array` and a boxed `Array<T>` (`Obj("kotlin/Array", [T])`).
            return Ty::Int;
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
        // Extension property: `recv.name` resolved by (erased receiver, name).
        if let Some((ty, _)) = self
            .syms
            .ext_props
            .get(&(rt.erased_recv(), name.to_string()))
        {
            return *ty;
        }
        // Library-type property read (`list.size`): semantic property metadata first, source-owned
        // physical getter fallback second. Mapped builtins stay in the symbol provider.
        if let Some(m) =
            crate::call_resolver::resolve_property_member(&*self.syms.libraries, rt, name)
        {
            return m.ret;
        }
        // Classpath EXTENSION property: `recv.name` whose getter is a top-level `get<Name>(recv)` static
        // (e.g. `descriptor.elementDescriptors` → `SerialDescriptorKt.getElementDescriptors(descriptor)`).
        // Tried last, after members/user-ext-props/library getters, so it never shadows a real member.
        if let Ty::Obj(_, _) = rt {
            if let Some(getter) = self.resolver().resolve_extension_property_getter(name, rt) {
                if let Some(e) = mexpr {
                    self.expr_lowers.insert(
                        e,
                        ExprLowering::ExtensionPropertyGet {
                            getter: Box::new(getter.clone()),
                        },
                    );
                }
                return getter.ret;
            }
        }
        if let Some(m) =
            crate::call_resolver::resolve_instance_member(&*self.syms.libraries, rt, name, &[])
        {
            if m.ret.is_read_value_result() {
                return m.ret;
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
        let t = self.check_member(rt, name, span, None);
        if self.diags.diags.len() > n || t == Ty::Error {
            self.diags.diags.truncate(n);
            return None;
        }
        Some(t)
    }

    /// The classpath internal name a bare class name resolves to — an explicit import first, then the
    /// federated class-name seed (default/same-package/wildcard imports). Used to reach a classpath type's
    /// `@Metadata` (e.g. constructor parameter names) from a simple-name constructor call.
    fn classpath_class_internal(&self, name: &str) -> Option<String> {
        // Resolve through the same NESTED-type rewrite the positional-construction path uses: an
        // unqualified nested-type import (`import lib.Op.Apply`) stores the flat `lib/Op/Apply`, but the
        // class is `lib/Op$Apply`. `imported_type_internal` applies the `/`→`$` recovery (and wildcard
        // imports), so a NAMED constructor call on such an import (`Apply(a = 1)`) resolves its ctor
        // parameter names the same way the positional form already resolves the class.
        self.imported_type_internal(name)
            .or_else(|| self.imports.get(name).cloned())
            .or_else(|| self.syms.class_names.get(name).cloned())
    }

    /// If `name` is imported from a classpath `object` (`import a.b.Obj.member` → `imports[member] =
    /// a/b/Obj/member`), the object's internal name — so an unqualified call `member(args)` can dispatch
    /// on the singleton. `None` unless the import's owner path resolves to an object carrying a member of
    /// this name (the owner-value lowering, `getstatic Obj.INSTANCE`, requires a plain object INSTANCE).
    fn object_member_import(&self, name: &str) -> Option<String> {
        let full = self.imports.get(name)?;
        let (owner_path, member) = full.rsplit_once('/')?;
        if member != name {
            return None;
        }
        let owner = self.nested_internal(owner_path)?;
        self.syms
            .libraries
            .resolve_type(&owner)
            .filter(|t| t.is_object())
            .map(|_| owner)
    }

    /// Primary-constructor parameter names of a same-file class, in declaration order (parallel to
    /// `ClassSig::ctor_params`/`ctor_defaults`) — for mapping named constructor arguments. `None` if
    /// `class_name` isn't a same-file class with a primary constructor.
    fn primary_ctor_param_names(&self, class_name: &str) -> Option<Vec<String>> {
        self.file
            .decls
            .iter()
            .find_map(|&d| match self.file.decl(d) {
                Decl::Class(c) if c.name == class_name && c.has_primary_ctor => {
                    Some(c.props.iter().map(|p| p.name.clone()).collect())
                }
                _ => None,
            })
    }

    fn check_call(&mut self, call: ExprId, callee: ExprId, args: &[ExprId], span: Span) -> Ty {
        // The called function's name (`foo` / `recv.method`) — a `Recv.() -> R` lambda argument to it
        // binds `this@<name>`, so this is the label pushed when checking any receiver-lambda argument.
        // Uniform across every function: scope functions (`run`/`apply`/`with`) are not special here.
        let call_fn_name: Option<String> = match self.file.expr(callee) {
            Expr::Name(n) => Some(n.clone()),
            Expr::Member { name, .. } => Some(name.clone()),
            _ => None,
        };
        // Named arguments map onto parameter positions for a top-level function or a method whose
        // signature records parameter names (e.g. a data-class `copy`). Elsewhere the labels would be
        // silently ignored — reject instead.
        let arg_names = self.file.call_arg_names.get(&call.0).cloned();
        if arg_names.is_some() {
            let callee_expr = self.file.expr(callee).clone();
            let supports_named = match &callee_expr {
                // A top-level function, or a same-file class CONSTRUCTOR (`C(b = 9)`) — the primary
                // ctor's parameter names map the labels onto positions, just like a function's.
                Expr::Name(n) => {
                    self.module_declares(n)
                        || self.syms.classes.contains_key(n.as_str())
                        // A CLASSPATH top-level function whose `@Metadata` records parameter names
                        // (`foo(b = …, a = …)` against a function from a jar/dependency module). Module
                        // top-level functions are covered by `module_declares`; this queries the federated
                        // library set for a classpath overload carrying names.
                        || self
                            .syms
                            .libraries
                            .functions(n, None)
                            .overloads
                            .iter()
                            .any(|o| {
                                o.kind == crate::libraries::FnKind::TopLevel
                                    && !o.call_sig.param_names.is_empty()
                            })
                        // A CLASSPATH CONSTRUCTOR whose `@Metadata` records parameter names
                        // (`Point(y = 2, x = 1)`, or `Cfg(a = 1, c = "x")` omitting a defaulted `b`,
                        // against a data/plain class from a dependency). `constructor_named_params` returns
                        // the FULL parameter list for a ctor with at least `args.len()` params, so an
                        // omitted-default named call is still recognized.
                        || self
                            .classpath_class_internal(n)
                            .and_then(|i| {
                                self.syms.libraries.constructor_named_params(&i, args.len())
                            })
                            .is_some()
                }
                Expr::Member { receiver, name }
                    if self
                        .qualified_nested_ctor_internal(*receiver, name)
                        .is_some() =>
                {
                    // A qualified nested-class CONSTRUCTOR with named args (`Op.Ext(a = 1)`). The receiver
                    // names a TYPE, so DON'T type it as a value (that would emit "unresolved reference");
                    // named args map via the nested class's `@Metadata` constructor parameter names.
                    self.qualified_nested_ctor_internal(*receiver, name)
                        .and_then(|i| self.syms.libraries.constructor_named_params(&i, args.len()))
                        .is_some()
                }
                Expr::Member { receiver, name } => {
                    // A method with default parameters (e.g. data-class `copy`) — `required < params` —
                    // queried through the module source.
                    let rt = self.expr(*receiver);
                    let module_overloads = crate::module_symbols::ModuleSymbols::new(self.syms)
                        .functions(name, Some(rt))
                        .overloads;
                    // A member with recorded parameter names supports named arguments: one with defaults
                    // (`required < params`, e.g. data-class `copy`) maps labels + fills omitted slots; one
                    // with all-required parameters (a plain method) reorders the labelled arguments onto
                    // positions (the lowerer evaluates the receiver + args in source order).
                    let module_member = matches!(rt, Ty::Obj(_, _))
                        && module_overloads
                            .iter()
                            .find(|o| o.kind == crate::libraries::FnKind::Member)
                            .map_or(false, |fi| !fi.call_sig.param_names.is_empty());
                    // A user-module EXTENSION with named parameters (`"s".foo(b = …, a = …)`).
                    let module_ext = module_overloads.iter().any(|o| {
                        o.kind == crate::libraries::FnKind::Extension
                            && !o.call_sig.param_names.is_empty()
                    });
                    // A CLASSPATH instance MEMBER or EXTENSION whose `@Metadata` records parameter names
                    // (`g.greet(b = …, a = …)` / `"s".tag(b = …, a = …)` against a jar/dependency function).
                    // An extension receiver may be any type (`String`/primitive), not only `Ty::Obj`.
                    module_member
                        || module_ext
                        || self
                            .syms
                            .libraries
                            .functions(name, Some(rt))
                            .overloads
                            .iter()
                            .any(|o| {
                                matches!(
                                    o.kind,
                                    crate::libraries::FnKind::Member
                                        | crate::libraries::FnKind::Extension
                                ) && !o.call_sig.param_names.is_empty()
                            })
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
                // A FULLY-QUALIFIED top-level FUNCTION call `a.b.helper(args)`: the receiver is a package
                // PATH (its leftmost segment is not a value in scope), and `helper` is a top-level function
                // of that package (compiled to `a/b/<File>Kt`). Resolve it by name among the classpath
                // top-level overloads and confirm the owning facade sits in the receiver's package.
                if let Some(root) = self.dotted_root(receiver) {
                    if self.lookup(&root).is_none() {
                        if let Some(pkg) = qualified_path(self.file, receiver) {
                            let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                            let targs: Vec<Ty> = self
                                .file
                                .call_type_args
                                .get(&call.0)
                                .map(|ts| ts.iter().map(|r| self.resolve_ty(r)).collect())
                                .unwrap_or_default();
                            if let Some(c) = self
                                .resolver()
                                .resolve_top_level_callable(&name, &arg_tys, &targs)
                            {
                                if c.owner.rsplit_once('/').map(|(p, _)| p) == Some(pkg.as_str()) {
                                    crate::trace_compiler!(
                                        "resolve",
                                        "fully-qualified top-level call {pkg}.{name} -> {}",
                                        c.owner
                                    );
                                    for (i, a) in args.iter().enumerate() {
                                        if let Some(p) = c.params.get(i) {
                                            self.expect_assignable(
                                                *p,
                                                arg_tys[i],
                                                self.span(*a),
                                                "argument",
                                            );
                                        }
                                    }
                                    return c.ret;
                                }
                            }
                            // A FQ call with a SYNTACTIC trailing lambda where the preceding parameters
                            // DEFAULT (`kotlinx.coroutines.runBlocking { … }`): the lambda binds the LAST
                            // parameter and the leading (defaulted) parameters are omitted, so the positional
                            // front-to-back resolution above saw the wrong arity and missed it.
                            if self.file.call_has_trailing_lambda.contains(&call.0)
                                && !arg_tys.is_empty()
                            {
                                // The trailing lambda was typed WITHOUT the expected-parameter hint
                                // (`{ … }` → arity 0), but the callee's block parameter is a receiver /
                                // suspend SAM (`kotlinx.coroutines.runBlocking`'s `CoroutineScope.() -> T`).
                                // Re-type the lambda against that parameter — the same shape data (aligned
                                // for the default-omitted trailing lambda) the bare-name (`import`ed) path
                                // uses — so it takes the right arity/receiver and overload resolution then
                                // binds its result type-parameter (`T = String`).
                                let last = args.len() - 1;
                                let mut partial: Vec<Option<Ty>> =
                                    arg_tys.iter().map(|t| Some(*t)).collect();
                                partial[last] = None;
                                let pts = self
                                    .resolver()
                                    .top_level_lambda_param_types(&name, &partial);
                                let recvs =
                                    self.resolver().top_level_lambda_receivers(&name, &partial);
                                if let Some(pt) = pts.as_ref().and_then(|p| p.get(last)).cloned() {
                                    // A RECEIVER function-type block parameter (`CoroutineScope.() -> T`):
                                    // `pt[0]` is the receiver bound as the lambda's `this`, `pt[1..]` its
                                    // value params — matching the bare-name path's `lambda_param_types` use.
                                    let has_recv = recvs
                                        .as_ref()
                                        .and_then(|r| r.get(last).copied().flatten())
                                        .is_some();
                                    let lam_ty = if has_recv && !pt.is_empty() {
                                        self.check_lambda_with_receiver_labeled(
                                            args[last],
                                            pt[0],
                                            &pt[1..],
                                            None,
                                        )
                                    } else {
                                        self.check_lambda_with_types(args[last], &pt)
                                    };
                                    let mut full = arg_tys.clone();
                                    full[last] = lam_ty;
                                    if let Some(c) = self
                                        .resolver()
                                        .resolve_top_level_callable(&name, &full, &targs)
                                    {
                                        if c.owner.rsplit_once('/').map(|(p, _)| p)
                                            == Some(pkg.as_str())
                                        {
                                            crate::trace_compiler!(
                                                "resolve",
                                                "fully-qualified trailing-lambda call {pkg}.{name} -> {}",
                                                c.owner
                                            );
                                            return c.ret;
                                        }
                                    }
                                }
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
                // CLASSPATH nested-class constructor `Outer.Nested(args)` (a sealed subclass
                // `Subject.User("x")`, or any nested class), OR a FULLY-QUALIFIED constructor via a package
                // path `a.b.Ctx(args)`: resolve the whole qualifier to an `Outer$Nested` / `a/b/Ctx`
                // classpath internal and match a constructor. Lowering emits `new …; invokespecial`.
                {
                    if let Some(internal) = self.qualified_nested_ctor_internal(receiver, &name) {
                        let qualified = internal.clone();
                        // NAMED arguments (`Op.Ext(a = 1, b = "x")`, or omitting a defaulted param):
                        // map the labels onto positions via the nested class's `@Metadata` ctor names,
                        // exactly as the unqualified/simple-name classpath-ctor path does. Every param
                        // supplied ⇒ a plain constructor; an omitted defaulted param ⇒ the
                        // `<init>$default` synthetic (the lowerer fills placeholder + mask + marker).
                        if arg_names.is_some() {
                            if let Some((param_names, param_defaults)) = self
                                .syms
                                .libraries
                                .constructor_named_params(&internal, args.len())
                            {
                                let required = param_defaults.iter().filter(|d| !**d).count();
                                match map_call_args(
                                    args,
                                    arg_names.as_deref(),
                                    &param_names,
                                    required,
                                    &param_defaults,
                                ) {
                                    Ok(slots) => {
                                        for &a in slots.iter().flatten() {
                                            self.expr(a);
                                        }
                                        let all_provided =
                                            slots.iter().copied().collect::<Option<Vec<ExprId>>>();
                                        if let Some(sel) = all_provided {
                                            let tys: Vec<Ty> = sel
                                                .iter()
                                                .map(|a| self.expr_types[a.0 as usize])
                                                .collect();
                                            if crate::call_resolver::resolve_constructor(
                                                &*self.syms.libraries,
                                                &internal,
                                                &tys,
                                            )
                                            .is_some()
                                            {
                                                return Ty::obj(&internal);
                                            }
                                        } else if crate::call_resolver::synthetic_default_ctor(
                                            &*self.syms.libraries,
                                            &internal,
                                            slots.len(),
                                        )
                                        .is_some()
                                        {
                                            return Ty::obj(&internal);
                                        }
                                    }
                                    Err(msg) => {
                                        self.diags.error(
                                            span,
                                            format!("constructor '{qualified}': {msg}"),
                                        );
                                        return Ty::Error;
                                    }
                                }
                            }
                        }
                        let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                        // POSITIONAL — a plain constructor, a value-class-param/omitted-default
                        // synthetic (`library_ctor_resolves` covers both). Type-check the provided
                        // arguments against the plain constructor's params when it matches.
                        if self.library_ctor_resolves(&internal, &arg_tys) {
                            crate::trace_compiler!(
                                "resolve",
                                "classpath nested constructor {qualified} -> {internal}"
                            );
                            if let Some(m) = crate::call_resolver::resolve_constructor(
                                &*self.syms.libraries,
                                &internal,
                                &arg_tys,
                            ) {
                                for (i, (p, a)) in m.params.iter().zip(&arg_tys).enumerate() {
                                    self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                                }
                            }
                            return Ty::obj(&internal);
                        }
                    }
                }
                // Classpath value-class COMPANION call `Result.success(args)`: `Result` is a classpath
                // value class whose companion declares `success` (an `inline` fn — private in bytecode,
                // public per `@Metadata`). Resolve metadata-first; lowering emits the companion `getstatic`
                // receiver + an inline-splice of the companion method.
                if let Expr::Name(root) = self.file.expr(receiver).clone() {
                    if self.lookup(&root).is_none() {
                        if let Some(internal) = self.imported_type_internal(&root) {
                            if let Some(cf) =
                                self.syms.libraries.resolve_type(&internal).and_then(|t| {
                                    t.value_companion_fns.into_iter().find(|cf| {
                                        cf.callable.name == name
                                            && cf.callable.params.len() == args.len()
                                    })
                                })
                            {
                                for &a in args {
                                    self.expr(a);
                                }
                                let ret = cf.callable.ret;
                                self.expr_lowers.insert(
                                    call,
                                    ExprLowering::InlineCall(InlineCall::ValueCompanion(Box::new(
                                        cf,
                                    ))),
                                );
                                return ret;
                            }
                        }
                    }
                }
                // `recv.run { … }` / `recv.apply { … }`: the lambda body has `recv` as its implicit
                // receiver (`this`); `run` yields the body, `apply` the receiver.
                if matches!(name.as_str(), "run" | "apply") && args.len() == 1 {
                    if let Expr::Lambda { params, body } = self.file.expr(args[0]).clone() {
                        if params.is_empty() {
                            let rt = self.expr(receiver);
                            let bt =
                                self.check_with_receiver_labeled(rt, body, call_fn_name.as_deref());
                            let returns_receiver = name == "apply";
                            self.expr_lowers.insert(
                                call,
                                ExprLowering::InlineCall(InlineCall::ReceiverLambda(
                                    ReceiverLambda {
                                        receiver,
                                        body,
                                        returns_receiver,
                                    },
                                )),
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
                // (not a local/param) resolvable on the classpath. A top-level PROPERTY of the same name
                // shadows the type/import in value position (`private val logger = logger {}; logger.info()`
                // — `logger` is the KLogger value, not the imported `logger` symbol), so skip the static
                // path and let the receiver resolve as that property value below.
                if let Expr::Name(cls) = self.file.expr(receiver).clone() {
                    if self.lookup(&cls).is_none() && !self.syms.props.contains_key(&cls) {
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
                                Some(m) => {
                                    // Type the class-name receiver as its own type so the LOWERING emits
                                    // the static call (`invokestatic <internal>.name`) via the classpath
                                    // static path — a Java class's static method (`Logf.make(x)`) or a
                                    // Kotlin `@JvmStatic`/companion static.
                                    self.set(receiver, Ty::obj(&internal));
                                    m.ret
                                }
                                // Not a companion STATIC — try a companion-object INSTANCE method
                                // (`Json.encodeToString(…)`/`Random.nextInt(…)` = `<Class>.Default.m(…)`):
                                // resolve `m` as an instance method on the companion's type.
                                None => {
                                    let inst = self
                                        .syms
                                        .libraries
                                        .resolve_type(&internal)
                                        .and_then(|lt| lt.companion_object)
                                        .and_then(|(_, cty)| {
                                            crate::call_resolver::resolve_instance_member(
                                                &*self.syms.libraries,
                                                Ty::obj(&cty),
                                                &name,
                                                &arg_tys,
                                            )
                                            .map(|m| (cty, m))
                                        });
                                    match inst {
                                        Some((cty, m)) => {
                                            // Record the receiver's type as the companion's type so the
                                            // LOWERING resolves this as an instance call on the
                                            // getstatic'd companion value (`Random` → `Random$Default`).
                                            self.set(receiver, Ty::obj(&cty));
                                            // A generic member whose return ERASED to `Any`
                                            // (`Json.decodeFromString(KSerializer<Foo>, String): T`)
                                            // recovers its substituted return (`Foo`) from the arguments.
                                            // Only when erased — a concrete return (`encodeToString: String`)
                                            // keeps the canonical `m.ret` (the recovered form would be a
                                            // non-canonical `Obj("kotlin/String")`).
                                            if m.member.physical_ret == Ty::obj("kotlin/Any") {
                                                // A reified member (`<T> T decodeFromString(…)`) called
                                                // with an explicit type argument (`decodeFromString<C>`)
                                                // returns that type; else recover it from the arguments.
                                                self.reified_type_arg(call).unwrap_or(m.ret)
                                            } else {
                                                m.ret
                                            }
                                        }
                                        None => {
                                            // A classpath `object` INSTANCE member (`Ids.generate()`,
                                            // `L.logger { }`): not a companion/static — dispatch on the
                                            // object singleton. Type the receiver as the object's own
                                            // type and record the singleton read so LOWERING emits
                                            // `getstatic <internal>.INSTANCE; invokevirtual`.
                                            let is_object = self
                                                .syms
                                                .libraries
                                                .resolve_type(&internal)
                                                .is_some_and(|t| t.is_object());
                                            if is_object {
                                                if let Some(m) =
                                                    crate::call_resolver::resolve_instance_member(
                                                        &*self.syms.libraries,
                                                        Ty::obj(&internal),
                                                        &name,
                                                        &arg_tys,
                                                    )
                                                {
                                                    crate::trace_compiler!(
                                                        "resolve",
                                                        "classpath object instance member {cls}.{name} on {internal}"
                                                    );
                                                    self.set(receiver, Ty::obj(&internal));
                                                    self.expr_lowers.insert(
                                                        receiver,
                                                        ExprLowering::ObjectValue {
                                                            internal: internal.clone(),
                                                        },
                                                    );
                                                    return m.ret;
                                                }
                                            }
                                            self.diags.error(span, format!("unresolved Java static '{cls}.{name}' for given argument types"));
                                            Ty::Error
                                        }
                                    }
                                }
                            };
                        }
                    }
                }
                let rt = self.expr(receiver);
                // For a class method with function-type parameters, type lambda arguments against the
                // method's `lambda_param_types` (so `it` resolves), mirroring the free-function path.
                let method_sig: Option<crate::libraries::FunctionInfo> = match rt {
                    Ty::Obj(_, _) => crate::module_symbols::ModuleSymbols::new(self.syms)
                        .functions(&name, Some(rt))
                        .overloads
                        .into_iter()
                        .find(|o| o.kind == crate::libraries::FnKind::Member),
                    _ => None,
                };
                // A generic higher-order member (`box.map { it.length }` where `box: Box<String>`):
                // substitute the receiver's type arguments into the lambda parameter types (so `it`
                // types as `String`/`Int`, not the erased `Any`) and remember the plan to infer the
                // method's own `<R>` from the lambda body — the call's result type — after the args type.
                let generic_member: Option<GenericMemberPlan> = self.plan_generic_member(rt, &name);
                // A library extension taking a lambda (`list.map { it … }`): the lambda's parameter
                // types are recovered from the extension's generic signature — bound by the receiver's
                // element type and the non-lambda arguments — so the lambda body checks against `Int`
                // rather than the erased `Any`. Type the non-lambda arguments first (the accumulator in
                // `fold(0) { acc, x -> }` binds `R`); lambda positions are `None` until resolved.
                let ext_lambda_partial: Option<Vec<Option<Ty>>> = if method_sig.is_none()
                    && rt != Ty::Error
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
                let ext_lambda_pts: Option<Vec<Vec<Ty>>> =
                    ext_lambda_partial.as_ref().and_then(|partial| {
                        self.resolver()
                            .extension_lambda_param_types(rt, &name, partial)
                    });
                let ext_lambda_recvs: Option<Vec<Option<Ty>>> =
                    ext_lambda_partial.as_ref().and_then(|partial| {
                        self.resolver()
                            .extension_lambda_receivers(rt, &name, partial)
                    });
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
                    let has_lam = |lpt: &[Vec<Ty>]| lpt.iter().any(|v| !v.is_empty());
                    // Exact-receiver user extension (module source rung 0): its lambda parameter types
                    // come straight off the call shape.
                    if let Some(fi) = crate::module_symbols::ModuleSymbols::new(self.syms)
                        .functions(&name, Some(rt))
                        .overloads
                        .into_iter()
                        .find(|o| {
                            o.kind == crate::libraries::FnKind::Extension && o.receiver_rank == 0
                        })
                    {
                        if has_lam(&fi.call_sig.lambda_param_types) {
                            return Some(fi.call_sig.lambda_param_types);
                        }
                    }
                    // Generic receiver (rung 1, the `Any` key): the decl's receiver type param → `rt` in
                    // the lambda param types (the type-param→receiver mapping stays AST-based).
                    let fi = crate::module_symbols::ModuleSymbols::new(self.syms)
                        .functions(&name, Some(rt))
                        .overloads
                        .into_iter()
                        .find(|o| {
                            o.kind == crate::libraries::FnKind::Extension && o.receiver_rank == 1
                        })?;
                    if !has_lam(&fi.call_sig.lambda_param_types) {
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
                        fi.call_sig
                            .lambda_param_types
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
                        .resolver()
                        .lambda_return_overload_param_types(rt, &name)?;
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
                        // A generic member's substituted lambda parameter types (`it: String`) take
                        // precedence over the method signature's erased ones (`it: Any`).
                        if let Some((_, _, ref lpt)) = generic_member {
                            if lpt.get(i).is_some_and(|v| !v.is_empty())
                                && matches!(self.file.expr(a), Expr::Lambda { .. })
                            {
                                let pt = lpt[i].clone();
                                return self.check_lambda_with_types(a, &pt);
                            }
                        }
                        if let Some(ref sig) = method_sig {
                            let lpt = &sig.call_sig.lambda_param_types;
                            if i < lpt.len()
                                && !lpt[i].is_empty()
                                && matches!(self.file.expr(a), Expr::Lambda { .. })
                            {
                                let pt = lpt[i].clone();
                                return self.check_lambda_with_types(a, &pt);
                            }
                        }
                        if let Some(ref pts) = ext_lambda_pts {
                            if pts.get(i).map_or(false, |v| !v.is_empty())
                                && matches!(self.file.expr(a), Expr::Lambda { .. })
                            {
                                if let Some(recv) = ext_lambda_recvs
                                    .as_ref()
                                    .and_then(|recvs| recvs.get(i))
                                    .copied()
                                    .flatten()
                                {
                                    let pt = &pts[i];
                                    let value_types = pt.get(1..).unwrap_or(&[]);
                                    return self.check_lambda_with_receiver_labeled(
                                        a,
                                        recv,
                                        value_types,
                                        call_fn_name.as_deref(),
                                    );
                                }
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
                    if let Some(ret) = crate::call_resolver::resolve_instance_member(
                        &*self.syms.libraries,
                        rt,
                        &name,
                        &arg_tys,
                    )
                    .map(|m| m.ret)
                    {
                        return ret;
                    }
                    match (name.as_str(), arg_tys.as_slice()) {
                        ("substring", [Ty::Int]) | ("substring", [Ty::Int, Ty::Int]) => {
                            return Ty::String;
                        }
                        ("indexOf", [Ty::String]) => return Ty::Int,
                        ("concat", [Ty::String]) => return Ty::String,
                        _ => {}
                    }
                    // `trimIndent()`/`trimMargin()` — stdlib extensions; krusty folds them at compile
                    // time on a string-literal receiver (codegen rejects a non-literal receiver).
                    if matches!(name.as_str(), "trimIndent" | "trimMargin") && arg_tys.is_empty() {
                        return Ty::String;
                    }
                }
                if let Some(m) = crate::call_resolver::resolve_instance_member(
                    &*self.syms.libraries,
                    rt,
                    &name,
                    &arg_tys,
                ) {
                    return m.ret;
                }
                // Instance method call on a class value: `p.method(args)` (own or inherited).
                if let Ty::Obj(_, _) = rt {
                    // The user member resolved through the current module as a `SymbolSource`
                    // (`ModuleSymbols`); its DFS member walk matches `lookup_method`, so the first Member
                    // overload is the same one hand-rolled lookup would pick. Collected owned so the
                    // borrow of `syms` ends before the mutating type-checks below.
                    let members: Vec<_> = crate::module_symbols::ModuleSymbols::new(self.syms)
                        .functions(&name, Some(rt))
                        .overloads
                        .into_iter()
                        .filter(|o| o.kind == crate::libraries::FnKind::Member)
                        .collect();
                    // The first Member overload is the most-derived override (for dispatch/return). For an
                    // OMITTED-argument call, the default may be declared on a SUPERTYPE (an interface
                    // method's default isn't redeclared on the override) — prefer an overload that records
                    // defaults so the omitted args resolve, falling back to the override.
                    let short = arg_names.is_some()
                        || members
                            .first()
                            .is_some_and(|o| arg_tys.len() != o.callable.params.len());
                    let module_member = if short {
                        members
                            .iter()
                            .find(|o| {
                                o.call_sig.required < o.callable.params.len()
                                    && !o.call_sig.param_names.is_empty()
                            })
                            .or_else(|| members.first())
                            .cloned()
                    } else {
                        members.first().cloned()
                    };
                    if let Some(fi) = module_member {
                        let params = fi.callable.params.clone();
                        let cs = &fi.call_sig;
                        // Named or omitted arguments: map each argument onto its parameter position via the
                        // parameter names (honouring `required`), then type-check against THAT parameter —
                        // a NAMED call may reorder (`z.test(b = …, a = …)`), so a positional check would
                        // pair each argument with the wrong parameter. Fires for any named call, and for an
                        // omitted-argument call to a method with defaults.
                        if !cs.param_names.is_empty()
                            && (arg_names.is_some()
                                || (arg_tys.len() != params.len() && cs.required < params.len()))
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
                            // Fall through to the shared return-type logic below (generic `<R>` inference /
                            // `inferred_member_ret`) rather than returning the erased `fi.callable.ret`.
                        } else if params.len() != arg_tys.len() {
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
                        // A generic higher-order member: the result is the method's `<R>` inferred from
                        // the lambda body (`box.map { it.length }` → `Int`), not the erased `Object`.
                        if let Some((gm, class_binds, _)) = &generic_member {
                            return self.generic_member_ret(gm, class_binds, &arg_tys);
                        }
                        return self
                            .inferred_member_ret(rt, &name, &params)
                            .unwrap_or(fi.callable.ret);
                    }
                    // A CLASSPATH instance member called with NAMED arguments: reorder the labels onto
                    // parameter positions via the member's `@Metadata` names, then check each against its
                    // parameter (overload resolution by source-order types would otherwise fail to pair).
                    if let Some(names) = arg_names
                        .as_ref()
                        .filter(|ns| ns.iter().any(Option::is_some))
                    {
                        if let Some(fi) = self
                            .syms
                            .libraries
                            .functions(&name, Some(rt))
                            .overloads
                            .into_iter()
                            .find(|o| {
                                o.kind == crate::libraries::FnKind::Member
                                    && !o.call_sig.param_names.is_empty()
                            })
                        {
                            let params = fi.callable.params.clone();
                            let pn = &fi.call_sig.param_names;
                            // Honour the member's per-parameter DEFAULT flags (a data-class `copy` defaults
                            // every parameter to the receiver's property), so a named call may OMIT one —
                            // otherwise every label would be required and `r.copy(b = "y")` errors on `a`.
                            match map_call_args(
                                args,
                                Some(names),
                                pn,
                                fi.call_sig.required,
                                &fi.call_sig.param_defaults,
                            ) {
                                Ok(slots) => {
                                    for (i, slot) in slots.iter().enumerate() {
                                        if let Some(a) = slot {
                                            if matches!(self.file.expr(*a), Expr::Lambda { .. }) {
                                                continue;
                                            }
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
                    }
                    // A classpath Java object: resolve the instance method via the `.class` reader.
                    if let Some(m) = crate::call_resolver::resolve_instance_member(
                        &*self.syms.libraries,
                        rt,
                        &name,
                        &arg_tys,
                    ) {
                        return m.ret;
                    }
                }
                // Builtin bitwise/shift operator methods on `Int`/`Long` (`a shl b`, `a and b`,
                // `a.inv()`) — the named primitive operators (no symbol form), resolved via the shared
                // `builtin_bitwise_ret` (also used by signature inference). The arg-type rule is the
                // checker's: a shift takes an `Int` amount; `and`/`or`/`xor` take the receiver's type.
                if let Some(ret) = builtin_bitwise_ret(rt, &name, arg_tys.len()) {
                    if let Some(arg0) = arg_tys.first() {
                        let expected = if matches!(name.as_str(), "shl" | "shr" | "ushr") {
                            Ty::Int
                        } else {
                            rt
                        };
                        self.expect_assignable(expected, *arg0, self.span(args[0]), "argument");
                    }
                    return ret;
                }
                // A builtin operator-method on a primitive (`5.rem(2)`, `5.plus(2)`) binds to the
                // primitive operator, which *beats* any same-named user extension (in Kotlin a
                // member/builtin wins over an extension). The arithmetic/compare/unary forms map
                // directly to the equivalent operator bytecode (see the mirror in `emit_call`); the
                // rest (`mod` floor-semantics, `rangeTo`, `inc`/`dec`) aren't modeled → reject rather
                // than dispatch to a user extension, which would miscompile.
                // The builtin only applies when every argument is itself a numeric/char primitive (the
                // operand types a builtin operator accepts). A non-numeric argument (`2 * V(3)` with a
                // user `operator fun Int.times(v: V)`) means this is an EXTENSION operator — fall through
                // to extension resolution rather than forcing (and rejecting) the builtin.
                if rt.is_numeric_or_char()
                    && is_builtin_operator_method(&name)
                    && arg_tys.iter().all(|a| a.is_numeric_or_char())
                {
                    // A user `infix`/`operator` extension with this name shadows the builtin for the
                    // *infix* form (`a rem b`) while the dot form (`a.rem(b)`) keeps the builtin.
                    let user_ext = crate::module_symbols::ModuleSymbols::new(self.syms)
                        .functions(&name, Some(rt))
                        .overloads
                        .iter()
                        .any(|o| {
                            o.kind == crate::libraries::FnKind::Extension && o.receiver_rank == 0
                        });
                    let infix_user_ext = self.file.infix_calls.contains(&call.0) && user_ext;
                    if !infix_user_ext && rt.is_numeric() {
                        // Binary arithmetic methods: `a.plus(b)` ≡ `a + b` (same numeric promotion).
                        let bin = BinOp::from_arith_operator_name(&name);
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
                    if !infix_user_ext && rt == Ty::Char {
                        // `Char` has only `plus`/`minus` operator overloads (no `times`/`div`/`rem`).
                        let bin = BinOp::from_arith_operator_name(&name)
                            .filter(|o| matches!(o, BinOp::Add | BinOp::Sub));
                        if let (Some(op), [at]) = (bin, arg_tys.as_slice()) {
                            return self.check_binary(op, rt, *at, span);
                        }
                    }
                    if !infix_user_ext {
                        self.diags.error(span, format!("krusty: builtin operator method '{name}' on a primitive is not supported"));
                        return Ty::Error;
                    }
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
                // NAMED arguments to a classpath EXTENSION (`"s".tag(count = …, name = …)`): the
                // `@Metadata` names are the LOGICAL value parameters (the receiver is a separate
                // `receiver_type`, not a label), so reorder the labelled arguments into parameter order
                // before resolving — source-order type matching would otherwise fail to pair them.
                if let Some(names) = self
                    .file
                    .call_arg_names
                    .get(&call.0)
                    .cloned()
                    .filter(|ns| ns.iter().any(Option::is_some))
                {
                    let sets: Vec<Vec<String>> = self
                        .syms
                        .libraries
                        .functions(&name, Some(rt))
                        .overloads
                        .into_iter()
                        .filter(|o| {
                            o.kind == crate::libraries::FnKind::Extension
                                && !o.call_sig.param_names.is_empty()
                        })
                        .map(|o| o.call_sig.param_names)
                        .collect();
                    if let [pn] = sets.as_slice() {
                        if let Ok(slots) = map_call_args(args, Some(&names), pn, pn.len(), &[]) {
                            if let Some(sel) = slots.into_iter().collect::<Option<Vec<ExprId>>>() {
                                let tys: Vec<Ty> =
                                    sel.iter().map(|a| self.expr_types[a.0 as usize]).collect();
                                if let Some(ret) =
                                    self.record_library_extension_call(&name, rt, &tys, &call_targs)
                                {
                                    return ret;
                                }
                            }
                        }
                    }
                }
                if let Some(ret) =
                    self.record_library_extension_call(&name, rt, &arg_tys, &call_targs)
                {
                    return ret;
                }
                // kotlinc adapts an integer LITERAL argument to a wider expected integer type
                // (`longRange step 3` resolves `LongProgression.step(Long)`, with `3` as `3L`). When the
                // exact-typed resolution failed, retry with integer-literal `Int` args widened to `Long`.
                // A non-literal `Int` is NOT widened (kotlinc rejects `longRange step intVar`); and this is
                // a fallback, so a call that already matched an `Int` overload is unaffected.
                let has_int_literal = arg_tys
                    .iter()
                    .zip(args.iter())
                    .any(|(t, a)| *t == Ty::Int && matches!(self.file.expr(*a), Expr::IntLit(_)));
                if has_int_literal {
                    let widened: Vec<Ty> = arg_tys
                        .iter()
                        .zip(args.iter())
                        .map(|(t, a)| {
                            if *t == Ty::Int && matches!(self.file.expr(*a), Expr::IntLit(_)) {
                                Ty::Long
                            } else {
                                *t
                            }
                        })
                        .collect();
                    if let Some(ret) =
                        self.record_library_extension_call(&name, rt, &widened, &call_targs)
                    {
                        return ret;
                    }
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
                    if let Some((c, _)) = self
                        .resolver()
                        .resolve_lambda_return_overload(rt, &name, lam_ret, &arg_tys)
                    {
                        return c.ret;
                    }
                }
                // Explicit `f.invoke(args)` on a function VALUE — the same invoke convention as `f(args)`.
                // A non-function receiver's explicit `.invoke(...)` stays on the normal member path.
                if name == CALLABLE_INVOKE_OPERATOR && matches!(rt, Ty::Fun(_)) {
                    if let Some(ret) = self.record_invoke(call, receiver, rt, args, &arg_tys, span)
                    {
                        return ret;
                    }
                }
                // A non-public (`@InlineOnly`) extension the backend SPLICES (no callable method to call):
                // a lambda-bearing scope fn (`takeIf`/`takeUnless`/…), recovering the receiver-bound return.
                if args.len() == 1 && matches!(self.file.expr(args[0]), Expr::Lambda { .. }) {
                    if let Some(c) = self
                        .resolver()
                        .resolve_extension_inline_callable(&name, rt, &arg_tys)
                    {
                        return c.ret;
                    }
                }
                // A non-public (`@InlineOnly`) extension is legal when the provider/backend has selected
                // an inline body; lowering emits an inline static call so the backend splices it instead
                // of invoking the private package-part method.
                if let Some(c) = self
                    .resolver()
                    .resolve_extension_inline_callable(&name, rt, &arg_tys)
                {
                    if c.inline.can_inline() {
                        return c.ret;
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
                        let cs = &fi.call_sig;
                        if (arg_names.is_some() || arg_tys.len() != logical.len())
                            && cs.required < logical.len()
                            && !cs.param_names.is_empty()
                        {
                            // Omitted/named extension arguments filled by parameter defaults.
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
                                                logical[i],
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
                        } else if logical.len() != arg_tys.len() {
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
                        return fi.callable.ret;
                    }
                }
                // A user GENERIC-receiver extension `<T> T.foo()` — its receiver erases to `kotlin/Any`,
                // so it's keyed under the `Any` descriptor and matches ANY actual receiver. Specialize the
                // return: a return naming the receiver type param (`T`) → the actual receiver type `rt`;
                // one naming a value-param type param → that argument's type; else the declared return.
                if erased_type_key(rt) != erased_type_key(Ty::obj("kotlin/Any")) {
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
                // `(suspend (…)->T).startCoroutine(completion)` — a `kotlin.coroutines` extension intrinsic
                // (recognized via the registry). Receiver is a suspend function value; the call starts the
                // coroutine and returns `Unit`; lowering asks the target runtime for the physical helper.
                if matches!(rt, Ty::Fun(s) if s.suspend)
                    && crate::libraries::coroutine_intrinsic(&name)
                        == Some(crate::libraries::CoroutineIntrinsic::StartCoroutine)
                {
                    return Ty::Unit;
                }
                // A `@JvmStatic` member of a classpath `object` (`Base58Uuid.of(x)`): kotlinc emits a
                // static method on the object class, so it lands in the type's `companion` (static) list —
                // not an instance member. Resolve it there as a static call on the receiver's type.
                if let Some(internal) = rt.obj_internal() {
                    if let Some(m) = crate::call_resolver::resolve_companion(
                        &*self.syms.libraries,
                        internal,
                        &name,
                        &arg_tys,
                    )
                    // A `@JvmStatic suspend fun` keeps its physical `Continuation` param here (the
                    // companion path doesn't strip/CPS it), so leave it unresolved rather than
                    // miscompile the calling convention.
                    .filter(|m| !m.suspend)
                    {
                        for (i, (p, a)) in m.params.iter().zip(&arg_tys).enumerate() {
                            self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                        }
                        return m.ret;
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
                // Calling a local of function type (`val f: () -> String = …; f()`) or one carrying a
                // member `operator fun invoke` — both go through the one invoke convention.
                if let Some(local) = self.lookup(&fname) {
                    let receiver_ty = local.ty;
                    let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                    if let Some(ret) =
                        self.record_invoke(call, callee, receiver_ty, args, &arg_tys, span)
                    {
                        return ret;
                    }
                }
                // Calling a TOP-LEVEL property of function type: `val x: () -> Int = ...; x()` (e.g. a
                // property bound to a function reference, `val x = ::foo`). Not a local (those are handled
                // above) — read the property and `invoke` it; the backend reads the facade getter then
                // calls `FunctionN.invoke`.
                if self.lookup(&fname).is_none() {
                    if let Some(&(rt @ Ty::Fun(_), _, _)) = self.syms.props.get(&fname) {
                        let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                        if let Some(ret) =
                            self.record_invoke(call, callee, rt, args, &arg_tys, span)
                        {
                            return ret;
                        }
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
                    self.mark_local_function_expr(call, stmt_id);
                    return ret;
                }
                // `with(x) { … }` — `x` is the lambda body's implicit receiver (intercept before the
                // args are evaluated, since the trailing lambda isn't a normal value).
                if fname == "with" && args.len() == 2 && !self.module_declares(&fname) {
                    if let Expr::Lambda { params, body } = self.file.expr(args[1]).clone() {
                        if params.is_empty() {
                            let rt = self.expr(args[0]);
                            let bt =
                                self.check_with_receiver_labeled(rt, body, call_fn_name.as_deref());
                            self.expr_lowers.insert(
                                call,
                                ExprLowering::InlineCall(InlineCall::ReceiverLambda(
                                    ReceiverLambda {
                                        receiver: args[0],
                                        body,
                                        returns_receiver: false,
                                    },
                                )),
                            );
                            return self.set(call, bt);
                        }
                    }
                }
                // `suspendCoroutineUninterceptedOrReturn { c -> … }` — a `kotlin.coroutines` inline
                // intrinsic (recognized through the platform registry, not by name here). The lambda
                // takes the current `Continuation<T>` and returns `Any?` (a resumed value, or
                // `COROUTINE_SUSPENDED`); the call yields `T` — the enclosing suspend function's return
                // type (`@Metadata` declares `<T>(block:(Continuation<T>)->Any?):T`; `T` binds from
                // context). Lowering inlines the lambda body with the function's own continuation bound.
                if args.len() == 1
                    && self.lookup(&fname).is_none()
                    && !self.module_declares(&fname)
                    && matches!(self.file.expr(args[0]), Expr::Lambda { .. })
                    && matches!(
                        crate::libraries::coroutine_intrinsic(&fname),
                        Some(
                            crate::libraries::CoroutineIntrinsic::SuspendCoroutineUninterceptedOrReturn
                                | crate::libraries::CoroutineIntrinsic::SuspendCoroutine
                        )
                    )
                {
                    let cont = Ty::obj("kotlin/coroutines/Continuation");
                    self.check_lambda_with_types(args[0], &[cont]);
                    let r = self.ret_ty;
                    return self.set(call, r);
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
                        if let Some(sam) = self
                            .syms
                            .libraries
                            .resolve_type(&internal)
                            .and_then(|t| t.sam_method)
                        {
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
                        self.resolver()
                            .top_level_lambda_param_types(&fname, partial)
                    })
                    .or_else(|| user_generic.clone());
                // Per-param RECEIVER function type for a classpath top-level HOF (`NavHost(builder:
                // NGB.()->Unit){…}`) — a lambda to such a param binds its implicit `this` to the receiver.
                // From `@Metadata`'s `@ExtensionFunctionType` (no JVM `Signature` needed, so this also
                // drives a krusty-emitted module's HOF whose `Signature` attribute is absent). `None` for a
                // user fn.
                let toplevel_lambda_recvs: Option<Vec<Option<Ty>>> = toplevel_partial
                    .as_ref()
                    .filter(|_| known_sig.is_none())
                    .and_then(|partial| {
                        self.resolver().top_level_lambda_receivers(&fname, partial)
                    });
                // Per-param `crossinline`/`noinline`: such a lambda argument is MATERIALIZED (a real
                // closure, e.g. the `Continuation(ctx){…}` factory's `resumeWith`), so a mutable local it
                // captures must be `Ref`-boxed — DON'T treat it as an inline splice.
                let toplevel_lambda_materialized: Option<Vec<bool>> = toplevel_partial
                    .as_ref()
                    .filter(|_| known_sig.is_none())
                    .and_then(|partial| {
                        self.resolver()
                            .top_level_lambda_materialized(&fname, partial)
                    });
                // A top-level NON-public (`@InlineOnly`) inline fn (`require`/`check`) inlines its lambda
                // argument (or the file is skipped), so a mutable capture is an inline capture — type the
                // lambda body with mutation allowed (don't `Ref`-box the captured var).
                let toplevel_must_inline = self.lookup(&fname).is_none()
                    && !self.module_declares(&fname)
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
                        // The receiver type for a RECEIVER function-type param (from `@Metadata`), if any.
                        let recv_i = toplevel_lambda_recvs
                            .as_ref()
                            .and_then(|r| r.get(i))
                            .copied()
                            .flatten();
                        if let Some(ref pts) = toplevel_lambda_pts {
                            if matches!(self.file.expr(a), Expr::Lambda { .. })
                                && i < pts.len()
                                && !pts[i].is_empty()
                            {
                                // A top-level INLINE fn (`repeat`/`run`/…) splices its lambda, so a mutable
                                // variable the lambda captures is an inline capture (no `Ref` box). EXCEPT a
                                // `crossinline`/`noinline` param materializes the lambda into a real closure,
                                // where a mutable capture IS `Ref`-boxed (e.g. `Continuation(ctx){ res = it }`).
                                let materialized = toplevel_lambda_materialized
                                    .as_ref()
                                    .and_then(|m| m.get(i))
                                    .copied()
                                    .unwrap_or(false);
                                let prev = self.allow_lambda_mutation;
                                self.allow_lambda_mutation =
                                    self.resolver().toplevel_is_inline(&fname) && !materialized;
                                // A RECEIVER function-type param: bind `pts[i][0]` (the Signature-derived
                                // receiver) as the lambda's `this`; the rest are value params.
                                let t = if recv_i.is_some() {
                                    self.check_lambda_with_receiver_labeled(
                                        a,
                                        pts[i][0],
                                        &pts[i][1..],
                                        call_fn_name.as_deref(),
                                    )
                                } else {
                                    self.check_lambda_with_types(a, &pts[i])
                                };
                                self.allow_lambda_mutation = prev;
                                return t;
                            }
                        }
                        // No JVM `Signature` for this callee (a krusty-emitted module) — but `@Metadata`
                        // marks this param a RECEIVER function type, so bind `this` to the receiver from
                        // `@Metadata` (a `Recv.() -> R` param has no value params).
                        if let Some(recv) = recv_i {
                            if matches!(self.file.expr(a), Expr::Lambda { .. }) {
                                return self.check_lambda_with_receiver_labeled(
                                    a,
                                    recv,
                                    &[],
                                    call_fn_name.as_deref(),
                                );
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
                                // A RECEIVER function-type param (`Recv.(A) -> R`): bind `pt[0]` as the
                                // lambda's implicit `this`; the rest are its value params.
                                let t = if sig.lambda_recv.get(i).copied().unwrap_or(false)
                                    && !pt.is_empty()
                                {
                                    self.check_lambda_with_receiver_labeled(
                                        a,
                                        pt[0],
                                        &pt[1..],
                                        call_fn_name.as_deref(),
                                    )
                                } else {
                                    self.check_lambda_with_types(a, &pt)
                                };
                                self.allow_lambda_mutation = prev;
                                return t;
                            }
                            // A lambda argument SAM-converted to a simple `fun interface` parameter:
                            // type it with the interface abstract method's parameter types so its
                            // params resolve concretely and the lowered impl matches the SAM descriptor.
                            if i < sig.params.len()
                                && matches!(self.file.expr(a), Expr::Lambda { .. })
                            {
                                if let Some(internal) = sig.params[i].obj_internal() {
                                    if self.simple_fun_interface(internal) {
                                        if let Some(sp) = self.fun_interface_sam_params(internal) {
                                            return self.check_lambda_with_types(a, &sp);
                                        }
                                    }
                                }
                            }
                        }
                        // A spread argument `*a` (`Array<E>`/`XArray`) contributes its ELEMENT type `E`
                        // to overload resolution and the vararg element check — it behaves like a list of
                        // `E`-typed varargs; only the lowering differs (the array is passed through). A
                        // mixed/unsupported spread shape still type-checks here but the lowering skips it.
                        if self.file.is_spread_arg(a) {
                            let t = self.expr(a);
                            if let Some(elem) = t.array_elem() {
                                return elem;
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
                    if !self.module_declares(&fname) {
                        // `arrayOfNulls<T>(n): Array<T?>` — a reified intrinsic. The element is the explicit
                        // type argument: a reference (`Array<String?>`), or a boxed primitive `Array<Int?>` =
                        // `Integer[]` of nulls (`Obj("kotlin/Array",[Int])`, unsigned excluded). Codegen
                        // allocates `new T[n]` (`b_arr_nulls`).
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
                            } else if elem.boxed_ref().is_some()
                                && !matches!(elem, Ty::UInt | Ty::ULong)
                            {
                                // `Array<Int?>` = `Integer[]` of nulls — the element is the NULLABLE
                                // primitive (so `arr[i] == null` type-checks), allocated boxed.
                                return Ty::obj_args("kotlin/Array", &[Ty::nullable(elem)]);
                            }
                        }
                        let explicit_elem = self
                            .file
                            .call_type_args
                            .get(&call.0)
                            .and_then(|ts| ts.first())
                            .map(|r| self.resolve_ty(r));
                        if let Some(t) =
                            self.check_array_builtin(&fname, args, &arg_tys, span, explicit_elem)
                        {
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
                        // Named-argument constructor call (`C(b = 9)`): map names → positions using the
                        // primary ctor's parameter names + per-parameter defaults, the same path a
                        // top-level function uses. An omitted parameter falls back to its default (the
                        // lowering fills a simple-literal default, or skips the file — never miscompiles).
                        if arg_names.is_some() {
                            if let Some(param_names) = self.primary_ctor_param_names(&fname) {
                                let param_defaults: Vec<bool> =
                                    cls.ctor_defaults.iter().map(|d| d.is_some()).collect();
                                let required = param_defaults.iter().filter(|d| !**d).count();
                                match map_call_args(
                                    args,
                                    arg_names.as_deref(),
                                    &param_names,
                                    required,
                                    &param_defaults,
                                ) {
                                    Ok(slots) => {
                                        for (i, slot) in slots.iter().enumerate() {
                                            if let (Some(a), Some(pt)) = (slot, ctor_params.get(i))
                                            {
                                                self.expect_assignable(
                                                    *pt,
                                                    self.expr_types[a.0 as usize],
                                                    self.span(*a),
                                                    "argument",
                                                );
                                            }
                                        }
                                    }
                                    Err(msg) => {
                                        self.diags
                                            .error(span, format!("constructor '{fname}': {msg}"));
                                    }
                                }
                                return self.ctor_result(call, &cls.internal);
                            }
                        }
                        // Omitted trailing arguments are allowed when those parameters have a default
                        // that is a *simple literal of the parameter's exact type* — the call site can
                        // emit it directly. Adapting defaults (`Long = 0`) or complex defaults
                        // (anonymous objects, `emptyArray()`) aren't modeled yet → skip those.
                        let got = arg_tys.len();
                        let ok_arity = got <= ctor_params.len()
                            && (got..ctor_params.len()).all(|i| {
                                // Match on the file-independent default value (no cross-file `ExprId`
                                // deref). A direct call-site fill emits only an exact-type literal (an
                                // object-singleton default is filled only by the `super(…)` path); keep
                                // that conservative behavior — `Object` here stays unmodeled (`false`).
                                match cls.ctor_defaults.get(i).and_then(|o| o.as_ref()) {
                                    Some(dv) => {
                                        let pt = ctor_params[i];
                                        match dv {
                                            CtorDefaultValue::Int(_) => matches!(
                                                pt,
                                                Ty::Int | Ty::Byte | Ty::Short | Ty::Char
                                            ),
                                            CtorDefaultValue::Long(_) => pt == Ty::Long,
                                            CtorDefaultValue::Double(_) => pt == Ty::Double,
                                            CtorDefaultValue::Float(_) => pt == Ty::Float,
                                            CtorDefaultValue::Bool(_) => pt == Ty::Boolean,
                                            CtorDefaultValue::Char(_) => pt == Ty::Char,
                                            CtorDefaultValue::Str(_) => pt == Ty::String,
                                            CtorDefaultValue::Null => pt.is_reference(),
                                            CtorDefaultValue::Object(_) => false,
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
                    // A CLASSPATH constructor with NAMED arguments (`Point(y = 2, x = 1)`, or
                    // `Cfg(a = 1, c = "x")` OMITTING a defaulted `b`): reorder the labels onto parameter
                    // positions via the ctor's `@Metadata` names + per-parameter default flags. Every
                    // parameter supplied ⇒ a plain constructor; an omitted defaulted parameter ⇒ the
                    // `<init>$default` synthetic (the lowerer fills the placeholder + bitmask + marker).
                    if arg_names.is_some() {
                        if let Some(internal) = self.classpath_class_internal(&fname) {
                            if let Some((param_names, param_defaults)) = self
                                .syms
                                .libraries
                                .constructor_named_params(&internal, args.len())
                            {
                                let required = param_defaults.iter().filter(|d| !**d).count();
                                match map_call_args(
                                    args,
                                    arg_names.as_deref(),
                                    &param_names,
                                    required,
                                    &param_defaults,
                                ) {
                                    Ok(slots) => {
                                        // Type-check every PROVIDED argument (omitted-default slots are `None`).
                                        for &a in slots.iter().flatten() {
                                            self.expr(a);
                                        }
                                        if let Some(sel) =
                                            slots.iter().copied().collect::<Option<Vec<ExprId>>>()
                                        {
                                            // All parameters supplied — a plain constructor.
                                            let tys: Vec<Ty> = sel
                                                .iter()
                                                .map(|a| self.expr_types[a.0 as usize])
                                                .collect();
                                            if let Some(m) =
                                                crate::call_resolver::resolve_constructor(
                                                    &*self.syms.libraries,
                                                    &internal,
                                                    &tys,
                                                )
                                            {
                                                for (p, a) in m.params.iter().zip(&sel) {
                                                    self.expect_assignable(
                                                        *p,
                                                        self.expr_types[a.0 as usize],
                                                        self.span(*a),
                                                        "argument",
                                                    );
                                                }
                                                return self.ctor_result(call, &internal);
                                            }
                                        } else if crate::call_resolver::synthetic_default_ctor(
                                            &*self.syms.libraries,
                                            &internal,
                                            slots.len(),
                                        )
                                        .is_some()
                                        {
                                            // A defaulted parameter was omitted — the `<init>$default`
                                            // synthetic (verified present) fills it; the lowerer emits it.
                                            return self.ctor_result(call, &internal);
                                        }
                                    }
                                    Err(msg) => self
                                        .diags
                                        .error(span, format!("constructor '{fname}': {msg}")),
                                }
                            }
                        }
                    }
                    // Constructing a classpath Java type: `Calc()` where `Calc` is imported. Resolve the
                    // EXPLICIT import through `nested_internal` so a NESTED type import (`import lib.Scope.Ws`
                    // → `lib/Scope$Ws`) maps to the real `$`-qualified internal, not the flat `lib/Scope/Ws`.
                    // Explicit-only (not `imported_type_internal`): a WILDCARD would resolve a mapped builtin
                    // like `Throwable` to `kotlin/Throwable` ahead of the `class_names` `java/lang/Throwable`,
                    // changing a nested ctor arg's type.
                    if let Some(internal) = self
                        .imports
                        .get(&fname)
                        .and_then(|i| self.nested_internal(i))
                    {
                        if self.library_ctor_resolves(&internal, &arg_tys) {
                            return self.ctor_result(call, &internal);
                        }
                    }
                    // A library type by simple name (`throw RuntimeException("msg")`, a mapped/aliased
                    // type with no explicit import): ask the library to resolve the constructor. The
                    // library owns any target-specific knowledge (e.g. the throwable-ctor shapes the
                    // JVM jimage can't surface) — the resolver no longer special-cases throwables.
                    if let Some(internal) = self.syms.class_names.get(&fname).cloned() {
                        if self.library_ctor_resolves(&internal, &arg_tys) {
                            return self.ctor_result(call, &internal);
                        }
                    }
                    // `Any()` constructs java.lang.Object (Kotlin's root type).
                    if fname == "Any" && arg_tys.is_empty() {
                        return Ty::obj("kotlin/Any");
                    }
                }
                // Unqualified call to a sibling instance method: `foo()` → `this.foo()`. Inside an
                // inner class, an unqualified call may target an enclosing method (`this.this$0.foo()`).
                if !self.module_declares(&fname) {
                    if let Some(Ty::Obj(internal, _)) = self.this_ty {
                        // The sibling member through the module source; the enclosing-class fallback walks
                        // `inner_of` (a LEXICAL scope, not the type hierarchy) so it stays on `lookup_method`.
                        let resolved: Option<(Vec<Ty>, Ty)> =
                            crate::module_symbols::ModuleSymbols::new(self.syms)
                                .functions(&fname, Some(Ty::obj(internal)))
                                .overloads
                                .into_iter()
                                .find(|o| o.kind == crate::libraries::FnKind::Member)
                                .map(|fi| (fi.callable.params.clone(), fi.callable.ret))
                                .or_else(|| {
                                    self.syms
                                        .class_by_internal(internal)
                                        .and_then(|c| c.inner_of.clone())
                                        .and_then(|outer| self.lookup_method(&outer, &fname))
                                        .map(|s| (s.params, s.ret))
                                });
                        crate::trace_compiler!(
                            "resolve",
                            "unqualified sibling call {fname}() on this_ty={internal} -> {resolved:?}"
                        );
                        if let Some((params, ret)) = resolved {
                            // A `vararg` sibling method (`fun f(vararg s: T)`) accepts trailing `T` args
                            // packed into the array param — element-type them, don't match the array
                            // positionally.
                            let vararg = self
                                .syms
                                .method_of(internal, &fname)
                                .is_some_and(|s| s.vararg);
                            self.expect_call_args(&params, vararg, args, &arg_tys);
                            // An EXPRESSION-body sibling method whose declared return was the collection
                            // default (`Unit`, not yet inferred) — refine from the inference recorded when
                            // its body was checked (an anonymous object / local class whose `fun m() = f()`
                            // return couldn't be inferred at collection). Matches the qualified-member path.
                            return self
                                .inferred_member_ret(Ty::obj(internal), &fname, &params)
                                .unwrap_or(ret);
                        }
                    } else {
                        crate::trace_compiler!(
                            "resolve",
                            "unqualified call {fname}(): this_ty={:?} module_declares={}",
                            self.this_ty,
                            self.module_declares(&fname)
                        );
                    }
                }
                // Resolve a receiver-less call: a user top-level function shadows everything; otherwise
                // an implicit-receiver member (receiver-lambda body), then a library top-level function.
                // The current module is queried as a `SymbolSource` (ModuleSymbols) and libraries through
                // the classpath set — the federation precedence (module > implicit-receiver > library) made
                // explicit, replacing the scattered `syms.funs.contains_key` guards.
                let user_shadows = self.module_declares(&fname);
                let module_top: Option<crate::libraries::FunctionInfo> =
                    crate::module_symbols::ModuleSymbols::new(self.syms)
                        .resolve_top_level(&fname, &arg_tys);
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
                    if let Some(&inferred) =
                        self.inferred_fun_rets.get(&(fname.clone(), params.clone()))
                    {
                        ret_ty = inferred;
                    }
                    // A user generic call whose return is a type parameter: bind from all arguments.
                    if user_generic.is_some() {
                        if let Some(r) = self.user_generic_return(&fname, &arg_tys) {
                            ret_ty = r;
                        }
                    }
                    // An EXPLICIT type argument (`asSeq<String>(x)`) on a generic call whose return is a
                    // bare type parameter takes precedence — the result type is the supplied argument.
                    if let Some(r) = self.explicit_generic_return(call, &fname) {
                        ret_ty = r;
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
                    } else if self.file.call_has_trailing_lambda.contains(&call.0)
                        && !args.is_empty()
                        && arg_tys.len() <= params.len()
                    {
                        // A purely-positional call with a SYNTACTIC trailing lambda: the lambda binds to the
                        // LAST parameter, preceding positionals fill from the front, and any skipped middle
                        // parameter must have a default (`host("x") { }` on `host(a, modifier = d, builder)`
                        // ⇒ `modifier` defaults, the lambda fills `builder`). Route through `map_call_args`
                        // by labelling the trailing lambda with the last parameter's name; it validates gaps
                        // against `param_defaults` per-slot.
                        let mut synth: Vec<Option<String>> = vec![None; args.len()];
                        if let (Some(last), Some(name)) = (synth.last_mut(), cs.param_names.last())
                        {
                            *last = Some(name.clone());
                        }
                        match map_call_args(
                            args,
                            Some(&synth),
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
                    // NAMED arguments to a classpath function (`describe(count = 3, name = "hi")`):
                    // reorder the arguments into PARAMETER order using the callee's `@Metadata` names, so
                    // overload resolution and per-argument checking pair against the right parameters. Falls
                    // back to source order when there are no labels or the callee has no usable single
                    // overload with names (`supports_named` already rejected truly unsupported callees).
                    let (sel_args, arg_tys): (Vec<ExprId>, Vec<Ty>) = match arg_names
                        .as_ref()
                        .filter(|ns| ns.iter().any(Option::is_some))
                    {
                        Some(names) => {
                            let pnames: Vec<Vec<String>> = self
                                .syms
                                .libraries
                                .functions(&fname, None)
                                .overloads
                                .into_iter()
                                .filter(|o| {
                                    o.kind == crate::libraries::FnKind::TopLevel
                                        && !o.call_sig.param_names.is_empty()
                                })
                                .map(|o| o.call_sig.param_names)
                                .collect();
                            match pnames.as_slice() {
                                [pn] => match map_call_args(args, Some(names), pn, pn.len(), &[]) {
                                    Ok(slots) if slots.iter().all(Option::is_some) => {
                                        let sa: Vec<ExprId> = slots.into_iter().flatten().collect();
                                        let at = sa
                                            .iter()
                                            .map(|a| self.expr_types[a.0 as usize])
                                            .collect();
                                        (sa, at)
                                    }
                                    _ => (args.to_vec(), arg_tys.clone()),
                                },
                                _ => (args.to_vec(), arg_tys.clone()),
                            }
                        }
                        None => (args.to_vec(), arg_tys.clone()),
                    };
                    if let Some(c) =
                        self.resolver()
                            .resolve_top_level_callable(&fname, &arg_tys, &call_targs)
                    {
                        let last_is_array =
                            c.params.last().is_some_and(|p| p.array_elem().is_some());
                        let vararg = last_is_array
                            && (c.params.len() != arg_tys.len()
                                || c.params.last().map_or(false, |p| arg_tys.last() != Some(p)));
                        if vararg && !c.params.is_empty() {
                            let fixed = c.params.len() - 1;
                            let elem = c.params[fixed].array_elem().unwrap_or(Ty::Error);
                            for i in 0..fixed.min(arg_tys.len()) {
                                self.expect_assignable(
                                    c.params[i],
                                    arg_tys[i],
                                    self.span(sel_args[i]),
                                    "argument",
                                );
                            }
                            for i in fixed..arg_tys.len() {
                                self.expect_assignable(
                                    elem,
                                    arg_tys[i],
                                    self.span(sel_args[i]),
                                    "vararg argument",
                                );
                            }
                        } else if c.default_call
                            && arg_tys.last().is_some_and(|t| matches!(t, Ty::Fun(_)))
                            && !arg_tys.is_empty()
                            && arg_tys.len() < c.params.len()
                        {
                            let prefix_len = arg_tys.len() - 1;
                            let last = c.params.len() - 1;
                            for i in 0..prefix_len {
                                self.expect_assignable(
                                    c.params[i],
                                    arg_tys[i],
                                    self.span(sel_args[i]),
                                    "argument",
                                );
                            }
                            let lambda_arg = sel_args[prefix_len];
                            if !matches!(self.file.expr(lambda_arg), Expr::Lambda { .. }) {
                                self.expect_assignable(
                                    c.params[last],
                                    arg_tys[prefix_len],
                                    self.span(lambda_arg),
                                    "argument",
                                );
                            }
                        } else {
                            for (i, a) in arg_tys.iter().enumerate() {
                                if matches!(self.file.expr(sel_args[i]), Expr::Lambda { .. }) {
                                    continue;
                                }
                                self.expect_assignable(
                                    c.params[i],
                                    *a,
                                    self.span(sel_args[i]),
                                    "argument",
                                );
                            }
                        }
                        return c.ret;
                    }
                }
                // An unqualified reference to a SIBLING nested class inside the enclosing class body
                // (`Inner()` in `class Outer { class Inner { … } }`) resolves to `Outer$Inner` — Kotlin's
                // nested-class scoping (a qualified `Outer.Inner()` already resolves). Exact-arity
                // positional construction only; named/omitted-default nested ctors are a later slice.
                if let Some(Ty::Obj(outer, _)) = self.this_ty {
                    let nested = format!("{outer}${fname}");
                    if let Some(cls) = self
                        .syms
                        .classes
                        .values()
                        .find(|s| s.internal == nested)
                        .cloned()
                    {
                        // An `inner class` needs the enclosing INSTANCE (a synthetic `this$0` ctor
                        // parameter not in `ctor_params`), so `Inner(…)` unqualified isn't a plain
                        // construction — leave it unresolved (skip). Only a PLAIN nested class resolves here.
                        if cls.inner_of.is_none()
                            && cls.ctor_params.len() == arg_tys.len()
                            && arg_names.is_none()
                        {
                            for (i, (p, a)) in cls.ctor_params.iter().zip(&arg_tys).enumerate() {
                                self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                            }
                            return self.ctor_result(call, &cls.internal);
                        }
                    }
                }
                // An unqualified call to a MEMBER function of a classpath `object` imported through
                // `import Obj.member` (`private val logger = logger {}`, kotlin-logging's idiom). The
                // positional import stores `Obj/member`; if the owner is an object with a matching member,
                // Kotlin dispatches on the singleton — record the object so LOWERING emits
                // `getstatic Obj.INSTANCE; invokevirtual`. Args (including a trailing lambda) were typed
                // above, so `resolve_instance_member` selects the overload.
                if let Some(internal) = self.object_member_import(&fname) {
                    if let Some(m) = crate::call_resolver::resolve_instance_member(
                        &*self.syms.libraries,
                        Ty::obj(&internal),
                        &fname,
                        &arg_tys,
                    ) {
                        crate::trace_compiler!(
                            "resolve",
                            "unqualified object-member import {fname}() -> {internal}.{fname}"
                        );
                        self.expr_lowers
                            .insert(call, ExprLowering::ObjectMemberCall { internal });
                        return m.ret;
                    }
                }
                self.diags
                    .error(span, format!("unresolved function '{fname}'"));
                Ty::Error
            }
            _ => {
                // An arbitrary callee expression (e.g. `make()(x)`): invoke it if it is a function
                // value or carries a member `operator fun invoke`.
                let callee_ty = self.expr(callee);
                let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(*a)).collect();
                if let Some(ret) = self.record_invoke(call, callee, callee_ty, args, &arg_tys, span)
                {
                    return ret;
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
        if is_nothing_ty(a) {
            return b;
        }
        if is_nothing_ty(b) {
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
        // A primitive joins with `null` as its boxed (nullable) form — `if (c) true else null` is a
        // `Boolean?`, the primitive branch boxed at the merge.
        if a == Ty::Null {
            if let Some(nb) = b.nullable_boxed() {
                return nb;
            }
        }
        if b == Ty::Null {
            if let Some(nb) = a.nullable_boxed() {
                return nb;
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
        // Parameter type of the user operator, if one exists (member first, then extension) — through
        // the module source. A member's `params` are just `[arg]`; an extension's are `[recv, arg]`.
        let fs = crate::module_symbols::ModuleSymbols::new(self.syms).functions(aname, Some(recv));
        let param = fs
            .overloads
            .iter()
            .find(|o| o.kind == crate::libraries::FnKind::Member && o.callable.params.len() == 1)
            .map(|o| o.callable.params[0])
            .or_else(|| {
                fs.overloads
                    .iter()
                    .find(|o| {
                        o.kind == crate::libraries::FnKind::Extension
                            && o.receiver_rank == 0
                            && o.callable.params.len() == 2
                    })
                    .map(|o| o.callable.params[1])
            });
        let rt = self.expr(rhs);
        if let Some(param) = param {
            if rt != Ty::Error {
                self.expect_assignable(param, rt, self.span(rhs), "operator argument");
            }
            self.stmt_lowers.insert(s, StmtLowering::PlusAssign);
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
                .resolver()
                .resolve_extension_inline_callable(aname, recv, &[rt])
                .is_some()
        {
            self.stmt_lowers.insert(s, StmtLowering::PlusAssign);
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
                // Record the resolved ANNOTATION type so the lowerer reuses it (a library type resolved
                // through imports survives even when the initializer is `null`/less specific).
                if let Some(d) = declared {
                    self.local_decl_types.insert(s, d);
                }
                self.declare(&name, bind, is_var);
            }
            Stmt::LocalDelegate {
                is_var,
                name,
                ty,
                delegate,
            } => {
                if self.declared_in_current_scope(&name) {
                    self.diags.error(
                        self.file.stmt_spans[s.0 as usize],
                        format!("krusty: conflicting local declaration '{name}'"),
                    );
                }
                // Type-check the delegate; the property's type is the annotation, else the delegate's
                // `getValue` return type.
                let dt = self.expr(delegate);
                let prop_ty = match ty.as_ref() {
                    Some(r) => self.resolve_ty(r),
                    None => dt
                        .obj_internal()
                        .and_then(|i| self.syms.method_of(i, "getValue"))
                        .map(|s| s.ret)
                        .unwrap_or(Ty::Error),
                };
                self.declare(&name, prop_ty, is_var);
            }
            Stmt::LocalLateinit { name, ty } => {
                if self.declared_in_current_scope(&name) {
                    self.diags.error(
                        self.file.stmt_spans[s.0 as usize],
                        format!("krusty: conflicting local declaration '{name}'"),
                    );
                }
                // A `lateinit var`: a mutable local of the (non-null) annotation type, initialized later.
                // No initializer to check; reads before assignment are allowed (they throw at runtime).
                let prop_ty = self.resolve_ty(&ty);
                self.local_decl_types.insert(s, prop_ty);
                self.declare(&name, prop_ty, true);
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
                                    crate::call_resolver::resolve_instance_member(
                                        &*self.syms.libraries,
                                        it,
                                        &comp,
                                        &[],
                                    )
                                    .map(|m| m.ret)
                                })
                        })
                        // `componentN` as a stdlib *extension* — a public one (`List.component1()`) or
                        // an `@InlineOnly` one (`Map.Entry.component1` → `getKey()`). The inline-admitting
                        // resolver covers both (the same path a qualified `entry.component1()` uses).
                        .or_else(|| self.library_extension_inline_return(&comp, it, &[]))
                        // A USER-defined `operator fun Recv.componentN()` extension (same module).
                        .or_else(|| {
                            crate::module_symbols::ModuleSymbols::new(self.syms)
                                .functions(&comp, Some(it))
                                .overloads
                                .into_iter()
                                .find(|o| {
                                    o.kind == crate::libraries::FnKind::Extension
                                        && o.receiver_rank == 0
                                        && o.callable.params.len() == 1
                                })
                                .map(|o| o.callable.ret)
                        })
                        // An indexable type (`List`): `componentN` is the inline `get(N-1)` — use the
                        // element type from `get(Int)` (which kotlinc inlines the component to).
                        .or_else(|| {
                            internal.and_then(|_| {
                                crate::call_resolver::resolve_instance_member(
                                    &*self.syms.libraries,
                                    it,
                                    "get",
                                    &[Ty::Int],
                                )
                                .map(|m| m.ret)
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
                    if let Some(Ty::Obj(internal, _)) = self.this_ty {
                        self.lookup_prop(internal, &name)
                    } else {
                        None
                    }
                };
                let found = self
                    .lookup(&name)
                    .map(|l| (l.ty, l.is_var))
                    .or_else(inherited)
                    .or_else(|| self.syms.props.get(&name).map(|&(t, v, _)| (t, v)));
                match found {
                    Some((ty, is_var)) => {
                        if !is_var {
                            self.diags
                                .error(span, "'val' cannot be reassigned.".to_string());
                        }
                        if !ty.is_numeric_or_char() {
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
                            let inherited = if let Some(Ty::Obj(internal, _)) = self.this_ty {
                                self.lookup_prop(internal, &name)
                            } else {
                                None
                            };
                            match inherited
                                .or_else(|| self.syms.props.get(&name).map(|&(t, v, _)| (t, v)))
                            {
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
                    .get(&(rt.erased_recv(), name.clone()))
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
                    // `m[i] = v` on a USER class with an `operator fun set(index, value)` → `m.set(i, v)`.
                    None if matches!(at, Ty::Obj(internal, _)
                        if self.syms.method_of(internal, "set").is_some_and(|sig| sig.params.len() == 2)) =>
                    {
                        if let Ty::Obj(internal, _) = at {
                            if let Some(sig) = self.syms.method_of(internal, "set") {
                                self.expect_assignable(sig.params[0], it, span, "index");
                                self.expect_assignable(
                                    sig.params[1],
                                    vt,
                                    span,
                                    "indexed assignment",
                                );
                            }
                        }
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
                // An array element type covers both `Ty::Array` and a boxed `Array<T>`
                // (`Obj("kotlin/Array", [T])`) — iterate either as an array.
                let elem = if let Some(e) = it.array_elem() {
                    e
                } else {
                    match it {
                        Ty::String => Ty::Char, // iterating a String yields its chars
                        Ty::Error => Ty::Error,
                        Ty::Obj(internal, args) => {
                            if let Some(info) = self.syms.libraries.counted_loop_info(internal) {
                                info.elem
                            } else if crate::call_resolver::resolve_instance(
                                &*self.syms.libraries,
                                internal,
                                "iterator",
                                &[],
                            )
                            .is_some()
                            {
                                args.first()
                                    .copied()
                                    .unwrap_or_else(|| Ty::obj("kotlin/Any"))
                            } else {
                                // The `iterator` operator is a member (handled above) OR an extension —
                                // public (an `Iterable`-shaped receiver) or `@InlineOnly` (`Map<K,V>
                                // .iterator()` → `Iterator<Map.Entry>`). The inline-admitting resolver
                                // covers both.
                                match self.library_extension_inline_return("iterator", it, &[]) {
                                    Some(ret) => ret
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
                        }
                        _ => {
                            self.diags.error(self.span(iterable), format!("krusty: 'for' over '{}' is not supported (only arrays, String, and Iterables)", it.name()));
                            Ty::Error
                        }
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
            // A local class is hoisted to a top-level `Decl::Class` (see `hoist_local_classes`) and
            // checked there — nothing to do for the in-body statement.
            Stmt::LocalClass(_) => {}
        }
    }

    /// Type-check a local function declaration (`fun` inside a function body). Non-capturing local
    /// functions are lifted to private static methods; captures become leading parameters.
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

        // Captured outer locals: lifted to extra leading parameters. A captured var whose cell must be
        // shared is marked explicitly; the target runtime chooses the holder representation.
        let mut captured_locals: Vec<(String, Ty)> = Vec::new();
        if !outer_names.is_empty() {
            if let FunBody::Expr(e) | FunBody::Block(e) = &f.body {
                for n in &outer_names {
                    let single: std::collections::HashSet<String> =
                        std::iter::once(n.clone()).collect();
                    if local_fun_body_uses_any(self.file, *e, &single) {
                        let ty = self.lookup(n).map(|l| l.ty).unwrap_or(Ty::Error);
                        captured_locals.push((n.clone(), ty));
                    }
                }
                captured_locals.sort_by(|a, b| a.0.cmp(&b.0));
            }
        }
        let captures: Vec<LocalCapture> = captured_locals
            .into_iter()
            .map(|(name, ty)| {
                let shared_cell = match &f.body {
                    FunBody::Expr(e) | FunBody::Block(e) => {
                        self.local_capture_needs_shared_cell(*e, &name)
                    }
                    FunBody::None => false,
                };
                LocalCapture {
                    name,
                    ty,
                    shared_cell,
                }
            })
            .collect();

        // Add the local function's own type parameters (a primitive/reference bound carries through).
        let resolve = class_internal_resolver(self.syms);
        let added_tparams =
            self.tparams
                .insert_decl_with(&f.type_params, &f.type_param_bounds, &resolve);

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
            lambda_recv: Vec::new(),
            is_inline: false,
            is_final: false,
            is_suspend: f.is_suspend,
        };

        // Register in current local-funs frame and in the TypeInfo maps.
        self.register_local_fun(&f.name, stmt_id, sig.clone());
        self.stmt_lowers.insert(
            stmt_id,
            StmtLowering::LocalFunction(Box::new(LocalFunInfo {
                mangled,
                sig: sig.clone(),
                captures,
            })),
        );

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
        let mut syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        let errs: Vec<String> = d.diags.iter().map(|x| x.msg.clone()).collect();
        (errs, Some(info))
    }

    fn ok(src: &str) {
        let (errs, _) = check(src);
        assert!(errs.is_empty(), "unexpected errors: {errs:?}");
    }
    #[test]
    fn subclass_resolves_generic_base_property_type() {
        // A base class declares a member in terms of its OWN type parameter (`val some: T`); a
        // subclass that fixes the type arg (`I : A<String>()`) must not see `T` as an unresolved
        // reference when collect_signatures pulls the base's props into the subclass's inference
        // scope. (Regression: base props were resolved with the SUBCLASS's type params.)
        ok("abstract class A<T> { abstract val some: T }\n\
            class I : A<String>() { override val some: String get() = \"OK\" }\n\
            fun box(): String = I().some");
    }

    #[test]
    fn serializable_class_exposes_static_serializer() {
        // The serialization plugin's SIGNATURE phase: a `@Serializable` class gains a static
        // `serializer(): KSerializer<C>` visible to the type-checker, so a user reference
        // `C.serializer()` resolves (the plugin emits the body at the backend phase).
        ok("@Serializable class Foo(val a: Int)\n\
            fun box(): String { val s = Foo.serializer(); return \"OK\" }");
    }

    #[test]
    fn boxed_array_type_resolves_and_type_checks() {
        // `Array<Int>` is a boxed `Integer[]` (distinct from `IntArray`). The type resolves (it used
        // to error "Array of a primitive"), and element access / size type-check as `Int`.
        ok("fun id(a: Array<Int>): Array<Int> = a\n\
            fun rd(a: Array<Int>): Int = a[0]\n\
            fun wr(a: Array<Int>) { a[0] = 5 }\n\
            fun sz(a: Array<Int>): Int = a.size\n\
            fun box(): String = \"OK\"");
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
    // tests use `EmptySymbolSource`, so a classpath-resolved call can't be typed).

    #[test]
    fn object_self_reference_resolves() {
        // An `object` may refer to itself by name inside its own body (it resolves to the singleton).
        // Previously this errored "unresolved reference". (Prerequisite for synthesized serializers,
        // whose `$serializer` object references its own INSTANCE.)
        ok("object Baz { fun me(): Baz = Baz }");
        // And an object used as a plain value elsewhere.
        ok("object Conf { val n: Int = 1 }\nfun f(): Conf = Conf");
    }

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
        // Unsigned inline primitives erase to their signed JVM representation, so these overloads
        // would collide as the same backend method even though they are distinct Kotlin surface types.
        err_contains(
            "fun f(x: Int): Int = x\nfun f(x: UInt): UInt = x\nfun box(): String = \"OK\"",
            "conflicting declarations",
        );
    }

    #[test]
    fn named_arguments() {
        // Accepted: named (any order), named combined with an omitted default, and named arguments on a
        // same-file class MEMBER (reordering is realized by the lowerer's source-order temp spill).
        ok("fun f(a: Int, b: Int): Int = a - b\nfun g(): Int = f(b = 2, a = 5)");
        ok("fun f(a: Int, b: Int = 10): Int = a + b\nfun g(): Int = f(a = 1)");
        ok("class C { fun m(a: Int, b: Int): Int = a - b }\nfun g(): Int = C().m(b = 2, a = 5)");
        // Rejected: unknown parameter name.
        err_contains(
            "fun f(a: Int): Int = a\nfun g(): Int = f(z = 1)",
            "no parameter named 'z'",
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
}
