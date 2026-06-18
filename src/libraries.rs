//! The library-set abstraction — one half of a target *platform* (the other half is its emitter,
//! e.g. `jvm::JvmBackend`). A `LibrarySet` is the common denominator a front end needs from a
//! target's compiled libraries: the type universe and the *shape* of each type and top-level
//! callable, whether the libraries are a JVM classpath (bytecode `.class` jars) or a klib (IR).
//!
//! The resolver and IR lowering depend **only** on this trait, never on the JVM backend: every
//! `java/lang/…` name, descriptor parse, and classpath read lives behind a concrete implementation
//! (`jvm::jvm_libraries::JvmLibraries`). Swapping in a klib-backed `LibrarySet` would let the same
//! front end target Kotlin/JS without touching `resolve`/`ir_lower`.
//!
//! The surface is deliberately Kotlin-semantic — there is no "static" (a `Type.foo()` call is a
//! companion-object member; a top-level/extension call is a package-level callable). The JVM
//! realization of those (invokestatic on a facade, `@JvmStatic`, descriptors) lives in the impl.

use crate::types::Ty;
use std::collections::HashMap;

/// The type universe the library set contributes, resolved to internal names: every importable
/// simple name → its internal name, plus type aliases (`alias` → target simple/internal name).
#[derive(Default)]
pub struct LibrarySeed {
    pub class_names: HashMap<String, String>,
    pub type_aliases: HashMap<String, String>,
}

/// One member (constructor, member function/property accessor, or companion member) of a library
/// type, in Kotlin terms. `descriptor` is an opaque backend token (a JVM method descriptor) the
/// matching emitter consumes verbatim — the front end matches on `params`/`ret`, never parsing it.
#[derive(Clone)]
pub struct LibraryMember {
    pub name: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub descriptor: String,
}

/// A package-level callable: a top-level function (`listOf`), or an extension (its receiver is the
/// first parameter). `owner` is the internal name of the facade/declaring container for emit.
#[derive(Clone)]
pub struct LibraryCallable {
    pub owner: String,
    pub name: String,
    pub params: Vec<Ty>,
    /// The *logical* return type — for a generic callable, the substituted type (`listOf<Int>` →
    /// `List<Int>`, `first()` → the element). The checker reports this.
    pub ret: Ty,
    /// The *physical* (erased) return type the JVM signature actually produces (`Object` for an erased
    /// type parameter). The backend inserts the unbox/checkcast bridging `physical_ret` → `ret`.
    pub physical_ret: Ty,
    pub descriptor: String,
    /// True when this resolves a `name$default` synthetic (a callable with defaulted parameters called
    /// with fewer arguments): `params` are the real parameters, and the backend appends zero/`null`
    /// placeholders for the omitted trailing ones, an `int` bit-mask (a bit set per omitted parameter),
    /// and a `null` marker — the JVM realization of default arguments. `false` for an ordinary call.
    pub default_call: bool,
}

/// The shape of a library type: enough for the front end to resolve member accesses against it
/// (publicness, kind, supertypes, constructors, instance members, and companion members) without
/// knowing the target ABI.
pub struct LibraryType {
    pub is_public: bool,
    pub is_interface: bool,
    pub is_annotation: bool,
    /// Internal names of the superclass + implemented interfaces (for the inherited-member walk).
    pub supertypes: Vec<String>,
    pub constructors: Vec<LibraryMember>,
    /// Instance members (member functions and property accessors).
    pub members: Vec<LibraryMember>,
    /// Companion-object members — accessed as `Type.member(…)` (the JVM realizes these as statics).
    pub companion: Vec<LibraryMember>,
}

/// Whether a member's parameter list matches `args` as a prefix — the loose match the JVM resolver
/// used (a call's argument descriptors prefixing the method's). One `Ty` → one descriptor token, so
/// a `Ty`-slice prefix is equivalent to a descriptor prefix.
fn params_prefix(member_params: &[Ty], args: &[Ty]) -> bool {
    member_params.len() >= args.len() && member_params[..args.len()] == *args
}

/// Whether `arg` can be passed where `param` is expected, in erased Kotlin terms: an exact `Ty`
/// match, or any argument into an erased generic (`Any`) parameter — a primitive boxes into it
/// (`List<Int>.add(E)` → `add(Object)`, calling with `Int` boxes to `Integer`), a reference passes
/// directly. This is what lets a primitive argument select the erased `(Object)` overload instead of
/// falling through to a longer-arity overload it happens to prefix.
fn arg_assignable(param: &Ty, arg: &Ty) -> bool {
    param == arg || *param == Ty::obj("kotlin/Any")
}

/// The best overload named `name` among `candidates` for `args`: an exact-arity exact-`Ty` match,
/// else an exact-arity match with autoboxing into erased `Any` parameters, else a prefix match (the
/// loose fallback covering varargs/defaulted trailing parameters).
fn best_overload<'a>(
    candidates: impl Iterator<Item = &'a LibraryMember> + Clone,
    name: &str,
    args: &[Ty],
) -> Option<&'a LibraryMember> {
    let named = candidates.filter(|m| m.name == name);
    named.clone().find(|m| m.params == *args)
        .or_else(|| named.clone().find(|m| m.params.len() == args.len() && m.params.iter().zip(args).all(|(p, a)| arg_assignable(p, a))))
        .or_else(|| named.clone().find(|m| params_prefix(&m.params, args)))
}

impl LibraryType {
    /// A constructor callable with `args` — exact arity, then a widening pass that erases each
    /// reference argument to `Any` (a JDK type may only expose the `(Object)` overload).
    pub fn ctor(&self, args: &[Ty]) -> Option<&LibraryMember> {
        if let Some(m) = self.constructors.iter().find(|m| m.params == *args) {
            return Some(m);
        }
        let widened: Vec<Ty> = args.iter().map(|t| if t.is_reference() { Ty::obj("kotlin/Any") } else { *t }).collect();
        self.constructors.iter().find(|m| m.params == widened)
    }

    /// The best companion member named `name` for `args` (exact, then boxing, then prefix).
    pub fn companion_member(&self, name: &str, args: &[Ty]) -> Option<&LibraryMember> {
        best_overload(self.companion.iter(), name, args)
    }

    /// The best instance member named `name` (declared on this type) for `args`.
    pub fn instance_member(&self, name: &str, args: &[Ty]) -> Option<&LibraryMember> {
        best_overload(self.members.iter(), name, args)
    }

    /// Annotation members `(name, Ty)` — the no-argument accessors of an `@interface`.
    pub fn annotation_members(&self) -> Option<Vec<(String, Ty)>> {
        if !self.is_annotation {
            return None;
        }
        let mut out = Vec::new();
        for m in &self.members {
            if m.params.is_empty() && m.name != "<init>" {
                if m.ret == Ty::Error {
                    return None; // a member type we can't model — skip the whole annotation
                }
                out.push((m.name.clone(), m.ret));
            }
        }
        Some(out)
    }
}

/// What the front end asks of the target's library set. Results are in Kotlin terms (`Ty`, internal
/// names); any backend-specific encoding (a JVM method descriptor) is an opaque string the matching
/// backend emitter consumes. Default methods resolve nothing, for an empty library set.
pub trait LibrarySet {
    /// The seed type universe (classpath/klib types + intrinsic built-in mappings).
    fn seed(&self) -> LibrarySeed {
        LibrarySeed::default()
    }

    /// The shape of the library type named `internal`, or `None` if the library has no such type.
    /// The single entry point for resolving constructors, member functions, companion members,
    /// interface-ness, and annotation members — the front end navigates the returned [`LibraryType`].
    fn resolve_type(&self, _internal: &str) -> Option<LibraryType> {
        None
    }

    /// Resolve a package-level callable: a top-level function (`receiver == None`) or an extension
    /// (`receiver == Some(t)`, passed as the callable's first argument). `type_args` are the call's
    /// explicit type arguments (`emptyList<Int>()`), bound to the callable's formal type parameters
    /// when the value arguments don't determine them; empty when none are written.
    fn resolve_callable(&self, _name: &str, _receiver: Option<Ty>, _args: &[Ty], _type_args: &[Ty]) -> Option<LibraryCallable> {
        None
    }

    /// The substituted (non-erased) return type of instance member `name(args)` on a *parameterized*
    /// receiver — `List<Int>.get(0)` → `Int`. Binds the receiver's type arguments through the generic
    /// hierarchy and substitutes the member's generic return. `None` when the receiver carries no type
    /// arguments, the member isn't generic, or the library can't resolve it (the caller then uses the
    /// erased return). The physical descriptor is unchanged — only the *logical* type is recovered.
    fn member_return(&self, _recv: Ty, _name: &str, _args: &[Ty]) -> Option<Ty> {
        None
    }

    /// For a generic extension `recv.name(args…)` taking function arguments, the *element-typed*
    /// parameter types of each call argument that is a lambda — `List<Int>.map { … }` → `[[Int]]` (the
    /// single lambda's parameter is the element `Int`). The type variables bind from the receiver and
    /// from the already-typed non-lambda arguments (`fold(0) { acc, x -> … }` binds the accumulator `R`
    /// from the `0`), so `arg_tys[i]` is `Some` for a typed non-lambda argument and `None` for a lambda
    /// not yet typed. Lets the checker type lambda bodies before resolving the call. Empty inner vec for
    /// a non-lambda argument; `None` if no such extension.
    fn extension_lambda_param_types(&self, _recv: Ty, _name: &str, _arg_tys: &[Option<Ty>]) -> Option<Vec<Vec<Ty>>> {
        None
    }
}

// --- Navigation helpers (the front end's resolution logic over the `LibrarySet`) -----------------
// These live in the core, expressed purely against the trait, so `resolve` and `ir_lower` share one
// implementation of the inherited-member walk without duplicating it or depending on the backend.

/// Resolve a constructor on a library type by argument types (with the type's own widening).
pub fn resolve_constructor(lib: &dyn LibrarySet, internal: &str, args: &[Ty]) -> Option<LibraryMember> {
    lib.resolve_type(internal)?.ctor(args).cloned()
}

/// Resolve a companion member `Type.name(args)` (the receiver type must be public).
pub fn resolve_companion(lib: &dyn LibrarySet, internal: &str, name: &str, args: &[Ty]) -> Option<LibraryMember> {
    let t = lib.resolve_type(internal)?;
    if !t.is_public {
        return None;
    }
    t.companion_member(name, args).cloned()
}

/// Resolve an instance member `recv.name(args)` — the receiver's static type must be public, but the
/// member may be inherited from a (possibly non-public) supertype, so walk the chain breadth-first.
pub fn resolve_instance(lib: &dyn LibrarySet, internal: &str, name: &str, args: &[Ty]) -> Option<LibraryMember> {
    if !lib.resolve_type(internal)?.is_public {
        return None;
    }
    // A generic method erases its type-parameter arguments to `Any` (`List<E>.add(E)` → `add(Object)`),
    // so a reference argument matches against an `Any` parameter — try the exact args, then widened.
    let widened: Vec<Ty> = args.iter().map(|t| if t.is_reference() { Ty::obj("kotlin/Any") } else { *t }).collect();
    let mut seen = std::collections::HashSet::new();
    let mut q = std::collections::VecDeque::new();
    q.push_back(internal.to_string());
    while let Some(cur) = q.pop_front() {
        if !seen.insert(cur.clone()) {
            continue;
        }
        let Some(t) = lib.resolve_type(&cur) else { continue };
        if let Some(m) = t.instance_member(name, args).or_else(|| t.instance_member(name, &widened)) {
            return Some(m.clone());
        }
        q.extend(t.supertypes);
    }
    None
}

/// A library set with no external libraries — compiling a self-contained source set with no classpath.
pub struct EmptyLibrarySet;

impl LibrarySet for EmptyLibrarySet {}
