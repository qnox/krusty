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
//! `ReceiverMro`), each of which re-implemented one slice — usually erased (dropping type
//! arguments and nullability) and without a type-variable context.

use crate::types::{Ty, TypeName};
use std::collections::HashMap;

/// The class-hierarchy oracle the assignability relation walks. Direct supertypes retain their applied
/// type arguments: `Child<String> : Producer<String>` must not collapse to the bare name `Producer`
/// before declaration-site variance is checked.
pub trait TypeOracle {
    /// The direct applied superclass + superinterfaces of `ty`. Empty when the classifier is unknown
    /// or has none (`kotlin/Any`).
    fn direct_supertypes(&self, ty: Ty) -> Vec<Ty>;

    /// Id-backed class identity comparison used by assignability/coercion walks. Platforms that unify
    /// multiple source names onto one runtime class override this without rendering full internal names.
    fn same_class_name(&self, a: TypeName, b: TypeName) -> bool {
        a == b
    }

    /// Declaration-site variance of one type parameter. Querying one slot avoids cloning complete
    /// variance vectors in the assignability hot path.
    fn type_param_variance(
        &self,
        _internal: TypeName,
        _index: usize,
    ) -> crate::types::TypeVariance {
        crate::types::TypeVariance::Invariant
    }

    /// Readable intersection upper bounds of one classifier type parameter. An empty declaration
    /// has Kotlin's implicit `Any?` bound.
    fn type_param_upper_bounds(&self, _internal: TypeName, _index: usize) -> Vec<Ty> {
        vec![Ty::nullable(Ty::obj("kotlin/Any"))]
    }

    /// Upper bound paired with a platform type's retained lower bound. JVM mapped collections
    /// override this with their read-only face (`MutableList<T>` -> `List<T>`); common array
    /// projection flexibility is added by the assignability relation itself.
    fn platform_flexible_upper_bound(&self, lower: Ty) -> Ty {
        lower
    }
}

/// Target-independent upper shape of a flexible type whose lower bound is retained in [`Ty`].
/// Array projection flexibility is Kotlin type semantics; the JVM provider only decides that an
/// external declaration has a platform type and supplies its lower bound.
pub(crate) fn platform_shape_upper_bound(lower: Ty) -> Ty {
    match lower {
        Ty::Obj(owner, arguments)
            if lower.is_reference_array()
                && matches!(arguments, [argument] if argument.projection_inner().is_none()) =>
        {
            Ty::obj_args_name(owner, &[Ty::out_projection(arguments[0])])
        }
        _ => lower,
    }
}

/// Inferred type-variable bindings. Declared bounds remain on `Ty::TyParam`; keeping the two states
/// separate prevents ordinary subtype checking from treating an unbound `T : Any?` as `Any?`.
#[derive(Default, Clone)]
pub struct TyCtx {
    vars: HashMap<String, Ty>,
}

impl TyCtx {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind a type variable to its inferred type.
    pub fn with_var(mut self, name: &str, ty: Ty) -> Self {
        self.vars.insert(name.to_string(), ty);
        self
    }

    /// Insert a variable binding in place.
    pub fn bind(&mut self, name: &str, ty: Ty) {
        self.vars.insert(name.to_string(), ty);
    }

    fn lookup(&self, name: &str) -> Option<Ty> {
        self.vars.get(name).copied()
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
            | Ty::UByte
            | Ty::UShort
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
/// - `Obj` walks the class hierarchy (via `oracle`) and applies the target classifier's declared
///   variance. Invariant arguments compare after resolving type-variable bindings/bounds.
/// - A value/inline-class value is assignable where its underlying representation is expected.
/// - A `TyParam` is checked through `cx` against its bound.
pub fn is_assignable(cx: &TyCtx, oracle: &dyn TypeOracle, sub: Ty, sup: Ty) -> bool {
    assignable_inner(cx, oracle, sub, sup)
}

/// Pure Kotlin SUBTYPING — like [`is_assignable`] but WITHOUT the value/inline-class erasure step (an
/// `Aid` is NOT a subtype of its underlying `String`). Use where a genuine type-hierarchy relation is
/// meant (a classpath supertype walk, a `when`-branch reachability), not a JVM-ABI boundary.
pub fn is_subtype(cx: &TyCtx, oracle: &dyn TypeOracle, sub: Ty, sup: Ty) -> bool {
    assignable_inner(cx, oracle, sub, sup)
}

fn assignable_inner(cx: &TyCtx, oracle: &dyn TypeOracle, sub: Ty, sup: Ty) -> bool {
    if sub == sup {
        return true;
    }
    // A type parameter's semantic identity is its key. The carried bound is constraint metadata and
    // may be normalized differently while a generic call is instantiated (`Any` versus `Any?`).
    if matches!((sub, sup), (Ty::TyParam(a, _), Ty::TyParam(b, _)) if a == b) {
        return true;
    }
    if sub == Ty::Error || sup == Ty::Error {
        return true;
    }
    if sub == Ty::Nothing {
        return true;
    }
    // Projection wrappers are meaningful only as generic arguments. When they reach a recursive
    // comparison, consume their readable bound; `obj_assignable` handles their direction.
    if let Ty::InProjection(inner) | Ty::OutProjection(inner) | Ty::StarProjection(inner) = sup {
        return assignable_inner(cx, oracle, sub, *inner);
    }
    if let Ty::InProjection(inner) | Ty::OutProjection(inner) | Ty::StarProjection(inner) = sub {
        return assignable_inner(cx, oracle, *inner, sup);
    }

    // A Java platform type `T!` is the flexible interval `T..T?`: as a source it may be consumed at
    // either bound, and as a target it accepts values admitted by the nullable upper bound.
    if let Ty::PlatformNullable(inner) = sup {
        let upper = platform_shape_upper_bound(oracle.platform_flexible_upper_bound(*inner));
        return assignable_inner(cx, oracle, sub, Ty::nullable(upper));
    }
    if let Ty::PlatformNullable(inner) = sub {
        return assignable_inner(cx, oracle, *inner, sup)
            || assignable_inner(cx, oracle, Ty::nullable(*inner), sup);
    }

    // Nullability. `null` fits any nullable target; `T` fits `T?`; `T?` does not fit non-null `T`.
    if sub == Ty::Null {
        return matches!(sup, Ty::Nullable(_)) || sup == Ty::Null;
    }
    if let Ty::Nullable(inner) = sup {
        // A symbolic source has the nullability of its upper bound. Follow that bound before
        // stripping the target's `?`: `<T : Any?>` is a subtype of `Any?` (and therefore fits a
        // star projection), but is not a subtype of non-null `Any`. Preserve the direct `T <: T?`
        // relation without expanding T to an unrelated upper bound first.
        if matches!(sub, Ty::TyParam(source, _) if matches!(*inner, Ty::TyParam(target, _) if source == target))
        {
            return true;
        }
        if let Ty::TyParam(_, bound) = sub {
            return *bound != sub && assignable_inner(cx, oracle, *bound, sup);
        }
        return is_assignable(cx, oracle, sub.non_null(), *inner);
    }
    if matches!(sub, Ty::Nullable(_)) {
        // sup is non-null here (the `Nullable` sup arm returned above).
        return false;
    }

    // A source type variable is a subtype of its declared upper bound, including another variable
    // (`<R, T : R>` means `T <: R`). Check this before rejecting an unbound target variable; otherwise
    // the target arm hides the only declared proof of that relation.
    if let (Ty::TyParam(source_name, source_bound), Ty::TyParam(target_name, _)) = (sub, sup) {
        if source_name != target_name {
            let source = cx.lookup(source_name).unwrap_or(*source_bound);
            return source != sub && assignable_inner(cx, oracle, source, sup);
        }
    }

    // An unbound target type variable is not its upper bound: `String <: Any?` does not prove
    // `String <: T`. Generic inference must bind T before ordinary assignability is asked. This comes
    // AFTER nullability so the same symbolic type still obeys Kotlin's ordinary `T <: T?` relation;
    // expanding the source `T` to its bound first loses that identity.
    if let Ty::TyParam(name, _) = sup {
        return cx
            .lookup(name)
            .is_some_and(|target| target != sup && assignable_inner(cx, oracle, sub, target));
    }
    if let Ty::TyParam(name, bound) = sub {
        let source = cx.lookup(name).unwrap_or(*bound);
        return source != sub && assignable_inner(cx, oracle, source, sup);
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
                    .all(|(sp, pp)| assignable_inner(cx, oracle, *pp, *sp))
                // Return is COVARIANT.
                && assignable_inner(cx, oracle, a.ret, b.ret)
        }
        (Ty::Obj(_, _), Ty::Obj(_, _)) => obj_assignable(cx, oracle, sub, sup),
        _ => {
            // Mixed reference shapes (`Ty::String` vs `Ty::Obj("kotlin/CharSequence")`, `Fun` vs `Obj`
            // FunctionN) compare through their Kotlin class identity.
            class_assignable(oracle, sub, sup)
        }
    }
}

/// Two `Obj` reference types: the sub-class reaches the super-class in the hierarchy AND every type
/// argument matches covariantly.
fn obj_assignable(cx: &TyCtx, oracle: &dyn TypeOracle, sub: Ty, sup: Ty) -> bool {
    let Some(applied_sub) = applied_supertype(oracle, sub, sup) else {
        return false;
    };
    let target = sup.obj_internal();
    sup.type_args()
        .iter()
        .zip(applied_sub.type_args().iter())
        .enumerate()
        .all(|(index, (&p, &a))| {
            match p {
                Ty::OutProjection(expected) | Ty::StarProjection(expected) => {
                    if matches!(a, Ty::InProjection(_)) {
                        let readable = target
                            .map(|owner| oracle.type_param_upper_bounds(owner, index))
                            .filter(|bounds| !bounds.is_empty())
                            .unwrap_or_else(|| vec![Ty::nullable(Ty::obj("kotlin/Any"))]);
                        return readable
                            .into_iter()
                            .any(|bound| assignable_inner(cx, oracle, bound, *expected));
                    }
                    let mut captured = cx.clone();
                    capture_projection_parameters(&mut captured, oracle, *expected, a);
                    return assignable_inner(
                        &captured,
                        oracle,
                        a.projection_inner().unwrap_or(a),
                        *expected,
                    );
                }
                Ty::InProjection(expected) => {
                    if matches!(a, Ty::OutProjection(_) | Ty::StarProjection(_)) {
                        return *expected == Ty::Nothing;
                    }
                    return assignable_inner(
                        cx,
                        oracle,
                        *expected,
                        a.projection_inner().unwrap_or(a),
                    );
                }
                _ => {}
            }
            match target
                .map(|owner| oracle.type_param_variance(owner, index))
                .unwrap_or(crate::types::TypeVariance::Invariant)
            {
                crate::types::TypeVariance::Out => assignable_inner(cx, oracle, a, p),
                crate::types::TypeVariance::In => assignable_inner(cx, oracle, p, a),
                crate::types::TypeVariance::Invariant => same_flexible_type(
                    normalized_type_argument(cx, a),
                    normalized_type_argument(cx, p),
                ),
            }
        })
}

/// Bind the declaration variables occurring inside a star projection's readable upper bound to the
/// corresponding actual shape. F-bounds such as `S : Entity<D, S>` otherwise leave `S` unbound and
/// reject `EntityImpl<D> : Entity<D, EntityImpl<D>>` as an argument of `Entity<D, *>`.
fn capture_projection_parameters(
    cx: &mut TyCtx,
    oracle: &dyn TypeOracle,
    template: Ty,
    actual: Ty,
) {
    match template {
        Ty::TyParam(name, _) => {
            if cx.lookup(name).is_none() && actual != template {
                cx.bind(name, actual.projection_inner().unwrap_or(actual));
            }
        }
        Ty::Obj(_, template_args) => {
            let Some(applied) = applied_supertype(oracle, actual, template) else {
                return;
            };
            for (&template, &actual) in template_args.iter().zip(applied.type_args()) {
                capture_projection_parameters(cx, oracle, template, actual);
            }
        }
        Ty::Fun(template) => {
            let Ty::Fun(actual) = actual.projection_inner().unwrap_or(actual) else {
                return;
            };
            for (&template, &actual) in template.params.iter().zip(&actual.params) {
                capture_projection_parameters(cx, oracle, template, actual);
            }
            capture_projection_parameters(cx, oracle, template.ret, actual.ret);
        }
        Ty::Nullable(template)
        | Ty::PlatformNullable(template)
        | Ty::InProjection(template)
        | Ty::OutProjection(template)
        | Ty::StarProjection(template) => capture_projection_parameters(
            cx,
            oracle,
            *template,
            actual.projection_inner().unwrap_or(actual).non_null(),
        ),
        Ty::Unit | Ty::Pending | Ty::Nothing | Ty::Null | Ty::Error => {}
    }
}

fn same_type_argument(left: Ty, right: Ty) -> bool {
    match (left, right) {
        (Ty::TyParam(a, _), Ty::TyParam(b, _)) => a == b,
        (Ty::Obj(a, aa), Ty::Obj(b, ba)) => {
            a == b
                && aa.len() == ba.len()
                && aa
                    .iter()
                    .zip(ba)
                    .all(|(&left, &right)| same_flexible_type(left, right))
        }
        (Ty::Nullable(a), Ty::Nullable(b))
        | (Ty::PlatformNullable(a), Ty::PlatformNullable(b))
        | (Ty::InProjection(a), Ty::InProjection(b))
        | (Ty::OutProjection(a), Ty::OutProjection(b))
        | (Ty::StarProjection(a), Ty::StarProjection(b)) => same_flexible_type(*a, *b),
        (Ty::Fun(a), Ty::Fun(b)) => {
            a.context_count == b.context_count
                && a.has_receiver == b.has_receiver
                && a.suspend == b.suspend
                && a.params.len() == b.params.len()
                && a.params
                    .iter()
                    .zip(&b.params)
                    .all(|(&left, &right)| same_flexible_type(left, right))
                && same_flexible_type(a.ret, b.ret)
        }
        _ => left == right,
    }
}

/// Semantic type-shape equality with Java platform nullability treated as its flexible interval.
/// This is shared by invariant type-argument comparison and declaration override-slot matching;
/// neither operation may turn the provider's `T!` into a fixed nullable or non-null type.
pub(crate) fn same_flexible_type(left: Ty, right: Ty) -> bool {
    match (left, right) {
        (Ty::PlatformNullable(left), Ty::PlatformNullable(right)) => {
            same_type_argument(*left, *right)
        }
        (Ty::PlatformNullable(inner), right) => {
            same_type_argument(*inner, right) || same_type_argument(Ty::nullable(*inner), right)
        }
        (left, Ty::PlatformNullable(inner)) => {
            same_type_argument(left, *inner) || same_type_argument(left, Ty::nullable(*inner))
        }
        _ => same_type_argument(left, right),
    }
}

/// Resolve a generic argument only for invariant identity comparison. A free type parameter becomes
/// its declared bound; an inferred parameter becomes its binding. It is deliberately not a wildcard:
/// `Box<T : Any>` is therefore not assignable to `Box<String>` until inference binds `T` to `String`.
fn normalized_type_argument(cx: &TyCtx, ty: Ty) -> Ty {
    fn resolve(cx: &TyCtx, current: Ty, origin: Ty) -> Ty {
        let Ty::TyParam(name, _) = current else {
            return current;
        };
        let Some(next) = cx.lookup(name) else {
            return current;
        };
        if next == current || next == origin {
            current
        } else {
            resolve(cx, next, origin)
        }
    }

    resolve(cx, ty, ty)
}

/// Map `sub` through its applied generic hierarchy to the classifier denoted by `sup`, keeping the
/// type arguments the hierarchy carries (`Login` against `BaseCmd` → `BaseCmd<Cmd>`). This single
/// walk serves both erased class reachability and generic argument comparison, so no caller can
/// observe the class relationship while silently discarding the supertype template.
pub(crate) fn applied_supertype(oracle: &dyn TypeOracle, sub: Ty, sup: Ty) -> Option<Ty> {
    let (Some(start), Some(target)) = (sub.kotlin_class_internal(), sup.kotlin_class_internal())
    else {
        return None;
    };
    let mut seen = std::collections::HashSet::new();
    seen.insert(start);
    let mut stack = vec![sub];
    while let Some(cur) = stack.pop() {
        let Some(owner) = cur.kotlin_class_internal() else {
            continue;
        };
        if oracle.same_class_name(owner, target) {
            return Some(cur);
        }
        let direct = oracle.direct_supertypes(cur);
        stack.extend(direct.into_iter().filter(|supertype| {
            supertype
                .kotlin_class_internal()
                .is_some_and(|name| seen.insert(name))
        }));
    }
    None
}

fn class_assignable(oracle: &dyn TypeOracle, sub: Ty, sup: Ty) -> bool {
    applied_supertype(oracle, sub, sup).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Ty;

    /// A tiny hand-wired hierarchy oracle for the relation's unit tests.
    struct Fake;
    impl TypeOracle for Fake {
        fn direct_supertypes(&self, ty: Ty) -> Vec<Ty> {
            let Some(internal) = ty.kotlin_class_internal() else {
                return Vec::new();
            };
            if internal.matches("app/Exact") {
                return vec![g("app/Source", ty.type_args())];
            }
            if internal.matches("app/Anonymous") {
                return vec![g("app/Box", ty.type_args())];
            }
            if internal.matches("app/EntityImpl") {
                let data = ty
                    .type_args()
                    .first()
                    .copied()
                    .unwrap_or_else(|| s("kotlin/Any"));
                return vec![g("app/Entity", &[data, g("app/EntityImpl", &[data])])];
            }
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
            s.iter().map(|x| Ty::obj(x)).collect()
        }
        fn same_class_name(&self, a: TypeName, b: TypeName) -> bool {
            a == b
                || ((a.matches("canonical/Readonly") || a.matches("canonical/Mutable"))
                    && (b.matches("canonical/Readonly") || b.matches("canonical/Mutable")))
        }
        fn type_param_variance(
            &self,
            internal: TypeName,
            _index: usize,
        ) -> crate::types::TypeVariance {
            if internal.matches("app/Sink") {
                crate::types::TypeVariance::In
            } else if internal.matches("kotlin/collections/List")
                || internal.matches("kotlin/collections/Iterable")
                || internal.matches("app/Source")
            {
                crate::types::TypeVariance::Out
            } else {
                crate::types::TypeVariance::Invariant
            }
        }
        fn platform_flexible_upper_bound(&self, lower: Ty) -> Ty {
            match lower {
                Ty::Obj(owner, arguments) if owner.matches("kotlin/collections/MutableList") => {
                    Ty::obj_args("kotlin/collections/List", &arguments)
                }
                _ => lower,
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

        let parameter = Ty::ty_param("T", Ty::nullable(s("kotlin/Any")));
        assert!(ok(parameter, Ty::nullable(parameter)));
        assert!(ok(parameter, Ty::nullable(s("kotlin/Any"))));
        assert!(!ok(parameter, s("kotlin/Any")));
        assert!(!ok(Ty::nullable(parameter), parameter));
    }

    #[test]
    fn java_platform_type_is_flexible_between_non_null_and_nullable_bounds() {
        let dog = s("app/Dog");
        let animal = s("app/Animal");
        let platform_dog = Ty::platform_nullable(dog);
        assert!(ok(platform_dog, dog));
        assert!(ok(platform_dog, Ty::nullable(dog)));
        assert!(ok(platform_dog, animal));
        assert!(ok(dog, platform_dog));
        assert!(ok(Ty::nullable(dog), platform_dog));
        assert!(ok(Ty::Null, platform_dog));
        assert!(ok(
            Ty::array(dog),
            Ty::platform_nullable(Ty::array(platform_dog))
        ));

        let platform_array =
            Ty::platform_nullable(g("kotlin/Array", &[Ty::platform_nullable(Ty::Int)]));
        assert!(ok(platform_array, g("kotlin/Array", &[Ty::Int])));
        assert!(ok(
            platform_array,
            Ty::nullable(g("kotlin/Array", &[Ty::out_projection(Ty::Int)],)),
        ));
        assert!(ok(
            g("kotlin/Array", &[Ty::out_projection(Ty::Int)]),
            platform_array,
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
    fn id_class_aliases_match_through_assignability() {
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
    fn generic_supertype_is_applied_before_target_variance() {
        assert!(ok(
            g("app/Exact", &[Ty::String]),
            g("app/Source", &[s("kotlin/Any")])
        ));
    }

    #[test]
    fn platform_collection_target_accepts_its_read_only_upper_bound() {
        assert!(ok(
            g("kotlin/collections/List", &[Ty::String]),
            Ty::platform_nullable(g(
                "kotlin/collections/MutableList",
                &[Ty::platform_nullable(Ty::String)],
            )),
        ));
    }

    #[test]
    fn invariant_star_projection_accepts_a_nullable_bounded_argument() {
        let parameter = Ty::ty_param("T", Ty::nullable(s("kotlin/Any")));
        assert!(ok(
            g("app/Box", &[parameter]),
            g(
                "app/Box",
                &[Ty::out_projection(Ty::nullable(s("kotlin/Any")))]
            )
        ));
    }

    #[test]
    fn f_bounded_star_projection_captures_its_self_type() {
        let data = Ty::ty_param("D", s("kotlin/Any"));
        let self_reference = Ty::ty_param("S", s("kotlin/Any"));
        let self_bound = g("app/Entity", &[data, self_reference]);

        assert!(ok(
            g("app/EntityImpl", &[data]),
            g("app/Entity", &[data, Ty::star_projection(self_bound)])
        ));
    }

    #[test]
    fn invariant_type_argument_requires_a_binding_not_a_wildcard() {
        let variable = Ty::ty_param("T", s("kotlin/Any"));
        let generic = g("app/Box", &[variable]);
        let concrete = g("app/Box", &[Ty::String]);

        assert!(!is_assignable(&TyCtx::new(), &Fake, generic, concrete));
        assert!(!is_assignable(&TyCtx::new(), &Fake, concrete, generic));
        assert!(is_assignable(
            &TyCtx::new().with_var("T", Ty::String),
            &Fake,
            generic,
            concrete
        ));

        let sink_of_any = g("app/Sink", &[s("kotlin/Any")]);
        let sink_of_variable = g("app/Sink", &[variable]);
        assert!(is_assignable(
            &TyCtx::new(),
            &Fake,
            sink_of_any,
            sink_of_variable
        ));
        assert!(!is_assignable(
            &TyCtx::new(),
            &Fake,
            g("app/Anonymous", &[variable]),
            g("app/Box", &[s("kotlin/Any")])
        ));
    }

    #[test]
    fn function_variance() {
        // (Animal) -> Dog  <:  (Dog) -> Animal   [param contravariant, ret covariant]
        let sub = Ty::fun(vec![s("app/Animal")], s("app/Dog"));
        let sup = Ty::fun(vec![s("app/Dog")], s("app/Animal"));
        assert!(ok(sub, sup));
        assert!(!ok(sup, sub));
        assert!(ok(
            Ty::fun(Vec::new(), Ty::Nothing),
            Ty::fun(Vec::new(), Ty::nullable(s("kotlin/Any")))
        ));
        // arity mismatch
        assert!(!ok(
            Ty::fun(vec![], Ty::Int),
            Ty::fun(vec![Ty::Int], Ty::Int)
        ));
    }

    #[test]
    fn array_invariance() {
        assert!(!ok(Ty::array(s("app/Dog")), Ty::array(s("app/Animal"))));
        assert!(!ok(Ty::array(s("app/Animal")), Ty::array(s("app/Dog"))));
    }

    #[test]
    fn type_variables() {
        // T with bound CharSequence: String <: T (via bound), and T <: Any.
        let cx = TyCtx::new().with_var("T", s("kotlin/CharSequence"));
        let tv = Ty::ty_param("T", s("kotlin/CharSequence"));
        assert!(is_assignable(&cx, &Fake, Ty::String, tv));
        assert!(is_assignable(&cx, &Fake, tv, s("kotlin/Any")));
        // A bound constrains T but is not a binding: Dog <: Any does not prove Dog <: T.
        let tv2 = Ty::ty_param("T", s("kotlin/Any"));
        assert!(!is_assignable(&TyCtx::new(), &Fake, s("app/Dog"), tv2));
        // A bound that rejects: Int is not <: T:CharSequence.
        assert!(!is_assignable(&cx, &Fake, Ty::Int, tv));

        // Nullability of a bound does not turn it into a binding either.
        let nullable_tv = Ty::ty_param("N", Ty::nullable(s("kotlin/Any")));
        assert!(!is_assignable(
            &TyCtx::new(),
            &Fake,
            Ty::nullable(s("app/Dog")),
            nullable_tv
        ));
        let nonnull_tv = Ty::ty_param("N", s("kotlin/Any"));
        assert!(!is_assignable(
            &TyCtx::new(),
            &Fake,
            Ty::nullable(s("app/Dog")),
            nonnull_tv
        ));

        let result = Ty::ty_param("R", Ty::nullable(s("kotlin/Any")));
        let derived = Ty::ty_param("T", result);
        assert!(is_assignable(&TyCtx::new(), &Fake, derived, result));
        assert!(!is_assignable(&TyCtx::new(), &Fake, result, derived));
        assert!(!is_assignable(&TyCtx::new(), &Fake, Ty::Null, result));
        assert!(!is_assignable(&TyCtx::new(), &Fake, Ty::Null, derived));
        assert!(is_assignable(
            &TyCtx::new(),
            &Fake,
            Ty::Null,
            Ty::nullable(derived)
        ));
    }

    #[test]
    fn value_class_is_not_assignable_to_its_storage_type() {
        assert!(!ok(s("app/Aid"), Ty::String));
        assert!(!ok(s("app/Aid"), s("kotlin/String")));
    }
}
