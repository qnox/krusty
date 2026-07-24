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
use crate::libraries::{
    required_arity, CallSig, EmptySymbolSource, InlineKind, Origin, ParamList, SemanticPlatform,
};
use crate::symbol_source::SymbolSource;
use crate::types::{existing_type_name, type_name, Ty, TypeName, Visibility};

pub type ResolvedMember = crate::symbol_resolver::ResolvedMember;

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
    /// File-independent default values, parallel to `params`. `Some` only for literal/object defaults the
    /// lowerer can emit without dereferencing the declaring file's AST arena.
    pub param_default_values: Vec<Option<CtorDefaultValue>>,
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
    /// Number of leading context parameters in `params`. Ordinary functions leave this at 0.
    pub context_count: usize,
    /// Source declaration id for a top-level function in the current compilation, when this signature
    /// came from an AST declaration. Member/local/classpath signatures leave this unset.
    pub source_decl: Option<DeclId>,
    /// Source file index paired with [`Self::source_decl`]. Declaration ids are arena-local to a file.
    pub source_file: Option<u32>,
    /// Declaring package in internal slash form (`pkg/sub`) for source top-level declarations.
    pub package: String,
}

/// The minimum arity of an ADAPTED callable reference to a signature — the parameters a reference must
/// supply, with trailing defaults and an optional trailing vararg omitted. `required` counts the vararg
/// (it has no default), so drop it when the required prefix reaches the vararg's position.
pub fn adapted_ref_arity(vararg: bool, required: usize, param_count: usize) -> usize {
    if vararg && required == param_count {
        required - 1
    } else {
        required
    }
}

impl Signature {
    pub fn requires_all_args(&self) -> bool {
        !self.vararg && self.params.len() == self.required
    }

    pub fn single_param(&self) -> Option<Ty> {
        (self.params.len() == 1).then(|| self.params[0])
    }

    pub fn call_sig(&self) -> CallSig {
        CallSig::source(
            self.param_names.clone(),
            self.param_defaults.clone(),
            self.lambda_param_types.clone(),
            self.lambda_recv.clone(),
            self.required,
            self.vararg,
        )
    }
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

impl CtorDefaultValue {
    pub fn fills_param_ty(&self, ty: Ty) -> bool {
        match self {
            CtorDefaultValue::Int(_) => ty.int_arithmetic_repr() == Ty::Int,
            CtorDefaultValue::Long(_) => ty == Ty::Long,
            CtorDefaultValue::Double(_) => ty == Ty::Double,
            CtorDefaultValue::Float(_) => ty == Ty::Float,
            CtorDefaultValue::Bool(_) => ty == Ty::Boolean,
            CtorDefaultValue::Char(_) => ty == Ty::Char,
            CtorDefaultValue::Str(_) => ty == Ty::String,
            CtorDefaultValue::Null => ty.is_reference(),
            CtorDefaultValue::Object(_) => false,
        }
    }
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
                .resolve_type_name(internal)
                .is_some_and(|t| t.is_object())
            {
                CtorDefaultValue::Object(internal.render())
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
    pub internal: TypeName,
    pub props: Vec<(String, Ty, bool)>, // backing-field properties (name, type, is_var)
    /// Full primary-constructor parameter types in order (includes non-property params).
    pub ctor_params: Vec<Ty>,
    /// Primary-constructor parameter NAMES, in order, and whether each declares a default. Needed to
    /// map a named-argument call (`C(b = 9)`) onto positions from ANY file in the module — the AST
    /// declaration is only reachable from the file that declares it.
    pub ctor_param_names: Vec<(String, bool)>,
    pub methods: MethodMap,
    /// True if this is an `interface` (calls dispatch via `invokeinterface`).
    pub is_interface: bool,
    /// True if declared `object` (a singleton).
    pub is_object: bool,
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
    pub inner_of: Option<TypeName>,
    /// `companion object` functions, emitted as `static` methods and called as `ClassName.fn(...)`.
    pub static_methods: HashMap<String, Signature>,
    /// The names in [`Self::static_methods`] that are SOURCE `companion object` functions, as opposed
    /// to signatures a compiler plugin synthesizes into `static_methods` (kotlinx.serialization's
    /// `serializer()`). A `ClassName.fn(...)` call lowers to `getstatic Companion; invokevirtual` only
    /// for a source companion function; a plugin-owned name is left to the plugin's own emit path.
    pub companion_fun_names: std::collections::HashSet<String>,
    /// `companion object` properties, emitted as `static final` fields read as `ClassName.PROP`.
    pub static_props: HashMap<String, Ty>,
    /// Names of `lateinit` properties (instance and companion) — reads emit a null-check that throws.
    pub lateinit_props: std::collections::HashSet<String>,
    /// Internal names of interfaces this type implements (for subtyping).
    pub interfaces: crate::types::TypeNameList,
    /// Internal name of the base class (`: Base(..)`), if any.
    pub super_internal: Option<TypeName>,
    /// The parameter types of the base constructor that this class's `super(args)` targets, as the
    /// CHECKER resolved it (uniformly for a same-file, module, or classpath base, via the symbol
    /// source). Empty when the class has no base arguments. The lowerer emits `super(args)` against
    /// these instead of re-resolving the constructor itself.
    pub super_ctor_params: Vec<Ty>,
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
    /// Each class type parameter's BOUND erasure, parallel to `tparam_names` (`<T: Int>` → `Int`,
    /// `<T: Box<Int>>` → `Box`, unbounded → `Any`). A `generic_props` read on a receiver whose type
    /// arguments were NOT recorded (a raw `Obj(C, [])`) types at the bound instead of `Any`, so a
    /// chained read keeps resolving.
    pub tparam_bound_erasures: Vec<Ty>,
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
    /// Visibility of each MEMBER property that is not `public`, keyed by name (a `public`/absent entry
    /// is the default). Read by the resolver's access check to reject a `private` member read from
    /// outside the declaring class. Only body properties are recorded; primary-constructor properties
    /// default to `public`.
    pub prop_visibility: HashMap<String, Visibility>,
    /// Visibility of each MEMBER function that is not `public`, keyed by name — the function analogue of
    /// [`Self::prop_visibility`], read by the same access check for a member call.
    pub fn_visibility: HashMap<String, Visibility>,
}

/// The un-erased declared shape of a generic higher-order method, retained so a call site can
/// substitute the receiver's type arguments and infer the method's own type parameters (mirrors the
/// signature-`Ty` unify/substitute machinery, but built from the source `TypeRef`s of a user-declared method).
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
    pub fn internal_name(&self) -> TypeName {
        self.internal
    }

    pub fn internal(&self) -> String {
        self.internal.render()
    }

    pub fn internal_matches(&self, internal: &str) -> bool {
        self.internal.matches(internal)
    }

    pub fn inner_of_name(&self) -> Option<TypeName> {
        self.inner_of
    }

    pub fn inner_of_matches(&self, internal: &str) -> bool {
        self.inner_of.is_some_and(|outer| outer.matches(internal))
    }

    pub fn super_internal_name(&self) -> Option<TypeName> {
        self.super_internal
    }

    pub fn interface_names(&self) -> impl Iterator<Item = TypeName> + '_ {
        self.interfaces.iter_ids()
    }

    pub fn prop(&self, name: &str) -> Option<(Ty, bool)> {
        self.props
            .iter()
            .find_map(|(n, t, v)| (n == name).then_some((*t, *v)))
    }

    pub fn single_method(&self) -> Option<&Signature> {
        let mut all = self.methods.values().flatten();
        match (all.next(), all.next()) {
            (Some(one), None) => Some(one),
            _ => None,
        }
    }

    /// The SOLE signature declared under `name` — `None` when the name is absent OR overloaded
    /// (an overloaded name needs [`Self::method_matching`] with the call's argument types; callers
    /// without arguments in hand must not guess an overload).
    pub fn method(&self, name: &str) -> Option<&Signature> {
        match self.methods.get(name)?.as_slice() {
            [one] => Some(one),
            _ => None,
        }
    }

    /// The overload of `name` matching `args` (same arity/assignability scoring as top-level
    /// [`pick_overload`]); ambiguity resolves to `None` so the caller skips rather than misdispatches.
    pub fn method_matching(&self, name: &str, args: &[Ty]) -> Option<&Signature> {
        let sigs = self.methods.get(name)?;
        pick_overload(sigs, args).map(|i| &sigs[i])
    }

    /// All signatures declared under `name`, in declaration order (empty slice if none).
    pub fn methods_named(&self, name: &str) -> &[Signature] {
        self.methods.get(name).map_or(&[], Vec::as_slice)
    }

    pub fn has_method(&self, name: &str) -> bool {
        self.methods.get(name).is_some_and(|v| !v.is_empty())
    }

    /// Append an overload under `name` (declaration order preserved per name).
    pub fn add_method(&mut self, name: String, sig: Signature) {
        self.methods.entry(name).or_default().push(sig);
    }
}

/// Per-name overload lists (declaration order within a name). One `Vec` per name keeps the common
/// single-overload case cheap while letting member overloading resolve by argument types.
pub type MethodMap = HashMap<String, Vec<Signature>>;

/// Narrow a member-overload candidate list to the overloads that FIT the call's argument types —
/// the member analog of [`pick_overload`]. Keeps every candidate tied for the best score, in input
/// (DFS, most-derived-first) order, so an override chain (same signature on several hierarchy
/// rungs) still resolves to the most-derived rung and the caller's defaults-preference logic still
/// applies. Named-argument calls are left untouched (the named-mapping path validates arity
/// itself). Returns an EMPTY list when an erased-`Any` argument sits at a position where viable
/// candidates' parameter types differ — krusty cannot reproduce kotlinc's precise-type selection
/// there, and an unresolved member (skip) beats a misdispatch.
/// Per-position score of `params` against `arg_tys` (2 exact, 1 assignable), `None` if any position is
/// not assignable. The shared scoring kernel of `pick_overload` and `pick_member_overloads` — the
/// selection rule must not drift between the top-level and member paths.
fn positional_score(params: &[Ty], arg_tys: &[Ty]) -> Option<usize> {
    let mut sc = 0;
    for (&p, &a) in params.iter().zip(arg_tys.iter()) {
        if !arg_assignable_simple(p, a) {
            return None;
        }
        sc += if p == a { 2 } else { 1 };
    }
    Some(sc)
}

/// Soundness guard shared by `pick_overload` and `pick_member_overloads`: krusty erases generics, so a
/// generic value reads as `kotlin/Any`. An erased-`Any` ARGUMENT at a position where the candidates'
/// parameter types DIFFER defeats selection — kotlinc selects on the precise type krusty no longer has —
/// so the caller must bail (leave unresolved / skip, never dispatch wrongly).
fn erased_arg_defeats_selection<'a>(
    arg_tys: &[Ty],
    param_lists: impl Iterator<Item = &'a [Ty]> + Clone,
) -> bool {
    for (i, &a) in arg_tys.iter().enumerate() {
        if a.is_erased_top() {
            let mut params_here = param_lists.clone().filter_map(|ps| ps.get(i));
            if let Some(first) = params_here.next() {
                if params_here.any(|p| p != first) {
                    return true;
                }
            }
        }
    }
    false
}

fn pick_member_overloads(
    members: Vec<crate::libraries::LibraryMember>,
    arg_tys: &[Ty],
    named: bool,
) -> Vec<crate::libraries::LibraryMember> {
    use std::collections::HashSet;
    let distinct: HashSet<&Vec<Ty>> = members.iter().map(|m| &m.params).collect();
    if members.len() <= 1 || named || distinct.len() <= 1 {
        return members;
    }
    let arity_ok = |m: &crate::libraries::LibraryMember| {
        if m.call_sig.vararg {
            arg_tys.len() + 1 >= m.params.len()
        } else {
            arg_tys.len() == m.params.len()
                || (arg_tys.len() < m.params.len()
                    && m.call_sig.can_map_omitted_args(m.params.len()))
        }
    };
    let viable: Vec<&crate::libraries::LibraryMember> =
        members.iter().filter(|m| arity_ok(m)).collect();
    if viable.is_empty() {
        return members; // keep the original list so the arity diagnostic names the call shape
    }
    // TRUE SIBLINGS (two overloads on the SAME owner) whose params differ at a position where
    // either side is an erased type variable (`foo(x: T)` vs `foo(x: A<T>)` — both erase, but
    // kotlinc selects on the SUBSTITUTED types krusty no longer has) → unresolved (skip, never
    // wrong). Candidates on DIFFERENT owners are an override CHAIN (`Z.foo(String)` over
    // `A<T>.foo(T)`) — the most-derived-first order below handles those, erasure differences and
    // all.
    for (vi, m) in viable.iter().enumerate() {
        for other in &viable[vi + 1..] {
            if m.owner != other.owner || m.params == other.params {
                continue;
            }
            let erased_involved = m
                .params
                .iter()
                .zip(&other.params)
                .any(|(p, q)| p != q && (p.is_erased_top() || q.is_erased_top()));
            if erased_involved {
                return Vec::new();
            }
        }
    }
    if erased_arg_defeats_selection(arg_tys, viable.iter().map(|m| m.params.as_slice())) {
        return Vec::new();
    }
    let score = |m: &crate::libraries::LibraryMember| -> Option<usize> {
        if m.params.len() != arg_tys.len() {
            return Some(1); // omitted-arg/vararg candidate: viable but never beats an exact fit
        }
        // +2 baseline: any exact-arity assignable fit outranks every omitted-arg/vararg candidate.
        positional_score(&m.params, arg_tys).map(|sc| sc + 2)
    };
    let scored: Vec<(usize, &crate::libraries::LibraryMember)> = viable
        .iter()
        .filter_map(|&m| score(m).map(|sc| (sc, m)))
        .collect();
    let Some(&(best, _)) = scored.iter().max_by_key(|&&(sc, _)| sc) else {
        return members; // nothing assignable: keep the list for the ordinary type-error diagnostics
    };
    scored
        .into_iter()
        .filter(|&(sc, _)| sc == best)
        .map(|(_, m)| m.clone())
        .collect()
}

/// Simple type name → JVM internal name, split into a SHARED read-only base (the library/classpath
/// type universe — tens of thousands of stdlib+JDK names, identical for every file on a classpath) and
/// a small per-file `user` overlay (the file's own classes + type aliases). Lookups check `user` first
/// (a user class shadows a classpath type of the same name), then the shared `base`. This avoids
/// cloning the whole base map per compilation — the dominant `collect_signatures` cost before — by
/// sharing it via `Rc`.
#[derive(Clone, Default)]
pub struct ClassNames {
    base: std::rc::Rc<HashMap<String, TypeName>>,
    user: HashMap<String, TypeName>,
}

impl ClassNames {
    pub fn new(base: std::rc::Rc<HashMap<String, TypeName>>) -> ClassNames {
        ClassNames {
            base,
            user: HashMap::new(),
        }
    }
    pub fn get(&self, k: &str) -> Option<TypeName> {
        self.user
            .get(k)
            .copied()
            .or_else(|| self.base.get(k).copied())
    }
    pub fn contains_key(&self, k: &str) -> bool {
        self.user.contains_key(k) || self.base.contains_key(k)
    }
    /// Does some registered simple name resolve to this JVM internal name? Used to confirm a
    /// qualified nested supertype (`Foo/Bar` → `Foo$Bar`) names a REAL declared class.
    pub fn has_internal(&self, internal: &str) -> bool {
        existing_type_name(internal).is_some_and(|internal| {
            self.user.values().any(|&v| v == internal) || self.base.values().any(|&v| v == internal)
        })
    }
    pub fn library_companion_const(
        &self,
        src: &dyn SymbolSource,
        type_name: &str,
        const_name: &str,
    ) -> Option<crate::libraries::LibraryConst> {
        let fallback = self
            .get(type_name)
            .unwrap_or_else(|| crate::types::type_name(&format!("kotlin/{type_name}")));
        src.resolve_type_name(fallback)
            .and_then(|t| t.companion_consts.get(const_name).copied())
    }
    pub fn insert(&mut self, k: String, v: impl AsRef<str>) -> Option<TypeName> {
        self.user.insert(k, crate::types::type_name(v.as_ref()))
    }
    pub fn insert_name(&mut self, k: String, v: TypeName) -> Option<TypeName> {
        self.user.insert(k, v)
    }
}

/// A collected TOP-LEVEL extension property: its declared type, mutability, and the source
/// declaration key (file index, decl id) — how the backend's facade-metadata builder matches a
/// declaration to its record without guessing by name (same-name properties may differ by receiver).
pub struct ExtPropSig {
    pub ty: Ty,
    pub is_var: bool,
    pub source: (u32, u32),
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
    pub libraries: Box<dyn SemanticPlatform>,
    /// Top-level extension functions: (erased receiver, method_name) → its overloads. The receiver is
    /// its [`Ty::erased_recv`] key (nullability/generics/type-params folded). Used to resolve
    /// `recv.method(args)` when no instance method matches. A `(recv, name)` may carry SEVERAL
    /// overloads that differ by parameter list (`fun IntArray.f()` and `fun IntArray.f(i: Int)`); only
    /// a true erased-parameter duplicate is a real JVM collision (rejected at collection).
    pub ext_funs: HashMap<(Ty, String), Vec<Signature>>,
    /// Top-level extension properties: (erased receiver, prop_name) → (type, is_var). The
    /// getter/setter are emitted as static `getName(Recv)`/`setName(Recv, T)` methods.
    pub ext_props: HashMap<(Ty, String), ExtPropSig>,
    /// Simple type name → JVM internal name: every resolvable reference type — user/classpath
    /// classes, classpath `TypeAliasesKt` aliases, and the ported `JavaToKotlinClassMap`
    /// built-ins. The single source of truth for "does this type name resolve, and to what".
    pub class_names: ClassNames,
    /// Top-level function name → the facade class it lives on (`helper` → `pkg/AKt`), for the WHOLE
    /// multi-file compilation. Populated only by the multi-file driver (which knows each file's
    /// stem/facade); empty for single-file/in-process callers. Lets `lower_file` emit a call to a
    /// function defined in ANOTHER file as a cross-facade `invokestatic` (`Callee::CrossFile`) instead
    /// of bailing. A function defined in the file being lowered is resolved locally first.
    pub fn_facades: HashMap<String, TypeName>,
    /// Top-level function source declaration → declaring facade. This is the declaration-keyed
    /// equivalent of [`Self::fn_facades`], used once the checker has selected a concrete overload.
    pub fn_facades_by_decl: HashMap<(u32, u32), TypeName>,
    /// Top-level property name → `(facade_internal, type, is_var)` across the WHOLE multi-file
    /// compilation. Populated only by the multi-file driver. A read of a property from ANOTHER file
    /// lowers to `invokestatic <facade>.getX()` (the field is private), a write to `setX(v)`. Empty for
    /// single-file callers; a property in the file being lowered is resolved locally (its static) first.
    pub prop_facades: HashMap<String, (TypeName, Ty, bool, bool)>,
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
            fn_facades: HashMap::new(),
            fn_facades_by_decl: HashMap::new(),
            prop_facades: HashMap::new(),
        }
    }
}

impl SymbolTable {
    pub fn insert_class(&mut self, name: String, sig: ClassSig) -> Option<ClassSig> {
        self.insert_class_sig(name, sig)
    }

    pub fn insert_class_sig(&mut self, name: String, sig: ClassSig) -> Option<ClassSig> {
        self.classes.insert(name, sig)
    }

    /// Resolve an extension-property read/write `recv.name` to its `(type, is_var)`.
    ///
    /// A property whose receiver is a free type parameter — `val <T> T.p` or `val <T> Array<T>.p` —
    /// erases its receiver (or array element) to `kotlin/Any`, so an exact `erased_recv` key never
    /// matches a concrete receiver like `String` or `Array<Int>`. Model the type-parameter's implicit
    /// `Any` upper bound by falling back from the exact key to progressively-generalized ones: the
    /// array-element-generalized key, then the bare `Any` receiver. Concrete overloads still win first.
    pub fn ext_prop(&self, recv: Ty, name: &str) -> Option<(Ty, bool)> {
        recv.erased_recv_candidates()
            .into_iter()
            .find_map(|k| self.ext_props.get(&(k, name.to_string())))
            .map(|s| (s.ty, s.is_var))
    }

    pub fn single_fun(&self, name: &str) -> Option<Signature> {
        self.funs.get(name).and_then(|v| match v.as_slice() {
            [sig] => Some(sig.clone()),
            _ => None,
        })
    }

    pub fn fun_by_params(&self, name: &str, params: &[Ty]) -> Option<&Signature> {
        let sigs = self.funs.get(name)?;
        sigs.iter()
            .find(|sig| sigs.len() == 1 || sig.params == params)
    }

    fn fun_ret_by_erased_params(&self, name: &str, params: &[ErasedTypeKey]) -> Option<Ty> {
        let overloads = self.funs.get(name)?;
        overloads
            .iter()
            .find(|sig| overloads.len() == 1 || erased_params_semantic_key(sig) == params)
            .map(|sig| sig.ret)
    }

    /// Resolve a class reference type `Ty::Obj` back to its declaration (by internal name).
    pub fn class_by_internal(&self, internal: &str) -> Option<&ClassSig> {
        existing_type_name(internal).and_then(|internal| self.class_by_type_name(internal))
    }

    pub fn class_by_type_name(&self, internal: TypeName) -> Option<&ClassSig> {
        self.classes.values().find(|sig| sig.internal == internal)
    }

    pub fn class_simple_name(&self, internal: TypeName) -> Option<&str> {
        self.classes
            .iter()
            .find_map(|(name, sig)| (sig.internal == internal).then_some(name.as_str()))
    }

    /// The extension-function overloads registered for a receiver + name (empty if none). The receiver
    /// is folded to its [`Ty::erased_recv`] key, matching how they are stored at collection.
    pub fn ext_fun_overloads(&self, recv: Ty, name: &str) -> &[Signature] {
        self.ext_funs
            .get(&(recv.erased_recv(), name.to_string()))
            .map_or(&[], |v| v.as_slice())
    }

    /// The first extension overload for `(recv, name)`, or `None`. Most direct-read sites resolve a
    /// single extension (or only need a representative overload — the arity/argument disambiguation for
    /// a genuine call runs through the `SymbolResolver`'s `receiver_extensions`), so they read the
    /// first. A site that must pick by argument arity iterates [`Self::ext_fun_overloads`].
    pub fn ext_fun(&self, recv: Ty, name: &str) -> Option<&Signature> {
        self.ext_fun_overloads(recv, name).first()
    }

    pub fn class_by_internal_mut(&mut self, internal: &str) -> Option<&mut ClassSig> {
        let internal = existing_type_name(internal)?;
        self.class_by_type_name_mut(internal)
    }

    pub fn class_by_type_name_mut(&mut self, internal: TypeName) -> Option<&mut ClassSig> {
        self.classes
            .values_mut()
            .find(|sig| sig.internal == internal)
    }

    /// A method (own or inherited up the base-class chain) on a class internal name.
    pub fn method_of(&self, internal: &str, name: &str) -> Option<Signature> {
        self.method_of_with_owner(internal, name)
            .map(|(_, sig)| sig)
    }

    pub fn method_of_name(&self, internal: TypeName, name: &str) -> Option<Signature> {
        self.method_of_with_owner_name(internal, name)
            .map(|(_, sig)| sig)
    }

    /// Whether every supertype of `internal` (transitively) is a MODULE class — i.e. the
    /// supertype member set visible to [`Self::supertype_methods_name`] is COMPLETE. A hierarchy
    /// touching a classpath type has members that walk cannot see, so completeness-based checks
    /// (an `override` must override something) must not fire.
    pub fn hierarchy_is_module_closed(&self, internal: TypeName) -> bool {
        self.hierarchy_is_module_closed_inner(internal, &mut std::collections::HashSet::new())
    }

    fn hierarchy_is_module_closed_inner(
        &self,
        internal: TypeName,
        seen: &mut std::collections::HashSet<TypeName>,
    ) -> bool {
        if !seen.insert(internal) {
            return true; // a supertype cycle is ill-formed source; don't recurse forever on it
        }
        let Some(c) = self.class_by_type_name(internal) else {
            // `kotlin/Any` / `java/lang/Object` roots aren't module classes but add no members a
            // source `override` could target beyond the universal ones.
            let n = internal.render();
            return n == "kotlin/Any" || n == "java/lang/Object";
        };
        c.super_internal
            .into_iter()
            .chain(c.interfaces.iter_ids())
            .all(|p| self.hierarchy_is_module_closed_inner(p, seen))
    }

    /// The overload of `name` on `internal` (or up its base chain) that could OVERRIDE a supertype
    /// method with parameters `want_params` — same arity, each param equal or erasable to the
    /// super's `Object`. A same-name overload with an unrelated shape is a SIBLING, not an
    /// override, and must not be paired (`None` = the super method is inherited untouched).
    pub fn override_impl_of_name(
        &self,
        internal: TypeName,
        name: &str,
        want_params: &[Ty],
    ) -> Option<Signature> {
        let obj = Ty::obj("kotlin/Any");
        let mut seen = std::collections::HashSet::new();
        let mut cur = Some(internal);
        while let Some(ci_name) = cur {
            if !seen.insert(ci_name) {
                return None; // supertype cycle (ill-formed source) — stop
            }
            let c = self.class_by_type_name(ci_name)?;
            if let Some(found) = c.methods_named(name).iter().find(|sig| {
                sig.params.len() == want_params.len()
                    && want_params
                        .iter()
                        .zip(&sig.params)
                        .all(|(e, c)| e == c || *e == obj)
            }) {
                return Some(found.clone());
            }
            cur = c.super_internal;
        }
        None
    }

    /// A method (own or inherited up the base-class chain) with the internal name of the class that
    /// declares it. Call resolution records this owner so lowering can dispatch to a cross-file base method
    /// instead of pretending the receiver class declares it.
    pub fn method_of_with_owner(&self, internal: &str, name: &str) -> Option<(String, Signature)> {
        let internal = existing_type_name(internal)?;
        self.method_of_with_owner_name(internal, name)
            .map(|(owner, sig)| (owner.render(), sig))
    }

    pub fn method_of_with_owner_name(
        &self,
        internal: TypeName,
        name: &str,
    ) -> Option<(TypeName, Signature)> {
        let c = self.class_by_type_name(internal)?;
        if let Some(sigs) = c.methods.get(name) {
            // Overloaded name: there is no single "the method" — the caller must select by
            // argument types (`method_matching`); returning one arbitrarily would misdispatch.
            return match sigs.as_slice() {
                [one] => Some((c.internal_name(), one.clone())),
                _ => None,
            };
        }
        self.method_of_with_owner_name(c.super_internal?, name)
    }

    /// Whether `internal`'s method `name` (or one inherited up the base chain) is `vararg` — a
    /// clone-free probe for the hot call paths, which only need the flag (`method_of` clones the whole
    /// `Signature`, an allocation per call when used merely to read one bool).
    pub fn method_is_vararg(&self, internal: &str, name: &str) -> bool {
        existing_type_name(internal)
            .is_some_and(|internal| self.method_is_vararg_name(internal, name))
    }

    pub fn method_is_vararg_name(&self, internal: TypeName, name: &str) -> bool {
        let Some(c) = self.class_by_type_name(internal) else {
            return false;
        };
        if let Some(sigs) = c.methods.get(name) {
            // The flag is only trustworthy when the name has a sole overload.
            return matches!(sigs.as_slice(), [one] if one.vararg);
        }
        c.super_internal
            .is_some_and(|s| self.method_is_vararg_name(s, name))
    }

    /// All method signatures inherited from declared supertypes (base-class chain + interfaces,
    /// recursively) as `(name, signature)`. Used to detect overrides that would need a JVM bridge
    /// method (covariant/generic return), which krusty does not synthesize.
    pub fn supertype_methods(&self, internal: &str) -> Vec<(String, Signature)> {
        let mut out = Vec::new();
        if let Some(internal) = existing_type_name(internal) {
            self.collect_super_methods(internal, &mut out);
        }
        out
    }

    pub fn supertype_methods_name(&self, internal: TypeName) -> Vec<(String, Signature)> {
        let mut out = Vec::new();
        self.collect_super_methods(internal, &mut out);
        out
    }

    fn collect_super_methods(&self, internal: TypeName, out: &mut Vec<(String, Signature)>) {
        let Some(c) = self.class_by_type_name(internal) else {
            return;
        };
        let mut parents: Vec<TypeName> = Vec::new();
        if let Some(s) = c.super_internal {
            parents.push(s);
        }
        parents.extend(c.interfaces.iter_ids());
        for p in parents {
            if let Some(pc) = self.class_by_type_name(p) {
                for (n, sigs) in &pc.methods {
                    for sig in sigs {
                        out.push((n.clone(), sig.clone()));
                    }
                }
            }
            self.collect_super_methods(p, out);
        }
    }

    pub fn supertype_internal_names(&self, internal: &str) -> Vec<TypeName> {
        let mut out = Vec::new();
        if let Some(internal) = existing_type_name(internal) {
            out = self.supertype_internal_names_from(internal);
        }
        out
    }

    pub fn supertype_internal_names_from(&self, internal: TypeName) -> Vec<TypeName> {
        let mut out = Vec::new();
        self.collect_super_internals(internal, &mut out);
        out
    }

    fn collect_super_internals(&self, internal: TypeName, out: &mut Vec<TypeName>) {
        let Some(c) = self.class_by_type_name(internal) else {
            return;
        };
        let mut parents: Vec<TypeName> = Vec::new();
        if let Some(s) = c.super_internal {
            parents.push(s);
        }
        parents.extend(c.interfaces.iter_ids());
        for p in parents {
            if !out.contains(&p) {
                out.push(p);
                self.collect_super_internals(p, out);
            }
        }
    }

    pub fn subclass_names_of(&self, internal: TypeName) -> Vec<TypeName> {
        // Direct subtypes: a subclass names the sealed base in `super_internal` (`class B : S()`),
        // but an implementer of a sealed INTERFACE names it in `interfaces` (`class B : S`) — cover
        // both, so a `when` over a sealed interface is proven exhaustive like a sealed class.
        self.classes
            .values()
            .filter(|c| {
                c.super_internal == Some(internal) || c.interfaces.iter_ids().any(|i| i == internal)
            })
            .map(ClassSig::internal_name)
            .collect()
    }

    /// A property (own or inherited) on a class internal name. Returns `(type, is_var)`.
    pub fn prop_of(&self, internal: &str, name: &str) -> Option<(Ty, bool)> {
        let internal = existing_type_name(internal)?;
        self.prop_of_name(internal, name)
    }

    pub fn prop_of_name(&self, internal: TypeName, name: &str) -> Option<(Ty, bool)> {
        let c = self.class_by_type_name(internal)?;
        if let Some(p) = c.prop(name) {
            return Some(p);
        }
        self.prop_of_name(c.super_internal?, name)
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
        u if u.is_unsigned() => u.scalar_value_repr().unwrap(),
        Ty::String => Ty::obj("kotlin/String"),
        Ty::Obj(n, args) if n.matches("kotlin/Array") => {
            let e = args
                .first()
                .copied()
                .unwrap_or_else(|| Ty::obj("kotlin/Any"));
            Ty::obj_args("kotlin/Array", &[erased_key_ty(erased_type_key(e))])
        }
        Ty::Obj(n, _) => Ty::Obj(n, &[]),
        Ty::Null | Ty::Nothing | Ty::Error => Ty::obj("kotlin/Any"),
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
    if erased_arg_defeats_selection(arg_tys, cands.iter().map(|&c| sigs[c].params.as_slice())) {
        return None;
    }
    cands
        .iter()
        .filter_map(|&i| positional_score(&sigs[i].params, arg_tys).map(|sc| (sc, i)))
        .max_by_key(|&(sc, _)| sc)
        .map(|(_, i)| i)
        .or_else(|| cands.first().copied())
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
        match e {
            Expr::Name(n) => {
                out.insert(n.clone());
            }
            // Type names in EXPRESSION position — a cast (`x as T`), a type test (`x is T`), or a catch
            // clause (`catch (e: T)`) — are just as much candidates as a declared parameter type, and are
            // the only place some names (a caught `NotImplementedError`) appear. The signature phase's
            // `ty_of_ref` / `catch_internal` resolve them through the same `class_names`, so they must be
            // import-resolved here too.
            Expr::Is { ty, .. } | Expr::As { ty, .. } => collect_typeref_names(ty, out),
            Expr::Try { catches, .. } => {
                for c in catches {
                    collect_typeref_names(&c.ty, out);
                }
            }
            _ => {}
        }
    }
    // A local declaration's type annotation (`val r: Reg`, `lateinit var x: T`) is a type-position name
    // too, and may be the only place a name appears (a classpath alias used only for a local `val`).
    for s in &file.stmt_arena {
        match s {
            Stmt::LocalLateinit { ty, .. } => collect_typeref_names(ty, out),
            Stmt::Local { ty: Some(ty), .. } | Stmt::LocalDelegate { ty: Some(ty), .. } => {
                collect_typeref_names(ty, out)
            }
            Stmt::LocalFun(f) => fun_names(f, file, out),
            _ => {}
        }
    }
    // Explicit call type arguments (`foo<Bar>()`, `arrayOf<Baz>()`) are type-position names too.
    for targs in file.call_type_args.values() {
        for t in targs {
            collect_typeref_names(t, out);
        }
    }
    // A `typealias A = Foo` TARGET is a candidate: alias expansion resolves `A` by looking up `Foo` in
    // the resolved names, so `Foo` must itself be import-resolved (it may not appear in any other type
    // position). Both simple (`Foo`) and dotted (`a.b.Foo`) targets go in; the resolver tries each.
    for (_, target) in &file.type_aliases {
        out.insert(target.clone());
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

/// A file's star/implicit import packages (internal form) grouped by kotlinc's descending precedence:
/// L1 the file's own package (same-package), L2 explicit star imports (`import a.b.*`), L3 the Kotlin
/// default imports, L4 the PLATFORM default imports (`java.lang`, `kotlin.jvm`). Kotlin defaults outrank
/// platform defaults so a name declared in BOTH (`Comparable`, `Number`, `CharSequence` — in `kotlin.*`
/// AND `java.lang.*`) binds to the Kotlin one, exactly as kotlinc, rather than looking ambiguous. The
/// leveled form the spec's name resolution walks — the single source both the signature pass and the
/// [`Checker`] build their import set from.
fn import_levels(file: &File, platform_defaults: &[&str]) -> [Vec<TypeName>; 4] {
    let own = match &file.package {
        Some(p) => type_name(&p.replace('.', "/")),
        None => type_name(""),
    };
    let explicit_star: Vec<TypeName> = file
        .imports
        .iter()
        .filter_map(|fq| {
            fq.strip_suffix(".*")
                .map(|p| type_name(&p.replace('.', "/")))
        })
        .collect();
    let kotlin_defaults: Vec<TypeName> = KOTLIN_DEFAULT_IMPORT_PACKAGES
        .iter()
        .map(|s| type_name(&s.replace('.', "/")))
        .collect();
    let platform: Vec<TypeName> = platform_defaults
        .iter()
        .map(|s| type_name(&s.replace('.', "/")))
        .collect();
    [vec![own], explicit_star, kotlin_defaults, platform]
}

/// Resolve a name to its fully-qualified internal name against a file's import set — the single
/// kotlinc-conforming resolver both the signature pass and the [`Checker`] use, so a name resolves
/// identically wherever it appears. A name is ALWAYS an FQN; an unqualified use forms candidate FQNs
/// `pkg/name` from the in-scope packages. Precedence (spec § resolution): an explicit non-star import
/// names the FQN directly (highest); otherwise each precedence level (`levels`, descending) supplies
/// candidates and the FIRST level with any resolving candidate wins. Two or more DISTINCT resolutions
/// within one level are AMBIGUOUS — kotlinc rejects a name two star-imports both supply — and resolve
/// to `None` (a genuinely ambiguous name never appears in a compiling program). A classpath `typealias`
/// candidate resolves to its target internal. Existence is verified via `resolve_type`.
fn resolve_name_against_imports_name(
    name: &str,
    explicit: &HashMap<String, String>,
    levels: &[Vec<TypeName>],
    source: &dyn SymbolSource,
) -> Option<TypeName> {
    if let Some(fq) = explicit.get(name) {
        // A nested-type import (`import lib.Outer.Ws` → `lib/Outer$Ws`) resolves through the flat form.
        if let Some(internal) = resolve_nested_internal_name(fq, source) {
            return Some(internal);
        }
    }
    for level in levels {
        // The CLASSIFIER namespace of the shared unqualified-name resolution loop: each in-scope package
        // yields a candidate fqn's namespace record; a type position consumes only its `classifier`. The
        // level-precedence + within-level ambiguity is applied HERE (the caller's own rule), the record
        // keeping classifier separate from callables so a coexisting `fun`/`val` never perturbs it.
        let mut hits: Vec<TypeName> = Vec::new();
        for (fqn, r) in crate::symbol_resolver::resolve_symbols_in_scope(source, name, level) {
            if let Some(t) = &r.classifier {
                let internal = t.alias_target.unwrap_or(fqn);
                if !hits.contains(&internal) {
                    hits.push(internal);
                }
            }
        }
        match hits.len() {
            0 => continue,
            1 => return hits.into_iter().next(),
            _ => return None, // ambiguous within this level — kotlinc rejects; leave unresolved
        }
    }
    None
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
/// here (not the richer `SemanticPlatform` `resolve_nested_internal` needs).
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

fn resolve_nested_internal(internal: &str, source: &dyn SymbolSource) -> Option<String> {
    resolve_nested_internal_name(internal, source).map(TypeName::render)
}

fn resolve_nested_internal_name(internal: &str, source: &dyn SymbolSource) -> Option<TypeName> {
    // A `typealias` resolves to its target internal, not to the (classless) alias name.
    let resolved = |name: &str| -> Option<TypeName> {
        let name = type_name(name);
        let t = source.resolve_type_name(name)?;
        Some(t.alias_target.unwrap_or(name))
    };
    if let Some(r) = resolved(internal) {
        return Some(r);
    }
    let mut cand = internal.to_string();
    while let Some(pos) = cand.rfind('/') {
        cand.replace_range(pos..=pos, "$");
        if let Some(r) = resolved(&cand) {
            return Some(r);
        }
    }
    None
}

fn resolve_dotted_classpath_type(
    name: &str,
    class_names: &ClassNames,
    imap: &HashMap<String, String>,
    wilds: &[String],
    libraries: &dyn SemanticPlatform,
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
            .map(TypeName::render)
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
    libraries: Box<dyn SemanticPlatform>,
    diags: &mut DiagSink,
) -> SymbolTable {
    let platform_default_imports = libraries.platform_default_import_packages();
    // Pass 1: every class simple-name -> internal name (no bodies, just the type universe). Nothing is
    // pre-seeded: every referenced type resolves through the file's imports and default packages below
    // (the import machinery), so a bare name binds ONLY to a default-import / imported / same-package class
    // — kotlinc semantics — never to an arbitrary classpath class. A classpath `typealias`
    // (`ArrayList` → `java/util/ArrayList`) resolves through the same probe: `resolve_type` returns the
    // alias's target.
    let mut class_names = ClassNames::new(std::rc::Rc::new(HashMap::new()));
    // A user-declared top-level class *shadows* any classpath/JDK type of the same simple name
    // (legal Kotlin — the JDK one would need an explicit import). Only a duplicate among the
    // user's own declarations is a conflict, so track which names the user has defined. The dedup
    // key is the package-qualified *internal* name, not the simple name: two classes sharing a
    // simple name in different packages (e.g. a test's root-package `EmptyContinuation` and the
    // injected `helpers.EmptyContinuation`) are distinct declarations, not a conflict.
    let mut user_defined: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (i, file) in files.iter().enumerate() {
        diags.set_file(i as u32);
        for &d in &file.decls {
            if let Decl::Class(c) = file.decl(d) {
                let internal = class_internal(file, &c.name);
                if !user_defined.insert(internal.clone()) {
                    diags.error(c.span, format!("conflicting declarations: {}", c.name));
                }
                class_names.insert(c.name.clone(), internal);
            }
        }
    }
    // (The library set's `seed` already merged the intrinsic Kotlin built-in → target class mapping,
    // e.g. the ported `JavaToKotlinClassMap`, beneath any classpath/user declarations.)

    // Resolve every referenced simple name through the file's imports — an explicit import, a
    // wildcard/default-import package that actually provides the type, or a dotted FQ. This is Kotlin's
    // import-driven name resolution (there is no global simple-name index): a bare name binds ONLY to a
    // default-import / imported / same-package class, verified to exist via `resolve_type`. Runs BEFORE
    // alias expansion so a user `typealias A = Foo` (Foo a classpath type) finds `Foo` already resolved.
    // A name imported INCONSISTENTLY across files (different full internals) is left unresolved (ambiguous).
    {
        let mut from_import: HashMap<String, Option<TypeName>> = HashMap::new();
        for file in files {
            let imap = import_map(file);
            let wilds = import_wildcards(file, platform_default_imports);
            let levels = import_levels(file, platform_default_imports);
            // Candidate simple names: every type referenced in the file (so a WILDCARD import can supply
            // it) plus the explicit-import names themselves.
            let mut names = std::collections::HashSet::new();
            collect_file_type_names(file, &mut names);
            names.extend(imap.keys().cloned());
            for name in names {
                if class_names.contains_key(name.as_str()) || user_defined.contains(&name) {
                    continue;
                }
                // Resolve the name to an FQN against the file's import set (explicit import, then the
                // implicit FQN candidates by kotlinc precedence) — the SAME resolver the checker uses.
                // Fall back to a dotted FQ / nested-under-prefix.
                let full = resolve_name_against_imports_name(&name, &imap, &levels, &*libraries)
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
                        .map(|internal| type_name(&internal))
                    })
                    .or_else(|| {
                        // A SAME-MODULE nested-type import (`import demo.Outer.Inner` → the hoisted
                        // class `demo/Outer$Inner`). The classpath-only `resolve_nested_internal_name`
                        // can't see a module-local nested class, so match the import path's `$`-flattened
                        // form (from the right, kotlinc's nesting separator) against a user-declared
                        // internal — nested classes are hoisted to top-level decls and recorded in
                        // `user_defined` during the class-name seed above.
                        imap.get(&name).and_then(|fq| {
                            let mut cand = fq.clone();
                            loop {
                                if user_defined.contains(&cand) {
                                    return Some(type_name(&cand));
                                }
                                match cand.rfind('/') {
                                    Some(pos) => cand.replace_range(pos..=pos, "$"),
                                    None => return None,
                                }
                            }
                        })
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
                class_names.insert_name(simple, full);
            }
        }
    }

    // Expand the input files' USER type aliases into class_names (classpath aliases already resolved
    // through the import pass, via `resolve_type`'s alias redirect).
    // `typealias A = B` where B is a user/classpath/import-resolved class → A resolves to the same internal.
    // `typealias A = Primitive` → A maps to `"__ty/<PrimName>"` (decoded in ty_of_ref).
    // `typealias A = java.lang.Foo` → A resolves to the JVM internal name `java/lang/Foo`.
    // Multiple passes handle chains: A = B, B = C.
    let mut alias_map: HashMap<String, String> = HashMap::new();
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
            if let Some(internal) = class_names.get(target.as_str()) {
                class_names.insert_name(alias.clone(), internal);
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
                                class_names.get(n)
                            });
                        fun_rets.insert(f.name.clone(), ty_of_ref(r, &class_names, &tp, diags));
                    }
                }
            }
        }
    }

    // Pass 2: resolve signatures/properties against the now-complete type universe.
    let mut table = SymbolTable::default();
    // Pre-seed object names from ALL files so a property-initializer's inference recognizes a
    // same-module `object` used as a value (`val h = Helper`) regardless of file order.
    for file in files {
        for &d in &file.decls {
            if let Decl::Class(c) = file.decl(d) {
                if c.is_object() {
                    table.objects.insert(c.name.clone());
                }
            }
        }
    }
    // Same-name top-level functions are kept as overloads; a real "conflicting declarations" clash
    // is a same-*package* same-erasure duplicate. Keyed by (package, name, erased params) so a
    // cross-package homonym (a star-imported function shadowed by a local one) is not a conflict.
    let mut seen_fun_keys: std::collections::HashSet<(String, String, Vec<ErasedTypeKey>)> =
        std::collections::HashSet::new();
    for (i, file) in files.iter().enumerate() {
        diags.set_file(i as u32);
        for &d in &file.decls {
            match file.decl(d) {
                Decl::Fun(f) => {
                    let tp = TParams::from_decl_with(&f.type_params, &f.type_param_bounds, &|n| {
                        class_names.get(n)
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
                                let t = infer_lit_ty_scoped(
                                    file,
                                    *e,
                                    &class_names,
                                    &fun_rets,
                                    &this_scope,
                                    &*libraries,
                                    &table,
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
                                if file.expr_uses_any_name(dx, &pnames) {
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
                        param_default_values: f
                            .params
                            .iter()
                            .map(|p| {
                                p.default.and_then(|dx| {
                                    extract_ctor_default(file, dx, &class_names, &*libraries)
                                })
                            })
                            .collect(),
                        param_names: f.params.iter().map(|p| p.name.clone()).collect(),
                        lambda_param_types,
                        lambda_recv: f.params.iter().map(|p| p.ty.fun_has_receiver).collect(),
                        is_inline: f.is_inline,
                        is_final: f.is_final,
                        is_suspend: f.is_suspend,
                        context_count: f.context_count,
                        source_decl: Some(d),
                        source_file: Some(i as u32),
                        package: file
                            .package
                            .as_deref()
                            .map(|p| p.replace('.', "/"))
                            .unwrap_or_default(),
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
                        // The recursion hazard applies to a receiver whose non-null type has a BUILTIN
                        // (or classpath) operator of this name — `String?.plus`, `Int?.inc`: the body's
                        // same-operator call on the non-null value routes back to the extension. A class
                        // declared IN THIS MODULE has no builtin operator, so a nullable-receiver
                        // operator extension on it (`operator fun MyClass?.inc()`) is the sole
                        // resolution and safe. (Checked via `file.decls`, NOT `class_names` — the latter
                        // also names default-imported builtins like `String`.)
                        let module_declared_recv = file.decls.iter().any(|&d| {
                            matches!(file.decl(d), crate::ast::Decl::Class(c) if c.name == recv_ref.name)
                        });
                        if recv_ref.nullable
                            && recv_ty.is_reference()
                            && is_operator
                            && !module_declared_recv
                        {
                            diags.error(f.span, "krusty: an operator extension on a nullable reference receiver is not supported".to_string());
                        } else {
                            // Overloading by ARITY: keep extensions of the same (erased receiver, name)
                            // that differ in parameter COUNT (`fun IntArray.f()` and `fun IntArray.f(i)`).
                            // The backend keys an extension's emitted method by arity, and the checker's
                            // overload picker selects by argument count, so an arity-distinct overload is
                            // dispatched unambiguously by BOTH phases. Two overloads of the SAME arity —
                            // distinguished only by parameter TYPE — cannot be told apart by the arity-keyed
                            // emit (they would collide to one method), so they are still rejected rather
                            // than risk dispatching to the wrong one.
                            let overloads = table
                                .ext_funs
                                .entry((recv_ty.erased_recv(), f.name.clone()))
                                .or_default();
                            let arity = sig.params.len();
                            if overloads.iter().any(|s| s.params.len() == arity) {
                                diags.error(f.span, "krusty: conflicting extension functions with the same erased receiver and name".to_string());
                            } else {
                                overloads.push(sig);
                            }
                        }
                    } else {
                        // Overloading: keep ALL same-name functions, keyed by name. Only an exact
                        // erased-parameter duplicate *in the same package* is a real conflict — a
                        // same-name/same-erasure function from another package (e.g. a star-imported
                        // `helpers.runBlocking` shadowed by a local top-level `runBlocking`) is a
                        // distinct declaration, not a clash. Use a Kotlin-level erasure key here
                        // instead of formatting JVM descriptors in the checker.
                        let key = erased_params_semantic_key(&sig);
                        let pkg = file.package.clone().unwrap_or_default();
                        let overloads = table.funs.entry(f.name.clone()).or_default();
                        if !seen_fun_keys.insert((pkg, f.name.clone(), key)) {
                            diags.error(f.span, format!("conflicting declarations: {}", f.name));
                        } else {
                            overloads.push(sig);
                        }
                    }
                }
                Decl::Class(c) => {
                    let internal = class_names
                        .get(&c.name)
                        .map(TypeName::render)
                        .unwrap_or_else(|| class_internal(file, &c.name));
                    // An `inner class` captures the enclosing instance, so the outer class's type
                    // parameters are in scope for its own member/ctor/field types (`inner class N :
                    // Iterator<T>` where `T` is the outer's parameter). Walk the `inner_of` chain and
                    // include each enclosing class's parameters (erased, like the class's own).
                    let mut ctp_names = c.type_params.clone();
                    {
                        let mut outer = c.inner_of.clone();
                        let mut guard = 0;
                        while let Some(on) = outer {
                            guard += 1;
                            if guard > 32 {
                                break;
                            }
                            if let Some(oc) = file
                                .decls
                                .iter()
                                .filter_map(|&d| match file.decl(d) {
                                    Decl::Class(x) => Some(x),
                                    _ => None,
                                })
                                .find(|x| x.name == on)
                            {
                                ctp_names.extend(oc.type_params.iter().cloned());
                                outer = oc.inner_of.clone();
                            } else {
                                break;
                            }
                        }
                    }
                    let ctp = TParams::erased(&ctp_names);
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
                                    // Kotlin nested-type scoping: the enclosing class's own nested type
                                    // SHADOWS a same-named top-level/imported type — insert unconditionally
                                    // (overwriting any top-level entry). Consistent with the checker's
                                    // `enclosing_nested_type` expression-path fallback.
                                    if !seg.contains('.') {
                                        let ni = class_names.get(&nc.name).unwrap_or_else(|| {
                                            type_name(&class_internal(file, &nc.name))
                                        });
                                        ext.insert_name(seg.to_string(), ni);
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
                    let ctor_param_names: Vec<(String, bool)> = c
                        .props
                        .iter()
                        .map(|p| (p.name.clone(), p.default.is_some()))
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
                                None => {
                                    let dt = infer_lit_ty_scoped(
                                        file,
                                        de,
                                        &class_names,
                                        &fun_rets,
                                        &init_scope,
                                        &*libraries,
                                        &table,
                                    );
                                    delegated_getvalue_ret_for_signature(
                                        file,
                                        &table,
                                        &*libraries,
                                        dt,
                                    )
                                    .unwrap_or(Ty::Error)
                                }
                            }
                        } else {
                            match (&bp.ty, &bp.getter) {
                                (Some(r), _) => ty_of_ref(r, &class_names, &ctp, diags),
                                (None, Some(FunBody::Expr(g))) if !c.is_value => {
                                    infer_lit_ty_scoped(
                                        file,
                                        *g,
                                        &class_names,
                                        &fun_rets,
                                        &init_scope,
                                        &*libraries,
                                        &table,
                                    )
                                }
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
                                        infer_lit_ty_scoped(
                                            file,
                                            i,
                                            &class_names,
                                            &fun_rets,
                                            &init_scope,
                                            &*libraries,
                                            &table,
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
                                        infer_lit_ty_scoped(
                                            file,
                                            i,
                                            &class_names,
                                            &fun_rets,
                                            &[],
                                            &*libraries,
                                            &table,
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
                                    &|n| class_names.get(n),
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
                                    class_names.get(n)
                                });
                            local_rets
                                .insert(m.name.clone(), ty_of_ref(r, &class_names, &mtp, diags));
                        }
                    }
                    let mut methods: MethodMap = MethodMap::new();
                    for (mname, msig) in c.methods.iter().map(|m| {
                        let mtp = ctp.extended_with(&m.type_params, &m.type_param_bounds, &|n| {
                            class_names.get(n)
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
                                    let t = infer_lit_ty_scoped(
                                        file,
                                        *e,
                                        &class_names,
                                        &local_rets,
                                        &scope,
                                        &*libraries,
                                        &table,
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
                    }) {
                        methods.entry(mname).or_default().push(msig);
                    }
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
                                vec![Signature {
                                    params: vec![],
                                    ret: *ty,
                                    vararg: false,
                                    required: 0,
                                    param_defaults: Vec::new(),
                                    param_default_values: Vec::new(),
                                    param_names: Vec::new(),
                                    lambda_param_types: Vec::new(),
                                    lambda_recv: Vec::new(),
                                    is_inline: false,
                                    is_final: true,
                                    is_suspend: false,
                                    context_count: 0,
                                    source_decl: None,
                                    source_file: None,
                                    package: String::new(),
                                }],
                            );
                        }
                        // Every `copy` parameter has a default (the receiver's property) — so `required`
                        // is 0 and any subset may be passed, by name or position.
                        methods.insert(
                            "copy".into(),
                            vec![Signature {
                                params: props.iter().map(|(_, t, _)| *t).collect(),
                                ret: self_ty,
                                vararg: false,
                                required: 0,
                                param_defaults: vec![true; props.len()],
                                param_default_values: Vec::new(),
                                param_names: props.iter().map(|(n, _, _)| n.clone()).collect(),
                                lambda_param_types: Vec::new(),
                                lambda_recv: Vec::new(),
                                is_inline: false,
                                is_final: true,
                                is_suspend: false,
                                context_count: 0,
                                source_decl: None,
                                source_file: None,
                                package: String::new(),
                            }],
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
                        let resolved = class_names
                            .get(s)
                            .map(TypeName::render)
                            // An erased type parameter used as a supertype (degenerate) stays as-is.
                            .or_else(|| ctp.contains(s).then(|| s.to_string()))
                            // A QUALIFIED nested supertype (`Foo.Bar` → `Foo/Bar` here) whose outer isn't a
                            // package: the nested class is registered as `Outer$Nested`. Accept the dollar
                            // form only when it names a REAL declared class (some registered simple name
                            // maps to it), so an unresolved name still errors.
                            .or_else(|| {
                                (s.contains('/') && class_names.has_internal(&s.replace('/', "$")))
                                    .then(|| s.replace('/', "$"))
                            })
                            // A supertype named by SIMPLE name from inside a nested class may be a SIBLING
                            // (or enclosing-scope) nested type: `class Outer { interface Foo; class Impl: Foo }`.
                            // Try the name qualified by each enclosing prefix of the current class
                            // (`Outer.Impl` → `Outer.Foo`, then walk further out).
                            .or_else(|| {
                                let mut prefix = c.name.as_str();
                                while let Some((p, _)) = prefix.rsplit_once('.') {
                                    if let Some(internal) = class_names.get(&format!("{p}.{s}")) {
                                        return Some(internal.render());
                                    }
                                    prefix = p;
                                }
                                None
                            });
                        match resolved {
                            Some(internal) => internal,
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
                    // SOURCE companion function names, captured before the plugin phase below injects
                    // any synthetic signature (`serializer()`) into `static_methods`.
                    let companion_fun_names: std::collections::HashSet<String> =
                        c.companion_methods.iter().map(|m| m.name.clone()).collect();
                    // `companion object` members → static methods/props on this class.
                    let mut static_methods: HashMap<String, Signature> = c
                        .companion_methods
                        .iter()
                        .map(|m| {
                            let mtp =
                                ctp.extended_with(&m.type_params, &m.type_param_bounds, &|n| {
                                    class_names.get(n)
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
                    // synthesized `serializer(): KSerializer<C>` signature before checking, so source
                    // references `C.serializer()` resolve. The plugin later chooses the physical placement
                    // (Companion for plain classes, static for the remaining supported shapes) before emit.
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
                                param_default_values: Vec::new(),
                                param_names: vec![],
                                lambda_param_types: vec![],
                                lambda_recv: vec![],
                                is_inline: false,
                                is_final: true,
                                is_suspend: false,
                                context_count: 0,
                                source_decl: None,
                                source_file: None,
                                package: String::new(),
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
                    // Each parameter's bound erasure (see `ClassSig::tparam_bound_erasures`); shares
                    // the bound-chasing logic every declaration-scope erasure uses.
                    let tparam_bound_erasures: Vec<Ty> = {
                        let tp =
                            TParams::from_decl_with(&c.type_params, &c.type_param_bounds, &|n| {
                                class_names.get(n)
                            });
                        tparam_names.iter().map(|n| tp.erase(n)).collect()
                    };
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
                    // Non-public member-property visibility, for the resolver's access check. Only body
                    // properties carry a source modifier; ctor properties default to public.
                    let prop_visibility: HashMap<String, Visibility> = c
                        .body_props
                        .iter()
                        .filter(|bp| bp.visibility != Visibility::Public)
                        .map(|bp| (bp.name.clone(), bp.visibility))
                        .collect();
                    // Non-public member-function visibility (the function analogue of `prop_visibility`).
                    let fn_visibility: HashMap<String, Visibility> = c
                        .methods
                        .iter()
                        .filter(|m| m.visibility != Visibility::Public)
                        .map(|m| (m.name.clone(), m.visibility))
                        .collect();
                    let comp_internal = format!("{internal}$Companion");
                    let has_companion_supertypes =
                        !companion_interfaces.is_empty() || companion_super_internal.is_some();
                    let internal_ref = type_name(&internal);
                    let inner_of_ref = inner_of.as_ref().map(|inner| type_name(inner));
                    let interfaces_ref: crate::types::TypeNameList = interfaces.into();
                    let super_internal_ref = super_internal
                        .as_ref()
                        .map(|super_internal| type_name(super_internal));
                    table.insert_class_sig(
                        c.name.clone(),
                        ClassSig {
                            internal: internal_ref,
                            props,
                            ctor_params,
                            methods,
                            is_interface: c.is_interface(),
                            is_object: c.is_object(),
                            is_abstract: c.is_abstract(),
                            is_fun_interface: c.is_fun_interface,
                            is_sealed: c.is_sealed(),
                            inner_of: inner_of_ref,
                            static_methods,
                            companion_fun_names,
                            static_props,
                            lateinit_props,
                            interfaces: interfaces_ref,
                            super_internal: super_internal_ref,
                            super_ctor_params: Vec::new(),
                            is_annotation: c.is_annotation(),
                            ctor_param_names,
                            ctor_defaults,
                            secondary_ctors,
                            tparam_names,
                            tparam_bound_erasures,
                            generic_props,
                            prop_visibility,
                            fn_visibility,
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
                    if has_companion_supertypes {
                        let comp_internal_ref = type_name(&comp_internal);
                        let companion_interfaces_ref: crate::types::TypeNameList =
                            companion_interfaces.into();
                        let companion_super_internal_ref = companion_super_internal
                            .as_ref()
                            .map(|super_internal| type_name(super_internal));
                        table.insert_class_sig(
                            comp_internal.clone(),
                            ClassSig {
                                internal: comp_internal_ref,
                                props: Vec::new(),
                                ctor_params: Vec::new(),
                                methods: companion_methods_sigs
                                    .into_iter()
                                    .map(|(n, sig)| (n, vec![sig]))
                                    .collect(),
                                is_interface: false,
                                is_object: false,
                                is_abstract: false,
                                is_fun_interface: false,
                                is_sealed: false,
                                inner_of: None,
                                static_methods: HashMap::new(),
                                companion_fun_names: std::collections::HashSet::new(),
                                static_props: HashMap::new(),
                                lateinit_props: Default::default(),
                                interfaces: companion_interfaces_ref,
                                super_internal: companion_super_internal_ref,
                                super_ctor_params: Vec::new(),
                                is_annotation: false,
                                ctor_param_names: Vec::new(),
                                ctor_defaults: Vec::new(),
                                secondary_ctors: Vec::new(),
                                tparam_names: Vec::new(),
                                tparam_bound_erasures: Vec::new(),
                                generic_props: HashMap::new(),
                                value_field: None,
                                generic_methods: HashMap::new(),
                                prop_visibility: HashMap::new(),
                                fn_visibility: HashMap::new(),
                            },
                        );
                    }
                }
                Decl::Property(p) => {
                    // Extension property `val Recv.name: T get() = …`: register by (erased receiver,
                    // name); emitted as a static `getName(Recv)`/`setName(Recv, T)`.
                    if let Some(recv_ref) = &p.receiver {
                        // An extension property's own type params (`val <T> Array<T>.length`) scope over
                        // the receiver and declared type — bind them (erased) so the receiver isn't a raw
                        // `Array` and `T` isn't mistaken for an unresolved class.
                        let resolve = |n: &str| class_names.get(n);
                        let ptp =
                            TParams::from_decl_with(&p.type_params, &p.type_param_bounds, &resolve);
                        let recv_ty = ty_of_ref(recv_ref, &class_names, &ptp, diags);
                        let ty =
                            p.ty.as_ref()
                                .map(|r| ty_of_ref(r, &class_names, &ptp, diags))
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
                            table.ext_props.insert(
                                key,
                                ExtPropSig {
                                    ty,
                                    is_var: p.is_var,
                                    source: (i as u32, d.0),
                                },
                            );
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
                            None => {
                                let dt =
                                    infer_lit_ty(file, de, &class_names, &fun_rets, &*libraries);
                                delegated_getvalue_ret_for_signature(file, &table, &*libraries, dt)
                                    .unwrap_or(Ty::Error)
                            }
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
                                    // Thread already-collected top-level properties into initializer
                                    // inference, including nested reads such as `a.compareTo(b)`.
                                    let props: Vec<(String, Ty, bool)> = table
                                        .props
                                        .iter()
                                        .map(|(n, (t, v, _))| (n.clone(), *t, *v))
                                        .collect();
                                    infer_lit_ty_scoped(
                                        file,
                                        i,
                                        &class_names,
                                        &fun_rets,
                                        &props,
                                        &*libraries,
                                        &table,
                                    )
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
                table.insert_class(alias.clone(), cs);
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

pub fn map_call_sig_args(
    args: &[ExprId],
    names: Option<&[Option<String>]>,
    sig: &CallSig,
) -> Result<Vec<Option<ExprId>>, String> {
    map_call_args(
        args,
        names,
        &sig.param_names,
        sig.required,
        &sig.param_defaults,
    )
}

pub fn map_param_list_args(
    args: &[ExprId],
    names: Option<&[Option<String>]>,
    params: &ParamList,
) -> Result<Vec<Option<ExprId>>, String> {
    map_call_args(
        args,
        names,
        &params.names,
        required_arity(params.names.len(), &params.defaults),
        &params.defaults,
    )
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
            indices,
            value,
        } => val(*array) || indices.iter().any(|&i| val(i)) || val(*value),
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
                indices,
                value,
            } => {
                ce(file, *array, active, out);
                for &i in indices {
                    ce(file, i, active, out);
                }
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

/// Names reassigned (`=`/`++`/`--`) INSIDE a nested lambda within `e`. A closure that writes a captured
/// `var` (possibly to null) could run between a narrowing assignment and a later read, so such a `var`
/// must never be flow-narrowed (soundness for [`Local::narrowed`]). Mirrors [`collect_all_reassigned`]
/// but only collects once the traversal has descended into a lambda body.
fn collect_closure_reassigned(file: &File, e: ExprId, out: &mut std::collections::HashSet<String>) {
    let cell = std::cell::RefCell::new(std::mem::take(out));
    fn ce(
        file: &File,
        e: ExprId,
        in_closure: bool,
        cell: &std::cell::RefCell<std::collections::HashSet<String>>,
    ) {
        let in_closure = in_closure || matches!(file.expr(e), Expr::Lambda { .. });
        if in_closure {
            if let Expr::IncDec { target, .. } = file.expr(e) {
                if let Expr::Name(n) = file.expr(*target) {
                    cell.borrow_mut().insert(n.clone());
                }
            }
        }
        file.any_child_expr(
            e,
            &mut |c| {
                ce(file, c, in_closure, cell);
                false
            },
            &mut |s| {
                cs(file, s, in_closure, cell);
                false
            },
        );
    }
    fn cs(
        file: &File,
        s: StmtId,
        in_closure: bool,
        cell: &std::cell::RefCell<std::collections::HashSet<String>>,
    ) {
        if in_closure {
            if let Stmt::Assign { name, .. } | Stmt::IncDec { name, .. } = file.stmt(s) {
                cell.borrow_mut().insert(name.clone());
            }
        }
        file.any_child_stmt(s, &mut |c| {
            ce(file, c, in_closure, cell);
            false
        });
    }
    ce(file, e, false, &cell);
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
    src: &dyn SemanticPlatform,
) -> Ty {
    let inferring = std::cell::RefCell::new(std::collections::HashSet::new());
    let env = InferEnv {
        up: &|_, _| None,
        inferring: &inferring,
        is_object: &|_| false,
    };
    infer_lit_ty_p(file, e, class_names, fun_rets, &[], src, &env)
}

fn delegated_getvalue_ret_for_signature(
    file: &File,
    table: &SymbolTable,
    libraries: &dyn SemanticPlatform,
    delegate_ty: Ty,
) -> Option<Ty> {
    let internal = delegate_ty.obj_internal()?;
    if internal.starts_with("kotlin/reflect/") {
        return None;
    }
    if let Some(sig) = table.method_of_name(internal, "getValue") {
        return Some(sig.ret);
    }
    let module = crate::module_symbols::ModuleSymbols::new(table);
    let scope = function_scope_packages_with(file, libraries.platform_default_import_packages());
    crate::symbol_resolver::SymbolResolver::new_scoped_with_module(libraries, &module, &scope)
        .resolve_symbol(
            crate::symbol_resolver::SymRecv::Value(delegate_ty),
            "getValue",
            &[Ty::obj("kotlin/Any"), Ty::obj("kotlin/reflect/KProperty")],
            &[],
        )
        .and_then(crate::symbol_resolver::Symbol::extension_call)
        .map(|c| c.ret)
}

/// The array type produced by a creation-builtin call in the lightweight inferer: a primitive
/// `intArrayOf(…)`/`byteArrayOf(…)`/… (element = the fixed primitive), or `arrayOf(…)` whose arguments
/// all AGREE on one REFERENCE type (element = that type — `Array<T>` = `[LT;`, and nullability erases
/// in the descriptor, so an explicit `arrayOf<T?>` reaches the same JVM type). Declines (`None`) — a
/// sound skip — for a mixed/empty `arrayOf` and for a PRIMITIVE-argument `arrayOf(1, 2)`: its element
/// boxes to `Array<Integer>`, but an explicit `arrayOf<Int?>(…)` (which this lightweight probe can't
/// see) or an expected-type context can change the element, so it is left to the full checker's
/// `check_array_builtin` — inferring it here risked disagreeing with the checked initializer type.
fn array_builtin_ret(fname: &str, arg_tys: &[Ty]) -> Option<Ty> {
    let prim = match fname {
        "intArrayOf" => Some(Ty::Int),
        "longArrayOf" => Some(Ty::Long),
        "doubleArrayOf" => Some(Ty::Double),
        "floatArrayOf" => Some(Ty::Float),
        "booleanArrayOf" => Some(Ty::Boolean),
        "charArrayOf" => Some(Ty::Char),
        "byteArrayOf" => Some(Ty::Byte),
        "shortArrayOf" => Some(Ty::Short),
        "uintArrayOf" => Some(Ty::UInt),
        "ulongArrayOf" => Some(Ty::ULong),
        _ => None,
    };
    if let Some(elem) = prim {
        return Some(Ty::array(elem));
    }
    if fname == "arrayOf" {
        if let Some(&first) = arg_tys.first() {
            if first.is_reference() && arg_tys.iter().all(|&t| t == first) {
                return Some(Ty::array(first));
            }
        }
    }
    None
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

/// Cross-call environment for [`infer_lit_ty_p`]: the module-class property resolver (`up`) plus the
/// cycle-guard set of expression bodies currently being inferred. Passed explicitly (formerly two of
/// these were an ambient thread-local) so the guard is per-inference and cannot leak across calls.
struct InferEnv<'a> {
    /// Resolve a property of a module class currently being collected. The library source cannot see
    /// module-local classes, so a `param.member` initializer needs this to infer its type.
    up: &'a dyn Fn(&str, &str) -> Option<Ty>,
    /// Expression-body ids currently on the inference stack — a companion method whose inferred return
    /// recurses back to itself (`a()=C.b(); b()=C.a()`) yields `Error` (skip) instead of looping.
    inferring: &'a std::cell::RefCell<std::collections::HashSet<u32>>,
    /// True if a simple name is a SAME-MODULE `object` (`val h = Helper`) — the library source can't
    /// see it, so the `Name` arm's classpath object-check misses it; this closes that gap.
    is_object: &'a dyn Fn(&str) -> bool,
}

/// Infer a declaration initializer's type with a fresh cycle-guard, using `table` to resolve
/// module-local class properties — the common entry used by signature collection.
fn infer_lit_ty_scoped(
    file: &File,
    e: ExprId,
    class_names: &ClassNames,
    fun_rets: &HashMap<String, Ty>,
    props: &[(String, Ty, bool)],
    src: &dyn SemanticPlatform,
    table: &SymbolTable,
) -> Ty {
    let up = |ci: &str, cn: &str| table.prop_of(ci, cn).map(|(pt, _)| pt);
    let is_object = |name: &str| table.objects.contains(name);
    let inferring = std::cell::RefCell::new(std::collections::HashSet::new());
    let env = InferEnv {
        up: &up,
        inferring: &inferring,
        is_object: &is_object,
    };
    infer_lit_ty_p(file, e, class_names, fun_rets, props, src, &env)
}

fn infer_lit_ty_p(
    file: &File,
    e: ExprId,
    class_names: &ClassNames,
    fun_rets: &HashMap<String, Ty>,
    props: &[(String, Ty, bool)],
    src: &dyn SemanticPlatform,
    env: &InferEnv,
) -> Ty {
    // Resolve the return type of `name` through the same scoped source the checker uses. This
    // lightweight pass accepts only unambiguous overload sets; otherwise it leaves inference to checking.
    fn resolved_ret(
        resolver: &crate::symbol_resolver::SymbolResolver,
        name: &str,
        receiver: Option<Ty>,
    ) -> Option<Ty> {
        let rets: Vec<Ty> = match receiver {
            Some(recv) => resolver
                .resolve_symbol(crate::symbol_resolver::SymRecv::Value(recv), name, &[], &[])
                .map(crate::symbol_resolver::Symbol::overloads)
                .unwrap_or_default()
                .into_iter()
                .map(|o| o.callable.ret)
                .collect(),
            None => crate::libraries::FunctionSet {
                overloads: resolver
                    .resolve_symbol(crate::symbol_resolver::SymRecv::TopLevel, name, &[], &[])
                    .map(crate::symbol_resolver::Symbol::overloads)
                    .unwrap_or_default(),
            }
            .into_top_level()
            .map(|o| o.callable.ret)
            .collect(),
        };
        let mut ret: Option<Ty> = None;
        for r in rets {
            match ret {
                None => ret = Some(r),
                Some(p) if p == r => {}
                Some(_) => return None, // overloads disagree on return type — needs arg selection
            }
        }
        ret
    }
    let scope = function_scope_packages_with(file, src.platform_default_import_packages());
    let resolver = crate::symbol_resolver::SymbolResolver::new_scoped(src, &scope);
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
            .find_map(|(pn, t, _)| (pn == n).then_some(*t))
            .or_else(|| {
                class_names
                    .get(n.as_str())
                    .filter(|internal| {
                        src.resolve_type_name(*internal)
                            .is_some_and(|t| t.is_object())
                            || (env.is_object)(n)
                    })
                    .map(Ty::obj_name)
            })
            .unwrap_or(Ty::Error),
        Expr::Member { receiver, name } => {
            if let Expr::Name(type_name) = file.expr(*receiver) {
                if let Some(c) = class_names.library_companion_const(src, type_name, name) {
                    return c.ty;
                }
            }
            // Property read (`s.length`, `list.size`, `vc.value`). Use the scoped resolver so an
            // imported extension property such as `Char.code` can resolve through its getter.
            let rt = infer_lit_ty_p(file, *receiver, class_names, fun_rets, props, src, env);
            if let Some(m) = resolver
                .resolve_symbol(crate::symbol_resolver::SymRecv::Value(rt), name, &[], &[])
                .and_then(crate::symbol_resolver::Symbol::property)
            {
                return m.ret;
            }
            if let Some(g) = resolver
                .resolve_symbol(crate::symbol_resolver::SymRecv::Value(rt), name, &[], &[])
                .and_then(crate::symbol_resolver::Symbol::extension_property_getter)
            {
                return g.ret;
            }
            // A property of a module class being collected, invisible to the library source.
            if let Some(internal) = rt.obj_internal() {
                if let Some(t) = (env.up)(&internal.render(), name) {
                    return t;
                }
            }
            Ty::Error
        }
        Expr::Unary { op, operand } => match op {
            UnOp::Not => Ty::Boolean,
            UnOp::Neg | UnOp::Plus => {
                infer_lit_ty_p(file, *operand, class_names, fun_rets, props, src, env)
            }
        },
        Expr::Binary { op, lhs, rhs } => {
            let (lt, rt) = (
                infer_lit_ty_p(file, *lhs, class_names, fun_rets, props, src, env),
                infer_lit_ty_p(file, *rhs, class_names, fun_rets, props, src, env),
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
                    // A primitive-array size constructor (`IntArray(n)`, `CharArray(n) { … }`) — a stdlib
                    // intrinsic the federated probe below doesn't surface as a return type. (`Array(n){}`
                    // needs the lambda's element type, so it stays with the full checker.)
                    if let Some(elem) = Ty::primitive_array_element(n) {
                        return Ty::array(elem);
                    }
                    // A JDK/classpath type resolvable by simple name (`val sb = StringBuilder()`).
                    if let Some(internal) = class_names.get(n.as_str()) {
                        // Preserve EXPLICIT type arguments on a generic constructor call
                        // (`ConcurrentHashMap<String, V>()`) so an inferred PROPERTY keeps them — else a
                        // later indexing / generic-member access on the field erases its element to `Any`.
                        // (The full checker keeps them for a local `val`; the signature phase must too.)
                        if let Some(targs) = file.call_type_args.get(&e.0).filter(|t| !t.is_empty())
                        {
                            let empty_tp = TParams::from_bindings([]);
                            let mut sink = crate::diag::DiagSink::new();
                            let args: Vec<Ty> = targs
                                .iter()
                                .map(|r| ty_of_ref(r, class_names, &empty_tp, &mut sink))
                                .collect();
                            return Ty::Obj(internal, Box::leak(args.into_boxed_slice()));
                        }
                        return Ty::obj_name(internal);
                    }
                    // A top-level library/stdlib function — federated resolution (no hardcoded names).
                    if let Some(t) = resolved_ret(&resolver, n, None) {
                        return t;
                    }
                    // A member of a classpath OBJECT imported unqualified (`import Obj.member`; the
                    // top-level `val logger = logger {}` idiom): resolve the member's return on the
                    // object's singleton type — mirroring the checker's `object_member_import`.
                    if let Some(internal) = object_member_import_sig(file, n, src) {
                        if let Some(t) = resolved_ret(&resolver, n, Some(Ty::obj(&internal))) {
                            return t;
                        }
                    }
                    // A GENERIC top-level function whose return type depends on its arguments
                    // (`arrayOf("a","b")` → `Array<String>`, `mapOf(1 to "x")` → `Map<Int,String>`):
                    // the return-agreement probe above can't decide it (the erased return is the same
                    // for every call), so resolve through the SAME federated `SymbolResolver` the full
                    // checker uses, binding the type parameters from the inferred argument types. Only
                    // reached when the simpler probe returned `None`, so it never overrides an inference.
                    let arg_tys: Vec<Ty> = args
                        .iter()
                        .map(|a| infer_lit_ty_p(file, *a, class_names, fun_rets, props, src, env))
                        .collect();
                    // Array-creation builtins (`arrayOf`, `intArrayOf`, …) are `@InlineOnly` intrinsics
                    // the federated probe doesn't surface as a return type — infer the element here,
                    // matching the full checker's `check_array_builtin` (which decides the property's
                    // field type identically, keeping the two in sync).
                    if let Some(t) = array_builtin_ret(n, &arg_tys) {
                        return t;
                    }
                    if !arg_tys.contains(&Ty::Error) {
                        if let Some(c) = resolver
                            .resolve_symbol(
                                crate::symbol_resolver::SymRecv::TopLevel,
                                n,
                                &arg_tys,
                                &[],
                            )
                            .and_then(crate::symbol_resolver::Symbol::top_level_call)
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
                    // `C.method(args)` — a COMPANION method of a same-file class `C` (invisible to the
                    // classpath source). Resolve its return type: a declared return, else a recursive
                    // inference of its expression body (`private fun <T> create() = C()` → `C`). Skip a
                    // self-referential body so the recursion can't loop.
                    if let Expr::Name(type_name) = file.expr(*receiver) {
                        if let Some(cm) = file.decls.iter().find_map(|&d| match file.decl(d) {
                            Decl::Class(c) if &c.name == type_name => {
                                c.companion_methods.iter().find(|m| &m.name == name)
                            }
                            _ => None,
                        }) {
                            if let Some(r) = &cm.ret {
                                let empty_tp = TParams::from_bindings([]);
                                let mut sink = crate::diag::DiagSink::new();
                                let t = ty_of_ref(r, class_names, &empty_tp, &mut sink);
                                if t != Ty::Error {
                                    return t;
                                }
                            } else if let FunBody::Expr(be) = &cm.body {
                                // Guard against a companion method whose inferred return recurses back to
                                // itself (directly or mutually, `a()=C.b(); b()=C.a()`) — track the bodies
                                // being inferred in `env.inferring` so a cycle yields `Error` (skip)
                                // instead of unbounded recursion. Real recursive functions require an
                                // explicit return type anyway (kotlinc rejects an inferred one).
                                let fresh = env.inferring.borrow_mut().insert(be.0);
                                if fresh {
                                    let t = infer_lit_ty_p(
                                        file,
                                        *be,
                                        class_names,
                                        fun_rets,
                                        props,
                                        src,
                                        env,
                                    );
                                    env.inferring.borrow_mut().remove(&be.0);
                                    if t != Ty::Error {
                                        return t;
                                    }
                                }
                            }
                        }
                    }
                    let recv_ty =
                        infer_lit_ty_p(file, *receiver, class_names, fun_rets, props, src, env);
                    if recv_ty != Ty::Error {
                        if let Some(t) = builtin_bitwise_ret(recv_ty, name, args.len()) {
                            return t;
                        }
                        // Everything else (`s.uppercase()`, `10.toLong()`, library members/extensions):
                        // resolve the call through the resolver's single discovering entry point,
                        // `resolve_symbol`. It maps a primitive/`String` receiver to its class and runs the
                        // same overload resolution the checker uses (so a numeric conversion — a real member
                        // on `kotlin/Int`/`Number` — is typed without any hardcoded method name). The `Any`
                        // fallback covers a USER receiver calling an inherited `toString`/`hashCode`/`equals`.
                        let arg_tys: Vec<Ty> = args
                            .iter()
                            .map(|a| {
                                infer_lit_ty_p(file, *a, class_names, fun_rets, props, src, env)
                            })
                            .collect();
                        // Only select an overload when every argument's type is known — an `Error` arg is
                        // assignable to any parameter, so it could spuriously match the wrong overload and
                        // infer a type the checker won't agree with. With an unknown arg, skip to the
                        // agreement-based extension fallback instead.
                        if !arg_tys.contains(&Ty::Error) {
                            for r in [recv_ty, Ty::obj("kotlin/Any")] {
                                if let Some(m) = resolver
                                    .resolve_symbol(
                                        crate::symbol_resolver::SymRecv::Value(r),
                                        name,
                                        &arg_tys,
                                        &[],
                                    )
                                    .and_then(crate::symbol_resolver::Symbol::call)
                                {
                                    return m.ret;
                                }
                            }
                        }
                        // A scoped receiver-EXTENSION (`"s".uppercase()`) isn't surfaced by the member
                        // facet in this phase — fall back to the federated extension resolution.
                        if let Some(t) = resolved_ret(&resolver, name, Some(recv_ty)) {
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
            let t = infer_lit_ty_p(file, *then_branch, class_names, fun_rets, props, src, env);
            let e = infer_lit_ty_p(file, *eb, class_names, fun_rets, props, src, env);
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
                let bt = infer_lit_ty_p(file, a.body, class_names, fun_rets, props, src, env);
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
        } => infer_lit_ty_p(file, *t, class_names, fun_rets, props, src, env),
        // A range value (`val r = 1..10`, `0 until n`, `4 downTo 1`) — the matching stdlib range type
        // (mirrors the checker's `RangeTo` typing), so a range-typed property's type infers.
        Expr::RangeTo { lo, hi, .. } => {
            let lt = infer_lit_ty_p(file, *lo, class_names, fun_rets, props, src, env);
            let rt = infer_lit_ty_p(file, *hi, class_names, fun_rets, props, src, env);
            Ty::range_value_type(lt, rt).unwrap_or(Ty::Error)
        }
        // A top-level property initialized from an unambiguous classpath function reference.
        Expr::CallableRef {
            receiver: None,
            name,
        } if name != "class" => {
            let overloads = crate::libraries::FunctionSet {
                overloads: resolver
                    .resolve_symbol(crate::symbol_resolver::SymRecv::TopLevel, name, &[], &[])
                    .map(crate::symbol_resolver::Symbol::overloads)
                    .unwrap_or_default(),
            };
            match overloads.into_single_top_level() {
                Some(o) if o.call_sig.requires_all_args(o.callable.params.len()) => {
                    Ty::fun(o.callable.params.clone(), o.callable.ret)
                }
                _ => Ty::Error,
            }
        }
        _ => Ty::Error,
    }
}

/// The `invoke` arity of a reflection property/function-reference type — a `KProperty{N}`,
/// `KMutableProperty{N}`, or `KFunction{N}` extends `Function{N}`, so calling the reference invokes
/// `Function{N}.invoke`. Returns `N` for those internal names, else `None`.
fn callable_reference_invoke_arity(internal: TypeName) -> Option<usize> {
    internal
        .unsigned_suffix_after_prefix("kotlin/reflect/KProperty")
        .or_else(|| internal.unsigned_suffix_after_prefix("kotlin/reflect/KMutableProperty"))
        .or_else(|| internal.unsigned_suffix_after_prefix("kotlin/reflect/KFunction"))
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
        resolve: &dyn Fn(&str) -> Option<TypeName>,
    ) -> Self {
        let erasure = names
            .iter()
            .map(|n| {
                // A bound may name ANOTHER type parameter of the same declaration
                // (`<T1 : C, T2 : T1>`): follow the chain to the first bound that is a real
                // class/primitive so `T2` erases to `C`, not `Any`. Cycle-guarded (`<A : B, B : A>`).
                let mut cur = n.as_str();
                let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
                let erased = loop {
                    let b = bounds.iter().find_map(|(bn, b)| (bn == cur).then_some(b));
                    match b {
                        // The bound is itself a (non-nullable, un-parameterized) type parameter of this
                        // declaration — hop to it and keep chasing.
                        Some(tb)
                            if !tb.nullable
                                && names.iter().any(|m| m == &tb.name)
                                && seen.insert(cur) =>
                        {
                            cur = tb.name.as_str();
                        }
                        other => break tparam_bound_erasure(other, resolve),
                    }
                };
                (n.clone(), erased)
            })
            .collect();
        TParams { erasure }
    }

    pub fn extended_with(
        &self,
        names: &[String],
        bounds: &[(String, TypeRef)],
        resolve: &dyn Fn(&str) -> Option<TypeName>,
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
        resolve: &dyn Fn(&str) -> Option<TypeName>,
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
/// [`crate::symbol_resolver::unify_gsig`], for user-declared generic methods.
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
fn class_internal_resolver(syms: &SymbolTable) -> impl Fn(&str) -> Option<TypeName> + '_ {
    move |n: &str| {
        syms.classes
            .get(n)
            .map(|c| type_name(&c.internal()))
            .or_else(|| syms.class_names.get(n))
    }
}

/// The JVM erasure of a type parameter from its declared upper bound. kotlinc erases a bounded `T` to
/// its bound's type — a specializable integral primitive bound (`<T: Int>`) to that primitive, any other
/// reference bound (`<T: CharSequence>`, `<T: Comparable<T>>`, a user class) to the bound's class — so a
/// value of type `T` accesses the bound's members and the descriptor uses the bound, not `Object`.
/// `Any` when there's no bound, a nullable bound, an unresolved bound, or a non-specializable primitive
/// (`Double`/unsigned/value — those bounds stay rejected on use).
fn tparam_bound_erasure(b: Option<&TypeRef>, resolve: &dyn Fn(&str) -> Option<TypeName>) -> Ty {
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
        Some(internal) => Ty::obj_name(internal),
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
        param_default_values: Vec::new(),
        param_names: m.params.iter().map(|p| p.name.clone()).collect(),
        lambda_param_types,
        lambda_recv: m.params.iter().map(|p| p.ty.fun_has_receiver).collect(),
        is_inline: false,
        is_final: m.is_final,
        is_suspend: m.is_suspend,
        context_count: 0,
        source_decl: None,
        source_file: None,
        package: String::new(),
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
    // Named functional-interface form `FunctionN<P1, …, PN, R>` — the explicit spelling of the arrow
    // type `(P1, …, PN) -> R`. Resolve it to the SAME `Ty::Fun` the arrow form yields (last type arg
    // is the return, the rest are parameters) so `x is Function1<*, *>`, a `val f: Function2<…>`, etc.
    // all agree with the arrow form. Bare `Function` (no arity) and non-numeric suffixes fall through.
    if let Some(suffix) = r.name.strip_prefix("Function") {
        if let Ok(arity) = suffix.parse::<usize>() {
            // Only `Function0..Function22` are ordinary `kotlin/jvm/functions/FunctionN` interfaces;
            // higher arities use the distinct big-arity `FunctionN` interface krusty doesn't model, so
            // leave those to fall through (unresolved → cleanly skipped, never a wrong `Object` test).
            if arity <= 22 && r.targs.len() == arity + 1 {
                let params: Vec<Ty> = r.targs[..arity].iter().map(&mut *recurse).collect();
                let ret = recurse(&r.targs[arity]);
                return Some(Ty::fun(params, ret));
            }
        }
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
                } else if e.jvm_boxed_ref().is_some() {
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
            Ty::from_name(&prim).unwrap_or(Ty::Error)
        } else if r.targs.is_empty() {
            Ty::obj_name(internal)
        } else {
            // Generic instantiation `C<A, …>` — carry the resolved arguments (erased in descriptors).
            let args: Vec<Ty> = r
                .targs
                .iter()
                .map(|a| ty_of_ref_with(a, classes, tparams, ctx, diags))
                .collect();
            Ty::Obj(internal, Box::leak(args.into_boxed_slice()))
        }
    } else {
        diags.error(r.span, format!("unresolved reference '{}'.", r.name));
        Ty::Error
    };
    // A nullable primitive/Unit/Nothing stays a source `Nullable(T)`; the backend chooses the concrete
    // reference carrier at the emit boundary. Unsupported non-reference values are rejected here.
    if r.nullable && !base.is_reference() && base != Ty::Error {
        if let Some(nullable) = base.nullable_non_ref() {
            return nullable;
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
    /// Selected expression lowerings that cannot be recovered from the expression shape alone.
    pub expr_lowers: HashMap<ExprId, ExprLowering>,
    /// Selected statement lowerings that differ from the parser's generic statement shape.
    pub stmt_lowers: HashMap<StmtId, StmtLowering>,
    /// The RESOLVED type of a `val`/`var` local that carries an explicit type annotation, keyed by the
    /// `Stmt::Local`. The lowerer reuses this instead of re-resolving the annotation — so a library type
    /// the checker resolves through imports (`var res: Result<T>? = null`) keeps its (value-)class type
    /// instead of collapsing to the initializer's type. Absent for an inferred (no-annotation) local.
    pub local_decl_types: HashMap<StmtId, Ty>,
    /// Call `ExprId` → its explicit type arguments RESOLVED to `Ty` (`getFor<Prov>` → `[Prov]`). The
    /// checker resolves them (imports/classpath types resolve here, not in the lowerer's local
    /// `ty_ref`); the lowerer pairs them with a `<reified T>` classpath extension's type-parameter names
    /// to drive the bytecode splicer's reified specialization. Recorded only where an explicit `<…>` is
    /// present on a call.
    pub resolved_call_type_args: HashMap<ExprId, Vec<Ty>>,
    /// A bare-member read (`Expr::Name`) or unqualified call that resolved against a FLOW-NARROWED
    /// implicit receiver (`if (this is B) … a …`, where `a`/`m()` is a member of `B` but not of the
    /// declared receiver). Maps the read/call `ExprId` to the narrowed receiver's internal name so the
    /// lowerer emits a `checkcast` on the loaded `this` before the field read / method call.
    pub narrowed_this_member: HashMap<ExprId, TypeName>,
    /// Calls the checker already resolved, keyed by the `Expr::Call` (or member-read) `ExprId`. The
    /// lowerer reads the resolved target here instead of re-running `symbol_resolver`, so a source call
    /// is resolved exactly once and the two passes cannot select different overloads. Module-local
    /// members and language intrinsics are still lowered from module tables or expression shape. See
    /// [`ResolvedCall`] for the target kinds and the typed accessors ([`Self::resolved_member`],
    /// [`Self::resolved_top_level`], …).
    pub resolved_calls: HashMap<ExprId, ResolvedCall>,
    /// Synthetic operator calls selected while checking a source expression that does not itself contain
    /// a call node for every desugared operation. Example: reference `x in a..b` resolves both
    /// `a.rangeTo(b)` and `<range>.contains(x)` from one `Expr::InRange`; lowering reads these selections
    /// instead of re-running operator/member resolution.
    pub resolved_operator_calls: HashMap<(ExprId, SyntheticOperatorCall), ResolvedCall>,
    /// Statement-level synthetic operator calls selected while checking, e.g. `a[i] = v` resolving to
    /// `a.set(i, v)` or `a.put(i, v)`. Lowering reads this table instead of selecting the setter again.
    pub resolved_stmt_operator_calls: HashMap<(StmtId, SyntheticOperatorCall), ResolvedCall>,
    /// For classpath-backed `a[i] = v`, the checker-selected `a.get(i)` logical return type. Lowering
    /// uses this only for its primitive narrowing guard and never resolves the getter itself.
    pub resolved_index_store_get_returns: HashMap<StmtId, Ty>,
    /// Destructuring component calls selected while checking, keyed by the destructuring statement and
    /// component position. Lowering reads this instead of re-selecting `componentN`, source-property
    /// getters, or indexed `get(Int)` fallbacks.
    pub resolved_destructure_components: HashMap<(StmtId, usize), DestructureComponentTarget>,
    /// Iterator protocol selected while checking a `for`/inlined-foreach receiver. Lowering emits these
    /// exact `iterator`/`hasNext`/`next` calls instead of resolving the protocol again.
    pub iterator_protocols: HashMap<ExprId, IteratorProtocolTarget>,
    /// Synthetic member calls selected while checking for a source construct whose lowering emits
    /// additional member calls that do not exist as AST call nodes. Keyed by the source expression that
    /// owns the expansion plus the synthetic member name.
    pub synthetic_member_calls: HashMap<(ExprId, String), crate::libraries::LibraryMember>,
    /// Bound classpath/library property references selected while checking, keyed by the
    /// `Expr::CallableRef` expression. Lowering emits the recorded getter instead of re-resolving.
    pub bound_property_refs: HashMap<ExprId, crate::symbol_resolver::BoundPropertyRef>,
    /// Bound classpath/library member references selected while checking, keyed by the
    /// `Expr::CallableRef` expression. Lowering emits the recorded virtual target instead of re-resolving.
    pub bound_member_refs: HashMap<ExprId, crate::libraries::LibraryMember>,
    /// Classpath/library property setters selected while checking, keyed by the assignment statement.
    pub property_setters: HashMap<StmtId, crate::libraries::LibraryCallable>,
    /// Classpath/library constructors selected while checking, keyed by the construction call.
    pub resolved_constructors: HashMap<ExprId, ResolvedConstructor>,
    /// `super.f(...)` targets selected while checking, keyed by the call expression.
    pub resolved_super_calls: HashMap<ExprId, ResolvedSuperCall>,
    /// Classpath member `$default` synthetic targets selected while checking for calls with omitted
    /// default arguments.
    pub resolved_default_member_calls: HashMap<ExprId, ResolvedDefaultMemberCall>,
    /// Library companion constants selected while checking (`Int.MAX_VALUE`, `Double.NaN`, ...), keyed by
    /// the member-read expression. Lowering inlines the recorded constant instead of probing the classpath.
    pub resolved_library_companion_consts: HashMap<ExprId, crate::libraries::LibraryConst>,
    /// Classpath/library enum entries selected while checking (`Kind.PENDING`), keyed by the member-read
    /// expression. Lowering emits the recorded owner field instead of resolving imports again.
    pub resolved_library_enum_entries: HashMap<ExprId, TypeName>,
    /// For a resolved classpath member, extension, or top-level call, maps callee parameter slots to
    /// source arguments. `None` means the target default-call ABI fills that slot.
    pub resolved_call_arg_slots: HashMap<ExprId, Vec<Option<ExprId>>>,
    /// Extension callables the checker resolved for a SYNTHESIZED call that has no source-call `ExprId` —
    /// a destructuring `componentN`, a `for`-loop `iterator`, a `+=` `plusAssign` — keyed by the receiver
    /// expression's `ExprId` (the destructured value / iterable / assignment target) and the operator
    /// name. The lowerer reads the callable here instead of re-resolving; it carries `inline`, so the
    /// backend splices when needed.
    pub synthetic_ext_calls: HashMap<(ExprId, String), crate::libraries::LibraryCallable>,
    /// Delegated-property `getValue` targets selected by the checker, keyed by the delegate expression.
    pub delegate_getvalue_targets: HashMap<ExprId, DelegateGetValueTarget>,
    /// For a call to a function with CONTEXT PARAMETERS (`context(a: A) fun f()`) where the context
    /// arguments are supplied IMPLICITLY, the in-scope source that fills each leading context parameter,
    /// in order — either the sentinel `"this"` (the enclosing implicit receiver, e.g. a `with` block's
    /// receiver) or the name of an in-scope local / enclosing context parameter. Keyed by the call
    /// `ExprId`. The lowerer loads each named value and PREPENDS them to the call arguments so the
    /// emitted call matches the callee's leading context parameters.
    pub context_args: HashMap<ExprId, Vec<String>>,
}

/// A call target the checker resolved, stashed for the lowerer keyed by `ExprId`. One entry per call —
/// the variant selects how it emits, so a member lowering that reads a `Companion` (static) entry, or a
/// top-level lowering that reads a `Member` (virtual) entry, simply doesn't match and falls back.
#[derive(Clone, Debug)]
pub enum ResolvedCall {
    /// A classpath instance-member call → `invokevirtual`/`invokeinterface`.
    Member(crate::symbol_resolver::ResolvedMember),
    /// A receiver-less top-level library call → `invokestatic` on the facade.
    TopLevel(crate::libraries::LibraryCallable),
    /// A `@JvmStatic`/companion `object` member (`UuidGen.of(x)`) → STATIC call, never virtual.
    Companion(crate::libraries::LibraryMember),
    /// A library EXTENSION call `recv.name(args)` → `invokestatic facade.name(recv, args)`. The checker
    /// is the sole resolver: the lowerer READS this callable and emits it (never re-resolving).
    Extension(crate::libraries::LibraryCallable),
    /// A same-module member operator selected by the checker for a source expression. Lowering links
    /// this exact owner/signature to the current file's IR method id; it must not re-select by name.
    ModuleMember {
        owner: TypeName,
        name: String,
        params: Vec<Ty>,
        ret: Ty,
        interface: bool,
    },
    /// A same-module extension operator selected by the checker for a source expression.
    ModuleExtension {
        receiver: Ty,
        name: String,
        params: Vec<Ty>,
        ret: Ty,
    },
    /// A same-module receiver-less top-level call selected by the checker. The lowerer maps this
    /// semantic target to the current file's lifted IR function or sibling facade; it must not
    /// re-resolve overloads by name/argument types.
    ModuleTopLevel(Box<ResolvedModuleTopLevelCall>),
    /// A local function selected by the checker. The lowerer reads this target and only maps the
    /// declaration id to the lifted IR function; it must not re-resolve the call by name.
    LocalFunction(Box<ResolvedLocalFunctionCall>),
    /// An INSTANCE MEMBER selected by lambda-return overload resolution (`recv.run2 { … }`). The lowerer
    /// has no resolution path for it (it is selected by the lambda's return type, not by normal member
    /// lookup), so the checker resolves and records it and the lowerer only READS it — emits
    /// `invokevirtual`. Distinct from [`Self::Member`] because it carries a raw `LibraryCallable`.
    LambdaReturnMember(crate::libraries::LibraryCallable),
}

#[derive(Clone, Debug)]
pub enum ResolvedConstructor {
    Plain {
        member: crate::libraries::LibraryMember,
        args: Vec<ExprId>,
    },
    PlainSlots {
        member: crate::libraries::LibraryMember,
        slots: Vec<Option<ExprId>>,
    },
    Synthetic {
        ctor: crate::symbol_resolver::SyntheticCtorCall,
        args: Vec<ExprId>,
    },
    NamedDefault {
        descriptor: String,
        real_params: Vec<Ty>,
        slots: Vec<Option<ExprId>>,
        mask: i32,
    },
}

#[derive(Clone, Debug)]
pub struct ResolvedSuperCall {
    pub owner: TypeName,
    pub interface: bool,
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub descriptor: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ResolvedDefaultMemberCall {
    pub descriptor: String,
    pub real_params: Vec<Ty>,
    pub ret: Ty,
    pub suspend: bool,
}

#[derive(Clone, Debug)]
pub enum DestructureComponentTarget {
    ModuleMember {
        owner: TypeName,
        name: String,
        params: Vec<Ty>,
        ret: Ty,
    },
    CrossFileModuleMember {
        owner: TypeName,
        name: String,
        params: Vec<Ty>,
        ret: Ty,
        interface: bool,
    },
    LibraryMember(crate::symbol_resolver::ResolvedMember),
    LibraryExtension(crate::libraries::LibraryCallable),
    ModuleExtension {
        receiver: Ty,
        name: String,
        params: Vec<Ty>,
        ret: Ty,
    },
    ModulePropertyGetter {
        owner: TypeName,
        property: String,
        ret: Ty,
        interface: bool,
    },
    IndexedGet(crate::symbol_resolver::ResolvedMember),
}

#[derive(Clone, Debug)]
pub enum IteratorDispatchTarget {
    Member {
        owner_fallback: TypeName,
        member: Box<crate::libraries::LibraryMember>,
    },
    Extension(Box<crate::libraries::LibraryCallable>),
}

impl IteratorDispatchTarget {
    pub fn ret(&self) -> Ty {
        match self {
            Self::Member { member, .. } => member.ret,
            Self::Extension(callable) => callable.ret,
        }
    }
}

#[derive(Clone, Debug)]
pub struct IteratorProtocolTarget {
    pub iterator: IteratorDispatchTarget,
    pub has_next: crate::libraries::LibraryMember,
    pub next: crate::libraries::LibraryMember,
    pub iter_ty: Ty,
    pub elem_ty: Ty,
}

impl DestructureComponentTarget {
    pub fn ret(&self) -> Ty {
        match self {
            Self::ModuleMember { ret, .. }
            | Self::CrossFileModuleMember { ret, .. }
            | Self::ModuleExtension { ret, .. }
            | Self::ModulePropertyGetter { ret, .. } => *ret,
            Self::LibraryMember(m) | Self::IndexedGet(m) => m.ret,
            Self::LibraryExtension(c) => c.ret,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SyntheticOperatorCall {
    RangeTo,
    Contains,
    Set,
    Put,
}

impl SyntheticOperatorCall {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "rangeTo" => Self::RangeTo,
            "contains" => Self::Contains,
            "set" => Self::Set,
            "put" => Self::Put,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ResolvedLocalFunctionCall {
    pub stmt_id: StmtId,
    pub sig: Signature,
    pub provided_arg_count: usize,
    pub context_args: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ResolvedModuleTopLevelCall {
    pub name: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub call_sig: CallSig,
    pub inline: InlineKind,
    pub suspend: bool,
    pub context_args: Vec<String>,
    pub source_file: Option<u32>,
    pub source_decl: Option<DeclId>,
    pub param_meta: Vec<(String, Option<ExprId>)>,
    pub param_default_values: Vec<Option<CtorDefaultValue>>,
    pub ret_is_tparam: bool,
}

#[derive(Clone, Debug)]
pub enum DelegateGetValueTarget {
    Member {
        owner: TypeName,
        params: Vec<Ty>,
        ret: Ty,
    },
    Extension(Box<crate::libraries::LibraryCallable>),
}

impl DelegateGetValueTarget {
    pub fn ret(&self) -> Ty {
        match self {
            Self::Member { ret, .. } => *ret,
            Self::Extension(c) => c.ret,
        }
    }
}

impl TypeInfo {
    /// The resolved classpath instance member at call `e`, if the checker recorded one.
    pub fn resolved_member(&self, e: ExprId) -> Option<&crate::symbol_resolver::ResolvedMember> {
        match self.resolved_calls.get(&e) {
            Some(ResolvedCall::Member(m)) => Some(m),
            _ => None,
        }
    }
    /// The resolved receiver-less top-level library callable at call `e`, if any.
    pub fn resolved_top_level(&self, e: ExprId) -> Option<&crate::libraries::LibraryCallable> {
        match self.resolved_calls.get(&e) {
            Some(ResolvedCall::TopLevel(c)) => Some(c),
            _ => None,
        }
    }
    /// The resolved companion/`@JvmStatic` member at call `e`, if any.
    pub fn resolved_companion(&self, e: ExprId) -> Option<&crate::libraries::LibraryMember> {
        match self.resolved_calls.get(&e) {
            Some(ResolvedCall::Companion(m)) => Some(m),
            _ => None,
        }
    }
    /// The resolved library EXTENSION callable at call `e` — the checker's sole resolution, read by the
    /// lowerer's extension-emit path (which never resolves).
    pub fn resolved_extension(&self, e: ExprId) -> Option<&crate::libraries::LibraryCallable> {
        match self.resolved_calls.get(&e) {
            Some(ResolvedCall::Extension(c)) => Some(c),
            _ => None,
        }
    }
    /// The resolved local function declaration selected at call `e`, if any.
    pub fn resolved_local_function(&self, e: ExprId) -> Option<&ResolvedLocalFunctionCall> {
        match self.resolved_calls.get(&e) {
            Some(ResolvedCall::LocalFunction(c)) => Some(c),
            _ => None,
        }
    }
    /// The resolved same-module top-level function selected at call `e`, if any.
    pub fn resolved_module_top_level(&self, e: ExprId) -> Option<&ResolvedModuleTopLevelCall> {
        match self.resolved_calls.get(&e) {
            Some(ResolvedCall::ModuleTopLevel(c)) => Some(c),
            _ => None,
        }
    }
    /// The resolved target for a synthetic operator selected while checking expression `e`.
    pub fn resolved_operator_call(&self, e: ExprId, name: &str) -> Option<&ResolvedCall> {
        self.resolved_operator_calls
            .get(&(e, SyntheticOperatorCall::from_name(name)?))
    }
    /// The resolved target for a synthetic operator selected while checking statement `s`.
    pub fn resolved_stmt_operator_call(&self, s: StmtId, name: &str) -> Option<&ResolvedCall> {
        self.resolved_stmt_operator_calls
            .get(&(s, SyntheticOperatorCall::from_name(name)?))
    }
    pub fn resolved_index_store_get_return(&self, s: StmtId) -> Option<Ty> {
        self.resolved_index_store_get_returns.get(&s).copied()
    }
    /// The resolved target for destructuring component `idx` in statement `s`.
    pub fn resolved_destructure_component(
        &self,
        s: StmtId,
        idx: usize,
    ) -> Option<&DestructureComponentTarget> {
        self.resolved_destructure_components.get(&(s, idx))
    }
    /// The resolved iterator protocol for iterable expression `e`, if the checker selected one.
    pub fn iterator_protocol(&self, e: ExprId) -> Option<&IteratorProtocolTarget> {
        self.iterator_protocols.get(&e)
    }
    /// The resolved target for a synthetic member call selected while checking source expression `e`.
    pub fn synthetic_member_call(
        &self,
        e: ExprId,
        name: &str,
    ) -> Option<&crate::libraries::LibraryMember> {
        self.synthetic_member_calls.get(&(e, name.to_string()))
    }
    /// The resolved classpath/library property getter for a bound property reference expression.
    pub fn bound_property_ref(
        &self,
        e: ExprId,
    ) -> Option<&crate::symbol_resolver::BoundPropertyRef> {
        self.bound_property_refs.get(&e)
    }
    /// The resolved classpath/library member target for a bound method reference expression.
    pub fn bound_member_ref(&self, e: ExprId) -> Option<&crate::libraries::LibraryMember> {
        self.bound_member_refs.get(&e)
    }
    /// The resolved classpath/library property setter for a member assignment statement.
    pub fn property_setter(&self, s: StmtId) -> Option<&crate::libraries::LibraryCallable> {
        self.property_setters.get(&s)
    }
    /// The resolved classpath/library constructor target for construction call `e`.
    pub fn resolved_constructor(&self, e: ExprId) -> Option<&ResolvedConstructor> {
        self.resolved_constructors.get(&e)
    }
    /// The resolved target for a `super.f(...)` call.
    pub fn resolved_super_call(&self, e: ExprId) -> Option<&ResolvedSuperCall> {
        self.resolved_super_calls.get(&e)
    }
    /// The resolved `$default` synthetic for a classpath member call with omitted arguments.
    pub fn resolved_default_member_call(&self, e: ExprId) -> Option<&ResolvedDefaultMemberCall> {
        self.resolved_default_member_calls.get(&e)
    }
    /// The library companion constant selected for member-read expression `e`, if any.
    pub fn resolved_library_companion_const(
        &self,
        e: ExprId,
    ) -> Option<crate::libraries::LibraryConst> {
        self.resolved_library_companion_consts.get(&e).copied()
    }
    /// The classpath/library enum-entry owner selected for member-read expression `e`, if any.
    pub fn resolved_library_enum_entry_owner(&self, e: ExprId) -> Option<TypeName> {
        self.resolved_library_enum_entries.get(&e).copied()
    }
    /// The extension callable the checker resolved for a SYNTHESIZED operator (`componentN`/`iterator`/
    /// `plusAssign`) on the receiver expression `recv`, if any.
    pub fn synthetic_ext(
        &self,
        recv: ExprId,
        name: &str,
    ) -> Option<&crate::libraries::LibraryCallable> {
        self.synthetic_ext_calls.get(&(recv, name.to_string()))
    }
    pub fn delegate_getvalue(&self, delegate: ExprId) -> Option<&DelegateGetValueTarget> {
        self.delegate_getvalue_targets.get(&delegate)
    }
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
    /// A classpath top-level function reference (`::foo`) resolved by the checker. Lowering reads the
    /// callable instead of resolving the reference again.
    ClasspathTopLevelFunctionRef(crate::libraries::LibraryCallable),
    /// An unqualified function reference `::foo` where `foo` is imported from a SAME-FILE `object`
    /// (`import Host.foo`) — a BOUND reference to that object's singleton member, lowered exactly like
    /// `Host::foo` (capture `Host.INSTANCE`, invoke the member).
    ImportedObjectMemberRef { internal: TypeName },
    /// A function reference `::of` whose target has a trailing `vararg` (and no fixed defaults), adapted
    /// to a function type of LARGER arity: the expected function type's extra parameters (beyond the
    /// `fixed` leading fixed parameters) are COLLECTED into the vararg array. `target_params` is `of`'s
    /// full parameter list (fixed… + the vararg array), `adapted_params`/`ret` are the expected function
    /// type's parameters/return, `fixed` is the count of leading fixed parameters.
    AdaptedVarargCollect {
        name: String,
        target_params: Vec<Ty>,
        adapted_params: Vec<Ty>,
        ret: Ty,
        fixed: usize,
        /// `Some(internal)` when the target is a member of a same-file `object` (`import Host.foo`) — the
        /// adapter invokes it on `Host.INSTANCE` instead of as a top-level static.
        object_internal: Option<TypeName>,
    },
    /// An ADAPTED same-file top-level function reference (`::foo` passed where a shorter function type is
    /// expected, `foo` having trailing DEFAULT parameters). Lowering synthesizes an adapter of the
    /// expected arity that calls `foo`'s `$default` stub filling the omitted defaults. `target_params` is
    /// `foo`'s full parameter list (locates its IR function), `adapted_params`/`ret` are the expected
    /// function type's parameters/return (the adapter's own signature).
    AdaptedRef {
        name: String,
        target_params: Vec<Ty>,
        adapted_params: Vec<Ty>,
        ret: Ty,
        /// The single dropped parameter is a trailing `vararg` filled with an EMPTY array (a plain call
        /// to the target), rather than trailing DEFAULTS filled via the `$default` stub.
        vararg_tail: bool,
        /// The target function's last parameter is a `vararg`. In the `$default` path (dropped defaults
        /// ending in the vararg), the dropped vararg slot gets an empty array and NO mask bit.
        target_vararg: bool,
        /// The expected function type returns `Unit` but the target returns a value — the adapter calls
        /// the target, DISCARDS the result, and returns `Unit` (kotlin's coercion-to-`Unit` for a
        /// reference in a `() -> Unit` position).
        coerce_unit: bool,
    },
    /// A call whose selected lowering is an inline/custom emit form rather than the normal function-call
    /// path: value-class companion calls (`Result.success`) or receiver-lambda scope calls.
    InlineCall(InlineCall),
    /// Lambda literal resolution facts: receiver-function closure receiver, if any, and whether capture
    /// collection must stay shallow because the lambda is spliced by an inline call.
    Lambda(LambdaInfo),
    /// A classpath `object` used as a value. Lowering emits `getstatic <internal>.INSTANCE`.
    ObjectValue { internal: TypeName },
    /// A public static field read. Lowering emits `getstatic <owner>.<name>:<descriptor>`.
    ExternalStaticFieldRead {
        owner: TypeName,
        name: String,
        descriptor: String,
    },
    /// The `.java` member of a class literal.
    ClassLiteralJava,
    /// A bare-name call `m(args)` resolved to a MEMBER function of a classpath `object` that was imported
    /// unqualified (`import Obj.m; m()`). Kotlin dispatches this on the singleton, so lowering reads
    /// `getstatic <internal>.INSTANCE` as the receiver and invokes the member — the same shape a qualified
    /// `Obj.m(args)` produces (a receiver whose [`ObjectValue`] lowering names `internal`).
    ObjectMemberCall { internal: TypeName },
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
    /// Member-syntax invoke of a RECEIVER-function-typed value in lexical scope: `b.f()` / `b?.f()`
    /// where `f: Bar.() -> R` is a local/parameter and `Bar` has no member `f`. The receiver becomes
    /// the function value's folded-first argument: lowering reads the local `name` and emits
    /// `InvokeFunction(f, [recv, args…])`. `params` is the FULL folded parameter list (receiver
    /// first), `ret` the function type's return.
    ReceiverFnInvoke {
        name: String,
        params: Vec<Ty>,
        ret: Ty,
    },
}

/// How a selected [`ExprLowering::Invoke`] is realized: the receiver is either a function value or an
/// object whose member `invoke` operator is called.
#[derive(Clone, Debug)]
pub enum InvokeKind {
    /// Receiver is a function value (`Ty::Fun`); lowering emits a direct function invocation. `suspend`
    /// is the function type's suspend-ness: a `suspend (A)->R` value implements `Function{N+1}` (the
    /// trailing `Continuation`), so the call must thread the continuation and invoke `Function{N+1}`.
    Function { ret: Ty, suspend: bool },
    /// Receiver carries a member `operator fun invoke`; lowering calls that member. `member` is present
    /// for classpath/cross-file callables the checker selected, and absent for same-file user classes
    /// whose IR method id is only available during lowering.
    Operator {
        receiver_ty: Ty,
        member: Option<Box<crate::symbol_resolver::ResolvedMember>>,
    },
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
    /// A `kotlin.contracts.contract { … }` statement: erased metadata, never executed and emits no
    /// bytecode (kotlinc drops it at codegen). The lowerer skips it entirely.
    Erased,
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
    /// A flow-narrowed READ type for a `var` that was assigned a non-null value (`var x: Int?; x = 10`
    /// smart-casts subsequent reads to `Int`). Separate from the declared `ty`, which still governs
    /// what may be ASSIGNED (a later `x = null` stays legal). `None` = read as the declared type.
    narrowed: Option<Ty>,
}

/// The packages in scope for an unqualified TOP-LEVEL / extension function call in `file`: the leveled
/// import packages (same-package, star, explicit, defaults) plus each explicit import's package. Used to
/// scope BOTH the checker and the lowerer so the two resolve a `@JvmName`-mangled library family (e.g.
/// `sumOf` → `sumOfInt`) identically — an unscoped lowerer walk misses it and bails.
pub(crate) fn function_scope_packages(file: &File, syms: &SymbolTable) -> Vec<TypeName> {
    function_scope_packages_with(file, syms.libraries.platform_default_import_packages())
}

/// [`function_scope_packages`] with only a platform default-import list, for pre-check contexts that do
/// not yet have a full `SymbolTable`.
pub(crate) fn function_scope_packages_with(
    file: &File,
    platform_defaults: &[&str],
) -> Vec<TypeName> {
    let imports = import_map(file);
    let import_levels = import_levels(file, platform_defaults);
    let mut fn_scope: Vec<TypeName> = import_levels.iter().flatten().copied().collect();
    for fq in imports.values() {
        if let Some((pkg, _)) = fq.rsplit_once('/') {
            let pkg = type_name(pkg);
            if !fn_scope.contains(&pkg) {
                fn_scope.push(pkg);
            }
        }
    }
    // A FULLY-QUALIFIED call written inline without an import (`kotlinx.coroutines.runBlocking { … }`)
    // names its package in the source but never imports it. Add the dotted receiver of every `a.b.c.foo`
    // chain as a candidate package so the scoped `resolve_symbols` seam finds such a callee — the same
    // packages the checker's FQ-call path (`qualified_path`) resolves against. Over-adding a non-package
    // prefix is inert (no `resolve_symbols` hit); this needs no classpath knowledge to decide the boundary.
    for i in 0..file.expr_spans.len() {
        if let Expr::Member { receiver, .. } = file.expr(ExprId(i as u32)) {
            if let Some(pkg) = qualified_path(file, *receiver) {
                let pkg = type_name(&pkg);
                if pkg.contains("/") && !fn_scope.contains(&pkg) {
                    fn_scope.push(pkg);
                }
            }
        }
    }
    fn_scope
}

fn make_checker<'a>(
    file: &'a File,
    file_index: u32,
    syms: &'a SymbolTable,
    diags: &'a mut DiagSink,
) -> Checker<'a> {
    let imports = import_map(file);
    let import_levels = import_levels(file, syms.libraries.platform_default_import_packages());
    let fn_scope = function_scope_packages(file, syms);
    Checker {
        file,
        syms,
        module: crate::module_symbols::ModuleSymbols::new(syms),
        file_index,
        diags,
        expr_types: vec![Ty::Error; file.expr_arena.len()],
        scopes: Vec::new(),
        ret_ty: Ty::Unit,
        expected: None,
        imports,
        import_levels,
        fn_scope,
        tparams: Default::default(),
        reified_tparams: std::collections::HashSet::new(),
        this_ty: None,
        this_narrow: None,
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
        resolved_call_type_args: HashMap::new(),
        narrowed_this_member: HashMap::new(),
        resolved_calls: HashMap::new(),
        resolved_operator_calls: HashMap::new(),
        resolved_stmt_operator_calls: HashMap::new(),
        resolved_index_store_get_returns: HashMap::new(),
        resolved_destructure_components: HashMap::new(),
        iterator_protocols: HashMap::new(),
        synthetic_member_calls: HashMap::new(),
        bound_property_refs: HashMap::new(),
        bound_member_refs: HashMap::new(),
        property_setters: HashMap::new(),
        resolved_constructors: HashMap::new(),
        resolved_super_calls: HashMap::new(),
        resolved_default_member_calls: HashMap::new(),
        resolved_library_companion_consts: HashMap::new(),
        resolved_library_enum_entries: HashMap::new(),
        resolved_call_arg_slots: HashMap::new(),
        synthetic_ext_calls: HashMap::new(),
        delegate_getvalue_targets: HashMap::new(),
        super_ctor_params: HashMap::new(),
        context_args: HashMap::new(),
        fn_reassigned: std::collections::HashSet::new(),
        fn_closure_reassigned: std::collections::HashSet::new(),
        narrow_active: false,
        expr_depth: 0,
        allow_lambda_mutation: false,
        loop_labels: Vec::new(),
    }
}

/// One pre-inference pass over `file`: check every EXPRESSION-body top-level function / class method
/// whose return type is not declared, and patch the inferred return into `syms` (funs, extension funs,
/// class methods). Returns `true` if any signature's return type changed — so a caller can iterate to a
/// fixpoint (a body that calls another expr-body decl declared later, or in another file). Runs on a
/// scratch `DiagSink` (inference diagnostics are not the real check's).
fn preinfer_returns_pass(file: &File, file_index: u32, syms: &mut SymbolTable) -> bool {
    let mut scratch = DiagSink::new();
    let mut pre = make_checker(file, file_index, &*syms, &mut scratch);
    for &d in &file.decls {
        if let Decl::Fun(f) = file.decl(d) {
            if f.ret.is_none() && matches!(f.body, FunBody::Expr(_)) {
                let resolve = class_internal_resolver(pre.syms);
                pre.tparams =
                    TParams::from_decl_with(&f.type_params, &f.type_param_bounds, &resolve);
                pre.reified_tparams = f.reified_type_params.iter().cloned().collect();
                pre.check_fun(f, Some(d));
                pre.tparams.clear();
                pre.reified_tparams.clear();
            }
        } else if let Decl::Class(cl) = file.decl(d) {
            let Some(internal) = pre.syms.classes.get(&cl.name).map(ClassSig::internal) else {
                continue;
            };
            pre.this_ty = Some(Ty::obj(&internal));
            for m in &cl.methods {
                if m.ret.is_none() && matches!(m.body, FunBody::Expr(_)) {
                    let resolve = class_internal_resolver(pre.syms);
                    pre.tparams =
                        TParams::from_decl_with(&m.type_params, &m.type_param_bounds, &resolve);
                    pre.reified_tparams = m.reified_type_params.iter().cloned().collect();
                    pre.check_method(m, &[]);
                    pre.tparams.clear();
                    pre.reified_tparams.clear();
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
    for ((file, decl), ret) in fun_rets {
        if let Some(sig) = syms.funs.values_mut().find_map(|sigs| {
            sigs.iter_mut()
                .find(|s| s.source_file == Some(file) && s.source_decl == Some(DeclId(decl)))
        }) {
            changed |= sig.ret != ret;
            sig.ret = ret;
        }
    }
    for ((recv, name, params), ret) in ext_rets {
        if let Some(sig) = syms
            .ext_funs
            .get_mut(&(recv, name))
            .and_then(|ov| ov.iter_mut().find(|s| s.params == params))
        {
            changed |= sig.ret != ret;
            sig.ret = ret;
        }
    }
    for ((internal, name, params), ret) in method_rets {
        if let Some(sig) = syms
            .class_by_type_name_mut(internal)
            .and_then(|c| c.methods.get_mut(&name))
            .and_then(|ov| ov.iter_mut().find(|s| s.params == params))
        {
            changed |= sig.ret != ret;
            sig.ret = ret;
        }
    }
    changed
}

/// Pre-infer EXPRESSION-body return types across the WHOLE module (every file), patching `syms` before
/// any file's main check. Per-file `check_file_at` only pre-infers its own file, so a call in file A to
/// an expression-body method defined in file B (`Obj.all() = listOf(...)` in another file, read as its
/// erased `java/util/List` instead of the inferred `List<Role>`) would resolve against the collection
/// default until B is processed. Iterating a global fixpoint over all files closes that cross-file gap.
pub fn preinfer_module_returns(files: &[File], syms: &mut SymbolTable, diags: &mut DiagSink) {
    let saved = diags.current_file();
    for _pass in 0..8 {
        let mut changed = false;
        for (i, file) in files.iter().enumerate() {
            diags.set_file(i as u32);
            changed |= preinfer_returns_pass(file, i as u32, syms);
        }
        if !changed {
            break;
        }
    }
    diags.set_file(saved);
}

pub fn check_file_at(
    file: &File,
    file_index: u32,
    syms: &mut SymbolTable,
    diags: &mut DiagSink,
) -> TypeInfo {
    // Pre-infer EXPRESSION-body return types (top-level functions AND class methods) and patch the
    // signature table BEFORE the main check — so a call to `fun m() = f()` resolves to its real return,
    // not the collection default `Unit`. Without this, a method whose return couldn't be inferred at
    // COLLECTION (an inherited-method-calling body in an anonymous object / hoisted local class → `Unit`)
    // is still `Unit` when a SIBLING call resolves it earlier in the same file
    // (`object { fun foo4() = foo3() }.apply { foo4() }`). A body that calls another expr-body method
    // declared LATER (forward reference) needs a second pass, so iterate to a FIXPOINT (bounded — the
    // dependency chain is shallow; an unresolvable case simply stops improving).
    for _pass in 0..8 {
        if !preinfer_returns_pass(file, file_index, syms) {
            break;
        }
    }

    let mut c = make_checker(file, file_index, &*syms, diags);
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
    c.check_no_erased_clash(&top_funs);

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
                c.reified_tparams = f.reified_type_params.iter().cloned().collect();
                c.check_fun(f, Some(d));
                c.tparams.clear();
                c.reified_tparams.clear();
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
                // specialize — see `TParams::erased`). An `inner class` also captures the enclosing
                // instance, so the outer class's type parameters are visible in its members
                // (kotlinc); walk the `inner_of` chain and include each enclosing class's parameters.
                let mut tparam_names = cl.type_params.clone();
                {
                    let mut outer = cl.inner_of.as_deref().and_then(|o| {
                        syms.classes
                            .get(o)
                            .map(ClassSig::internal_name)
                            .or_else(|| existing_type_name(o))
                    });
                    let mut guard = 0;
                    while let Some(o) = outer {
                        guard += 1;
                        if guard > 32 {
                            break;
                        }
                        if let Some(s) = syms.class_by_type_name(o) {
                            tparam_names.extend(s.tparam_names.iter().cloned());
                            outer = s.inner_of_name();
                        } else {
                            break;
                        }
                    }
                }
                c.tparams = TParams::erased(&tparam_names);
                // Member functions are checked with the class's properties (resolved in Stage C)
                // visible as an implicit `this` scope.
                let mut props = syms
                    .classes
                    .get(&cl.name)
                    .map(|s| s.props.clone())
                    .unwrap_or_default();
                // An inner class's methods can read the enclosing instance's properties (via `this$0`);
                // make the outer class's backing-field properties resolvable as implicit-`this` members.
                if let Some(os) = cl.inner_of.as_deref().and_then(|outer| {
                    syms.classes.get(outer).or_else(|| {
                        existing_type_name(outer).and_then(|o| syms.class_by_type_name(o))
                    })
                }) {
                    props.extend(os.props.clone());
                }
                c.this_ty = syms
                    .classes
                    .get(&cl.name)
                    .map(|s| Ty::obj_name(s.internal_name()));
                // Push the enclosing-class labels for the duration of this class's member checks: the
                // OUTER chain first (`this@Outer` for an `inner class`, resolved via `this$0`), then the
                // class's own label (`this@C`) innermost. Walk `inner_of` outward.
                let mut label_depth = 0usize;
                {
                    let mut chain: Vec<(String, Ty)> = Vec::new();
                    let mut outer = cl.inner_of.as_deref().and_then(|o| {
                        syms.classes
                            .get(o)
                            .map(ClassSig::internal_name)
                            .or_else(|| existing_type_name(o))
                    });
                    while let Some(o) = outer {
                        if let Some(s) = syms.class_by_type_name(o) {
                            let key = syms
                                .class_simple_name(o)
                                .unwrap_or("<anonymous>")
                                .to_string();
                            chain.push((key, Ty::obj_name(s.internal_name())));
                            outer = s.inner_of_name();
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
                c.check_no_erased_clash(&methods);
                if let Some(internal) = syms.classes.get(&cl.name).map(ClassSig::internal_name) {
                    // An `override` member must MATCH a supertype member (same name + arity) —
                    // kotlinc rejects an `override` that overrides nothing. With member overloads a
                    // same-name sibling of a different arity no longer pairs, so check explicitly.
                    // Only when the hierarchy is MODULE-closed: a classpath supertype's members are
                    // invisible to this walk, so absence there proves nothing (properties and
                    // property-overrides are likewise out of scope — this checks methods only).
                    if c.syms.hierarchy_is_module_closed(internal) {
                        let supers = c.syms.supertype_methods_name(internal);
                        // `kotlin/Any`'s universal members are overridable in every class but never
                        // appear in the module supertype walk.
                        let is_any_member = |m: &FunDecl| {
                            matches!(
                                (m.name.as_str(), m.params.len()),
                                ("toString", 0) | ("hashCode", 0) | ("equals", 1)
                            )
                        };
                        for m in &cl.methods {
                            if m.is_override
                                && !is_any_member(m)
                                && !supers
                                    .iter()
                                    .any(|(n, s)| *n == m.name && s.params.len() == m.params.len())
                            {
                                c.diags
                                    .error(m.span, format!("'{}' overrides nothing", m.name));
                            }
                        }
                    }
                    c.check_no_bridge_needed(internal, cl.span);
                    // A `data class` implementing an interface that declares `copy`/`componentN` would
                    // need bridges for its *synthesized* members (which return the class itself, not
                    // the supertype) — krusty doesn't emit those, so reject (cleanly skip).
                    if cl.is_data {
                        let supers = syms.supertype_methods_name(internal);
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
                        !bp.is_var
                            && bp.init.is_none()
                            && (bp.getter.is_none() || bp.getter_reads_field)
                            && bp.ty.is_some()
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
                            // `this(…)` targets the primary constructor OR a SIBLING secondary (never
                            // itself). Collect the candidate signatures and check assignability against
                            // the unique same-arity one — the lowering resolves the exact target (and
                            // bails on ambiguity), so a multi-candidate arity match isn't rejected here.
                            let mut candidates: Vec<Vec<Ty>> = Vec::new();
                            if cl.has_primary_ctor {
                                candidates.push(primary_params.clone());
                            }
                            for s in &cl.secondary_ctors {
                                if !std::ptr::eq(s, sc) {
                                    candidates.push(
                                        s.params.iter().map(|p| c.resolve_ty(&p.ty)).collect(),
                                    );
                                }
                            }
                            let same: Vec<&Vec<Ty>> =
                                candidates.iter().filter(|p| p.len() == ats.len()).collect();
                            if same.is_empty() {
                                c.diags.error(
                                    sc.span,
                                    format!(
                                        "krusty: this(…) has no target constructor taking {} args",
                                        ats.len()
                                    ),
                                );
                            } else if same.len() == 1 {
                                for (i, (p, a)) in same[0].iter().zip(&ats).enumerate() {
                                    c.expect_assignable(*p, *a, c.span(args[i]), "this() argument");
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
                        c.with_ret(Ty::Unit, |c| {
                            c.expr(body);
                        });
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
                        c.check_default_arg(&p.ty, dx, pty);
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
                        !bp.is_var
                            && bp.init.is_none()
                            && (bp.getter.is_none() || bp.getter_reads_field)
                            && bp.ty.is_some()
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
                let base_arg_tys: Vec<Ty> = cl.base_args.iter().map(|&arg| c.expr(arg)).collect();
                // Resolve which base constructor `super(args)` targets — uniformly for a same-file,
                // module, or classpath base via the symbol source — and record its parameter types so
                // the lowerer emits `super(args)` against them instead of re-resolving the constructor.
                if !cl.base_args.is_empty() {
                    let internal = class_internal(c.file, &cl.name);
                    let base_internal = c
                        .syms
                        .class_by_internal(&internal)
                        .and_then(ClassSig::super_internal_name);
                    if let Some(base_int) = base_internal {
                        if let Some(params) =
                            c.resolve_super_ctor_params_name(base_int, &base_arg_tys)
                        {
                            c.super_ctor_params.insert(internal, params);
                        }
                    }
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
                        let dt = c.expr(de);
                        c.record_delegate_getvalue(de, dt);
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
                                    .find_map(|(n, t, _)| (n == &bp.name).then_some(*t))
                            })
                        })
                        .unwrap_or(Ty::Error);
                    if let Some(getter) = &bp.getter {
                        c.with_ret_field(prop_ty, Some(prop_ty), |c| match getter {
                            FunBody::Expr(g) => {
                                let gt = c.expr(*g);
                                c.expect_assignable(prop_ty, gt, c.span(*g), "getter body");
                            }
                            FunBody::Block(g) => {
                                let _ = c.expr(*g);
                            }
                            FunBody::None => {}
                        });
                    }
                    if let Some(setter) = &bp.setter {
                        if let Some(body) = &setter.body {
                            c.with_ret_field(Ty::Unit, Some(prop_ty), |c| {
                                c.push_scope();
                                let pname =
                                    crate::ast::setter_param_or_value(setter.param.as_ref());
                                c.declare(&pname, prop_ty, true);
                                match body {
                                    FunBody::Expr(g) | FunBody::Block(g) => {
                                        let _ = c.expr(*g);
                                    }
                                    FunBody::None => {}
                                }
                                c.pop_scope();
                            });
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
                        // A NAMED argument (`C(b = "b")`) is checked against the type of the parameter it
                        // names, not the one at its textual position; positional arguments keep order.
                        let mut next_pos = 0usize;
                        for (j, a) in entry.args.iter().enumerate() {
                            let idx = match entry.arg_names.get(j).and_then(|n| n.as_ref()) {
                                Some(name) => cl.props.iter().position(|p| &p.name == name),
                                None => {
                                    let p = next_pos;
                                    next_pos += 1;
                                    Some(p)
                                }
                            };
                            let expected = idx.and_then(|i| ctor_tys.get(i)).copied();
                            // A lambda passed to a function-typed enum-ctor parameter (`plus("+",
                            // { x, y -> x + y })`) binds its parameter types from that parameter's
                            // `Ty::Fun` — like a constructor/function call — so the body sees real types.
                            let at = match expected {
                                Some(Ty::Fun(s))
                                    if matches!(c.file.expr(*a), Expr::Lambda { .. }) =>
                                {
                                    let pts = s.params.clone();
                                    c.check_lambda_with_types(*a, &pts)
                                }
                                _ => c.expr(*a),
                            };
                            if let Some(expected_ty) = expected {
                                c.expect_assignable(
                                    expected_ty,
                                    at,
                                    c.span(*a),
                                    "enum entry argument",
                                );
                            }
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
                        c.check_fun(m, None);
                    }
                    c.companion_of = None;
                }
                c.tparams.clear();
            }
            Decl::Property(p) => {
                // An extension property's own generic type parameters (`val <T> Array<T>.length: Int`)
                // scope over its receiver, declared type, and accessor bodies — bind them (erased) so
                // `T` resolves rather than reading as an unresolved reference.
                let resolve = class_internal_resolver(c.syms);
                c.tparams = TParams::from_decl_with(&p.type_params, &p.type_param_bounds, &resolve);
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
                                    .map(|s| s.ty)
                            })
                        })
                        .unwrap_or(Ty::Error);
                // A top-level computed property (`val g: T get() = …`) emits a `getG()` static method
                // (Phase: top-level computed). Type-check the getter body against the declared type. A
                // top-level backing-field property (`val x = init get() = field`) binds `field` to the
                // property type for the accessor body (like a member accessor).
                let has_backing_field = p.receiver.is_none() && p.init.is_some();
                if let Some(g) = &p.getter {
                    let field_ty = has_backing_field.then_some(prop_ty);
                    c.with_ret_field(prop_ty, field_ty, |c| match g {
                        FunBody::Expr(e) => {
                            let gt = c.expr(*e);
                            c.expect_assignable(prop_ty, gt, c.span(*e), "getter body");
                        }
                        FunBody::Block(b) => {
                            let _ = c.expr(*b);
                        }
                        FunBody::None => {}
                    });
                }
                // A setter body: an extension property's is checked with `this` = receiver; a top-level
                // backing-field property's binds `field` to the property type. Both bind the value param.
                if p.receiver.is_some() || has_backing_field {
                    if let Some(setter) = &p.setter {
                        if let Some(body) = &setter.body {
                            let field_ty = has_backing_field.then_some(prop_ty);
                            c.with_ret_field(Ty::Unit, field_ty, |c| {
                                c.push_scope();
                                let pname =
                                    crate::ast::setter_param_or_value(setter.param.as_ref());
                                c.declare(&pname, prop_ty, true);
                                match body {
                                    FunBody::Expr(g) | FunBody::Block(g) => {
                                        let _ = c.expr(*g);
                                    }
                                    FunBody::None => {}
                                }
                                c.pop_scope();
                            });
                        }
                    }
                }
                c.this_ty = prev_this;
                // A delegated property's delegate expression (`by Del()`) must be type-checked so its
                // (and its sub-expressions') types are recorded for the lowering of `x$delegate`.
                if let Some(de) = p.delegate {
                    let dt = c.expr(de);
                    c.record_delegate_getvalue(de, dt);
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
                c.tparams.clear();
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
        resolved_call_type_args,
        narrowed_this_member,
        resolved_calls,
        resolved_operator_calls,
        resolved_stmt_operator_calls,
        resolved_index_store_get_returns,
        resolved_destructure_components,
        iterator_protocols,
        synthetic_member_calls,
        bound_property_refs,
        bound_member_refs,
        property_setters,
        resolved_constructors,
        resolved_super_calls,
        resolved_default_member_calls,
        resolved_library_companion_consts,
        resolved_library_enum_entries,
        resolved_call_arg_slots,
        synthetic_ext_calls,
        delegate_getvalue_targets,
        context_args,
        super_ctor_params,
        ..
    } = c;
    for (internal, params) in super_ctor_params {
        if let Some(cs) = syms.class_by_internal_mut(&internal) {
            cs.super_ctor_params = params;
        }
    }
    for ((file, decl), ret) in inferred_fun_rets {
        if let Some(sig) = syms.funs.values_mut().find_map(|sigs| {
            sigs.iter_mut()
                .find(|s| s.source_file == Some(file) && s.source_decl == Some(DeclId(decl)))
        }) {
            sig.ret = ret;
        }
    }
    for ((recv, name, params), ret) in inferred_ext_fun_rets {
        if let Some(sig) = syms
            .ext_funs
            .get_mut(&(recv, name))
            .and_then(|ov| ov.iter_mut().find(|s| s.params == params))
        {
            sig.ret = ret;
        }
    }
    for ((internal, name, params), ret) in inferred_method_rets {
        if let Some(sig) = syms
            .class_by_type_name_mut(internal)
            .and_then(|c| c.methods.get_mut(&name))
            .and_then(|ov| ov.iter_mut().find(|s| s.params == params))
        {
            sig.ret = ret;
        }
    }
    TypeInfo {
        expr_types,
        expr_lowers,
        stmt_lowers,
        local_decl_types,
        resolved_call_type_args,
        narrowed_this_member,
        resolved_calls,
        resolved_operator_calls,
        resolved_stmt_operator_calls,
        resolved_index_store_get_returns,
        resolved_destructure_components,
        iterator_protocols,
        synthetic_member_calls,
        bound_property_refs,
        bound_member_refs,
        property_setters,
        resolved_constructors,
        resolved_super_calls,
        resolved_default_member_calls,
        resolved_library_companion_consts,
        resolved_library_enum_entries,
        resolved_call_arg_slots,
        synthetic_ext_calls,
        delegate_getvalue_targets,
        context_args,
    }
}

pub fn check_file(file: &File, syms: &mut SymbolTable, diags: &mut DiagSink) -> TypeInfo {
    check_file_at(file, diags.current_file(), syms, diags)
}

struct Checker<'a> {
    file: &'a File,
    syms: &'a SymbolTable,
    /// This compilation's declarations as a [`SymbolSource`], federated OVER the classpath by the resolver
    /// so a user function/type shadows a library one of the same name. Borrows the same `syms`.
    module: crate::module_symbols::ModuleSymbols<'a>,
    file_index: u32,
    diags: &'a mut DiagSink,
    expr_types: Vec<Ty>,
    scopes: Vec<HashMap<String, Local>>,
    ret_ty: Ty,
    /// The EXPECTED type of the expression about to be checked, propagated from an enclosing typed
    /// context (a declared `val f: (Int) -> Int = …`) into RESULT positions (an `if`/`when` branch, a
    /// block's trailing value) so a bare lambda literal there takes its parameter types from the
    /// expectation instead of erasing to `Any`. Consumed (cleared) at each `expr()` entry, so it only
    /// reaches the immediate expression; propagation sites re-arm it via [`Self::expr_expected`].
    expected: Option<Ty>,
    imports: HashMap<String, String>,
    /// Star/implicit import packages by kotlinc precedence level (same-package, explicit stars, Kotlin
    /// defaults, platform defaults) — the import set [`Self::imported_type_internal`] resolves against.
    import_levels: [Vec<TypeName>; 4],
    /// The packages in scope for TOP-LEVEL function resolution: every `import_levels` package PLUS the
    /// package of each explicit import (`import a.b.foo` scopes `a/b`). A top-level call resolves only to
    /// a function whose facade is in this set (kotlinc), passed to the [`SymbolResolver`].
    fn_scope: Vec<TypeName>,
    /// Generic type parameters in scope (erased to `java/lang/Object`).
    tparams: TParams,
    /// The `reified` type parameters in scope (a subset of `tparams`, from the enclosing `inline fun`).
    /// A class literal `T::class` is only valid on a reified `T` (kotlinc rejects it otherwise).
    reified_tparams: std::collections::HashSet<String>,
    /// The type of `this` when checking class members (`None` at top level).
    this_ty: Option<Ty>,
    /// A flow-narrowing of the implicit receiver established by `if (this is B)`: `this` is known to
    /// be `B` (a subtype of `this_ty`) inside the guarded branch, so a bare member of `B` resolves.
    /// Separate from `this_ty` (which stays the DECLARED receiver type so `this` still types as it,
    /// and the lowerer inserts a `checkcast` only where the narrowing was actually used). Cleared at
    /// every scope boundary, like local flow-narrowings.
    this_narrow: Option<Ty>,
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
    inferred_fun_rets: HashMap<(u32, u32), Ty>,
    inferred_ext_fun_rets: HashMap<(Ty, String, Vec<Ty>), Ty>,
    inferred_method_rets: HashMap<(TypeName, String, Vec<Ty>), Ty>,
    /// A class internal name → the base-constructor parameter types its `super(args)` resolved to
    /// (see [`ClassSig::super_ctor_params`]). Stashed during checking (where the argument types are
    /// known) and applied to the `ClassSig` after, since `syms` is borrowed immutably while checking.
    super_ctor_params: HashMap<String, Vec<Ty>>,
    stmt_lowers: HashMap<StmtId, StmtLowering>,
    local_decl_types: HashMap<StmtId, Ty>,
    resolved_call_type_args: HashMap<ExprId, Vec<Ty>>,
    narrowed_this_member: HashMap<ExprId, TypeName>,
    /// Calls resolved during checking, keyed by the `Expr::Call` `ExprId` (moved into
    /// [`TypeInfo::resolved_calls`] so the lowerer reads them instead of re-resolving). See
    /// [`ResolvedCall`] for the variants.
    resolved_calls: HashMap<ExprId, ResolvedCall>,
    resolved_operator_calls: HashMap<(ExprId, SyntheticOperatorCall), ResolvedCall>,
    resolved_stmt_operator_calls: HashMap<(StmtId, SyntheticOperatorCall), ResolvedCall>,
    resolved_index_store_get_returns: HashMap<StmtId, Ty>,
    resolved_destructure_components: HashMap<(StmtId, usize), DestructureComponentTarget>,
    iterator_protocols: HashMap<ExprId, IteratorProtocolTarget>,
    synthetic_member_calls: HashMap<(ExprId, String), crate::libraries::LibraryMember>,
    bound_property_refs: HashMap<ExprId, crate::symbol_resolver::BoundPropertyRef>,
    bound_member_refs: HashMap<ExprId, crate::libraries::LibraryMember>,
    property_setters: HashMap<StmtId, crate::libraries::LibraryCallable>,
    resolved_constructors: HashMap<ExprId, ResolvedConstructor>,
    resolved_super_calls: HashMap<ExprId, ResolvedSuperCall>,
    resolved_default_member_calls: HashMap<ExprId, ResolvedDefaultMemberCall>,
    resolved_library_companion_consts: HashMap<ExprId, crate::libraries::LibraryConst>,
    resolved_library_enum_entries: HashMap<ExprId, TypeName>,
    resolved_call_arg_slots: HashMap<ExprId, Vec<Option<ExprId>>>,
    synthetic_ext_calls: HashMap<(ExprId, String), crate::libraries::LibraryCallable>,
    delegate_getvalue_targets: HashMap<ExprId, DelegateGetValueTarget>,
    /// Implicit context arguments per call (see [`TypeInfo::context_args`]).
    context_args: HashMap<ExprId, Vec<String>>,
    /// Names reassigned anywhere in the function body currently being checked (including inside its
    /// closures). A captured `var` is boxed only if it's in here — kotlinc treats a captured-but-never-
    /// reassigned `var` as effectively final (passed by value).
    fn_reassigned: std::collections::HashSet<String>,
    /// Names reassigned INSIDE a closure (lambda) within the function body. A `var` in here can be set
    /// (e.g. to null) by a closure invoked between a narrowing assignment and a later read, so it is
    /// never flow-narrowed after an assignment (soundness for [`Local::narrowed`]).
    fn_closure_reassigned: std::collections::HashSet<String>,
    /// True while any [`Local::narrowed`] flow-narrowing is set — a cheap guard so the common (no
    /// narrowing) path skips the scope walk in [`Self::clear_local_narrows`] on every push/pop.
    narrow_active: bool,
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

enum ClasspathMemberSlotCall {
    Resolved(Ty),
    Ambiguous,
    NoMatch,
}

/// Record a type-parameter → type binding, tracking a CONFLICT when the same parameter is bound to two
/// different types across binding sites (a plain-value arg and a lambda return, or two lambda returns).
/// The first binding is kept in `binds` (for the selection paths); `conflicted` names the ambiguous
/// parameters so the generic-return inference can decline (their real type argument is a common
/// supertype / intersection krusty can't compute).
fn bind_or_conflict<'k>(
    binds: &mut std::collections::HashMap<&'k str, Ty>,
    conflicted: &mut std::collections::HashSet<&'k str>,
    name: &'k str,
    ty: Ty,
) {
    match binds.get(name) {
        Some(&prev) if prev != ty => {
            conflicted.insert(name);
        }
        Some(_) => {}
        None => {
            binds.insert(name, ty);
        }
    }
}

impl crate::assignable::TypeOracle for Checker<'_> {
    fn direct_supertypes(&self, internal: TypeName) -> Vec<TypeName> {
        self.resolver()
            .resolve_type_name(internal)
            .map(|t| t.supertypes.iter_ids().collect())
            .unwrap_or_default()
    }

    fn same_class_name(&self, a: TypeName, b: TypeName) -> bool {
        let a = self.syms.libraries.library_value_form_name(a);
        let b = self.syms.libraries.library_value_form_name(b);
        crate::symbol_resolver::platform_type_names_match(a, b)
    }
}

impl<'a> Checker<'a> {
    fn resolved_type(&self, internal: &str) -> Option<crate::libraries::LibraryType> {
        self.resolver().resolve_type(internal)
    }

    fn resolved_type_name(
        &self,
        internal: TypeName,
    ) -> Option<std::rc::Rc<crate::libraries::LibraryType>> {
        self.resolver().resolve_type_name(internal)
    }

    fn with_ret<R>(&mut self, ret_ty: Ty, f: impl FnOnce(&mut Self) -> R) -> R {
        let prev = std::mem::replace(&mut self.ret_ty, ret_ty);
        let r = f(self);
        self.ret_ty = prev;
        r
    }

    fn with_ret_field<R>(
        &mut self,
        ret_ty: Ty,
        field_ty: Option<Ty>,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let prev_ret = std::mem::replace(&mut self.ret_ty, ret_ty);
        let prev_field = std::mem::replace(&mut self.field_ty, field_ty);
        let r = f(self);
        self.ret_ty = prev_ret;
        self.field_ty = prev_field;
        r
    }

    fn with_lambda_mutation<R>(&mut self, allow: bool, f: impl FnOnce(&mut Self) -> R) -> R {
        let prev = std::mem::replace(&mut self.allow_lambda_mutation, allow);
        let r = f(self);
        self.allow_lambda_mutation = prev;
        r
    }

    fn with_this_narrow<R>(&mut self, narrow: Option<Ty>, f: impl FnOnce(&mut Self) -> R) -> R {
        let prev = self.this_narrow;
        if let Some(n) = narrow {
            self.this_narrow = Some(n);
        }
        let r = f(self);
        self.this_narrow = prev;
        r
    }

    fn check_default_arg(&mut self, ty_ref: &TypeRef, default: ExprId, pty: Ty) {
        let dty = if matches!(self.file.expr(default), Expr::Lambda { .. })
            && (!ty_ref.fun_params.is_empty() || ty_ref.name == "<fun>")
        {
            let lam_pts: Vec<Ty> = ty_ref
                .fun_params
                .iter()
                .map(|r| self.resolve_ty(r))
                .collect();
            self.check_lambda_with_types(default, &lam_pts)
        } else {
            self.expr(default)
        };
        self.expect_assignable(pty, dty, self.span(default), "default argument");
    }

    /// The arg-binding call-resolution layer over this checker's [`SymbolSource`]. Cheap to construct.
    fn resolver(&self) -> crate::symbol_resolver::SymbolResolver<'_> {
        crate::symbol_resolver::SymbolResolver::new_scoped_with_module(
            &*self.syms.libraries,
            &self.module,
            &self.fn_scope,
        )
    }

    /// A resolver whose top-level scope is an EXPLICIT package `scope`, for a fully-qualified reference
    /// (`kotlinx.coroutines.runBlocking`): the package is resolution SCOPE, not part of the name — a dotted
    /// name can't be split into package/class by inspection (a package segment may be capitalized), so the
    /// caller, which already resolved which prefix is the package, supplies it here.
    fn resolver_in_scope<'s>(
        &'s self,
        scope: &'s [TypeName],
    ) -> crate::symbol_resolver::SymbolResolver<'s> {
        crate::symbol_resolver::SymbolResolver::new_scoped_with_module(
            &*self.syms.libraries,
            &self.module,
            scope,
        )
    }

    /// The federated symbol source (this module SHADOWING the classpath) — the source the resolver's
    /// receiver-rank / extension-ranking helpers key on. Cheap to build (borrows the two child sources).
    fn fed_source(&self) -> crate::symbol_source::CompositeSource<'_> {
        crate::symbol_source::CompositeSource::new(vec![
            &self.module as &dyn SymbolSource,
            &*self.syms.libraries as &dyn SymbolSource,
        ])
    }

    // Lambda call-SHAPE derivation over the resolver's overload family. These consume
    // `resolve_symbol(...).overloads()` (the ONE resolution seam) and align the candidates' generic
    // signatures / call-shape against a PARTIAL argument list (lambda slots not yet typed) — a checker
    // concern (typing lambda arguments), so it lives here rather than on `SymbolResolver`.

    /// Resolve `receiver.name(lambda)` where the return type binds from the lambda's return. Returns the
    /// callable plus `is_member` (`true` = instance member → `invokevirtual`; `false` = extension → static
    /// call with the receiver as `args[0]`).
    fn lambda_return_overload(
        &self,
        receiver: Ty,
        name: &str,
        lambda_ret: Ty,
        arg_tys: &[Ty],
    ) -> Option<(crate::libraries::LibraryCallable, bool)> {
        if arg_tys.len() != 1 {
            return None;
        }
        let src = self.fed_source();
        let mro = crate::symbol_resolver::ReceiverMro::new(&src, receiver);
        self.resolver()
            .resolve_symbol(crate::symbol_resolver::SymRecv::TopLevel, name, &[], &[])
            .map(crate::symbol_resolver::Symbol::overloads)
            .unwrap_or_default()
            .into_iter()
            .find(|o| {
                matches!(o.kind, crate::libraries::FnKind::Member | crate::libraries::FnKind::Extension)
                    && !matches!(o.callable.origin, crate::libraries::Origin::Module { .. })
                    && o.callable.ret == lambda_ret
                    && match o.kind {
                        crate::libraries::FnKind::Extension => {
                            o.receiver.is_none_or(|dr| mro.rank(&src, dr).is_some())
                        }
                        _ => true,
                    }
            })
            .map(|o| {
                crate::trace_compiler!(
                    "resolve",
                    "lambda-return {name} recv={receiver:?} lambda_ret={lambda_ret:?} -> {}.{}{} kind={:?}",
                    o.callable.owner.render(),
                    o.callable.name,
                    o.callable.descriptor,
                    o.kind
                );
                let is_member = o.kind == crate::libraries::FnKind::Member;
                (o.callable, is_member)
            })
    }

    /// Parameter types for the lambda argument of a call selected by lambda return type
    /// (`Iterable<T>.sumOf { … }`), read from the selected overload family.
    fn lambda_return_overload_param_types(&self, receiver: Ty, name: &str) -> Option<Vec<Ty>> {
        let src = self.fed_source();
        let mro = crate::symbol_resolver::ReceiverMro::new(&src, receiver);
        self.resolver()
            .resolve_symbol(crate::symbol_resolver::SymRecv::TopLevel, name, &[], &[])
            .map(crate::symbol_resolver::Symbol::overloads)
            .unwrap_or_default()
            .iter()
            .filter(|o| {
                o.is_extension() && o.receiver.is_none_or(|dr| mro.rank(&src, dr).is_some())
            })
            .find_map(|o| {
                let gsig = o.generic_sig.as_ref()?;
                let mut binds = std::collections::HashMap::new();
                if let Some(recv_sig) = gsig.receiver {
                    crate::symbol_resolver::unify_ty(recv_sig, receiver, &mut binds);
                }
                gsig.params
                    .first()
                    .map(|selector| crate::symbol_resolver::function_input_types(*selector, &binds))
                    .filter(|params| !params.is_empty())
            })
    }

    /// Lambda call-shape facts for a receiver-less top-level call, aligned to the PARTIAL argument list
    /// (a generic HOF binds lambda parameter types from the already-typed non-lambda args).
    fn top_level_lambda_shape(
        &self,
        name: &str,
        arg_tys: &[Option<Ty>],
    ) -> Option<crate::symbol_resolver::TopLevelLambdaShape> {
        let fs = crate::libraries::FunctionSet {
            overloads: self
                .resolver()
                .resolve_symbol(crate::symbol_resolver::SymRecv::TopLevel, name, &[], &[])
                .map(crate::symbol_resolver::Symbol::overloads)
                .unwrap_or_default(),
        };
        let has_exact = fs.has_top_level_arity(arg_tys.len());
        let mut shape = crate::symbol_resolver::TopLevelLambdaShape::default();
        for o in fs.top_level() {
            if shape.param_types.is_none() {
                if let Some(gsig) = o.generic_sig.as_ref() {
                    if !(has_exact && gsig.params.len() != arg_tys.len()) {
                        if let Some(map) = crate::symbol_resolver::trailing_default_arg_indices(
                            gsig.params.len(),
                            arg_tys,
                        ) {
                            let mut binds = std::collections::HashMap::new();
                            for (ai, at) in arg_tys.iter().enumerate() {
                                if let (Some(t), Some(ps)) = (at, gsig.params.get(map[ai])) {
                                    crate::symbol_resolver::unify_ty(*ps, *t, &mut binds);
                                }
                            }
                            let out: Vec<Vec<Ty>> = map
                                .iter()
                                .map(|&pi| {
                                    gsig.params
                                        .get(pi)
                                        .map(|ps| {
                                            crate::symbol_resolver::function_input_types(
                                                *ps, &binds,
                                            )
                                        })
                                        .unwrap_or_default()
                                })
                                .collect();
                            if out
                                .iter()
                                .zip(arg_tys)
                                .any(|(v, at)| at.is_none() && !v.is_empty())
                            {
                                shape.param_types = Some(out);
                            }
                        }
                    }
                }
            }
            if shape.receivers.is_none() {
                let recvs = &o.call_sig.lambda_receivers;
                if !(has_exact && recvs.len() != arg_tys.len()) {
                    if let Some(map) =
                        crate::symbol_resolver::trailing_default_arg_indices(recvs.len(), arg_tys)
                    {
                        let out: Vec<Option<Ty>> = map
                            .iter()
                            .map(|&pi| recvs.get(pi).cloned().flatten())
                            .collect();
                        if out.iter().any(Option::is_some) {
                            shape.receivers = Some(out);
                        }
                    }
                }
            }
            if shape.materialized.is_none() {
                let m = &o.call_sig.lambda_materialized;
                if m.len() == arg_tys.len() && m.iter().any(|b| *b) {
                    shape.materialized = Some(m.clone());
                }
            }
            if shape.param_types.is_some()
                && shape.receivers.is_some()
                && shape.materialized.is_some()
            {
                break;
            }
        }
        (shape.param_types.is_some() || shape.receivers.is_some() || shape.materialized.is_some())
            .then_some(shape)
    }

    /// Lambda parameter types for an extension call before lambda bodies are typed — binds the selected
    /// extension's generic signature from the receiver plus already-typed non-lambda args.
    fn extension_lambda_param_types(
        &self,
        receiver: Ty,
        name: &str,
        arg_tys: &[Option<Ty>],
    ) -> Option<Vec<Vec<Ty>>> {
        let src = self.fed_source();
        let fs = crate::libraries::FunctionSet {
            overloads: self
                .resolver()
                .resolve_symbol(
                    crate::symbol_resolver::SymRecv::Value(receiver),
                    name,
                    &[],
                    &[],
                )
                .map(crate::symbol_resolver::Symbol::overloads)
                .unwrap_or_default()
                .into_iter()
                .filter(crate::libraries::FunctionInfo::is_extension)
                .collect(),
        };
        for allow_must_inline in [false, true] {
            for o in crate::symbol_resolver::ranked_extension_overloads_by_recv(
                &src,
                receiver,
                &fs,
                allow_must_inline,
            ) {
                let Some(gsig) = o.generic_sig.as_ref() else {
                    continue;
                };
                let Some(param_indices) = crate::symbol_resolver::trailing_default_arg_indices(
                    gsig.params.len(),
                    arg_tys,
                ) else {
                    continue;
                };
                let mapped: Vec<Ty> = param_indices.iter().map(|&i| gsig.params[i]).collect();
                let mut binds = std::collections::HashMap::new();
                if let Some(recv_sig) = gsig.receiver {
                    crate::symbol_resolver::unify_ty(recv_sig, receiver, &mut binds);
                }
                for (ps, at) in mapped.iter().zip(arg_tys) {
                    if let Some(t) = at {
                        crate::symbol_resolver::unify_ty(*ps, *t, &mut binds);
                    }
                }
                let out: Vec<Vec<Ty>> = mapped
                    .iter()
                    .map(|ps| crate::symbol_resolver::function_input_types(*ps, &binds))
                    .collect();
                if out.iter().any(|v| !v.is_empty()) {
                    return Some(out);
                }
            }
        }
        None
    }

    /// Lambda RECEIVER types for an extension call before lambda bodies are typed (a `Recv.() -> R`
    /// lambda parameter binds its implicit `this`).
    fn extension_lambda_receivers(
        &self,
        receiver: Ty,
        name: &str,
        arg_tys: &[Option<Ty>],
    ) -> Option<Vec<Option<Ty>>> {
        let src = self.fed_source();
        let fs = crate::libraries::FunctionSet {
            overloads: self
                .resolver()
                .resolve_symbol(
                    crate::symbol_resolver::SymRecv::Value(receiver),
                    name,
                    &[],
                    &[],
                )
                .map(crate::symbol_resolver::Symbol::overloads)
                .unwrap_or_default()
                .into_iter()
                .filter(crate::libraries::FunctionInfo::is_extension)
                .collect(),
        };
        for allow_must_inline in [false, true] {
            for o in crate::symbol_resolver::ranked_extension_overloads_by_recv(
                &src,
                receiver,
                &fs,
                allow_must_inline,
            ) {
                let Some(gsig) = o.generic_sig.as_ref() else {
                    continue;
                };
                if gsig.params.is_empty() {
                    continue;
                }
                let Some(param_indices) = crate::symbol_resolver::trailing_default_arg_indices(
                    gsig.params.len() - 1,
                    arg_tys,
                ) else {
                    continue;
                };
                let mapped: Vec<(usize, Ty)> = param_indices
                    .iter()
                    .map(|&i| (i, gsig.params[i + 1]))
                    .collect();
                let mut binds = std::collections::HashMap::new();
                crate::symbol_resolver::unify_ty(gsig.params[0], receiver, &mut binds);
                for ((_, ps), at) in mapped.iter().zip(arg_tys) {
                    if let Some(t) = at {
                        crate::symbol_resolver::unify_ty(*ps, *t, &mut binds);
                    }
                }
                let out: Vec<Option<Ty>> = mapped
                    .iter()
                    .map(|(logical_idx, ps)| {
                        if let Some(recv) = o
                            .call_sig
                            .lambda_receivers
                            .get(*logical_idx)
                            .copied()
                            .flatten()
                        {
                            return Some(recv);
                        }
                        if o.call_sig
                            .lambda_receiver_params
                            .get(*logical_idx)
                            .copied()
                            .unwrap_or(false)
                        {
                            return crate::symbol_resolver::function_input_types(*ps, &binds)
                                .first()
                                .copied();
                        }
                        None
                    })
                    .collect();
                if out.iter().any(Option::is_some) {
                    return Some(out);
                }
            }
        }
        None
    }

    // Call-site sugar over the ONE resolution entry point [`SymbolResolver::resolve_symbol`]: each
    // states only the syntax it wrote (a value/type receiver + name + args + a call/read/write/ref
    // form) and reads the discovered [`Symbol`] variant it expects. Resolution itself lives entirely in
    // `resolve_symbol`; these just spare every call site the `resolve_symbol(…).and_then(Symbol::…)`.
    fn resolve_instance_member(
        &self,
        recv: Ty,
        name: &str,
        args: &[Ty],
    ) -> Option<crate::symbol_resolver::ResolvedMember> {
        use crate::symbol_resolver::{SymRecv, Symbol};
        self.resolver()
            .resolve_symbol(SymRecv::Value(recv), name, args, &[])
            .and_then(Symbol::call)
    }
    fn resolve_property_member(
        &self,
        recv: Ty,
        name: &str,
    ) -> Option<crate::symbol_resolver::ResolvedMember> {
        use crate::symbol_resolver::{SymRecv, Symbol};
        self.resolver()
            .resolve_symbol(SymRecv::Value(recv), name, &[], &[])
            .and_then(Symbol::property)
    }
    fn resolve_property_setter(
        &self,
        recv: Ty,
        name: &str,
    ) -> Option<crate::libraries::LibraryCallable> {
        use crate::symbol_resolver::{SymRecv, Symbol};
        self.resolver()
            .resolve_symbol(SymRecv::Value(recv), name, &[], &[])
            .and_then(Symbol::property_setter)
    }
    fn resolve_property_ref(
        &self,
        recv: Ty,
        name: &str,
    ) -> Option<crate::symbol_resolver::BoundPropertyRef> {
        use crate::symbol_resolver::{SymRecv, Symbol};
        self.resolver()
            .resolve_symbol(SymRecv::Value(recv), name, &[], &[])
            .and_then(Symbol::property_ref)
    }
    fn resolve_instance_ref(
        &self,
        recv: Ty,
        name: &str,
    ) -> Option<crate::libraries::LibraryMember> {
        use crate::symbol_resolver::{SymRecv, Symbol};
        self.resolver()
            .resolve_symbol(SymRecv::Value(recv), name, &[], &[])
            .and_then(Symbol::method_ref)
    }
    fn resolve_instance_name(
        &self,
        internal: TypeName,
        name: &str,
        args: &[Ty],
    ) -> Option<crate::libraries::LibraryMember> {
        use crate::symbol_resolver::{SymRecv, Symbol};
        self.resolver()
            .resolve_symbol(SymRecv::TypeName(internal), name, args, &[])
            .and_then(Symbol::instance)
    }
    fn resolve_companion(
        &self,
        internal: &str,
        name: &str,
        args: &[Ty],
    ) -> Option<crate::libraries::LibraryMember> {
        use crate::symbol_resolver::{SymRecv, Symbol};
        self.resolver()
            .resolve_symbol(SymRecv::Type(internal), name, args, &[])
            .and_then(Symbol::companion)
    }
    fn resolve_companion_name(
        &self,
        internal: TypeName,
        name: &str,
        args: &[Ty],
    ) -> Option<crate::libraries::LibraryMember> {
        use crate::symbol_resolver::{SymRecv, Symbol};
        self.resolver()
            .resolve_symbol(SymRecv::TypeName(internal), name, args, &[])
            .and_then(Symbol::companion)
    }
    fn resolve_constructor_name(
        &self,
        internal: TypeName,
        args: &[Ty],
    ) -> Option<crate::libraries::LibraryMember> {
        use crate::symbol_resolver::{SymRecv, Symbol};
        self.resolver()
            .resolve_symbol(SymRecv::TypeName(internal), "", args, &[])
            .and_then(Symbol::constructor)
    }
    fn resolve_synthetic_constructor_name(
        &self,
        internal: TypeName,
        args: &[Ty],
    ) -> Option<crate::symbol_resolver::SyntheticCtorCall> {
        use crate::symbol_resolver::{SymRecv, Symbol};
        self.resolver()
            .resolve_symbol(SymRecv::TypeName(internal), "", args, &[])
            .and_then(Symbol::synthetic_constructor)
    }
    /// Whether the current module declares a top-level function `name` (shadow-precedence test) — asked
    /// through the module source rather than touching `syms.funs` directly.
    fn module_declares(&self, name: &str) -> bool {
        crate::module_symbols::ModuleSymbols::new(self.syms).declares_top_level(name)
    }
    /// True when `e` is a call to the `kotlin.contracts.contract { … }` intrinsic — the erased
    /// contract-declaration block. The callee name `contract` is confirmed to resolve to a
    /// top-level function in `kotlin/contracts` through the symbol resolver, so a user function that
    /// happens to be named `contract` is not mistaken for it.
    fn is_contract_call(&self, e: ExprId) -> bool {
        let Expr::Call { callee, .. } = self.file.expr(e) else {
            return false;
        };
        let Expr::Name(name) = self.file.expr(*callee) else {
            return false;
        };
        if name != "contract" {
            return false;
        }
        // A user-declared top-level `fun contract(…)` in this module shadows the stdlib intrinsic —
        // then the call is a real function and must NOT be erased.
        if self.module_declares("contract") {
            return false;
        }
        crate::libraries::FunctionSet {
            overloads: self
                .resolver()
                .resolve_symbol(
                    crate::symbol_resolver::SymRecv::TopLevel,
                    "contract",
                    &[],
                    &[],
                )
                .map(crate::symbol_resolver::Symbol::overloads)
                .unwrap_or_default(),
        }
        .overloads
        .iter()
        .any(|o| o.callable.owner.starts_with("kotlin/contracts"))
    }

    fn is_resolved_stdlib_precondition_call(&self, call: ExprId, name: &str) -> bool {
        matches!(
            self.resolved_calls.get(&call),
            Some(ResolvedCall::TopLevel(c))
                if c.name == name && c.owner.starts_with("kotlin/PreconditionsKt")
        )
    }

    /// Resolve an operator/method call `receiver.name(args)` — a user-class MEMBER, a same-module
    /// EXTENSION, or a library member — checking each argument type and returning the selected target.
    /// `None` when no such method of matching arity exists (the caller then declines). Used by the
    /// reference-range `in` desugaring (`rangeTo` then `contains`), which records the returned calls only
    /// after the whole desugaring is valid.
    fn operator_call_ret(
        &mut self,
        receiver: Ty,
        name: &str,
        arg_tys: &[Ty],
        arg_exprs: &[ExprId],
    ) -> Option<(Ty, ResolvedCall)> {
        if let Some(internal) = receiver.obj_internal() {
            if let Some((owner, sig)) = self.syms.method_of_with_owner_name(internal, name) {
                if sig.params.len() == arg_tys.len() {
                    self.expect_call_args(&sig.params, false, arg_exprs, arg_tys);
                    let interface = self
                        .syms
                        .class_by_type_name(owner)
                        .is_some_and(|c| c.is_interface);
                    return Some((
                        sig.ret,
                        ResolvedCall::ModuleMember {
                            owner,
                            name: name.to_string(),
                            params: sig.params.clone(),
                            ret: sig.ret,
                            interface,
                        },
                    ));
                }
            }
        }
        if let Some(sig) = self
            .syms
            .ext_fun_overloads(receiver, name)
            .iter()
            .find(|s| !s.vararg && s.params.len() == arg_tys.len())
            .cloned()
        {
            self.expect_call_args(&sig.params, false, arg_exprs, arg_tys);
            return Some((
                sig.ret,
                ResolvedCall::ModuleExtension {
                    receiver,
                    name: name.to_string(),
                    params: sig.params.clone(),
                    ret: sig.ret,
                },
            ));
        }
        self.resolve_instance_member(receiver, name, arg_tys)
            .map(|m| (m.ret, ResolvedCall::Member(m)))
    }
    /// Resolve each context-parameter type to an in-scope source that satisfies it: the enclosing
    /// implicit receiver (the sentinel `"this"`, e.g. a `with` block's receiver) if its type is a
    /// subtype, else an in-scope local / enclosing context parameter of a matching type (innermost
    /// first). `None` if any context parameter has no satisfying source (the call then falls back to the
    /// normal arity path and skips). The returned names are what the lowerer loads and prepends.
    fn resolve_context_args(&self, ctx_types: &[Ty]) -> Option<Vec<String>> {
        let matches = |have: Ty, want: Ty| -> bool {
            if have == want {
                return true;
            }
            match (have.obj_internal(), want.obj_internal()) {
                (Some(h), Some(w)) => self.obj_name_is_subtype(h, w),
                _ => false,
            }
        };
        let mut out = Vec::with_capacity(ctx_types.len());
        for &want in ctx_types {
            if let Some(this_t) = self.this_ty {
                if matches(this_t, want) {
                    out.push("this".to_string());
                    continue;
                }
            }
            // Innermost scope first, matching Kotlin's context resolution preferring the nearest binding.
            let local = self.scopes.iter().rev().find_map(|s| {
                s.iter()
                    .find_map(|(n, l)| matches(l.ty, want).then(|| n.clone()))
            });
            if let Some(name) = local {
                out.push(name);
            } else {
                return None;
            }
        }
        // Two context parameters resolving to the SAME source is ambiguous (kotlinc rejects duplicate
        // context types); decline rather than pass one value into two parameters.
        for i in 0..out.len() {
            if out[i + 1..].contains(&out[i]) {
                return None;
            }
        }
        Some(out)
    }
    fn module_top_level_return(
        &self,
        call: ExprId,
        selected: &crate::libraries::FunctionInfo,
        arg_tys: &[Ty],
    ) -> Ty {
        let mut ret_ty = selected.callable.ret;
        if let Some(&inferred) = selected
            .source_key
            .and_then(|source_key| self.inferred_fun_rets.get(&source_key))
        {
            ret_ty = inferred;
        }
        if let Some(f) = selected
            .source_key
            .filter(|(file, _)| *file == self.file_index)
            .and_then(|(_, decl)| match self.file.decl(DeclId(decl)) {
                Decl::Fun(f) => Some(f),
                _ => None,
            })
        {
            if let Some(r) = self.user_generic_return(f, arg_tys) {
                ret_ty = r;
            }
            if let Some(r) = self.explicit_generic_return(call, f) {
                ret_ty = r;
            }
        }
        ret_ty
    }

    fn resolve_context_module_top_level(
        &self,
        name: &str,
        arg_tys: &[Ty],
    ) -> Option<(crate::libraries::FunctionInfo, Vec<String>)> {
        let mut best: Option<(usize, usize, crate::libraries::FunctionInfo, Vec<String>)> = None;
        for (idx, fi) in self
            .module
            .top_level_overloads_in_scope(name, &self.fn_scope)
            .into_iter()
            .enumerate()
        {
            let ctx_count = fi.context_count;
            if ctx_count == 0 || ctx_count > fi.callable.params.len() {
                continue;
            }
            let value_params = &fi.callable.params[ctx_count..];
            if arg_tys.len() > value_params.len() {
                continue;
            }
            let omitted_ok = (arg_tys.len()..value_params.len())
                .all(|i| fi.call_sig.param_has_default(ctx_count + i));
            if !omitted_ok {
                continue;
            }
            let Some(sources) = self.resolve_context_args(&fi.callable.params[..ctx_count]) else {
                continue;
            };
            let mut score = 0;
            for (&p, &a) in value_params.iter().zip(arg_tys) {
                if !arg_assignable_simple(p, a) {
                    score = 0;
                    break;
                }
                score += if p == a { 2 } else { 1 };
            }
            if score == 0 && !arg_tys.is_empty() {
                continue;
            }
            if best.as_ref().is_none_or(|(best_score, best_idx, ..)| {
                score > *best_score || (score == *best_score && idx < *best_idx)
            }) {
                best = Some((score, idx, fi, sources));
            }
        }
        best.map(|(_, _, fi, sources)| (fi, sources))
    }

    fn set(&mut self, e: ExprId, t: Ty) -> Ty {
        self.expr_types[e.0 as usize] = t;
        t
    }
    fn span(&self, e: ExprId) -> Span {
        self.file.expr_spans[e.0 as usize]
    }

    fn push_scope(&mut self) {
        // A flow-narrowing (`Local::narrowed`) is only sound along a straight-line statement sequence
        // in one scope; crossing INTO a nested scope (a branch/loop/block/lambda body) or back OUT of
        // one can invalidate it, so drop all narrowings at every scope boundary.
        self.clear_local_narrows();
        self.scopes.push(HashMap::new());
    }
    fn pop_scope(&mut self) {
        self.scopes.pop();
        self.clear_local_narrows();
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
    fn mark_local_function_ref(&mut self, e: ExprId, stmt_id: StmtId) {
        self.expr_lowers
            .insert(e, ExprLowering::LocalFunction { stmt_id });
    }
    fn mark_local_function_call(
        &mut self,
        call: ExprId,
        stmt_id: StmtId,
        sig: Signature,
        provided_arg_count: usize,
        context_args: Vec<String>,
    ) {
        self.resolved_calls.insert(
            call,
            ResolvedCall::LocalFunction(Box::new(ResolvedLocalFunctionCall {
                stmt_id,
                sig,
                provided_arg_count,
                context_args,
            })),
        );
    }
    fn mark_module_top_level_call(
        &mut self,
        call: ExprId,
        name: &str,
        selected: &crate::libraries::FunctionInfo,
        ret: Ty,
        context_args: Vec<String>,
    ) {
        let source_file = selected.source_key.map(|(file, _)| file);
        let source_decl = selected.source_key.map(|(_, decl)| DeclId(decl));
        let source_fun = selected
            .source_key
            .filter(|(file, _)| *file == self.file_index)
            .map(|(_, decl)| DeclId(decl))
            .and_then(|decl| match self.file.decl(decl) {
                Decl::Fun(f) => Some(f),
                _ => None,
            });
        let param_meta = source_fun
            .map(|f| {
                f.params
                    .iter()
                    .map(|p| (p.name.clone(), p.default))
                    .collect()
            })
            .unwrap_or_default();
        let param_default_values = selected
            .source_key
            .and_then(|(file, decl)| {
                self.syms.funs.values().find_map(|sigs| {
                    sigs.iter()
                        .find(|s| {
                            s.source_file == Some(file) && s.source_decl == Some(DeclId(decl))
                        })
                        .map(|s| s.param_default_values.clone())
                })
            })
            .unwrap_or_default();
        let ret_is_tparam = source_fun.is_some_and(|f| {
            f.ret.as_ref().is_some_and(|r| {
                r.targs.is_empty()
                    && r.fun_params.is_empty()
                    && f.type_params.iter().any(|tp| tp == &r.name)
            })
        });
        self.resolved_calls.insert(
            call,
            ResolvedCall::ModuleTopLevel(Box::new(ResolvedModuleTopLevelCall {
                name: name.to_string(),
                params: selected.callable.params.clone(),
                ret,
                call_sig: selected.call_sig.clone(),
                inline: selected.flags.inline,
                suspend: selected.flags.suspend,
                context_args,
                source_file,
                source_decl,
                param_meta,
                param_default_values,
                ret_is_tparam,
            })),
        );
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
            // A property/function REFERENCE value (`obj::p` → `KProperty0`, `Type::p` → `KProperty1`,
            // `::foo` → `KFunctionN`) IS a `FunctionN` (invoke arity = the trailing digit), so invoking
            // it goes through `Function{N}.invoke` — an `invokeinterface`, not the `operator fun invoke`
            // member path (which would emit an `invokevirtual` on the interface method → ICCE). Args and
            // result are the erased `Object` the reflection `invoke` uses.
            Ty::Obj(internal, _) if callable_reference_invoke_arity(internal).is_some() => {
                let arity = callable_reference_invoke_arity(internal).unwrap();
                let obj = Ty::obj_name(crate::types::wk::any());
                (
                    vec![obj; arity],
                    obj,
                    InvokeKind::Function {
                        ret: obj,
                        suspend: false,
                    },
                )
            }
            _ => {
                // A member `operator fun invoke`: source/user classes are emitted by IR method id later,
                // while classpath/cross-file members carry the checker-selected callable into lowering.
                if let Some(sig) = receiver_ty.obj_internal().and_then(|internal| {
                    self.syms.method_of_name(internal, CALLABLE_INVOKE_OPERATOR)
                }) {
                    (
                        sig.params,
                        sig.ret,
                        InvokeKind::Operator {
                            receiver_ty,
                            member: None,
                        },
                    )
                } else if let Some(m) =
                    self.resolve_instance_member(receiver_ty, CALLABLE_INVOKE_OPERATOR, arg_tys)
                {
                    (
                        m.member.params.clone(),
                        m.ret,
                        InvokeKind::Operator {
                            receiver_ty,
                            member: Some(Box::new(m)),
                        },
                    )
                } else {
                    let fi = self
                        .resolver()
                        .resolve_symbol(
                            crate::symbol_resolver::SymRecv::Value(receiver_ty),
                            CALLABLE_INVOKE_OPERATOR,
                            &[],
                            &[],
                        )
                        .map(crate::symbol_resolver::Symbol::overloads)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|o| o.is_extension() && o.receiver_rank == 0)
                        .find(|o| {
                            o.extension_value_params().len() == arg_tys.len()
                                // A `suspend operator fun …invoke` would need continuation threading the
                                // ExtensionOperator lowering doesn't do — leave it unresolved (skip).
                                && !o.flags.suspend
                        })?;
                    let params = fi.extension_value_params().to_vec();
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
    fn local_function_use_count(&self) -> usize {
        let refs = self
            .expr_lowers
            .values()
            .filter(|v| matches!(v, ExprLowering::LocalFunction { .. }))
            .count();
        let calls = self
            .resolved_calls
            .values()
            .filter(|v| matches!(v, ResolvedCall::LocalFunction(_)))
            .count();
        refs + calls
    }
    fn declare(&mut self, name: &str, ty: Ty, is_var: bool) {
        self.scopes.last_mut().unwrap().insert(
            name.to_string(),
            Local {
                ty,
                is_var,
                narrowed: None,
            },
        );
    }
    fn lookup(&self, name: &str) -> Option<&Local> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }
    /// Record (or clear, with `None`) the flow-narrowed read type of the innermost `name` binding.
    fn set_local_narrow(&mut self, name: &str, narrowed: Option<Ty>) {
        if narrowed.is_some() {
            self.narrow_active = true;
        }
        for scope in self.scopes.iter_mut().rev() {
            if let Some(l) = scope.get_mut(name) {
                l.narrowed = narrowed;
                return;
            }
        }
    }
    /// Drop ALL flow-narrowings. Called at scope boundaries (branch/loop/block/lambda): a narrowing
    /// established on a straight-line path is not guaranteed to hold once control can branch, loop, or
    /// defer into a closure, so it is conservatively discarded (sound; only linear narrowings survive).
    fn clear_local_narrows(&mut self) {
        if !self.narrow_active {
            return;
        }
        for scope in self.scopes.iter_mut() {
            for l in scope.values_mut() {
                l.narrowed = None;
            }
        }
        self.narrow_active = false;
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
    fn lexical_value_declares(&self, name: &str) -> bool {
        self.lookup(name).is_some() || self.lookup_local_fun(name).is_some()
    }
    fn value_root_shadows_classifier(&self, name: &str) -> bool {
        self.lexical_value_declares(name)
            || self.syms.props.contains_key(name)
            || self.syms.prop_facades.contains_key(name)
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

    /// Flatten a pure `Name`/`Member` chain into its dotted path (`A.E` for `Member{Name("A"),"E"}`),
    /// or `None` if any link is not a plain name access. Used to recover the hoisted key of a nested
    /// type referenced through its enclosing type name (`Outer.Nested`).
    fn dotted_full_path(&self, e: ExprId) -> Option<String> {
        match self.file.expr(e) {
            Expr::Name(n) => Some(n.clone()),
            Expr::Member { receiver, name } => {
                Some(format!("{}.{}", self.dotted_full_path(*receiver)?, name))
            }
            _ => None,
        }
    }

    fn qualified_nested_ctor_internal_name(
        &self,
        receiver: ExprId,
        name: &str,
    ) -> Option<TypeName> {
        // The receiver's leftmost segment must be a TYPE/PACKAGE, not a value in scope.
        let root = self.dotted_root(receiver)?;
        if self.value_root_shadows_classifier(&root) {
            return None;
        }
        match self.file.expr(receiver) {
            // `Outer.Nested(…)` — a nested type under an in-scope/imported outer type.
            Expr::Name(outer) => self.resolve_qualified_nested_name(&format!("{outer}.{name}")),
            // `a.b.Ctx(…)` — a FULLY-QUALIFIED constructor via a package PATH: the receiver `a.b` is a
            // package, `Ctx` a top-level class of it (`a/b/Ctx`), verified on the classpath.
            Expr::Member { .. } => {
                let internal = format!("{}/{name}", qualified_path(self.file, receiver)?);
                let internal = type_name(&internal);
                self.resolved_type_name(internal).map(|_| internal)
            }
            _ => None,
        }
    }

    fn resolve_qualified_nested_name(&self, name: &str) -> Option<TypeName> {
        // A nested type under a resolvable outer type FIRST (`Subject.User` → `lib/Subject$User`) — an
        // in-scope type name shadows a package path, as kotlinc resolves it.
        if let Some((outer, rest)) = name.split_once('.') {
            let base = self
                .syms
                .classes
                .get(outer)
                .map(ClassSig::internal_name)
                .or_else(|| self.imported_type_name(outer))
                .or_else(|| {
                    self.syms
                        .class_names
                        .get(outer)
                        .filter(|i| !i.starts_with("__ty/"))
                });
            if let Some(base) = base {
                let candidate = type_name(&format!("{}${}", base.render(), rest.replace('.', "$")));
                if self.resolved_type_name(candidate).is_some() {
                    return Some(candidate);
                }
            }
        }
        // A fully-qualified PACKAGE path (`lib.Thing` → `lib/Thing`): the qualifier is a package, not a
        // type. Verified via `resolve_type`. Handles both a type reference (`x: lib.Thing?`) and a
        // qualified constructor call (`lib.Thing(5)`). `nested_internal` also recovers a DEEP FQN whose
        // tail names a NESTED type (`a.b.Outer.Inner` → `a/b/Outer$Inner`), which the flat slash form misses.
        let fq = name.replace('.', "/");
        self.nested_internal_name(&fq)
    }

    fn classpath_object_value(&self, name: &str) -> Option<TypeName> {
        let internal = self.imported_type_name(name)?;
        if self.resolved_type_name(internal)?.is_object() {
            Some(internal)
        } else {
            None
        }
    }

    /// Resolve a bare type `name` through this file's imports to an internal name that actually exists on
    /// The parameter types of the base constructor that `: Base(args)` targets — the UNIQUE constructor
    /// (module or classpath, resolved through the symbol source) to which every argument is assignable.
    /// `base_internal` is the ALREADY-RESOLVED base class internal name.
    /// `None` if the base type is unresolved, has no matching constructor, or the match is ambiguous
    /// (then the lowerer bails rather than emitting a `super(...)` to a guessed overload).
    fn resolve_super_ctor_params_name(
        &self,
        base_internal: TypeName,
        arg_tys: &[Ty],
    ) -> Option<Vec<Ty>> {
        let lt = self.resolved_type_name(base_internal)?;
        // EXACT (nullability-insensitive) type match — a loose reference→reference assignability can't
        // tell `RuntimeException(String)` from `RuntimeException(Throwable)` for a `String` argument.
        let mut matches = lt.constructors.iter().filter(|ctor| {
            ctor.params.len() == arg_tys.len()
                && ctor
                    .params
                    .iter()
                    .zip(arg_tys)
                    .all(|(&p, &a)| p.non_null() == a.non_null())
        });
        let first = matches.next()?;
        if matches.next().is_some() {
            return None; // ambiguous — don't guess an overload
        }
        Some(first.params.clone())
    }

    /// the classpath — the SAME kotlinc-conforming resolver the signature pass uses
    /// ([`resolve_name_against_imports`]): explicit import first, then the implicit FQN candidates by
    /// precedence level, with same-level ambiguity left unresolved. Existence is verified via
    /// `resolve_type`.
    fn imported_type_internal(&self, name: &str) -> Option<String> {
        self.imported_type_name(name).map(TypeName::render)
    }

    fn imported_type_name(&self, name: &str) -> Option<TypeName> {
        let source = self.fed_source();
        resolve_name_against_imports_name(name, &self.imports, &self.import_levels, &source)
    }

    /// Resolve a dotted import flattened to slashes (`import lib.Scope.Ws` → `lib/Scope/Ws`) to the
    /// internal name that actually EXISTS on the classpath, treating trailing path segments as NESTED
    /// classes (`lib/Scope$Ws`). A nested-type import can't be told apart from a package path
    /// syntactically, so convert `/` → `$` from the RIGHT until `resolve_type` finds the class. Returns
    /// the input unchanged when it already resolves (the common package-qualified case).
    fn nested_internal(&self, internal: &str) -> Option<String> {
        self.nested_internal_name(internal).map(TypeName::render)
    }

    fn nested_internal_name(&self, internal: &str) -> Option<TypeName> {
        let source = self.fed_source();
        resolve_nested_internal_name(internal, &source)
    }

    /// The internal name of an ENCLOSING class's nested type named `name` (`Inner` inside `Outer` →
    /// `Outer$Inner`), walking outward through the current `this_ty`'s `$`-separated enclosing chain
    /// (nearest first). `None` if no enclosing class declares such a nested type — the scope in which a
    /// nested type SHADOWS a same-named top-level (Kotlin nested-type scoping).
    fn enclosing_nested_type(&self, name: &str) -> Option<String> {
        self.enclosing_nested_type_name(name).map(TypeName::render)
    }

    fn enclosing_nested_type_name(&self, name: &str) -> Option<TypeName> {
        let Some(Ty::Obj(outer, _)) = self.this_ty else {
            return None;
        };
        let outer = outer.render();
        let mut prefix: &str = &outer;
        loop {
            let cand = type_name(&format!("{prefix}${name}"));
            if self.syms.class_by_type_name(cand).is_some() {
                return Some(cand);
            }
            match prefix.rsplit_once('$') {
                Some((p, _)) => prefix = p,
                None => return None,
            }
        }
    }

    /// If a bare type name `n` denotes a reference type usable as an unbound class literal `n::class`,
    /// its `Ty`. Checks built-ins, user classes, enclosing nested types, and imports, but not the global
    /// simple-name index because it collides with built-in names. Primitive or unknown names return `None`.
    fn class_literal_unbound_ty(&self, n: &str) -> Option<Ty> {
        if self.tparams.contains(n) {
            return None;
        }
        if self.lookup(n).is_some() {
            return None;
        }
        let ty = Ty::from_name(n)
            .or_else(|| {
                self.syms
                    .classes
                    .get(n)
                    .map(|cs| Ty::obj_name(cs.internal_name()))
            })
            .or_else(|| self.enclosing_nested_type_name(n).map(Ty::obj_name))
            .or_else(|| self.imported_type_name(n).map(Ty::obj_name))?;
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

    fn obj_with_targs_name(&mut self, internal: TypeName, r: &TypeRef) -> Ty {
        if r.targs.is_empty() {
            Ty::obj_name(internal)
        } else {
            let args: Vec<Ty> = r.targs.iter().map(|a| self.resolve_ty(a)).collect();
            Ty::obj_args_name(internal, &args)
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
                    } else if e.jvm_boxed_ref().is_some() {
                        // A boxed primitive `Array<Int>` = `Integer[]` — the SAME logical form as
                        // `arrayOf(1)`/`Array(n){…}` (`Obj("kotlin/Array", [Int])`, element read unboxed).
                        Ty::obj_args("kotlin/Array", &[e])
                    } else {
                        Ty::Error
                    }
                }
                None => Ty::Error,
            }
        } else if self.tparams.contains(&r.name) {
            self.tparams.erase(&r.name) // erased generic type parameter (primitive if `<T: Int>`)
        } else if let Some(internal) = self.enclosing_nested_type_name(&r.name) {
            // Kotlin nested-type scoping: an UNQUALIFIED name that names one of the ENCLOSING class's own
            // nested types SHADOWS a same-named top-level/imported type — so resolve the nested form FIRST
            // (before the global `syms.classes`/import lookups). Drives both this type position AND, via
            // `info.ty`, the construction-expression lowering, keeping them consistent.
            self.obj_with_targs_name(internal, r)
        } else if let Some(cs) = self.syms.classes.get(&r.name) {
            self.obj_with_targs_name(cs.internal_name(), r)
        } else if let Some(internal) = self.syms.class_names.get(&r.name) {
            // Built-in mapped types (`Number`, `Comparable`, `List`, …), classpath classes, and
            // type aliases — the *same* map emit resolves against, so the checker and codegen agree
            // (otherwise a leniently-`Error` type here becomes a real `Obj` in emit → VerifyError).
            // `"__ty/<Prim>"` encodes an alias to a primitive/builtin.
            match internal.strip_prefix("__ty/") {
                Some(prim) => Ty::from_name(&prim).unwrap_or(Ty::Error),
                None => self.obj_with_targs_name(internal, r),
            }
        } else if let Some(internal) = self.imported_type_name(&r.name) {
            // An explicit/wildcard import resolves a name whose simple form is ABSENT from the global
            // index — either never registered or pruned because it's ambiguous across the whole classpath
            // (`Continuation` collides with `jdk/internal/vm/Continuation`). The import names the package.
            self.obj_with_targs_name(internal, r)
        } else if let Some(internal) = self.resolve_qualified_nested_name(&r.name) {
            // A dotted CLASSPATH nested type (`Subject.User`, `SlugValidation.Ok`) → `Outer$Nested`.
            self.obj_with_targs_name(internal, r)
        } else if let Some(internal) = {
            // An UNQUALIFIED reference to a sibling nested type within the enclosing class body (`Inner`
            // in `class Outer { class Inner }`) → `Outer$Inner` (Kotlin nested-type scoping). Reached only
            // when nothing else resolved, in a checker-only position (`val v: Inner`, `x as Inner`);
            // member SIGNATURE positions are covered by the collect_signatures class-scope extension.
            if let Some(Ty::Obj(outer, _)) = self.this_ty {
                let nested = type_name(&format!("{outer}${}", r.name));
                self.syms
                    .class_by_type_name(nested)
                    .map(ClassSig::internal_name)
            } else {
                None
            }
        } {
            self.obj_with_targs_name(internal, r)
        } else {
            Ty::Error
        };
        if r.nullable && !base.is_reference() && base != Ty::Error {
            if let Some(nullable) = base.nullable_non_ref() {
                return nullable;
            }
            self.diags.error(
                r.span,
                format!("nullable primitive type '{}?' is not supported", r.name),
            );
            return Ty::Error;
        }
        // A NULLABLE value/inline class reference (`Result<T>?`) keeps its `?`, even though ordinary
        // reference nullability is dropped: a value class has a distinct boxed-vs-unboxed representation
        // (like a primitive), and downstream (the shared-cell holder, the box/unbox pass) distinguishes
        // the boxed nullable form ONLY by the `?` (see `ref_elem_ir` / `nullable_is_boxed`). Without this a
        // `var res: Result<T>?` would type as non-null `Result` → the pass reads it unboxed → `res!!` skips
        // the `unbox-impl` a `getOrThrow()`/member access needs.
        if r.nullable && !base.is_nullable() {
            if let Ty::Obj(internal, _) = base {
                if self.resolver().is_value_name(internal) {
                    return Ty::nullable(base);
                }
            }
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
                erased_type_key(Ty::obj(&cs.internal()))
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
        matches!(t, Ty::Obj(n, _) if self.syms.class_by_type_name(n).is_some_and(|c| c.value_field.is_some()))
    }

    /// Whether the file-declared class `internal` declares method `name` as abstract (`fun f()` /
    /// `abstract override fun f()`). Such a declaration is not a concrete `super.f()` target; resolution
    /// must continue to a matching interface default when one exists.
    fn class_method_is_abstract_name(&self, internal: TypeName, name: &str) -> bool {
        self.file.decls.iter().any(|&d| {
            matches!(self.file.decl(d), Decl::Class(c) if !c.is_interface()
                && internal.matches(&class_internal(self.file, &c.name))
                && c.methods.iter().any(|m| m.name == name && matches!(m.body, FunBody::None)))
        })
    }

    /// Reject classes whose *effective* implementation of a supertype method has the same erased
    /// parameters but a different return descriptor (covariant or generic return) — including
    /// *fake overrides*, where the implementation is inherited from a base class while the differing
    /// signature comes from an interface. The JVM resolves such a call via the supertype's descriptor
    /// and would need a synthetic bridge method, which krusty does not emit — so the file is cleanly
    /// skipped rather than throwing `AbstractMethodError` at runtime.
    fn check_no_bridge_needed(&mut self, internal: TypeName, span: Span) {
        let supers = self.syms.supertype_methods_name(internal);
        let obj = Ty::obj("kotlin/Any");
        for (name, ssig) in &supers {
            // Pair the super method with the overload that could actually OVERRIDE it (same arity,
            // params equal or erasure-compatible) — a same-name SIBLING overload (`f(Int)` next to
            // an inherited `f()`) is not an override and needs no bridge.
            let Some(impl_sig) = self
                .syms
                .override_impl_of_name(internal, name, &ssig.params)
            else {
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

    /// Report (and thereby skip the file for) functions whose signatures collide: an EXACT
    /// erased-signature duplicate is always a JVM `ClassFormatError`. Same-name functions with
    /// DIFFERENT erased signatures are legal overloads — top-level AND class members — dispatched
    /// at the call site by argument types ([`pick_overload`] / `ClassSig::method_matching`, with
    /// the member overload lists flowing through `module_symbols` into the `SymbolResolver`).
    fn check_no_erased_clash(&mut self, funs: &[&FunDecl]) {
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
        // (`subclass_names_of`); a CLASSPATH sealed class reads its `@Metadata` `sealedSubclassFqName`
        // (`sealed_subclasses`), so `when (d) { is D.A -> …; is D.B -> … }` over a classpath sealed `D`
        // is proven exhaustive (an expression) the same way a same-module one is.
        let subs: Vec<TypeName> = match self.syms.class_by_type_name(internal) {
            Some(cs) if cs.is_sealed => self.syms.subclass_names_of(internal),
            Some(_) => return false,
            None => self
                .resolved_type_name(internal)
                .map(|t| t.sealed_subclasses.iter_ids().collect())
                .unwrap_or_default(),
        };
        if subs.is_empty() {
            return false;
        }
        let mut covered: std::collections::HashSet<TypeName> = std::collections::HashSet::new();
        for arm in arms {
            for &c in &arm.conditions {
                match self.file.expr(c) {
                    // `is Sub` — type-test arm.
                    Expr::Is {
                        ty, negated: false, ..
                    } => {
                        if let Ty::Obj(n, _) = self.resolve_ty_no_diag(ty) {
                            covered.insert(n);
                        }
                    }
                    // `Sub ->` — value arm naming a singleton object subclass (`object A : S`); a bare
                    // name resolving to a known class whose internal is one of the sealed subclasses.
                    Expr::Name(n) => {
                        if let Some(ci) = self.syms.classes.get(n) {
                            let internal = ci.internal_name();
                            if subs.contains(&internal) {
                                covered.insert(internal);
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
                .map_or(false, |c| c.internal_name() == internal)
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
                            .map_or(false, |c| c.internal_name() == internal)
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
        // result type — no hardcoded intrinsic name list).
        if self.expr_types[e.0 as usize] == Ty::Nothing {
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
    fn catch_internal_name(&self, name: &str) -> Option<TypeName> {
        self.imports
            .get(name)
            .map(|internal| type_name(internal))
            .or_else(|| self.syms.classes.get(name).map(ClassSig::internal_name))
            // Exception types resolve from the classpath: stdlib `TypeAliasesKt` aliases
            // (`Exception`, `RuntimeException`, …) and the ported `JavaToKotlinClassMap`
            // built-ins (`Throwable`) are both folded into `class_names`.
            .or_else(|| self.syms.class_names.get(name))
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
            Ty::obj(&cs.internal())
        } else if let Some(internal) = self.syms.class_names.get(&r.name) {
            // The block-resolved name→internal map (built-in mapped types, classpath classes, and
            // typealiases resolved to their TARGET) — the same map `resolve_ty` narrows against, so a
            // smart-cast's checkcast uses the real internal (`IllegalStateException` → `java/lang/…`, not
            // the classless alias name). `"__ty/<prim>"` is an alias to a primitive, not modeled here.
            match internal.strip_prefix("__ty/") {
                Some(_) => Ty::Error,
                None => Ty::obj_name(internal),
            }
        } else if let Some(internal) = self
            .imported_type_name(&r.name)
            .or_else(|| self.resolve_qualified_nested_name(&r.name))
        {
            // A CLASSPATH type (imported `is Ok`, or a qualified nested `is V.Ok`) — resolved the same way
            // `resolve_ty` resolves it, so an `is`/`as` smart-cast to a classpath sealed/open subclass
            // narrows (`val v: V; if (v is V.Ok) v.v`). Without this the type erased to `Ty::Error`, the
            // narrowing was dropped, and every member access on the smart-cast value failed ("member … on
            // <parent>").
            Ty::obj_name(internal)
        } else if let Some(Ty::Obj(outer, _)) = self.this_ty {
            // A sibling nested type unqualified within the enclosing class body (`is Inner` in
            // `class Outer { class Inner }`) → `Outer$Inner`, so a nested-type `is`/`as` smart-cast
            // narrows. Mirrors the same fallback in `resolve_ty`.
            let nested = type_name(&format!("{outer}${}", r.name));
            self.syms
                .class_by_type_name(nested)
                .map(|s| Ty::obj_name(s.internal_name()))
                .unwrap_or(Ty::Error)
        } else {
            Ty::Error
        }
    }

    /// If `cond` is `x is T` (or `x !is T` when `for_else`) and `x` is a stable local/parameter and
    /// `T` a non-nullable known reference type, return the smart-cast binding `(x, T)`.
    /// The narrowed implicit-receiver type established by `if (this is B)` (`this !is B` in the
    /// else-branch): `this` is a subtype `B` of the declared receiver. `this` is immutable, so the
    /// narrowing is sound across nested scopes. Only a KNOWN reference subtype narrows (a primitive /
    /// nullable / unresolved target is left un-narrowed, so the file skips rather than miscompiles).
    fn this_is_narrowing(&self, cond: ExprId, for_else: bool) -> Option<Ty> {
        let Expr::Is {
            operand,
            ty,
            negated,
        } = self.file.expr(cond).clone()
        else {
            return None;
        };
        if negated != for_else {
            return None;
        }
        if !matches!(self.file.expr(operand), Expr::Name(n) if n == "this") {
            return None;
        }
        if ty.nullable {
            return None;
        }
        let tt = self.resolve_ty_no_diag(&ty);
        tt.is_reference().then_some(tt)
    }

    fn smartcast_binding(&self, cond: ExprId, for_else: bool) -> Option<(String, Ty)> {
        // `x != null` (then-branch) / `x == null` (else-branch) narrows a nullable-primitive wrapper to
        // its unboxed primitive — the only null-narrowing krusty needs (a nullable reference is already
        // its non-null type here). Only a stable `val`/parameter narrows soundly.
        if let Expr::Binary { op, lhs, rhs } = self.file.expr(cond).clone() {
            if matches!(op, BinOp::Ne | BinOp::Eq) {
                let narrows_then = matches!(op, BinOp::Ne); // `!= null` narrows in the then-branch
                if narrows_then != for_else {
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

    fn check_fun(&mut self, f: &FunDecl, source_decl: Option<DeclId>) {
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
        self.fn_closure_reassigned.clear();
        if let FunBody::Expr(b) | FunBody::Block(b) = &f.body {
            collect_all_reassigned(self.file, *b, &mut self.fn_reassigned);
            collect_closure_reassigned(self.file, *b, &mut self.fn_closure_reassigned);
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
            // Pick THIS declaration's overload out of the receiver+name overload set by matching its
            // parameter list (an extension may be overloaded by arity — `fun R.f()` and `fun R.f(x)`).
            let want: Vec<Ty> = f
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
            self.ret_ty = self
                .syms
                .ext_fun_overloads(recv_ty, &f.name)
                .iter()
                .find(|s| s.params == want)
                .map(|s| s.ret)
                .or_else(|| f.ret.as_ref().map(|r| self.resolve_ty(r)))
                .unwrap_or(Ty::Unit);
        } else {
            // Use this declaration's own collected return type; companion methods fall back to the
            // declared return type because they are not stored in `funs`.
            let want: Vec<ErasedTypeKey> = f
                .params
                .iter()
                .map(|p| erased_type_key(self.resolve_ty(&p.ty)))
                .collect();
            let own_ret = self.syms.fun_ret_by_erased_params(&f.name, &want);
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
                self.check_default_arg(&p.ty, dx, pty);
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
                        self.inferred_ext_fun_rets.insert(
                            (recv_ty.erased_recv(), f.name.clone(), ptys.clone()),
                            inferred,
                        );
                    } else {
                        if let Some(decl) = source_decl {
                            self.inferred_fun_rets
                                .insert((self.file_index, decl.0), inferred);
                        }
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
        self.fn_closure_reassigned.clear();
        if let FunBody::Expr(b) | FunBody::Block(b) = &f.body {
            collect_all_reassigned(self.file, *b, &mut self.fn_reassigned);
            collect_closure_reassigned(self.file, *b, &mut self.fn_closure_reassigned);
        }
        let resolve = class_internal_resolver(self.syms);
        let added = self
            .tparams
            .insert_decl_with(&f.type_params, &f.type_param_bounds, &resolve);
        let reified_added: Vec<String> = f
            .reified_type_params
            .iter()
            .filter(|t| self.reified_tparams.insert((*t).clone()))
            .cloned()
            .collect();
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
                        .class_by_type_name(internal)
                        .and_then(|c| c.method(&f.name))
                    {
                        return sig.ret;
                    }
                    if let Some((_, sig)) = self
                        .syms
                        .supertype_methods_name(internal)
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
        // Type each parameter's DEFAULT value so its type info is recorded — the `$default` stub lowering
        // needs it to resolve a NON-literal default (a ctor call `f: Filt = Filt()`, an object read); a
        // method's defaults were previously left unchecked (only `check_fun` did top-level ones), so a
        // non-literal member default typed `Error` and the stub bailed ("call Filt"). A default may read
        // `this`/members but not other parameters (the latter is rejected in `collect_signatures`).
        for p in &f.params {
            if let Some(dx) = p.default {
                let pty = self.resolve_ty(&p.ty);
                self.check_default_arg(&p.ty, dx, pty);
            }
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
                            .insert((internal, f.name.clone(), params), inferred);
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
        for t in reified_added {
            self.reified_tparams.remove(&t);
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

    fn obj_name_is_subtype(&self, sub: TypeName, sup: TypeName) -> bool {
        crate::assignable::is_subtype(
            &crate::assignable::TyCtx::new(),
            self,
            Ty::obj_name(sub),
            Ty::obj_name(sup),
        )
    }

    /// Are two reference types comparable as `when`-subject value arms? A `when (s) { A -> … }` over a
    /// sealed subject `s: S` matches the *object* `A` (a subtype of `S`) by `==` — valid in Kotlin
    /// whenever one operand's type is a subtype of the other (the comparison can be non-trivially true).
    /// Only object/array reference types qualify; primitives go through `Ty::promote`.
    fn when_objs_comparable(&self, st: Ty, ct: Ty) -> bool {
        match (st, ct) {
            (Ty::Obj(a, _), Ty::Obj(b, _)) => {
                self.obj_name_is_subtype(a, b) || self.obj_name_is_subtype(b, a)
            }
            _ => false,
        }
    }

    fn lookup_method_name(&self, internal: TypeName, name: &str) -> Option<Signature> {
        let c = self.syms.class_by_type_name(internal)?;
        if let Some(sigs) = c.methods.get(name) {
            // Sole overload only: this lookup has no argument types to select with.
            return match sigs.as_slice() {
                [one] => Some(one.clone()),
                _ => None,
            };
        }
        // A class provides its implemented interfaces' methods — directly overridden, inherited, or (for
        // `: I by d`) delegated. Resolving them here lets a delegating class's calls type-check.
        for i in c.interfaces.iter_ids() {
            if let Some(sig) = self.lookup_method_name(i, name) {
                return Some(sig);
            }
        }
        self.lookup_method_name(c.super_internal?, name)
    }

    /// Resolve a property (own or inherited) on a class internal name.
    fn lookup_prop(&self, internal: &str, name: &str) -> Option<(Ty, bool)> {
        let internal = existing_type_name(internal)?;
        self.lookup_prop_name(internal, name)
    }

    fn lookup_prop_name(&self, internal: TypeName, name: &str) -> Option<(Ty, bool)> {
        let c = self.syms.class_by_type_name(internal)?;
        if let Some(p) = c.prop(name) {
            return Some(p);
        }
        self.lookup_prop_name(c.super_internal?, name)
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
        let lt = self.resolved_type_name(internal)?;
        let companion_ty = lt.companion_object.as_ref()?.1;
        Some(Ty::obj_name(companion_ty))
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
                    || p.is_erased_top()
                    || a.is_erased_top()
                    || matches!((p, a), (Ty::Obj(e, _), Ty::Obj(x, _)) if self.obj_name_is_subtype(x, e))
                    || p.accepts_numeric(a)
            })
    }

    /// Whether `internal` is a SAM interface krusty can soundly convert a lambda to: a user
    /// `fun interface` that is NON-generic and whose methods involve no value class. (A generic SAM
    /// erases its method to `Object` — the `LambdaMetafactory` descriptor `lower_lambda_sam` emits
    /// wouldn't match; a value-class method has a mangled name / boxing the path doesn't model; a
    /// library/Kotlin function interface is handled separately at the `Foo { … }` call site.)
    fn simple_fun_interface_name(&self, internal: TypeName) -> bool {
        let Some(c) = self.syms.class_by_type_name(internal) else {
            return false;
        };
        // A generic fun interface is allowed: its method erases to `Object`, which the SAM descriptor
        // (built from the erased interface method) and the erased lambda parameter types both match. A
        // value-class method is still excluded (mangled name / boxing not modeled).
        c.is_fun_interface
            && c.methods.values().flatten().all(|sig| {
                !self.ty_is_value_class(sig.ret)
                    && sig.params.iter().all(|p| !self.ty_is_value_class(*p))
            })
    }

    /// The SAM parameter types of a simple fun interface, used to type a converted lambda.
    fn fun_interface_sam_params_name(&self, internal: TypeName) -> Option<Vec<Ty>> {
        let c = self.syms.class_by_type_name(internal)?;
        Some(c.single_method()?.params.clone())
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
        // `Nothing?` — the type of a diverging safe call (`x?.let { return … }`: `null` when the receiver
        // is null, else never). It is `Nothing` widened nullable, so it flows into any reference target
        // (krusty erases reference nullability), exactly like the `null` literal below — but not a
        // primitive.
        if actual.non_null() == Ty::Nothing && expected.is_reference() {
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
        // Only in a RETURN position (an expression body, a getter, or a `return <expr>` statement in a
        // block body): a body like `= x as? A` — or `return xs.firstOrNull { … }` — yields a nullable
        // reference assignable to the declared non-null-erased return. `resolve_ty` erases reference
        // nullability from the declared return (`C?` → `C`), so a block-body `return` of a genuinely
        // nullable expression (`C?`) must compare non-null forms exactly as the expression-body path does;
        // otherwise the two spellings of the same return position diverge. Elsewhere keep the strict
        // comparison so a genuinely distinct nullable assignment isn't silently accepted.
        let (expected, actual) = if matches!(
            ctx,
            "function body" | "getter body" | "local function body" | "return"
        ) {
            (
                self.strip_nullable_ref(expected),
                self.strip_nullable_ref(actual),
            )
        } else {
            (expected, actual)
        };
        // Same-type non-null value flowing into its nullable form (`X` -> `X?`, including `Unit?`) —
        // any context, like the primitive box rule below: an assignment/argument boxes exactly as a
        // return does (the value-class pass inserts the box from the nullable target type). Generic
        // arguments compare by class only (`Result<T>` vs an erased call's `Result`), matching the
        // non-null `Obj`-to-`Obj` rule below, which ignores arguments too.
        if expected.is_nullable() && !actual.is_nullable() {
            let en = expected.non_null();
            let same_class =
                en == actual || matches!((en, actual), (Ty::Obj(e, _), Ty::Obj(a, _)) if e == a);
            if same_class
                && (actual == Ty::Unit
                    || self.ty_is_value_class(actual)
                    || self.syms.libraries.value_underlying(actual).is_some())
            {
                return;
            }
        }
        // A nullable REFERENCE argument whose non-null form is the expected type: krusty erases reference
        // nullability from a declared parameter (`C?` param → `C`), so a genuinely-nullable argument — an
        // INFERRED `C?` such as an elvis / branch-join result, which (unlike a declared type) keeps its
        // `?` — must still pass. The two share the JVM representation and krusty does not enforce
        // null-safety. `strip_nullable_ref` leaves a value class / nullable primitive nullable (their
        // nullable form is a distinct representation), so those are correctly NOT accepted here.
        if actual.is_nullable()
            && !expected.is_nullable()
            && self.strip_nullable_ref(actual) == expected
        {
            return;
        }
        // An erased generic reference array (`Array<Any>`, e.g. `emptyArray<T>()` → `Object[]`) is
        // assignable to any specific reference array — `Array` is invariant, but the erased value
        // really is the target type at runtime, so kotlinc inserts a `checkcast` at the use site.
        if let (Some(ae), Some(ee)) = (actual.array_elem(), expected.array_elem()) {
            if ae.is_erased_top() && ee.is_reference() {
                return;
            }
        }
        // Numeric literal narrowing and primitive widening; emit sites insert the conversion.
        if expected.accepts_numeric(actual) {
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
        if expected.is_erased_top() {
            return;
        }
        if actual.is_erased_top() && expected != Ty::Unit {
            return;
        }
        // A primitive flowing into a reference supertype is checked through its boxed source type; the
        // provider's type hierarchy decides whether that box implements `Number`, `Comparable`, etc.
        if let (Some(Ty::Obj(b, _)), Ty::Obj(e, _)) = (actual.boxed_ref(), expected) {
            if self.obj_name_is_subtype(b, e) {
                return;
            }
        }
        // The dedicated `Ty::String` variant and its object form `Obj("kotlin/String")` denote the SAME
        // type (they share an erased key); a value of one flows into the other. A metadata-derived return
        // spells it as the object form, the source `: String` annotation as `Ty::String`.
        if (expected == Ty::String && actual == Ty::obj("kotlin/String"))
            || (actual == Ty::String && expected == Ty::obj("kotlin/String"))
        {
            return;
        }
        // String is a reference type with classpath supertypes; ask the same hierarchy walker instead of
        // keeping a local list of platform interfaces.
        if actual == Ty::String || actual == Ty::obj("kotlin/String") {
            if let Ty::Obj(e, _) = expected {
                if self.obj_name_is_subtype(type_name("kotlin/String"), e) {
                    return;
                }
            }
        }
        // A class value is assignable to an interface (supertype) it implements.
        if let (Ty::Obj(e, _), Ty::Obj(a, _)) = (expected, actual) {
            if self.obj_name_is_subtype(a, e) {
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
            // A user class that declares a function-type supertype (`class C : () -> R`) implements the
            // matching `kotlin/jvm/functions/FunctionN`, so an instance is-a `(…) -> R`. Reach that
            // interface through the class hierarchy.
            if let (Some(actual_internal), Some(fn_internal)) = (
                actual.obj_internal(),
                expected.function_interface_internal(),
            ) {
                if self.obj_name_is_subtype(actual_internal, type_name(fn_internal)) {
                    return;
                }
            }
        }
        // SAM conversion: a function value (lambda) is assignable to a simple `fun interface` — the
        // lowering builds an instance whose single abstract method runs the lambda.
        if matches!(actual, Ty::Fun(_)) {
            if let Some(internal) = expected.obj_internal() {
                if self.simple_fun_interface_name(internal) {
                    return;
                }
            }
        }
        if expected.array_elem().is_some()
            && actual.array_elem().is_some()
            && crate::assignable::is_subtype(
                &crate::assignable::TyCtx::new(),
                self,
                actual,
                expected,
            )
        {
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

    fn expr(&mut self, e: ExprId) -> Ty {
        // Guard against a stack overflow on a pathologically deep expression: past the limit the
        // expression types as `Error` (the file is skipped, never crashed).
        self.expr_depth += 1;
        if self.expr_depth > 500 {
            self.expr_depth -= 1;
            return self.set(e, Ty::Error);
        }
        // Consume the propagated expectation so it reaches only THIS expression; a nested
        // subexpression sees `None` unless a propagation site re-arms it via `expr_expected`.
        let expected = self.expected.take();
        let t = self.expr_inner(e, expected);
        self.expr_depth -= 1;
        t
    }

    /// Check `e` with an EXPECTED type propagated in (see [`Self::expected`]).
    fn expr_expected(&mut self, e: ExprId, expected: Ty) -> Ty {
        self.expected = Some(expected);
        self.expr(e)
    }

    fn arg_tys(&mut self, args: &[ExprId]) -> Vec<Ty> {
        args.iter().map(|&a| self.expr(a)).collect()
    }

    /// Like [`Self::arg_tys`], but type each LAMBDA argument against the extension `name`'s block
    /// parameter (bound by `receiver`), so `it` gets the real element/receiver type instead of the erased
    /// `Any` — the same binding the plain member-call path applies. A non-lambda argument types normally
    /// (once). Used where an extension call's arguments are typed OUTSIDE that path (e.g. the safe-call
    /// `?.` arm, which passes `receiver.non_null()`).
    fn ext_arg_tys(&mut self, receiver: Ty, name: &str, args: &[ExprId]) -> Vec<Ty> {
        let partial: Vec<Option<Ty>> = args
            .iter()
            .map(|&x| (!matches!(self.file.expr(x), Expr::Lambda { .. })).then(|| self.expr(x)))
            .collect();
        let pts = self.extension_lambda_param_types(receiver, name, &partial);
        args.iter()
            .enumerate()
            .map(|(i, &x)| match pts.as_ref().and_then(|p| p.get(i)) {
                Some(pt) if !pt.is_empty() && matches!(self.file.expr(x), Expr::Lambda { .. }) => {
                    self.check_lambda_with_types(x, pt)
                }
                _ => partial[i].unwrap_or_else(|| self.expr(x)),
            })
            .collect()
    }

    /// Try to ADAPT a same-file top-level function reference `::name` to an expected function type `exp`
    /// that has FEWER parameters — the dropped trailing parameters must all have defaults, and the
    /// retained parameters + return must match exactly. Records the adaptation (the lowerer synthesizes
    /// an adapter calling `name`'s `$default` stub) and returns the expected function type. `None` when no
    /// clean adaptation applies (the caller then types the reference normally). vararg/`suspend`
    /// conversion and non-trailing defaults are out of scope (later slices) — a sound skip.
    fn try_adapt_toplevel_ref(
        &mut self,
        ref_expr: ExprId,
        name: &str,
        exp: &crate::types::FnSig,
    ) -> Option<Ty> {
        if exp.suspend {
            return None;
        }
        // The target is a same-file top-level function, or a member imported from a same-file `object`
        // (`import Host.foo`) — the latter recorded so the adapter invokes it on `Host.INSTANCE`.
        let (sig, object_internal) = if let Some(sig) = self.syms.single_fun(name) {
            (sig, None)
        } else if !self.module_declares(name) {
            let internal = self.object_member_import(name)?;
            let sig = self.syms.method_of_name(internal, name)?;
            (sig, Some(internal))
        } else {
            return None;
        };
        let (n, m) = (exp.params.len(), sig.params.len());
        let call_sig = sig.call_sig();
        // VARARG COLLECTION: `::of` where `of`'s last parameter is a `vararg` (and the leading fixed
        // parameters have no defaults) adapted to a function type with MORE parameters than fixed — the
        // extra expected parameters are collected into the vararg array. `fixed` = m-1 leading parameters.
        if sig.vararg && !sig.is_suspend {
            let fixed = m - 1;
            let coerce_unit = exp.ret == Ty::Unit && sig.ret != Ty::Unit;
            let fixed_ok = (0..fixed).all(|i| !call_sig.param_has_default(i))
                && sig.params.get(..fixed) == exp.params.get(..fixed);
            // Only when there is at least one COLLECTED argument (n > fixed) — an exactly-fixed count is
            // the empty-vararg drop handled by the `vararg_tail` path below.
            if n > fixed
                && fixed_ok
                && (sig.ret == exp.ret || coerce_unit)
                && sig.params[fixed].array_elem().is_some_and(|elem| {
                    exp.params[fixed..]
                        .iter()
                        .all(|&p| elem.is_erased_top() || p == elem)
                })
            {
                self.expr_lowers.insert(
                    ref_expr,
                    ExprLowering::AdaptedVarargCollect {
                        name: name.to_string(),
                        target_params: sig.params.clone(),
                        adapted_params: exp.params.clone(),
                        ret: exp.ret,
                        fixed,
                        object_internal,
                    },
                );
                return Some(Ty::fun(exp.params.clone(), exp.ret));
            }
        }
        // Only the vararg-collection shape is modeled for an object-member target; the `$default`/drop
        // shapes below are top-level-only.
        if object_internal.is_some() || sig.is_suspend || n > m {
            return None;
        }
        // The retained prefix parameters must match; the return matches EXACTLY or coerces to `Unit`
        // (the expected type discards a value result).
        let coerce_unit = exp.ret == Ty::Unit && sig.ret != Ty::Unit;
        if sig.params[..n] != exp.params[..] || (sig.ret != exp.ret && !coerce_unit) {
            return None;
        }
        // Adaptation must actually CHANGE something — otherwise the reference types normally.
        if n == m && !coerce_unit {
            return None;
        }
        // Parameter-drop shape for the dropped tail `sig.params[n..m]` (empty when `n == m`). Each dropped
        // parameter must be fillable WITHOUT an argument: a DEFAULT (filled by the `$default` stub) or the
        // function's single trailing `vararg` (filled with an empty array). A `vararg` as the ONLY dropped
        // parameter (`vararg_tail`) is a plain call; a vararg preceded by dropped defaults routes through
        // `$default` with an empty array for the vararg slot.
        let vararg_tail = sig.vararg && n == m - 1;
        if n < m
            && !vararg_tail
            && !(n..m).all(|k| call_sig.param_has_default(k) || (call_sig.vararg && k + 1 == m))
        {
            return None;
        }
        self.expr_lowers.insert(
            ref_expr,
            ExprLowering::AdaptedRef {
                name: name.to_string(),
                target_params: sig.params.clone(),
                adapted_params: exp.params.clone(),
                ret: exp.ret,
                vararg_tail,
                target_vararg: sig.vararg,
                coerce_unit,
            },
        );
        Some(Ty::fun(exp.params.clone(), exp.ret))
    }

    fn expr_inner(&mut self, e: ExprId, expected: Option<Ty>) -> Ty {
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
                // An EXPECTED function type propagated in from a typed context binds the lambda's
                // parameter types — `val f: (Int) -> Int = { it * 2 }`, even when the lambda is the
                // result of a nested `if`/`when` branch or block. Delegate to the shared typed-lambda
                // check (the same path a HOF/typed-initializer argument takes).
                if let Some(Ty::Fun(s)) = &expected {
                    let pts = s.params.clone();
                    return self.check_lambda_with_types(e, &pts);
                }
                // A lambda literal `{ a, b -> body }` — type is `Fun(arity)`. With no explicit
                // parameters but a body referencing `it`, bind the implicit single parameter.
                let bind_names: Vec<String> = if !params.is_empty() {
                    params.clone()
                } else if self.file.expr_uses_name(body, "it") {
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
                let lc_before = self.local_function_use_count();
                let bret = self.expr(body);
                // A non-inlined lambda that calls a local function would dispatch it on the lambda
                // class (the local fun lives on the enclosing facade/class) — reject rather than
                // miscompile (the recursive nested-closure case).
                if self.local_function_use_count() > lc_before {
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
                // An anonymous function's declared return type (`fun (…): T`) IS the function type's
                // return — a block body ending in `return` yields body type `Nothing`, which would
                // otherwise erase the result. Fall back to the body type when unannotated.
                let ret = self
                    .file
                    .anon_fun_ret
                    .get(&e.0)
                    .map(|r| self.resolve_ty(r))
                    .unwrap_or(bret);
                Ty::fun(fun_params, ret)
            }
            Expr::Index { array, indices } => {
                // `a[i]` / `recv[i, j, …]` — a subscript. A SINGLE index over an array is element access
                // (or `String.get`); otherwise (and for two-or-more indices) it is a `get(i, j, …)`
                // operator — a user member, a same-module extension, or a library member.
                let at = self.expr(array);
                let its: Vec<Ty> = indices.iter().map(|&i| self.expr(i)).collect();
                if let [index] = indices.as_slice() {
                    if let Some(elem) = at.array_elem() {
                        self.expect_assignable(Ty::Int, its[0], self.span(*index), "array index");
                        return self.set(e, elem);
                    }
                    // `str[i]` is the `String.get(Int): Char` operator.
                    if at == Ty::String {
                        if let Some(m) = self.resolve_instance_member(at, "get", &its) {
                            let ret = m.ret;
                            self.resolved_calls.insert(e, ResolvedCall::Member(m));
                            return self.set(e, ret);
                        }
                    }
                }
                if at == Ty::Error {
                    return Ty::Error;
                }
                // A user-class member `operator fun get(i, j, …)`.
                if let Some(internal) = at.obj_internal() {
                    if let Some((owner, sig)) = self.syms.method_of_with_owner_name(internal, "get")
                    {
                        if sig.params.len() == its.len() {
                            for (i, &pt) in sig.params.iter().enumerate() {
                                self.expect_assignable(pt, its[i], self.span(indices[i]), "index");
                            }
                            let interface = self
                                .syms
                                .class_by_type_name(owner)
                                .is_some_and(|c| c.is_interface);
                            self.resolved_calls.insert(
                                e,
                                ResolvedCall::ModuleMember {
                                    owner,
                                    name: "get".to_string(),
                                    params: sig.params.clone(),
                                    ret: sig.ret,
                                    interface,
                                },
                            );
                            return self.set(e, sig.ret);
                        }
                    }
                }
                // A same-module extension `operator fun Recv.get(i, j, …)`.
                if let Some(sig) = self
                    .syms
                    .ext_fun_overloads(at, "get")
                    .iter()
                    .find(|s| s.params.len() == its.len())
                    .cloned()
                {
                    for (i, &pt) in sig.params.iter().enumerate() {
                        self.expect_assignable(pt, its[i], self.span(indices[i]), "index");
                    }
                    self.resolved_calls.insert(
                        e,
                        ResolvedCall::ModuleExtension {
                            receiver: at,
                            name: "get".to_string(),
                            params: sig.params.clone(),
                            ret: sig.ret,
                        },
                    );
                    return self.set(e, sig.ret);
                }
                // A library `get(i, j, …)` member (`List.get(Int)`, `Map.get(K)`).
                if let Some(m) = self.resolve_instance_member(at, "get", &its) {
                    let ret = m.ret;
                    self.resolved_calls.insert(e, ResolvedCall::Member(m));
                    return self.set(e, ret);
                }
                self.diags.error(
                    self.span(e),
                    if indices.len() == 1 {
                        format!("'{}' is not an array (cannot index)", at.name())
                    } else {
                        format!(
                            "no 'get' operator taking {} indices on '{}'",
                            its.len(),
                            at.name()
                        )
                    },
                );
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
                // A nested try + finally only mis-emits when the inlined finally is re-entered on a RETURN
                // path: a `return` inlines the finally into the protected body, so a finally that then
                // diverges (its own `throw`/`return`) re-enters the enclosing handler and runs twice, and a
                // finally that CONTAINS a `try` likewise only breaks when a `return` crosses it (without a
                // `return`, the finally emits once per exit and the catch-all's live-exception slot is now
                // tracked in the frames — see `emit_try`). With no `return`, both shapes emit correctly.
                let reentrant = expr_try_finally_has_return(self.file, e)
                    || (expr_has_finally_with_try(self.file, e) && expr_has_return(self.file, e));
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
                    let cty = match self.catch_internal_name(&c.ty.name) {
                        // A catch type SHOULD be a `Throwable` subtype, but krusty's exception-hierarchy
                        // walk is incomplete (`NotImplementedError` and other stdlib errors don't chain
                        // to `Throwable`), so enforcing it here false-rejects valid catches. Deferred
                        // until the hierarchy is complete.
                        Some(i) => Ty::obj_name(i),
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
                    // (mismatch → `Unit`) so only an expression use that needs a value is constrained. A
                    // diverging (`Nothing`) branch drops out. Two values of the SAME class with differing
                    // type arguments (`List<Backup>` from the body vs `List<Nothing>` from a bare
                    // `emptyList()` catch) merge to that class with erased arguments (`List<*>`), assignable
                    // to the declared `List<Backup>` return — instead of collapsing to `Unit`, which wrongly
                    // typed an expression-bodied `try { … } catch { emptyList() }` as `Unit`. (This mirrors
                    // one case of `join` without its by-span coercion side effects, which mis-emit here.)
                    result = if result == ht {
                        result
                    } else if result == Ty::Nothing {
                        ht
                    } else if ht == Ty::Nothing {
                        result
                    } else if let (Ty::Obj(ai, _), Ty::Obj(bi, _)) = (result, ht) {
                        if ai == bi {
                            Ty::obj_name(ai)
                        } else {
                            Ty::Unit
                        }
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
                let unit_cast = ot == Ty::Unit || tt == Ty::Unit;
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
                            tt.is_erased_top()
                                || tt
                                    .obj_internal()
                                    .is_some_and(|t| self.obj_name_is_subtype(bw, t))
                        }));
                // A SAFE cast of a PRIMITIVE operand (`1 as? Byte`, `1.0 as? Int`): box the operand to its
                // wrapper, then `instanceof` the target wrapper/class — `null` on a mismatch (an `Int` box
                // is not a `Byte`). Sound for any known target (reference or boxable primitive).
                let prim_operand_safe_cast = nullable
                    && ot.jvm_boxed_ref().is_some()
                    && !self.ty_is_value_class(ot)
                    && (tt.is_reference() || tt.boxed_ref().is_some());
                // A cast between two (non-unsigned) PRIMITIVES (`1 as Byte`, `1.0 as Int`, `1 as Int`):
                // a CHECKED cast, not a numeric conversion — kotlinc boxes the operand, `checkcast`s the
                // TARGET wrapper (CCE when it differs), then unboxes; a same-type cast is identity. The
                // lowerer emits the box/checkcast/unbox.
                let scalar_prim =
                    |t: Ty| t.jvm_boxed_ref().is_some() && !t.is_reference() && !t.is_unsigned();
                let prim_to_prim = !nullable && scalar_prim(ot) && scalar_prim(tt);
                if (!(tt.is_reference() || prim_unbox || unit_cast)
                    || (!ot.is_reference() && !prim_box && !unit_cast && ot != Ty::Error))
                    && !prim_operand_safe_cast
                    && !prim_to_prim
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
                } else if st.is_reference() && st == et {
                    // A REFERENCE range `a..b` with a user/library `rangeTo` operator: `x in a..b`
                    // desugars to `a.rangeTo(b).contains(x)`. Resolve and record both selected
                    // operators here; lowering consumes those exact targets instead of re-resolving.
                    if let Some((range_ty, range_call)) =
                        self.operator_call_ret(st, "rangeTo", &[et], &[end])
                    {
                        if let Some((Ty::Boolean, contains_call)) =
                            self.operator_call_ret(range_ty, "contains", &[vt], &[value])
                        {
                            self.resolved_operator_calls
                                .insert((e, SyntheticOperatorCall::RangeTo), range_call);
                            self.resolved_operator_calls
                                .insert((e, SyntheticOperatorCall::Contains), contains_call);
                            return self.set(e, Ty::Boolean);
                        }
                    }
                    self.diags.error(
                        self.span(e),
                        "krusty: 'in' is only supported for primitive numeric ranges".to_string(),
                    );
                    Ty::Error
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
                if let Some(ty) = Ty::range_value_type(lt, rt) {
                    ty
                } else {
                    self.diags.error(
                        self.span(e),
                        "krusty: range expression is only supported for Int/Long/Char operands"
                            .to_string(),
                    );
                    Ty::Error
                }
            }
            Expr::IncDec { target, dec, .. } => {
                // `target++`/`++target` as a value: a simple mutable numeric/Char variable (the built-in
                // `inc`/`dec`), or a variable whose type has a user `inc`/`dec` operator. The result type
                // is the variable's type.
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
                            if !tt.is_numeric_or_char()
                                && self.inc_dec_operator_ret(tt, dec).is_none()
                            {
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
                } else if lt == Ty::Null
                    || matches!(lt0, Ty::Nullable(inner) if *inner == Ty::Nothing)
                {
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
                    if let Some(fi) = self
                        .resolver()
                        .resolve_symbol(crate::symbol_resolver::SymRecv::Value(rt), &name, &[], &[])
                        .map(crate::symbol_resolver::Symbol::overloads)
                        .unwrap_or_default()
                        .into_iter()
                        .find(|o| o.is_extension() && o.receiver_rank == 0)
                    {
                        let logical = fi.extension_value_params().to_vec();
                        let arg_tys = args.as_deref().map_or_else(Vec::new, |a| self.arg_tys(a));
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
                        // After `?.` the receiver is non-null, so resolve the member against the NON-NULL
                        // receiver type — mirroring the args (extension) branch below. A genuinely nullable
                        // receiver that ISN'T smart-cast (a call result: `xs.firstOrNull()?.field`) reached
                        // here as `Nullable(Obj(..))`, and `check_member` doesn't peel the nullable for a
                        // user class, so a member read on it failed ("unresolved member … on 'C'"). A
                        // smart-cast local (`val c: C? = C(); c?.x`) already arrived non-null, which hid this.
                        None => self.check_member(rt.non_null(), &name, self.span(e), Some(e)),
                        Some(a) => {
                            // A safe call to a lambda-taking extension (`c?.takeIf { it.at > 0 }`) types its
                            // lambda argument against the extension's block parameter (bound by the NON-NULL
                            // receiver) — otherwise `it` defaults to `Any` and a member access on it fails
                            // ("member … on Any"). `?.let`/`?.run`/… already route through
                            // `safe_scope_call_result`; this covers `takeIf`/`takeUnless`/any other lambda
                            // extension reached by `?.`.
                            // After `?.` the receiver is non-null, so resolve the member/extension against
                            // the NON-NULL receiver type. Matching `rt` directly missed a nullable object
                            // receiver (`Tok?` — e.g. the correctly-recovered nullable return of a classpath
                            // call), dropping to the naive `arg_tys` fallback below that re-typed the lambda's
                            // `it` as `Any`. `recv` restores parity with the non-safe call path.
                            let recv = rt.non_null();
                            let arg_tys = self.ext_arg_tys(recv, &name, a);
                            let inline_arg_supported = !a
                                .iter()
                                .any(|x| matches!(self.file.expr(*x), Expr::CallableRef { .. }));
                            if let ("toString", []) = (name.as_str(), arg_tys.as_slice()) {
                                Ty::String
                            } else if let ("hashCode", []) = (name.as_str(), arg_tys.as_slice()) {
                                Ty::Int // Int (not a reference), so safe-call rejection fires below
                            } else if recv == Ty::String {
                                if let Some(m) = self.resolve_instance_member(recv, &name, &arg_tys)
                                {
                                    let ret = m.ret;
                                    self.resolved_calls.insert(e, ResolvedCall::Member(m));
                                    ret
                                } else {
                                    // A stdlib extension reached by `?.` on a `String` — resolve AND RECORD it
                                    // (keyed by the safe-call `ExprId`) for the lowerer, admitting `@InlineOnly`.
                                    inline_arg_supported
                                        .then(|| {
                                            self.record_library_extension_call(
                                                Some(e),
                                                &name,
                                                rt.non_null(),
                                                &arg_tys,
                                                &[],
                                            )
                                        })
                                        .flatten()
                                        .unwrap_or(Ty::Error)
                                }
                            } else if let Ty::Obj(internal, _) = recv {
                                // A MODULE (user) class member only; a classpath / inherited-classpath
                                // member falls through to the classpath selectors below (which pick by
                                // argument fit and record the call for emit).
                                crate::module_symbols::ModuleSymbols::new(self.syms)
                                    .instance_members(recv, &name)
                                    .into_iter()
                                    .next()
                                    .map(|m| m.ret)
                                    .or_else(|| {
                                        self.resolve_instance_name(internal, &name, &arg_tys).map(
                                            |m| {
                                                let ret = m.ret;
                                                let suspend = m.suspend;
                                                self.resolved_calls.insert(
                                                    e,
                                                    ResolvedCall::Member(
                                                        crate::symbol_resolver::ResolvedMember {
                                                            member: m,
                                                            ret,
                                                            suspend,
                                                        },
                                                    ),
                                                );
                                                ret
                                            },
                                        )
                                    })
                                    // A stdlib/classpath EXTENSION reached by `?.` (`c?.takeIf { … }`):
                                    // resolve AND RECORD the callable keyed by the safe-call `ExprId`, so the
                                    // lowerer's `lower_ext_call_on(e)` reads it instead of re-resolving. One
                                    // resolution admits `@InlineOnly` (`takeIf`) — no separate inline path.
                                    .or_else(|| {
                                        inline_arg_supported
                                            .then(|| {
                                                self.record_library_extension_call(
                                                    Some(e),
                                                    &name,
                                                    rt.non_null(),
                                                    &arg_tys,
                                                    &[],
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
                    let arg_tys = args.as_deref().map_or_else(Vec::new, |a| self.arg_tys(a));
                    self.resolver()
                        .resolve_symbol(
                            crate::symbol_resolver::SymRecv::Value(rt.non_null()),
                            &name,
                            &[],
                            &[],
                        )
                        .map(crate::symbol_resolver::Symbol::overloads)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(crate::libraries::FunctionInfo::is_extension)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .find(|o| o.extension_value_params().len() == arg_tys.len())
                        .map(|fi| fi.callable.ret)
                        .unwrap_or(Ty::Error)
                } else {
                    result
                };
                // A RECEIVER-function-typed value in scope reached by `?.` (`b?.f()` where
                // `f: Bar.() -> R` is a local/parameter — `(x as? Bar)?.bar()`): no member or
                // extension matched above; resolve `f` lexically with the NON-NULL receiver as the
                // folded-first argument, mirroring the plain `b.f()` member-call path. Non-`suspend`
                // only (no continuation threading here).
                let result = if result == Ty::Error {
                    let arg_tys = args.as_deref().map_or_else(Vec::new, |a| self.arg_tys(a));
                    self.lookup(&name)
                        .and_then(|l| match l.narrowed.unwrap_or(l.ty) {
                            Ty::Fun(sig) if !sig.suspend => Some(sig),
                            _ => None,
                        })
                        .and_then(|sig| {
                            let (&first, rest) = sig.params.split_first()?;
                            (rest.len() == arg_tys.len()
                                && arg_assignable_simple(first, rt.non_null()))
                            .then(|| {
                                self.expr_lowers.insert(
                                    e,
                                    ExprLowering::ReceiverFnInvoke {
                                        name: name.clone(),
                                        params: sig.params.clone(),
                                        ret: sig.ret,
                                    },
                                );
                                sig.ret
                            })
                        })
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
                    if result == Ty::Nothing {
                        // A safe call whose member/block VALUE is `Nothing` — a value-yielding scope block
                        // that diverges (`x?.let { return … }` / `x?.run { throw … }`) or a member returning
                        // `Nothing` (`x?.fail()`). The whole safe call is `Nothing?` — `null` when the
                        // receiver is null, else control never comes back. That is a nullable (reference)
                        // type, so the lowerer's null-merge stays well-typed; this mirrors kotlinc, which
                        // types `x?.let { return … }` as `Nothing?`. Keyed on the `Nothing` result TYPE, not
                        // on a function name — a plain `C?` receiver hits it too. (A receiver-returning scope
                        // fn — `also`/`apply` — keeps the receiver type here even when its block diverges; the
                        // lowerer detects that block-body divergence and applies the same guarded lowering.)
                        return self.set(e, Ty::nullable(Ty::Nothing));
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
                Some(l) => l.narrowed.unwrap_or(l.ty),
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
                    if let Some(bt) = self.this_narrow {
                        if let Some(bi) = bt.obj_internal() {
                            if let Some((ty, _)) = self.lookup_prop_name(bi, &n) {
                                self.narrowed_this_member.insert(e, bi);
                                return self.set(e, ty);
                            }
                            if let Some(ty) = self.try_member_read(bt, &n, self.span(e), Some(e)) {
                                self.narrowed_this_member.insert(e, bi);
                                return self.set(e, ty);
                            }
                        }
                    }
                    // Unqualified property of the implicit/extension receiver: `fun Box.f() = v`
                    // means `this.v` (sibling method calls already resolve via `this_ty`).
                    if let Some(Ty::Obj(internal, _)) = self.this_ty {
                        if let Some((ty, _)) = self.lookup_prop_name(internal, &n) {
                            return self.set(e, ty);
                        }
                    }
                    // An unqualified COMPANION property inside a REGULAR member (`HEX_RADIX` where
                    // `companion object { const val HEX_RADIX = 16 }`): the companion's members are hoisted
                    // to static fields on the outer class and are readable unqualified from any member.
                    // The `companion_of` branch above only fires when checking a companion member; this
                    // covers the bare form from a plain method (the qualified `C.HEX_RADIX` path already
                    // resolves it via `static_props`).
                    if let Some(Ty::Obj(internal, _)) = self.this_ty {
                        if let Some(&ty) = self
                            .syms
                            .class_by_type_name(internal)
                            .and_then(|c| c.static_props.get(&n))
                        {
                            return self.set(e, ty);
                        }
                    }
                    // A bare name resolved against the implicit receiver (`this`) of arbitrary type —
                    // e.g. `length` inside `"ab".run { length }` (`this` is `String`). Goes through the
                    // general member read so builtin/library members (`String.length`) resolve too.
                    if let Some(rt) = self.this_ty {
                        if let Some(ty) = self.try_member_read(rt, &n, self.span(e), Some(e)) {
                            return self.set(e, ty);
                        }
                    }
                    // The bare name is not a member of the DECLARED receiver — try the flow-narrowed
                    // receiver from an enclosing `if (this is B)`. A member found only on `B` records
                    // the narrowing so the lowerer inserts a `checkcast` on `this` before the read.
                    if let Some(bt) = self.this_narrow {
                        if let Some((ty, _)) =
                            bt.obj_internal().and_then(|i| self.lookup_prop_name(i, &n))
                        {
                            if let Some(bi) = bt.obj_internal() {
                                self.narrowed_this_member.insert(e, bi);
                            }
                            return self.set(e, ty);
                        }
                    }
                    if self.syms.objects.contains(&n) {
                        // A bare `object` name used as a value (`val x = Foo`, or a self-reference
                        // `object Foo { … Foo … }`) — its type is the singleton, read as `Foo.INSTANCE`
                        // by lowering. Resolved here so an object can refer to itself in its own body.
                        if let Some(cls) = self.syms.classes.get(&n) {
                            return self.set(e, Ty::obj(&cls.internal()));
                        }
                    }
                    // A class NAME with a typed `companion object` used as a VALUE (`val c: I = C`): its
                    // value is the companion instance (`C.Companion`), typed as `C$Companion` — which the
                    // collect pass registered with the companion's supertypes, so it is assignable to them.
                    // Lowering reads `getstatic C.Companion`. (Only classes whose companion declares a
                    // supertype get a `C$Companion` ClassSig; a plain companion isn't a first-class value.)
                    if !self.syms.objects.contains(&n) {
                        if let Some(cls) = self.syms.classes.get(&n) {
                            let comp_internal = format!("{}$Companion", cls.internal());
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
                        // Unit`). Lowering materializes the JVM singleton when a value is needed.
                        Ty::Unit
                    } else if let Some(ct) = self.classpath_companion_ty(&n) {
                        // A bare reference to a CLASSPATH class with a companion object (`Json` →
                        // `Json.Default`): its value is the companion instance, typed as the companion's
                        // type, so `Json.encodeToString(…)` resolves as an instance method on it.
                        // Lowering emits `getstatic <class>.<field>:LcompanionType;`.
                        self.set(e, ct)
                    } else if let Some(internal) = self.classpath_object_value(&n) {
                        // A CLASSPATH `object` referenced as a value (`EmptyCoroutineContext`): its type is
                        // the object type; lowering reads `getstatic <internal>.INSTANCE`.
                        self.expr_lowers
                            .insert(e, ExprLowering::ObjectValue { internal });
                        Ty::obj_name(internal)
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
                            .and_then(|i| self.syms.class_by_type_name(i))
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
                        if let Some(fi) = self
                            .resolver()
                            .resolve_symbol(
                                crate::symbol_resolver::SymRecv::Value(lt),
                                fname,
                                &[],
                                &[],
                            )
                            .map(crate::symbol_resolver::Symbol::overloads)
                            .unwrap_or_default()
                            .into_iter()
                            .find(|o| o.is_extension() && o.receiver_rank == 0)
                        {
                            // Only apply the extension when the RIGHT operand actually matches its
                            // parameter type; otherwise this is the builtin (`Int * Int` inside the body
                            // of a `Int.times(V)` extension must NOT re-pick that extension and infer `V`).
                            if let [p] = fi.extension_value_params() {
                                // Match the lowerer's guard (ir_lower Binary extension path): an exact
                                // operand/param match, or a reference subtype. No loose cross-numeric
                                // clause — a numeric-param operator on a primitive is the builtin's job
                                // (and `p == rt` already covers a same-type numeric param).
                                let arg_ok = *p == rt
                                    || (p.is_reference()
                                        && rt.is_reference()
                                        && match ((*p).obj_internal(), rt.obj_internal()) {
                                            (Some(ps), Some(rs)) => {
                                                self.obj_name_is_subtype(rs, ps)
                                            }
                                            _ => true,
                                        });
                                if arg_ok {
                                    return self.set(e, fi.callable.ret);
                                }
                            }
                        }
                    }
                }
                // A class member operator (`a + b` -> `a.plus(b)`). Lowering re-resolves it.
                if let Ty::Obj(internal, _) = &lt {
                    let op_name = op.arith_operator_name();
                    if let Some(fname) = op_name {
                        if let Some(sig) = self.syms.method_of_name(*internal, fname) {
                            if let Some(param) = sig.single_param().filter(|_| rt != Ty::Error) {
                                self.expect_assignable(
                                    param,
                                    rt,
                                    self.span(rhs),
                                    "operator argument",
                                );
                                return self.set(e, sig.ret);
                            }
                        }
                    }
                    // A class `compareTo(o): Int` drives `<`/`<=`/`>`/`>=`.
                    if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
                        && rt != Ty::Error
                    {
                        if let Some(sig) = self.syms.method_of_name(*internal, "compareTo") {
                            if let Some(param) = sig.single_param().filter(|_| sig.ret == Ty::Int) {
                                self.expect_assignable(
                                    param,
                                    rt,
                                    self.span(rhs),
                                    "operator argument",
                                );
                                return self.set(e, Ty::Boolean);
                            }
                        }
                        // A CLASSPATH `Comparable` type (`class Money : Comparable<Money>` compiled
                        // separately): its `operator fun compareTo(o): Int` is on the classpath, not in
                        // `method_of`. Resolve it through the library set and record the selected member
                        // for lowering.
                        // Only a REFERENCE right operand: an erased generic `Comparable<Double>.compareTo`
                        // takes `Object`, so a PRIMITIVE argument would need a box the lowering path here
                        // doesn't apply — leave that to the existing generic handling / a sound skip.
                        if rt.is_reference() {
                            if let Some(m) = self.resolve_instance_member(lt, "compareTo", &[rt]) {
                                if m.ret == Ty::Int {
                                    crate::trace_compiler!(
                                        "resolve",
                                        "classpath compareTo drives comparison on {internal}"
                                    );
                                    self.resolved_calls.insert(e, ResolvedCall::Member(m));
                                    return self.set(e, Ty::Boolean);
                                }
                            }
                        }
                    }
                    // A library operator function on a reference receiver: `a + b` desugars to `a.plus(b)`,
                    // resolved as a stdlib member/extension (`List + element` → `CollectionsKt.plus`).
                    // Record the selected callable so lowering emits the same target.
                    let op_name = op.arith_operator_name();
                    // Resolve `a + b` (etc.) as `a.plus(b)` through the library set. Overload selection
                    // picks the most specific candidate (`list + list` → the `Iterable` concat overload,
                    // `list + element` → the element overload), so a reference right operand is fine.
                    if let Some(fname) = op_name {
                        if rt != Ty::Error {
                            if let Some(ret) =
                                self.record_library_extension_call(Some(e), fname, lt, &[rt], &[])
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
                    if !self.value_root_shadows_classifier(&type_name) {
                        let source = self.fed_source();
                        if let Some(c) = self
                            .syms
                            .class_names
                            .library_companion_const(&source, &type_name, &name)
                        {
                            self.resolved_library_companion_consts.insert(e, c);
                            return self.set(e, c.ty);
                        }
                    }
                }
                // `Outer.NestedEnum.ENTRY` — a nested enum accessed through its enclosing type name.
                // The receiver `Outer.NestedEnum` is a `Member` chain (not a bare `Name`); flatten it
                // to the hoisted dotted key (`A.E`) under which the nested enum's entries register.
                if matches!(self.file.expr(receiver), Expr::Member { .. }) {
                    if let Some(path) = self.dotted_full_path(receiver) {
                        if self
                            .dotted_root(receiver)
                            .is_some_and(|r| !self.value_root_shadows_classifier(&r))
                        {
                            if let Some(entries) = self.syms.enums.get(&path) {
                                if entries.iter().any(|en| en == &name) {
                                    let internal = self
                                        .syms
                                        .classes
                                        .get(&path)
                                        .map(ClassSig::internal)
                                        .unwrap_or_else(|| path.replace('.', "$"));
                                    return self.set(e, Ty::obj(&internal));
                                }
                            }
                        }
                    }
                }
                // `EnumName.ENTRY` — a static enum entry access (receiver is the enum type name).
                if let Expr::Name(en) = self.file.expr(receiver).clone() {
                    if !self.value_root_shadows_classifier(&en) {
                        if let Some(entries) = self.syms.enums.get(&en) {
                            if entries.iter().any(|e| e == &name) {
                                let internal = self
                                    .syms
                                    .classes
                                    .get(&en)
                                    .map(ClassSig::internal)
                                    .unwrap_or(en.clone());
                                return self.set(e, Ty::obj(&internal));
                            }
                        }
                        // `Kind.PENDING` on a CLASSPATH enum — a static enum-constant field of the enum's
                        // own type. Lowering emits `getstatic <internal>.ENTRY:L<internal>;`.
                        if let Some(internal) = self
                            .imported_type_name(&en)
                            .or_else(|| self.syms.class_names.get(&en))
                        {
                            if self
                                .resolved_type_name(internal)
                                .is_some_and(|t| t.is_enum_entry(&name))
                            {
                                let rendered = internal.render();
                                crate::trace_compiler!(
                                    "resolve",
                                    "classpath enum entry {en}.{name} -> {rendered}"
                                );
                                self.resolved_library_enum_entries.insert(e, internal);
                                return self.set(e, Ty::obj_name(internal));
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
                            if self.resolved_type(&nested).is_some_and(|t| t.is_object()) {
                                // Value = `getstatic Outer$Nested.INSTANCE` (lowering reads `nested`), but
                                // TYPE it as the OUTER class — the runtime object is-a Outer, and erased
                                // argument matching wants `Outer` (`PrimitiveSerialDescriptor(_, PrimitiveKind)`
                                // accepts `PrimitiveKind.STRING`), not the narrower nested type.
                                self.expr_lowers.insert(
                                    e,
                                    ExprLowering::ObjectValue {
                                        internal: type_name(&nested),
                                    },
                                );
                                return self.set(e, Ty::obj(&outer));
                            }
                        }
                        // `ClassName.NestedObject` on a same-file class.
                        if let Some(cs) = self.syms.classes.get(&en) {
                            let nested = format!("{}${name}", cs.internal);
                            if self
                                .syms
                                .class_by_internal(&nested)
                                .is_some_and(|nc| nc.is_object)
                            {
                                self.expr_lowers.insert(
                                    e,
                                    ExprLowering::ObjectValue {
                                        internal: type_name(&nested),
                                    },
                                );
                                return self.set(e, Ty::obj(&nested));
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
                        .and_then(|i| self.syms.class_by_type_name(i))
                        .is_some_and(|c| c.value_field.is_some());
                    // A `Boolean` narrowed inside an `&&` chain then used as a `compareTo` receiver
                    // mis-lowers (a primitive `Boolean` has no instance `compareTo`); leave it un-narrowed
                    // in the chain case (a single `if (x is Boolean)` is unchanged).
                    if is_value || (is_and_chain && *t == Ty::Boolean) {
                        continue;
                    }
                    self.declare(n, *t, false);
                }
                // `if (this is B)` narrows the implicit receiver to `B` for the branch body.
                let tt =
                    self.with_this_narrow(
                        self.this_is_narrowing(cond, false),
                        |c| match &expected {
                            Some(ex) => c.expr_expected(then_branch, *ex),
                            None => c.expr(then_branch),
                        },
                    );
                self.pop_scope();
                match else_branch {
                    Some(eb) => {
                        let else_cast = self.smartcast_binding(cond, true);
                        self.push_scope();
                        if let Some((n, t)) = &else_cast {
                            self.declare(n, *t, false);
                        }
                        let et = self.with_this_narrow(self.this_is_narrowing(cond, true), |c| {
                            match &expected {
                                Some(ex) => c.expr_expected(eb, *ex),
                                None => c.expr(eb),
                            }
                        });
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
                        // `require(x is T)` / `check(x is T)` — a stdlib precondition that throws when the
                        // condition is FALSE (`contract { returns() implies (x is T) }`), so the condition
                        // holds for the rest of the block. Narrow a stable binding in the FIRST argument
                        // (the condition), exactly as the `if (…) return` guard above does. Gated on the
                        // stdlib name not being shadowed by a lexical local or module-declared function.
                        else if let Expr::Call { callee, args } = self.file.expr(ie).clone() {
                            if let Expr::Name(fname) = self.file.expr(callee).clone() {
                                if (fname == "require" || fname == "check")
                                    && !args.is_empty()
                                    && self.is_resolved_stdlib_precondition_call(ie, &fname)
                                {
                                    if let Some((n, t)) = self.smartcast_binding(args[0], false) {
                                        self.declare(&n, t, false);
                                    }
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
                    // Propagate an expected type into the block's trailing value (a typed context
                    // reaching a `{ … ; lambda }` result).
                    Some(te) => match &expected {
                        Some(ex) => self.expr_expected(te, *ex),
                        None => self.expr(te),
                    },
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
                    // `when (this) { is B -> … }` narrows the implicit receiver to `B` in that arm's
                    // body (the `when`-subject analog of `if (this is B)`).
                    let arm_this_narrow = match arm.conditions.as_slice() {
                        [cnd] => self.this_is_narrowing(*cnd, false),
                        _ => None,
                    };
                    self.push_scope();
                    if let Some((n, t)) = &arm_cast {
                        self.declare(n, *t, false);
                    }
                    let bt = self.with_this_narrow(arm_this_narrow, |c| match &expected {
                        Some(ex) => c.expr_expected(arm.body, *ex),
                        None => c.expr(arm.body),
                    });
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
                        // `T::class` on a REIFIED type parameter is an unbound literal: the lowerer
                        // substitutes `T` to the call-site type (`reified_subst`) when it expands the inline
                        // body. Recorded as `Obj(T)`, a marker the lowerer resolves by name. Only a REIFIED
                        // `T` is accepted — kotlinc rejects a class literal on a non-reified type parameter.
                        self.class_literal_unbound_ty(&n)
                            .or_else(|| self.reified_tparams.contains(&n).then(|| Ty::obj(&n)))
                    } else {
                        None
                    };
                    if unbound.is_none() {
                        // Bound: a reference receiver, or a boxable primitive (boxed then `getClass`).
                        let rt = self.expr(recv);
                        let boxable = rt.jvm_boxed_ref().is_some();
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
                        if sig.requires_all_args() {
                            self.mark_local_function_ref(e, stmt_id);
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
                        if let Some(sig) = self.syms.method_of_name(internal, &name) {
                            if sig.requires_all_args() && sig.ret != Ty::Nothing {
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
                    if let Some(sig) = self.syms.single_fun(&name) {
                        if sig.requires_all_args() {
                            return self.set(e, Ty::fun(sig.params.clone(), sig.ret));
                        }
                    }
                    // `::foo` where `foo` is imported from a SAME-FILE `object` (`import Host.foo`) — a
                    // bound reference to the singleton member, lowered like `Host::foo`. Only when the
                    // module does not declare `foo` itself (a local declaration shadows the import).
                    if !self.module_declares(&name) {
                        if let Some(internal) = self.object_member_import(&name) {
                            if let Some(sig) = self.syms.method_of_name(internal, &name) {
                                if sig.requires_all_args() {
                                    self.expr_lowers.insert(
                                        e,
                                        ExprLowering::ImportedObjectMemberRef { internal },
                                    );
                                    return self.set(e, Ty::fun(sig.params.clone(), sig.ret));
                                }
                            }
                        }
                    }
                    if !self.syms.classes.contains_key(&name) && !self.lexical_value_declares(&name)
                    {
                        let overload = crate::libraries::FunctionSet {
                            overloads: self
                                .resolver()
                                .resolve_symbol(
                                    crate::symbol_resolver::SymRecv::TopLevel,
                                    &name,
                                    &[],
                                    &[],
                                )
                                .map(crate::symbol_resolver::Symbol::overloads)
                                .unwrap_or_default(),
                        }
                        .into_single_top_level();
                        if let Some(o) = overload {
                            if o.call_sig.requires_all_args(o.callable.params.len())
                                && o.callable.ret != Ty::Nothing
                            {
                                self.expr_lowers.insert(
                                    e,
                                    ExprLowering::ClasspathTopLevelFunctionRef(o.callable.clone()),
                                );
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
                                    Ty::fun(cls.ctor_params.clone(), Ty::obj(&cls.internal())),
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
                                if let Some(sig) = self.syms.method_of_name(internal, &name) {
                                    if sig.requires_all_args() {
                                        return self.set(e, Ty::fun(sig.params.clone(), sig.ret));
                                    }
                                }
                                if let Some((_, is_var)) = self.lookup_prop_name(internal, &name) {
                                    if let Some(ty) = self.property_ref_ty(0, is_var) {
                                        return self.set(e, ty);
                                    }
                                }
                            }
                        }
                        // bound: `obj::m` where `obj` is an in-scope value
                        if let Some(loc) = self.lookup(&rn) {
                            if let Some(internal) = loc.ty.obj_internal() {
                                if let Some(sig) = self.syms.method_of_name(internal, &name) {
                                    if sig.requires_all_args() {
                                        self.expr(r); // capture the receiver
                                        return self.set(e, Ty::fun(sig.params.clone(), sig.ret));
                                    }
                                }
                                // bound property reference `obj::prop` keeps property-reference APIs.
                                if let Some(is_var) =
                                    self.syms.class_by_type_name(internal).and_then(|c| {
                                        c.props
                                            .iter()
                                            .find_map(|(n, _, v)| (*n == name).then_some(*v))
                                    })
                                {
                                    self.expr(r); // capture the receiver
                                    if let Some(ty) = self.property_ref_ty(0, is_var) {
                                        return self.set(e, ty);
                                    }
                                }
                                // bound EXTENSION property reference `obj::ext` (`val Recv.ext`) — a
                                // `KProperty0`; the lowerer synthesizes a reference calling the static
                                // extension getter/setter with the captured receiver.
                                if let Some((_, is_var)) =
                                    self.syms.ext_prop(Ty::obj_name(internal), &name)
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
                        if !self.value_root_shadows_classifier(&rn)
                            && !self.syms.objects.contains(&rn)
                        {
                            if let Some(cls) = self.syms.classes.get(&rn).cloned() {
                                if let Some(sig) = cls.method(&name).cloned() {
                                    if sig.requires_all_args() {
                                        let cls_internal = cls.internal();
                                        let mut params = vec![Ty::obj(&cls_internal)];
                                        params.extend(sig.params.iter().copied());
                                        return self.set(e, Ty::fun(params, sig.ret));
                                    }
                                }
                                // Unbound reference to a same-module EXTENSION function (`A::foo` where
                                // `fun A.foo()` is top-level): the function type prepends the receiver to
                                // the extension's own args — `(A, ext-args…) -> ext-ret`. (A member of
                                // the same name, checked above, takes precedence.)
                                let recv_ty = Ty::obj(&cls.internal());
                                if let Some(sig) = self.syms.ext_fun(recv_ty, &name).cloned() {
                                    if sig.requires_all_args() && sig.ret != Ty::Nothing {
                                        let mut params = vec![recv_ty];
                                        params.extend(sig.params.iter().copied());
                                        return self.set(e, Ty::fun(params, sig.ret));
                                    }
                                }
                                // unbound property reference `Type::prop` keeps property-reference APIs.
                                if let Some(is_var) = cls
                                    .props
                                    .iter()
                                    .find_map(|(n, _, v)| (*n == name).then_some(*v))
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
                        if !self.value_root_shadows_classifier(&rn)
                            && self.syms.objects.contains(&rn)
                        {
                            if let Some(cls) = self.syms.classes.get(&rn).cloned() {
                                if let Some(sig) = cls.method(&name).cloned() {
                                    if sig.requires_all_args() {
                                        return self.set(e, Ty::fun(sig.params.clone(), sig.ret));
                                    }
                                }
                                // Object property reference `O::p` — bound to the singleton, a
                                // `KProperty0` whose get/set dispatch the member accessor on `O.INSTANCE`.
                                if let Some(is_var) = cls
                                    .props
                                    .iter()
                                    .find_map(|(n, _, v)| (*n == name).then_some(*v))
                                {
                                    if let Some(ty) = self.property_ref_ty(0, is_var) {
                                        return self.set(e, ty);
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
                        if let Some(sig) = self.syms.method_of_name(internal, &name) {
                            if sig.requires_all_args() {
                                return self.set(e, Ty::fun(sig.params.clone(), sig.ret));
                            }
                            // ADAPTED bound member reference: the target has trailing default/vararg
                            // parameters the reference omits (`C(..)::memberVararg` → `(Int) -> Unit`).
                            // Expose the minimum-arity prefix; the lowerer's synthesized adapter fills the
                            // omitted parameters via the target's `$default` stub.
                            let min_arity =
                                adapted_ref_arity(sig.vararg, sig.required, sig.params.len());
                            if min_arity < sig.params.len() && sig.params.len() <= 31 {
                                let adapted: Vec<Ty> = sig.params[..min_arity].to_vec();
                                return self.set(e, Ty::fun(adapted, sig.ret));
                            }
                        }
                        // Bound property reference on an arbitrary-expression USER-class receiver
                        // (`A(..)::p`): the receiver is evaluated once and captured; the ref is a
                        // `KProperty0`. Only a `val` is typed — the lowerer models a read-only bound
                        // reference; a `var` (mutable reference) isn't lowered, so don't type it. (The
                        // `obj::p` Name form is handled above; a bound METHOD ref on such a receiver is
                        // handled by the member-method path in the lowerer.)
                        let immutable_prop = self.syms.class_by_type_name(internal).and_then(|c| {
                            c.props
                                .iter()
                                .find_map(|(n, _, v)| (*n == name).then_some(*v))
                        }) == Some(false);
                        if immutable_prop {
                            if let Some(ty) = self.property_ref_ty(0, false) {
                                return self.set(e, ty);
                            }
                        }
                    }
                    if let Some(sig) = self.syms.ext_fun(rty, &name).cloned() {
                        if sig.requires_all_args() {
                            return self.set(e, Ty::fun(sig.params.clone(), sig.ret));
                        }
                        // ADAPTED bound extension reference (`C(..)::extensionVararg` → `(Int) -> Unit`):
                        // expose the minimum-arity prefix; the lowerer synthesizes an adapter that fills
                        // the omitted default/vararg parameters via the extension's `$default` stub.
                        let min_arity =
                            adapted_ref_arity(sig.vararg, sig.required, sig.params.len());
                        if min_arity < sig.params.len() && sig.params.len() <= 31 {
                            let adapted: Vec<Ty> = sig.params[..min_arity].to_vec();
                            return self.set(e, Ty::fun(adapted, sig.ret));
                        }
                    }
                    // Bound member on a LIBRARY-type receiver (`"KOTLIN"::get`): resolve the classpath
                    // instance method and type as (member-args) -> ret, receiver bound. A `suspend` or
                    // `Nothing`-returning member isn't modeled as a plain function value → skip.
                    // A NULLABLE, type-parameter, or bare erased-`Any` receiver may be `null` at runtime,
                    // and kotlinc routes `t::toString`/`hashCode`/`equals` on such a receiver through a
                    // null-safe intrinsic (`null::toString` yields "null"); a plain `invokevirtual` on the
                    // captured null would NPE. Only a non-null CONCRETE receiver is safe for the
                    // direct-dispatch bound ref. Kept in lock-step with the same guard in the lowerer's
                    // `lower_bound_expr_ref`, so the checker never types a ref the lowerer will skip.
                    let concrete = !matches!(rty, Ty::TyParam(..) | Ty::Nullable(..))
                        && rty.kotlin_class_internal() != Some(crate::types::wk::any());
                    if concrete && rty.kotlin_class_internal().is_some() {
                        // A bound PROPERTY reference (`"kotlin"::length`) is a `KProperty0<T>` whose
                        // `get()` yields the value — resolve it (the resolver decides property-vs-method
                        // and emittability) BEFORE the plain-function path, so `.get()`/`.name` resolve.
                        if let Some(prop_ref) = self.resolve_property_ref(rty, &name) {
                            if let Some(ty) = self.property_ref_ty(0, false) {
                                self.bound_property_refs.insert(e, prop_ref);
                                return self.set(e, ty);
                            }
                        }
                        if let Some(m) = self.resolve_instance_ref(rty, &name) {
                            if !m.suspend && m.ret != Ty::Nothing {
                                self.bound_member_refs.insert(e, m.clone());
                                return self.set(e, Ty::fun(m.params.clone(), m.ret));
                            }
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
        // Unsigned arithmetic: both operands the same unsigned library integer type. `Ty` owns which
        // types those are; mixed signed/unsigned falls through to the ordinary type error.
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
                // Structural equality accepts reference-vs-value comparisons: Kotlin boxes the value
                // operand and calls null-safe equality (`Any? == 5`, `x.getOrNull() == 42`). Ordering and
                // arithmetic remain stricter.
                // A nullable-primitive wrapper (`Int?`/`Double?`) compares with its primitive (`a == 5.0`):
                // the lowerer null-checks the wrapper, then UNBOXES it and does a primitive `==` (`dcmp`/
                // `fcmp` for Float/Double — IEEE-754, so `-0.0 == 0.0`, `NaN != NaN`), never boxed `equals`.
                let wrapper_vs_prim =
                    |w: Ty, p: Ty| w.nullable_primitive().map_or(false, |pw| pw == p);
                let is_unit = |t: Ty| t.non_null() == Ty::Unit;
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
                    || (lt.is_erased_top() && has_boxable_value_equality(rt))
                    || (rt.is_erased_top() && has_boxable_value_equality(lt))
                    || lt.is_erased_top()
                    || rt.is_erased_top()
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
                Some(e) if e.jvm_boxed_ref().is_some() => {
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
            if elem.is_array() {
                self.diags.error(
                    span,
                    "krusty: Array(n) {…} with an array element is not supported".to_string(),
                );
                return Some(Ty::Error);
            }
            // `Array(n) { … }` is always the reference `Array<T>` (`Obj("kotlin/Array", [T])`), distinct
            // from a primitive array. The element stays LOGICAL `T` (a primitive `Int` reads as `Int`,
            // not a boxed wrapper); the backend owns the physical boxed/value-class array layout.
            return Some(Ty::obj_args("kotlin/Array", &[elem]));
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

    fn mark_receiver_lambda_call(
        &mut self,
        call: ExprId,
        receiver: ExprId,
        body: ExprId,
        returns_receiver: bool,
    ) {
        self.expr_lowers.insert(
            call,
            ExprLowering::InlineCall(InlineCall::ReceiverLambda(ReceiverLambda {
                receiver,
                body,
                returns_receiver,
            })),
        );
    }

    fn check_with_receiver_body(&mut self, recv: Ty, body: ExprId) -> Ty {
        self.push_scope();
        // A user class receiver's own properties are visible unqualified inside the body; for builtin
        // and library receivers (`String`, `StringBuilder`, …) a bare member resolves through the
        // implicit-`this` member probe in the `Expr::Name`/call arms instead.
        if let Ty::Obj(internal, _) = recv {
            if let Some(cs) = self.syms.class_by_type_name(internal) {
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
        let [arg] = args.as_deref()? else {
            return None;
        };
        let Expr::Lambda { params, body } = self.file.expr(*arg).clone() else {
            return None;
        };
        // Inside the lambda the receiver is NON-null: a nullable-primitive receiver (`Int?` =
        // `java/lang/Integer`, e.g. from a chained `s?.let { … }?.let { it + 1 }`) binds `it`/`this` as
        // the UNBOXED primitive (`Int`), so `it + 1` is primitive arithmetic, not `Integer + Int`. A
        // nullable REFERENCE receiver (`Map.Entry<K,V>?` from `map.entries.find { … }?.let { … }`) must
        // likewise drop its nullability, else a destructuring lambda param (`{ (k, v) -> … }`) resolves
        // `componentN` against the nullable type and fails ("cannot destructure … no operator 'component1'").
        let rt = rt.nullable_primitive().unwrap_or_else(|| rt.non_null());
        match name {
            "run" | "apply" if params.is_empty() => {
                let bt = self.check_with_receiver_labeled(rt, body, Some(name));
                Some(if name == "apply" { rt } else { bt })
            }
            "let" | "also" => {
                let lt = self.check_lambda_with_types(*arg, &[rt]);
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
    /// A user class that EXTENDS a classpath type inherits that supertype's members (`class Sub :
    /// lib.Base()` calling an inherited method, e.g. a `protected` one). `resolve_instance_member` on the
    /// user class itself misses them — the library has no `resolve_type` for a user class — so walk the
    /// declared supertype chain and resolve the member on each CLASSPATH supertype. Records the resolved
    /// member so the lowerer emits it directly. MUST run only after the module-member lookup fails, so a
    /// user override keeps precedence over the inherited classpath member.
    fn classpath_super_member_ret(
        &mut self,
        call: ExprId,
        sub_internal: TypeName,
        name: &str,
        arg_tys: &[Ty],
    ) -> Option<Ty> {
        // Not a user class — its own members already went through the library.
        self.syms.class_by_type_name(sub_internal)?;
        for sup in self.syms.supertype_internal_names_from(sub_internal) {
            if self.syms.class_by_type_name(sup).is_some() {
                continue; // a user supertype — already covered by the module-member walk
            }
            // Only inherit through a base CLASS chain, never an interface supertype: a concrete member
            // reached via an interface (e.g. `Object.clone` seen through a `Cloneable` supertype) would
            // emit an `invokevirtual` whose owner is an interface (`IncompatibleClassChangeError`).
            // Interface default methods are resolved by the ordinary member paths, not here.
            if self
                .resolved_type_name(sup)
                .is_none_or(|t| t.is_interface())
            {
                continue;
            }
            if let Some(m) = self.resolve_instance_member(Ty::obj_name(sup), name, arg_tys) {
                let ret = m.ret;
                self.resolved_calls.insert(call, ResolvedCall::Member(m));
                return Some(ret);
            }
        }
        None
    }

    fn this_member_call_ret(
        &mut self,
        call: ExprId,
        rt: Ty,
        name: &str,
        arg_tys: &[Ty],
        args: &[ExprId],
    ) -> Option<Ty> {
        if let ("toString", []) = (name, arg_tys) {
            // A function value's `toString()` is Kotlin's `(T) -> T` form, not the JVM default — reject.
            if matches!(rt, Ty::Fun(_)) {
                self.diags.error(
                    self.span(call),
                    "krusty: toString() on a function value is not supported".to_string(),
                );
                return Some(Ty::Error);
            }
            return Some(Ty::String);
        }
        if rt == Ty::String {
            match self.record_classpath_member_call_with_slots(call, rt, name, args) {
                ClasspathMemberSlotCall::Resolved(ret) => return Some(ret),
                ClasspathMemberSlotCall::Ambiguous => return Some(Ty::Error),
                ClasspathMemberSlotCall::NoMatch => {}
            }
            if let Some(m) = self.resolve_instance_member(rt, name, arg_tys) {
                let ret = m.ret;
                self.resolved_calls.insert(call, ResolvedCall::Member(m));
                return Some(ret);
            }
        }
        if let Ty::Obj(internal, _) = &rt {
            // A `vararg` member (`fun f(vararg s: T)`) accepts any number of trailing `T` arguments,
            // packed into the array parameter — element-type them rather than matching the single array
            // parameter positionally (which would reject `f(x)` as "T but Array<T> expected").
            if self.syms.method_is_vararg_name(*internal, name) {
                if let Some(sig) = self.syms.method_of_name(*internal, name) {
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
        // A MODULE (user-declared) class member, resolved by arity through the module source only. An
        // INHERITED classpath member must NOT bind here (it would arity-bind ignoring argument fit and
        // record nothing for the lowerer) — it falls through to `resolve_instance_member` below.
        if let Ty::Obj(_, _) = rt {
            let module_member = crate::module_symbols::ModuleSymbols::new(self.syms)
                .instance_members(rt, name)
                .into_iter()
                .next();
            if let Some(fi) = module_member {
                let params = fi.params.clone();
                if params.len() == arg_tys.len() {
                    for (i, (p, a)) in params.iter().zip(arg_tys).enumerate() {
                        self.expect_assignable(*p, *a, self.span(args[i]), "argument");
                    }
                    return Some(
                        self.inferred_member_ret(rt, name, &params)
                            .unwrap_or(fi.ret),
                    );
                }
            }
            match self.record_classpath_member_call_with_slots(call, rt, name, args) {
                ClasspathMemberSlotCall::Resolved(ret) => return Some(ret),
                ClasspathMemberSlotCall::Ambiguous => return Some(Ty::Error),
                ClasspathMemberSlotCall::NoMatch => {}
            }
            if let Some(m) = self.resolve_instance_member(rt, name, arg_tys) {
                let ret = m.ret;
                self.resolved_calls.insert(call, ResolvedCall::Member(m));
                return Some(ret);
            }
            if let Some(internal) = rt.obj_internal() {
                if let Some(ret) = self.classpath_super_member_ret(call, internal, name, arg_tys) {
                    return Some(ret);
                }
            }
        }
        // A MODULE extension on the receiver (`fun Recv.name(args)` declared in this compilation) — keyed
        // by the receiver's erased key, exactly as a qualified `recv.name(args)` extension call
        // resolves. Lets a bare call inside a receiver lambda reach a same-module extension on `this`.
        if let Some(sig) = self
            .syms
            .ext_fun_overloads(rt, name)
            .iter()
            .find(|s| !s.vararg && s.params.len() == arg_tys.len())
            .cloned()
        {
            for (i, (p, a)) in sig.params.iter().zip(arg_tys).enumerate() {
                self.expect_assignable(*p, *a, self.span(args[i]), "argument");
            }
            return Some(sig.ret);
        }
        // A stdlib/library EXTENSION on the receiver (`String.reversed()`, `String.uppercase()`):
        // resolved receiver-aware so the right overload is selected (`CharSequence.reversed`, not the
        // `Iterable.reversed` that a receiver-blind fallthrough would pick). RECORD the resolved callable
        // (keyed by the call `ExprId`) — not just its return type — so the lowerer emits it through the
        // ordinary extension path instead of bailing (`lower_ext_call_on` reads `resolved_extension`).
        // Mirrors the qualified `recv.name(args)` extension typing + recording.
        if let Some(ret) = self.record_library_extension_call_with_slots(call, name, rt, args, &[])
        {
            return Some(ret);
        }
        if let Some(ret) = self.record_library_extension_call(Some(call), name, rt, arg_tys, &[]) {
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
            .get(&(internal, name.to_string(), params.to_vec()))
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
                    || t.is_unsigned()
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
    fn user_generic_return(&self, f: &FunDecl, arg_tys: &[Ty]) -> Option<Ty> {
        if f.receiver.is_some() || f.type_params.is_empty() || f.params.len() != arg_tys.len() {
            return None;
        }
        let tparams: std::collections::HashSet<&str> =
            f.type_params.iter().map(String::as_str).collect();
        let mut binds: std::collections::HashMap<&str, Ty> = std::collections::HashMap::new();
        // Type parameters bound to two DIFFERENT types across binding sites (a plain-value param AND a
        // lambda return, or two lambda returns) — the real type argument is their common supertype /
        // intersection, which krusty can't compute, so a non-inline return over such a parameter is
        // declined below. `or_insert` keeps the first binding for the (inline) selection paths; the
        // conflict set is the soundness guard layered on top, covering EVERY binding site (not just the
        // plain-value witness scan).
        let mut conflicted: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (i, p) in f.params.iter().enumerate() {
            let at = &arg_tys[i];
            if p.ty.fun_params.is_empty() && p.ty.name != "<fun>" {
                // A plain value parameter typed as a bare type parameter (`x: T`).
                if tparams.contains(p.ty.name.as_str()) {
                    bind_or_conflict(&mut binds, &mut conflicted, p.ty.name.as_str(), *at);
                }
            } else if let Ty::Fun(fsig) = at {
                // A function-typed parameter `(A) -> R`: bind `A` from the lambda's parameter types
                // and `R` from its return type.
                for (decl, actual) in p.ty.fun_params.iter().zip(&fsig.params) {
                    if tparams.contains(decl.name.as_str()) {
                        bind_or_conflict(&mut binds, &mut conflicted, decl.name.as_str(), *actual);
                    }
                }
                if let Some(rret) = &p.ty.arg {
                    if tparams.contains(rret.name.as_str()) {
                        bind_or_conflict(&mut binds, &mut conflicted, rret.name.as_str(), fsig.ret);
                    }
                }
            }
        }
        // The return type parameter: from an explicit `: T` annotation, or — when the return is
        // inferred — from an expression body that is a bare parameter reference (`fun <T> id(x: T) = x`),
        // whose type is the parameter's declared type parameter.
        let ret_tp: Option<String> = if let Some(r) = &f.ret {
            Some(r.name.clone())
        } else if let FunBody::Expr(b) = &f.body {
            if let Expr::Name(n) = self.file.expr(*b) {
                f.params
                    .iter()
                    .find(|p| &p.name == n)
                    .filter(|p| p.ty.fun_params.is_empty() && tparams.contains(p.ty.name.as_str()))
                    .map(|p| p.ty.name.clone())
            } else {
                None
            }
        } else {
            None
        };
        // A type parameter with a declared upper bound (`<T : Int>`, `<T : Comparable<T>>`) is handled
        // by the bound-driven machinery: a primitive bound SPECIALIZES the descriptor (`(I)I`) and a
        // reference bound erases to it, while a primitive argument into a reference bound is DECLINED.
        // Inferring a boxed/plain return here would fight those paths (VerifyError, or a call that must
        // skip). Only recover the return for an UNBOUNDED type parameter.
        let ret_tp = ret_tp.filter(|r| !f.type_param_bounds.iter().any(|(n, _)| n == r));
        let bound = ret_tp.as_deref().and_then(|r| binds.get(r).copied())?;
        // A NON-inline generic call crosses the JVM erasure boundary (return is physically `Object`),
        // so recovering a concrete return here is only sound when the binding is UNAMBIGUOUS. krusty has
        // no constraint solver / least-upper-bound, so decline (stay erased → the use site's members
        // simply don't resolve, and the file skips instead of miscompiling) whenever the inference could
        // disagree with the value actually returned. An `inline` function is spliced with no erasure
        // boundary, so its binding flows directly and keeps the pre-existing behaviour.
        if !f.is_inline {
            let r = ret_tp.as_deref()?;
            // The return type parameter was bound to two different types across binding sites (a
            // plain-value arg and a lambda return, or two lambda returns `(Int) -> T` / `(String) -> T`)
            // — its real type argument is a common supertype the emitter can't `checkcast` to soundly.
            if conflicted.contains(r) {
                return None;
            }
            // A vararg param (`vararg ts: T`) binds `T` to the element type, but the physical parameter
            // is an array and the return machinery here doesn't model that — decline (KT-2739).
            if f.params.last().is_some_and(|p| p.is_vararg) {
                return None;
            }
            // Every plain-value parameter that mentions the return type parameter must bind it to the
            // SAME concrete type. Two arguments binding `T` to different types means the real `T` is a
            // common supertype / intersection (`select(x: T?, y: T)` called with unrelated args) — the
            // returned value's runtime type can then differ from any single argument's, so a naive
            // `checkcast` to one of them is a `ClassCastException`. A nullable-bare-`T` parameter
            // (`x: T?`) is likewise excluded: its argument type carries a nullability the erased `T`
            // slot doesn't, so it isn't a reliable witness for `T`.
            let mut witness: Option<Ty> = None;
            for (i, p) in f.params.iter().enumerate() {
                if p.ty.fun_params.is_empty() && p.ty.name == r {
                    if p.ty.nullable {
                        return None;
                    }
                    // A `null` literal / bottom argument (`bar(null, …)`) binds `T` to `Null`/`Nothing`,
                    // which is not the real `T` — the value actually returned (`r as T`) has the
                    // expected type, so typing the return as the bottom type is a `VerifyError` (KT-73166).
                    if matches!(arg_tys[i], Ty::Null | Ty::Nothing)
                        || matches!(arg_tys[i], Ty::Nullable(inner) if matches!(*inner, Ty::Nothing))
                    {
                        return None;
                    }
                    match witness {
                        None => witness = Some(arg_tys[i]),
                        Some(w) if w != arg_tys[i] => return None,
                        _ => {}
                    }
                }
            }
            // The runtime value of a primitive binding is its BOXED wrapper — type it as `Wrapper?` (as
            // `explicit_generic_return` does) so a use site unboxes through the normal nullable-primitive
            // machinery rather than the emitter calling a primitive method on a boxed value.
            if !bound.is_reference() {
                return bound.nullable_boxed();
            }
        }
        Some(bound)
    }

    /// A user top-level generic function called with an EXPLICIT type argument (`asSeq<String>(x)`)
    /// whose declared return is a bare type parameter (`fun <T> asSeq(...): T`): the call's result
    /// type is the supplied argument (`String`), so members of the result resolve (`…length`). `None`
    /// when there's no explicit type argument or the return isn't one of the function's type params.
    fn explicit_generic_return(&self, call: ExprId, f: &FunDecl) -> Option<Ty> {
        let targs = self.file.call_type_args.get(&call.0)?;
        if f.receiver.is_some() || f.type_params.is_empty() {
            return None;
        }
        let ret = f.ret.as_ref()?;
        let idx = f.type_params.iter().position(|tp| tp == &ret.name)?;
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
        call: Option<ExprId>,
        name: &str,
        receiver: Ty,
        arg_tys: &[Ty],
        type_args: &[Ty],
    ) -> Option<Ty> {
        crate::trace_compiler!(
            "resolve",
            "record_library_extension_call {name} recv={receiver:?} args={arg_tys:?} targs={type_args:?}"
        );
        let c = self.library_extension_callable(name, receiver, arg_tys, type_args)?;
        crate::trace_compiler!(
            "resolve",
            "record_library_extension_call {name} -> ret={:?} owner={} jvm={} desc={}",
            c.ret,
            c.owner.render(),
            c.name,
            c.descriptor
        );
        // The checker is the sole resolver: record the resolved extension callable so the lowerer emits
        // it directly without re-resolving. Keyed by the call `ExprId`; `None` for a synthesized operator
        // call (`a + b`) whose lowering is not an extension-emit read.
        if let Some(call) = call {
            self.resolved_calls
                .insert(call, ResolvedCall::Extension(c.clone()));
            self.record_inline_expansion_synthetic_members(call, name, receiver, &c);
        }
        Some(c.ret)
    }

    fn record_inline_expansion_synthetic_members(
        &mut self,
        call: ExprId,
        name: &str,
        receiver: Ty,
        callable: &crate::libraries::LibraryCallable,
    ) {
        if name != "withLock"
            || !callable.suspend
            || !callable.owner.starts_with("kotlinx/coroutines/sync/")
        {
            return;
        }
        let Some(receiver_internal) = receiver.obj_internal() else {
            return;
        };
        let owner = Ty::obj("kotlin/Any");
        self.record_synthetic_member_call(call, receiver_internal, "lock", &[owner]);
        self.record_synthetic_member_call(call, receiver_internal, "unlock", &[owner]);
    }

    fn record_synthetic_member_call(
        &mut self,
        anchor: ExprId,
        receiver_internal: TypeName,
        name: &str,
        args: &[Ty],
    ) {
        if let Some(mut member) = self.resolve_instance_name(receiver_internal, name, args) {
            let owner = member.owner_type_or(receiver_internal);
            member.is_interface = self
                .resolved_type_name(owner)
                .is_some_and(|ty| ty.is_interface());
            self.synthetic_member_calls
                .insert((anchor, name.to_string()), member);
        }
    }

    fn record_library_extension_call_with_slots(
        &mut self,
        call: ExprId,
        name: &str,
        receiver: Ty,
        args: &[ExprId],
        type_args: &[Ty],
    ) -> Option<Ty> {
        let names = self
            .file
            .call_arg_names
            .get(&call.0)
            .cloned()
            .filter(|ns| ns.iter().any(Option::is_some))?;
        let mut candidates: Vec<_> = self
            .resolver()
            .resolve_symbol(
                crate::symbol_resolver::SymRecv::Value(receiver),
                name,
                &[],
                &[],
            )
            .map(crate::symbol_resolver::Symbol::overloads)
            .unwrap_or_default()
            .into_iter()
            .filter(crate::libraries::FunctionInfo::is_extension)
            .filter(|o| o.call_sig.has_param_names())
            .filter_map(|o| {
                let params = o.extension_value_params().to_vec();
                let slots = map_call_sig_args(args, Some(&names), &o.call_sig).ok()?;
                let mut score = self.call_slot_score(&params, &slots)?;
                if matches!(o.callable.origin, Origin::Module { .. }) {
                    score += 1_000_000;
                }
                Some((score, o, params, slots))
            })
            .collect();
        candidates.sort_by_key(|(score, _, _, _)| std::cmp::Reverse(*score));
        if candidates
            .get(1)
            .zip(candidates.first())
            .is_some_and(|((next_score, _, _, _), (best_score, _, _, _))| next_score == best_score)
        {
            return None;
        }
        let (_, fi, params, slots) = candidates.into_iter().next()?;
        let slot_tys: Vec<Option<Ty>> = slots
            .iter()
            .map(|slot| slot.map(|arg| self.expr_types[arg.0 as usize]))
            .collect();
        let c = self
            .resolver()
            .build_extension_callable_for_slots(name, receiver, type_args, &fi, &slot_tys)?;
        for (i, slot) in slots.iter().enumerate() {
            if let Some(arg) = slot {
                self.expect_assignable(
                    params[i],
                    self.expr_types[arg.0 as usize],
                    self.span(*arg),
                    "argument",
                );
            }
        }
        let ret = c.ret;
        self.resolved_calls
            .insert(call, ResolvedCall::Extension(c.clone()));
        self.record_inline_expansion_synthetic_members(call, name, receiver, &c);
        self.resolved_call_arg_slots.insert(call, slots);
        Some(ret)
    }

    /// Resolve and record the extension callable for a SYNTHESIZED operator (`componentN`/`iterator`/
    /// `plusAssign`) on `recv_ty`, keyed by the receiver expression `recv_expr` + operator name, so the
    /// lowerer reads it instead of re-resolving. No-op when no such extension exists (the construct
    /// resolves through a member / user path the lowerer reconstructs from `syms`).
    fn record_synthetic_ext(&mut self, recv_expr: ExprId, name: &str, recv_ty: Ty, arg_tys: &[Ty]) {
        if let Some(c) = self.library_extension_callable(name, recv_ty, arg_tys, &[]) {
            self.synthetic_ext_calls
                .insert((recv_expr, name.to_string()), c);
        }
    }

    fn iterator_protocol_target(&self, iterable_ty: Ty) -> Option<IteratorProtocolTarget> {
        let internal = iterable_ty.obj_internal()?;
        let iterator = if let Some(member) = self.resolve_instance_name(internal, "iterator", &[]) {
            IteratorDispatchTarget::Member {
                owner_fallback: internal,
                member: Box::new(member),
            }
        } else {
            IteratorDispatchTarget::Extension(Box::new(self.library_extension_callable(
                "iterator",
                iterable_ty,
                &[],
                &[],
            )?))
        };
        let iter_ty = iterator.ret();
        let iter_internal = iter_ty.obj_internal()?;
        let has_next = self.resolve_instance_name(iter_internal, "hasNext", &[])?;
        let next = self.resolve_instance_name(iter_internal, "next", &[])?;
        let elem_ty = iterable_ty
            .obj_internal()
            .and_then(|i| self.syms.libraries.iterable_element_type_name(i))
            .or_else(|| iter_ty.type_args().first().copied())
            .or_else(|| iterable_ty.type_args().first().copied())
            .unwrap_or_else(|| Ty::obj("kotlin/Any"));
        Some(IteratorProtocolTarget {
            iterator,
            has_next,
            next,
            iter_ty,
            elem_ty,
        })
    }

    fn record_iterator_protocol(&mut self, iterable: ExprId, iterable_ty: Ty) -> Option<Ty> {
        let target = self.iterator_protocol_target(iterable_ty)?;
        let elem = target.elem_ty;
        if matches!(target.iterator, IteratorDispatchTarget::Extension(_)) {
            if let IteratorDispatchTarget::Extension(c) = &target.iterator {
                self.synthetic_ext_calls
                    .insert((iterable, "iterator".to_string()), (**c).clone());
            }
        }
        self.iterator_protocols.insert(iterable, target);
        Some(elem)
    }

    fn module_member_target(
        &self,
        recv: Ty,
        name: &str,
        params: &[Ty],
    ) -> Option<DestructureComponentTarget> {
        let owner = recv.obj_internal()?;
        if !self.current_file_declares_class_name(owner) {
            return None;
        }
        let sig = self
            .syms
            .class_by_type_name(owner)?
            .methods_named(name)
            .iter()
            .find(|s| s.params.as_slice() == params)?;
        Some(DestructureComponentTarget::ModuleMember {
            owner,
            name: name.to_string(),
            params: sig.params.clone(),
            ret: sig.ret,
        })
    }

    fn cross_file_module_member_target(
        &self,
        recv: Ty,
        name: &str,
        params: &[Ty],
    ) -> Option<DestructureComponentTarget> {
        let owner = recv.obj_internal()?;
        let sig = self.syms.class_by_type_name(owner)?;
        let method = sig
            .methods_named(name)
            .iter()
            .find(|s| s.params.as_slice() == params)?;
        Some(DestructureComponentTarget::CrossFileModuleMember {
            owner,
            name: name.to_string(),
            params: method.params.clone(),
            ret: method.ret,
            interface: sig.is_interface,
        })
    }

    fn module_extension_target(
        &self,
        recv: Ty,
        name: &str,
        params: &[Ty],
    ) -> Option<DestructureComponentTarget> {
        let sig = self
            .syms
            .ext_fun_overloads(recv, name)
            .iter()
            .find(|sig| !sig.vararg && sig.params.as_slice() == params)?;
        Some(DestructureComponentTarget::ModuleExtension {
            receiver: recv,
            name: name.to_string(),
            params: sig.params.clone(),
            ret: sig.ret,
        })
    }

    fn destructure_component_target(
        &self,
        recv: Ty,
        name: &str,
        params: &[Ty],
    ) -> Option<DestructureComponentTarget> {
        self.module_member_target(recv, name, params)
            .or_else(|| self.cross_file_module_member_target(recv, name, params))
            .or_else(|| {
                self.resolve_instance_member(recv, name, params)
                    .map(DestructureComponentTarget::LibraryMember)
            })
            .or_else(|| {
                self.library_extension_callable(name, recv, params, &[])
                    .map(DestructureComponentTarget::LibraryExtension)
            })
            .or_else(|| self.module_extension_target(recv, name, params))
    }

    fn destructure_indexed_get_target(&self, recv: Ty) -> Option<DestructureComponentTarget> {
        self.resolve_instance_member(recv, "get", &[Ty::Int])
            .map(DestructureComponentTarget::IndexedGet)
    }

    fn module_property_getter_target(
        &self,
        recv: Ty,
        property: &str,
    ) -> Option<DestructureComponentTarget> {
        fn find(
            syms: &SymbolTable,
            internal: TypeName,
            property: &str,
        ) -> Option<(TypeName, Ty, bool)> {
            let c = syms.class_by_type_name(internal)?;
            if let Some((ty, _)) = c.prop(property) {
                return Some((internal, ty, c.is_interface));
            }
            find(syms, c.super_internal?, property)
        }

        let owner = recv.obj_internal()?;
        let (owner, ret, interface) = find(self.syms, owner, property)?;
        Some(DestructureComponentTarget::ModulePropertyGetter {
            owner,
            property: property.to_string(),
            ret,
            interface,
        })
    }

    fn current_file_declares_class_name(&self, internal: TypeName) -> bool {
        self.file.decls.iter().any(|&d| {
            matches!(
                self.file.decl(d),
                Decl::Class(c) if internal.matches(&class_internal(self.file, &c.name))
            )
        })
    }

    fn record_delegate_getvalue(&mut self, delegate: ExprId, delegate_ty: Ty) -> Option<Ty> {
        if let Some(internal) = delegate_ty.obj_internal() {
            if internal.starts_with("kotlin/reflect/") {
                return None;
            }
            if let Some(sig) = self.syms.method_of_name(internal, "getValue") {
                self.delegate_getvalue_targets.insert(
                    delegate,
                    DelegateGetValueTarget::Member {
                        owner: internal,
                        params: sig.params.clone(),
                        ret: sig.ret,
                    },
                );
                return Some(sig.ret);
            }
        }
        let callable = self.library_extension_callable(
            "getValue",
            delegate_ty,
            &[Ty::obj("kotlin/Any"), Ty::obj("kotlin/reflect/KProperty")],
            &[],
        )?;
        let ret = callable.ret;
        self.delegate_getvalue_targets.insert(
            delegate,
            DelegateGetValueTarget::Extension(Box::new(callable)),
        );
        Some(ret)
    }

    fn library_extension_callable(
        &self,
        name: &str,
        receiver: Ty,
        arg_tys: &[Ty],
        type_args: &[Ty],
    ) -> Option<crate::libraries::LibraryCallable> {
        self.resolver()
            .resolve_symbol(
                crate::symbol_resolver::SymRecv::Value(receiver),
                name,
                arg_tys,
                type_args,
            )
            .and_then(crate::symbol_resolver::Symbol::extension_call)
    }

    fn library_extension_inline_return(
        &self,
        name: &str,
        receiver: Ty,
        arg_tys: &[Ty],
    ) -> Option<Ty> {
        self.resolver()
            .resolve_symbol(
                crate::symbol_resolver::SymRecv::Value(receiver),
                name,
                arg_tys,
                &[],
            )
            .and_then(crate::symbol_resolver::Symbol::extension_call)
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
            } else if !value_types.is_empty() || self.file.expr_uses_name(body, "it") {
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
                if let Some(cs) = self.syms.class_by_type_name(internal) {
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
            } else if !param_types.is_empty() || self.file.expr_uses_name(body, "it") {
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
        let cs = self.syms.class_by_type_name(internal)?;
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
    /// Select and record the classpath/library constructor target for `call`. The checker owns overload,
    /// named/default, value-class, and marker-constructor selection; lowering only emits this target.
    fn select_library_constructor_name(
        &self,
        internal: TypeName,
        args: Vec<ExprId>,
        arg_tys: &[Ty],
    ) -> Option<ResolvedConstructor> {
        if let Some(member) = self.resolve_constructor_name(internal, arg_tys) {
            // A value-class ctor has an EMPTY descriptor (the marker krusty uses); resolve it like any
            // class — `Plain` carrying its parameter types. ir_lower emits a uniform `New` from those, and
            // the value-class JVM pass realizes `constructor-impl`. No value-class handling in the resolver.
            return Some(ResolvedConstructor::Plain { member, args });
        }
        self.resolve_synthetic_constructor_name(internal, arg_tys)
            .map(|ctor| ResolvedConstructor::Synthetic { ctor, args })
    }

    fn record_library_constructor_name(
        &mut self,
        call: ExprId,
        internal: TypeName,
        args: Vec<ExprId>,
        arg_tys: &[Ty],
    ) -> Option<ResolvedConstructor> {
        let target = self.select_library_constructor_name(internal, args, arg_tys)?;
        self.resolved_constructors.insert(call, target.clone());
        Some(target)
    }

    fn record_named_library_constructor_name(
        &mut self,
        call: ExprId,
        internal: TypeName,
        args: &[ExprId],
        arg_names: &[Option<String>],
    ) -> Result<Option<ResolvedConstructor>, String> {
        let Some(ctor_params) = self
            .resolved_type_name(internal)
            .and_then(|t| t.constructor_named_params(args.len()))
        else {
            return Ok(None);
        };
        let slots = map_param_list_args(args, Some(arg_names), &ctor_params)?;
        for &a in slots.iter().flatten() {
            self.expr(a);
        }
        if let Some(ordered) = slots.iter().copied().collect::<Option<Vec<ExprId>>>() {
            let tys: Vec<Ty> = ordered
                .iter()
                .map(|a| self.expr_types[a.0 as usize])
                .collect();
            let Some(target) = self.select_library_constructor_name(internal, ordered, &tys) else {
                return Ok(None);
            };
            let target = match target {
                ResolvedConstructor::Plain { member, .. } => {
                    ResolvedConstructor::PlainSlots { member, slots }
                }
                other => other,
            };
            self.resolved_constructors.insert(call, target.clone());
            return Ok(Some(target));
        }
        let source = self.fed_source();
        let Some((descriptor, real_params)) =
            crate::symbol_resolver::synthetic_default_ctor_name(&source, internal, slots.len())
        else {
            return Ok(None);
        };
        let mask = slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.is_none())
            .map(|(i, _)| 1i32 << i)
            .sum();
        let target = ResolvedConstructor::NamedDefault {
            descriptor,
            real_params,
            slots,
            mask,
        };
        self.resolved_constructors.insert(call, target.clone());
        Ok(Some(target))
    }

    fn record_default_member_call(
        &mut self,
        call: ExprId,
        rt: Ty,
        member: &crate::libraries::LibraryMember,
        arity: usize,
    ) {
        let Some(owner) = member
            .owner
            .map(|owner| owner.render())
            .or_else(|| rt.non_null().obj_internal().map(|n| n.render()))
        else {
            return;
        };
        let source = self.fed_source();
        let Some((descriptor, real_params, ret, suspend)) =
            crate::symbol_resolver::synthetic_default_member(&source, &owner, &member.name, arity)
        else {
            return;
        };
        self.resolved_default_member_calls.insert(
            call,
            ResolvedDefaultMemberCall {
                descriptor,
                real_params,
                ret,
                suspend,
            },
        );
    }

    fn ctor_result_name(&mut self, call: ExprId, internal: TypeName) -> Ty {
        // Cannot construct an abstract class / interface directly (kotlinc rejects it; the JVM would
        // throw at `new`). Only fires on a genuine construction call here — a `super(…)` delegation
        // and an `object : I {}` literal reach the backend by other paths, not `ctor_result`.
        if let Some(cls) = self.syms.class_by_type_name(internal) {
            if cls.is_interface || cls.is_abstract {
                let kind = if cls.is_interface {
                    "an interface"
                } else {
                    "an abstract class"
                };
                let rendered = internal.render();
                self.diags.error(
                    self.span(call),
                    format!("cannot create an instance of {kind} '{rendered}'"),
                );
            }
        }
        if let Some(targs) = self.file.call_type_args.get(&call.0).cloned() {
            let args: Vec<Ty> = targs.iter().map(|r| self.resolve_ty(r)).collect();
            if !args.is_empty() {
                return Ty::obj_args_name(internal, &args);
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
                .resolved_type_name(internal)
                .and_then(|t| crate::symbol_resolver::infer_constructor_type_args(&t, &arg_tys))
            {
                if inferred.iter().any(|t| !t.is_erased_top()) {
                    return Ty::obj_args_name(internal, &inferred);
                }
            }
        }
        Ty::obj_name(internal)
    }

    /// Whether a member of `owner` with visibility `vis` is accessible from the CURRENT site (the class
    /// being checked, `self.this_ty`), by Kotlin's rules. `public` and `internal` are always accessible
    /// (a resolved user member is in-module; cross-module `internal` is a separate, later concern).
    /// `private` reaches the declaring class and classes lexically nested inside it (an inner/nested
    /// class or the companion, whose JVM internal name is `<owner>$…`); `protected` reaches those plus
    /// any subclass of `owner`. At a top-level site (no enclosing class) a non-public member is
    /// inaccessible.
    fn member_accessible(&self, vis: Visibility, owner: TypeName) -> bool {
        match vis {
            Visibility::Public | Visibility::Internal => true,
            Visibility::Private | Visibility::Protected => {
                let Some(enc) = self.this_ty.and_then(Ty::obj_internal) else {
                    return false;
                };
                if enc == owner {
                    return true;
                }
                let nested_prefix = format!("{}$", owner.render());
                if enc.starts_with(&nested_prefix) {
                    return true;
                }
                // `protected` additionally reaches from a subclass of the owner.
                vis == Visibility::Protected
                    && self
                        .syms
                        .supertype_internal_names_from(enc)
                        .contains(&owner)
            }
        }
    }

    /// The EFFECTIVE visibility of member `name` on `receiver` and the class that declares it, walking
    /// the base-class chain. Stops at the FIRST class that declares `name` (a subclass member shadows a
    /// same-named base one), so an inherited `private` on a superclass is checked against that
    /// superclass — not silently allowed because the receiver's own map lacks it. `is_fn` selects the
    /// function vs property table. `None` when no class in the chain declares it (a builtin/absent member).
    fn effective_member_visibility(
        &self,
        receiver: TypeName,
        name: &str,
        is_fn: bool,
    ) -> Option<(Visibility, TypeName)> {
        let mut cur = Some(receiver);
        while let Some(internal) = cur {
            let cs = self.syms.class_by_type_name(internal)?;
            let declares = if is_fn {
                cs.methods.contains_key(name)
            } else {
                cs.prop(name).is_some()
            };
            if declares {
                let vis = if is_fn {
                    cs.fn_visibility.get(name)
                } else {
                    cs.prop_visibility.get(name)
                }
                .copied()
                .unwrap_or(Visibility::Public);
                return Some((vis, internal));
            }
            cur = cs.super_internal_name();
        }
        None
    }

    /// Emit kotlinc's access diagnostic when a member of `owner` with visibility `vis` is NOT reachable
    /// from the current site. Shared by the property-read and member-call checks.
    fn reject_if_inaccessible(&mut self, vis: Visibility, name: &str, owner: TypeName, span: Span) {
        if !self.member_accessible(vis, owner) {
            let kind = match vis {
                Visibility::Private => "private",
                Visibility::Protected => "protected",
                Visibility::Internal => "internal",
                Visibility::Public => "public",
            };
            self.diags.error(
                span,
                format!(
                    "cannot access '{name}': it is {kind} in '{}'",
                    owner.render()
                ),
            );
        }
    }

    /// Resolve a semantic property read `recv.name` without reporting "unresolved" on a miss. When the
    /// selected property has a backend handle (classpath/member getter or extension getter), record it on
    /// the read expression so lowering reads the checker-selected property instead of reconstructing it.
    fn resolve_property_read(
        &mut self,
        rt: Ty,
        name: &str,
        span: Span,
        mexpr: Option<ExprId>,
    ) -> Option<Ty> {
        if let Ty::Obj(internal, args) = rt {
            let internal_name = internal;
            let internal = internal_name.render();
            if let Some((ty, _)) = self.lookup_prop(&internal, name) {
                if let Some((vis, owner)) =
                    self.effective_member_visibility(internal_name, name, false)
                {
                    if vis != Visibility::Public {
                        self.reject_if_inaccessible(vis, name, owner, span);
                    }
                }
                if let Some(cs) = self.syms.class_by_internal(&internal) {
                    if let Some(&i) = cs.generic_props.get(name) {
                        if let Some(&arg) = args.get(i) {
                            return Some(arg);
                        }
                        // No recorded type argument (a raw `Obj(C, [])` receiver, e.g. a ctor call
                        // whose inference didn't capture the argument): type at the parameter's
                        // BOUND erasure when it says more than `Any`, so a primitive-bounded read
                        // computes and a class-bounded chained read keeps resolving.
                        if let Some(bound) = cs.tparam_bound_erasures.get(i) {
                            if *bound != Ty::obj("kotlin/Any") {
                                return Some(*bound);
                            }
                        }
                    }
                }
                return Some(ty);
            }
            let is_enum_val = self.syms.enums.keys().any(|en| {
                self.syms
                    .classes
                    .get(en)
                    .map_or(false, |c| c.internal_matches(&internal))
            });
            if is_enum_val {
                match name {
                    "name" => return Some(Ty::String),
                    "ordinal" => return Some(Ty::Int),
                    _ => {}
                }
            }
        }
        if let Some((ty, _)) = self.syms.ext_prop(rt, name) {
            return Some(ty);
        }
        if let Some(m) = self.resolve_property_member(rt, name) {
            let ret = m.ret;
            if let Some(me) = mexpr {
                self.resolved_calls.insert(me, ResolvedCall::Member(m));
            }
            return Some(ret);
        }
        if let Some(getter) = self
            .resolver()
            .resolve_symbol(crate::symbol_resolver::SymRecv::Value(rt), name, &[], &[])
            .and_then(crate::symbol_resolver::Symbol::extension_property_getter)
        {
            let ret = getter.ret;
            if let Some(e) = mexpr {
                self.expr_lowers.insert(
                    e,
                    ExprLowering::ExtensionPropertyGet {
                        getter: Box::new(getter),
                    },
                );
            }
            return Some(ret);
        }
        None
    }

    fn check_member(&mut self, rt: Ty, name: &str, span: Span, mexpr: Option<ExprId>) -> Ty {
        if rt == Ty::Error {
            return Ty::Error;
        }
        if let (Ty::String, "length") = (rt, name) {
            if let Some(m) = self.resolve_property_member(rt, name) {
                if let Some(me) = mexpr {
                    self.resolved_calls.insert(me, ResolvedCall::Member(m));
                }
            }
            return Ty::Int;
        }
        if let (Ty::Char, "code") = (rt, name) {
            return Ty::Int; // `c.code` — the Char's UTF-16 code unit as an `Int`.
        }
        if name == "size" && rt.array_elem().is_some() {
            // `arr.size` — covers primitive arrays and a boxed `Array<T>` (`array_elem` sees both).
            return Ty::Int;
        }
        if let Some(ty) = self.resolve_property_read(rt, name, span, mexpr) {
            return ty;
        }
        if let Some(m) = self.resolve_instance_member(rt, name, &[]) {
            if m.ret.is_read_value_result() {
                let ret = m.ret;
                if let Some(me) = mexpr {
                    self.resolved_calls.insert(me, ResolvedCall::Member(m));
                }
                return ret;
            }
        }
        if name == "java"
            && rt
                .non_null()
                .obj_internal()
                .is_some_and(|internal| internal.matches("java/lang/Class"))
        {
            if let Some(me) = mexpr {
                self.expr_lowers.insert(me, ExprLowering::ClassLiteralJava);
            }
            return rt;
        }
        if let Some(internal) = rt.non_null().obj_internal() {
            if let Some(sf) = self.syms.libraries.static_field_name(internal, name) {
                let ty = sf.ty;
                if let Some(me) = mexpr {
                    self.expr_lowers.insert(
                        me,
                        ExprLowering::ExternalStaticFieldRead {
                            owner: sf.owner,
                            name: sf.name,
                            descriptor: sf.descriptor,
                        },
                    );
                }
                return ty;
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
    fn try_member_read(
        &mut self,
        rt: Ty,
        name: &str,
        span: Span,
        mexpr: Option<ExprId>,
    ) -> Option<Ty> {
        let n = self.diags.diags.len();
        let t = self.check_member(rt, name, span, mexpr);
        if self.diags.diags.len() > n || t == Ty::Error {
            self.diags.diags.truncate(n);
            if let Some(e) = mexpr {
                self.resolved_calls.remove(&e);
                self.expr_lowers.remove(&e);
            }
            return None;
        }
        Some(t)
    }

    fn record_classpath_member_call_with_slots(
        &mut self,
        call: ExprId,
        rt: Ty,
        name: &str,
        args: &[ExprId],
    ) -> ClasspathMemberSlotCall {
        let arg_names = self.file.call_arg_names.get(&call.0).cloned();
        let needs_slot_map = arg_names.is_some();
        let mut candidates: Vec<_> = self
            .resolver()
            .resolve_symbol(crate::symbol_resolver::SymRecv::Value(rt), name, &[], &[])
            .map(crate::symbol_resolver::Symbol::overloads)
            .unwrap_or_default()
            .into_iter()
            .filter(|o| o.kind == crate::libraries::FnKind::Member)
            .filter(|o| o.call_sig.has_param_names())
            .filter_map(|o| {
                let params = o.callable.params.clone();
                let needs_slot_map = needs_slot_map || args.len() != params.len();
                if !needs_slot_map {
                    return None;
                }
                let slots = map_call_sig_args(args, arg_names.as_deref(), &o.call_sig).ok()?;
                let score = self.call_slot_score(&params, &slots)?;
                Some((score, o, params, slots))
            })
            .collect();
        candidates.sort_by_key(|(score, _, _, _)| std::cmp::Reverse(*score));
        if candidates
            .get(1)
            .zip(candidates.first())
            .is_some_and(|((next_score, _, _, _), (best_score, _, _, _))| next_score == best_score)
        {
            self.diags.error(
                self.span(call),
                format!("overload resolution ambiguity for member '{name}'"),
            );
            return ClasspathMemberSlotCall::Ambiguous;
        }
        let Some((_, fi, params, slots)) = candidates.into_iter().next() else {
            return ClasspathMemberSlotCall::NoMatch;
        };
        for (i, slot) in slots.iter().enumerate() {
            if let Some(a) = slot {
                let aty = self.expr_types[a.0 as usize];
                self.expect_assignable(params[i], aty, self.span(*a), "argument");
            }
        }
        let ret = if matches!(fi.callable.origin, Origin::Module { .. }) {
            self.inferred_member_ret(rt, &fi.callable.name, &params)
                .unwrap_or(fi.callable.ret)
        } else {
            fi.callable.ret
        };
        if matches!(fi.callable.origin, Origin::Module { .. }) {
            let interface = self
                .syms
                .class_by_type_name(fi.callable.owner)
                .is_some_and(|c| c.is_interface);
            self.resolved_calls.insert(
                call,
                ResolvedCall::ModuleMember {
                    owner: fi.callable.owner,
                    name: fi.callable.name.clone(),
                    params: params.clone(),
                    ret,
                    interface,
                },
            );
        } else {
            let member = fi.member_with_return(ret);
            if slots.iter().any(Option::is_none) {
                self.record_default_member_call(call, rt, &member, slots.len());
            }
            self.resolved_calls.insert(
                call,
                ResolvedCall::Member(crate::symbol_resolver::ResolvedMember {
                    member,
                    ret,
                    suspend: fi.flags.suspend,
                }),
            );
        }
        self.resolved_call_arg_slots.insert(call, slots);
        ClasspathMemberSlotCall::Resolved(ret)
    }

    fn call_slot_score(&self, params: &[Ty], slots: &[Option<ExprId>]) -> Option<usize> {
        if params.len() != slots.len() {
            return None;
        }
        let mut score = 0usize;
        for (i, slot) in slots.iter().enumerate() {
            let Some(arg) = slot else { continue };
            let aty = self.expr_types[arg.0 as usize];
            if !arg_assignable_simple(params[i], aty) {
                return None;
            }
            score += if params[i] == aty { 4 } else { 1 };
        }
        Some(score)
    }

    /// The classpath internal name a bare class name resolves to — an explicit import first, then the
    /// federated class-name seed (default/same-package/wildcard imports). Used to reach a classpath type's
    /// `@Metadata` (e.g. constructor parameter names) from a simple-name constructor call.
    fn classpath_class_internal_name(&self, name: &str) -> Option<TypeName> {
        // Resolve through the same NESTED-type rewrite the positional-construction path uses: an
        // unqualified nested-type import (`import lib.Op.Apply`) stores the flat `lib/Op/Apply`, but the
        // class is `lib/Op$Apply`. `imported_type_internal` applies the `/`→`$` recovery (and wildcard
        // imports), so a NAMED constructor call on such an import (`Apply(a = 1)`) resolves its ctor
        // parameter names the same way the positional form already resolves the class.
        self.imported_type_name(name)
            .or_else(|| {
                self.imports
                    .get(name)
                    .and_then(|i| self.nested_internal_name(i))
            })
            .or_else(|| self.syms.class_names.get(name))
    }

    /// If `name` is imported from a classpath `object` (`import a.b.Obj.member` → `imports[member] =
    /// a/b/Obj/member`), the object's internal name — so an unqualified call `member(args)` can dispatch
    /// on the singleton. `None` unless the import's owner path resolves to an object carrying a member of
    /// this name (the owner-value lowering, `getstatic Obj.INSTANCE`, requires a plain object INSTANCE).
    fn object_member_import(&self, name: &str) -> Option<TypeName> {
        let full = self.imports.get(name)?;
        let (owner_path, member) = full.rsplit_once('/')?;
        if member != name {
            return None;
        }
        // A SAME-FILE object with a member of this name — dispatch on its singleton exactly like a
        // classpath object (`getstatic Obj.INSTANCE; invoke`).
        if self.syms.objects.contains(owner_path) {
            if let Some(cls) = self.syms.classes.get(owner_path) {
                if cls.methods.contains_key(name) {
                    return Some(cls.internal_name());
                }
            }
        }
        let owner = self.nested_internal_name(owner_path)?;
        self.resolved_type_name(owner)
            .filter(|t| t.is_object())
            .map(|_| owner)
    }

    /// Primary-constructor parameter names/defaults of a same-file class, in declaration order — for
    /// mapping named constructor arguments. `None` if `class_name` isn't a same-file primary constructor.
    fn primary_ctor_param_list(&self, class_name: &str) -> Option<ParamList> {
        self.file
            .decls
            .iter()
            .find_map(|&d| match self.file.decl(d) {
                Decl::Class(c) if c.name == class_name && c.has_primary_ctor => Some(ParamList {
                    names: c.props.iter().map(|p| p.name.clone()).collect(),
                    defaults: c.props.iter().map(|p| p.default.is_some()).collect(),
                }),
                _ => None,
            })
            // Declared in ANOTHER FILE of the same module: the AST is out of reach, but the resolver
            // already recorded the names on the class signature.
            .or_else(|| {
                let sig = self.syms.classes.get(class_name)?;
                (!sig.ctor_param_names.is_empty()).then(|| ParamList {
                    names: sig
                        .ctor_param_names
                        .iter()
                        .map(|(n, _)| n.clone())
                        .collect(),
                    defaults: sig.ctor_param_names.iter().map(|(_, d)| *d).collect(),
                })
            })
    }

    /// The source qname `Outer.Inner` for a same-file nested class.
    fn same_file_nested_class_qname(&self, receiver: ExprId, name: &str) -> Option<String> {
        let Expr::Name(root) = self.file.expr(receiver) else {
            return None;
        };
        if self.value_root_shadows_classifier(root) {
            return None;
        }
        let qname = format!("{root}.{name}");
        self.syms.classes.contains_key(&qname).then_some(qname)
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
                        || crate::libraries::FunctionSet { overloads: self
                            .resolver().resolve_symbol(crate::symbol_resolver::SymRecv::TopLevel, n, &[], &[]).map(crate::symbol_resolver::Symbol::overloads).unwrap_or_default() }
                            .top_level()
                            .any(|o| o.call_sig.has_param_names())
                        || self.this_ty.is_some_and(|rt| {
                            self.resolver()
                                .resolve_symbol(
                                    crate::symbol_resolver::SymRecv::Value(rt),
                                    n,
                                    &[],
                                    &[],
                                )
                                .map(crate::symbol_resolver::Symbol::overloads)
                                .unwrap_or_default()
                                .iter()
                                .any(|o| {
                                    matches!(
                                        o.kind,
                                        crate::libraries::FnKind::Member
                                            | crate::libraries::FnKind::Extension
                                    )
                                        && o.call_sig.has_param_names()
                                })
                        })
                        // A CLASSPATH CONSTRUCTOR whose `@Metadata` records parameter names
                        // (`Point(y = 2, x = 1)`, or `Cfg(a = 1, c = "x")` omitting a defaulted `b`,
                        // against a data/plain class from a dependency). `constructor_named_params` returns
                        // the FULL parameter list for a ctor with at least `args.len()` params, so an
                        // omitted-default named call is still recognized.
                        || self
                            .classpath_class_internal_name(n)
                            .and_then(|i| self.resolved_type_name(i))
                            .and_then(|t| t.constructor_named_params(args.len()))
                            .is_some()
                }
                Expr::Member { receiver, name }
                    if self.same_file_nested_class_qname(*receiver, name).is_some() =>
                {
                    self.same_file_nested_class_qname(*receiver, name)
                        .and_then(|q| self.primary_ctor_param_list(&q))
                        .is_some()
                }
                Expr::Member { receiver, name }
                    if self
                        .qualified_nested_ctor_internal_name(*receiver, name)
                        .is_some() =>
                {
                    // Classpath nested-class constructor with named args.
                    self.qualified_nested_ctor_internal_name(*receiver, name)
                        .and_then(|i| self.resolved_type_name(i))
                        .and_then(|t| t.constructor_named_params(args.len()))
                        .is_some()
                }
                Expr::Member { receiver, name } => {
                    // A method with default parameters (e.g. data-class `copy`) — `required < params` —
                    // queried through the module source.
                    let rt = self.expr(*receiver);
                    // A member with recorded parameter names supports named arguments: one with defaults
                    // (`required < params`, e.g. data-class `copy`) maps labels + fills omitted slots; one
                    // with all-required parameters (a plain method) reorders the labelled arguments onto
                    // positions (the lowerer evaluates the receiver + args in source order). Members and
                    // extensions (module + classpath) both resolve through the federated resolver.
                    self.resolver()
                        .resolve_symbol(crate::symbol_resolver::SymRecv::Value(rt), name, &[], &[])
                        .map(crate::symbol_resolver::Symbol::overloads)
                        .unwrap_or_default()
                        .iter()
                        .any(|o| o.call_sig.has_param_names())
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
                    if !self.value_root_shadows_classifier(&root) {
                        if let Some(internal) = qualified_path(self.file, callee) {
                            if let Some(members) = self
                                .resolved_type(&internal)
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
                    if !self.value_root_shadows_classifier(&root) {
                        if let Some(pkg) = qualified_path(self.file, receiver) {
                            let arg_tys = self.arg_tys(args);
                            let targs: Vec<Ty> = self
                                .file
                                .call_type_args
                                .get(&call.0)
                                .map(|ts| ts.iter().map(|r| self.resolve_ty(r)).collect())
                                .unwrap_or_default();
                            let pkg_scope = [type_name(&pkg)];
                            if let Some(c) = self
                                .resolver_in_scope(&pkg_scope)
                                .resolve_symbol(
                                    crate::symbol_resolver::SymRecv::TopLevel,
                                    &name,
                                    &arg_tys,
                                    &targs,
                                )
                                .and_then(crate::symbol_resolver::Symbol::top_level_call)
                            {
                                if c.owner_package_matches_name(pkg_scope[0]) {
                                    crate::trace_compiler!(
                                        "resolve",
                                        "fully-qualified top-level call {pkg}.{name} -> {}",
                                        c.owner.render()
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
                                    // Record for the lowerer (sole resolver): a FQ top-level call.
                                    let ret = c.ret;
                                    self.resolved_calls.insert(call, ResolvedCall::TopLevel(c));
                                    return ret;
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
                                let shape = self.top_level_lambda_shape(&name, &partial);
                                if let Some(pt) = shape
                                    .as_ref()
                                    .and_then(|s| s.param_types.as_ref())
                                    .and_then(|p| p.get(last))
                                    .cloned()
                                {
                                    // A RECEIVER function-type block parameter (`CoroutineScope.() -> T`):
                                    // `pt[0]` is the receiver bound as the lambda's `this`, `pt[1..]` its
                                    // value params — matching the bare-name path's `lambda_param_types` use.
                                    let recv = shape
                                        .as_ref()
                                        .and_then(|s| s.receivers.as_ref())
                                        .and_then(|r| r.get(last).copied().flatten());
                                    let lam_ty = if let Some(recv) = recv {
                                        self.check_lambda_with_receiver_labeled(
                                            args[last],
                                            recv,
                                            if !pt.is_empty() { &pt[1..] } else { &[] },
                                            None,
                                        )
                                    } else {
                                        self.check_lambda_with_types(args[last], &pt)
                                    };
                                    let mut full = arg_tys.clone();
                                    full[last] = lam_ty;
                                    let pkg_scope = [type_name(&pkg)];
                                    if let Some(c) = self
                                        .resolver_in_scope(&pkg_scope)
                                        .resolve_symbol(
                                            crate::symbol_resolver::SymRecv::TopLevel,
                                            &name,
                                            &full,
                                            &targs,
                                        )
                                        .and_then(crate::symbol_resolver::Symbol::top_level_call)
                                    {
                                        if c.owner_package_matches_name(pkg_scope[0]) {
                                            crate::trace_compiler!(
                                                "resolve",
                                                "fully-qualified trailing-lambda call {pkg}.{name} -> {}",
                                                c.owner.render()
                                            );
                                            // Record the resolved callable so the lowerer emits it (the
                                            // non-trailing-lambda FQ path above records the same way).
                                            let ret = c.ret;
                                            self.resolved_calls
                                                .insert(call, ResolvedCall::TopLevel(c));
                                            return ret;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // Nested-class construction `Outer.Inner(args)`.
                if let Some(qname) = self.same_file_nested_class_qname(receiver, &name) {
                    if let Some(cls) = self.syms.classes.get(&qname).cloned() {
                        if arg_names.is_some() {
                            if let Some(param_list) = self.primary_ctor_param_list(&qname) {
                                match map_param_list_args(args, arg_names.as_deref(), &param_list) {
                                    Ok(slots) => {
                                        for &a in slots.iter().flatten() {
                                            self.expr(a);
                                        }
                                        for (i, slot) in slots.iter().enumerate() {
                                            if let (Some(a), Some(pt)) =
                                                (slot, cls.ctor_params.get(i))
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
                                    Err(msg) => self
                                        .diags
                                        .error(span, format!("constructor '{qname}': {msg}")),
                                }
                                return self.ctor_result_name(call, cls.internal_name());
                            }
                        }
                        let arg_tys = self.arg_tys(args);
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
                                self.expect_call_args(&ps, false, args, &arg_tys);
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
                        return self.ctor_result_name(call, cls.internal_name());
                    }
                }
                // `EnumName.values()` / `EnumName.valueOf(s)` — synthetic static enum methods.
                if let Expr::Name(en) = self.file.expr(receiver).clone() {
                    if !self.value_root_shadows_classifier(&en) && self.syms.enums.contains_key(&en)
                    {
                        let internal = self
                            .syms
                            .classes
                            .get(&en)
                            .map(ClassSig::internal)
                            .unwrap_or(en.clone());
                        if name == "values" && args.is_empty() {
                            return Ty::array(Ty::obj(&internal));
                        }
                        if let ("valueOf", [arg]) = (name.as_str(), args) {
                            let at = self.expr(*arg);
                            self.expect_assignable(Ty::String, at, self.span(*arg), "argument");
                            return Ty::obj(&internal);
                        }
                    }
                }
                // Nested-class constructor `Outer.Inner(args)` (when `Outer` isn't a local).
                if let Expr::Name(outer) = self.file.expr(receiver).clone() {
                    if !self.value_root_shadows_classifier(&outer) {
                        let qualified = format!("{outer}.{name}");
                        if let Some(cls) = self.syms.classes.get(&qualified).cloned() {
                            let arg_tys = self.arg_tys(args);
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
                                self.expect_call_args(&cls.ctor_params, false, args, &arg_tys);
                            }
                            return Ty::obj(&cls.internal());
                        }
                    }
                }
                // CLASSPATH nested-class constructor `Outer.Nested(args)` (a sealed subclass
                // `Subject.User("x")`, or any nested class), OR a FULLY-QUALIFIED constructor via a package
                // path `a.b.Ctx(args)`: resolve the whole qualifier to an `Outer$Nested` / `a/b/Ctx`
                // classpath internal and match a constructor. Lowering emits `new …; invokespecial`.
                {
                    if let Some(internal) =
                        self.qualified_nested_ctor_internal_name(receiver, &name)
                    {
                        let qualified = internal.render();
                        // Named classpath constructors use metadata names/defaults; lowering selects
                        // either the plain constructor or the default-argument synthetic.
                        if let Some(names) = arg_names.as_deref() {
                            match self
                                .record_named_library_constructor_name(call, internal, args, names)
                            {
                                Ok(Some(_)) => return Ty::obj_name(internal),
                                Ok(None) => {}
                                Err(msg) => {
                                    self.diags
                                        .error(span, format!("constructor '{qualified}': {msg}"));
                                    return Ty::Error;
                                }
                            }
                        }
                        let arg_tys = self.arg_tys(args);
                        // POSITIONAL — a plain constructor, a value-class-param/omitted-default
                        // synthetic. Type-check the provided
                        // arguments against the plain constructor's params when it matches.
                        if let Some(target) = self.record_library_constructor_name(
                            call,
                            internal,
                            args.to_vec(),
                            &arg_tys,
                        ) {
                            crate::trace_compiler!(
                                "resolve",
                                "classpath nested constructor {qualified} -> {internal}"
                            );
                            if let ResolvedConstructor::Plain { member, .. } = target {
                                self.expect_call_args(&member.params, false, args, &arg_tys);
                            }
                            return Ty::obj_name(internal);
                        }
                    }
                }
                // Classpath value-class COMPANION call `Result.success(args)`: `Result` is a classpath
                // value class whose companion declares `success` (an `inline` fn — private in bytecode,
                // public per `@Metadata`). Resolve metadata-first; lowering emits the companion `getstatic`
                // receiver + an inline-splice of the companion method.
                if let Expr::Name(root) = self.file.expr(receiver).clone() {
                    if !self.value_root_shadows_classifier(&root) {
                        if let Some(internal) = self.imported_type_name(&root) {
                            if let Some(cf) = self.resolved_type_name(internal).and_then(|t| {
                                t.value_companion_fns
                                    .iter()
                                    .find(|cf| {
                                        cf.callable.name == name
                                            && cf.callable.params.len() == args.len()
                                    })
                                    .cloned()
                            }) {
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
                // `recv.run {…}` / `recv.apply {…}`: body receiver is `recv`; result is body/receiver.
                if let ("run" | "apply", [arg]) = (name.as_str(), args) {
                    if let Expr::Lambda { params, body } = self.file.expr(*arg).clone() {
                        if params.is_empty() {
                            let rt = self.expr(receiver);
                            let bt =
                                self.check_with_receiver_labeled(rt, body, call_fn_name.as_deref());
                            let returns_receiver = name == "apply";
                            self.mark_receiver_lambda_call(call, receiver, body, returns_receiver);
                            return self.set(call, if returns_receiver { rt } else { bt });
                        }
                    }
                }
                // `super.method(args)` / `super<T>.method(args)` — dispatch to a supertype's method
                // (non-virtual). A `super<T>` qualifier (encoded on the name) selects that supertype.
                let super_qual: Option<Option<String>> = match self.file.expr(receiver) {
                    Expr::Name(r) if r.starts_with("super") => {
                        // The optional `<T>` type qualifier (a `@label` may follow it, ignored here).
                        let ty = r
                            .strip_prefix("super<")
                            .and_then(|s| s.split('>').next())
                            .map(|s| s.to_string());
                        Some(ty)
                    }
                    _ => None,
                };
                if let Some(super_ty) = super_qual {
                    // A LABELED super (`super@A.f()`) selects an ENCLOSING class's supertype — not
                    // modeled; reject so the file skips rather than dispatching to the wrong receiver.
                    if matches!(self.file.expr(receiver), Expr::Name(r) if r.contains('@')) {
                        self.diags.error(
                            span,
                            format!("krusty: labeled super '{name}' is not supported"),
                        );
                        return Ty::Error;
                    }
                    let arg_tys = self.arg_tys(args);
                    // A `super<T>` qualifier matches a supertype by its simple name.
                    let matches_qual = |internal: TypeName| {
                        super_ty
                            .as_deref()
                            .is_none_or(|t| internal.qualifier_matches(t))
                    };
                    if let Some(Ty::Obj(internal, _)) = self.this_ty {
                        let sup = self
                            .syms
                            .class_by_type_name(internal)
                            .and_then(|c| c.super_internal);
                        if let Some(sup) = sup.filter(|s| matches_qual(*s)) {
                            // A user base-class method.
                            if let Some(sig) = self
                                .syms
                                .method_of_name(sup, &name)
                                .filter(|_| !self.class_method_is_abstract_name(sup, &name))
                            {
                                self.expect_call_args(&sig.params, false, args, &arg_tys);
                                self.resolved_super_calls.insert(
                                    call,
                                    ResolvedSuperCall {
                                        owner: sup,
                                        interface: false,
                                        params: sig.params.clone(),
                                        ret: sig.ret,
                                        descriptor: None,
                                    },
                                );
                                return sig.ret;
                            }
                            // A classpath base-class method (`class C : ArrayList<…>() { … super.add(x) }`).
                            if let Some(m) = self.resolve_instance_name(sup, &name, &arg_tys) {
                                self.resolved_super_calls.insert(
                                    call,
                                    ResolvedSuperCall {
                                        owner: sup,
                                        interface: false,
                                        params: m.params.clone(),
                                        ret: m.ret,
                                        descriptor: Some(m.descriptor.clone()),
                                    },
                                );
                                return m.ret;
                            }
                        }
                        // An INTERFACE DEFAULT method: a class `C : I` with `super.foo()` dispatches to
                        // `I`'s default. Resolve across the superinterfaces matching the `super<T>`
                        // qualifier — with none, EXACTLY ONE must provide the method (matching the
                        // lowerer); more than one needs the explicit `super<T>.foo()` krusty now honors.
                        let matches: Vec<(TypeName, Signature)> = self
                            .syms
                            .class_by_type_name(internal)
                            .into_iter()
                            .flat_map(|c| c.interfaces.iter_ids())
                            .filter(|iface| matches_qual(*iface))
                            .filter_map(|iface| {
                                self.syms
                                    .method_of_name(iface, &name)
                                    .map(|sig| (iface, sig.clone()))
                            })
                            .collect();
                        if let [(iface, sig)] = matches.as_slice() {
                            self.expect_call_args(&sig.params, false, args, &arg_tys);
                            self.resolved_super_calls.insert(
                                call,
                                ResolvedSuperCall {
                                    owner: *iface,
                                    interface: true,
                                    params: sig.params.clone(),
                                    ret: sig.ret,
                                    descriptor: None,
                                },
                            );
                            return sig.ret;
                        }
                    }
                    self.diags
                        .error(span, format!("krusty: unresolved super method '{name}'"));
                    return Ty::Error;
                }
                // Fully-qualified static call `pkg.Type.method(args)`.
                if let Expr::Member { .. } = self.file.expr(receiver) {
                    if let Some(fq) = qualified_path(self.file, receiver) {
                        let leftmost = fq.split('/').next().unwrap_or("");
                        if self.lookup(leftmost).is_none() && self.resolved_type(&fq).is_some() {
                            let arg_tys = self.arg_tys(args);
                            if let Some(m) = self.resolve_companion(&fq, &name, &arg_tys) {
                                self.set(receiver, Ty::obj(&fq));
                                let ret = m.ret;
                                self.resolved_calls.insert(call, ResolvedCall::Companion(m));
                                return ret;
                            }
                        }
                    }
                }
                // Java static call: `ClassName.method(args)` where ClassName is an imported class
                // (not a local/param) resolvable on the classpath. A top-level PROPERTY of the same name
                // shadows the type/import in value position (`private val logger = logger {}; logger.info()`
                // — `logger` is the KLogger value, not the imported `logger` symbol), so skip the static
                // path and let the receiver resolve as that property value below.
                if let Expr::Name(cls) = self.file.expr(receiver).clone() {
                    if !self.value_root_shadows_classifier(&cls) {
                        // `ClassName.fn(args)` — a companion (static) method call.
                        if let Some(sig) = self
                            .syms
                            .classes
                            .get(&cls)
                            .and_then(|c| c.static_methods.get(&name))
                            .cloned()
                        {
                            let arg_tys = self.arg_tys(args);
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
                                self.expect_call_args(&sig.params, false, args, &arg_tys);
                            }
                            if let Some(comp_internal) =
                                self.file
                                    .decls
                                    .iter()
                                    .find_map(|&d| match self.file.decl(d) {
                                        Decl::Class(c)
                                            if c.name == cls
                                                && c.companion_methods
                                                    .iter()
                                                    .any(|m| m.name == name) =>
                                        {
                                            Some(format!(
                                                "{}$Companion",
                                                class_internal(self.file, &c.name)
                                            ))
                                        }
                                        _ => None,
                                    })
                            {
                                self.expr_lowers.insert(
                                    call,
                                    ExprLowering::ObjectMemberCall {
                                        internal: type_name(&comp_internal),
                                    },
                                );
                            }
                            return sig.ret;
                        }
                        // `Object.member(args)` — a singleton member call.
                        if self.syms.objects.contains(&cls) {
                            let arg_tys = self.arg_tys(args);
                            let internal = self
                                .syms
                                .classes
                                .get(&cls)
                                .map(ClassSig::internal_name)
                                .unwrap_or_else(|| type_name(&class_internal(self.file, &cls)));
                            return match self
                                .syms
                                .classes
                                .get(&cls)
                                .and_then(|c| c.method_matching(&name, &arg_tys))
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
                                    self.expect_call_args(&sig.params, false, args, &arg_tys);
                                    self.expr_lowers
                                        .insert(call, ExprLowering::ObjectMemberCall { internal });
                                    sig.ret
                                }
                                None => {
                                    self.diags
                                        .error(span, format!("unresolved reference '{name}'."));
                                    Ty::Error
                                }
                            };
                        }
                        // An explicit import maps the name directly; otherwise resolve through the
                        // import levels (same-package — including the ROOT package for a classpath
                        // class compiled without one, e.g. a javac'd test source — then wildcards and
                        // defaults), so `J.greet()` on a same-package Java class resolves exactly like
                        // a type-position `J`.
                        let receiver_class = self
                            .imports
                            .get(&cls)
                            .cloned()
                            .or_else(|| self.imported_type_internal(&cls));
                        if let Some(internal) = receiver_class {
                            let arg_tys = self.arg_tys(args);
                            return match self.resolve_companion(&internal, &name, &arg_tys) {
                                Some(m) => {
                                    // Type the class-name receiver as its own type so the LOWERING emits
                                    // the static call (`invokestatic <internal>.name`) via the classpath
                                    // static path — a Java class's static method (`Logf.make(x)`) or a
                                    // Kotlin `@JvmStatic`/companion static.
                                    self.set(receiver, Ty::obj(&internal));
                                    let ret = m.ret;
                                    self.resolved_calls.insert(call, ResolvedCall::Companion(m));
                                    ret
                                }
                                // Not a companion STATIC — try a companion-object INSTANCE method
                                // (`Json.encodeToString(…)`/`Random.nextInt(…)` = `<Class>.Default.m(…)`):
                                // resolve `m` as an instance method on the companion's type.
                                None => {
                                    let inst = self
                                        .resolved_type(&internal)
                                        .and_then(|lt| lt.companion_object)
                                        .and_then(|(_, cty)| {
                                            self.resolve_instance_member(
                                                Ty::obj_name(cty),
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
                                            self.set(receiver, Ty::obj_name(cty));
                                            // A generic member whose return ERASED to `Any`
                                            // (`Json.decodeFromString(KSerializer<Foo>, String): T`)
                                            // recovers its substituted return (`Foo`) from the arguments.
                                            // Only when erased — a concrete return (`encodeToString: String`)
                                            // keeps the canonical `m.ret` (the recovered form would be a
                                            // non-canonical `Obj("kotlin/String")`).
                                            let ret = if m.member.physical_ret.is_erased_top() {
                                                // A reified member (`<T> T decodeFromString(…)`) called
                                                // with an explicit type argument (`decodeFromString<C>`)
                                                // returns that type; else recover it from the arguments.
                                                self.reified_type_arg(call).unwrap_or(m.ret)
                                            } else {
                                                m.ret
                                            };
                                            self.resolved_calls
                                                .insert(call, ResolvedCall::Member(m));
                                            ret
                                        }
                                        None => {
                                            // A classpath `object` INSTANCE member (`Ids.generate()`,
                                            // `L.logger { }`): not a companion/static — dispatch on the
                                            // object singleton. Type the receiver as the object's own
                                            // type and record the singleton read so LOWERING emits
                                            // `getstatic <internal>.INSTANCE; invokevirtual`.
                                            let is_object = self
                                                .resolved_type(&internal)
                                                .is_some_and(|t| t.is_object());
                                            if is_object {
                                                if let Some(m) = self.resolve_instance_member(
                                                    Ty::obj(&internal),
                                                    &name,
                                                    &arg_tys,
                                                ) {
                                                    crate::trace_compiler!(
                                                        "resolve",
                                                        "classpath object instance member {cls}.{name} on {internal}"
                                                    );
                                                    self.set(receiver, Ty::obj(&internal));
                                                    self.expr_lowers.insert(
                                                        receiver,
                                                        ExprLowering::ObjectValue {
                                                            internal: type_name(&internal),
                                                        },
                                                    );
                                                    let ret = m.ret;
                                                    self.resolved_calls
                                                        .insert(call, ResolvedCall::Member(m));
                                                    return ret;
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
                // A MODULE (user-declared) class method only: a classpath receiver leaves this `None` so
                // the extension lambda-param path below runs (else a Java member such as
                // `Iterable.forEach(Consumer)` would suppress the Kotlin `forEach` extension).
                let method_sig: Option<crate::libraries::LibraryMember> =
                    crate::module_symbols::ModuleSymbols::new(self.syms)
                        .instance_members(rt, &name)
                        .into_iter()
                        .next();
                // A generic higher-order member (`box.map { it.length }` where `box: Box<String>`):
                // substitute the receiver's type arguments into the lambda parameter types (so `it`
                // types as `String`/`Int`, not the erased `Any`) and remember the plan to infer the
                // method's own `<R>` from the lambda body — the call's result type — after the args type.
                let generic_member: Option<GenericMemberPlan> = self.plan_generic_member(rt, &name);
                crate::trace_compiler!(
                    "resolve",
                    "MCALL name={name} rt={rt:?} nargs={} generic_member={}",
                    args.len(),
                    generic_member.is_some()
                );
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
                let ext_lambda_pts: Option<Vec<Vec<Ty>>> = ext_lambda_partial
                    .as_ref()
                    .and_then(|partial| self.extension_lambda_param_types(rt, &name, partial));
                let ext_lambda_recvs: Option<Vec<Option<Ty>>> = ext_lambda_partial
                    .as_ref()
                    .and_then(|partial| self.extension_lambda_receivers(rt, &name, partial));
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
                    if let Some(fi) = self
                        .resolver()
                        .resolve_symbol(crate::symbol_resolver::SymRecv::Value(rt), &name, &[], &[])
                        .map(crate::symbol_resolver::Symbol::overloads)
                        .unwrap_or_default()
                        .into_iter()
                        .find(|o| o.is_extension() && o.receiver_rank == 0)
                    {
                        if has_lam(&fi.call_sig.lambda_param_types) {
                            return Some(fi.call_sig.lambda_param_types);
                        }
                    }
                    let fi = self
                        .resolver()
                        .resolve_symbol(crate::symbol_resolver::SymRecv::Value(rt), &name, &[], &[])
                        .map(crate::symbol_resolver::Symbol::overloads)
                        .unwrap_or_default()
                        .into_iter()
                        .find(|o| o.is_extension() && o.receiver_rank == 1)?;
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
                    Some(
                        fi.call_sig
                            .lambda_param_types
                            .iter()
                            .map(|v| {
                                v.iter()
                                    .map(|t| {
                                        if recv_tp.is_some() && t.is_erased_top() {
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
                    let params = self.lambda_return_overload_param_types(rt, &name)?;
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
                // Inline extensions splice lambdas, so captured mutable locals stay direct for this call.
                let allow_lambda_mutation = ext_lambda_pts.is_some()
                    && self
                        .resolver()
                        .resolve_symbol(crate::symbol_resolver::SymRecv::Value(rt), &name, &[], &[])
                        .map(crate::symbol_resolver::Symbol::overloads)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(crate::libraries::FunctionInfo::is_extension)
                        .collect::<Vec<_>>()
                        .iter()
                        .any(|o| o.flags.inline.can_inline());
                let arg_tys: Vec<Ty> = self.with_lambda_mutation(allow_lambda_mutation, |c| {
                    args.iter()
                        .enumerate()
                        .map(|(i, &a)| {
                            // A generic member's substituted lambda parameter types (`it: String`) take
                            // precedence over the method signature's erased ones (`it: Any`).
                            if let Some((_, _, ref lpt)) = generic_member {
                                if lpt.get(i).is_some_and(|v| !v.is_empty())
                                    && matches!(c.file.expr(a), Expr::Lambda { .. })
                                {
                                    let pt = lpt[i].clone();
                                    return c.check_lambda_with_types(a, &pt);
                                }
                            }
                            if let Some(ref sig) = method_sig {
                                let lpt = &sig.call_sig.lambda_param_types;
                                if i < lpt.len()
                                    && !lpt[i].is_empty()
                                    && matches!(c.file.expr(a), Expr::Lambda { .. })
                                {
                                    let pt = lpt[i].clone();
                                    return c.check_lambda_with_types(a, &pt);
                                }
                            }
                            if let Some(ref pts) = ext_lambda_pts {
                                if pts.get(i).map_or(false, |v| !v.is_empty())
                                    && matches!(c.file.expr(a), Expr::Lambda { .. })
                                {
                                    if let Some(recv) = ext_lambda_recvs
                                        .as_ref()
                                        .and_then(|recvs| recvs.get(i))
                                        .copied()
                                        .flatten()
                                    {
                                        let pt = &pts[i];
                                        let value_types = pt.get(1..).unwrap_or(&[]);
                                        return c.check_lambda_with_receiver_labeled(
                                            a,
                                            recv,
                                            value_types,
                                            call_fn_name.as_deref(),
                                        );
                                    }
                                    let pt = pts[i].clone();
                                    return c.check_lambda_with_types(a, &pt);
                                }
                            }
                            c.expr(a)
                        })
                        .collect()
                });
                if rt == Ty::Error {
                    return Ty::Error;
                }
                if let ("toString", []) = (name.as_str(), arg_tys.as_slice()) {
                    // A function value's `toString()` is Kotlin's structured `(T) -> T` form, not the
                    // JVM default — krusty can't reproduce it, so reject rather than emit the wrong text.
                    if matches!(rt, Ty::Fun(_)) {
                        self.diags.error(
                            self.span(call),
                            "krusty: toString() on a function value is not supported".to_string(),
                        );
                        return Ty::Error;
                    }
                    return Ty::String; // intrinsic on any type
                }
                match self.record_classpath_member_call_with_slots(call, rt, &name, args) {
                    ClasspathMemberSlotCall::Resolved(ret) => return ret,
                    ClasspathMemberSlotCall::Ambiguous => return Ty::Error,
                    ClasspathMemberSlotCall::NoMatch => {}
                }
                if rt == Ty::String {
                    if let Some(m) = self.resolve_instance_member(rt, &name, &arg_tys) {
                        let ret = m.ret;
                        self.resolved_calls.insert(call, ResolvedCall::Member(m));
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
                if matches!(
                    (rt, name.as_str(), arg_tys.as_slice()),
                    (Ty::Char, "compareTo", [Ty::Char])
                ) {
                    return Ty::Int;
                }
                if let Some(m) = self.resolve_instance_member(rt, &name, &arg_tys) {
                    crate::trace_compiler!(
                        "resolve",
                        "RIM-9451 name={name} rt={rt:?} -> ret={:?}",
                        m.ret
                    );
                    // Record the resolved member so the lowerer emits it directly rather than
                    // re-resolving the same call (see [`TypeInfo::resolved_members`]).
                    let ret = m.ret;
                    self.resolved_calls.insert(call, ResolvedCall::Member(m));
                    return ret;
                }
                // A CLASSPATH member call with NAMED or OMITTED defaulted arguments
                // (`workspace.copy(owner = o)` on a classpath data class): the argument-fit
                // resolution above can't match (1 supplied arg vs 7 parameters). Map the labels
                // onto positions via the `@Metadata` parameter names + default flags, type-check
                // each SUPPLIED argument against its mapped parameter, and leave emission to the
                // lowerer's `name$default` synthetic path (`lower_library_default_member_call`).
                {
                    use crate::symbol_resolver::{SymRecv, Symbol};
                    let member = self
                        .resolver()
                        .resolve_symbol(SymRecv::Value(rt), &name, &[], &[])
                        .map(Symbol::overloads)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|o| o.kind == crate::libraries::FnKind::Member)
                        .filter(|o| matches!(o.callable.origin, Origin::Library))
                        .find(|o| {
                            (arg_names.is_some() || arg_tys.len() != o.callable.params.len())
                                && o.call_sig.can_map_omitted_args(o.callable.params.len())
                        });
                    if let Some(fi) = member {
                        if let Ok(slots) =
                            map_call_sig_args(args, arg_names.as_deref(), &fi.call_sig)
                        {
                            let logical = &fi.callable.params;
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
                            // Record the resolved member AND the argument→slot mapping so the lowerer
                            // emits this through the member's `name$default` synthetic — without the
                            // slot record it would see fewer args than parameters and bail.
                            let ret = fi.callable.ret;
                            let member = fi.member_with_return(ret);
                            if slots.iter().any(Option::is_none) {
                                self.record_default_member_call(call, rt, &member, slots.len());
                            }
                            self.resolved_calls.insert(
                                call,
                                ResolvedCall::Member(crate::symbol_resolver::ResolvedMember {
                                    member,
                                    ret,
                                    suspend: fi.flags.suspend,
                                }),
                            );
                            self.resolved_call_arg_slots.insert(call, slots);
                            return ret;
                        }
                    }
                }
                // Instance method call on a class value: `p.method(args)` (own or inherited).
                if let Ty::Obj(internal_name, _) = rt {
                    // A non-public member FUNCTION may be inaccessible from this site — kotlinc rejects it;
                    // surface the same diagnostic rather than silently compiling an illegal call.
                    if let Some((vis, owner)) =
                        self.effective_member_visibility(internal_name, &name, true)
                    {
                        if vis != Visibility::Public {
                            self.reject_if_inaccessible(vis, &name, owner, span);
                        }
                    }
                    // The user member resolved through the current module as a `SymbolSource`
                    // (`ModuleSymbols`); its DFS member walk matches `lookup_method`, so the first Member
                    // overload is the same one hand-rolled lookup would pick. Collected owned so the
                    // borrow of `syms` ends before the mutating type-checks below.
                    // MODULE (user) class members only: a classpath receiver already resolved through
                    // `resolve_instance_member` above (by argument fit); querying the federated members here
                    // would arity-bind a Java member (`Iterable.forEach(Consumer)`) and reject the Kotlin
                    // lambda, shadowing the extension the classpath selectors pick.
                    let members = pick_member_overloads(
                        crate::module_symbols::ModuleSymbols::new(self.syms)
                            .instance_members(rt, &name),
                        &arg_tys,
                        arg_names.is_some(),
                    );
                    // The first Member overload is the most-derived override (for dispatch/return). For an
                    // OMITTED-argument call, the default may be declared on a SUPERTYPE (an interface
                    // method's default isn't redeclared on the override) — prefer an overload that records
                    // defaults so the omitted args resolve, falling back to the override.
                    let short = arg_names.is_some()
                        || members
                            .first()
                            .is_some_and(|o| arg_tys.len() != o.params.len());
                    let module_member = if short {
                        members
                            .iter()
                            .find(|o| o.call_sig.can_map_omitted_args(o.params.len()))
                            .or_else(|| members.first())
                            .cloned()
                    } else {
                        members.first().cloned()
                    };
                    if let Some(fi) = module_member {
                        let params = fi.params.clone();
                        let cs = &fi.call_sig;
                        let mut mapped_slots: Option<Vec<Option<ExprId>>> = None;
                        // Named or omitted arguments: map each argument onto its parameter position via the
                        // parameter names (honouring `required`), then type-check against THAT parameter —
                        // a NAMED call may reorder (`z.test(b = …, a = …)`), so a positional check would
                        // pair each argument with the wrong parameter. Fires for any named call, and for an
                        // omitted-argument call to a method with defaults.
                        if cs.has_param_names()
                            && (arg_names.is_some()
                                || (arg_tys.len() != params.len() && cs.required < params.len()))
                        {
                            match map_call_sig_args(args, arg_names.as_deref(), cs) {
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
                                    mapped_slots = Some(slots);
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
                            self.expect_call_args(&params, false, args, &arg_tys);
                        }
                        // A generic higher-order member: the result is the method's `<R>` inferred from
                        // the lambda body (`box.map { it.length }` → `Int`), not the erased `Object`.
                        let ret = if let Some((gm, class_binds, _)) = &generic_member {
                            self.generic_member_ret(gm, class_binds, &arg_tys)
                        } else {
                            self.inferred_member_ret(rt, &name, &params)
                                .unwrap_or(fi.ret)
                        };
                        self.resolved_calls.insert(
                            call,
                            ResolvedCall::ModuleMember {
                                owner: fi.owner.unwrap_or(internal_name),
                                name: name.clone(),
                                params: params.clone(),
                                ret,
                                interface: fi.is_interface,
                            },
                        );
                        if let Some(slots) = mapped_slots {
                            self.resolved_call_arg_slots.insert(call, slots);
                        }
                        return ret;
                    }
                    // A classpath Java object: resolve the instance method via the `.class` reader.
                    if let Some(m) = self.resolve_instance_member(rt, &name, &arg_tys) {
                        let ret = m.ret;
                        self.resolved_calls.insert(call, ResolvedCall::Member(m));
                        return ret;
                    }
                    // A user class that EXTENDS a classpath type inherits that supertype's members
                    // (`sub.inheritedMethod()`). Runs after the module-member lookup, so a user override
                    // wins.
                    if let Some(internal) = rt.obj_internal() {
                        if let Some(ret) =
                            self.classpath_super_member_ret(call, internal, &name, &arg_tys)
                        {
                            return ret;
                        }
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
                    let user_ext = self
                        .resolver()
                        .resolve_symbol(crate::symbol_resolver::SymRecv::Value(rt), &name, &[], &[])
                        .map(crate::symbol_resolver::Symbol::overloads)
                        .unwrap_or_default()
                        .into_iter()
                        .find(|o| o.is_extension() && o.receiver_rank == 0)
                        .is_some();
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
                crate::trace_compiler!(
                    "resolve",
                    "EXT-SECTION name={name} rt={rt:?} call_targs={call_targs:?}"
                );
                // A lambda-bearing call (`x.forEach { … }`, `x.map { … }`, …) may be lowered by inlining
                // an iteration over the receiver; record the full iterator protocol keyed by the receiver
                // expr so the lowerer reads the capability instead of re-resolving. Gated structurally on
                // a function argument — no method-name list.
                if arg_tys.iter().any(|t| matches!(t, Ty::Fun(_))) {
                    self.record_iterator_protocol(receiver, rt);
                }
                // Stash the RESOLVED explicit type arguments so the lowerer can specialize a `<reified T>`
                // classpath extension's spliced body (imports/classpath types resolve here, not there).
                if !call_targs.is_empty() {
                    self.resolved_call_type_args
                        .insert(call, call_targs.clone());
                }
                if let Some(ret) = self.record_library_extension_call_with_slots(
                    call,
                    &name,
                    rt,
                    args,
                    &call_targs,
                ) {
                    return ret;
                }
                if let Some(ret) =
                    self.record_library_extension_call(Some(call), &name, rt, &arg_tys, &call_targs)
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
                    if let Some(ret) = self.record_library_extension_call(
                        Some(call),
                        &name,
                        rt,
                        &widened,
                        &call_targs,
                    ) {
                        return ret;
                    }
                }
                // A call selected by lambda RETURN type (`recv.sumOf { it * 2 }: Int`): the `@JvmName`
                // overload matching the lambda's return is resolved from `@Metadata`. Record the resolved
                // callable (+ member-vs-extension) so the lowerer emits it directly — its UNSCOPED resolver
                // cannot re-derive the mangled JVM method name.
                if let Some(lam_ret) = arg_tys.iter().find_map(|t| {
                    if let Ty::Fun(s) = t {
                        Some(s.ret)
                    } else {
                        None
                    }
                }) {
                    if let Some((c, is_member)) =
                        self.lambda_return_overload(rt, &name, lam_ret, &arg_tys)
                    {
                        let ret = c.ret;
                        // An instance member has no other lowerer resolution path (records as a member);
                        // an extension flows through the general extension-emit branch (records as one).
                        let resolved = if is_member {
                            ResolvedCall::LambdaReturnMember(c)
                        } else {
                            ResolvedCall::Extension(c)
                        };
                        self.resolved_calls.insert(call, resolved);
                        return ret;
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
                // (`@InlineOnly` extensions — scope fns `takeIf`/`let`/… — are resolved and recorded by
                // `record_library_extension_call` above: one extension resolution admits inline, and the
                // lowerer splices via the callable's `inline` flag. No separate inline path here.)
                // User-defined extension function in this file (invokestatic on the file facade), resolved
                // through the current module as a `SymbolSource`. The exact-receiver overload is rung 0;
                // its `callable.params` prepend the receiver and `callable.descriptor` is the full static
                // `(recv + params)ret` the emitter wants.
                {
                    // Select the overload matching this call's arguments (an extension may be
                    // overloaded by arity — `fun R.f()` and `fun R.f(x)`); fall back to the first when
                    // none fits exactly, preserving the omitted-default / named-argument handling below.
                    let exts: Vec<_> = self
                        .resolver()
                        .resolve_symbol(crate::symbol_resolver::SymRecv::Value(rt), &name, &[], &[])
                        .map(crate::symbol_resolver::Symbol::overloads)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|o| o.is_extension() && o.receiver_rank == 0)
                        .collect();
                    let module_ext = exts
                        .iter()
                        .find(|fi| {
                            let lp = fi.extension_value_params();
                            lp.len() == arg_tys.len()
                                && lp
                                    .iter()
                                    .zip(&arg_tys)
                                    .all(|(p, a)| crate::symbol_resolver::arg_fits(p, a))
                        })
                        .or_else(|| exts.first())
                        .cloned();
                    if let Some(fi) = module_ext {
                        let logical = fi.extension_value_params().to_vec();
                        let cs = &fi.call_sig;
                        if (arg_names.is_some() || arg_tys.len() != logical.len())
                            && cs.can_map_omitted_args(logical.len())
                        {
                            // Omitted/named extension arguments filled by parameter defaults.
                            match map_call_sig_args(args, arg_names.as_deref(), cs) {
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
                            self.expect_call_args(&logical, false, args, &arg_tys);
                        }
                        return fi.callable.ret;
                    }
                }
                if erased_type_key(rt) != erased_type_key(Ty::obj("kotlin/Any")) {
                    let module_ext = self
                        .resolver()
                        .resolve_symbol(crate::symbol_resolver::SymRecv::Value(rt), &name, &[], &[])
                        .map(crate::symbol_resolver::Symbol::overloads)
                        .unwrap_or_default()
                        .into_iter()
                        .find(|o| o.is_extension() && o.receiver_rank == 1);
                    if let Some(fi) = module_ext {
                        let logical = fi.extension_value_params().to_vec();
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
                                self.expect_call_args(&logical, false, args, &arg_tys);
                                let recv_tp = decl.receiver.as_ref().map(|r| r.name.clone());
                                let ret = match &decl.ret {
                                    Some(r) if Some(&r.name) == recv_tp.as_ref() => rt,
                                    Some(r) => decl
                                        .params
                                        .iter()
                                        .zip(&arg_tys)
                                        .find_map(|(p, a)| (p.ty.name == r.name).then_some(*a))
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
                if rt.is_array() {
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
                            cs.internal_matches(&inner_internal)
                                && cs.inner_of_name() == Some(outer_internal)
                        })
                        .cloned()
                    {
                        if inner.ctor_params.len() == arg_tys.len() {
                            self.expect_call_args(&inner.ctor_params, false, args, &arg_tys);
                            return Ty::obj_name(inner.internal_name());
                        }
                    }
                }
                // `(suspend (…)->T).startCoroutine(completion)` — a `kotlin.coroutines` extension intrinsic
                // (recognized via the registry). Receiver is a suspend function value; semantically the
                // call starts the coroutine and returns `Unit`.
                if matches!(rt, Ty::Fun(s) if s.suspend)
                    && crate::libraries::coroutine_intrinsic(&name)
                        == Some(crate::libraries::CoroutineIntrinsic::StartCoroutine)
                {
                    return Ty::Unit;
                }
                // A `@JvmStatic` member of a classpath `object` (`UuidGen.of(x)`): kotlinc emits a
                // static method on the object class, so it lands in the type's `companion` (static) list —
                // not an instance member. Resolve it there as a static call on the receiver's type.
                if let Some(internal) = rt.obj_internal() {
                    if let Some(m) = self
                        .resolve_companion_name(internal, &name, &arg_tys)
                        // A `@JvmStatic suspend fun` keeps its physical `Continuation` param here (the
                        // companion path doesn't strip/CPS it), so leave it unresolved rather than
                        // miscompile the calling convention.
                        .filter(|m| !m.suspend)
                    {
                        self.expect_call_args(&m.params, false, args, &arg_tys);
                        // Record the resolved static member so the lowerer emits it without
                        // re-resolving (see [`TypeInfo::resolved_companions`]).
                        let ret = m.ret;
                        self.resolved_calls.insert(call, ResolvedCall::Companion(m));
                        return ret;
                    }
                }
                // Member-syntax invoke of a RECEIVER-function-typed value in scope: `b.f()` where
                // `f: Bar.() -> R` is a local/parameter and `Bar` has no member `f`. Kotlin then
                // resolves `f` lexically; the receiver becomes the function value's folded-first
                // argument. Non-`suspend` only (a suspend invoke needs continuation threading this
                // path doesn't model — leave it unresolved so the file skips).
                if let Some(sig) =
                    self.lookup(&name)
                        .and_then(|l| match l.narrowed.unwrap_or(l.ty) {
                            Ty::Fun(sig) if !sig.suspend => Some(sig),
                            _ => None,
                        })
                {
                    if let Some((&first, rest)) = sig.params.split_first() {
                        if rest.len() == arg_tys.len() && arg_assignable_simple(first, rt) {
                            let rest = rest.to_vec();
                            self.expect_call_args(&rest, false, args, &arg_tys);
                            self.expr_lowers.insert(
                                call,
                                ExprLowering::ReceiverFnInvoke {
                                    name: name.clone(),
                                    params: sig.params.clone(),
                                    ret: sig.ret,
                                },
                            );
                            return sig.ret;
                        }
                    }
                }
                // Invoking a function-typed MEMBER PROPERTY: `obj.func(args)` where `func` is a
                // `val func: (…) -> R` property (e.g. an enum entry's `func`). No method `func` exists;
                // read the property (its type is `Ty::Fun`) and invoke it through the one invoke
                // convention, exactly like a local/top-level function-typed value.
                let prop_fun_ty = self
                    .resolve_property_read(rt, &name, span, Some(callee))
                    .filter(|t| matches!(t, Ty::Fun(_)));
                if let Some(fun_ty) = prop_fun_ty {
                    if let Some(ret) =
                        self.record_invoke(call, callee, fun_ty, args, &arg_tys, span)
                    {
                        return ret;
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
                    let arg_tys = self.arg_tys(args);
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
                if !self.lexical_value_declares(&fname) {
                    if let Some(&(rt @ Ty::Fun(_), _, _)) = self.syms.props.get(&fname) {
                        let arg_tys = self.arg_tys(args);
                        if let Some(ret) =
                            self.record_invoke(call, callee, rt, args, &arg_tys, span)
                        {
                            return ret;
                        }
                    }
                }
                // Local function call — resolved before top-level funs and constructors.
                if let Some((stmt_id, sig)) = self.lookup_local_fun(&fname) {
                    // Context parameters on a local function (`context(a: A) fun f() = a`): the leading
                    // `context_count` params are supplied implicitly from the enclosing context. When
                    // the explicit args fill the remaining value params and every context param is
                    // satisfied by an in-scope source, resolve them and record the sources for the
                    // lowerer (mirrors the top-level context-parameter path).
                    let ctx_count = match self.file.stmt(stmt_id) {
                        Stmt::LocalFun(f) => f.context_count,
                        _ => 0,
                    };
                    if ctx_count > 0
                        && ctx_count <= sig.params.len()
                        && args.len() == sig.params.len() - ctx_count
                    {
                        let ctx_types = &sig.params[..ctx_count];
                        if let Some(sources) = self.resolve_context_args(ctx_types) {
                            for (i, a) in args.iter().enumerate() {
                                let p = sig.params[ctx_count + i];
                                let aty = match p {
                                    Ty::Fun(fs)
                                        if matches!(self.file.expr(*a), Expr::Lambda { .. }) =>
                                    {
                                        let pts = fs.params.clone();
                                        self.check_lambda_with_types(*a, &pts)
                                    }
                                    _ => self.expr(*a),
                                };
                                self.expect_assignable(p, aty, self.span(*a), "argument");
                            }
                            self.mark_local_function_call(
                                call,
                                stmt_id,
                                sig.clone(),
                                args.len(),
                                sources,
                            );
                            return sig.ret;
                        }
                    }
                    // A local function may OMIT trailing arguments whose parameters have defaults
                    // (`fun bar(x: Int = 1); bar()`). krusty emits local functions as plain methods (no
                    // `$default` synthetic), so the lowerer fills the omitted defaults at the call site.
                    // Allow the short call only when every omitted trailing parameter has a default (and
                    // no context parameters are in play — those took the branch above).
                    // An omitted default is lowered at the CALL site (where the local function's own
                    // parameters are NOT in scope), so a default that references another parameter can't be
                    // filled here — leave those to be rejected (a shadowing outer variable of the same name
                    // would otherwise be captured instead of the parameter).
                    let omitted_all_default = ctx_count == 0
                        && args.len() < sig.params.len()
                        && matches!(self.file.stmt(stmt_id), Stmt::LocalFun(f) if {
                            let pnames: std::collections::HashSet<&str> =
                                f.params.iter().map(|p| p.name.as_str()).collect();
                            f.params.len() == sig.params.len()
                                && f.params[args.len()..].iter().all(|p| {
                                    p.default
                                        .is_some_and(|dx| !self.file.expr_uses_any_name(dx, &pnames))
                                })
                        });
                    if args.len() > sig.params.len()
                        || (args.len() < sig.params.len() && !omitted_all_default)
                    {
                        let arg_tys = self.arg_tys(args); // still record types for lowering
                        self.diags.error(
                            span,
                            format!(
                                "local function '{fname}' expects {} args, got {}",
                                sig.params.len(),
                                arg_tys.len()
                            ),
                        );
                    } else {
                        // Type each PROVIDED argument against its declared parameter (omitted trailing
                        // parameters use their defaults). A LAMBDA argument passed to a function-typed
                        // parameter must be checked WITH that parameter's block parameter types
                        // (`check_lambda_with_types`) — exactly as a top-level call does — so a
                        // destructured / `it` lambda parameter gets its real type instead of erased `Any`.
                        for (i, &a) in args.iter().enumerate() {
                            let p = sig.params[i];
                            let aty = match p {
                                Ty::Fun(fs) if matches!(self.file.expr(a), Expr::Lambda { .. }) => {
                                    let pts = fs.params.clone();
                                    self.check_lambda_with_types(a, &pts)
                                }
                                _ => self.expr(a),
                            };
                            self.expect_assignable(p, aty, self.span(a), "argument");
                        }
                    }
                    let ret = sig.ret;
                    self.mark_local_function_call(call, stmt_id, sig, args.len(), Vec::new());
                    return ret;
                }
                if let ("with", [receiver, lambda], false) =
                    (fname.as_str(), args, self.module_declares(&fname))
                {
                    if let Expr::Lambda { params, body } = self.file.expr(*lambda).clone() {
                        if params.is_empty() {
                            let rt = self.expr(*receiver);
                            let bt =
                                self.check_with_receiver_labeled(rt, body, call_fn_name.as_deref());
                            self.mark_receiver_lambda_call(call, *receiver, body, false);
                            return self.set(call, bt);
                        }
                    }
                }
                // Coroutine intrinsics type their lambda with the current Continuation and yield the
                // enclosing suspend function's return type.
                let one_lambda_arg = match args {
                    [arg] if matches!(self.file.expr(*arg), Expr::Lambda { .. }) => Some(*arg),
                    _ => None,
                };
                let unshadowed_name = !self.lexical_value_declares(&fname);
                let suspend_coroutine = matches!(
                    crate::libraries::coroutine_intrinsic(&fname),
                    Some(
                        crate::libraries::CoroutineIntrinsic::SuspendCoroutineUninterceptedOrReturn
                            | crate::libraries::CoroutineIntrinsic::SuspendCoroutine
                    )
                );
                if let (Some(lambda), true, true) = (
                    one_lambda_arg,
                    unshadowed_name,
                    !self.module_declares(&fname) && suspend_coroutine,
                ) {
                    let cont = Ty::obj("kotlin/coroutines/Continuation");
                    self.check_lambda_with_types(lambda, &[cont]);
                    let r = self.ret_ty;
                    return self.set(call, r);
                }
                // SAM conversion `Pred { lambda }`: type the lambda from the SAM method parameters.
                if let (Some(lambda), true) = (one_lambda_arg, unshadowed_name) {
                    if let Some(cls) = self.syms.classes.get(&fname).cloned() {
                        if let Some(sig) = cls.single_method().filter(|_| cls.is_interface) {
                            let pts = sig.params.clone();
                            self.check_lambda_with_types(lambda, &pts);
                            return self.set(call, Ty::obj(&cls.internal()));
                        }
                    }
                    // A classpath functional interface (`Runnable`, `Comparator`, …).
                    if let Some(internal) = self.syms.class_names.get(&fname) {
                        if let Some(sam) = self
                            .resolved_type_name(internal)
                            .and_then(|t| t.sam_method.clone())
                        {
                            self.check_lambda_with_types(lambda, &sam.params);
                            return self.set(call, Ty::obj_name(internal));
                        }
                    }
                }
                // Type-directed lambda checking: if we know the target function's signature and a
                // parameter is a function type with known inner param types, check lambda args with
                // the correct `it` type instead of always using Object.
                // For lambda-argument pre-typing we need a single known signature; use it only when the
                // name is unambiguous (one overload). An overloaded call's lambda `it` falls back to the
                // erased type — a minor precision loss, not a miscompile.
                let known_sig = self.syms.single_fun(&fname);
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
                let toplevel_partial: Option<Vec<Option<Ty>>> = if !self
                    .lexical_value_declares(&fname)
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
                let toplevel_lambda_shape = toplevel_partial
                    .as_ref()
                    // A library top-level function only when no user function shadows it.
                    .filter(|_| known_sig.is_none())
                    .and_then(|partial| self.top_level_lambda_shape(&fname, partial));
                let toplevel_lambda_pts: Option<Vec<Vec<Ty>>> = toplevel_lambda_shape
                    .as_ref()
                    .and_then(|shape| shape.param_types.clone())
                    .or_else(|| user_generic.clone());
                // Per-param RECEIVER function type for a classpath top-level HOF (`NavHost(builder:
                // NGB.()->Unit){…}`) — a lambda to such a param binds its implicit `this` to the receiver.
                // From `@Metadata`'s `@ExtensionFunctionType` (no JVM `Signature` needed, so this also
                // drives a krusty-emitted module's HOF whose `Signature` attribute is absent). `None` for a
                // user fn.
                let toplevel_lambda_recvs: Option<Vec<Option<Ty>>> = toplevel_lambda_shape
                    .as_ref()
                    .and_then(|shape| shape.receivers.clone());
                // Per-param `crossinline`/`noinline`: such a lambda argument is MATERIALIZED (a real
                // closure, e.g. the `Continuation(ctx){…}` factory's `resumeWith`), so a mutable local it
                // captures must be `Ref`-boxed — DON'T treat it as an inline splice.
                let toplevel_lambda_materialized: Option<Vec<bool>> = toplevel_lambda_shape
                    .as_ref()
                    .and_then(|shape| shape.materialized.clone());
                // A top-level NON-public (`@InlineOnly`) inline fn (`require`/`check`) inlines its lambda
                // argument (or the file is skipped), so a mutable capture is an inline capture — type the
                // lambda body with mutation allowed (don't `Ref`-box the captured var).
                let top_level_functions = crate::libraries::FunctionSet {
                    overloads: self
                        .resolver()
                        .resolve_symbol(crate::symbol_resolver::SymRecv::TopLevel, &fname, &[], &[])
                        .map(crate::symbol_resolver::Symbol::overloads)
                        .unwrap_or_default(),
                };
                let toplevel_inline = toplevel_lambda_pts.is_some()
                    && top_level_functions
                        .top_level()
                        .any(|o| o.flags.inline.can_inline());
                let toplevel_must_inline = !self.lexical_value_declares(&fname)
                    && !self.module_declares(&fname)
                    && args
                        .iter()
                        .any(|&a| matches!(self.file.expr(a), Expr::Lambda { .. }))
                    && top_level_functions
                        .top_level()
                        .any(|o| o.flags.inline.must_inline());
                // An implicit-`this` member higher-order call (`update { … }` reached inside a member or
                // extension body, with no explicit receiver): resolve the member through the module
                // hierarchy and pre-type each lambda argument from the member's declared function-type
                // parameter. Without this a no-parameter lambda (`{ null }`) adopts the erased zero-arg
                // form and then fails the arity check against the parameter's `(T?) -> T?`. Mirrors the
                // explicit-receiver `method_sig` lambda pre-typing; gated to a receiver-less name that is
                // neither a local nor a top-level function (`known_sig` covers the latter).
                let this_member_lambda_pts: Option<Vec<Vec<Ty>>> = if !self
                    .lexical_value_declares(&fname)
                    && known_sig.is_none()
                    && args
                        .iter()
                        .any(|&a| matches!(self.file.expr(a), Expr::Lambda { .. }))
                {
                    self.this_ty.and_then(|rt| {
                        crate::module_symbols::ModuleSymbols::new(self.syms)
                            .instance_members(rt, &fname)
                            .into_iter()
                            .next()
                            .map(|m| m.call_sig.lambda_param_types)
                    })
                } else {
                    None
                };
                // A same-file class CONSTRUCTOR call (`C({ x, y -> x + y })`, an `enum` entry
                // `plus({ x, y -> … })`): a lambda passed to a function-typed primary-ctor parameter
                // binds its parameter types from that parameter's `Ty::Fun`, exactly like a top-level
                // function call — without this the lambda parameters erase to `Any` and the body fails
                // (`x + y` → "operator cannot be applied to Any and Any").
                let ctor_lambda_pts: Option<Vec<(Vec<Ty>, bool)>> = if !self
                    .lexical_value_declares(&fname)
                    && args
                        .iter()
                        .any(|&a| matches!(self.file.expr(a), Expr::Lambda { .. }))
                {
                    // `Ty::Fun` folds a receiver into the first parameter and drops the marker, so a
                    // RECEIVER function-type ctor param (`config: Pipeline.() -> Unit`) reads its
                    // `fun_has_receiver` flag from the same-file class declaration — the lambda then
                    // binds `this`, not `it` (KT-606: a bare member call inside the lambda must
                    // dispatch on the receiver, not fall back to a stdlib top-level).
                    let recv_flags: Vec<bool> = self
                        .file
                        .decls
                        .iter()
                        .find_map(|&d| match self.file.decl(d) {
                            Decl::Class(c) if c.name == fname => {
                                Some(c.props.iter().map(|p| p.ty.fun_has_receiver).collect())
                            }
                            _ => None,
                        })
                        .unwrap_or_default();
                    self.syms.classes.get(&fname).map(|cls| {
                        cls.ctor_params
                            .iter()
                            .enumerate()
                            .map(|(i, p)| match p {
                                Ty::Fun(s) => (
                                    s.params.clone(),
                                    recv_flags.get(i).copied().unwrap_or(false),
                                ),
                                _ => (Vec::new(), false),
                            })
                            .collect()
                    })
                } else {
                    None
                };
                let arg_tys: Vec<Ty> = args
                    .iter()
                    .enumerate()
                    .map(|(i, &a)| {
                        if array_init_lambda && i == 1 {
                            return self.check_lambda_with_types(a, &[Ty::Int]);
                        }
                        // Constructor lambda argument → typed from the ctor parameter's function type;
                        // a RECEIVER function-type param binds the folded-first param as `this`.
                        if let Some(ref pts) = ctor_lambda_pts {
                            if pts.get(i).is_some_and(|(v, _)| !v.is_empty())
                                && matches!(self.file.expr(a), Expr::Lambda { .. })
                            {
                                let (pt, has_recv) = pts[i].clone();
                                if has_recv {
                                    return self.check_lambda_with_receiver_labeled(
                                        a,
                                        pt[0],
                                        &pt[1..],
                                        None,
                                    );
                                }
                                return self.check_lambda_with_types(a, &pt);
                            }
                        }
                        // Implicit-`this` member HOF: pre-type the lambda from the member's declared
                        // function-type parameter (see `this_member_lambda_pts`).
                        if let Some(ref pts) = this_member_lambda_pts {
                            if pts.get(i).is_some_and(|v| !v.is_empty())
                                && matches!(self.file.expr(a), Expr::Lambda { .. })
                            {
                                let pt = pts[i].clone();
                                return self.check_lambda_with_types(a, &pt);
                            }
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
                                // crossinline/noinline materializes a closure; other inline lambdas splice.
                                let materialized = toplevel_lambda_materialized
                                    .as_ref()
                                    .and_then(|m| m.get(i))
                                    .copied()
                                    .unwrap_or(false);
                                // A RECEIVER function-type param: bind the receiver as the lambda's `this`;
                                // the rest are value params. Prefer the @Metadata receiver (`recv_i`,
                                // deterministic — `MutableList` for `buildList`) over `pts[i][0]`, whose
                                // JVM-`Signature` decode is order-dependent (flakily `java/util/List`, on which
                                // a `MutableList` extension like `removeLastOrNull` fails to resolve).
                                return self.with_lambda_mutation(
                                    toplevel_inline && !materialized,
                                    |c| {
                                        if let Some(recv) = recv_i {
                                            c.check_lambda_with_receiver_labeled(
                                                a,
                                                recv,
                                                &pts[i][1..],
                                                call_fn_name.as_deref(),
                                            )
                                        } else {
                                            c.check_lambda_with_types(a, &pts[i])
                                        }
                                    },
                                );
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
                            return self
                                .with_lambda_mutation(true, |c| c.check_lambda_with_types(a, &pt));
                        }
                        // Reuse the already-computed non-lambda argument type (avoid re-typing).
                        if let Some(Some(t)) = toplevel_partial.as_ref().and_then(|p| p.get(i)) {
                            return *t;
                        }
                        if let Some(ref sig) = known_sig {
                            // The PARAMETER index this argument binds — not always the positional `i`:
                            // a NAMED argument binds its named parameter, and a SYNTACTIC trailing
                            // lambda always binds the LAST parameter (omitted middles take their
                            // defaults: `ef("m") { … }` on `ef(msg, chk = null, action)` puts the
                            // lambda in `action`, not `chk`). Without this the lambda pre-types
                            // against the WRONG parameter's function shape (arity mismatch).
                            let pi = arg_names
                                .as_ref()
                                .and_then(|ns| ns.get(i))
                                .and_then(|n| n.as_ref())
                                .and_then(|n| sig.param_names.iter().position(|p| p == n))
                                .unwrap_or_else(|| {
                                    if self.file.call_has_trailing_lambda.contains(&call.0)
                                        && i + 1 == args.len()
                                        && args.len() <= sig.params.len()
                                    {
                                        sig.params.len() - 1
                                    } else {
                                        i
                                    }
                                });
                            // An ADAPTED function reference argument: `::foo` (a same-file top-level
                            // function with trailing defaults) passed to a function-typed parameter of
                            // SMALLER arity. Type it as the expected function type and record the
                            // adaptation (the lowerer synthesizes an arity-matching adapter).
                            if let Some(&Ty::Fun(exp)) = sig.params.get(pi) {
                                if let Expr::CallableRef {
                                    receiver: None,
                                    name,
                                } = self.file.expr(a).clone()
                                {
                                    if let Some(t) = self.try_adapt_toplevel_ref(a, &name, exp) {
                                        return t;
                                    }
                                }
                            }
                            // A lambda argument to a function-typed parameter. For an `inline fun` the lambda
                            // is inlined into the caller, so it may capture a mutable local (like the stdlib
                            // `repeat`/`forEach`). This also covers zero-parameter lambdas (`() -> Unit`),
                            // whose `lambda_param_types[i]` is empty.
                            if pi < sig.params.len()
                                && matches!(sig.params[pi], Ty::Fun(_))
                                && matches!(self.file.expr(a), Expr::Lambda { .. })
                            {
                                let pt =
                                    sig.lambda_param_types.get(pi).cloned().unwrap_or_default();
                                // A RECEIVER function-type param (`Recv.(A) -> R`): bind `pt[0]` as the
                                // lambda's implicit `this`; the rest are its value params.
                                return self.with_lambda_mutation(sig.is_inline, |c| {
                                    if sig.lambda_recv.get(pi).copied().unwrap_or(false)
                                        && !pt.is_empty()
                                    {
                                        c.check_lambda_with_receiver_labeled(
                                            a,
                                            pt[0],
                                            &pt[1..],
                                            call_fn_name.as_deref(),
                                        )
                                    } else {
                                        c.check_lambda_with_types(a, &pt)
                                    }
                                });
                            }
                            // A lambda argument SAM-converted to a simple `fun interface` parameter:
                            // type it with the interface abstract method's parameter types so its
                            // params resolve concretely and the lowered impl matches the SAM descriptor.
                            if pi < sig.params.len()
                                && matches!(self.file.expr(a), Expr::Lambda { .. })
                            {
                                if let Some(internal) = sig.params[pi].obj_internal() {
                                    if self.simple_fun_interface_name(internal) {
                                        if let Some(sp) =
                                            self.fun_interface_sam_params_name(internal)
                                        {
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
                if !self.lexical_value_declares(&fname) {
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
                            } else if elem.jvm_boxed_ref().is_some() {
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
                            self.expect_call_args(&sig.params, false, args, &arg_tys);
                            return sig.ret;
                        }
                    }
                }
                // Constructor call: `ClassName(args)` (when not shadowed by a local). An unqualified name
                // that is one of the ENCLOSING class's own nested types resolves to the NESTED class
                // (Kotlin nested-type scoping), preferred over a same-named top-level — consistent with
                // `resolve_type`, so the construction's type matches the field/return-position type.
                if !self.value_root_shadows_classifier(&fname) {
                    let ctor_cls = self
                        .enclosing_nested_type(&fname)
                        .and_then(|nested| {
                            self.syms
                                .classes
                                .values()
                                .find(|s| s.internal_matches(&nested))
                                .cloned()
                        })
                        .or_else(|| self.syms.classes.get(&fname).cloned())
                        .or_else(|| {
                            // An IMPORTED nested class (`import demo.Outer.Inner` → `Inner`): the
                            // ClassSig is keyed by its hoisted name (`Outer.Inner`), not the simple
                            // name, so resolve the simple name through imports to its internal and find
                            // the sig by that — the same reference qualified `Outer.Inner(…)` uses.
                            self.imported_type_name(&fname).and_then(|internal| {
                                self.syms.class_by_type_name(internal).cloned()
                            })
                        });
                    if let Some(cls) = ctor_cls {
                        let ctor_params: Vec<Ty> = cls.ctor_params.clone();
                        // A value class is resolved like ANY class here — no value-class special case.
                        // Its construction (incl. `Vid()` omitting a defaulted sole param) lowers through
                        // the uniform `New`, which the value-class JVM pass realizes as `constructor-impl` /
                        // `constructor-impl$default`. The resolver carries no value-class knowledge.
                        // Named-argument constructor call (`C(b = 9)`): map names → positions using the
                        // primary ctor's parameter names + per-parameter defaults, the same path a
                        // top-level function uses. An omitted parameter falls back to its default (the
                        // lowering fills a simple-literal default, or skips the file — never miscompiles).
                        if arg_names.is_some() {
                            if let Some(param_list) = self.primary_ctor_param_list(&fname) {
                                match map_param_list_args(args, arg_names.as_deref(), &param_list) {
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
                                return self.ctor_result_name(call, cls.internal_name());
                            }
                        }
                        // Omitted trailing arguments are allowed when those parameters have a default
                        // that is a *simple literal of the parameter's exact type* — the call site can
                        // emit it directly. Adapting defaults (`Long = 0`) or complex defaults
                        // (anonymous objects, `emptyArray()`) aren't modeled yet → skip those.
                        let got = arg_tys.len();
                        // Omitting a trailing parameter is allowed when either: it has a directly-emittable
                        // literal default (any class, filled at the call site), OR the class is a VALUE
                        // class and the param simply HAS a default — a value class's non-const default
                        // (`ServerId(val v = Base58Uuid.generate())`) lowers via `constructor-impl$default`.
                        let ok_arity = got <= ctor_params.len()
                            && (got..ctor_params.len()).all(|i| {
                                cls.ctor_defaults
                                    .get(i)
                                    .and_then(|o| o.as_ref())
                                    .is_some_and(|dv| dv.fills_param_ty(ctor_params[i]))
                                    || (cls.value_field.is_some()
                                        && cls
                                            .ctor_param_names
                                            .get(i)
                                            .is_some_and(|(_, has_default)| *has_default))
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
                                self.expect_call_args(sparams, false, args, &arg_tys);
                                return self.ctor_result_name(call, cls.internal_name());
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
                                    self.expect_call_args(sparams, false, args, &arg_tys);
                                    return self.ctor_result_name(call, cls.internal_name());
                                }
                            }
                            self.expect_call_args(&ctor_params, false, args, &arg_tys);
                        }
                        return self.ctor_result_name(call, cls.internal_name());
                    }
                    // Named classpath constructors use metadata names/defaults; lowering selects
                    // either the plain constructor or the default-argument synthetic.
                    if let Some(names) = arg_names.as_deref() {
                        if let Some(internal) = self.classpath_class_internal_name(&fname) {
                            match self
                                .record_named_library_constructor_name(call, internal, args, names)
                            {
                                Ok(Some(target)) => {
                                    if let ResolvedConstructor::Plain { member, args } = target {
                                        for (p, a) in member.params.iter().zip(&args) {
                                            self.expect_assignable(
                                                *p,
                                                self.expr_types[a.0 as usize],
                                                self.span(*a),
                                                "argument",
                                            );
                                        }
                                    }
                                    return self.ctor_result_name(call, internal);
                                }
                                Ok(None) => {}
                                Err(msg) => self
                                    .diags
                                    .error(span, format!("constructor '{fname}': {msg}")),
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
                        .and_then(|i| self.nested_internal_name(i))
                    {
                        if self
                            .record_library_constructor_name(
                                call,
                                internal,
                                args.to_vec(),
                                &arg_tys,
                            )
                            .is_some()
                        {
                            return self.ctor_result_name(call, internal);
                        }
                    }
                    // A library type by simple name (`throw RuntimeException("msg")`, a mapped/aliased
                    // type with no explicit import): ask the library to resolve the constructor. The
                    // library owns any target-specific knowledge (e.g. the throwable-ctor shapes the
                    // JVM jimage can't surface) — the resolver no longer special-cases throwables.
                    if let Some(internal) = self.syms.class_names.get(&fname) {
                        if self
                            .record_library_constructor_name(
                                call,
                                internal,
                                args.to_vec(),
                                &arg_tys,
                            )
                            .is_some()
                        {
                            return self.ctor_result_name(call, internal);
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
                        let internal_rendered = internal.render();
                        // The sibling member through the module source; the enclosing-class fallback walks
                        // `inner_of` (a LEXICAL scope, not the type hierarchy) so it stays on `lookup_method`.
                        let resolved: Option<(Vec<Ty>, Ty)> =
                            crate::module_symbols::ModuleSymbols::new(self.syms)
                                .instance_members(Ty::obj_name(internal), &fname)
                                .into_iter()
                                .next()
                                .map(|m| (m.params, m.ret))
                                .or_else(|| {
                                    self.syms
                                        .class_by_type_name(internal)
                                        .and_then(ClassSig::inner_of_name)
                                        .and_then(|outer| self.lookup_method_name(outer, &fname))
                                        .map(|s| (s.params, s.ret))
                                });
                        crate::trace_compiler!(
                            "resolve",
                            "unqualified sibling call {fname}() on this_ty={internal_rendered} -> {resolved:?}"
                        );
                        if let Some((params, ret)) = resolved {
                            // A `vararg` sibling method (`fun f(vararg s: T)`) accepts trailing `T` args
                            // packed into the array param — element-type them, don't match the array
                            // positionally.
                            let vararg = self
                                .syms
                                .method_of_name(internal, &fname)
                                .is_some_and(|s| s.vararg);
                            self.expect_call_args(&params, vararg, args, &arg_tys);
                            // An EXPRESSION-body sibling method whose declared return was the collection
                            // default (`Unit`, not yet inferred) — refine from the inference recorded when
                            // its body was checked (an anonymous object / local class whose `fun m() = f()`
                            // return couldn't be inferred at collection). Matches the qualified-member path.
                            return self
                                .inferred_member_ret(Ty::obj_name(internal), &fname, &params)
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
                    // The declared receiver has no such member — try the flow-narrowed receiver from an
                    // enclosing `if (this is B)` (`fun A.test() = if (this is B) foo()`, where `foo` is a
                    // member of the subtype `B`). Record the narrowing on the call so the lowerer
                    // `checkcast`s `this` to `B` before dispatching, mirroring the bare-name property read.
                    // `this_narrow` is only ever a known reference subtype of the receiver.
                    if let Some(bt) = self.this_narrow {
                        if let Some(bi) = bt.obj_internal() {
                            if let Some(m) = crate::module_symbols::ModuleSymbols::new(self.syms)
                                .instance_members(bt, &fname)
                                .into_iter()
                                .next()
                            {
                                self.narrowed_this_member.insert(call, bi);
                                let vararg = self
                                    .syms
                                    .method_of_name(bi, &fname)
                                    .is_some_and(|s| s.vararg);
                                self.expect_call_args(&m.params, vararg, args, &arg_tys);
                                return self
                                    .inferred_member_ret(bt, &fname, &m.params)
                                    .unwrap_or(m.ret);
                            }
                        }
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
                        .resolve_top_level_in_scope(&fname, &arg_tys, &self.fn_scope);
                // A member of the nearest implicit receiver (a receiver-lambda body / `with` block —
                // `"ab".run { uppercase() }`, `with(scope) { test1() }`) shadows a top-level function of
                // the same name: the receiver is a closer scope, so kotlinc binds the member. Attempt it
                // FIRST — `this_member_call_ret` returns `None` when no member matches the arguments, so a
                // genuine top-level call (no such member) still falls through to `module_top` below.
                if let Some(rt) = self.this_ty {
                    if let Some(ret) = self.this_member_call_ret(call, rt, &fname, &arg_tys, args) {
                        return ret;
                    }
                }
                let shadowed_by_member = self.this_ty.is_some_and(|t| {
                    // A user-class member, OR a member on a classpath/builtin receiver type — either
                    // takes precedence over a context-parameter function, so decline context resolution
                    // rather than risk a wrong binding.
                    t.obj_internal()
                        .is_some_and(|i| self.syms.method_of_name(i, &fname).is_some())
                        || self.resolve_instance_member(t, &fname, &arg_tys).is_some()
                });
                if module_top.is_none() && !shadowed_by_member {
                    if let Some((fi, sources)) =
                        self.resolve_context_module_top_level(&fname, &arg_tys)
                    {
                        let params = &fi.callable.params;
                        let ctx_count = fi.context_count;
                        let ret_ty = self.module_top_level_return(call, &fi, &arg_tys);
                        for (i, a) in arg_tys.iter().enumerate() {
                            self.expect_assignable(
                                params[ctx_count + i],
                                *a,
                                self.span(args[i]),
                                "argument",
                            );
                        }
                        self.mark_module_top_level_call(call, &fname, &fi, ret_ty, sources);
                        return ret_ty;
                    }
                }
                if let Some(fi) = module_top {
                    let params = &fi.callable.params;
                    let cs = &fi.call_sig;
                    let ret_ty = self.module_top_level_return(call, &fi, &arg_tys);
                    // Context parameters (`context(a: A) fun f()`): the leading `context_count`
                    // parameters are supplied IMPLICITLY from the enclosing context, not positionally.
                    // When the explicit arguments exactly fill the remaining (value) parameters and every
                    // context parameter is satisfied by an in-scope source (an implicit receiver or an
                    // enclosing context parameter/local), resolve them and record the sources for the
                    // lowerer. Otherwise fall through (a missing context → the normal arity error → skip).
                    let ctx_count = fi.context_count;
                    // A member of the enclosing implicit receiver with the same name takes precedence
                    // over a context-parameter function (kotlinc resolution order). krusty resolves the
                    // implicit-receiver member only when there is no top-level shadow, so rather than
                    // mis-bind the context function here, decline the context resolution (the call then
                    // hits the normal arity path and the file skips — sound, never a wrong binding).
                    let value_count = params.len().saturating_sub(ctx_count);
                    let context_value_args_ok = arg_tys.len() <= value_count
                        && (arg_tys.len()..value_count)
                            .all(|i| cs.param_has_default(ctx_count + i));
                    if ctx_count > 0
                        && !shadowed_by_member
                        && ctx_count <= params.len()
                        && context_value_args_ok
                    {
                        let ctx_types = &params[..ctx_count];
                        if let Some(sources) = self.resolve_context_args(ctx_types) {
                            // Type-check the explicit (value) arguments against the trailing parameters.
                            for (i, a) in arg_tys.iter().enumerate() {
                                self.expect_assignable(
                                    params[ctx_count + i],
                                    *a,
                                    self.span(args[i]),
                                    "argument",
                                );
                            }
                            self.mark_module_top_level_call(call, &fname, &fi, ret_ty, sources);
                            return ret_ty;
                        }
                    }
                    if cs.vararg {
                        // The `vararg` is always the LAST parameter (`n_fixed` = its index). The minimum
                        // positional argument count is the number of LEADING non-default fixed parameters
                        // — a defaulted fixed parameter (or the vararg) may be omitted. A caller supplies
                        // fixed parameters first, then vararg elements.
                        let n_fixed = params.len() - 1;
                        let min_args = (0..n_fixed)
                            .take_while(|&i| !cs.param_has_default(i))
                            .count();
                        // Any omitted fixed parameter must be defaulted (a middle non-default hole can't be
                        // filled positionally by later arguments).
                        let omitted_ok = arg_tys.len() >= n_fixed
                            || (arg_tys.len()..n_fixed).all(|i| cs.param_has_default(i));
                        if arg_tys.len() < min_args || !omitted_ok {
                            self.diags.error(
                                span,
                                format!(
                                    "function '{fname}' expects at least {min_args} args, got {}",
                                    arg_tys.len()
                                ),
                            );
                        } else {
                            let provided_fixed = arg_tys.len().min(n_fixed);
                            for i in 0..provided_fixed {
                                self.expect_assignable(
                                    params[i],
                                    arg_tys[i],
                                    self.span(args[i]),
                                    "argument",
                                );
                            }
                            let elem = params[n_fixed].array_elem().unwrap_or(Ty::Error);
                            for i in n_fixed..arg_tys.len() {
                                self.expect_assignable(
                                    elem,
                                    arg_tys[i],
                                    self.span(args[i]),
                                    "vararg argument",
                                );
                            }
                        }
                    } else if let Some(names) = &arg_names {
                        match map_call_sig_args(args, Some(names), cs) {
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
                        match map_call_sig_args(args, Some(&synth), cs) {
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
                    self.mark_module_top_level_call(call, &fname, &fi, ret_ty, Vec::new());
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
                    // NAMED arguments to a classpath function (`describe(count = 3, name = "hi")`): map
                    // labels through the callee's `@Metadata` names so overload resolution and
                    // per-argument checking pair against parameter slots. Lowering uses the recorded slots
                    // to preserve source evaluation order while emitting parameter order.
                    let (sel_args, arg_tys, resolved_slots): (
                        Vec<ExprId>,
                        Vec<Ty>,
                        Option<Vec<Option<ExprId>>>,
                    ) = match arg_names
                        .as_ref()
                        .filter(|ns| ns.iter().any(Option::is_some))
                    {
                        Some(names) => {
                            let pnames: Vec<Vec<String>> = crate::libraries::FunctionSet {
                                overloads: self
                                    .resolver()
                                    .resolve_symbol(
                                        crate::symbol_resolver::SymRecv::TopLevel,
                                        &fname,
                                        &[],
                                        &[],
                                    )
                                    .map(crate::symbol_resolver::Symbol::overloads)
                                    .unwrap_or_default(),
                            }
                            .into_top_level_with_param_names()
                            .map(|o| o.call_sig.param_names)
                            .collect();
                            match pnames.as_slice() {
                                [pn] => match map_call_args(args, Some(names), pn, pn.len(), &[]) {
                                    Ok(slots) if slots.iter().all(Option::is_some) => {
                                        let sa: Vec<ExprId> =
                                            slots.iter().copied().flatten().collect();
                                        let at = sa
                                            .iter()
                                            .map(|a| self.expr_types[a.0 as usize])
                                            .collect();
                                        (sa, at, Some(slots))
                                    }
                                    _ => (args.to_vec(), arg_tys.clone(), None),
                                },
                                _ => (args.to_vec(), arg_tys.clone(), None),
                            }
                        }
                        None => (args.to_vec(), arg_tys.clone(), None),
                    };
                    if let Some(c) = self
                        .resolver()
                        .resolve_symbol(
                            crate::symbol_resolver::SymRecv::TopLevel,
                            &fname,
                            &arg_tys,
                            &call_targs,
                        )
                        .and_then(crate::symbol_resolver::Symbol::top_level_call)
                    {
                        // Record the resolved callable so the lowerer emits it without re-resolving
                        // (see [`TypeInfo::resolved_top_level`]).
                        self.resolved_calls
                            .insert(call, ResolvedCall::TopLevel(c.clone()));
                        if let Some(slots) = resolved_slots {
                            self.resolved_call_arg_slots.insert(call, slots);
                        }
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
                        .find(|s| s.internal_matches(&nested))
                        .cloned()
                    {
                        // A sibling nested class — plain or `inner`. An `inner class` also needs the
                        // enclosing INSTANCE (a synthetic `this$0` the lowerer supplies from the current
                        // `this`); its source `ctor_params` (like a plain class) exclude it, so the arity
                        // check is the same. It is valid ONLY inside the enclosing instance where `this`
                        // is available — this branch requires `this_ty == outer`, and an inner class must
                        // declare exactly that outer (`inner_of == outer`) so `this` is the right instance.
                        let inner_ok = cls.inner_of_name().is_none_or(|o| o == outer);
                        if inner_ok && cls.ctor_params.len() == arg_tys.len() && arg_names.is_none()
                        {
                            self.expect_call_args(&cls.ctor_params, false, args, &arg_tys);
                            return self.ctor_result_name(call, cls.internal_name());
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
                    // A SAME-FILE object member resolves through the module (`method_of`), not the
                    // classpath — the same singleton dispatch (`ObjectMemberCall`) lowers it. A local
                    // declaration of the same name SHADOWS the import (Kotlin), so only bind the import
                    // when the module does not declare `fname` itself.
                    if !self.module_declares(&fname) {
                        if let Some(sig) = self.syms.method_of_name(internal, &fname) {
                            if sig.requires_all_args() && args.len() == sig.params.len() {
                                for (i, &a) in args.iter().enumerate() {
                                    self.expect_assignable(
                                        sig.params[i],
                                        arg_tys[i],
                                        self.span(a),
                                        "argument",
                                    );
                                }
                                self.expr_lowers
                                    .insert(call, ExprLowering::ObjectMemberCall { internal });
                                return sig.ret;
                            }
                        }
                    }
                    let internal_rendered = internal.render();
                    if let Some(m) =
                        self.resolve_instance_member(Ty::obj_name(internal), &fname, &arg_tys)
                    {
                        crate::trace_compiler!(
                            "resolve",
                            "unqualified object-member import {fname}() -> {internal_rendered}.{fname}"
                        );
                        let ret = m.ret;
                        self.expr_lowers
                            .insert(call, ExprLowering::ObjectMemberCall { internal });
                        self.resolved_calls.insert(call, ResolvedCall::Member(m));
                        return ret;
                    }
                }
                // Off-classpath `println`/`print` still type as `Unit`; the target owns the runtime shape.
                if matches!(fname.as_str(), "println" | "print")
                    && arg_tys.len() <= 1
                    && !self.module_declares(&fname)
                {
                    let params: Vec<_> = arg_tys
                        .first()
                        .map(|t| match t {
                            Ty::Short | Ty::Byte => Ty::Int,
                            t if t.is_reference() => Ty::obj("kotlin/Any"),
                            t => *t,
                        })
                        .into_iter()
                        .collect();
                    let c = crate::libraries::LibraryCallable::library(
                        "kotlin/io/ConsoleKt",
                        fname.clone(),
                        params,
                        Ty::Unit,
                        Ty::Unit,
                        "",
                    );
                    self.resolved_calls.insert(call, ResolvedCall::TopLevel(c));
                    return Ty::Unit;
                }
                // A `Type(args)` FACTORY where `Type` is a classpath class/interface whose COMPANION
                // carries an `operator fun invoke(args)`: evaluate `Type` as its companion INSTANCE (a
                // `getstatic Type.Companion` value) and dispatch as an invoke-operator on it — exactly
                // kotlinc's `Type.Companion.invoke(args)`. An interface has no constructor, so a factory
                // `invoke` is the only way to "construct" it (`Wrapped(uuid)` in production code).
                if !self.value_root_shadows_classifier(&fname) {
                    if let Some(ct) = self.classpath_companion_ty(&fname) {
                        let has_invoke = self
                            .resolve_instance_member(ct, CALLABLE_INVOKE_OPERATOR, &arg_tys)
                            .is_some_and(|m| !m.member.suspend);
                        if has_invoke {
                            // Type the callee name as the companion instance so lowering reads it as the
                            // `getstatic Type.Companion` receiver; `record_invoke` selects the operator.
                            self.set(callee, ct);
                            if let Some(t) =
                                self.record_invoke(call, callee, ct, args, &arg_tys, span)
                            {
                                return t;
                            }
                        }
                    }
                }
                if !self.module_declares(&fname) {
                    if let Some((owner_path, member)) = self.imports.get(&fname).and_then(|f| {
                        f.rsplit_once('/')
                            .map(|(o, m)| (o.to_string(), m.to_string()))
                    }) {
                        if member == fname {
                            if let Some(owner_internal) = self.nested_internal(&owner_path) {
                                if let Some(m) =
                                    self.resolve_companion(&owner_internal, &fname, &arg_tys)
                                {
                                    let owner = m.owner_name_or(&owner_internal);
                                    let phys =
                                        m.physical_name.clone().unwrap_or_else(|| m.name.clone());
                                    let mut callable = crate::libraries::LibraryCallable::library(
                                        owner,
                                        phys,
                                        m.params.clone(),
                                        m.ret,
                                        m.physical_ret,
                                        m.descriptor.clone(),
                                    );
                                    callable.suspend = m.suspend;
                                    let ret = m.ret;
                                    let vararg =
                                        m.params.last().and_then(|p| p.array_elem()).filter(|_| {
                                            m.params.len() != args.len()
                                                || arg_tys.last() != m.params.last()
                                        });
                                    if let Some(elem) = vararg {
                                        callable.vararg_elem = Some(elem);
                                        let fixed = m.params.len() - 1;
                                        for (i, &a) in args.iter().enumerate() {
                                            let pt = if i < fixed { m.params[i] } else { elem };
                                            self.expect_assignable(
                                                pt,
                                                arg_tys[i],
                                                self.span(a),
                                                "argument",
                                            );
                                        }
                                    } else {
                                        self.expect_call_args(&m.params, false, args, &arg_tys);
                                    }
                                    self.resolved_calls
                                        .insert(call, ResolvedCall::TopLevel(callable));
                                    return ret;
                                }
                            }
                        }
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
                let arg_tys = self.arg_tys(args);
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
        if (a == Ty::Unit && b == Ty::Null) || (b == Ty::Unit && a == Ty::Null) {
            return Ty::nullable(Ty::Unit);
        }
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
        // `List<D>` → `List<*>`), differing only in NULLABILITY to its nullable form (`C` and `C?` → `C?`).
        // Comparing the NON-NULL forms covers the mixed case (`x ?: y` where one side is `C` and the other
        // `C?` — e.g. a map get typed `C` elvis a nullable member return `C?`), which the bare-`Obj` match
        // missed, collapsing it to `Any`.
        if let (Some(ai), Some(bi)) = (a.non_null().obj_internal(), b.non_null().obj_internal()) {
            if ai == bi {
                let base = Ty::obj_name(ai);
                return if a.is_nullable() || b.is_nullable() {
                    Ty::nullable(base)
                } else {
                    base
                };
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
    /// The return type of a user `inc`/`dec` operator (member first, then extension) on a receiver of
    /// type `ty`, if one exists — used to type an overloaded `x++`/`x--` on a non-numeric variable. A
    /// no-arg member/extension `inc`()/`dec`() only.
    fn inc_dec_operator_ret(&self, ty: Ty, dec: bool) -> Option<Ty> {
        let name = if dec { "dec" } else { "inc" };
        if let Some(m) = crate::module_symbols::ModuleSymbols::new(self.syms)
            .instance_members(ty, name)
            .into_iter()
            .find(|m| m.params.is_empty())
        {
            return Some(m.ret);
        }
        self.syms
            .ext_fun_overloads(ty, name)
            .iter()
            .find(|sig| sig.params.is_empty())
            .map(|sig| sig.ret)
    }

    /// Detect a USER-defined `op`Assign operator (member or extension), type-check its argument, and mark
    /// it for lowerer emission. Classpath `+=` keeps the existing `target = target + rhs` lowering.
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
        let param = crate::module_symbols::ModuleSymbols::new(self.syms)
            .instance_members(recv, aname)
            .into_iter()
            .find_map(|m| (m.params.len() == 1).then(|| m.params[0]))
            .or_else(|| {
                self.syms
                    .ext_fun_overloads(recv, aname)
                    .iter()
                    .find_map(|sig| sig.single_param())
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
        if rt != Ty::Error && matches!(recv, Ty::Obj(..)) {
            // Record the (inline) `plusAssign` extension callable keyed by the target expr for the lowerer.
            self.record_synthetic_ext(lhs, aname, recv, &[rt]);
        }
        if rt != Ty::Error
            && matches!(recv, Ty::Obj(..))
            && self
                .synthetic_ext_calls
                .contains_key(&(lhs, aname.to_string()))
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
                // An UNRESOLVED annotation must fail the file, not silently bind `Error`: the local
                // would otherwise take its initializer's shape while every use site's checks are
                // Error-suppressed — a cross-module `val b: Bar<String> = { "OK" }` (alias declared in
                // another module) then SAM-converts the lambda by ITS OWN arity and miscompiles
                // (IncompatibleClassChangeError at the call expecting the annotated shape).
                if let (Some(Ty::Error), Some(r)) = (declared, ty.as_ref()) {
                    self.diags
                        .error(r.span, format!("unresolved reference '{}'.", r.name));
                }
                // An initializer with a declared FUNCTION type takes its parameter types from the
                // annotation, so `val f: (Int) -> Int = { it * 2 }` types `it`/`x` as `Int` (not the
                // erased `Object`). Propagating the expectation also reaches a lambda that is the
                // result of a nested `if`/`when`/block initializer. HOF *arguments* already do this.
                let it = match declared {
                    Some(d @ Ty::Fun(_)) => self.expr_expected(init, d),
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
                // Flow-narrow a nullable `var` whose initializer is a non-null value (`var x: Int? = 10`
                // reads as `Int`), matching kotlinc's smart-cast, but only when the var is not written
                // inside a closure that could reset it to null on a deferred path. A `val` is left
                // un-narrowed: narrowing a nullable-PRIMITIVE `val` to its unboxed form would change the
                // physical representation an identity `===`/safe-call/`!!` still relies on (the value
                // stays boxed), so those keep the declared type — kotlinc narrows in the type system
                // without changing the boxed storage, which this backend can't reproduce for a `val`.
                if is_var
                    && bind.is_nullable()
                    && !it.is_nullable()
                    && !matches!(it, Ty::Null | Ty::Error)
                    && it != Ty::Nothing
                    && !self.fn_closure_reassigned.contains(&name)
                {
                    self.set_local_narrow(&name, Some(it));
                }
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
                let delegate_ret = self.record_delegate_getvalue(delegate, dt);
                let prop_ty = match ty.as_ref() {
                    Some(r) => {
                        let t = self.resolve_ty(r);
                        // Same rule as `Stmt::Local`: an unresolved annotation errors instead of
                        // silently binding `Error` (every use-site check would be suppressed).
                        if t == Ty::Error {
                            self.diags
                                .error(r.span, format!("unresolved reference '{}'.", r.name));
                        }
                        t
                    }
                    None => delegate_ret.unwrap_or(Ty::Error),
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
                // Same rule as `Stmt::Local`: an unresolved annotation errors, never a silent `Error`.
                if prop_ty == Ty::Error {
                    self.diags
                        .error(ty.span, format!("unresolved reference '{}'.", ty.name));
                }
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
                let source_props = self.file.destructure_source_props.get(&s.0).cloned();
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
                    // NAME-BASED entry (`val (newName = sourceProp) = src`): bind to the receiver's
                    // `sourceProp` property (a member read), not `componentN`.
                    if let Some(prop) = source_props
                        .as_ref()
                        .and_then(|sp| sp.get(idx))
                        .and_then(|o| o.as_ref())
                    {
                        // The source property's type: a USER-class field (`lookup_prop`), else a library
                        // member property (`IndexedValue.value`) resolved through the platform's property
                        // seam — which knows its own getter naming/`@JvmName` mangling.
                        let target = self.module_property_getter_target(it, prop).or_else(|| {
                            self.resolve_property_member(it, prop)
                                .map(DestructureComponentTarget::LibraryMember)
                        });
                        match target {
                            Some(target) => {
                                let t = target.ret();
                                self.resolved_destructure_components
                                    .insert((s, idx), target);
                                self.declare(name, t, *is_var);
                            }
                            None => {
                                self.diags.error(
                                    span,
                                    format!(
                                        "krusty: unresolved property '{prop}' in destructuring"
                                    ),
                                );
                                self.declare(name, Ty::Error, *is_var);
                            }
                        }
                        continue;
                    }
                    let comp = format!("component{}", idx + 1);
                    let target = self
                        .destructure_component_target(it, &comp, &[])
                        .or_else(|| internal.and_then(|_| self.destructure_indexed_get_target(it)));
                    match target {
                        Some(target) => {
                            let t = target.ret();
                            self.resolved_destructure_components
                                .insert((s, idx), target);
                            self.declare(name, t, *is_var);
                        }
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
            Stmt::IncDec { name, dec } => {
                // `inc`/`dec` are overloadable operators: the built-in numeric ones, or a user
                // `inc`/`dec` operator on the variable's type. Anything else is rejected (never
                // miscompiled).
                let span = self.file.stmt_spans[s.0 as usize];
                let inherited = || {
                    if let Some(Ty::Obj(internal, _)) = self.this_ty {
                        self.lookup_prop_name(internal, &name)
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
                        if !ty.is_numeric_or_char() && self.inc_dec_operator_ret(ty, dec).is_none()
                        {
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
                            // Flow-narrow a nullable `var` to the assigned value's non-null type
                            // (`var x: Int?; x = 10` → reads as `Int`), matching kotlinc's smart-cast.
                            // Only when the value is genuinely non-null, and the `var` is never written
                            // inside a closure (which could reset it to null on a deferred path). A
                            // reassignment that is nullable (or the var is closure-written) clears any
                            // prior narrowing so a later read widens back to the declared type.
                            if is_var {
                                let narrow = lty.is_nullable()
                                    && !vt.is_nullable()
                                    && !matches!(vt, Ty::Null | Ty::Error)
                                    && vt != Ty::Nothing
                                    && !self.fn_closure_reassigned.contains(&name);
                                self.set_local_narrow(&name, narrow.then_some(vt));
                            }
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
                                self.lookup_prop_name(internal, &name)
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
                if let Some((lty, is_var)) = self.syms.ext_prop(rt, &name) {
                    if !is_var {
                        self.diags
                            .error(span, "'val' cannot be reassigned.".to_string());
                    }
                    self.expect_assignable(lty, vt, span, "assignment");
                } else {
                    match rt {
                        Ty::Error => {}
                        Ty::Obj(internal, _) => {
                            if let Some((lty, is_var)) = self.syms.prop_of_name(internal, &name) {
                                if !is_var {
                                    self.diags
                                        .error(span, "'val' cannot be reassigned.".to_string());
                                }
                                self.expect_assignable(lty, vt, span, "assignment");
                            } else if let Some(setter) = self.resolve_property_setter(rt, &name) {
                                // A `var` member of a CLASSPATH type: its setter comes from `@Metadata`
                                // (the `properties` query), not the user-declared `props` map. A setter
                                // existing means the property is a `var`; the value is checked against
                                // the setter's parameter type. (A classpath `val` exposes no setter →
                                // falls to the error below, as before.)
                                let pty = setter.params.first().copied().unwrap_or(Ty::Error);
                                self.expect_assignable(pty, vt, span, "assignment");
                                self.property_setters.insert(s, setter);
                            } else {
                                self.diags.error(
                                    span,
                                    format!("unresolved member '{name}' on '{}'", rt.name()),
                                );
                            }
                        }
                        _ => self.diags.error(
                            span,
                            format!("cannot assign to a member of '{}'", rt.name()),
                        ),
                    }
                }
            }
            Stmt::AssignIndex {
                array,
                indices,
                value,
            } => {
                // `a[i] = v` stores an array element; `recv[i, j, …] = v` calls `set` (or Map `put`).
                let at = self.expr(array);
                let its: Vec<Ty> = indices.iter().map(|&i| self.expr(i)).collect();
                let vt = self.expr(value);
                let span = self.file.stmt_spans[s.0 as usize];
                let single_index = matches!(indices.as_slice(), [_]);
                if single_index {
                    if let Some(elem) = at.array_elem() {
                        self.expect_assignable(Ty::Int, its[0], span, "array index");
                        self.expect_assignable(elem, vt, span, "array element assignment");
                        return;
                    }
                }
                if at == Ty::Error {
                    return;
                }
                let mut set_args = its.clone();
                set_args.push(vt);
                let mut set_exprs = indices.clone();
                set_exprs.push(value);
                // Resolve `set` as a member, same-module extension, or library member. A single-index Map
                // store may resolve to `put`. Record the selected target so lowering does not choose again.
                let selected = self
                    .operator_call_ret(at, "set", &set_args, &set_exprs)
                    .map(|(_, call)| (SyntheticOperatorCall::Set, call))
                    .or_else(|| {
                        single_index
                            .then(|| self.operator_call_ret(at, "put", &set_args, &set_exprs))
                            .flatten()
                            .map(|(_, call)| (SyntheticOperatorCall::Put, call))
                    });
                let ok = if let Some((op, call)) = selected {
                    if single_index && matches!(&call, ResolvedCall::Member(_)) {
                        if let Some(get) = self.resolve_instance_member(at, "get", &[its[0]]) {
                            self.resolved_index_store_get_returns.insert(s, get.ret);
                        }
                    }
                    self.resolved_stmt_operator_calls.insert((s, op), call);
                    true
                } else {
                    false
                };
                if !ok && at != Ty::Error {
                    self.diags.error(
                        span,
                        if single_index {
                            format!("'{}' is not an array (cannot index-assign)", at.name())
                        } else {
                            format!(
                                "no 'set' operator taking {} indices on '{}'",
                                its.len(),
                                at.name()
                            )
                        },
                    );
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
                let elem = if st == et {
                    st.range_counter_type()
                } else {
                    None
                }
                .unwrap_or_else(|| {
                    self.expect_assignable(Ty::Int, st, self.span(range.start), "range start");
                    self.expect_assignable(Ty::Int, et, self.span(range.end), "range end");
                    Ty::Int
                });
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
                // An array element type covers primitive arrays and a boxed `Array<T>`
                // (`Obj("kotlin/Array", [T])`) — iterate either as an array.
                let elem = if let Some(e) = it.array_elem() {
                    e
                } else {
                    match it {
                        Ty::String => Ty::Char, // iterating a String yields its chars
                        Ty::Error => Ty::Error,
                        Ty::Obj(_, _) => match self.record_iterator_protocol(iterable, it) {
                            Some(elem) => elem,
                            None => {
                                self.diags.error(self.span(iterable), format!("krusty: 'for' over '{}' is not supported (only arrays, String, and Iterables)", it.name()));
                                Ty::Error
                            }
                        },
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
                // A `kotlin.contracts.contract { … }` statement is erased metadata: it is never
                // executed and produces no bytecode (kotlinc drops it). Its lambda body uses the
                // `ContractBuilder` DSL (`callsInPlace`/`returns`/`implies`) which isn't ordinary
                // executable code — skip type-checking it and mark the statement for the lowerer to
                // drop, instead of resolving the DSL members as if they were real calls.
                if self.is_contract_call(e) {
                    self.stmt_lowers.insert(s, StmtLowering::Erased);
                } else {
                    self.expr(e);
                }
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
        // shared is marked explicitly; later lowering/backend code chooses the holder representation.
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
            param_default_values: Vec::new(),
            param_names: f.params.iter().map(|p| p.name.clone()).collect(),
            lambda_param_types: Vec::new(),
            lambda_recv: Vec::new(),
            is_inline: false,
            is_final: false,
            is_suspend: f.is_suspend,
            context_count: f.context_count,
            source_decl: None,
            source_file: None,
            package: String::new(),
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
        self.with_ret(ret_ty, |c| {
            c.push_local_funs();
            c.push_scope();
            for (p, &ty) in f.params.iter().zip(&params) {
                c.declare(&p.name, ty, false);
            }
            match &f.body.clone() {
                FunBody::Expr(e) => {
                    // Already checked above for inference; re-check to fill in expr_types.
                    let t = c.expr(*e);
                    c.expect_assignable(ret_ty, t, c.span(*e), "local function body");
                }
                FunBody::Block(b) => {
                    let _ = c.expr(*b);
                }
                FunBody::None => {}
            }
            c.pop_scope();
            c.pop_local_funs();
        });
        for t in added_tparams {
            self.tparams.remove(&t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::LangFeatures;
    use crate::lexer::lex;
    use crate::parser::{parse, parse_with_features};

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
    fn local_function_calls_record_resolved_target_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file(
            r#"
fun box(): String {
    val base = 40
    fun f(x: Int = 2) = base + x
    return f().toString()
}
"#,
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );

        let call = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::Call { callee, .. } => match files[0].expr(*callee) {
                    Expr::Name(name) if name == "f" => Some(ExprId(idx as u32)),
                    _ => None,
                },
                _ => None,
            })
            .expect("source should contain f() local function call");
        let target_stmt = files[0]
            .stmt_arena
            .iter()
            .enumerate()
            .find_map(|(idx, stmt)| match stmt {
                Stmt::LocalFun(f) if f.name == "f" => Some(StmtId(idx as u32)),
                _ => None,
            })
            .expect("source should contain f local function declaration");

        let Some(ResolvedCall::LocalFunction(target)) = info.resolved_calls.get(&call) else {
            panic!("checker must record the selected local function target for lowering");
        };
        assert_eq!(target.stmt_id, target_stmt);
        assert_eq!(target.sig.params, vec![Ty::Int]);
        assert_eq!(target.sig.ret, Ty::Int);
        assert_eq!(target.provided_arg_count, 0);
        assert!(target.context_args.is_empty());
        assert!(
            !matches!(
                info.expr_lowers.get(&call),
                Some(ExprLowering::LocalFunction { .. })
            ),
            "ordinary local function calls must not duplicate the target in expr_lowers"
        );
    }

    #[test]
    fn local_function_calls_record_shadowed_declaration_target() {
        let mut d = DiagSink::new();
        let file = parse_file(
            r#"
fun box(): String {
    fun f(x: Int = 1) = x
    fun g(): Int {
        fun f(x: Int = 2) = x + 10
        return f()
    }
    return g().toString()
}
"#,
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );

        let inner_f_stmt = files[0]
            .stmt_arena
            .iter()
            .enumerate()
            .filter_map(|(idx, stmt)| match stmt {
                Stmt::LocalFun(f) if f.name == "f" => Some(StmtId(idx as u32)),
                _ => None,
            })
            .next_back()
            .expect("source should contain inner f local function declaration");
        let f_call = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::Call { callee, .. } => match files[0].expr(*callee) {
                    Expr::Name(name) if name == "f" => Some(ExprId(idx as u32)),
                    _ => None,
                },
                _ => None,
            })
            .expect("source should contain f() local function call");

        let Some(ResolvedCall::LocalFunction(target)) = info.resolved_calls.get(&f_call) else {
            panic!("checker must record the selected local function target for lowering");
        };
        assert_eq!(target.stmt_id, inner_f_stmt);
        assert_eq!(target.provided_arg_count, 0);
    }

    #[test]
    fn local_value_root_shadows_classifier_member_paths() {
        let mut d = DiagSink::new();
        let file = parse_file(
            r#"
enum class Kind { PENDING }
class Holder(val PENDING: String)
fun box(): String {
    val Kind = Holder("OK")
    return Kind.PENDING
}
"#,
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );
        let member = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::Member { name, .. } if name == "PENDING" => Some(ExprId(idx as u32)),
                _ => None,
            })
            .expect("source should contain Kind.PENDING member read");
        assert_eq!(info.ty(member), Ty::String);
        assert!(
            info.resolved_library_enum_entry_owner(member).is_none(),
            "local value root must not be recorded as an enum-entry classifier path"
        );
    }

    #[test]
    fn local_value_named_like_class_blocks_constructor_fallback() {
        let (errs, _) = check(
            r#"
class Foo
fun box(): String {
    val Foo = 1
    Foo()
    return "OK"
}
"#,
        );
        assert!(
            errs.iter().any(|e| e.contains("unresolved function 'Foo'")),
            "expected local value to shadow class constructor, got {errs:?}"
        );
    }

    fn parse_file(src: &str, d: &mut DiagSink) -> File {
        let toks = lex(src, d);
        parse(src, &toks, d)
    }

    fn parse_file_with_detected_features(src: &str, d: &mut DiagSink) -> File {
        let toks = lex(src, d);
        let features = LangFeatures::from_source(src);
        parse_with_features(src, &toks, d, &features)
    }

    fn assert_no_diags(d: &DiagSink) {
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );
    }

    fn named_call(file: &File, name: &str) -> ExprId {
        file.expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::Call { callee, .. } => match file.expr(*callee) {
                    Expr::Name(callee_name) if callee_name == name => Some(ExprId(idx as u32)),
                    _ => None,
                },
                _ => None,
            })
            .unwrap_or_else(|| panic!("source should contain {name}() call"))
    }

    fn module_top_level_target(info: &TypeInfo, call: ExprId) -> &ResolvedModuleTopLevelCall {
        let Some(ResolvedCall::ModuleTopLevel(target)) = info.resolved_calls.get(&call) else {
            panic!("checker must record the selected same-module top-level target for lowering");
        };
        target
    }

    fn top_level_fun_decl(
        file: &File,
        name: &str,
        matches_decl: impl Fn(&FunDecl) -> bool,
    ) -> DeclId {
        file.decls
            .iter()
            .copied()
            .find(|&decl| matches!(file.decl(decl), Decl::Fun(f) if f.name == name && matches_decl(f)))
            .unwrap_or_else(|| panic!("source should contain {name} declaration"))
    }

    struct FakeMemberPlatform;

    impl crate::symbol_source::SymbolSource for FakeMemberPlatform {
        fn resolve_symbols(&self, fqn: &str) -> crate::libraries::ResolvedSymbols {
            if matches!(
                fqn,
                "BoxedComparable" | "BoxedIndex" | "BoxedIterable" | "BoxedIterator" | "TestMutex"
            ) {
                return crate::libraries::ResolvedSymbols {
                    classifier: self.resolve_type(fqn).map(std::rc::Rc::new),
                    callables: crate::libraries::Callables::None,
                };
            }
            let (kind, receiver, owner, name, params, ret, descriptor) = match fqn {
                "withLock" | "kotlinx/coroutines/sync/withLock" => (
                    crate::libraries::FnKind::Extension,
                    Some(Ty::obj("TestMutex")),
                    "kotlinx/coroutines/sync/MutexKt",
                    "withLock",
                    vec![
                        Ty::obj("TestMutex"),
                        Ty::obj("kotlin/Any"),
                        Ty::fun(vec![], Ty::Int),
                    ],
                    Ty::Int,
                    "(LTestMutex;Ljava/lang/Object;Lkotlin/jvm/functions/Function0;Lkotlin/coroutines/Continuation;)Ljava/lang/Object;",
                ),
                "knownTop" => (
                    crate::libraries::FnKind::TopLevel,
                    None,
                    "test/TopKt",
                    "knownTop",
                    vec![Ty::String, Ty::Int],
                    Ty::String,
                    "(Ljava/lang/String;I)Ljava/lang/String;",
                ),
                "Foo" => (
                    crate::libraries::FnKind::TopLevel,
                    None,
                    "test/TopKt",
                    "Foo",
                    vec![Ty::String],
                    Ty::String,
                    "(Ljava/lang/String;)Ljava/lang/String;",
                ),
                "test/getValue" => (
                    crate::libraries::FnKind::Extension,
                    Some(Ty::obj("test/Delegate")),
                    "test/DelegateKt",
                    "getValue",
                    vec![
                        Ty::obj("test/Delegate"),
                        Ty::nullable(Ty::obj("kotlin/Any")),
                        Ty::obj("kotlin/reflect/KProperty"),
                    ],
                    Ty::String,
                    "(LDelegate;Ljava/lang/Object;Lkotlin/reflect/KProperty;)Ljava/lang/String;",
                ),
                _ => return crate::libraries::ResolvedSymbols::default(),
            };
            let callable = crate::libraries::LibraryCallable::library(
                owner, name, params, ret, ret, descriptor,
            );
            let mut info = crate::libraries::FunctionInfo::plain(kind, receiver, callable);
            if name == "withLock" {
                info.flags.inline = crate::libraries::InlineKind::CanInline;
                info.flags.suspend = true;
                info.callable.inline = crate::libraries::InlineKind::CanInline;
                info.callable.suspend = true;
            }
            info.call_sig = CallSig {
                param_names: if name == "knownTop" {
                    vec!["a".to_string(), "b".to_string()]
                } else if name == "getValue" {
                    vec!["thisRef".to_string(), "property".to_string()]
                } else if name == "withLock" {
                    vec!["owner".to_string(), "action".to_string()]
                } else {
                    vec!["s".to_string()]
                },
                param_defaults: if name == "withLock" {
                    vec![true, false]
                } else {
                    Vec::new()
                },
                lambda_param_types: if name == "withLock" {
                    vec![Vec::new(), Vec::new()]
                } else {
                    Vec::new()
                },
                required: match name {
                    "knownTop" | "getValue" => 2,
                    _ => 1,
                },
                ..Default::default()
            };
            crate::libraries::ResolvedSymbols {
                classifier: None,
                callables: crate::libraries::Callables::Functions(crate::libraries::FunctionSet {
                    overloads: vec![info],
                }),
            }
        }

        fn resolve_type(&self, internal: &str) -> Option<crate::libraries::LibraryType> {
            matches!(
                internal,
                "kotlin/String"
                    | "BoxedComparable"
                    | "BoxedIndex"
                    | "BoxedIterable"
                    | "BoxedIterator"
                    | "TestMutex"
            )
            .then(|| crate::libraries::LibraryType {
                is_public: true,
                kind: crate::libraries::TypeKind::Class,
                supertypes: crate::types::TypeNameList::new(),
                constructors: vec![],
                members: vec![],
                companion: vec![],
                companion_consts: HashMap::new(),
                sam_method: None,
                companion_object: None,
                value_companion_fns: vec![],
                value_underlying: None,
                alias_target: None,
                type_params: vec![],
                sealed_subclasses: crate::types::TypeNameList::new(),
                enum_entries: vec![],
                value_ctor_has_default: false,
                ctor_named_params: vec![],
                value_class_properties: vec![],
                retention: None,
            })
        }

        fn member_overloads(&self, recv: Ty, name: &str) -> crate::libraries::FunctionSet {
            if recv == Ty::obj("TestMutex") && matches!(name, "lock" | "unlock") {
                let callable = crate::libraries::LibraryCallable::library(
                    "TestMutex",
                    name,
                    vec![Ty::obj("kotlin/Any")],
                    Ty::Unit,
                    Ty::Unit,
                    "(Ljava/lang/Object;)V",
                );
                let mut info = crate::libraries::FunctionInfo::plain(
                    crate::libraries::FnKind::Member,
                    Some(Ty::obj("TestMutex")),
                    callable,
                );
                info.flags.suspend = name == "lock";
                return crate::libraries::FunctionSet {
                    overloads: vec![info],
                };
            }
            if recv == Ty::obj("BoxedComparable") && name == "compareTo" {
                let callable = crate::libraries::LibraryCallable::library(
                    "BoxedComparable",
                    "compareTo",
                    vec![Ty::obj("BoxedComparable")],
                    Ty::Int,
                    Ty::Int,
                    "(LBoxedComparable;)I",
                );
                return crate::libraries::FunctionSet {
                    overloads: vec![crate::libraries::FunctionInfo::plain(
                        crate::libraries::FnKind::Member,
                        Some(Ty::obj("BoxedComparable")),
                        callable,
                    )],
                };
            }
            if recv == Ty::obj("BoxedIndex") && name == "get" {
                let callable = crate::libraries::LibraryCallable::library(
                    "BoxedIndex",
                    "get",
                    vec![Ty::Int],
                    Ty::String,
                    Ty::String,
                    "(I)Ljava/lang/String;",
                );
                return crate::libraries::FunctionSet {
                    overloads: vec![crate::libraries::FunctionInfo::plain(
                        crate::libraries::FnKind::Member,
                        Some(Ty::obj("BoxedIndex")),
                        callable,
                    )],
                };
            }
            if recv == Ty::obj("BoxedIndex") && name == "set" {
                let callable = crate::libraries::LibraryCallable::library(
                    "BoxedIndex",
                    "set",
                    vec![Ty::Int, Ty::String],
                    Ty::Unit,
                    Ty::Unit,
                    "(ILjava/lang/String;)V",
                );
                return crate::libraries::FunctionSet {
                    overloads: vec![crate::libraries::FunctionInfo::plain(
                        crate::libraries::FnKind::Member,
                        Some(Ty::obj("BoxedIndex")),
                        callable,
                    )],
                };
            }
            if recv == Ty::obj("BoxedIterable") && name == "iterator" {
                let iter_ty = Ty::obj_args("BoxedIterator", &[Ty::String]);
                let callable = crate::libraries::LibraryCallable::library(
                    "BoxedIterable",
                    "iterator",
                    vec![],
                    iter_ty,
                    iter_ty,
                    "()LBoxedIterator;",
                );
                return crate::libraries::FunctionSet {
                    overloads: vec![crate::libraries::FunctionInfo::plain(
                        crate::libraries::FnKind::Member,
                        Some(Ty::obj("BoxedIterable")),
                        callable,
                    )],
                };
            }
            if recv
                .obj_internal()
                .is_some_and(|n| n.matches("BoxedIterator"))
                && name == "hasNext"
            {
                let callable = crate::libraries::LibraryCallable::library(
                    "BoxedIterator",
                    "hasNext",
                    vec![],
                    Ty::Boolean,
                    Ty::Boolean,
                    "()Z",
                );
                return crate::libraries::FunctionSet {
                    overloads: vec![crate::libraries::FunctionInfo::plain(
                        crate::libraries::FnKind::Member,
                        Some(recv),
                        callable,
                    )],
                };
            }
            if recv
                .obj_internal()
                .is_some_and(|n| n.matches("BoxedIterator"))
                && name == "next"
            {
                let callable = crate::libraries::LibraryCallable::library(
                    "BoxedIterator",
                    "next",
                    vec![],
                    Ty::String,
                    Ty::String,
                    "()Ljava/lang/String;",
                );
                return crate::libraries::FunctionSet {
                    overloads: vec![crate::libraries::FunctionInfo::plain(
                        crate::libraries::FnKind::Member,
                        Some(recv),
                        callable,
                    )],
                };
            }
            if recv == Ty::String && name == "get" {
                let callable = crate::libraries::LibraryCallable::library(
                    "kotlin/String",
                    "get",
                    vec![Ty::Int],
                    Ty::Char,
                    Ty::Char,
                    "(I)C",
                );
                return crate::libraries::FunctionSet {
                    overloads: vec![crate::libraries::FunctionInfo::plain(
                        crate::libraries::FnKind::Member,
                        Some(Ty::String),
                        callable,
                    )],
                };
            }
            if recv != Ty::String || !matches!(name, "known" | "choose" | "tie") {
                return crate::libraries::FunctionSet::default();
            }
            let member = |params: Vec<Ty>, descriptor: &'static str, defaults: Vec<bool>| {
                let callable = crate::libraries::LibraryCallable::library(
                    "test/Host",
                    name,
                    params,
                    Ty::String,
                    Ty::String,
                    descriptor,
                );
                let mut info = crate::libraries::FunctionInfo::plain(
                    crate::libraries::FnKind::Member,
                    Some(Ty::String),
                    callable,
                );
                info.call_sig = CallSig {
                    param_names: if defaults.len() == 1 {
                        vec!["a".to_string()]
                    } else {
                        vec!["a".to_string(), "b".to_string()]
                    },
                    param_defaults: defaults,
                    required: 1,
                    ..Default::default()
                };
                info
            };
            let overloads = match name {
                "known" => vec![member(
                    vec![Ty::Int, Ty::Int],
                    "(II)Ljava/lang/String;",
                    vec![false, true],
                )],
                "choose" => vec![
                    member(vec![Ty::Boolean], "(Z)Ljava/lang/String;", vec![false]),
                    member(vec![Ty::Int], "(I)Ljava/lang/String;", vec![false]),
                ],
                "tie" => vec![
                    member(vec![Ty::Long], "(J)Ljava/lang/String;", vec![false]),
                    member(vec![Ty::Byte], "(B)Ljava/lang/String;", vec![false]),
                ],
                _ => Vec::new(),
            };
            crate::libraries::FunctionSet { overloads }
        }
    }

    impl SemanticPlatform for FakeMemberPlatform {}

    #[test]
    fn suspend_inline_expansion_records_synthetic_member_targets_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "suspend fun f(m: TestMutex): Int = m.withLock { 1 }",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures_with_cp(&files, Box::new(FakeMemberPlatform), &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert_no_diags(&d);

        let call = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::Call { callee, .. } => match files[0].expr(*callee) {
                    Expr::Member { name, .. } if name == "withLock" => Some(ExprId(idx as u32)),
                    _ => None,
                },
                _ => None,
            })
            .expect("source should contain m.withLock { ... } call");

        let ext = info
            .resolved_extension(call)
            .expect("checker must record the selected suspend inline extension");
        assert_eq!(ext.name, "withLock");
        assert!(ext.owner.starts_with("kotlinx/coroutines/sync/"));

        let lock = info
            .synthetic_member_call(call, "lock")
            .expect("checker must record the synthetic lock member target");
        assert_eq!(lock.name, "lock");
        assert_eq!(lock.params, vec![Ty::obj("kotlin/Any")]);
        assert!(lock.suspend);

        let unlock = info
            .synthetic_member_call(call, "unlock")
            .expect("checker must record the synthetic unlock member target");
        assert_eq!(unlock.name, "unlock");
        assert_eq!(unlock.params, vec![Ty::obj("kotlin/Any")]);
        assert!(!unlock.suspend);
    }

    #[test]
    fn same_simple_name_in_different_packages_is_not_a_conflict() {
        // Two files declaring a class (and a top-level function) of the same simple name in
        // DIFFERENT packages are distinct declarations, not a "conflicting declarations" clash —
        // exactly the shape produced when the coroutine-helper source (`package helpers`) is
        // injected alongside a box test that redeclares `EmptyContinuation` / `runBlocking` in the
        // root package. The dedup keys are package-qualified, so this collects without error.
        let mut d = DiagSink::new();
        let a = parse_file("class EmptyContinuation\nfun runBlocking() {}", &mut d);
        let b = parse_file(
            "package helpers\nclass EmptyContinuation\nfun runBlocking() {}",
            &mut d,
        );
        let files = vec![a, b];
        let _ = collect_signatures(&files, &mut d);
        let errs: Vec<String> = d.diags.iter().map(|x| x.msg.clone()).collect();
        assert!(
            errs.is_empty(),
            "cross-package homonyms wrongly flagged: {errs:?}"
        );
    }

    #[test]
    fn same_package_duplicate_class_and_fun_still_conflicts() {
        // The guard stays sound: a genuine same-package duplicate (same internal name / same erased
        // signature) is still reported.
        let mut d = DiagSink::new();
        let a = parse_file("class Dup\nfun f() {}", &mut d);
        let b = parse_file("class Dup\nfun f() {}", &mut d);
        let files = vec![a, b];
        let _ = collect_signatures(&files, &mut d);
        let errs: Vec<String> = d.diags.iter().map(|x| x.msg.clone()).collect();
        assert!(
            errs.iter()
                .any(|e| e.contains("conflicting declarations: Dup")),
            "expected class conflict, got {errs:?}"
        );
        assert!(
            errs.iter()
                .any(|e| e.contains("conflicting declarations: f")),
            "expected fun conflict, got {errs:?}"
        );
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

    #[test]
    fn classpath_property_reads_record_resolved_members_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "val String.length: String get() = \"bad\"\n\
             fun len(s: String, n: String?): Int = s.length + (n?.length ?: 0)",
            &mut d,
        );
        let files = vec![file];
        let cp = std::rc::Rc::new(crate::jvm::classpath::Classpath::new(vec![]));
        let mut syms = collect_signatures_with_cp(
            &files,
            Box::new(crate::jvm::jvm_libraries::JvmLibraries::new(cp)),
            &mut d,
        );
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );
        let member = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::Member { name, .. } if name == "length" => Some(ExprId(idx as u32)),
                _ => None,
            })
            .expect("source should contain s.length member expression");
        assert!(
            matches!(
                info.resolved_calls.get(&member),
                Some(ResolvedCall::Member(_))
            ),
            "checker must record the selected classpath getter for lowering"
        );
        let safe_call = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::SafeCall {
                    name, args: None, ..
                } if name == "length" => Some(ExprId(idx as u32)),
                _ => None,
            })
            .expect("source should contain n?.length safe-call expression");
        assert!(
            matches!(
                info.resolved_calls.get(&safe_call),
                Some(ResolvedCall::Member(_))
            ),
            "checker must record the selected classpath getter for safe-call lowering"
        );
    }

    #[test]
    fn classpath_member_calls_record_resolved_members_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "fun direct(s: String): String = s.known(b = 2, a = 1)\n\
             fun String.implicit(): String = known(a = 2)\n\
             fun overloaded(s: String): String = s.choose(a = 1)",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures_with_cp(&files, Box::new(FakeMemberPlatform), &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );

        let direct = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::Call { callee, .. } => match files[0].expr(*callee) {
                    Expr::Member { name, .. } if name == "known" => Some(ExprId(idx as u32)),
                    _ => None,
                },
                _ => None,
            })
            .expect("source should contain s.known() call");
        assert!(
            matches!(
                info.resolved_calls.get(&direct),
                Some(ResolvedCall::Member(_))
            ),
            "checker must record direct classpath member calls for lowering"
        );
        let direct_slots = info
            .resolved_call_arg_slots
            .get(&direct)
            .expect("direct named member call should record argument slots");
        let direct_values: Vec<_> = direct_slots
            .iter()
            .map(|slot| {
                slot.and_then(|arg| match files[0].expr(arg) {
                    Expr::IntLit(v) => Some(*v),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(direct_values, vec![Some(1), Some(2)]);

        let implicit = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::Call { callee, .. } => match files[0].expr(*callee) {
                    Expr::Name(name) if name == "known" => Some(ExprId(idx as u32)),
                    _ => None,
                },
                _ => None,
            })
            .expect("source should contain implicit known() call");
        assert!(
            matches!(
                info.resolved_calls.get(&implicit),
                Some(ResolvedCall::Member(_))
            ),
            "checker must record implicit-receiver classpath member calls for lowering"
        );
        let implicit_slots = info
            .resolved_call_arg_slots
            .get(&implicit)
            .expect("implicit defaulted member call should record argument slots");
        let implicit_values: Vec<_> = implicit_slots
            .iter()
            .map(|slot| {
                slot.and_then(|arg| match files[0].expr(arg) {
                    Expr::IntLit(v) => Some(*v),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(implicit_values, vec![Some(2), None]);

        let overloaded = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::Call { callee, .. } => match files[0].expr(*callee) {
                    Expr::Member { name, .. } if name == "choose" => Some(ExprId(idx as u32)),
                    _ => None,
                },
                _ => None,
            })
            .expect("source should contain s.choose() call");
        let Some(ResolvedCall::Member(selected)) = info.resolved_calls.get(&overloaded) else {
            panic!("checker must record overloaded classpath member calls for lowering");
        };
        assert_eq!(selected.member.params, vec![Ty::Int]);
    }

    #[test]
    fn classpath_compare_operator_records_resolved_member_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "fun less(a: BoxedComparable, b: BoxedComparable): Boolean = a < b",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures_with_cp(&files, Box::new(FakeMemberPlatform), &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );

        let comparison = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::Binary { op: BinOp::Lt, .. } => Some(ExprId(idx as u32)),
                _ => None,
            })
            .expect("source should contain a < b comparison");
        let Some(ResolvedCall::Member(selected)) = info.resolved_calls.get(&comparison) else {
            panic!("checker must record classpath compareTo selected for relational lowering");
        };
        assert_eq!(selected.member.name, "compareTo");
        assert_eq!(selected.ret, Ty::Int);
    }

    #[test]
    fn classpath_index_operator_records_resolved_member_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file("fun first(xs: BoxedIndex): String = xs[0]", &mut d);
        let files = vec![file];
        let mut syms = collect_signatures_with_cp(&files, Box::new(FakeMemberPlatform), &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );

        let index = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::Index { .. } => Some(ExprId(idx as u32)),
                _ => None,
            })
            .expect("source should contain xs[0] index expression");
        let Some(ResolvedCall::Member(selected)) = info.resolved_calls.get(&index) else {
            panic!("checker must record classpath get selected for index lowering");
        };
        assert_eq!(selected.member.name, "get");
        assert_eq!(selected.ret, Ty::String);
    }

    #[test]
    fn classpath_string_index_operator_records_resolved_member_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file("fun first(s: String): Char = s[0]", &mut d);
        let files = vec![file];
        let mut syms = collect_signatures_with_cp(&files, Box::new(FakeMemberPlatform), &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );

        let index = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::Index { .. } => Some(ExprId(idx as u32)),
                _ => None,
            })
            .expect("source should contain s[0] index expression");
        let Some(ResolvedCall::Member(selected)) = info.resolved_calls.get(&index) else {
            panic!("checker must record classpath String.get selected for index lowering");
        };
        assert_eq!(selected.member.name, "get");
        assert_eq!(selected.ret, Ty::Char);
    }

    #[test]
    fn module_index_operator_records_member_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "class Box { operator fun get(i: Int): String = \"x\" }\n\
             fun first(b: Box): String = b[0]",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );

        let index = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::Index { .. } => Some(ExprId(idx as u32)),
                _ => None,
            })
            .expect("source should contain b[0] index expression");
        assert!(
            matches!(
                info.resolved_calls.get(&index),
                Some(ResolvedCall::ModuleMember { owner, name, params, ret, .. })
                    if owner.matches("Box")
                        && name == "get"
                        && params == &vec![Ty::Int]
                        && *ret == Ty::String
            ),
            "checker must record same-module member get selected for index lowering"
        );
    }

    #[test]
    fn cross_file_inherited_member_records_declaring_owner() {
        let mut d = DiagSink::new();
        let files = vec![
            parse_file("open class Base { fun ok(): String = \"O\" }", &mut d),
            parse_file(
                "class Child : Base()\nfun box(): String = Child().ok()",
                &mut d,
            ),
        ];
        let mut syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[1], &mut syms, &mut d);
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );

        let call = files[1]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::Call { callee, .. } => match files[1].expr(*callee) {
                    Expr::Member { name, .. } if name == "ok" => Some(ExprId(idx as u32)),
                    _ => None,
                },
                _ => None,
            })
            .expect("source should contain Child().ok() call");

        assert!(
            matches!(
                info.resolved_calls.get(&call),
                Some(ResolvedCall::ModuleMember { owner, name, params, ret, interface })
                    if owner.matches("Base")
                        && name == "ok"
                        && params.is_empty()
                        && *ret == Ty::String
                        && !*interface
            ),
            "checker must record the declaring module owner for inherited member calls"
        );
    }

    #[test]
    fn module_index_operator_records_extension_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "class Box\n\
             operator fun Box.get(i: Int): String = \"x\"\n\
             fun first(b: Box): String = b[0]",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );

        let index = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::Index { .. } => Some(ExprId(idx as u32)),
                _ => None,
            })
            .expect("source should contain b[0] index expression");
        assert!(
            matches!(
                info.resolved_calls.get(&index),
                Some(ResolvedCall::ModuleExtension { receiver, name, params, ret })
                    if *receiver == Ty::obj("Box")
                        && name == "get"
                        && params == &vec![Ty::Int]
                        && *ret == Ty::String
            ),
            "checker must record same-module extension get selected for index lowering"
        );
    }

    #[test]
    fn reference_range_in_records_operator_calls_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "class VR(val a: Int, val b: Int) {\n\
             \x20 operator fun contains(v: V): Boolean = v.x in a..b\n\
             }\n\
             class V(val x: Int) {\n\
             \x20 operator fun rangeTo(o: V): VR = VR(x, o.x)\n\
             }\n\
             fun box(): Boolean = V(2) in V(1)..V(3)",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );

        let in_range = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::InRange { start, .. } if info.ty(*start) == Ty::obj("V") => {
                    Some(ExprId(idx as u32))
                }
                _ => None,
            })
            .expect("source should contain reference in-range expression");

        assert!(
            matches!(
                info.resolved_operator_call(in_range, "rangeTo"),
                Some(ResolvedCall::ModuleMember { owner, name, params, ret, .. })
                    if owner.matches("V")
                        && name == "rangeTo"
                        && params.as_slice() == [Ty::obj("V")]
                        && *ret == Ty::obj("VR")
            ),
            "checker must record rangeTo selected for reference-range lowering"
        );
        assert!(
            matches!(
                info.resolved_operator_call(in_range, "contains"),
                Some(ResolvedCall::ModuleMember { owner, name, params, ret, .. })
                    if owner.matches("VR")
                        && name == "contains"
                        && params.as_slice() == [Ty::obj("V")]
                        && *ret == Ty::Boolean
            ),
            "checker must record contains selected for reference-range lowering"
        );
    }

    #[test]
    fn rejected_reference_range_does_not_record_partial_operator_calls() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "class VR {\n\
             \x20 operator fun contains(v: V): Int = 1\n\
             }\n\
             class V {\n\
             \x20 operator fun rangeTo(o: V): VR = VR()\n\
             }\n\
             fun box(): Boolean = V() in V()..V()",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            !d.diags.is_empty(),
            "invalid contains return should reject the in-range expression"
        );

        let in_range = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::InRange { start, .. } if info.ty(*start) == Ty::obj("V") => {
                    Some(ExprId(idx as u32))
                }
                _ => None,
            })
            .expect("source should contain reference in-range expression");

        assert_eq!(info.ty(in_range), Ty::Error);
        assert!(
            info.resolved_operator_call(in_range, "rangeTo").is_none(),
            "rejected in-range expression must not publish partial rangeTo resolution"
        );
        assert!(
            info.resolved_operator_call(in_range, "contains").is_none(),
            "rejected in-range expression must not publish invalid contains resolution"
        );
    }

    #[test]
    fn index_assignment_records_member_set_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "class Box { operator fun set(i: Int, v: String) {} }\n\
             fun write(b: Box) { b[0] = \"x\" }",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert_no_diags(&d);

        let assign = files[0]
            .stmt_arena
            .iter()
            .enumerate()
            .find_map(|(idx, stmt)| match stmt {
                Stmt::AssignIndex { .. } => Some(StmtId(idx as u32)),
                _ => None,
            })
            .expect("source should contain indexed assignment");

        assert!(
            matches!(
                info.resolved_stmt_operator_call(assign, "set"),
                Some(ResolvedCall::ModuleMember { owner, name, params, ret, .. })
                    if owner.matches("Box")
                        && name == "set"
                        && params.as_slice() == [Ty::Int, Ty::String]
                        && *ret == Ty::Unit
            ),
            "checker must record member set selected for indexed-assignment lowering"
        );
    }

    #[test]
    fn index_assignment_records_extension_set_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "class Box\n\
             operator fun Box.set(i: Int, v: String) {}\n\
             fun write(b: Box) { b[0] = \"x\" }",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert_no_diags(&d);

        let assign = files[0]
            .stmt_arena
            .iter()
            .enumerate()
            .find_map(|(idx, stmt)| match stmt {
                Stmt::AssignIndex { .. } => Some(StmtId(idx as u32)),
                _ => None,
            })
            .expect("source should contain indexed assignment");

        assert!(
            matches!(
                info.resolved_stmt_operator_call(assign, "set"),
                Some(ResolvedCall::ModuleExtension { receiver, name, params, ret })
                    if *receiver == Ty::obj("Box")
                        && name == "set"
                        && params.as_slice() == [Ty::Int, Ty::String]
                        && *ret == Ty::Unit
            ),
            "checker must record extension set selected for indexed-assignment lowering"
        );
    }

    #[test]
    fn index_assignment_records_get_return_for_lowering_guard() {
        let mut d = DiagSink::new();
        let file = parse_file("fun write(b: BoxedIndex) { b[0] = \"x\" }", &mut d);
        let files = vec![file];
        let mut syms = collect_signatures_with_cp(&files, Box::new(FakeMemberPlatform), &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert_no_diags(&d);

        let assign = files[0]
            .stmt_arena
            .iter()
            .enumerate()
            .find_map(|(idx, stmt)| match stmt {
                Stmt::AssignIndex { .. } => Some(StmtId(idx as u32)),
                _ => None,
            })
            .expect("source should contain indexed assignment");

        assert_eq!(
            info.resolved_index_store_get_return(assign),
            Some(Ty::String),
            "checker must record get(Int) return used by indexed-assignment lowering guard"
        );
    }

    #[test]
    fn foreach_records_iterator_protocol_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "fun loop(xs: BoxedIterable) { for (x in xs) { val y = x } }",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures_with_cp(&files, Box::new(FakeMemberPlatform), &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert_no_diags(&d);

        let iterable = files[0]
            .stmt_arena
            .iter()
            .find_map(|stmt| match stmt {
                Stmt::ForEach { iterable, .. } => Some(*iterable),
                _ => None,
            })
            .expect("source should contain foreach statement");
        let protocol = info
            .iterator_protocol(iterable)
            .expect("checker must record foreach iterator protocol");

        assert_eq!(protocol.elem_ty, Ty::String);
        assert_eq!(
            protocol.iter_ty,
            Ty::obj_args("BoxedIterator", &[Ty::String])
        );
        assert_eq!(protocol.has_next.name, "hasNext");
        assert_eq!(protocol.next.name, "next");
        assert!(
            matches!(
                &protocol.iterator,
                IteratorDispatchTarget::Member { owner_fallback, member }
                    if owner_fallback.matches("BoxedIterable") && member.name == "iterator"
            ),
            "checker must record the member iterator dispatch selected for foreach lowering"
        );
    }

    #[test]
    fn destructuring_records_member_component_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "class Box(val s: String) { operator fun component1(): String = s }\n\
             fun read(b: Box): String { val (x) = b; return x }",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert_no_diags(&d);

        let stmt = files[0]
            .stmt_arena
            .iter()
            .enumerate()
            .find_map(|(idx, stmt)| match stmt {
                Stmt::Destructure { .. } => Some(StmtId(idx as u32)),
                _ => None,
            })
            .expect("source should contain destructuring statement");

        assert!(
            matches!(
                info.resolved_destructure_component(stmt, 0),
                Some(DestructureComponentTarget::ModuleMember { owner, name, params, ret })
                    if owner.matches("Box")
                        && name == "component1"
                        && params.is_empty()
                        && *ret == Ty::String
            ),
            "checker must record member component selected for destructuring lowering"
        );
    }

    #[test]
    fn destructuring_records_extension_component_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "class Box(val s: String)\n\
             operator fun Box.component1(): String = s\n\
             fun read(b: Box): String { val (x) = b; return x }",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert_no_diags(&d);

        let stmt = files[0]
            .stmt_arena
            .iter()
            .enumerate()
            .find_map(|(idx, stmt)| match stmt {
                Stmt::Destructure { .. } => Some(StmtId(idx as u32)),
                _ => None,
            })
            .expect("source should contain destructuring statement");

        assert!(
            matches!(
                info.resolved_destructure_component(stmt, 0),
                Some(DestructureComponentTarget::ModuleExtension { receiver, name, params, ret })
                    if *receiver == Ty::obj("Box")
                        && name == "component1"
                        && params.is_empty()
                        && *ret == Ty::String
            ),
            "checker must record extension component selected for destructuring lowering"
        );
    }

    #[test]
    fn name_based_destructuring_records_property_getter_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file_with_detected_features(
            "// LANGUAGE: +NameBasedDestructuring, +EnableNameBasedDestructuringShortForm\n\
             data class P(val first: Int, val second: String)\n\
             fun read(p: P): String { val (text = second) = p; return text }",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert_no_diags(&d);

        let stmt = files[0]
            .stmt_arena
            .iter()
            .enumerate()
            .find_map(|(idx, stmt)| match stmt {
                Stmt::Destructure { .. } => Some(StmtId(idx as u32)),
                _ => None,
            })
            .expect("source should contain destructuring statement");

        assert!(
            matches!(
                info.resolved_destructure_component(stmt, 0),
                Some(DestructureComponentTarget::ModulePropertyGetter {
                    owner,
                    property,
                    ret,
                    interface
                }) if owner.matches("P")
                    && property == "second"
                    && *ret == Ty::String
                    && !*interface
            ),
            "checker must record source-property getter selected for name-based destructuring"
        );
    }

    #[test]
    fn module_top_level_calls_record_selected_overload_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "fun pick(x: Int): String = \"int\"\n\
             fun pick(x: String): String = x\n\
             fun box(): String = pick(\"OK\")",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert_no_diags(&d);

        let call = named_call(&files[0], "pick");
        let target = module_top_level_target(&info, call);
        let selected_decl = top_level_fun_decl(&files[0], "pick", |f| {
            f.params.first().is_some_and(|p| p.ty.name == "String")
        });
        assert_eq!(target.name, "pick");
        assert_eq!(target.params, vec![Ty::String]);
        assert_eq!(target.ret, Ty::String);
        assert_eq!(target.source_file, Some(0));
        assert_eq!(target.source_decl, Some(selected_decl));
        assert_eq!(target.param_meta, vec![("x".to_string(), None)]);
        assert!(!target.ret_is_tparam);
        assert_eq!(target.context_args, Vec::<String>::new());
    }

    #[test]
    fn module_top_level_context_calls_record_context_sources_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "class A(val x: String)\n\
             fun leaf(x: Int): String = \"int\"\n\
             context(a: A) fun leaf(): String = a.x\n\
             context(a: A) fun mid(): String = leaf()",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert_no_diags(&d);

        let call = named_call(&files[0], "leaf");
        let target = module_top_level_target(&info, call);
        assert_eq!(target.name, "leaf");
        assert_eq!(target.params, vec![Ty::obj("A")]);
        assert_eq!(target.ret, Ty::String);
        assert_eq!(target.source_file, Some(0));
        assert!(target.source_decl.is_some());
        assert_eq!(target.param_meta, vec![("a".to_string(), None)]);
        assert_eq!(target.context_args, vec!["a".to_string()]);
        assert!(
            !info.context_args.contains_key(&call),
            "module top-level context calls must carry context sources in the resolved target"
        );
    }

    #[test]
    fn module_top_level_context_overload_skips_unsatisfied_candidate() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "class A(val x: String)\n\
             class B(val x: String)\n\
             context(a: A) fun leaf(): String = a.x\n\
             context(b: B) fun leaf(): String = b.x\n\
             context(b: B) fun mid(): String = leaf()",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert_no_diags(&d);

        let call = named_call(&files[0], "leaf");
        let target = module_top_level_target(&info, call);
        assert_eq!(target.params, vec![Ty::obj("B")]);
        assert_eq!(target.context_args, vec!["b".to_string()]);
    }

    #[test]
    fn module_top_level_context_call_records_defaulted_value_param() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "class A(val x: String)\n\
             context(a: A) fun leaf(s: String = \"OK\"): String = s\n\
             context(a: A) fun mid(): String = leaf()",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures(&files, &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert_no_diags(&d);

        let call = named_call(&files[0], "leaf");
        let target = module_top_level_target(&info, call);
        assert_eq!(target.params, vec![Ty::obj("A"), Ty::String]);
        assert_eq!(target.context_args, vec!["a".to_string()]);
        assert_eq!(target.param_meta.len(), 2);
        assert_eq!(target.param_meta[0], ("a".to_string(), None));
        assert_eq!(target.param_meta[1].0, "s");
        assert!(target.param_meta[1].1.is_some());
    }

    #[test]
    fn module_top_level_cross_file_calls_record_source_key_for_lowering() {
        let mut d = DiagSink::new();
        let files = vec![
            parse_file("fun helper(s: String): String = s", &mut d),
            parse_file("fun box(): String = helper(\"OK\")", &mut d),
        ];
        let mut syms = collect_signatures(&files, &mut d);
        let helper_decl = top_level_fun_decl(&files[0], "helper", |_| true);
        syms.fn_facades_by_decl
            .insert((0, helper_decl.0), crate::types::type_name("AKt"));
        syms.fn_facades
            .insert("helper".to_string(), crate::types::type_name("AKt"));
        d.set_file(1);
        let info = check_file_at(&files[1], 1, &mut syms, &mut d);
        assert_no_diags(&d);

        let call = named_call(&files[1], "helper");
        let target = module_top_level_target(&info, call);
        assert_eq!(target.name, "helper");
        assert_eq!(target.source_file, Some(0));
        assert_eq!(target.source_decl, Some(helper_decl));
        assert!(target.param_meta.is_empty());
    }

    #[test]
    fn module_top_level_cross_file_overload_uses_selected_source_key() {
        let mut d = DiagSink::new();
        let files = vec![
            parse_file("fun helper(x: Int): String = \"int\"", &mut d),
            parse_file("fun helper(s: String): String = s", &mut d),
            parse_file("fun box(): String = helper(\"OK\")", &mut d),
        ];
        let mut syms = collect_signatures(&files, &mut d);
        let int_decl = top_level_fun_decl(&files[0], "helper", |_| true);
        let string_decl = top_level_fun_decl(&files[1], "helper", |_| true);
        syms.fn_facades_by_decl
            .insert((0, int_decl.0), crate::types::type_name("AKt"));
        syms.fn_facades_by_decl
            .insert((1, string_decl.0), crate::types::type_name("BKt"));
        syms.fn_facades
            .insert("helper".to_string(), crate::types::type_name("AKt"));
        d.set_file(2);
        let info = check_file_at(&files[2], 2, &mut syms, &mut d);
        assert_no_diags(&d);

        let call = named_call(&files[2], "helper");
        let target = module_top_level_target(&info, call);
        assert_eq!(target.name, "helper");
        assert_eq!(target.params, vec![Ty::String]);
        assert_eq!(target.source_file, Some(1));
        assert_eq!(target.source_decl, Some(string_decl));
    }

    #[test]
    fn module_top_level_selection_respects_package_scope() {
        let mut d = DiagSink::new();
        let files = vec![
            parse_file("package a\nfun helper(): String = \"a\"", &mut d),
            parse_file("package b\nfun helper(): String = \"OK\"", &mut d),
            parse_file("package b\nfun box(): String = helper()", &mut d),
        ];
        let mut syms = collect_signatures(&files, &mut d);
        d.set_file(2);
        let info = check_file_at(&files[2], 2, &mut syms, &mut d);
        assert_no_diags(&d);

        let call = named_call(&files[2], "helper");
        let target = module_top_level_target(&info, call);
        let b_decl = top_level_fun_decl(&files[1], "helper", |_| true);
        assert_eq!(target.source_file, Some(1));
        assert_eq!(target.source_decl, Some(b_decl));
    }

    #[test]
    fn classpath_top_level_named_calls_record_arg_slots_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file("fun direct(): String = knownTop(b = 2, a = \"x\")", &mut d);
        let files = vec![file];
        let mut syms = collect_signatures_with_cp(&files, Box::new(FakeMemberPlatform), &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );

        let call = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::Call { callee, .. } => match files[0].expr(*callee) {
                    Expr::Name(name) if name == "knownTop" => Some(ExprId(idx as u32)),
                    _ => None,
                },
                _ => None,
            })
            .expect("source should contain knownTop() call");
        assert!(
            matches!(
                info.resolved_calls.get(&call),
                Some(ResolvedCall::TopLevel(_))
            ),
            "checker must record the selected classpath top-level callable for lowering"
        );
        let slots = info
            .resolved_call_arg_slots
            .get(&call)
            .expect("classpath top-level named call should record argument slots");
        let values: Vec<_> = slots
            .iter()
            .map(|slot| {
                slot.map(|arg| match files[0].expr(arg) {
                    Expr::StringLit(v) => v.clone(),
                    Expr::IntLit(v) => v.to_string(),
                    other => panic!("unexpected argument expression in slot: {other:?}"),
                })
            })
            .collect();
        assert_eq!(values, vec![Some("x".to_string()), Some("2".to_string())]);
    }

    #[test]
    fn classpath_top_level_function_refs_record_callable_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file("val ref = ::knownTop", &mut d);
        let files = vec![file];
        let mut syms = collect_signatures_with_cp(&files, Box::new(FakeMemberPlatform), &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );

        let function_ref = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::CallableRef {
                    receiver: None,
                    name,
                } if name == "knownTop" => Some(ExprId(idx as u32)),
                _ => None,
            })
            .expect("source should contain ::knownTop");
        let Some(ExprLowering::ClasspathTopLevelFunctionRef(callable)) =
            info.expr_lowers.get(&function_ref)
        else {
            panic!("checker must record classpath top-level function refs for lowering");
        };
        assert!(callable.owner.matches("test/TopKt"));
        assert_eq!(callable.name, "knownTop");
        assert_eq!(callable.params, vec![Ty::String, Ty::Int]);
    }

    #[test]
    fn constructor_refs_are_not_shadowed_by_same_named_classpath_functions() {
        let mut d = DiagSink::new();
        let file = parse_file("class Foo(val i: Int)\nval ref = ::Foo", &mut d);
        let files = vec![file];
        let mut syms = collect_signatures_with_cp(&files, Box::new(FakeMemberPlatform), &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );

        let constructor_ref = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::CallableRef {
                    receiver: None,
                    name,
                } if name == "Foo" => Some(ExprId(idx as u32)),
                _ => None,
            })
            .expect("source should contain ::Foo");
        assert_eq!(
            info.expr_types[constructor_ref.0 as usize],
            Ty::fun(vec![Ty::Int], Ty::obj("Foo"))
        );
        assert!(
            !matches!(
                info.expr_lowers.get(&constructor_ref),
                Some(ExprLowering::ClasspathTopLevelFunctionRef(_))
            ),
            "constructor refs must not record a classpath top-level callable"
        );
    }

    #[test]
    fn delegated_properties_record_classpath_extension_getvalue_for_lowering() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "package test\nclass Delegate\nval prop: String by Delegate()",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures_with_cp(&files, Box::new(FakeMemberPlatform), &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );

        let delegate_expr = files[0]
            .decls
            .iter()
            .find_map(|decl| match files[0].decl(*decl) {
                Decl::Property(prop) if prop.name == "prop" => prop.delegate,
                _ => None,
            })
            .expect("source should contain delegated property");
        let target = info
            .delegate_getvalue(delegate_expr)
            .expect("checker must record delegated getValue target for lowering");
        let DelegateGetValueTarget::Extension(callable) = target else {
            panic!("expected classpath extension getValue target, got {target:?}");
        };
        assert!(callable.owner.matches("test/DelegateKt"));
        assert_eq!(callable.name, "getValue");
        assert_eq!(callable.ret, Ty::String);
        assert!(
            info.synthetic_ext(delegate_expr, "getValue").is_none(),
            "delegated getValue must use the dedicated target map"
        );
    }

    #[test]
    fn unannotated_delegated_properties_infer_classpath_extension_getvalue_return() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "package test\nclass Delegate\nval prop by Delegate()",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures_with_cp(&files, Box::new(FakeMemberPlatform), &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );
        assert_eq!(
            syms.props.get("prop").map(|(ty, _, _)| *ty),
            Some(Ty::String)
        );

        let delegate_expr = files[0]
            .decls
            .iter()
            .find_map(|decl| match files[0].decl(*decl) {
                Decl::Property(prop) if prop.name == "prop" => prop.delegate,
                _ => None,
            })
            .expect("source should contain delegated property");
        assert_eq!(
            info.delegate_getvalue(delegate_expr)
                .expect("checker must record delegated getValue")
                .ret(),
            Ty::String
        );
    }

    #[test]
    fn local_delegated_properties_infer_classpath_extension_getvalue_return() {
        let mut d = DiagSink::new();
        let file = parse_file(
            "package test\nclass Delegate\nfun box(): String { val prop by Delegate(); return prop }",
            &mut d,
        );
        let files = vec![file];
        let mut syms = collect_signatures_with_cp(&files, Box::new(FakeMemberPlatform), &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            d.diags.is_empty(),
            "unexpected diagnostics: {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );

        let delegate_expr = files[0]
            .stmt_arena
            .iter()
            .find_map(|stmt| match stmt {
                Stmt::LocalDelegate { name, delegate, .. } if name == "prop" => Some(*delegate),
                _ => None,
            })
            .expect("source should contain local delegated property");
        assert_eq!(
            info.delegate_getvalue(delegate_expr)
                .expect("checker must record local delegated getValue")
                .ret(),
            Ty::String
        );
    }

    #[test]
    fn classpath_member_slot_ties_do_not_fall_back_to_first_match() {
        let mut d = DiagSink::new();
        let file = parse_file("fun ambiguous(s: String): String = s.tie(a = 1)", &mut d);
        let files = vec![file];
        let mut syms = collect_signatures_with_cp(&files, Box::new(FakeMemberPlatform), &mut d);
        let info = check_file(&files[0], &mut syms, &mut d);
        assert!(
            d.diags
                .iter()
                .any(|diag| diag.msg.contains("overload resolution ambiguity")),
            "expected ambiguity diagnostic, got {:?}",
            d.diags.iter().map(|x| &x.msg).collect::<Vec<_>>()
        );

        let call = files[0]
            .expr_arena
            .iter()
            .enumerate()
            .find_map(|(idx, expr)| match expr {
                Expr::Call { callee, .. } => match files[0].expr(*callee) {
                    Expr::Member { name, .. } if name == "tie" => Some(ExprId(idx as u32)),
                    _ => None,
                },
                _ => None,
            })
            .expect("source should contain s.tie() call");
        assert!(
            !matches!(info.resolved_calls.get(&call), Some(ResolvedCall::Member(_))),
            "ambiguous slot-aware classpath member call must not fall back to first-match resolution"
        );
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
    fn private_member_property_access_is_checked() {
        // A `private` member property is readable only from within its declaring class — kotlinc rejects
        // a read from anywhere else, and so must krusty (rather than silently compiling it).
        err_contains(
            "class C { private val secret = 42 }\nfun box(): String = C().secret.toString()",
            "it is private in 'C'",
        );
        // Legal accesses from WITHIN the class (implicit `this`, explicit `this`, and another instance of
        // the same class) are accepted.
        ok("class C {\n  private val secret = 42\n  fun a(): Int = secret\n  fun b(): Int = this.secret\n  fun c(o: C): Int = o.secret\n}");
        // A class NESTED inside the owner (here an `inner class`) may read the enclosing private property
        // through an instance of the owner — kotlinc allows it, so the access check must not reject it.
        ok("class C {\n  private val secret = 42\n  inner class N {\n    fun r(o: C): Int = o.secret\n  }\n}");
    }

    #[test]
    fn private_member_function_access_is_checked() {
        // A `private` member FUNCTION is callable only from within its class — a qualified call from
        // outside is rejected (kotlinc); a same-class call is accepted.
        err_contains(
            "class C { private fun secret(): Int = 1 }\nfun box(): String { C().secret(); return \"OK\" }",
            "it is private in 'C'",
        );
        ok("class C {\n  private fun secret(): Int = 1\n  fun via(o: C): Int = o.secret()\n}");
    }

    #[test]
    fn protected_member_property_access_is_checked() {
        // A `protected` member is unreadable from unrelated code (here a top-level function)…
        err_contains(
            "open class Base { protected val secret = 1 }\nfun box(): String = Base().secret.toString()",
            "it is protected in 'Base'",
        );
        // …but readable from a SUBCLASS (through a base-typed receiver inside the subclass).
        ok("open class Base { protected val secret = 1 }\nclass Sub : Base() {\n  fun r(b: Base): Int = b.secret\n}");
    }

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
