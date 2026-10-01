//! Extension-receiver applicability: the receiver's supertype closure, ranked by rung, and the
//! per-argument consistency of a declared receiver with the actual one.

use super::{
    classifier_callable_signature, declared_function_type, direct_supertypes,
    ty_subst_keep_unbound, unify_ty_from_symbols, GSigBinds, SourceOracle,
};
use crate::symbol_source::SymbolSource;
use crate::types::Ty;

/// Whether the declared receiver's type arguments are consistent with the actual receiver's, position by
/// position, under Kotlin's COVARIANT reading of a receiver position: each actual argument must be
/// assignable to the declared one (`ReceiverMro::rank` reaching from actual to declared). A declared
/// argument that is a type variable or `Any`/`Object` is a wildcard (an `Iterable<T>` / erased
/// `Iterable<Any>` extension binds any element). This rejects the `@JvmName` reduction variant whose
/// element does not match (`Iterable<Byte>.averageOfByte` against a `List<Double>` — `Double` is not
/// assignable to `Byte`) while accepting a nested-generic supertype (`Iterable<Iterable<T>>.flatten`
/// against `List<List<Int>>` — `List<Int>` IS assignable to `Iterable<Any>`). The erased supertype walk
/// in `ReceiverMro` alone keys on the outer class only, so it would tie the reduction variants.
///
/// A type variable of a postponed call in the actual receiver (`Buildee<FT>` inside `build { }`) is
/// not a fixed type: kotlinc adds the receiver constraint to the shared constraint system, so the
/// candidate applies when some value of the variable makes the receiver fit. Each such variable is
/// bound from its concrete counterpart in the declared receiver before the ordinary check; the
/// checker records that constraint and tests it against the variable's others when the call commits.
fn receiver_type_args_match(
    src: &dyn SymbolSource,
    decl_recv: Ty,
    recv: Ty,
    type_variables: &[String],
) -> bool {
    let recv = bind_receiver_type_variables(src, decl_recv, recv, type_variables);
    // Each actual argument must be assignable to the declared one under Kotlin's covariant receiver
    // reading. A declared argument that is a type variable or erased `Any` is a WILDCARD — the metadata
    // decode drops the nullability flag, so a `T?` receiver element reads as bare `Any`, and a nullable
    // actual (`Int?`) must still match it (`is_assignable(Int?, Any)` is correctly `false` under strict
    // Kotlin, but here `Any` stands for the erased variable, not the type `Any`).
    let cx = crate::assignable::TyCtx::new();
    let oracle = SourceOracle(src);
    if decl_recv.mentions_ty_param() {
        let mut bindings = GSigBinds::new();
        unify_ty_from_symbols(src, decl_recv, recv, &mut bindings);
        let specialized = ty_subst_keep_unbound(decl_recv, &bindings);
        // A callee-owned receiver variable may bind to a still-symbolic variable owned by the
        // caller: `Flow<Flow<T@flattenMerge>>` against `Flow<Flow<R@flatMapMerge>>`. The
        // specialized shapes are then exactly equal even though the caller's `R` quite correctly
        // remains a type parameter. Do not mistake that retained caller identity for an unbound
        // callee formal.
        if specialized == recv {
            return true;
        }
        if !specialized.mentions_ty_param() {
            return crate::assignable::is_assignable(&cx, &oracle, recv, specialized);
        }
    }
    let erased_top = |t: Ty| {
        let t = t.projection_inner().unwrap_or(t);
        matches!(t.non_null(), Ty::Obj(n, _)
            if crate::types::same(n, crate::types::wk::any())
                || crate::types::same(n, crate::types::wk::java_object()))
    };
    // Only the DECLARED side's type variables are wildcards: they are the callee's own formals,
    // bound by this very call. A type parameter in the ACTUAL receiver belongs to the caller and is
    // a fixed type there; `Iterable<T>` for a caller's `T : Comparable<T>` is not an
    // `Iterable<Double>`, so it is checked through its bounds like any other argument.
    let declared_wildcard =
        |t: Ty| t.projection_inner().unwrap_or(t).is_ty_param() || erased_top(t);
    decl_recv
        .type_args()
        .iter()
        .zip(recv.type_args().iter())
        .all(|(&d, &r)| {
            if declared_wildcard(d) || erased_top(r) {
                return true;
            }
            match d {
                Ty::InProjection(expected) => {
                    crate::assignable::is_assignable(&cx, &oracle, *expected, r)
                }
                Ty::OutProjection(expected) => {
                    crate::assignable::is_assignable(&cx, &oracle, r, *expected)
                }
                _ => crate::assignable::is_assignable(&cx, &oracle, r, d),
            }
        })
}

/// The rung of `decl_recv` in `recv`'s SOURCE-type supertype closure (0 = same class), or `None` if the
/// extension's declared receiver is neither `recv` nor a supertype of it. Uses `erased_recv` Kotlin-level
/// keys + `resolve_type` supertypes — NO JVM descriptors — so `kotlin/UInt` ≠ `kotlin/Int` ≠ `kotlin/Result`
/// are distinct by their class, a generic value-class receiver (`Result<T>`) binds a concrete one
/// (`Result<String>` — `erased_recv` drops type arguments), and `UInt` never binds an `Int` extension.
/// Replaces the descriptor-based `extension_receiver_rank`, whose value-class special-case existed only
/// because the erased `I`/`Object` descriptors tied distinct value classes together.
///
/// The receiver's erased supertype closure with its BFS rungs, computed ONCE per receiver and probed
/// per candidate. Every rank query used to run a fresh supertype BFS (hash-set churn included) per
/// candidate even though the receiver is FIXED across a call site's whole candidate set. The closure
/// is small (a handful of supertypes), so a `Vec` probe beats hashing.
pub(crate) struct ReceiverMro {
    recv: Ty,
    /// `(applied supertype, BFS rung)` in first-seen order.
    /// Empty for a receiver with no class-name key (an array): such a receiver ranks only by exact
    /// `Ty` equality or the universal `Any` fallback, exactly as the per-candidate BFS did.
    ranks: Vec<(Ty, u32)>,
    /// Type variables of enclosing postponed calls that the receiver may mention; see
    /// [`receiver_type_args_match`].
    type_variables: Vec<String>,
}

fn bind_receiver_type_variables(
    src: &dyn SymbolSource,
    decl_recv: Ty,
    recv: Ty,
    type_variables: &[String],
) -> Ty {
    if !crate::types::ty_mentions_param(recv, type_variables) {
        return recv;
    }
    let mut bindings = GSigBinds::new();
    unify_ty_from_symbols(src, recv, decl_recv, &mut bindings);
    bindings
        .retain(|variable, value| type_variables.contains(variable) && !value.mentions_ty_param());
    ty_subst_keep_unbound(recv, &bindings)
}

/// Whether two semantic function shapes form an applicable extension-receiver match. Declared
/// type parameters are bound from the actual callable shape; erased tops remain wildcards. Kotlin
/// receiver-function notation shares the same value representation and parameter list, so the
/// `has_receiver` marker does not participate here.
pub(crate) fn function_shape_matches(src: &dyn SymbolSource, actual: Ty, declared: Ty) -> bool {
    let (Ty::Fun(actual), Ty::Fun(declared)) = (actual.non_null(), declared.non_null()) else {
        return false;
    };
    let mut bindings = GSigBinds::new();
    let component_matches = |declared: Ty, actual: Ty, bindings: &mut GSigBinds| {
        if declared.is_erased_top() {
            return true;
        }
        unify_ty_from_symbols(src, declared, actual, bindings);
        ty_subst_keep_unbound(declared, bindings) == actual
    };
    actual.params.len() == declared.params.len()
        && actual.suspend == declared.suspend
        && declared
            .params
            .iter()
            .zip(&actual.params)
            .all(|(&declared, &actual)| component_matches(declared, actual, &mut bindings))
        && component_matches(declared.ret, actual.ret, &mut bindings)
}

impl ReceiverMro {
    pub(crate) fn new(src: &dyn SymbolSource, recv: Ty) -> ReceiverMro {
        let mut ranks = Vec::new();
        if let Some(internal) = recv.erased_recv().kotlin_class_internal() {
            let root = if recv.non_null().obj_internal().is_some() {
                recv.non_null()
            } else {
                Ty::obj_name(internal)
            };
            let mut frontier = vec![root];
            let mut seen = std::collections::HashSet::new();
            let mut rung = 0u32;
            while !frontier.is_empty() {
                let mut next = Vec::new();
                for ty in frontier {
                    let Some(internal) = ty.kotlin_class_internal() else {
                        continue;
                    };
                    if !seen.insert(internal) {
                        continue;
                    }
                    ranks.push((ty, rung));
                    next.extend(direct_supertypes(src, ty));
                }
                frontier = next;
                rung += 1;
            }
        }
        ReceiverMro {
            recv,
            ranks,
            type_variables: Vec::new(),
        }
    }

    pub(crate) fn with_type_variables(mut self, type_variables: &[String]) -> Self {
        self.type_variables = type_variables.to_vec();
        self
    }

    /// Use the applied supertype unless its classpath signature erased every argument.
    fn binding_receiver_for(&self, applied: Ty) -> Ty {
        let applied_args = applied.type_args();
        let recv_args = self.recv.type_args();
        if !recv_args.is_empty()
            && (applied_args.is_empty()
                || (recv_args.len() == applied_args.len()
                    && applied_args
                        .iter()
                        .all(|arg| arg.is_erased_top() || arg.is_ty_param())))
        {
            self.recv
        } else {
            applied
        }
    }

    pub(super) fn match_receiver(
        &self,
        src: &dyn SymbolSource,
        decl_recv: Ty,
    ) -> Option<(u32, Ty)> {
        // A generic receiver may bind its parameter to a nullable type even though a bare T is not
        // itself a nullable value occurrence. An explicit non-null upper bound closes that route.
        let accepts_nullable = decl_recv.admits_null()
            || matches!(decl_recv, Ty::TyParam(_, bound) if bound.upper_bound_admits_null());
        // The null literal has no classifier hierarchy of its own, but it is a valid receiver for
        // every nullable extension receiver. Candidate specificity is decided after this applicability
        // rung; inventing a class key for `Null` would incorrectly make it a member of `Any`'s MRO.
        if self.recv == Ty::Null || (self.recv.is_nullable() && self.recv.non_null() == Ty::Nothing)
        {
            return accepts_nullable.then_some((0, self.recv));
        }
        if self.recv.is_nullable() && !accepts_nullable {
            return None;
        }
        // Function types have no classifier hierarchy to walk. A generic extension receiver such as
        // `suspend () -> T` must nevertheless admit `suspend () -> Unit`; the later generic-binding
        // pass binds `T`. Compare the semantic function shape here and treat only declared type
        // parameters/erased tops as wildcards—no classifier-name or arity reconstruction is involved.
        // Receiver-function notation is not a distinct function class: `A.() -> R` and `(A) -> R`
        // have the same parameter list and values freely cross that notation boundary. Keep the flag
        // for lambda binding, but do not make it part of extension-receiver applicability.
        if function_shape_matches(src, self.recv, decl_recv) {
            return Some((0, self.recv));
        }
        if declared_function_type(src, decl_recv)
            .is_some_and(|declared| function_shape_matches(src, self.recv, declared))
        {
            return Some((0, self.recv));
        }
        // A nominal classifier may implement a function type directly or through an interface.
        // Its member scope remains nominal, but extension applicability uses the exact callable
        // supertype shape published by the provider. Bind generic receiver slots from that shape;
        // returning the nominal receiver here loses `T` in `suspend () -> T` before selection.
        if matches!(decl_recv.non_null(), Ty::Fun(_)) {
            let callable = classifier_callable_signature(src, self.recv)?;
            if function_shape_matches(src, callable, decl_recv) {
                let callable_rung = self
                    .ranks
                    .iter()
                    .find_map(|(applied, rung)| {
                        let internal = applied.kotlin_class_internal()?;
                        src.classifier(internal)?
                            .callable_signature
                            .is_some()
                            .then_some(rung.saturating_add(1))
                    })
                    .unwrap_or(1);
                return Some((callable_rung, callable));
            }
        }
        // Same source type — rung 0. Plain `Ty` equality (interned, NO erasure): the exact receiver an
        // extension is declared on. This is the ONLY rank an ARRAY receiver (`IntArray.sum()`) can carry
        // besides the universal `Any` — an array has no class-name key in the closure, and its
        // element type must be matched exactly (an `IntArray` extension must not bind an `Array<String>`).
        if self.recv.non_null() == decl_recv.non_null() {
            return Some((0, self.recv));
        }
        let want = decl_recv.erased_recv().kotlin_class_internal();
        if let Some(want) = want {
            if let Some(&(applied, rung)) = self.ranks.iter().find(|(applied, _)| {
                if applied.kotlin_class_internal() != Some(want) {
                    return false;
                }
                let binding_receiver = self.binding_receiver_for(*applied);
                receiver_type_args_match(src, decl_recv, binding_receiver, &self.type_variables)
            }) {
                let binding_receiver = if decl_recv.is_ty_param() || decl_recv.is_erased_top() {
                    self.recv
                } else {
                    self.binding_receiver_for(applied)
                };
                return Some((rung, binding_receiver));
            }
        }
        // A universal `Any`-receiver extension (`<T> T.let`) applies to every receiver — arrays included
        // — at lowest precedence.
        want.is_some_and(|name| crate::types::same(name, crate::types::wk::any()))
            .then_some((u32::MAX - 1, self.recv))
    }

    pub(crate) fn rank(&self, src: &dyn SymbolSource, decl_recv: Ty) -> Option<u32> {
        self.match_receiver(src, decl_recv).map(|(rank, _)| rank)
    }

    pub(super) fn binding_receiver(&self, src: &dyn SymbolSource, decl_recv: Ty) -> Option<Ty> {
        self.match_receiver(src, decl_recv)
            .map(|(_, applied)| applied)
    }
}

#[cfg(test)]
mod tests {
    use super::receiver_type_args_match;
    use crate::types::Ty;

    #[test]
    fn both_caller_bounds_specialize_a_nullable_any_receiver_by_equality() {
        let any = Ty::obj("kotlin/Any");
        let iterable = |argument| Ty::obj_args("kotlin/collections/Iterable", &[argument]);
        let declared = iterable(Ty::nullable(Ty::ty_param("stdlib:T", any)));
        let source = crate::libraries::EmptySymbolSource;

        let non_null_caller = Ty::ty_param("caller:T", any);
        let non_null_receiver = iterable(Ty::nullable(non_null_caller));
        // These callers are ordinary bounds, not variables of a postponed call.
        assert!(receiver_type_args_match(
            &source,
            declared,
            non_null_receiver,
            &[]
        ));

        let nullable_caller = Ty::ty_param("caller:U", Ty::nullable(any));
        let nullable_receiver = iterable(Ty::nullable(nullable_caller));
        assert!(receiver_type_args_match(
            &source,
            declared,
            nullable_receiver,
            &[]
        ));
        assert_eq!(non_null_caller.ty_param_bound(), Some(any));
        assert_eq!(nullable_caller.ty_param_bound(), Some(Ty::nullable(any)));
    }
}
