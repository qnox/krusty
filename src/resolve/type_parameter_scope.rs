//! The generic type parameters in scope at a declaration or body, with their bounds and erasures.
//!
//! [`TParams`] is the checker's view of the parameters a scope declares and inherits: each
//! parameter's first declared bound (its JVM erasure), the bounds beyond the first, and which are
//! `reified`. The free functions here resolve a written bound to that semantic form.

use super::*;

/// Generic type parameters in scope, each with its JVM erasure. A parameter with a wrappable standard
/// primitive upper bound (`<T : Int>`) erases to that PRIMITIVE — kotlinc specializes it (descriptor
/// `(I)I`, not `(Object)Object`); every other parameter erases to `java/lang/Object`.
#[derive(Default, Clone)]
pub struct TParams {
    erasure: crate::types::TyParamBindings,
    /// The subset declared `reified` (only an `inline fun` may). A class literal `T::class` is
    /// valid only on one of these. Kept HERE rather than beside the parameters so the two cannot
    /// Bounds BEYOND the first, for a parameter declared with several (`where T : A, T : B`). Kotlin
    /// gives such a parameter an INTERSECTION bound; `Ty::TyParam` carries a single one — the erasure,
    /// which is the FIRST, matching kotlinc's JVM rule. Member lookup consults these so a member
    /// declared only on a later bound still resolves.
    extra_bounds: crate::fx_hash::HashMap<String, Vec<Ty>>,
}

impl TParams {
    /// Record one parameter's bound (and the bounds beyond the first), replacing any entry of the
    /// same name — used to reassemble a `TParams` view from the scope chain's classifier bindings,
    /// outermost rung first, so an inner declaration's parameter shadows the outer one.
    pub(crate) fn insert_binding(&mut self, name: &str, bound: Ty, extra_bounds: Vec<Ty>) {
        self.erasure
            .insert(crate::types::TyParamName::new(name), bound);
        if extra_bounds.is_empty() {
            self.extra_bounds.remove(name);
        } else {
            self.extra_bounds.insert(name.to_string(), extra_bounds);
        }
    }

    /// The type each parameter in scope denotes, carrying its semantic identity and bound.
    pub(super) fn binding_types(&self) -> impl Iterator<Item = Ty> + '_ {
        self.erasure.values().copied()
    }

    /// Each parameter in scope by its source name, with the type its occurrences denote.
    pub(super) fn bindings(&self) -> impl Iterator<Item = (&str, Ty)> + '_ {
        self.erasure
            .iter()
            .map(|(name, binding)| (name.as_str(), *binding))
    }

    /// The bounds `name` declares beyond the first.
    pub(crate) fn extra_bounds_of(&self, name: &str) -> Vec<Ty> {
        self.extra_bounds.get(name).cloned().unwrap_or_default()
    }

    pub(crate) fn contains(&self, name: &str) -> bool {
        self.erasure.contains_key(name)
    }

    /// The erasure of type parameter `name` (`Object` if unbounded / reference-bounded).
    /// The declared upper bound of `name` — what the checker types `T` as.
    pub(crate) fn bound(&self, name: &str) -> Ty {
        self.erasure
            .get(name)
            .cloned()
            .unwrap_or_else(|| Ty::obj("kotlin/Any"))
    }

    /// The semantic identity of the parameter written `name`: the identity its binding carries,
    /// or the spelling itself for a parameter bound to a substitution rather than to itself.
    pub(crate) fn identity(&self, name: &str) -> crate::types::TyParamName {
        crate::types::TyParamName::new(self.bound(name).ty_param_name().unwrap_or(name))
    }

    /// Build directly from explicit name→type bindings — a SUBSTITUTION (concrete types), not an
    /// erasure. Used to map a generic method's type parameters to their receiver-supplied / inferred
    /// concrete types so `ty_of_ref` realizes `T`/`R` to `String`/`Int` rather than the erased `Object`.
    pub(crate) fn from_bindings(bindings: impl IntoIterator<Item = (String, Ty)>) -> Self {
        TParams {
            erasure: bindings
                .into_iter()
                .map(|(name, ty)| (crate::types::TyParamName::from(name), ty))
                .collect(),
            ..Default::default()
        }
    }

    /// CLASS type parameters, erased to their declared REFERENCE bound: `class Bounded<T : Cargo>`
    /// signs its constructor/field/accessors with `Lapp/Cargo;`, exactly as kotlinc does and as the
    /// FUNCTION path ([`from_decl_with`](Self::from_decl_with)) already did. A primitive-bounded
    /// parameter still erases to `Any` — kotlinc specializes those too, but krusty's value-class pass
    /// owns class-param bound handling and naive specialization there breaks the Object/value-class
    /// boundary (VerifyError), so scalar bounds keep an `Any` erasure.
    pub(crate) fn erased_with(
        names: &[String],
        bounds: &[(String, TypeRef)],
        resolve: &dyn Fn(&str) -> Option<TypeName>,
    ) -> Self {
        let mut out = TParams::from_decl_with(names, bounds, resolve);
        for erased in out.erasure.values_mut() {
            if !erased.is_reference() {
                *erased = Ty::obj("kotlin/Any");
            }
        }
        out
    }

    /// [`erased_with`](Self::erased_with) layered over an enclosing scope — the class counterpart of
    /// [`extended_with`](Self::extended_with), used to fold an inner/local class's enclosing
    /// declarations before its own parameters.
    pub(crate) fn erased_extended_with(
        &self,
        names: &[String],
        bounds: &[(String, TypeRef)],
        resolve: &dyn Fn(&str) -> Option<TypeName>,
    ) -> Self {
        let mut out = self.clone();
        let mut declared = TParams::erased_with(names, bounds, resolve);
        for name in names {
            if let Some(bound) = self.enclosing_bound_erasure(name, names, bounds) {
                declared
                    .erasure
                    .insert(crate::types::TyParamName::new(name), bound);
            }
        }
        out.erasure.extend(declared.erasure);
        out.extra_bounds.extend(declared.extra_bounds);
        out
    }

    /// Build from declared names + their upper bounds, resolving a CLASS/interface bound to its JVM
    /// type so member access on `T` resolves and the descriptor erases to the bound (`<T: CharSequence>`
    /// → `java/lang/CharSequence`, not `Object`). `resolve` maps a bound's simple class name to its JVM
    /// internal name (primitive/`String` bounds need no resolver). Without a resolver (`from_decl`) only
    /// primitive bounds are recovered — a reference bound stays `Any`.
    pub(crate) fn from_decl_with(
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
                let mut seen: crate::fx_hash::HashSet<&str> = crate::fx_hash::HashSet::default();
                let erased = loop {
                    let b = bounds.iter().find_map(|(bn, b)| (bn == cur).then_some(b));
                    match b {
                        // The bound is itself a (non-nullable, un-parameterized) type parameter of this
                        // declaration — hop to it and keep chasing.
                        Some(tb)
                            if !tb.nullable()
                                && names.iter().any(|m| m == &tb.name)
                                && seen.insert(cur) =>
                        {
                            cur = tb.name.as_str();
                        }
                        other => break tparam_bound_erasure(other, resolve),
                    }
                };
                (crate::types::TyParamName::from(n), erased)
            })
            .collect();
        // Every bound after the first for each name. The first IS the erasure above; the rest are only
        // reachable through `extra_bounds`, since `Ty::TyParam` holds exactly one.
        let extra_bounds = names
            .iter()
            .filter_map(|n| {
                let rest: Vec<Ty> = bounds
                    .iter()
                    .filter(|(bn, _)| bn == n)
                    .skip(1)
                    .map(|(_, b)| tparam_bound_erasure(Some(b), resolve))
                    .filter(|ty| *ty != Ty::obj("kotlin/Any"))
                    .collect();
                (!rest.is_empty()).then(|| (n.clone(), rest))
            })
            .collect();
        TParams {
            erasure,
            extra_bounds,
        }
    }

    /// Build the physical type-parameter view used by a JVM-facing callable signature. Kotlin
    /// permits an interface bound to be written before a concrete class bound, but classfile
    /// erasure always uses the class bound (`T : Comparable<T>, T : Number` -> `Number`). Reorder
    /// each parameter's bound list only for this erased view; [`Self::symbolic_from_decl_with`]
    /// retains the complete source intersection used by inference and overload applicability.
    pub(crate) fn from_decl_with_primary_class(
        names: &[String],
        bounds: &[(String, TypeRef)],
        resolve: &dyn Fn(&str) -> Option<TypeName>,
        is_interface: &dyn Fn(TypeName) -> bool,
    ) -> Self {
        let mut ordered = Vec::with_capacity(bounds.len());
        for name in names {
            let mut declared = bounds
                .iter()
                .filter(|(owner, _)| owner == name)
                .cloned()
                .collect::<Vec<_>>();
            if let Some(primary) = declared.iter().position(|(_, bound)| {
                !bound.nullable() && resolve(&bound.name).is_some_and(|name| !is_interface(name))
            }) {
                let class_bound = declared.remove(primary);
                ordered.push(class_bound);
            }
            ordered.extend(declared);
        }
        Self::from_decl_with(names, &ordered, resolve)
    }

    pub(crate) fn extended_with(
        &self,
        names: &[String],
        bounds: &[(String, TypeRef)],
        resolve: &dyn Fn(&str) -> Option<TypeName>,
    ) -> Self {
        let mut out = self.clone();
        let mut declared = TParams::from_decl_with(names, bounds, resolve);
        for name in names {
            if let Some(bound) = self.enclosing_bound_erasure(name, names, bounds) {
                declared
                    .erasure
                    .insert(crate::types::TyParamName::new(name), bound);
            }
        }
        out.erasure.extend(declared.erasure);
        out.extra_bounds.extend(declared.extra_bounds);
        out
    }

    pub(crate) fn enclosing_bound_erasure(
        &self,
        name: &str,
        local_names: &[String],
        bounds: &[(String, TypeRef)],
    ) -> Option<Ty> {
        let mut current = name;
        let mut seen = crate::fx_hash::HashSet::default();
        loop {
            if !seen.insert(current) {
                return None;
            }
            let bound = bounds
                .iter()
                .find_map(|(owner, bound)| (owner == current).then_some(bound))?;
            if bound.nullable()
                || bound.arg.is_some()
                || !bound.targs.is_empty()
                || !bound.fun_params.is_empty()
            {
                return None;
            }
            if local_names.iter().any(|local| local == &bound.name) {
                current = &bound.name;
                continue;
            }
            let mut erased = self.erasure.get(bound.name.as_str()).copied()?;
            let mut bound_names = crate::fx_hash::HashSet::default();
            while let TyKind::TyParam(parameter, upper) = erased.kind() {
                if !bound_names.insert(parameter) {
                    return None;
                }
                erased = upper;
            }
            return Some(erased);
        }
    }

    /// Preserve type variables while inferring a declaration's semantic shape.
    pub(crate) fn symbolic_from_decl_with(
        names: &[String],
        bounds: &[(String, TypeRef)],
        resolve: &dyn Fn(&str) -> Option<TypeName>,
    ) -> Self {
        Self::symbolic_from_decl_enclosing(names, bounds, resolve, &|_| None)
    }

    /// As [`Self::symbolic_from_decl_with`], but a bound may also name a type parameter of an
    /// ENCLOSING declaration: a local function's `fun <S : T> h(..)` inside `fun <T> outer(..)`
    /// bounds `S` by the outer `T`. Without that lookup the bound resolves to nothing and the whole
    /// declaration reads as `unresolved reference 'T'`.
    pub(crate) fn symbolic_from_decl_enclosing(
        names: &[String],
        bounds: &[(String, TypeRef)],
        resolve: &dyn Fn(&str) -> Option<TypeName>,
        enclosing: &dyn Fn(&str) -> Option<Ty>,
    ) -> Self {
        let implicit = Ty::nullable(Ty::obj("kotlin/Any"));
        /// Resolve a bound through the ordinary shared type-shape operations while keeping references
        /// to this declaration's and enclosing declarations' type parameters symbolic.
        fn semantic_bound(
            bound: &TypeRef,
            names: &[String],
            bounds: &[(String, TypeRef)],
            enclosing: &dyn Fn(&str) -> Option<Ty>,
            resolve: &dyn Fn(&str) -> Option<TypeName>,
            visiting: &mut crate::fx_hash::HashSet<String>,
        ) -> Ty {
            tparam_bound_semantic_with(bound, resolve, &mut |candidate| {
                if let Some(ty) = enclosing(candidate) {
                    return Some(ty);
                }
                names.iter().any(|name| name == candidate).then(|| {
                    Ty::ty_param(
                        candidate,
                        parameter_bound(candidate, names, bounds, resolve, enclosing, visiting),
                    )
                })
            })
        }
        fn parameter_bound(
            name: &str,
            names: &[String],
            bounds: &[(String, TypeRef)],
            resolve: &dyn Fn(&str) -> Option<TypeName>,
            enclosing: &dyn Fn(&str) -> Option<Ty>,
            visiting: &mut crate::fx_hash::HashSet<String>,
        ) -> Ty {
            let implicit = Ty::nullable(Ty::obj("kotlin/Any"));
            if !visiting.insert(name.to_string()) {
                return implicit;
            }
            let bound = bounds
                .iter()
                .find_map(|(owner, bound)| (owner == name).then_some(bound));
            let result = match bound {
                Some(bound)
                    if !bound.nullable()
                        && !bound.definitely_non_null()
                        && bound.arg.is_none()
                        && bound.targs.is_empty()
                        && bound.fun_params.is_empty()
                        && names.iter().any(|candidate| candidate == &bound.name) =>
                {
                    Ty::ty_param(
                        &bound.name,
                        parameter_bound(&bound.name, names, bounds, resolve, enclosing, visiting),
                    )
                }
                Some(bound) => semantic_bound(bound, names, bounds, enclosing, resolve, visiting),
                None => implicit,
            };
            visiting.remove(name);
            result
        }
        let mut out = TParams::default();
        for name in names {
            let direct_bounds = bounds
                .iter()
                .filter_map(|(owner, bound)| (owner == name).then_some(bound))
                .map(|bound| {
                    let is_parameter_bound = !bound.nullable()
                        && !bound.definitely_non_null()
                        && bound.arg.is_none()
                        && bound.targs.is_empty()
                        && bound.fun_params.is_empty()
                        && names.iter().any(|candidate| candidate == &bound.name);
                    if is_parameter_bound {
                        Ty::ty_param(
                            &bound.name,
                            parameter_bound(
                                &bound.name,
                                names,
                                bounds,
                                resolve,
                                enclosing,
                                &mut crate::fx_hash::HashSet::default(),
                            ),
                        )
                    } else {
                        semantic_bound(
                            bound,
                            names,
                            bounds,
                            enclosing,
                            resolve,
                            &mut crate::fx_hash::HashSet::default(),
                        )
                    }
                })
                .collect::<Vec<_>>();
            let bound = direct_bounds.first().copied().unwrap_or(implicit);
            out.erasure.insert(
                crate::types::TyParamName::new(name),
                Ty::ty_param(name, bound),
            );
            let mut extra_bounds = direct_bounds.into_iter().skip(1).collect::<Vec<_>>();
            // `direct_bounds` already contains every constraint written on this parameter. Flatten
            // additional constraints only through a bare type-parameter edge (`T : X`); starting
            // the traversal at `T` itself would decode each direct generic bound a second time
            // without symbolic arguments (`Comparable<T>` -> `Comparable<Any>`).
            for inherited in bounds
                .iter()
                .filter_map(|(owner, candidate)| {
                    (owner == name
                        && !candidate.nullable()
                        && !candidate.definitely_non_null()
                        && candidate.arg.is_none()
                        && candidate.targs.is_empty()
                        && candidate.fun_params.is_empty()
                        && names.iter().any(|parameter| parameter == &candidate.name))
                    .then_some(candidate.name.as_str())
                })
                .flat_map(|inherited| {
                    declared_tparam_semantic_bounds(inherited, names, bounds, resolve)
                })
            {
                if inherited != bound && !extra_bounds.contains(&inherited) {
                    extra_bounds.push(inherited);
                }
            }
            if !extra_bounds.is_empty() {
                out.extra_bounds.insert(name.clone(), extra_bounds);
            }
        }
        out
    }

    /// Give one declaration's semantic formals stable identities while keeping source names as the
    /// keys used by lexical type lookup. Distinct declarations may both write `T`; their inference
    /// variables must still be distinct.
    fn alpha_renamed(&self, names: &[String], key: impl Fn(usize, &str) -> &'static str) -> Self {
        let identities = names
            .iter()
            .enumerate()
            .map(|(index, source)| (source.as_str(), key(index, source)))
            .collect::<crate::fx_hash::HashMap<_, _>>();
        let mut out = self.clone();
        for source in names {
            out.insert_binding(
                source,
                crate::types::ty_rename_params(self.bound(source), &identities),
                self.extra_bounds_of(source)
                    .into_iter()
                    .map(|bound| crate::types::ty_rename_params(bound, &identities))
                    .collect(),
            );
        }
        out
    }

    /// A declaration start is unique within its file and stable for the compilation. It gives
    /// top-level, member, class, and local formals the same semantic identity scheme.
    pub(super) fn alpha_renamed_declaration(
        &self,
        names: &[String],
        compilation: u64,
        file: u32,
        declaration_start: u32,
    ) -> Self {
        self.alpha_renamed(names, |index, source| {
            crate::types::declaration_type_parameter(
                compilation,
                file,
                declaration_start,
                index,
                source,
            )
        })
    }

    pub(crate) fn symbolic_extended_with(
        &self,
        names: &[String],
        bounds: &[(String, TypeRef)],
        resolve: &dyn Fn(&str) -> Option<TypeName>,
    ) -> Self {
        let declared = TParams::symbolic_from_decl_enclosing(names, bounds, resolve, &|name| {
            self.erasure.get(name).copied()
        });
        let mut out = self.clone();
        out.erasure.extend(declared.erasure);
        out.extra_bounds.extend(declared.extra_bounds);
        out
    }
}

/// The JVM erasure of a type parameter from its declared upper bound. kotlinc erases a bounded `T` to
/// its bound's type — a specializable scalar bound (`<T: Int>`) to that primitive, any other
/// reference bound (`<T: CharSequence>`, `<T: Comparable<T>>`, a user class) to the bound's class — so a
/// value of type `T` accesses the bound's members and the descriptor uses the bound, not `Object`.
/// `Any` when there's no bound, a nullable bound, an unresolved bound, or a non-specializable
/// unsigned/value primitive.
fn tparam_bound_erasure(b: Option<&TypeRef>, resolve: &dyn Fn(&str) -> Option<TypeName>) -> Ty {
    let any = Ty::obj("kotlin/Any");
    let Some(b) = b else { return any };
    // A nullable / generic-instantiated bound name still resolves by its head: `<T: Comparable<T>>`
    // erases to `Comparable` (type args drop on erasure). A nullable bound keeps `Any` (conservative).
    if b.nullable() {
        return any;
    }
    if let Some(prim) = Ty::from_name(&b.name) {
        // Scalar bounds specialize; `String`/`Any` reference bounds erase to themselves; unsigned
        // and value primitives are not specialized here.
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

/// Follow a declared bound through bare type-parameter references.
pub(crate) fn declared_tparam_semantic_bound(
    name: &str,
    names: &[String],
    bounds: &[(String, TypeRef)],
    resolve: &dyn Fn(&str) -> Option<TypeName>,
) -> Option<Ty> {
    let mut current = name;
    let mut seen = crate::fx_hash::HashSet::default();
    loop {
        let bound = bounds
            .iter()
            .find_map(|(parameter, bound)| (parameter == current).then_some(bound))?;
        if !bound.nullable()
            && !bound.definitely_non_null()
            && bound.arg.is_none()
            && bound.targs.is_empty()
            && bound.fun_params.is_empty()
            && names.iter().any(|candidate| candidate == &bound.name)
        {
            if !seen.insert(current) {
                return Some(Ty::obj("kotlin/Any"));
            }
            current = &bound.name;
            continue;
        }
        return Some(tparam_bound_semantic(bound, resolve));
    }
}

/// All semantic upper bounds of `name`, including bounds inherited through another parameter.
/// `<T : X, X : Comparable<UInt>> where X : UInt` therefore gives `T` the same intersection
/// `[Comparable, UInt]` as `X`. JVM erasure still uses the first entry; member/operator lookup sees
/// the complete intersection.
fn declared_tparam_semantic_bounds(
    name: &str,
    names: &[String],
    bounds: &[(String, TypeRef)],
    resolve: &dyn Fn(&str) -> Option<TypeName>,
) -> Vec<Ty> {
    fn collect(
        current: &str,
        names: &[String],
        bounds: &[(String, TypeRef)],
        resolve: &dyn Fn(&str) -> Option<TypeName>,
        visiting: &mut crate::fx_hash::HashSet<String>,
        out: &mut Vec<Ty>,
    ) {
        if !visiting.insert(current.to_string()) {
            return;
        }
        for bound in bounds
            .iter()
            .filter_map(|(parameter, bound)| (parameter == current).then_some(bound))
        {
            let parameter_bound = !bound.nullable()
                && !bound.definitely_non_null()
                && bound.arg.is_none()
                && bound.targs.is_empty()
                && bound.fun_params.is_empty()
                && names.iter().any(|candidate| candidate == &bound.name);
            if parameter_bound {
                collect(&bound.name, names, bounds, resolve, visiting, out);
            } else {
                let ty = tparam_bound_semantic(bound, resolve);
                if !out.contains(&ty) {
                    out.push(ty);
                }
            }
        }
        visiting.remove(current);
    }

    let mut out = Vec::new();
    collect(
        name,
        names,
        bounds,
        resolve,
        &mut crate::fx_hash::HashSet::default(),
        &mut out,
    );
    out
}

/// Resolve a type-parameter bound without erasing nullability.
fn tparam_bound_semantic_with(
    b: &TypeRef,
    resolve: &dyn Fn(&str) -> Option<TypeName>,
    parameter: &mut dyn FnMut(&str) -> Option<Ty>,
) -> Ty {
    let parameter_reference = (b.arg.is_none() && b.targs.is_empty() && b.fun_params.is_empty())
        .then(|| parameter(&b.name))
        .flatten();
    let base = parameter_reference
        .or_else(|| {
            typeref_leaf(b, &mut |component| {
                tparam_bound_semantic_with(component, resolve, parameter)
            })
        })
        .or_else(|| {
            resolve(&b.name).map(|internal| {
                let arguments = b
                    .targs
                    .iter()
                    .map(|argument| {
                        projected_typeref_argument(
                            argument,
                            tparam_bound_semantic_with(argument, resolve, parameter),
                            Ty::nullable(Ty::obj("kotlin/Any")),
                        )
                    })
                    .collect::<Vec<_>>();
                Ty::obj_args_name(internal, &arguments)
            })
        })
        .unwrap_or_else(|| Ty::obj("kotlin/Any"));
    if b.nullable() {
        Ty::nullable(base)
    } else {
        base
    }
}

fn tparam_bound_semantic(b: &TypeRef, resolve: &dyn Fn(&str) -> Option<TypeName>) -> Ty {
    tparam_bound_semantic_with(b, resolve, &mut |_| None)
}
