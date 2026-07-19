//! The ONE assignability relation over the full [`Ty`] lattice.
//!
//! Kotlin subtyping — "is a value of type `sub` assignable where `sup` is expected" — for every `Ty`
//! variant in one place: primitives, generic `Obj` (with covariant type arguments), function types
//! (`Fun`, contravariant parameters / covariant return), arrays (covariant element), `Nullable`, the
//! bottom `Nothing`, the `null` literal, and type variables (`TyParam`) checked against their declared
//! bound and resolved through a [`TyCtx`]. The class hierarchy walk and value-class underlying are
//! provided by a [`TypeOracle`] the caller supplies (federated over the user module + classpath), so the
//! relation is platform-neutral: no JVM descriptors, no classpath strings scattered per call site.
//!
//! This replaces the former scatter (`reference_subtype`, `is_classpath_subtype`, `obj_is_subtype`,
//! `arg_subtype_assignable`, `ref_subtype_fits`, `arg_assignable`, `descriptor_arg_subtype_of_param`,
//! `array_covariant_assignable`, `elem_covariant_assignable`, the receiver-argument covariance in
//! `source_receiver_rank`), each of which re-implemented one slice — usually erased (dropping type
//! arguments and nullability) and without a type-variable context.

use crate::types::{Ty, TypeName};
use std::borrow::Cow;
use std::collections::HashMap;

/// The class-hierarchy oracle the assignability relation walks — the direct supertypes of a class in
/// Kotlin internal-name form (federated over the user module and the classpath), plus two platform hooks.
pub trait TypeOracle {
    /// The DIRECT supertypes (superclass + superinterfaces) of `internal`, as Kotlin internal names.
    /// Empty when the class is unknown or has none (`kotlin/Any`).
    fn direct_supertypes(&self, internal: TypeName) -> Vec<TypeName>;

    /// The underlying representation type of a value/inline class (`Aid(val v: String)` → `kotlin/String`),
    /// or `None` for a non-value class. Lets the relation accept a value-class argument where its erased
    /// underlying is expected — the JVM ABI a `@JvmInline` boundary presents. Default: no value classes.
    fn value_underlying(&self, _ty: Ty) -> Option<Ty> {
        None
    }

    /// A canonical class identity used to equate names the platform unifies — a Kotlin collection interface
    /// and its single JVM interface (`kotlin/collections/List` ≡ `kotlin/collections/MutableList` ≡
    /// `java/util/List`). Two classes with the same canonical identity are the same class here. Default:
    /// the name itself (no aliasing).
    fn canonical_class<'a>(&self, internal: &'a str) -> Cow<'a, str> {
        Cow::Borrowed(internal)
    }

    /// Whether two class names denote the same platform class identity. The default compares canonical
    /// names; platforms that have spelling aliases should override this to avoid materializing a
    /// normalized string on hot hierarchy walks.
    fn same_class(&self, a: &str, b: &str) -> bool {
        a == b || self.canonical_class(a) == self.canonical_class(b)
    }

    /// Whether `candidate` denotes the class whose original name is `target` and whose canonical name
    /// was already computed. Hierarchy walks use this so the target identity is not recomputed for every
    /// visited superclass.
    fn matches_class(&self, candidate: &str, target: &str, target_canonical: &str) -> bool {
        candidate == target || self.canonical_class(candidate).as_ref() == target_canonical
    }

    /// Id-backed variant used by hot hierarchy walks. The default renders only when the id did not match
    /// directly and platform canonicalization must be consulted.
    fn matches_class_name(
        &self,
        candidate: TypeName,
        target: TypeName,
        target_canonical: &str,
    ) -> bool {
        if candidate == target {
            return true;
        }
        let candidate = candidate.render();
        self.canonical_class(&candidate).as_ref() == target_canonical
    }

    /// Id-backed class identity comparison used by assignability/coercion walks. The default preserves
    /// legacy string hooks only as a compatibility fallback; production oracles should override this when
    /// they can compare platform identities by ids.
    fn same_class_name(&self, a: TypeName, b: TypeName) -> bool {
        if a == b {
            return true;
        }
        let a = a.render();
        let b = b.render();
        self.same_class(&a, &b)
    }
}

/// The in-scope type variables, each mapped to a `Ty` — its declared upper BOUND for a free variable
/// (`<T : CharSequence>` → `CharSequence`), or a concrete BINDING once inferred (`T` ↦ `String`). A bare
/// `Ty::TyParam` carries its own bound inline; the context overrides it (and supplies a binding) by name.
#[derive(Default, Clone)]
pub struct TyCtx {
    vars: HashMap<String, Ty>,
}

impl TyCtx {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind/override a type variable to a `Ty` (a bound or an inferred type).
    pub fn with_var(mut self, name: &str, ty: Ty) -> Self {
        self.vars.insert(name.to_string(), ty);
        self
    }

    /// Insert a variable binding in place.
    pub fn bind(&mut self, name: &str, ty: Ty) {
        self.vars.insert(name.to_string(), ty);
    }

    /// The context type for a variable: its bound/binding if known, else the variable's own inline bound.
    fn lookup(&self, name: &str, inline_bound: Ty) -> Ty {
        self.vars.get(name).copied().unwrap_or(inline_bound)
    }
}

fn is_any(t: Ty) -> bool {
    matches!(t, Ty::Obj(n, _)
        if crate::types::same(n, crate::types::wk::any())
            || crate::types::same(n, crate::types::wk::java_object()))
}

/// A JVM primitive-family scalar (Kotlin has no implicit widening among these — assignability is exact).
fn is_scalar(t: Ty) -> bool {
    matches!(
        t,
        Ty::Int
            | Ty::Byte
            | Ty::Short
            | Ty::Long
            | Ty::Float
            | Ty::Double
            | Ty::Boolean
            | Ty::Char
            | Ty::UInt
            | Ty::ULong
    )
}

/// Whether a value of type `sub` is assignable where `sup` is expected, under Kotlin subtyping.
///
/// - `Nothing` is assignable to everything; `null` to any nullable type; `Error` fits either side
///   (a type error is already reported — do not cascade).
/// - `T` (non-null) is assignable to `T?`; `T?` is NOT assignable to non-null `T`.
/// - `Any`/`Object` (non-null) accepts every non-null value, primitives included (boxing).
/// - Primitives are assignable only to the SAME primitive (no `Int` → `Long`).
/// - `Fun` is contravariant in parameters, covariant in return, matched by arity.
/// - `Array` is covariant in its element (krusty's array model).
/// - `Obj` walks the class hierarchy (via `oracle`) and matches type arguments COVARIANTLY (a receiver /
///   read position — `List<Int>` is assignable to `Iterable<Any>`); an `Any`/`Object`/type-variable
///   argument on either side is a wildcard.
/// - A value/inline-class value is assignable where its underlying representation is expected.
/// - A `TyParam` is checked through `cx` against its bound.
pub fn is_assignable(cx: &TyCtx, oracle: &dyn TypeOracle, sub: Ty, sup: Ty) -> bool {
    assignable_inner(cx, oracle, sub, sup, true)
}

/// Pure Kotlin SUBTYPING — like [`is_assignable`] but WITHOUT the value/inline-class erasure step (an
/// `Aid` is NOT a subtype of its underlying `String`). Use where a genuine type-hierarchy relation is
/// meant (a classpath supertype walk, a `when`-branch reachability), not a JVM-ABI boundary.
pub fn is_subtype(cx: &TyCtx, oracle: &dyn TypeOracle, sub: Ty, sup: Ty) -> bool {
    assignable_inner(cx, oracle, sub, sup, false)
}

fn assignable_inner(
    cx: &TyCtx,
    oracle: &dyn TypeOracle,
    sub: Ty,
    sup: Ty,
    value_class: bool,
) -> bool {
    if sub == sup {
        return true;
    }
    if sub == Ty::Error || sup == Ty::Error {
        return true;
    }
    if sub == Ty::Nothing {
        return true;
    }

    // Nullability. `null` fits any nullable target; `T` fits `T?`; `T?` does not fit non-null `T`.
    if sub == Ty::Null {
        return matches!(sup, Ty::Nullable(_)) || sup == Ty::Null;
    }
    if let Ty::Nullable(inner) = sup {
        return is_assignable(cx, oracle, sub.non_null(), *inner);
    }
    if matches!(sub, Ty::Nullable(_)) {
        // sup is non-null here (the `Nullable` sup arm returned above).
        return false;
    }

    // Type variables — resolve through the context to the bound/binding, then compare.
    if let Ty::TyParam(name, bound) = sup {
        let target = cx.lookup(name, *bound);
        // Avoid infinite regress when the context maps the variable to itself.
        return target != sup && assignable_inner(cx, oracle, sub, target, value_class);
    }
    if let Ty::TyParam(name, bound) = sub {
        let source = cx.lookup(name, *bound);
        return source != sub && assignable_inner(cx, oracle, source, sup, value_class);
    }

    // Everything (a boxed primitive included) is assignable to `Any`/`Object`.
    if is_any(sup) {
        return true;
    }

    // A scalar TARGET admits only the identical scalar — no implicit numeric widening in Kotlin, and no
    // reference is assignable to a primitive. A scalar SOURCE against a reference target boxes and is
    // decided by the boxed class's hierarchy below (`Int` <: `Number`/`Comparable`).
    if is_scalar(sup) {
        return sub == sup;
    }

    match (sub, sup) {
        (Ty::Fun(a), Ty::Fun(b)) => {
            a.params.len() == b.params.len()
                // Parameters are CONTRAVARIANT: the supertype function's parameter must be assignable to
                // the subtype's (a function taking `Any` is-a function taking `String`).
                && a.params
                    .iter()
                    .zip(b.params.iter())
                    .all(|(sp, pp)| assignable_inner(cx, oracle, *pp, *sp, value_class))
                // Return is COVARIANT.
                && assignable_inner(cx, oracle, a.ret, b.ret, value_class)
        }
        (Ty::Obj(_, _), Ty::Obj(_, _)) => obj_assignable(cx, oracle, sub, sup, value_class),
        _ => {
            // Mixed reference shapes (`Ty::String` vs `Ty::Obj("kotlin/CharSequence")`, `Fun` vs `Obj`
            // FunctionN) compare through their Kotlin class identity.
            class_assignable(oracle, sub, sup, value_class)
        }
    }
}

/// Two `Obj` reference types: the sub-class reaches the super-class in the hierarchy AND every type
/// argument matches covariantly.
fn obj_assignable(
    cx: &TyCtx,
    oracle: &dyn TypeOracle,
    sub: Ty,
    sup: Ty,
    value_class: bool,
) -> bool {
    if !class_assignable(oracle, sub, sup, value_class) {
        return false;
    }
    // Type arguments, covariantly. A wildcard (`Any`/`Object`/type variable) on either side matches.
    sup.type_args()
        .iter()
        .zip(sub.type_args().iter())
        .all(|(&p, &a)| {
            arg_wildcard(p) || arg_wildcard(a) || assignable_inner(cx, oracle, a, p, value_class)
        })
}

fn arg_wildcard(t: Ty) -> bool {
    t.is_ty_param() || is_any(t.non_null())
}

/// The class-identity reach: `sub`'s class is `sup`'s class (canonically) or transitively extends /
/// implements it, walking `oracle.direct_supertypes`. Reference types only — a class-less `Ty` yields
/// `false`. Uses `kotlin_class_internal` so a `Ty::String` / `Ty::Fun` maps to its class. When
/// `value_class`, a value/inline-class value also reaches its underlying representation's class.
fn class_assignable(oracle: &dyn TypeOracle, sub: Ty, sup: Ty, value_class: bool) -> bool {
    let (Some(start), Some(target)) = (sub.kotlin_class_internal(), sup.kotlin_class_internal())
    else {
        return false;
    };
    let mut seen = std::collections::HashSet::new();
    seen.insert(start);
    let mut stack = vec![start];
    while let Some(cur) = stack.pop() {
        if oracle.same_class_name(cur, target) {
            return true;
        }
        let direct = oracle.direct_supertypes(cur);
        stack.extend(direct.into_iter().filter(|s| seen.insert(*s)));
    }
    // A value/inline class is assignable where its underlying representation is expected (the JVM-ABI
    // boundary) — only under `is_assignable`, not the pure `is_subtype` relation.
    value_class
        && oracle
            .value_underlying(sub)
            .is_some_and(|u| u.kotlin_class_internal().is_some_and(|n| n == target) || u == sup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Ty;

    /// A tiny hand-wired hierarchy oracle for the relation's unit tests.
    struct Fake;
    impl TypeOracle for Fake {
        fn direct_supertypes(&self, internal: TypeName) -> Vec<TypeName> {
            let s: &[&str] = match internal {
                n if n.matches("kotlin/String") => &["kotlin/CharSequence", "kotlin/Comparable"],
                n if n.matches("kotlin/CharSequence") => &["kotlin/Any"],
                n if n.matches("kotlin/Comparable") => &["kotlin/Any"],
                n if n.matches("kotlin/collections/List") => &["kotlin/collections/Iterable"],
                n if n.matches("kotlin/collections/MutableList") => &["kotlin/collections/List"],
                n if n.matches("kotlin/collections/Iterable") => &["kotlin/Any"],
                n if n.matches("kotlin/Int") || n.matches("kotlin/Double") => &["kotlin/Number"],
                n if n.matches("kotlin/Number") => &["kotlin/Any"],
                n if n.matches("app/Dog") => &["app/Animal"],
                n if n.matches("app/Animal") => &["kotlin/Any"],
                _ => &[],
            };
            s.iter().map(|x| crate::types::type_name(x)).collect()
        }
        fn value_underlying(&self, ty: Ty) -> Option<Ty> {
            match ty {
                Ty::Obj(n, _) if n.matches("app/Aid") => Some(Ty::String),
                _ => None,
            }
        }
        fn canonical_class<'a>(&self, internal: &'a str) -> Cow<'a, str> {
            match internal {
                "canonical/Readonly" | "canonical/Mutable" => Cow::Borrowed("canonical/List"),
                _ => Cow::Borrowed(internal),
            }
        }
    }

    fn ok(sub: Ty, sup: Ty) -> bool {
        is_assignable(&TyCtx::new(), &Fake, sub, sup)
    }
    fn s(n: &str) -> Ty {
        Ty::obj(n)
    }
    fn g(n: &str, args: &[Ty]) -> Ty {
        Ty::obj_args(n, args)
    }

    #[test]
    fn identity_and_error_and_nothing() {
        assert!(ok(Ty::Int, Ty::Int));
        assert!(ok(s("app/Dog"), s("app/Dog")));
        assert!(ok(Ty::Error, Ty::Int));
        assert!(ok(Ty::Int, Ty::Error));
        assert!(ok(Ty::Nothing, s("app/Dog")));
        assert!(ok(Ty::Nothing, Ty::Int));
    }

    #[test]
    fn nullability() {
        assert!(ok(Ty::Null, Ty::nullable(s("app/Dog"))));
        assert!(!ok(Ty::Null, s("app/Dog")));
        assert!(ok(s("app/Dog"), Ty::nullable(s("app/Dog"))));
        assert!(ok(s("app/Dog"), Ty::nullable(s("app/Animal"))));
        assert!(!ok(Ty::nullable(s("app/Dog")), s("app/Dog")));
        assert!(ok(
            Ty::nullable(s("app/Dog")),
            Ty::nullable(s("app/Animal"))
        ));
    }

    #[test]
    fn primitives_exact_no_widening() {
        assert!(ok(Ty::Int, Ty::Int));
        assert!(!ok(Ty::Int, Ty::Long));
        assert!(!ok(Ty::Byte, Ty::Int));
        // Boxing to Any.
        assert!(ok(Ty::Int, s("kotlin/Any")));
        // A reference is never assignable to a primitive.
        assert!(!ok(s("app/Dog"), Ty::Int));
    }

    #[test]
    fn reference_subtyping() {
        assert!(ok(s("app/Dog"), s("app/Animal")));
        assert!(ok(s("app/Dog"), s("kotlin/Any")));
        assert!(!ok(s("app/Animal"), s("app/Dog")));
        assert!(ok(Ty::String, s("kotlin/CharSequence")));
        assert!(ok(Ty::Int, s("kotlin/Number")));
    }

    #[test]
    fn canonical_class_aliases_match_through_assignability() {
        assert!(ok(s("canonical/Mutable"), s("canonical/Readonly")));
    }

    #[test]
    fn generic_covariance() {
        assert!(ok(
            g("kotlin/collections/List", &[Ty::Int]),
            g("kotlin/collections/Iterable", &[Ty::Int])
        ));
        // Covariant read position: List<Int> <: Iterable<Any>.
        assert!(ok(
            g("kotlin/collections/List", &[Ty::Int]),
            g("kotlin/collections/Iterable", &[s("kotlin/Any")])
        ));
        // Nested: List<List<Int>> <: Iterable<Iterable<Any>>.
        assert!(ok(
            g(
                "kotlin/collections/List",
                &[g("kotlin/collections/List", &[Ty::Int])]
            ),
            g(
                "kotlin/collections/Iterable",
                &[g("kotlin/collections/Iterable", &[s("kotlin/Any")])]
            )
        ));
        // Element mismatch (the reduction-family case): Iterable<Double> is NOT <: Iterable<Int>.
        assert!(!ok(
            g("kotlin/collections/Iterable", &[Ty::Double]),
            g("kotlin/collections/Iterable", &[Ty::Int])
        ));
        assert!(ok(
            g("kotlin/collections/MutableList", &[Ty::Int]),
            g("kotlin/collections/List", &[Ty::Int])
        ));
    }

    #[test]
    fn function_variance() {
        // (Animal) -> Dog  <:  (Dog) -> Animal   [param contravariant, ret covariant]
        let sub = Ty::fun(vec![s("app/Animal")], s("app/Dog"));
        let sup = Ty::fun(vec![s("app/Dog")], s("app/Animal"));
        assert!(ok(sub, sup));
        assert!(!ok(sup, sub));
        // arity mismatch
        assert!(!ok(
            Ty::fun(vec![], Ty::Int),
            Ty::fun(vec![Ty::Int], Ty::Int)
        ));
    }

    #[test]
    fn array_covariance() {
        assert!(ok(Ty::array(s("app/Dog")), Ty::array(s("app/Animal"))));
        assert!(!ok(Ty::array(s("app/Animal")), Ty::array(s("app/Dog"))));
    }

    #[test]
    fn type_variables() {
        // T with bound CharSequence: String <: T (via bound), and T <: Any.
        let cx = TyCtx::new().with_var("T", s("kotlin/CharSequence"));
        let tv = Ty::ty_param("T", s("kotlin/CharSequence"));
        assert!(is_assignable(&cx, &Fake, Ty::String, tv));
        assert!(is_assignable(&cx, &Fake, tv, s("kotlin/Any")));
        // Unbounded T defaults to Any bound: Dog <: T.
        let tv2 = Ty::ty_param("T", s("kotlin/Any"));
        assert!(is_assignable(&TyCtx::new(), &Fake, s("app/Dog"), tv2));
        // A bound that rejects: Int is not <: T:CharSequence.
        assert!(!is_assignable(&cx, &Fake, Ty::Int, tv));
    }

    #[test]
    fn value_class_underlying() {
        // Aid (value class over String) is assignable where its underlying String is expected.
        assert!(ok(s("app/Aid"), Ty::String));
        assert!(ok(s("app/Aid"), s("kotlin/String")));
    }
}
