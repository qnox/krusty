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

use crate::symbol_source::SymbolSource;
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

/// Which source a resolved callable came from — set by the source that resolves it, read by the
/// lowerer to choose the emit form: a current-module callable lowers to a same-file `Local`/cross-file
/// call, a library callable to an `invokestatic`/external call. `facade` is the module callable's
/// declaring facade internal name (the file/class it belongs to). Defaults to [`Origin::Library`].
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Origin {
    #[default]
    Library,
    Module {
        facade: String,
    },
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
    /// True when the callee is a Kotlin `inline` function (per its `@Metadata`, decoded with the
    /// signature). The JVM backend may splice the callee's compiled body at the call site (the bytecode
    /// inliner) instead of emitting an `invokestatic`. `false` for a non-inline callable.
    pub is_inline: bool,
    /// True when this resolves a `name$default` synthetic (a callable with defaulted parameters called
    /// with fewer arguments): `params` are the real parameters, and the backend appends zero/`null`
    /// placeholders for the omitted trailing ones, an `int` bit-mask (a bit set per omitted parameter),
    /// and a `null` marker — the JVM realization of default arguments. `false` for an ordinary call.
    pub default_call: bool,
    /// For a generic `vararg` callable resolved with a bound element type (`listOf<Long>(…)` →
    /// `Long`): the *logical* element type the trailing arguments adapt to. `None` for a non-vararg
    /// call or when the element type is not recovered. The backend uses it to coerce each packed
    /// element to that type before boxing (an integer literal in `listOf<Long>(3)` becomes a boxed
    /// `Long`, not `Integer`), since the JVM array element is erased to `Object`.
    pub vararg_elem: Option<Ty>,
    /// True when the callee is NON-PUBLIC (an `@InlineOnly` precondition like `require`/`check`/`error`):
    /// kotlinc emits no callable method, so the backend MUST splice its body — there is no legal
    /// `invokestatic` fallback. If the emitter can't splice such a call (e.g. a branchy body on a
    /// non-empty operand stack), it skips the whole file (never a miscompile / `IllegalAccessError`).
    pub must_inline: bool,
    /// The callee's generic `Signature` (an opaque backend token), kept so an arg-binding SELECTOR can
    /// recover the substituted return (`fold`'s `R` from the initial value, `let`'s `R` from the lambda)
    /// when picking this overload out of a [`FunctionSet`]. `None` when the callable has no generic
    /// signature. The front end never parses it — only the platform's resolution helpers do.
    pub signature: Option<String>,
    /// Which source produced this callable — the lowerer's cue for the emit form. [`Origin::Library`]
    /// for a classpath callable; [`Origin::Module`] (with its facade) for a current-/sibling-module one.
    pub origin: Origin,
}

/// How a resolved function relates to the call's receiver — drives Kotlin overload precedence (a member
/// wins over an extension, both over a top-level function).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FnKind {
    /// A member of the receiver's type (or an inherited one).
    Member,
    /// An extension function on the receiver's type.
    Extension,
    /// A receiver-less top-level function.
    TopLevel,
}

/// The source-level call shape of one overload — the call-site facts the CHECKER needs that the erased
/// emit `descriptor` drops. Parallel to the LOGICAL parameter list (the receiver is NOT included, even
/// for an extension whose `callable.params` prepends it). Empty/zero `Default` means "not provided by
/// this source"; the federated consumer falls back as it did before the consolidation.
#[derive(Clone, Default)]
pub struct CallSig {
    /// Parameter names, parallel to the logical params — maps named arguments (`f(x = 1)`) to positions.
    pub param_names: Vec<String>,
    /// Per logical param: whether it has a default value (so it may be omitted). Parallel to the params.
    pub param_defaults: Vec<bool>,
    /// Per logical param: if it is a function type `(A, B) -> R`, its inner param types `[A, B]` (to type
    /// a lambda argument's `it`/params); otherwise empty. Parallel to the params.
    pub lambda_param_types: Vec<Vec<Ty>>,
    /// Minimum arguments a caller must supply (params beyond this have defaults). 0 by default.
    pub required: usize,
    /// True if the last logical param is `vararg` (callers pack trailing args into its array).
    pub vararg: bool,
}

/// One overload in a [`FunctionSet`]: the full platform-neutral shape of a single function the front end
/// needs, in ONE place — no follow-up metadata calls. `callable` is the opaque emit handle (the platform
/// emitter consumes it; the front end never inspects it).
#[derive(Clone)]
pub struct FunctionInfo {
    pub kind: FnKind,
    /// The extension/member receiver type; `None` for a top-level function.
    pub receiver: Option<Ty>,
    /// Whether the Kotlin return type is nullable (`T?`) — the JVM signature erases this.
    pub ret_nullable: bool,
    /// `inline`, `@InlineOnly` (`inline_only`), and friends — from `@Metadata`.
    pub flags: FnFlags,
    /// The opaque platform callable (owner/name/descriptor on JVM) + its resolved `params`/`ret`. Reuses
    /// [`LibraryCallable`]; the front end reads `params`/`ret` and passes the whole thing to the emitter.
    pub callable: LibraryCallable,
    /// Whether the callee is PUBLIC. A non-public callable has no legal call site (`@InlineOnly`); an
    /// arg-binding selector includes it only when it will SPLICE (never emits an `invokestatic`).
    pub public: bool,
    /// For an [`FnKind::Extension`] overload, the receiver-MRO RUNG it was found at (0 = the receiver's
    /// own type, increasing up the supertype chain). An arg-binding selector groups candidates by this
    /// rank and processes rungs most-specific-first, so a `List` extension wins over an `Iterable` one —
    /// the same receiver precedence the classpath lookup gives, preserved through the consolidated query.
    /// `0` for members/top-level (precedence there is by [`FnKind`], not rung); `u32::MAX` marks a
    /// candidate that must never preempt a real rung (the `@OverloadResolutionByLambdaReturnType` family).
    pub receiver_rank: u32,
    /// The source-level call shape (defaults, named params, lambda param types, vararg) the checker needs
    /// beyond the erased descriptor. `Default` (empty) when the source doesn't provide it.
    pub call_sig: CallSig,
}

/// Function metadata flags, decoded once from `@Metadata`.
#[derive(Clone, Copy, Default, Debug)]
pub struct FnFlags {
    pub inline: bool,
    /// Non-public `@InlineOnly` — has no callable method; the emitter MUST splice its body.
    pub inline_only: bool,
    /// `suspend` — decoded from `@Metadata` (the `IS_SUSPEND` function flag). A call to a suspend
    /// function is a coroutine suspension point (the JVM lowering threads a `Continuation`).
    pub suspend: bool,
}

/// All overloads of one function name applicable to a call — members AND extensions AND top-level, in one
/// query, each tagged with its [`FnKind`] so the caller applies Kotlin precedence and picks (e.g. by the
/// lambda's return type for `@OverloadResolutionByLambdaReturnType`). The consolidation that replaces the
/// scattered `resolve_callable` / `is_inline` / return-overload / nullable lookups.
#[derive(Clone, Default)]
pub struct FunctionSet {
    pub overloads: Vec<FunctionInfo>,
}

/// The shape of a library type: enough for the front end to resolve member accesses against it
/// (publicness, kind, supertypes, constructors, instance members, and companion members) without
/// knowing the target ABI.
pub struct LibraryType {
    pub is_public: bool,
    /// The declaration kind (class / interface / annotation / object). One field instead of parallel
    /// booleans — read it through the `is_*` accessors, which encode the JVM reality that an annotation
    /// is also an interface.
    pub kind: TypeKind,
    /// Internal names of the superclass + implemented interfaces (for the inherited-member walk).
    pub supertypes: Vec<String>,
    pub constructors: Vec<LibraryMember>,
    /// Instance members (member functions and property accessors).
    pub members: Vec<LibraryMember>,
    /// Companion-object members — accessed as `Type.member(…)` (the JVM realizes these as statics).
    pub companion: Vec<LibraryMember>,
}

/// What a library type *is*. Mutually exclusive at the source level; at the JVM level an `Annotation`
/// also carries `ACC_INTERFACE`, which `is_interface()` reflects.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeKind {
    Class,
    Interface,
    Annotation,
    /// A Kotlin `object` (singleton) — has a `public static final INSTANCE` field of its own type, read
    /// as `getstatic <Type>.INSTANCE` when the object is referenced as a value.
    Object,
}

impl LibraryType {
    pub fn is_interface(&self) -> bool {
        matches!(self.kind, TypeKind::Interface | TypeKind::Annotation)
    }
    pub fn is_annotation(&self) -> bool {
        self.kind == TypeKind::Annotation
    }
    pub fn is_object(&self) -> bool {
        self.kind == TypeKind::Object
    }
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
pub(crate) fn best_overload<'a>(
    candidates: impl Iterator<Item = &'a LibraryMember> + Clone,
    name: &str,
    args: &[Ty],
) -> Option<&'a LibraryMember> {
    let named = candidates.filter(|m| m.name == name);
    named
        .clone()
        .find(|m| m.params == *args)
        .or_else(|| {
            named.clone().find(|m| {
                m.params.len() == args.len()
                    && m.params.iter().zip(args).all(|(p, a)| arg_assignable(p, a))
            })
        })
        .or_else(|| named.clone().find(|m| params_prefix(&m.params, args)))
}

impl LibraryType {
    /// A constructor callable with `args` — exact arity, then a widening pass that erases each
    /// reference argument to `Any` (a JDK type may only expose the `(Object)` overload).
    pub fn ctor(&self, args: &[Ty]) -> Option<&LibraryMember> {
        if let Some(m) = self.constructors.iter().find(|m| m.params == *args) {
            return Some(m);
        }
        // A constructor of a GENERIC class has erased `Object`/`Any` parameters; a reference arg widens to
        // `Any`, and a PRIMITIVE arg boxes to `Any` too (`Pair(1, 2)` → `Pair(Object, Object)`). Match the
        // erased ctor with both widenings (the exact-match check above already handled primitive-param
        // ctors like `Foo(Int)`).
        let widened: Vec<Ty> = args
            .iter()
            .map(|t| {
                if t.is_reference() || t.is_primitive() {
                    Ty::obj("kotlin/Any")
                } else {
                    *t
                }
            })
            .collect();
        self.constructors.iter().find(|m| m.params == widened)
    }

    /// The best companion member named `name` for `args` (exact, then boxing, then prefix).
    pub fn companion_member(&self, name: &str, args: &[Ty]) -> Option<&LibraryMember> {
        best_overload(self.companion.iter(), name, args)
    }

    /// Annotation members `(name, Ty)` — the no-argument accessors of an `@interface`.
    pub fn annotation_members(&self) -> Option<Vec<(String, Ty)>> {
        if !self.is_annotation() {
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
/// A primitive constant value read from a library (a `const`/`static final` field's compile-time
/// value), platform-agnostic so the front end can inline it like the reference compiler does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LibConst {
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
}

/// A compiled-library source: a [`SymbolSource`] (its type universe, overloads, and type shapes) PLUS
/// the JVM-emit extras the backend needs (mangled members, inline-body splice checks, companion
/// constants, `@Metadata` queries). The federatable half is `SymbolSource`; these extras are consulted
/// only by the JVM emitter, never across the source federation.
pub trait LibrarySet: SymbolSource {
    /// The compile-time value of a primitive companion constant (`Int.MAX_VALUE`, `Double.NaN`, …),
    /// read from the library (e.g. the JVM `IntCompanionObject.MAX_VALUE` `ConstantValue`). The
    /// front end inlines it at the use site, exactly as the reference compiler does. `None` if not a
    /// known constant / not in the library.
    fn prim_companion_const(&self, _prim: &str, _field: &str) -> Option<LibConst> {
        None
    }

    /// Resolve a package-level callable: a top-level function (`receiver == None`) or an extension
    /// (`receiver == Some(t)`, passed as the callable's first argument). `type_args` are the call's
    /// explicit type arguments (`emptyList<Int>()`), bound to the callable's formal type parameters
    /// when the value arguments don't determine them; empty when none are written.
    fn resolve_callable(
        &self,
        _name: &str,
        _receiver: Option<Ty>,
        _args: &[Ty],
        _type_args: &[Ty],
    ) -> Option<LibraryCallable> {
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

    /// The return `Ty` of a BUILTIN type's class member (`internal` e.g. `kotlin/String`), by name +
    /// argument types, read from the type's builtins declarations rather than a hardcoded table. `None`
    /// if the name isn't a declared member there (e.g. a `StringsKt` extension on `String`).
    fn builtin_member_ret(&self, _internal: &str, _name: &str, _args: &[Ty]) -> Option<Ty> {
        None
    }

    /// The single abstract method of a functional interface (`Runnable.run`, `Comparator.compare`) —
    /// its name and `LibraryMember` — for SAM conversion of a lambda. `None` if `internal` isn't an
    /// interface with exactly one abstract (non-default, non-static, non-`Object`) method.
    fn sam_method(&self, _internal: &str) -> Option<LibraryMember> {
        None
    }

    /// Resolve a method on `internal` whose JVM name starts with `prefix`, returning `(name, descriptor)`.
    /// Inline-class members carry a mangled name suffix (`getFirst-pVg5ArA` on `UIntRange`, where the
    /// `-…` hash encodes the inline-class signature), so a call site can't name them directly — it looks
    /// the real name up from the classpath instead of recomputing kotlinc's mangling. `None` if absent.
    fn mangled_member(&self, _internal: &str, _prefix: &str) -> Option<(String, String)> {
        None
    }

    /// For a generic extension `recv.name(args…)` taking function arguments, the *element-typed*
    /// parameter types of each call argument that is a lambda — `List<Int>.map { … }` → `[[Int]]` (the
    /// single lambda's parameter is the element `Int`). The type variables bind from the receiver and
    /// from the already-typed non-lambda arguments (`fold(0) { acc, x -> … }` binds the accumulator `R`
    /// from the `0`), so `arg_tys[i]` is `Some` for a typed non-lambda argument and `None` for a lambda
    /// not yet typed. Lets the checker type lambda bodies before resolving the call. Empty inner vec for
    /// a non-lambda argument; `None` if no such extension.
    fn extension_lambda_param_types(
        &self,
        _recv: Ty,
        _name: &str,
        _arg_tys: &[Option<Ty>],
    ) -> Option<Vec<Vec<Ty>>> {
        None
    }

    /// The same as [`extension_lambda_param_types`](Self::extension_lambda_param_types) but for a
    /// *receiver-less top-level* library function (`applyIt(5) { it + 1 }`): the lambda parameter types
    /// come from the function's generic `Signature` (`it: Int` from `f: (Int) -> Int`), which the erased
    /// `Function1` descriptor hides. Lets the checker type a lib fn's lambda argument before resolving.
    fn toplevel_lambda_param_types(
        &self,
        _name: &str,
        _arg_tys: &[Option<Ty>],
    ) -> Option<Vec<Vec<Ty>>> {
        None
    }

    /// Whether the platform can splice (truly inline) the lambda-taking `inline fun` `owner.name desc`
    /// at a call site — i.e. its compiled body is shaped for the lambda-argument splice. The front end
    /// routes a call to the inliner only when this is true, so an un-spliceable (e.g. `@InlineOnly`,
    /// uncallable) callee is never emitted as a broken call. `false` for non-JVM platforms.
    fn can_inline_lambda(&self, _owner: &str, _name: &str, _descriptor: &str) -> bool {
        false
    }

    /// Whether a NO-lambda `inline fun` body can be spliced at a call site (`String.uppercase()`). The
    /// lowerer routes a private `@InlineOnly` extension call to the splicer only when this is true, so an
    /// un-spliceable (e.g. branchy) private callee is never emitted as a broken `invokestatic`.
    fn can_inline_call(&self, _owner: &str, _name: &str, _descriptor: &str) -> bool {
        false
    }

    /// Resolve a scope-function extension call (`receiver.let { … }`) for the bytecode inliner — like
    /// [`resolve_callable`](Self::resolve_callable) with a receiver, but ALSO matching `@InlineOnly`
    /// package-private candidates (which `resolve_callable` hides, since they aren't callable). The
    /// caller must inline the result, never emit a call. `None` for non-JVM platforms.
    fn resolve_scope_inline(
        &self,
        _name: &str,
        _receiver: Ty,
        _args: &[Ty],
    ) -> Option<LibraryCallable> {
        None
    }

    /// Whether the Kotlin return type of `owner.name` is an unsigned type (`UByte`/`UShort`/`UInt`/
    /// `ULong`), from `@Metadata`. The JVM signature erases these to a signed primitive, so krusty's `Ty`
    /// can't tell `Int.toUShort(): UShort` (the value `40000`) from a signed `Short` (`-25536`). Used to
    /// REJECT an `@InlineOnly` extension with an unsigned result rather than splice it to a wrong value
    /// (krusty's unsigned support is incomplete). `false` for non-JVM platforms / non-unsigned returns.
    fn metadata_return_unsigned(&self, _owner: &str, _name: &str) -> bool {
        false
    }

    /// The lambda parameter types for a Kotlin-name call that resolves by lambda RETURN type
    /// (`@OverloadResolutionByLambdaReturnType`, e.g. `Iterable<T>.sumOf(selector: (T) -> R)`): `[element]`
    /// so the selector's `it` types as the receiver's element rather than the erased `Any`. `None` when
    /// `name` isn't such a function on `receiver`. (Return-type independent — used before the body is typed.)
    fn lambda_return_overload_param(&self, _receiver: Ty, _name: &str) -> Option<Vec<Ty>> {
        None
    }
}

/// A library set with no external libraries — compiling a self-contained source set with no classpath.
pub struct EmptyLibrarySet;

impl SymbolSource for EmptyLibrarySet {}
impl LibrarySet for EmptyLibrarySet {}
