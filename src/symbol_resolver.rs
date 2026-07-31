//! Call resolution — the binding layer that sits *above* a [`SymbolSource`].
//!
//! A [`SymbolSource`] is an argument-independent metadata oracle: given a name (and optional receiver)
//! it returns every overload with its raw signature and flags ([`crate::libraries::FunctionSet`]). It
//! does no overload selection and no type-variable binding.
//!
//! [`SymbolResolver`] is the arg-dependent layer on top: given the actual argument types at a call site
//! it selects the right overload and binds generic receiver/parameter/return types. It uses
//! [`crate::libraries::SemanticPlatform`] for source-level library facts; backend descriptors and runtime
//! ABI are not part of resolution.

use crate::libraries::{
    FnKind, FunctionInfo, FunctionSet, GenericSig, InlineKind, LibraryCallable, LibraryMember,
    Origin, PropKind, PropertyInfo, SemanticPlatform,
};
use crate::symbol_source::SymbolSource;
use crate::types::{type_name, Ty, TypeName, Visibility};

/// Result of inherited nested-classifier lookup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InheritedNestedClassifier {
    NotFound,
    Found(TypeName),
    Ambiguous,
}

impl InheritedNestedClassifier {
    pub(crate) fn found(self) -> Option<TypeName> {
        match self {
            Self::Found(internal) => Some(internal),
            Self::NotFound | Self::Ambiguous => None,
        }
    }
}

/// Return a source class and its lexical owners, nearest first.
pub(crate) fn lexical_enclosing_classifier_names(
    owner: TypeName,
    mut classifier_exists: impl FnMut(TypeName) -> bool,
) -> Vec<TypeName> {
    let rendered = owner.render();
    let mut candidate = rendered.as_str();
    let mut owners = Vec::new();
    loop {
        let internal = type_name(candidate);
        if classifier_exists(internal) {
            owners.push(internal);
        }
        let Some((enclosing, _)) = candidate.rsplit_once('$') else {
            break;
        };
        candidate = enclosing;
    }
    owners
}

pub(crate) fn inherited_nested_classifier_name(
    name: &str,
    roots: Vec<TypeName>,
    mut direct_supertypes: impl FnMut(TypeName) -> Vec<TypeName>,
    mut classifier_exists: impl FnMut(TypeName) -> bool,
) -> InheritedNestedClassifier {
    if name.contains(['.', '/', '$']) {
        return InheritedNestedClassifier::NotFound;
    }
    let mut level = roots;
    let mut seen = std::collections::HashSet::new();
    while !level.is_empty() {
        let mut matches = std::collections::HashSet::new();
        let mut next = Vec::new();
        for owner in level {
            if !seen.insert(owner) {
                continue;
            }
            let candidate = type_name(&format!("{}${name}", owner.render()));
            if classifier_exists(candidate) {
                matches.insert(candidate);
            }
            next.extend(direct_supertypes(owner));
        }
        match matches.len() {
            0 => level = next,
            1 => {
                return InheritedNestedClassifier::Found(
                    matches
                        .into_iter()
                        .next()
                        .expect("one inherited classifier"),
                )
            }
            _ => return InheritedNestedClassifier::Ambiguous,
        }
    }
    InheritedNestedClassifier::NotFound
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LambdaCallShape {
    pub param_types: Option<Vec<Vec<Ty>>>,
    pub receivers: Option<Vec<Option<Ty>>>,
    pub context_counts: Option<Vec<usize>>,
    pub materialized: Option<Vec<bool>>,
}

#[derive(Clone, Debug)]
pub(crate) struct CallableImport {
    package: TypeName,
    declared_name: String,
}

impl CallableImport {
    pub(crate) fn new(package: TypeName, declared_name: String) -> Self {
        Self {
            package,
            declared_name,
        }
    }
}

/// Name-aware import scope for unqualified top-level and extension callables.
#[derive(Clone, Debug)]
pub(crate) struct FunctionImportScope {
    explicit: std::collections::HashMap<String, CallableImport>,
    levels: [Vec<TypeName>; 4],
}

impl FunctionImportScope {
    pub(crate) fn new(
        explicit: std::collections::HashMap<String, CallableImport>,
        levels: [Vec<TypeName>; 4],
    ) -> Self {
        Self { explicit, levels }
    }

    pub(crate) fn explicit_package(&self, name: &str) -> Option<TypeName> {
        self.explicit
            .get(name)
            .map(|import| import.package)
            .or_else(|| {
                name.strip_suffix("$default")
                    .and_then(|base| self.explicit.get(base).map(|import| import.package))
            })
    }

    fn explicit_target(&self, name: &str) -> Option<(TypeName, String)> {
        if let Some(import) = self.explicit.get(name) {
            return Some((import.package, import.declared_name.clone()));
        }
        let base = name.strip_suffix("$default")?;
        let import = self.explicit.get(base)?;
        Some((import.package, format!("{}$default", import.declared_name)))
    }

    pub(crate) fn levels(&self) -> &[Vec<TypeName>; 4] {
        &self.levels
    }
}

pub(crate) type GSigBinds = std::collections::HashMap<String, Ty>;

/// [`crate::assignable::TypeOracle`] over a federated [`SymbolSource`] (module ∪ classpath): the class
/// hierarchy walk the one assignability relation needs. Kotlin-name supertypes, no JVM canonicalization —
/// source-type space, as `ReceiverMro` uses.
pub(crate) struct SourceOracle<'a>(pub &'a dyn SymbolSource);

impl crate::assignable::TypeOracle for SourceOracle<'_> {
    fn direct_supertypes(&self, internal: TypeName) -> Vec<TypeName> {
        self.0
            .resolve_type_name(internal)
            .map(|t| t.supertypes.iter_ids().collect())
            .unwrap_or_default()
    }
    fn value_underlying(&self, ty: Ty) -> Option<Ty> {
        self.0
            .resolve_type_name(ty.kotlin_class_internal()?)
            .and_then(|t| t.value_underlying)
    }

    fn same_class_name(&self, a: TypeName, b: TypeName) -> bool {
        a == b
    }
}

/// [`crate::assignable::TypeOracle`] over a [`SemanticPlatform`].
pub(crate) struct PlatformOracle<'a>(pub &'a dyn SemanticPlatform);

impl crate::assignable::TypeOracle for PlatformOracle<'_> {
    fn direct_supertypes(&self, internal: TypeName) -> Vec<TypeName> {
        self.0
            .resolve_type_name(internal)
            .map(|t| t.supertypes.iter_ids().collect())
            .unwrap_or_default()
    }
    fn value_underlying(&self, ty: Ty) -> Option<Ty> {
        self.0.value_underlying(ty)
    }
    fn same_class_name(&self, a: TypeName, b: TypeName) -> bool {
        let a = self.0.library_value_form_name(a);
        let b = self.0.library_value_form_name(b);
        platform_type_names_match(a, b)
    }
}

#[cfg(test)]
pub(crate) fn platform_class_identity(internal: &str) -> &str {
    crate::jvm::jvm_class_map::kotlin_builtin_to_jvm(internal).unwrap_or(internal)
}

#[cfg(test)]
pub(crate) fn platform_class_names_match(a: &str, b: &str) -> bool {
    a == b || nested_separator_names_match(a, b)
}

pub(crate) fn platform_type_names_match(a: TypeName, b: TypeName) -> bool {
    a == b
        || crate::jvm::jvm_class_map::type_names_map_to_same_jvm_internal(a, b)
        || a.nested_separator_matches(b)
}

#[cfg(test)]
fn nested_separator_names_match(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let a_nested_start = a.rfind('/').map_or(0, |i| i + 1);
    let b_nested_start = b.rfind('/').map_or(0, |i| i + 1);
    if a_nested_start != b_nested_start || a[..a_nested_start] != b[..b_nested_start] {
        return false;
    }
    a.bytes().zip(b.bytes()).enumerate().all(|(i, (a, b))| {
        a == b || (i >= a_nested_start && matches!((a, b), (b'.', b'$') | (b'$', b'.')))
    })
}

/// The type arguments of a constructed generic type INFERRED from a construction's argument types
/// (`Pair(1, 2)` → `[Int, Int]`, so `Pair(1, 2)` types as `Pair<Int, Int>`). Each of the type's formal
/// parameters (`ty.type_params`) is bound by unifying the matching-arity constructor's parsed generic
/// parameter signatures against `arg_tys`; an unbound formal defaults to `Any`. `None` when the type is
/// non-generic or no constructor carries a generic signature to unify.
pub(crate) fn infer_constructor_type_args(
    ty: &crate::libraries::LibraryType,
    arg_tys: &[Ty],
) -> Option<Vec<Ty>> {
    if ty.type_params.is_empty() {
        return None;
    }
    let mut binds = GSigBinds::new();
    for ctor in &ty.constructors {
        let Some(gsig) = &ctor.generic_sig else {
            continue;
        };
        if gsig.params.len() != arg_tys.len() {
            continue;
        }
        for (p, a) in gsig.params.iter().zip(arg_tys) {
            unify_ty(*p, *a, &mut binds);
        }
        break;
    }
    if binds.is_empty() {
        return None;
    }
    Some(
        ty.type_params
            .iter()
            .map(|f| {
                binds
                    .get(f)
                    .copied()
                    .unwrap_or_else(|| Ty::obj("kotlin/Any"))
            })
            .collect(),
    )
}

/// Bind type variables by unifying a signature `Ty` (whose type variables are [`Ty::TyParam`]) against
/// an actual argument `Ty`.
pub(crate) fn unify_ty(sig: Ty, actual: Ty, binds: &mut GSigBinds) {
    match sig {
        Ty::TyParam(n, _) => {
            binds.entry(n.to_string()).or_insert(actual);
        }
        Ty::Fun(fsig) => {
            // A function parameter (`Function1<T, R>`) unifies against a lambda argument (`Ty::Fun`):
            // the parameter nodes bind the lambda's parameters and the return node binds its return, so
            // `map`'s `R` binds from the lambda body's type (`{ it * 2 }` → `Int`).
            if let Ty::Fun(afsig) = actual {
                // A SUSPEND SAM parameter (`suspend CoroutineScope.() -> T`) erases to
                // `Function2<CoroutineScope, Continuation<T>, Object>` — the RESULT type parameter `T`
                // lives inside the trailing `Continuation<T>`, and the JVM return node is `Object`. The
                // lambda argument, however, ERASES its own `Continuation` type argument (to `Any`) and
                // carries its real result in `afsig.ret`. Binding `T` from the erased `Continuation<Any>`
                // would fix it to `Any` (`runBlocking { … } : Any`, losing the block's type); bind it from
                // `afsig.ret` instead, and skip the `Continuation` param so it isn't double-unified.
                let value_params: &[Ty] = match fsig.params.last() {
                    Some(Ty::Obj(n, cargs))
                        if crate::types::same(*n, crate::types::wk::continuation())
                            && !cargs.is_empty() =>
                    {
                        unify_ty(cargs[0], afsig.ret, binds);
                        &fsig.params[..fsig.params.len() - 1]
                    }
                    _ => &fsig.params,
                };
                for (a, p) in value_params.iter().zip(afsig.params.iter()) {
                    unify_ty(*a, *p, binds);
                }
                if let Ty::Nullable(inner) = fsig.ret {
                    unify_ty(*inner, afsig.ret.non_null(), binds);
                } else {
                    unify_ty(fsig.ret, afsig.ret, binds);
                }
            }
        }
        Ty::Obj(_, args) => {
            // Unify the type arguments positionally against the actual's carried arguments, if any.
            if let Ty::Obj(_, targs) = actual {
                for (a, t) in args.iter().zip(targs.iter()) {
                    unify_ty(*a, *t, binds);
                }
            }
        }
        Ty::Nullable(inner) if matches!(*inner, Ty::Fun(_)) => {
            unify_ty(*inner, actual.non_null(), binds);
        }
        _ => {}
    }
}

pub(crate) fn inference_actual(actual: Ty) -> Ty {
    if actual == Ty::Null {
        Ty::nullable(Ty::Nothing)
    } else {
        actual
    }
}

pub(crate) fn merge_inferred_ty(current: Option<Ty>, actual: Ty) -> Ty {
    let actual = inference_actual(actual);
    let Some(current) = current else {
        return actual;
    };
    if actual == Ty::Nothing {
        current
    } else if current == Ty::Nothing {
        actual
    } else if current == actual {
        current
    } else if matches!(actual, Ty::Nullable(inner) if *inner == Ty::Nothing) {
        Ty::nullable(current)
    } else if matches!(current, Ty::Nullable(inner) if *inner == Ty::Nothing) {
        Ty::nullable(actual)
    } else if current.non_null() == actual.non_null() {
        Ty::nullable(current.non_null())
    } else {
        let any = Ty::obj("kotlin/Any");
        if current.is_nullable() || actual.is_nullable() {
            Ty::nullable(any)
        } else {
            any
        }
    }
}

fn unify_inferred_ty(sig: Ty, actual: Ty, binds: &mut GSigBinds) {
    match sig {
        Ty::TyParam(name, _) => match binds.entry(name.to_string()) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(inference_actual(actual));
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry.insert(merge_inferred_ty(Some(*entry.get()), actual));
            }
        },
        Ty::Fun(signature) => {
            if let Ty::Fun(actual) = actual {
                for (parameter, actual) in signature.params.iter().zip(actual.params.iter()) {
                    unify_inferred_ty(*parameter, *actual, binds);
                }
                unify_inferred_ty(signature.ret, actual.ret, binds);
            }
        }
        Ty::Obj(_, arguments) => {
            if let Ty::Obj(_, actual_arguments) = actual {
                for (argument, actual) in arguments.iter().zip(actual_arguments.iter()) {
                    unify_inferred_ty(*argument, *actual, binds);
                }
            }
        }
        Ty::Nullable(inner) => unify_inferred_ty(*inner, actual.non_null(), binds),
        _ => {}
    }
}

pub(crate) fn infer_generic_bindings(
    generic_sig: &GenericSig,
    actuals: impl IntoIterator<Item = (usize, Ty)>,
) -> GSigBinds {
    let mut binds = GSigBinds::new();
    for (parameter, actual) in actuals {
        if let Some(&shape) = generic_sig.params.get(parameter) {
            unify_inferred_ty(shape, actual, &mut binds);
        }
    }
    binds
}

pub(crate) fn generic_bindings_satisfy_bounds(
    generic_sig: &GenericSig,
    bindings: &GSigBinds,
    mut admits: impl FnMut(Ty, Ty) -> bool,
) -> bool {
    generic_sig
        .formals
        .iter()
        .zip(&generic_sig.formal_bounds)
        .all(|(formal, bounds)| {
            let Some(actual) = bindings.get(formal).copied() else {
                return true;
            };
            bounds
                .iter()
                .all(|bound| admits(actual, ty_subst(*bound, bindings)))
        })
}

/// A JVM method signature may reference owner type parameters without declaring them. Recover those
/// bindings from the provider's receiver-specialized return; method-owned formals still bind from args.
fn seed_undeclared_return_bindings(
    sig: Ty,
    actual: Ty,
    declared_formals: &[String],
    binds: &mut GSigBinds,
) {
    match sig {
        Ty::TyParam(n, _)
            if !declared_formals.iter().any(|formal| formal == n) && actual != Ty::Error =>
        {
            binds.entry(n.to_string()).or_insert(actual);
        }
        Ty::Fun(fsig) => {
            if let Ty::Fun(afsig) = actual {
                for (s, a) in fsig.params.iter().zip(afsig.params.iter()) {
                    seed_undeclared_return_bindings(*s, *a, declared_formals, binds);
                }
                seed_undeclared_return_bindings(fsig.ret, afsig.ret, declared_formals, binds);
            }
        }
        Ty::Nullable(inner) => {
            if let Ty::Nullable(actual_inner) = actual {
                seed_undeclared_return_bindings(*inner, *actual_inner, declared_formals, binds);
            } else {
                seed_undeclared_return_bindings(*inner, actual, declared_formals, binds);
            }
        }
        Ty::Obj(_, args) => {
            if let Ty::Obj(_, actual_args) = actual {
                for (s, a) in args.iter().zip(actual_args.iter()) {
                    seed_undeclared_return_bindings(*s, *a, declared_formals, binds);
                }
            }
        }
        _ => {}
    }
}

fn merge_specialized_return(provider: Ty, inferred: Ty) -> Ty {
    if provider == Ty::Error {
        return inferred;
    }
    if inferred == Ty::Error || inferred.is_erased_top() {
        return provider;
    }
    if provider.is_erased_top() {
        return inferred;
    }
    match (provider, inferred) {
        (Ty::Nullable(provider), Ty::Nullable(inferred)) => {
            Ty::nullable(merge_specialized_return(*provider, *inferred))
        }
        (Ty::Nullable(provider), inferred) => {
            Ty::nullable(merge_specialized_return(*provider, inferred))
        }
        (provider, Ty::Nullable(inferred)) => {
            Ty::nullable(merge_specialized_return(provider, *inferred))
        }
        (Ty::Obj(provider_name, provider_args), Ty::Obj(inferred_name, inferred_args))
            if platform_type_names_match(provider_name, inferred_name) =>
        {
            if provider_args.is_empty() {
                return Ty::obj_args_name(provider_name, inferred_args);
            }
            if inferred_args.is_empty() || provider_args.len() != inferred_args.len() {
                return Ty::obj_args_name(provider_name, provider_args);
            }
            let args = provider_args
                .iter()
                .zip(inferred_args)
                .map(|(&provider, &inferred)| merge_specialized_return(provider, inferred))
                .collect::<Vec<_>>();
            Ty::obj_args_name(provider_name, &args)
        }
        _ => provider,
    }
}

/// Realize a signature `Ty` under the current bindings — a bound type variable substitutes to its
/// binding, an unbound one erases to `Any`; a class substitutes its carried type arguments in place.
pub(crate) fn ty_subst(sig: Ty, binds: &GSigBinds) -> Ty {
    match sig {
        Ty::TyParam(n, _) => binds
            .get(n)
            .copied()
            .unwrap_or_else(|| Ty::obj("kotlin/Any")),
        Ty::Fun(fsig) => {
            let params = ty_subst_all(&fsig.params, binds);
            let ret = ty_subst(fsig.ret, binds);
            Ty::fun_with_shape(
                params,
                ret,
                fsig.context_count,
                fsig.has_receiver,
                fsig.suspend,
            )
        }
        Ty::Nullable(inner) => Ty::nullable(ty_subst(*inner, binds)),
        Ty::Obj(internal, args) if !args.is_empty() => {
            Ty::obj_args_name(internal, &ty_subst_all(args, binds))
        }
        _ => sig,
    }
}

pub(crate) fn ty_subst_all(sigs: &[Ty], binds: &GSigBinds) -> Vec<Ty> {
    sigs.iter().map(|s| ty_subst(*s, binds)).collect()
}

#[derive(Clone, Debug)]
pub(crate) struct ClasspathSamSignature {
    pub params: Vec<Ty>,
    pub ret: Ty,
}

pub(crate) fn classpath_sam_signature(
    lib: &dyn SemanticPlatform,
    target: Ty,
) -> Option<ClasspathSamSignature> {
    let target = target.non_null();
    let internal = target.obj_internal()?;
    let ty = lib.resolve_type_name(internal)?;
    let sam = ty.sam_method.as_ref()?;
    let Some(gsig) = sam.generic_sig.as_ref() else {
        return Some(ClasspathSamSignature {
            params: sam.params.clone(),
            ret: sam.ret,
        });
    };

    let mut binds = GSigBinds::new();
    for (formal, actual) in ty.type_params.iter().zip(target.type_args()) {
        binds.insert(formal.clone(), *actual);
    }
    if let Some(receiver) = gsig.receiver {
        unify_ty(receiver, target, &mut binds);
    }
    Some(ClasspathSamSignature {
        params: ty_subst_all(&gsig.params, &binds),
        ret: ty_subst(gsig.ret, &binds),
    })
}

pub(crate) fn specialized_sam_member_params(
    member: &LibraryMember,
    args: &[CallArgKind],
    type_args: &[Ty],
) -> Vec<Ty> {
    specialized_sam_params(&member.params, member.generic_sig.as_ref(), args, type_args)
}

fn specialized_sam_params(
    params: &[Ty],
    generic_sig: Option<&GenericSig>,
    args: &[CallArgKind],
    type_args: &[Ty],
) -> Vec<Ty> {
    let Some(gsig) = generic_sig.filter(|sig| sig.params.len() == params.len()) else {
        return params.to_vec();
    };
    // Explicit call type arguments bind the formals before argument inference. `CallArgKind`
    // deliberately owns the syntactic lambda/literal provenance, so this generic specialization
    // never has to keep parallel boolean slices aligned with the argument types.
    let mut binds = seeded_gsig_binds(gsig, type_args);
    for (&param, arg) in gsig.params.iter().zip(args) {
        if !arg.is_lambda_literal() {
            unify_inferred_ty(param, arg.ty(), &mut binds);
        }
    }
    let mut specialized = params.to_vec();
    for (index, param) in specialized.iter_mut().enumerate() {
        if args.get(index).is_some_and(|arg| arg.is_lambda_literal()) {
            *param = ty_subst(gsig.params[index], &binds);
        }
    }
    specialized
}

fn seeded_gsig_binds(gsig: &GenericSig, type_args: &[Ty]) -> GSigBinds {
    gsig.formals
        .iter()
        .cloned()
        .zip(type_args.iter().copied())
        .collect()
}

fn bind_gsig_return(
    gsig: &GenericSig,
    type_args: &[Ty],
    actuals: impl IntoIterator<Item = (Ty, Ty)>,
    expected: Option<Ty>,
) -> Ty {
    let mut binds = seeded_gsig_binds(gsig, type_args);
    for (ps, a) in actuals {
        unify_ty(ps, a, &mut binds);
    }
    if let Some(expected) = expected {
        unify_ty(gsig.ret, expected, &mut binds);
    }
    ty_subst(gsig.ret, &binds)
}

fn bind_member_return(gsig: &GenericSig, receiver: Ty, args: &[Ty], provider_ret: Ty) -> Ty {
    let mut binds = GSigBinds::new();
    if let Some(declared_receiver) = gsig.receiver {
        unify_ty(declared_receiver, receiver, &mut binds);
    } else {
        seed_undeclared_return_bindings(gsig.ret, provider_ret, &gsig.formals, &mut binds);
    }
    for (&parameter, &argument) in gsig.params.iter().zip(args) {
        unify_ty(parameter, argument, &mut binds);
    }
    let ret = ty_subst(gsig.ret, &binds);
    merge_specialized_return(provider_ret, ret)
}

fn specialize_property(mut property: PropertyInfo, receiver: Ty) -> PropertyInfo {
    let mut binds = GSigBinds::new();
    if let Some(declared_receiver) = property.receiver {
        unify_ty(declared_receiver, receiver, &mut binds);
    }
    property.ty = ty_subst(property.ty, &binds);
    property.getter.ret = property.ty;
    property
}

fn bind_ext_ret(gsig: &GenericSig, receiver: Ty, args: &[Ty], targs: &[Ty]) -> Ty {
    let mut binds = seeded_gsig_binds(gsig, targs);
    if let Some(recv_sig) = gsig.receiver {
        unify_ty(recv_sig, receiver, &mut binds);
    }
    for (ps, a) in gsig.params.iter().zip(args.iter().copied()) {
        unify_ty(*ps, a, &mut binds);
    }
    ty_subst(gsig.ret, &binds)
}

fn bind_defaulted_ext_ret(
    o: &FunctionInfo,
    receiver: Ty,
    args: &[Ty],
    targs: &[Ty],
    trailing_lambda: bool,
) -> Ty {
    let semantic = o.semantic_signature();
    let mut binds = seeded_gsig_binds(&semantic, targs);
    if let Some(recv_sig) = semantic.receiver {
        unify_ty(recv_sig, receiver, &mut binds);
    }
    if trailing_lambda {
        let prefix = args.len().saturating_sub(1);
        for (ps, a) in semantic.params.iter().take(prefix).zip(args) {
            unify_ty(*ps, *a, &mut binds);
        }
        if let (Some(ls), Some(la)) = (semantic.params.last(), args.last()) {
            unify_ty(*ls, *la, &mut binds);
        }
    } else {
        for (ps, a) in semantic.params.iter().zip(args) {
            unify_ty(*ps, *a, &mut binds);
        }
    }
    ty_subst(semantic.ret, &binds)
}

fn bind_defaulted_ext_ret_slots(
    o: &FunctionInfo,
    receiver: Ty,
    slots: &[Option<Ty>],
    targs: &[Ty],
) -> Ty {
    let semantic = o.semantic_signature();
    let mut binds = seeded_gsig_binds(&semantic, targs);
    if let Some(recv_sig) = semantic.receiver {
        unify_ty(recv_sig, receiver, &mut binds);
    }
    for (ps, slot) in semantic.params.iter().zip(slots) {
        if let Some(arg) = slot {
            unify_ty(*ps, *arg, &mut binds);
        }
    }
    ty_subst(semantic.ret, &binds)
}

/// If `sig` is a function type, the substituted types of its lambda parameters. Empty for anything else.
pub(crate) fn function_input_types(sig: Ty, binds: &GSigBinds) -> Vec<Ty> {
    match sig.non_null() {
        Ty::Fun(fsig) => ty_subst_all(&fsig.params, binds),
        _ => Vec::new(),
    }
}

/// Whether argument `a` can be passed where parameter `p` is expected, in erased Kotlin terms: an
/// exact match, any argument into an erased `Any` parameter, or the *same erased class* (a parameter
/// `Pair` accepts an argument `Pair<Int, String>` — generic parameters erase to the raw type).
pub(crate) fn arg_fits(p: &Ty, a: &Ty) -> bool {
    // A lambda value fits a function-typed parameter when arities agree; its body result is handled by
    // the selected call's generic binding, not by erased descriptor matching. An erased `Any` parameter —
    // whether spelled `kotlin/Any` or its JVM form `java/lang/Object` (a generic vararg element erases to
    // it) — accepts any reference argument.
    p == a
        || matches!(p, Ty::Obj(n, _) if crate::types::same(*n, crate::types::wk::any())
            || crate::types::same(*n, crate::types::wk::java_object()))
        || matches!((p.fun_arity(), a.fun_arity()), (Some(pn), Some(an)) if pn == an)
        || matches!((p, a), (Ty::Obj(pi, _), Ty::Obj(ai, _)) if pi == ai)
}

fn classpath_sam_arg_matches(lib: &dyn SemanticPlatform, param: Ty, arg: Ty) -> bool {
    let Some(sam) = classpath_sam_signature(lib, param) else {
        return false;
    };
    if arg == Ty::Error {
        return sam.params.len() <= 1;
    }
    let Some(arity) = arg.fun_arity() else {
        return false;
    };
    if sam.params.len() != usize::from(arity) {
        return false;
    }
    let Some(arg_ret) = arg.fun_ret() else {
        return false;
    };
    sam.ret == Ty::Unit
        || matches!(arg_ret, Ty::Error | Ty::Nothing)
        || platform_arg_assignable(lib, &sam.ret, &arg_ret)
}

fn arg_fits_platform(lib: &dyn SemanticPlatform, param: &Ty, arg: &Ty) -> bool {
    arg_fits(param, arg)
        || param
            .fun_arity()
            .zip(lib.function_like_arity(*arg))
            .is_some_and(|(p, a)| usize::from(p) == a)
}

fn arg_fits_source(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    param: &Ty,
    arg: &Ty,
) -> bool {
    arg_fits_platform(lib, param, arg)
        || platform_arg_assignable(lib, param, arg)
        || crate::assignable::is_assignable(
            &crate::assignable::TyCtx::new(),
            &SourceOracle(src),
            *arg,
            *param,
        )
}

fn resolution_subtype(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    sub: Ty,
    sup: Ty,
) -> bool {
    crate::assignable::is_subtype(
        &crate::assignable::TyCtx::new(),
        &SourceOracle(src),
        sub,
        sup,
    ) || platform_subtype(lib, sub, sup)
}

pub(crate) enum CandidateSelection<T> {
    None,
    Selected(T),
    Ambiguous,
}

fn unique_most_specific<T>(
    candidates: impl IntoIterator<Item = (Vec<Ty>, T)>,
    at_least_as_specific: impl Fn(usize, Ty, Ty) -> bool,
) -> CandidateSelection<T> {
    unique_most_specific_with_conflicts(candidates, at_least_as_specific, |_, _| false)
}

/// Select the unique most-specific candidate.
fn unique_most_specific_with_conflicts<T>(
    candidates: impl IntoIterator<Item = (Vec<Ty>, T)>,
    at_least_as_specific: impl Fn(usize, Ty, Ty) -> bool,
    equivalent_conflicts: impl Fn(&T, &T) -> bool,
) -> CandidateSelection<T> {
    let mut applicable = Vec::new();
    for (params, candidate) in candidates {
        let equivalent =
            applicable.iter().find(|(existing, _): &&(Vec<Ty>, T)| {
                existing.len() == params.len()
                    && existing.iter().zip(&params).enumerate().all(
                        |(position, (&left, &right))| {
                            at_least_as_specific(position, left, right)
                                && at_least_as_specific(position, right, left)
                        },
                    )
            });
        if let Some((_, existing_candidate)) = equivalent {
            if !equivalent_conflicts(existing_candidate, &candidate) {
                continue;
            }
        }
        applicable.push((params, candidate));
    }
    if applicable.is_empty() {
        return CandidateSelection::None;
    }

    let mut selected = None;
    for (index, (params, _)) in applicable.iter().enumerate() {
        let dominated =
            applicable
                .iter()
                .enumerate()
                .any(|(other_index, (other, _))| {
                    index != other_index
                        && other.len() == params.len()
                        && other.iter().zip(params).enumerate().all(
                            |(position, (&left, &right))| {
                                at_least_as_specific(position, left, right)
                            },
                        )
                        && !params.iter().zip(other).enumerate().all(
                            |(position, (&left, &right))| {
                                at_least_as_specific(position, left, right)
                            },
                        )
                });
        if !dominated && selected.replace(index).is_some() {
            return CandidateSelection::Ambiguous;
        }
    }

    let Some(selected) = selected else {
        return CandidateSelection::Ambiguous;
    };
    CandidateSelection::Selected(applicable.swap_remove(selected).1)
}

fn fixed_parameter_shape(
    params: &[Ty],
    args: &[CallArgKind],
    fits: impl Fn(usize, &Ty, &CallArgKind) -> bool,
) -> Option<Vec<Ty>> {
    (params.len() == args.len()
        && params
            .iter()
            .zip(args)
            .enumerate()
            .all(|(i, (param, arg))| fits(i, param, arg)))
    .then(|| params.to_vec())
}

fn omitted_parameter_shape(
    params: &[Ty],
    args: &[CallArgKind],
    fits: impl Fn(usize, &Ty, &CallArgKind) -> bool,
) -> Option<Vec<Ty>> {
    (params.len() > args.len()
        && params[..args.len()]
            .iter()
            .zip(args)
            .enumerate()
            .all(|(i, (param, arg))| fits(i, param, arg)))
    .then(|| params.to_vec())
}

fn vararg_parameter_shape(
    params: &[Ty],
    args: &[CallArgKind],
    fits: impl Fn(usize, &Ty, &CallArgKind) -> bool,
) -> Option<Vec<Ty>> {
    let vararg_index = params.len().checked_sub(1)?;
    vararg_parameter_shape_at(params, args, vararg_index, &[], fits)
}

/// Expand positional element-form arguments at an explicitly declared vararg slot. Parameters
/// after a non-final vararg cannot consume positional arguments in Kotlin; they must be named or
/// defaulted, so this type-only selector admits the shape only when every trailing parameter has
/// a default. The returned shape is parallel to the provided arguments for specificity ranking.
fn vararg_parameter_shape_at(
    params: &[Ty],
    args: &[CallArgKind],
    vararg_index: usize,
    param_defaults: &[bool],
    fits: impl Fn(usize, &Ty, &CallArgKind) -> bool,
) -> Option<Vec<Ty>> {
    let array = *params.get(vararg_index)?;
    let element = array.array_elem()?;
    if args.len() == vararg_index + 1
        && args.get(vararg_index).map(|argument| argument.ty()) == Some(array)
    {
        return None;
    }
    if args.len() < vararg_index
        || params[..vararg_index]
            .iter()
            .zip(args)
            .enumerate()
            .any(|(position, (parameter, argument))| !fits(position, parameter, argument))
        || (vararg_index + 1..params.len())
            .any(|position| !param_defaults.get(position).copied().unwrap_or(false))
        || args[vararg_index..]
            .iter()
            .enumerate()
            .any(|(offset, argument)| !fits(vararg_index + offset, &element, argument))
    {
        return None;
    }
    let mut expanded = params[..vararg_index].to_vec();
    expanded.resize(args.len(), element);
    Some(expanded)
}

fn integer_literal_call_applies(
    params: &[Ty],
    args: &[CallArgKind],
    mut fits: impl FnMut(usize, &Ty, &CallArgKind) -> bool,
) -> Option<bool> {
    if params.len() != args.len() {
        return None;
    }
    params
        .iter()
        .zip(args)
        .enumerate()
        .try_fold(false, |adapted, (i, (&param, arg))| {
            if param == arg.ty() {
                Some(adapted)
            } else if arg.adapts_integer_literal_to(param) {
                Some(true)
            } else if fits(i, &param, arg) {
                Some(adapted)
            } else {
                None
            }
        })
}

fn parameter_at_least_as_specific(
    lib: &dyn SemanticPlatform,
    left: Ty,
    right: Ty,
    arg: CallArgKind,
) -> bool {
    left == right
        || (left == arg.ty() && arg.adapts_integer_literal_to(right))
        || platform_arg_assignable(lib, &right, &left)
}

fn integer_literal_overload<T>(
    candidates: impl Iterator<Item = (Vec<Ty>, T)>,
    args: &[CallArgKind],
    mut fits: impl FnMut(usize, &Ty, &CallArgKind) -> bool,
    at_least_as_specific: impl Fn(usize, Ty, Ty, CallArgKind) -> bool,
    equivalent_conflicts: impl Fn(&T, &T) -> bool,
) -> CandidateSelection<T> {
    if !args.iter().any(|arg| arg.is_integer_literal()) {
        return CandidateSelection::None;
    }
    let mut applicable = Vec::new();
    let mut has_adaptation = false;
    for (params, candidate) in candidates {
        let Some(adapted) = integer_literal_call_applies(&params, args, &mut fits) else {
            continue;
        };
        has_adaptation |= adapted;
        if let Some((_, existing_candidate)) = applicable
            .iter()
            .find(|(existing, _): &&(Vec<Ty>, T)| existing == &params)
        {
            if !equivalent_conflicts(existing_candidate, &candidate) {
                continue;
            }
        }
        applicable.push((params, candidate));
    }
    if !has_adaptation {
        return CandidateSelection::None;
    }
    unique_most_specific_with_conflicts(
        applicable,
        |position, left, right| {
            at_least_as_specific(
                position,
                left,
                right,
                *args.get(position).unwrap_or(&CallArgKind::Typed(Ty::Error)),
            )
        },
        equivalent_conflicts,
    )
}

fn best_companion_overload<'a>(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    candidates: impl Iterator<Item = &'a LibraryMember> + Clone,
    name: &str,
    args: &[CallArgKind],
    type_args: &[Ty],
) -> Option<&'a LibraryMember> {
    let adapts = |p: &Ty, arg: &CallArgKind, _i: usize| arg.adapts_integer_literal_to(*p);
    let fits = |_position: usize, param: &Ty, arg: &CallArgKind| {
        if arg.is_lambda_literal() {
            if param.fun_arity().is_some() {
                arg_fits_source(lib, src, param, &arg.ty())
            } else {
                classpath_sam_arg_matches(lib, *param, arg.ty())
            }
        } else {
            arg_fits_source(lib, src, param, &arg.ty())
        }
    };
    let logical = |member: &LibraryMember| {
        let params = specialized_sam_member_params(member, args, type_args);
        apply_platform_call_parameter_nullability(
            params,
            &member.call_sig.platform_nullable_params,
            &args.iter().map(|arg| arg.ty()).collect::<Vec<_>>(),
            member.call_sig.vararg,
        )
    };
    let named = candidates.filter(|member| member.name == name);
    // Literal provenance lives beside the type, so exact probes see the ordinary runtime `Int`.
    let arg_tys: Vec<Ty> = args.iter().map(|arg| arg.ty()).collect();
    if let Some(exact) = named.clone().find(|member| logical(member) == arg_tys) {
        return Some(exact);
    }
    match integer_literal_overload(
        named.clone().map(|member| (logical(member), member)),
        args,
        |position, param, arg| fits(position, param, arg),
        |_position, left, right, arg| {
            parameter_at_least_as_specific(lib, left, right, arg)
                || resolution_subtype(lib, src, left, right)
        },
        |_, _| false,
    ) {
        CandidateSelection::Selected(member) => return Some(member),
        CandidateSelection::Ambiguous => return None,
        CandidateSelection::None => {}
    }
    match unique_most_specific(
        named.clone().filter_map(|member| {
            fixed_parameter_shape(&logical(member), args, |position, param, arg| {
                fits(position, param, arg)
            })
            .map(|shape| (shape, member))
        }),
        |_, left, right| resolution_subtype(lib, src, left, right),
    ) {
        CandidateSelection::Selected(member) => return Some(member),
        CandidateSelection::Ambiguous => return None,
        CandidateSelection::None => {}
    }
    match unique_most_specific(
        named.clone().filter_map(|member| {
            let params = logical(member);
            (args.len()..params.len())
                .all(|position| member.call_sig.param_has_default(position))
                .then(|| {
                    omitted_parameter_shape(&params, args, |i, param, arg| {
                        fits(i, param, arg) || adapts(param, arg, i)
                    })
                    .map(|shape| (shape, member))
                })
                .flatten()
        }),
        |_, left, right| resolution_subtype(lib, src, left, right),
    ) {
        CandidateSelection::Selected(member) => return Some(member),
        CandidateSelection::Ambiguous => return None,
        CandidateSelection::None => {}
    }
    match unique_most_specific(
        named.filter_map(|member| {
            let params = logical(member);
            let vararg_index = member.call_sig.vararg_index?;
            vararg_parameter_shape_at(
                &params,
                args,
                vararg_index,
                &member.call_sig.param_defaults,
                |i, param, arg| fits(i, param, arg) || adapts(param, arg, i),
            )
            .map(|shape| (shape, member))
        }),
        |_, left, right| resolution_subtype(lib, src, left, right),
    ) {
        CandidateSelection::Selected(member) => Some(member),
        CandidateSelection::None | CandidateSelection::Ambiguous => None,
    }
}

pub(crate) fn ranked_extension_overloads_by_recv<'a>(
    src: &dyn SymbolSource,
    receiver: Ty,
    fs: &'a FunctionSet,
    allow_must_inline: bool,
    current_source_file: Option<u32>,
) -> Vec<(u32, Ty, &'a FunctionInfo)> {
    ranked_extension_candidates(
        src,
        receiver,
        fs.overloads.iter(),
        allow_must_inline,
        current_source_file,
    )
}

fn ranked_extension_candidates<'a>(
    src: &dyn SymbolSource,
    receiver: Ty,
    overloads: impl Iterator<Item = &'a FunctionInfo>,
    allow_must_inline: bool,
    current_source_file: Option<u32>,
) -> Vec<(u32, Ty, &'a FunctionInfo)> {
    let mro = ReceiverMro::new(src, receiver);
    let mut out: Vec<(u32, Ty, &FunctionInfo)> = overloads
        .filter(|o| {
            o.is_extension()
                && o.receiver_rank != u32::MAX
                && (source_extension_visible_from(o, current_source_file)
                    || (allow_must_inline && o.flags.inline.must_inline()))
        })
        .filter_map(|o| {
            let decl = o.semantic_receiver()?;
            let (rank, binding_receiver) = mro.match_receiver(src, decl)?;
            if matches!(o.receiver, Some(Ty::Obj(n, args)) if n.matches("kotlin/Any") && args.is_empty())
                && !physical_receiver_admits(src, Some(&mro), receiver, &o.callable.descriptor)
            {
                return None;
            }
            Some((rank, binding_receiver, o))
        })
        .collect();
    out.sort_by_key(|(rank, _, _)| *rank);
    out
}

/// Map each provided argument to a logical parameter index. Identity when the counts match; else, for a
/// call that omits leading defaulted parameters before a TRAILING lambda (`runBlocking { … }`), leading
/// args → leading params and the trailing lambda → the LAST parameter.
pub(crate) fn trailing_default_arg_indices(
    param_count: usize,
    arg_tys: &[Option<Ty>],
) -> Option<Vec<usize>> {
    let n = arg_tys.len();
    if param_count == n {
        Some((0..n).collect())
    } else if param_count > n && n >= 1 && arg_tys[n - 1].is_none() {
        let mut map: Vec<usize> = (0..n - 1).collect();
        map.push(param_count - 1);
        Some(map)
    } else {
        None
    }
}

fn is_default_ctor_marker(ty: Ty) -> bool {
    matches!(
        ty,
        Ty::Obj(n, _) if n.matches("kotlin/jvm/internal/DefaultConstructorMarker")
    )
}

fn has_default_tail(
    params: &[Ty],
    prefix_len: usize,
    masked_params: usize,
    marker: impl FnOnce(Ty) -> bool,
) -> bool {
    let mask_count = masked_params.div_ceil(32).max(1);
    params.len() == prefix_len + mask_count + 1
        && params[prefix_len..prefix_len + mask_count]
            .iter()
            .all(|&parameter| parameter == Ty::Int)
        && params.last().copied().is_some_and(marker)
}

fn callable_with_return(c: &LibraryCallable, ret: Ty, default_call: bool) -> LibraryCallable {
    LibraryCallable {
        ret,
        default_call,
        vararg_elem: None,
        vararg_index: None,
        ..c.clone()
    }
}

/// The arg-dependent binding layer over a [`SymbolSource`]: it selects overloads and binds generics for
/// a specific call site. Holds the oracle by reference — cheap to construct per query.
pub struct SymbolResolver<'a> {
    /// Source-level library facts used during resolution.
    lib: &'a dyn SemanticPlatform,
    /// The aggregated resolution source: module declarations shadow library declarations of the same name.
    src: crate::symbol_source::CompositeSource<'a>,
    /// The current compilation module, when present.
    module: Option<&'a dyn SymbolSource>,
    /// The packages in scope for TOP-LEVEL function resolution (same-package, star/explicit imports,
    /// defaults). `None` disables the filter (a context with no import scope — signature inference).
    /// When `Some`, a top-level function resolves only if its facade's package is in scope, matching
    /// kotlinc: an unqualified top-level call binds ONLY to an imported/same-package/default function,
    /// not to any classpath function of that name.
    fn_scope: Option<FunctionScopeRef<'a>>,
    /// Lexically enclosing classes, nearest first.
    lexical_classes: Vec<TypeName>,
    /// Package containing the current source declaration, for Java package-private classifier access.
    access_package: Option<TypeName>,
}

#[derive(Clone, Copy)]
enum FunctionScopeRef<'a> {
    Flat(&'a [TypeName]),
    Imports(&'a FunctionImportScope),
}

impl FunctionScopeRef<'_> {
    fn package_count(self) -> usize {
        match self {
            Self::Flat(packages) => packages.len(),
            Self::Imports(scope) => {
                scope.explicit.len() + scope.levels.iter().map(Vec::len).sum::<usize>()
            }
        }
    }
}

/// The receiver of a reference: a value, an implicit `this`, or a named type.
#[derive(Clone, Copy)]
pub enum SymRecv<'q> {
    Value(Ty),
    ImplicitValue(Ty),
    Type(&'q str),
    TypeName(TypeName),
    /// No receiver — a plain `name(args)` resolved against the import scope's top-level (and same-facade
    /// extension) functions. A DOTTED `name` (`kotlinx.coroutines.runBlocking`) is a fully-qualified
    /// reference: it resolves against its own package, not the import scope.
    TopLevel,
}

/// What a name DENOTES on its receiver — the declared thing the resolver found, NOT how it is used.
/// [`SymbolResolver::resolve_symbol`] resolves a name to one of these; the CALLER then applies whatever
/// its syntax needs (invoke it, read it, write its setter, take a reference), including handling a
/// mismatch itself (`Test()` where `Test` is a property — the caller emits an `invoke`). The resolver
/// does not care whether the site is a call, a read, a write, or a reference.
/// The facets a `recv.name` member supports — see [`Symbol::Member`]. Boxed into the enum so a member
/// symbol stays pointer-sized.
pub struct MemberFacets {
    pub call: Option<ResolvedMember>,
    pub read: Option<ResolvedMember>,
    pub write: Option<LibraryCallable>,
    pub method_ref: Option<LibraryMember>,
    pub property_ref: Option<ResolvedPropertyRef>,
    /// Every overload named `name` applicable to the receiver — instance members, operators, AND in-scope
    /// extension functions with a matching receiver — most-derived/member-first. A caller inspecting the
    /// whole family (named-arg mapping, defaults, return agreement, member-vs-extension dispatch) filters
    /// this by [`FunctionInfo::kind`]/`receiver_rank`.
    pub overloads: Vec<FunctionInfo>,
    /// For a receiver-less [`SymRecv::TopLevel`] name: the single top-level callable selected against
    /// `args`/`type_args` (default/vararg-aware), ready for the emit seam. `None` for a value/type receiver.
    pub top_level_call: Option<LibraryCallable>,
    /// For a value receiver: the classpath EXTENSION callable `recv.name(args)` selected against
    /// `args`/`type_args` (default/vararg-aware; admits `@InlineOnly` splice candidates), ready for the emit
    /// seam. A same-module extension is `None` (it emits through the module path, not a library callable).
    pub extension_call: Option<LibraryCallable>,
    pub extension_property: Option<PropertyInfo>,
}

pub enum Symbol {
    /// A member of a value receiver `recv.name`, with whichever facets the declaration supports. A name
    /// may support several at once — a Java zero-argument method (`list.size`, `str.length`) is both a
    /// property `read` and a `call`/method `reference` — so the resolver reports them all and the caller
    /// takes the one its syntax needs (`recv.name(args)` → `call`, `recv.name` → `read`, `recv.name = v`
    /// → `write`, `recv::name` → `method_ref`/`property_ref`).
    Member(Box<MemberFacets>),
    /// An object/companion instance member `Type.name(args)`.
    Instance(LibraryMember),
    /// A static/companion member `Type.name(args)`.
    Companion(LibraryMember),
    /// A constructor `Type(args)`.
    Constructor(LibraryMember),
    /// A synthesized (value-class / default-argument) constructor.
    SyntheticConstructor(SyntheticCtorCall),
}

impl Symbol {
    pub(crate) fn selected_member(self) -> Option<LibraryMember> {
        match self {
            Symbol::Member(f) => f.call.map(|resolved| resolved.member),
            Symbol::Instance(member) | Symbol::Companion(member) | Symbol::Constructor(member) => {
                Some(member)
            }
            Symbol::SyntheticConstructor(_) => None,
        }
    }

    /// This name invoked as a method with the resolved arguments (`recv.name(args)`).
    pub fn call(self) -> Option<ResolvedMember> {
        match self {
            Symbol::Member(f) => f.call,
            _ => None,
        }
    }
    /// This name read as a property (`recv.name`).
    pub fn property(self) -> Option<ResolvedMember> {
        match self {
            Symbol::Member(f) => f.read,
            _ => None,
        }
    }
    /// The setter of this property (`recv.name = v`).
    pub fn property_setter(self) -> Option<LibraryCallable> {
        match self {
            Symbol::Member(f) => f.write,
            _ => None,
        }
    }
    /// A bound method reference to this name (`recv::name`).
    pub fn method_ref(self) -> Option<LibraryMember> {
        match self {
            Symbol::Member(f) => f.method_ref,
            _ => None,
        }
    }
    /// A bound property reference to this name (`recv::name`).
    pub fn property_ref(self) -> Option<ResolvedPropertyRef> {
        match self {
            Symbol::Member(f) => f.property_ref,
            _ => None,
        }
    }
    /// Every overload named this on the receiver — members, operators, and applicable in-scope extensions.
    pub fn overloads(self) -> Vec<FunctionInfo> {
        match self {
            Symbol::Member(f) => f.overloads,
            _ => Vec::new(),
        }
    }
    /// The selected receiver-less top-level callable ([`SymRecv::TopLevel`]).
    pub fn top_level_call(self) -> Option<LibraryCallable> {
        match self {
            Symbol::Member(f) => f.top_level_call,
            _ => None,
        }
    }
    /// The selected classpath extension callable for `recv.name(args)`.
    pub fn extension_call(self) -> Option<LibraryCallable> {
        match self {
            Symbol::Member(f) => f.extension_call,
            _ => None,
        }
    }
    /// The getter of a classpath extension property `recv.name`.
    pub fn extension_property_getter(self) -> Option<LibraryCallable> {
        match self {
            Symbol::Member(f) => f.extension_property.map(|property| property.getter),
            _ => None,
        }
    }
    pub fn extension_property(self) -> Option<PropertyInfo> {
        match self {
            Symbol::Member(f) => f.extension_property,
            _ => None,
        }
    }
    pub fn extension_property_ref(self) -> Option<ResolvedPropertyRef> {
        match self {
            Symbol::Member(f) => f
                .extension_property
                .and_then(resolve_extension_property_ref),
            _ => None,
        }
    }
    /// The object/companion instance member this resolved to (`Type.name(args)`).
    pub fn instance(self) -> Option<LibraryMember> {
        if let Symbol::Instance(m) = self {
            Some(m)
        } else {
            None
        }
    }
    /// The static/companion member this resolved to (`Type.name(args)`).
    pub fn companion(self) -> Option<LibraryMember> {
        if let Symbol::Companion(m) = self {
            Some(m)
        } else {
            None
        }
    }
    /// The constructor this resolved to (`Type(args)`).
    pub fn constructor(self) -> Option<LibraryMember> {
        if let Symbol::Constructor(m) = self {
            Some(m)
        } else {
            None
        }
    }
    /// The synthesized constructor this resolved to (`Type(args)`).
    pub fn synthetic_constructor(self) -> Option<SyntheticCtorCall> {
        if let Symbol::SyntheticConstructor(s) = self {
            Some(s)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AmbiguousExtensionProperty;

impl<'a> SymbolResolver<'a> {
    pub fn new(lib: &'a dyn SemanticPlatform) -> Self {
        SymbolResolver {
            lib,
            src: crate::symbol_source::CompositeSource::new(vec![lib as &dyn SymbolSource]),
            module: None,
            fn_scope: None,
            lexical_classes: Vec::new(),
            access_package: None,
        }
    }

    /// A resolver whose top-level function resolution is restricted to `fn_scope`'s packages.
    pub fn new_scoped(lib: &'a dyn SemanticPlatform, fn_scope: &'a [TypeName]) -> Self {
        SymbolResolver {
            lib,
            src: crate::symbol_source::CompositeSource::new(vec![lib as &dyn SymbolSource]),
            module: None,
            fn_scope: Some(FunctionScopeRef::Flat(fn_scope)),
            lexical_classes: Vec::new(),
            access_package: None,
        }
    }

    pub(crate) fn new_import_scoped(
        lib: &'a dyn SemanticPlatform,
        fn_scope: &'a FunctionImportScope,
    ) -> Self {
        SymbolResolver {
            lib,
            src: crate::symbol_source::CompositeSource::new(vec![lib as &dyn SymbolSource]),
            module: None,
            fn_scope: Some(FunctionScopeRef::Imports(fn_scope)),
            lexical_classes: Vec::new(),
            access_package: None,
        }
    }

    /// The primary resolver: symbol resolution federates the current `module` over the classpath `lib`.
    pub fn new_scoped_with_module(
        lib: &'a dyn SemanticPlatform,
        module: &'a dyn SymbolSource,
        fn_scope: &'a [TypeName],
    ) -> Self {
        SymbolResolver {
            lib,
            src: crate::symbol_source::CompositeSource::new(vec![module, lib as &dyn SymbolSource]),
            module: Some(module),
            fn_scope: Some(FunctionScopeRef::Flat(fn_scope)),
            lexical_classes: Vec::new(),
            access_package: None,
        }
    }

    pub(crate) fn new_import_scoped_with_module(
        lib: &'a dyn SemanticPlatform,
        module: &'a dyn SymbolSource,
        fn_scope: &'a FunctionImportScope,
    ) -> Self {
        SymbolResolver {
            lib,
            src: crate::symbol_source::CompositeSource::new(vec![module, lib as &dyn SymbolSource]),
            module: Some(module),
            fn_scope: Some(FunctionScopeRef::Imports(fn_scope)),
            lexical_classes: Vec::new(),
            access_package: None,
        }
    }

    pub(crate) fn with_access_context(mut self, package: TypeName, classes: Vec<TypeName>) -> Self {
        self.access_package = Some(package);
        self.lexical_classes = classes;
        self
    }

    fn classifier_accessible(&self, internal: TypeName) -> bool {
        let visibility = self.src.classifier_visibility(internal);
        if visibility == Some(crate::types::Visibility::Public)
            || (visibility.is_none()
                && self
                    .src
                    .resolve_type_name(internal)
                    .is_some_and(|shape| shape.is_public))
        {
            return true;
        }
        if self.access_package.is_some_and(|package| {
            self.src
                .classifier_accessible_from_package(internal, package)
        }) {
            return true;
        }
        // A private nested classifier is a private member of its enclosing classifier, not a
        // file-private top-level declaration. Module lookup therefore needs the lexical owner stack:
        // code in `Outer` (or one of its nested classes) may name `Outer$Hidden`, while another class
        // in the same file may not. The module source handles the separate top-level-private rule.
        if visibility == Some(crate::types::Visibility::Private)
            && self
                .module
                .is_some_and(|module| module.classifier_visibility(internal).is_some())
        {
            let rendered = internal.render();
            if self.lexical_classes.iter().copied().any(|owner| {
                let owner = owner.render();
                rendered == owner
                    || rendered
                        .strip_prefix(&owner)
                        .is_some_and(|suffix| suffix.starts_with('$'))
            }) {
                return true;
            }
        }
        let rendered = internal.render();
        let Some(simple) = rendered.rsplit_once('$').map(|(_, simple)| simple) else {
            return false;
        };
        self.lexical_classes.iter().copied().any(|owner| {
            inherited_nested_classifier_name(
                simple,
                self.src
                    .direct_supertypes(Ty::obj_name(owner))
                    .into_iter()
                    .filter_map(Ty::kotlin_class_internal)
                    .collect(),
                |candidate_owner| {
                    self.src
                        .direct_supertypes(Ty::obj_name(candidate_owner))
                        .into_iter()
                        .filter_map(Ty::kotlin_class_internal)
                        .collect()
                },
                |candidate| {
                    self.src
                        .inherited_classifier_shape(candidate, owner)
                        .is_some()
                },
            ) == InheritedNestedClassifier::Found(internal)
        })
    }

    pub(crate) fn inaccessible_classifier_access(
        &self,
        internal: TypeName,
    ) -> Option<crate::symbol_source::ClassifierAccess> {
        let access = self.src.classifier_access(internal)?;
        (!self.classifier_accessible(internal)).then_some(access)
    }

    /// Whether `internal` names a `@JvmInline value`/inline class — resolved through the FEDERATED source
    /// (the current module over the classpath), so an in-file value class and a classpath one answer alike.
    /// The one authority for value-class-ness; callers ask the resolver, not a `SymbolSource` directly.
    pub fn is_value(&self, internal: &str) -> bool {
        self.src.is_value(internal)
    }

    pub fn is_value_name(&self, internal: TypeName) -> bool {
        self.src.is_value_name(internal)
    }

    /// The single-field UNDERLYING type of the value class named `internal` (`Result` → `Object`), resolved
    /// through the federated source. `None` if not a value class this resolver knows.
    pub fn value_underlying(&self, internal: &str) -> Option<Ty> {
        self.src
            .resolve_type(internal)
            .and_then(|t| t.value_underlying)
    }

    pub fn value_underlying_name(&self, internal: TypeName) -> Option<Ty> {
        self.src
            .resolve_type_name(internal)
            .and_then(|t| t.value_underlying)
    }

    /// Whether the type named `internal` — or anything in its (classpath) supertype chain — declares a
    /// member named `name` (Kotlin/source or physical JVM name). Drives the OVERRIDE test for a class
    /// whose supertype is not in the same file: an override is emitted without `ACC_FINAL` (kotlinc).
    pub fn declares_member(&self, internal: &str, name: &str) -> bool {
        let mut work = vec![crate::types::type_name(internal)];
        let mut seen = std::collections::HashSet::new();
        while let Some(cur) = work.pop() {
            if cur.matches("java/lang/Object") || cur.matches("kotlin/Any") || !seen.insert(cur) {
                continue;
            }
            let Some(t) = self.src.resolve_type_name(cur) else {
                continue;
            };
            if t.members
                .iter()
                .any(|m| m.name == name || m.physical_name.as_deref() == Some(name))
            {
                return true;
            }
            work.extend(t.supertypes.iter_ids());
        }
        false
    }

    /// The unqualified-name resolution loop for this resolver's import scope — `resolve_symbols` per
    /// candidate fqn `pkg/name` over the federated source. THE way to resolve an unqualified name: the
    /// caller extracts `classifier`, `callables.functions` (∪ classifier constructors, then `invoke`), or
    /// `callables.properties` from the records. Empty when there is no import scope (caller falls back).
    fn symbols_in_scope(
        &self,
        name: &str,
    ) -> Vec<(TypeName, std::rc::Rc<crate::libraries::ResolvedSymbols>)> {
        self.fn_scope
            .map(|scope| resolve_symbols_in_function_scope(&self.src, name, scope))
            .unwrap_or_default()
    }

    /// Select the nearest in-scope extension property, rejecting equal-rank candidates.
    pub fn resolve_extension_property(
        &self,
        receiver: Ty,
        name: &str,
    ) -> Result<Option<PropertyInfo>, AmbiguousExtensionProperty> {
        let receiver_mro = ReceiverMro::new(&self.src, receiver);
        let mut candidates = self
            .symbols_in_scope(name)
            .into_iter()
            .flat_map(|(_, symbols)| match &symbols.callables {
                crate::libraries::Callables::Properties(properties) => properties.overloads.clone(),
                crate::libraries::Callables::Both { properties, .. } => {
                    properties.overloads.clone()
                }
                _ => Vec::new(),
            })
            .filter(|property| property.kind == PropKind::Extension)
            .filter(|property| property.context_count == 0)
            .filter_map(|property| {
                let declared = ty_subst(property.receiver?, &std::collections::HashMap::new());
                if receiver.is_nullable() && !declared.is_nullable() {
                    return None;
                }
                receiver_mro
                    .rank(&self.src, declared)
                    .map(|rank| (rank, property))
            })
            .collect::<Vec<_>>();
        let Some(nearest) = candidates.iter().map(|(rank, _)| *rank).min() else {
            return Ok(None);
        };
        candidates.retain(|(rank, _)| *rank == nearest);
        match candidates.as_mut_slice() {
            [(_, property)] => Ok(Some(property.clone())),
            _ => Err(AmbiguousExtensionProperty),
        }
    }

    /// Classify a type name — the ONE type query. `internal` → its [`LibraryType`] (a class/object/
    /// interface shape), or `None` for an unknown name. The type-side counterpart of [`resolve_symbol`].
    pub fn resolve_type(&self, internal: &str) -> Option<crate::libraries::LibraryType> {
        self.src.resolve_type(internal)
    }

    /// Id-backed type query for callers that already carry a [`TypeName`], returning the source's
    /// shared handle.
    pub fn resolve_type_name(
        &self,
        internal: TypeName,
    ) -> Option<std::rc::Rc<crate::libraries::LibraryType>> {
        self.src.resolve_type_name(internal)
    }

    pub fn inheritance_shape_name(
        &self,
        internal: TypeName,
    ) -> Option<crate::symbol_source::InheritanceShape> {
        self.src.inheritance_shape_name(internal)
    }

    pub fn static_field(
        &self,
        internal: TypeName,
        name: &str,
    ) -> Option<crate::libraries::StaticFieldRef> {
        self.lib.static_field_name(internal, name)
    }

    /// The declared type of the member property `name` on `recv` — the property itself, with no accessor
    /// in the answer. A property is a declaration, not a method: whether the target realizes reading it
    /// through a method at all is not a resolution question, so a read must not be made to depend on
    /// finding one. Returns the selected declaration owner and its interface shape beside the logical
    /// property type so lowering does not rediscover either from a source-specific table. Nearest
    /// declaration wins, as for any member.
    pub fn member_property_type(&self, recv: Ty, name: &str) -> Option<(TypeName, Ty, bool)> {
        let receiver_accessible = !recv.is_nullable()
            && recv
                .kotlin_class_internal()
                .is_some_and(|internal| self.classifier_accessible(internal));
        if !receiver_accessible {
            return None;
        }
        let access = MemberAccess {
            source: &self.src,
            module: self.module,
            lexical_classes: &self.lexical_classes,
            receiver: Some(recv),
        };
        self.src
            .property_members(recv, name)
            .overloads
            .into_iter()
            .filter(|property| {
                property.kind == PropKind::Member
                    && property.context_count == 0
                    && member_visible(Some(&access), property.visibility, property.owner)
            })
            .min_by_key(|property| property.receiver_rank)
            .map(|property| {
                let interface = self
                    .src
                    .resolve_type_name(property.owner)
                    .is_some_and(|owner| owner.is_interface());
                (property.owner, property.ty, interface)
            })
    }

    /// Resolve a name on a receiver to the thing it DENOTES — a member, a property, a companion/instance
    /// member, or a constructor — WITHOUT being told how the site uses it. The resolver does not care
    /// whether the caller is going to call it, read it, write it, or take a reference; it just says what
    /// the name is. The caller applies its own syntax to the returned [`Symbol`] (invoke the callable,
    /// read the property, use its setter, take a reference) and handles any mismatch itself (a `Type()`
    /// whose type has no constructor, an `invoke` on a property, …). `args` select a callable overload /
    /// constructor; they do not change WHAT the name is. This and [`resolve_type`] are the resolver's two
    /// resolution entry points.
    pub fn resolve_symbol(
        &self,
        recv: SymRecv,
        name: &str,
        args: &[Ty],
        type_args: &[Ty],
    ) -> Option<Symbol> {
        let args: Vec<CallArgKind> = args.iter().map(|&ty| CallArgKind::Typed(ty)).collect();
        self.resolve_symbol_with_literal_args(recv, name, &args, type_args)
    }

    pub(crate) fn resolve_symbol_with_literal_args(
        &self,
        recv: SymRecv,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
    ) -> Option<Symbol> {
        self.resolve_symbol_with_literal_and_lambda_args(recv, name, args, type_args)
    }

    /// Resolve a top-level call with expected-return-type inference.
    pub(crate) fn resolve_top_level_with_expected(
        &self,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
        expected: Ty,
    ) -> Option<LibraryCallable> {
        let fs = function_set_from_symbols(self.symbols_in_scope(name));
        self.pick_top_level(name, &fs, args, type_args, Some(expected))
    }

    pub(crate) fn top_level_candidates(&self, name: &str) -> Vec<FunctionInfo> {
        function_set_from_symbols(self.symbols_in_scope(name))
            .into_top_level()
            .collect()
    }

    /// Infer expected argument types from the selected extension's generic bounds.
    pub(crate) fn extension_argument_expectations(
        &self,
        receiver: Ty,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
    ) -> Vec<Option<Ty>> {
        let Some(overload) = select_overload(
            self.lib,
            receiver,
            name,
            args,
            type_args,
            FnKind::Extension,
            ExtCtx {
                allow_must_inline: true,
                fn_scope: self.fn_scope,
                current_source_file: None,
                source: &self.src,
                member_access: None,
            },
        ) else {
            return Vec::new();
        };
        let receiver = self.extension_binding_receiver(receiver, &overload);
        let Some(gsig) = overload.generic_sig.as_ref() else {
            return Vec::new();
        };
        let mut binds = seeded_gsig_binds(gsig, type_args);
        if let Some(recv_sig) = gsig.receiver {
            unify_ty(recv_sig, receiver, &mut binds);
        }
        let arg_tys: Vec<Ty> = args.iter().map(|arg| arg.ty()).collect();
        for (&parameter, &argument) in gsig.params.iter().zip(&arg_tys) {
            unify_ty(parameter, argument, &mut binds);
        }
        let expectations: Vec<Option<Ty>> = gsig
            .params
            .iter()
            .zip(&arg_tys)
            .map(|(&parameter, &argument)| {
                let Ty::TyParam(name, _) = parameter else {
                    return None;
                };
                let formal = gsig
                    .formals
                    .iter()
                    .position(|candidate| candidate == name)?;
                gsig.formal_bounds
                    .get(formal)
                    .into_iter()
                    .flatten()
                    .find_map(|&bound| {
                        refine_argument_from_bound(self.lib, argument, ty_subst(bound, &binds))
                    })
            })
            .collect();
        expectations
    }

    pub(crate) fn resolve_symbol_with_literal_and_lambda_args(
        &self,
        recv: SymRecv,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
    ) -> Option<Symbol> {
        let implicit_value = matches!(recv, SymRecv::ImplicitValue(_));
        match recv {
            SymRecv::Value(ty) | SymRecv::ImplicitValue(ty) => {
                // Resolve every facet the name supports on this receiver; a name can support several (a
                // Java zero-arg method is a property read AND a callable). Each facet is exactly the
                // former per-use resolution, so the caller's chosen facet behaves as before.
                let member_receiver_accessible = !ty.is_nullable()
                    && ty
                        .kotlin_class_internal()
                        .is_some_and(|internal| self.classifier_accessible(internal));
                let member_access = MemberAccess {
                    source: &self.src,
                    module: self.module,
                    lexical_classes: &self.lexical_classes,
                    receiver: (!implicit_value).then_some(ty),
                };
                let call = member_receiver_accessible
                    .then(|| {
                        resolve_instance_member(self.lib, ty, name, args, Some(&member_access))
                    })
                    .flatten();
                let read = member_receiver_accessible
                    .then(|| resolve_property_member(self.lib, ty, name, Some(&member_access)))
                    .flatten();
                let write = member_receiver_accessible
                    .then(|| resolve_property_setter(self.lib, ty, name, Some(&member_access)))
                    .flatten();
                let method_ref = member_receiver_accessible
                    .then(|| resolve_instance_ref(self.lib, ty, name, Some(&member_access)))
                    .flatten();
                let property_ref = member_receiver_accessible
                    .then(|| resolve_property_ref(self.lib, ty, name, Some(&member_access)))
                    .flatten();
                // The classpath EXTENSION callable for `recv.name(args)`: one extension selection (admits
                // `@InlineOnly` splice candidates — a plain and an inline call resolve identically, only the
                // emitter differs). A same-module extension emits through the module path, not a library
                // callable, so it is dropped here.
                let arg_tys: Vec<Ty> = args.iter().map(|arg| arg.ty()).collect();
                let extension_call = select_overload(
                    self.lib,
                    ty,
                    name,
                    args,
                    type_args,
                    FnKind::Extension,
                    ExtCtx {
                        allow_must_inline: true,
                        fn_scope: self.fn_scope,
                        current_source_file: None,
                        source: &self.src,
                        member_access: None,
                    },
                )
                .filter(|o| !matches!(o.callable.origin, Origin::Module { .. }))
                .and_then(|o| self.build_extension_callable(name, ty, &arg_tys, type_args, &o));
                let recv_mro = ReceiverMro::new(&self.src, ty);
                let extension_property = self
                    .resolve_extension_property(ty, name)
                    .ok()
                    .flatten()
                    .map(|property| specialize_property(property, ty))
                    .filter(|property| property.getter.ret.is_read_value_result());
                // EVERY overload named `name` applicable to the receiver: instance members and operators
                // (the receiver-aware member query, federated over module + libraries) UNION the in-scope
                // extension functions whose declared receiver is in the receiver's supertype closure. This
                // is the whole candidate family `select_overload` picks from — a caller inspecting the set
                // (named-argument mapping, default-argument selection, return agreement, member-vs-extension
                // dispatch) reads it here and filters by `kind`/`receiver_rank` as it needs.
                let mut overloads = if member_receiver_accessible {
                    self.src
                        .member_overloads(ty, name)
                        .overloads
                        .into_iter()
                        .filter(|overload| {
                            member_access
                                .allows(overload.visibility, overload.callable.owner_type())
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                if self.fn_scope.is_some() {
                    overloads.extend(
                        function_set_from_symbols(self.symbols_in_scope(name))
                            .overloads
                            .into_iter()
                            .filter(|o| {
                                o.is_extension()
                                    && o.semantic_receiver()
                                        .and_then(|dr| recv_mro.rank(&self.src, dr))
                                        .is_some()
                            }),
                    );
                }
                if call.is_none()
                    && read.is_none()
                    && write.is_none()
                    && method_ref.is_none()
                    && property_ref.is_none()
                    && overloads.is_empty()
                    && extension_call.is_none()
                    && extension_property.is_none()
                {
                    return None;
                }
                Some(Symbol::Member(Box::new(MemberFacets {
                    call,
                    read,
                    write,
                    method_ref,
                    property_ref,
                    overloads,
                    top_level_call: None,
                    extension_call,
                    extension_property,
                })))
            }
            SymRecv::TopLevel => {
                // A receiver-less name: its top-level (and same-facade extension) overloads over this
                // resolver's scope. A fully-qualified `pkg.name(args)` resolves by constructing a resolver
                // scoped to `pkg` (the package is scope, not part of the name) and calling this. The caller
                // reads `overloads` to inspect the family, or `top_level_call` for the arg/type-arg selected
                // callable (default/vararg-aware) ready to emit.
                let fs = function_set_from_symbols(self.symbols_in_scope(name));
                let top_level_call = self.pick_top_level(name, &fs, args, type_args, None);
                let overloads = fs.overloads;
                if overloads.is_empty() && top_level_call.is_none() {
                    return None;
                }
                Some(Symbol::Member(Box::new(MemberFacets {
                    call: None,
                    read: None,
                    write: None,
                    method_ref: None,
                    property_ref: None,
                    overloads,
                    top_level_call,
                    extension_call: None,
                    extension_property: None,
                })))
            }
            SymRecv::Type(internal) => self.resolve_symbol_with_literal_and_lambda_args(
                SymRecv::TypeName(crate::types::type_name(internal)),
                name,
                args,
                type_args,
            ),
            SymRecv::TypeName(internal) => {
                if !self.classifier_accessible(internal) {
                    return None;
                }
                let access = MemberAccess {
                    source: &self.src,
                    module: self.module,
                    lexical_classes: &self.lexical_classes,
                    receiver: Some(Ty::obj_name(internal)),
                };
                // Constructor overload probes compare RUNTIME types (see above).
                let arg_tys: Vec<Ty> = args.iter().map(|arg| arg.ty()).collect();
                if name.is_empty() {
                    // `Type(args)` — the type's constructor, real or synthesized.
                    resolve_constructor_name(self.lib, &self.src, internal, &arg_tys)
                        .filter(|member| access.allows(member.visibility, internal))
                        .map(Symbol::Constructor)
                        .or_else(|| {
                            resolve_synthetic_constructor_name(self.lib, internal, &arg_tys)
                                .filter(|constructor| {
                                    access.allows(constructor.visibility, internal)
                                })
                                .map(Symbol::SyntheticConstructor)
                        })
                } else {
                    // `Type.name(args)` — an object/companion instance member, else a static/companion
                    // member. The resolver discovers which.
                    resolve_instance_name(self.lib, internal, name, args, Some(&access))
                        .map(Symbol::Instance)
                        .or_else(|| {
                            resolve_companion_name(
                                self.lib,
                                &self.src,
                                internal,
                                name,
                                args,
                                type_args,
                                Some(&access),
                            )
                            .map(Symbol::Companion)
                        })
                }
            }
        }
    }

    pub(crate) fn resolve_extension_info(
        &self,
        recv: Ty,
        name: &str,
        args: &[CallArgKind],
        type_args: &[Ty],
        current_source_file: Option<u32>,
    ) -> Option<FunctionInfo> {
        select_overload(
            self.lib,
            recv,
            name,
            args,
            type_args,
            FnKind::Extension,
            ExtCtx {
                allow_must_inline: true,
                fn_scope: self.fn_scope,
                current_source_file,
                source: &self.src,
                member_access: None,
            },
        )
    }

    pub(crate) fn resolve_super_instance(
        &self,
        internal: TypeName,
        name: &str,
        args: &[CallArgKind],
    ) -> Option<LibraryMember> {
        let access = MemberAccess {
            source: &self.src,
            module: self.module,
            lexical_classes: &self.lexical_classes,
            receiver: None,
        };
        resolve_instance_name(self.lib, internal, name, args, Some(&access))
    }

    /// Overload-resolve a top-level call against an already-built [`FunctionSet`] (from the resolver's
    /// scope). The [`SymRecv::TopLevel`] arm of [`Self::resolve_symbol`] uses this to fill `top_level_call`.
    fn pick_top_level(
        &self,
        name: &str,
        fs: &FunctionSet,
        args: &[CallArgKind],
        type_args: &[Ty],
        expected: Option<Ty>,
    ) -> Option<LibraryCallable> {
        // Exact/default probes see ordinary runtime types; `args` separately drives adaptation.
        let arg_tys: Vec<Ty> = args.iter().map(|arg| arg.ty()).collect();
        let parsed: Vec<(&FunctionInfo, Vec<Ty>, Ty)> = fs
            .top_level()
            .filter(|o| o.public())
            .map(|o| {
                let params = apply_platform_call_parameter_nullability(
                    o.callable.params.clone(),
                    &o.call_sig.platform_nullable_params,
                    &arg_tys,
                    o.call_sig.vararg,
                );
                (o, params, o.callable.ret)
            })
            .collect();
        let fits = |p: &Ty, a: &CallArgKind| self.arg_fits_or_subtype(p, &a.ty());
        let adapts = |p: &Ty, a: &CallArgKind, _i: usize| a.adapts_integer_literal_to(*p);

        let pick = if let Some(exact) = parsed.iter().find(|(_, params, _)| params == &arg_tys) {
            Some(exact)
        } else {
            let literal_pick = match integer_literal_overload(
                parsed
                    .iter()
                    .map(|entry @ (_, params, _)| (params.clone(), entry)),
                args,
                |_, param, arg| fits(param, arg),
                |_position, left, right, arg| {
                    parameter_at_least_as_specific(self.lib, left, right, arg)
                        || resolution_subtype(self.lib, &self.src, left, right)
                },
                |_, _| false,
            ) {
                CandidateSelection::Selected(entry) => Some(entry),
                CandidateSelection::Ambiguous => return None,
                CandidateSelection::None => None,
            };
            match literal_pick {
                Some(entry) => Some(entry),
                None => match unique_most_specific(
                    parsed.iter().filter_map(|entry @ (_, params, _)| {
                        fixed_parameter_shape(params, args, |_, param, arg| fits(param, arg))
                            .map(|shape| (shape, entry))
                    }),
                    |_, left, right| resolution_subtype(self.lib, &self.src, left, right),
                ) {
                    CandidateSelection::Selected(entry) => Some(entry),
                    CandidateSelection::Ambiguous => return None,
                    CandidateSelection::None => match unique_most_specific(
                        parsed.iter().filter_map(|entry @ (_, params, _)| {
                            vararg_parameter_shape(params, args, |i, param, arg| {
                                fits(param, arg) || adapts(param, arg, i)
                            })
                            .map(|shape| (shape, entry))
                        }),
                        |_, left, right| resolution_subtype(self.lib, &self.src, left, right),
                    ) {
                        CandidateSelection::Selected(entry) => Some(entry),
                        CandidateSelection::Ambiguous => return None,
                        CandidateSelection::None => None,
                    },
                },
            }
        };

        if pick.is_none() {
            if let Some(c) =
                self.resolve_top_level_default_callable(name, &arg_tys, type_args, expected)
            {
                crate::trace_compiler!(
                    "resolve",
                    "top-level {name} args={arg_tys:?} -> {}.{}{} default inline={:?}",
                    c.owner.render(),
                    c.name,
                    c.descriptor,
                    c.inline
                );
                return Some(c);
            }
        }

        if let Some(c) =
            self.resolve_top_level_inline_only_callable(fs, &arg_tys, type_args, expected)
        {
            crate::trace_compiler!(
                "resolve",
                "top-level {name} args={arg_tys:?} -> {}.{}{} inline-only",
                c.owner.render(),
                c.name,
                c.descriptor
            );
            return Some(c);
        }

        let (o, params, ret) = pick?;
        let c = &o.callable;
        if ret
            .obj_internal()
            .is_some_and(|n| n.matches("kotlin/reflect/KType"))
        {
            return None;
        }

        let mut vararg_elem = None;
        let ret_ty = o
            .generic_sig
            .as_ref()
            .map(|gsig| {
                let mut binds = seeded_gsig_binds(gsig, type_args);
                // A vararg call binds `T` from the ELEMENTS, not from the array param. Detect it by the
                // trailing array parameter receiving element-wise args — NOT merely by arity: a SINGLE
                // element (`listOf(pair)`) has `params.len() == args.len()`, yet still spreads into the
                // vararg, so a plain `zip` would unify `Array<T>` against the non-array `Pair` and leave
                // `T` unbound (→ `List<Any>`). A spread (`listOf(*arr)`) passes the array itself — same
                // arity AND the last arg IS the array param — so it is not a vararg here.
                let vararg = params.last().is_some_and(|p| p.array_elem().is_some())
                    && (params.len() != arg_tys.len() || arg_tys.last() != params.last());
                if vararg && !gsig.params.is_empty() {
                    let fixed = gsig.params.len() - 1;
                    for (i, ps) in gsig.params.iter().take(fixed).enumerate() {
                        if let Some(a) = arg_tys.get(i) {
                            if type_args.is_empty() {
                                unify_inferred_ty(*ps, *a, &mut binds);
                            } else {
                                unify_ty(*ps, *a, &mut binds);
                            }
                        }
                    }
                    if let Some(inner) = gsig.params[fixed].array_elem() {
                        for a in &arg_tys[fixed..] {
                            if type_args.is_empty() {
                                unify_inferred_ty(inner, *a, &mut binds);
                            } else {
                                unify_ty(inner, *a, &mut binds);
                            }
                        }
                        vararg_elem = Some(ty_subst(inner, &binds));
                    }
                } else {
                    for (ps, a) in gsig.params.iter().zip(&arg_tys) {
                        if type_args.is_empty() {
                            unify_inferred_ty(*ps, *a, &mut binds);
                        } else {
                            unify_ty(*ps, *a, &mut binds);
                        }
                    }
                }
                if let Some(expected) = expected {
                    unify_ty(gsig.ret, expected, &mut binds);
                }
                ty_subst(gsig.ret, &binds)
            })
            .unwrap_or(*ret);
        let ret_ty = o.ret.apply(if o.flags.suspend { c.ret } else { ret_ty });

        crate::trace_compiler!(
            "resolve",
            "top-level {name} args={arg_tys:?} -> {}.{}{} inline={:?}",
            c.owner.render(),
            c.name,
            c.descriptor,
            c.inline
        );
        Some(LibraryCallable {
            params: params.clone(),
            ret: ret_ty,
            physical_ret: *ret,
            default_call: false,
            vararg_elem,
            vararg_index: vararg_elem.and(o.call_sig.vararg_index),
            ..c.clone()
        })
    }

    /// Shape a selected extension overload into a [`LibraryCallable`] for the call site. An EXACT call binds
    /// the generic return directly. A call that OMITS trailing defaults picks the emit form by a Kotlin ABI
    /// fact — an `inline` function has no `$default` synthetic (kotlinc materializes defaults by inlining),
    /// so it becomes a MUST-INLINE splice; a non-`inline` one binds the `name$default` synthetic (the
    /// backend appends placeholders + a bit-mask).
    fn build_extension_callable(
        &self,
        name: &str,
        receiver: Ty,
        args: &[Ty],
        type_args: &[Ty],
        o: &FunctionInfo,
    ) -> Option<LibraryCallable> {
        let binding_receiver = self.extension_binding_receiver(receiver, o);
        let vparams = logical_value_params(self.lib, o, binding_receiver, type_args);
        // A `vararg` overload SPREAD over the trailing arguments is NOT a defaulted call — the caller
        // builds the packed array and the physical argument list still ends in it. Comparing raw arity
        // reads `"ab..!!".trimEnd('!', '.')` (2 arguments, 1 array parameter) as an omitted-default call
        // and then hunts for a `$default` synthetic that does not exist, so the whole call fell through
        // unresolved. Normalize to the PHYSICAL shape — fixed prefix plus the array — so the arity test
        // below sees what is actually emitted. `f(charArray)` passes the array THROUGH and is untouched.
        let spread = o
            .call_sig
            .vararg_index
            .filter(|&slot| args.len() > slot)
            .and_then(|slot| {
                let array = *vparams.get(slot)?;
                let element = array.array_elem()?;
                // Positional arguments beginning at a non-final vararg all belong to that
                // vararg; later parameters can only be supplied by name. Preserve an array
                // argument only for the already-normalized spread/pass-through shape.
                if args.len() == slot + 1 && args.get(slot) == Some(&array) {
                    return None;
                }
                let mut physical = args[..slot].to_vec();
                physical.push(array);
                Some((physical, slot, element))
            });
        let spread_slot = spread.as_ref().map(|(_, slot, elem)| (*slot, *elem));
        let args: &[Ty] = spread.as_ref().map_or(args, |(a, _, _)| a.as_slice());
        if vparams.len() == args.len() {
            let c = &o.callable;
            let semantic = o.semantic_signature();
            let ret_ty = bind_ext_ret(&semantic, binding_receiver, args, type_args);
            let ret_class = o
                .ret
                .class
                .filter(|meta| self.lib.value_underlying(*meta).is_some());
            let ret_ty2 = o.ret.apply_with_class(ret_class, ret_ty);
            crate::trace_compiler!(
                "resolve",
                "bind_extension_callable {}.{} gsig={} type_args={type_args:?} ret_ty={ret_ty:?} -> {ret_ty2:?}",
                c.owner.render(),
                c.name,
                o.generic_sig.is_some()
            );
            // `vararg_elem` is what tells the LOWERER to build the packed array. It must come from the
            // resolved overload's own `vararg` flag, never from the shape of the parameter list: plenty of
            // non-vararg extensions END in an array parameter (`Array<out T>?.contentEquals(other:
            // Array<out T>?)`), and packing one of those wraps the caller's array in a fresh 1-element
            // array — a silent miscompile the box corpus caught as `collectionLiterals/array.kt`.
            let mut c = callable_with_return(c, ret_ty2, false);
            if let Some((slot, element)) = spread_slot {
                c.vararg_elem = Some(element);
                c.vararg_index = Some(slot);
            }
            return Some(c);
        }
        // Defaulted call — omitted trailing/middle params. Bind the return with default-aware alignment.
        let trailing_lambda = args.last().is_some_and(|a| matches!(a, Ty::Fun(_)));
        let ret_ty = o.ret.apply(bind_defaulted_ext_ret(
            o,
            binding_receiver,
            args,
            type_args,
            trailing_lambda,
        ));
        // Prefer a real `name$default` synthetic when it exists — even for an `inline` function. Many
        // `inline` stdlib/coroutine functions (`Mutex.withLock`) also emit a `$default` callable (the
        // `$$forInline` variant is what kotlinc splices); calling `$default` threads the `Continuation`
        // through the ordinary suspend machinery instead of splicing a suspend body. Splice (MUST-INLINE)
        // only when there is NO `$default` synthetic — a genuine `@InlineOnly` callee with no call target.
        if let Some(c) = self.default_synthetic_callable(name, o, args) {
            crate::trace_compiler!(
                "resolve",
                "extension defaulted ($default) {name} recv={receiver:?} args={args:?} -> {}.{}{} ret={ret_ty:?}",
                c.owner.render(),
                c.name,
                c.descriptor
            );
            let mut c = callable_with_return(&c, ret_ty, true);
            // An element-form vararg call reaching the `$default` (`split('.')`): tell the
            // lowerer which element type to PACK before the mask machinery — without it the
            // loose element lowers straight into the array slot (a VerifyError).
            if let Some((index, elem)) = spread_slot.or_else(|| {
                (!o.flags.suspend)
                    .then_some(o.call_sig.vararg_index)
                    .flatten()
                    .and_then(|index| {
                        vparams
                            .get(index)
                            .and_then(|param| param.array_elem())
                            .map(|element| (index, element))
                    })
            }) {
                // `spread_slot` already normalized the selector's arguments to the physical
                // array shape, so its equality here is expected. The fallback comparison covers
                // older providers that expose the vararg flag without the normalized shape.
                if spread_slot.is_some() || args.get(index).copied() != vparams.get(index).copied()
                {
                    c.vararg_elem = Some(elem);
                    c.vararg_index = Some(index);
                }
            }
            return Some(c);
        }
        if o.flags.inline.can_inline() {
            let mut callable = callable_with_return(&o.callable, ret_ty, true);
            callable.inline = crate::libraries::InlineKind::MustInline;
            crate::trace_compiler!(
                "resolve",
                "extension defaulted (inline) {name} recv={receiver:?} args={args:?} -> {}.{}{} ret={ret_ty:?}",
                callable.owner.render(),
                callable.name,
                callable.descriptor
            );
            return Some(callable);
        }
        None
    }

    fn extension_binding_receiver(&self, receiver: Ty, overload: &FunctionInfo) -> Ty {
        overload
            .semantic_receiver()
            .and_then(|declared| {
                ReceiverMro::new(&self.src, receiver).binding_receiver(&self.src, declared)
            })
            .unwrap_or(receiver)
    }

    pub(crate) fn build_extension_callable_for_slots(
        &self,
        name: &str,
        receiver: Ty,
        type_args: &[Ty],
        o: &FunctionInfo,
        slots: &[Option<Ty>],
    ) -> Option<LibraryCallable> {
        let binding_receiver = self.extension_binding_receiver(receiver, o);
        if !self.extension_slots_admit_bounds(receiver, type_args, o, slots) {
            return None;
        }
        let vparams = logical_value_params(self.lib, o, binding_receiver, type_args);
        if vparams.len() != slots.len() {
            return None;
        }
        for (param, slot) in vparams.iter().zip(slots) {
            if let Some(arg) = slot {
                if !self.arg_fits_or_subtype(param, arg) {
                    return None;
                }
            }
        }
        if slots.iter().all(Option::is_some) {
            let args: Vec<Ty> = slots.iter().map(|slot| slot.unwrap()).collect();
            return self.build_extension_callable(name, receiver, &args, type_args, o);
        }

        let ret_ty = o.ret.apply(bind_defaulted_ext_ret_slots(
            o,
            binding_receiver,
            slots,
            type_args,
        ));
        if let Some(c) = self.default_synthetic_callable_for_slots(name, o, slots) {
            crate::trace_compiler!(
                "resolve",
                "extension defaulted slots ($default) {name} recv={receiver:?} slots={slots:?} -> {}.{}{} ret={ret_ty:?}",
                c.owner.render(),
                c.name,
                c.descriptor
            );
            return Some(callable_with_return(&c, ret_ty, true));
        }
        if o.flags.inline.can_inline() {
            let mut callable = callable_with_return(&o.callable, ret_ty, true);
            callable.inline = crate::libraries::InlineKind::MustInline;
            return Some(callable);
        }
        None
    }

    pub(crate) fn extension_slots_admit_bounds(
        &self,
        receiver: Ty,
        type_args: &[Ty],
        overload: &FunctionInfo,
        slots: &[Option<Ty>],
    ) -> bool {
        generic_bounds_admit_slots(
            self.lib,
            &self.src,
            overload.generic_sig.as_ref(),
            self.extension_binding_receiver(receiver, overload),
            slots,
            type_args,
        )
    }

    /// Find the `name$default` synthetic callable for a defaulted extension call — the emit-shaped callable
    /// (receiver at `params[0]`, all real params present) the backend fills with placeholders.
    fn default_synthetic_callable(
        &self,
        name: &str,
        base: &FunctionInfo,
        args: &[Ty],
    ) -> Option<LibraryCallable> {
        let trailing_lambda = args.last().is_some_and(|a| matches!(a, Ty::Fun(_)));
        // The `name$default` synthetic is a JVM static on a facade, reachable only through the static-method
        // index (NOT `resolve_type`, which reads a class's members not a facade's statics). Surface it via
        // the scope-pruned `top_level_function_set`, which truncates the trailing `(int mask, Object marker)`
        // so the emit shape is `[receiver, real…]`. Matching is by the base overload's leading RECEIVER
        // parameter (`params[0]`), NOT owner: a value-class receiver (`UIntArray`) erases its `$default` to
        // the UNDERLYING array facade (`ArraysKt.copyInto$default([I…)`, receiver `[I`) — the same erased
        // shape the base carries — so the plain-array `$default` binds and the value-class emit pass is not
        // engaged, exactly as the removed receiver-indexed `functions(…, Some(recv))` lookup resolved it.
        // Resolve overloads DIRECTLY from the scope, not through `resolve_symbol(TopLevel)`: this runs
        // inside `pick_top_level`, which the TopLevel arm calls — routing back through `resolve_symbol`
        // would recurse without bound.
        let fs =
            function_set_from_symbols(self.symbols_in_scope(&format!("{name}$default"))).overloads;
        for o in &fs {
            if !o.public() && !o.flags.inline.must_inline() {
                continue;
            }
            let params = &o.callable.params;
            if params.is_empty() {
                continue;
            }
            if base.callable.params.first() != params.first() {
                continue;
            }
            // The `$default` synthetic mirrors its base's physical parameters exactly. When
            // the base overload was already SELECTED with element-form vararg arguments
            // (`split('.')` against `split(vararg delimiters: Char, …)`), pair the synthetic
            // by parameter identity — re-fitting the caller's elements against the ARRAY
            // parameter below would reject it (Char does not fit CharArray).
            if base.call_sig.vararg && !base.flags.suspend && *params == base.callable.params {
                return Some(o.callable.clone());
            }
            let real_count = params.len() - 1;
            let fits = if trailing_lambda {
                let prefix_len = args.len() - 1;
                prefix_len < real_count
                    && matches!(params[real_count], Ty::Fun(_))
                    && params[1..1 + prefix_len]
                        .iter()
                        .zip(&args[..prefix_len])
                        .all(|(p, a)| self.arg_fits_or_subtype(p, a))
            } else {
                args.len() <= real_count
                    && params[1..1 + args.len()]
                        .iter()
                        .zip(args)
                        .all(|(p, a)| self.arg_fits_or_subtype(p, a))
            };
            if fits {
                return Some(o.callable.clone());
            }
        }
        None
    }

    fn default_synthetic_callable_for_slots(
        &self,
        name: &str,
        base: &FunctionInfo,
        slots: &[Option<Ty>],
    ) -> Option<LibraryCallable> {
        let fs =
            function_set_from_symbols(self.symbols_in_scope(&format!("{name}$default"))).overloads;
        for o in &fs {
            if !o.public() && !o.flags.inline.must_inline() {
                continue;
            }
            let params = &o.callable.params;
            if params.is_empty() || base.callable.params.first() != params.first() {
                continue;
            }
            let real = params.len() - 1;
            if real != slots.len() {
                continue;
            }
            if slots.iter().enumerate().all(|(i, slot)| {
                slot.is_none_or(|arg| self.arg_fits_or_subtype(&params[i + 1], &arg))
            }) {
                return Some(o.callable.clone());
            }
        }
        None
    }

    fn arg_fits_or_subtype(&self, param: &Ty, arg: &Ty) -> bool {
        arg_fits_source(self.lib, &self.src, param, arg)
    }

    fn default_arg_mapping(
        &self,
        info: &FunctionInfo,
        params: &[Ty],
        args: &[Ty],
    ) -> Option<Vec<(usize, usize)>> {
        let real_count = params.len();
        let sig = &info.call_sig;
        if args.len() > real_count {
            return None;
        }
        let fits = |p: &Ty, a: &Ty| arg_fits_platform(self.lib, p, a);
        let trailing_lambda = args.last().is_some_and(|a| matches!(a, Ty::Fun(_)));
        if trailing_lambda && args.len() < real_count {
            let last_param = real_count.checked_sub(1)?;
            if !fits(&params[last_param], args.last().unwrap()) {
                return None;
            }
            let prefix_len = args.len() - 1;
            if !params[..prefix_len]
                .iter()
                .zip(&args[..prefix_len])
                .all(|(p, a)| fits(p, a))
            {
                return None;
            }
            if sig.has_known_required_param(prefix_len..last_param) {
                return None;
            }
            let mut mapping: Vec<(usize, usize)> = (0..prefix_len).map(|i| (i, i)).collect();
            mapping.push((last_param, args.len() - 1));
            return Some(mapping);
        }
        if !params[..args.len()]
            .iter()
            .zip(args)
            .all(|(p, a)| fits(p, a))
        {
            return None;
        }
        if sig.has_known_required_param(args.len()..real_count) {
            return None;
        }
        Some((0..args.len()).map(|i| (i, i)).collect())
    }

    fn resolve_top_level_default_callable(
        &self,
        name: &str,
        args: &[Ty],
        type_args: &[Ty],
        expected: Option<Ty>,
    ) -> Option<LibraryCallable> {
        // Direct scope resolution, not `resolve_symbol(TopLevel)`: runs inside `pick_top_level` (see
        // `resolve_top_level_default_callable`) — routing back through `resolve_symbol` would recurse.
        let fsd = function_set_from_symbols(self.symbols_in_scope(&format!("{name}$default")));
        for o in fsd.top_level() {
            let c = &o.callable;
            if !o.public() && !o.flags.inline.must_inline() {
                continue;
            }
            let params = &c.params;
            let Some(mapping) = self.default_arg_mapping(o, params, args) else {
                continue;
            };
            // A `$default` synthetic usually carries NO generic `Signature` (it isn't API), so binding the
            // return type parameter off it fails and the erased `Object` return leaks (`runBlocking { … }`
            // → `Any`, losing the block's result type). Fall back to the BASE function's gsig — its leading
            // real parameters (and their type-parameter positions) align with the `$default`'s, so unifying
            // the provided args against it recovers `T` (`runBlocking<T>(block: () -> T): T` → `T = Ch`).
            let base_gsig = o.generic_sig.clone().or_else(|| {
                // The `$default` (krusty models it with the REAL params, no mask/marker) shares its base
                // function's parameter shape, so a SAME-ARITY base overload's generic signature applies.
                // Among same-arity candidates, prefer one whose return is a bare type PARAMETER (the
                // generic `fun <T> …(): T` form we need to bind), so a same-name/same-arity non-generic
                // sibling doesn't cross-bind.
                let bases: Vec<FunctionInfo> =
                    function_set_from_symbols(self.symbols_in_scope(name))
                        .into_top_level()
                        .filter(|b| {
                            b.generic_sig.is_some() && b.callable.params.len() == params.len()
                        })
                        .collect();
                bases
                    .iter()
                    .find(|b| b.generic_sig.as_ref().is_some_and(|g| g.ret.is_ty_param()))
                    .or_else(|| bases.first())
                    .and_then(|b| b.generic_sig.clone())
            });
            let ret_ty = base_gsig
                .as_ref()
                .map(|gsig| {
                    bind_gsig_return(
                        gsig,
                        type_args,
                        mapping.iter().filter_map(|(param_i, arg_i)| {
                            gsig.params.get(*param_i).map(|ps| (*ps, args[*arg_i]))
                        }),
                        expected,
                    )
                })
                .unwrap_or(c.ret);
            crate::trace_compiler!(
                "resolve",
                "top_level_default {name} base_gsig={} mapping={mapping:?} -> ret={ret_ty:?}",
                base_gsig.is_some()
            );
            let ret_ty = o.ret.apply(ret_ty);
            return Some(callable_with_return(c, ret_ty, true));
        }
        None
    }

    fn resolve_top_level_inline_only_callable(
        &self,
        fs: &FunctionSet,
        args: &[Ty],
        type_args: &[Ty],
        expected: Option<Ty>,
    ) -> Option<LibraryCallable> {
        for o in fs.top_level() {
            let c = &o.callable;
            if !c.inline.must_inline() {
                continue;
            }
            // The JVM descriptor can't express Kotlin nullability: `fun <T : Any>
            // requireNotNull(value: T?)` decodes `params = [Any]`, the parameter nullability living
            // in `platform_nullable_params`. Apply it exactly like the public path in
            // `pick_top_level`, or a `String?` argument is checked against `Any`.
            let params = apply_platform_call_parameter_nullability(
                c.params.clone(),
                &o.call_sig.platform_nullable_params,
                args,
                o.call_sig.vararg,
            );
            if params.len() != args.len()
                || !params
                    .iter()
                    .zip(args)
                    .all(|(p, a)| self.arg_fits_or_subtype(p, a))
            {
                continue;
            }
            let recovered = o
                .generic_sig
                .as_ref()
                .map(|gsig| {
                    // A platform-nullable parameter whose gsig shape is a bare `T` bounded only by
                    // non-nullable bounds (Kotlin `T?` with `T : Any`) binds from the argument's
                    // NON-NULL form: `requireNotNull(String?)` binds `T = String`, so the `T`
                    // return is non-null. Binding verbatim would put `String?` into a `T : Any`
                    // slot and type the return `String?`.
                    // INVARIANT: `args` are in DECLARED parameter order and arity-checked above
                    // (`params.len() != args.len()`), so the positional zip against `gsig.params`
                    // / `platform_nullable_params` is aligned. Holds because named-argument calls
                    // into the classpath are rejected upstream and defaulted calls resolve through
                    // `resolve_top_level_default_callable` instead — if either changes, this zip
                    // must be re-keyed to parameter indices first.
                    let actuals = gsig
                        .params
                        .iter()
                        .copied()
                        .zip(args.iter().copied())
                        .enumerate()
                        .map(|(i, (shape, actual))| {
                            let binds_non_null = o
                                .call_sig
                                .platform_nullable_params
                                .get(i)
                                .copied()
                                .unwrap_or(false)
                                && matches!(shape, Ty::TyParam(name, bound)
                                if !bound.is_nullable()
                                    && gsig.formals.iter().zip(&gsig.formal_bounds).any(
                                        |(f, bounds)| f.as_str() == name
                                            && bounds.iter().all(|b| !b.is_nullable()),
                                    ));
                            (
                                shape,
                                if binds_non_null {
                                    actual.non_null()
                                } else {
                                    actual
                                },
                            )
                        });
                    bind_gsig_return(gsig, type_args, actuals, expected)
                })
                .unwrap_or(c.ret);
            let logical_ret = o.ret.apply(recovered);
            let mut callable = callable_with_return(c, logical_ret, false);
            // Params are CALL-SITE-specific (platform nullability depends on this call's args) —
            // parity with the `pick_top_level` public path, which applies the same adjustment.
            callable.params = params;
            callable.inline = InlineKind::MustInline;
            return Some(callable);
        }
        None
    }
}

// --- Navigation helpers (member/constructor resolution expressed purely against the trait) --------
// The inherited-member walk over a library type's hierarchy — arg-dependent binding, so it lives in
// this layer (not the oracle). `resolve` and `ir_lower` share one implementation, backend-agnostic.

fn abi_form_args(lib: &dyn SemanticPlatform, args: &[Ty]) -> Option<Vec<Ty>> {
    let out: Vec<Ty> = args.iter().map(|a| lib.library_value_form(*a)).collect();
    (out.as_slice() != args).then_some(out)
}

fn params_match_abi_form(lib: &dyn SemanticPlatform, params: &[Ty], args: &[Ty]) -> bool {
    params.len() == args.len()
        && params
            .iter()
            .zip(args)
            .all(|(p, a)| lib.library_value_form(*p) == *a)
}

fn platform_subtype(lib: &dyn SemanticPlatform, sub: Ty, sup: Ty) -> bool {
    crate::assignable::is_subtype(
        &crate::assignable::TyCtx::new(),
        &PlatformOracle(lib),
        sub,
        sup,
    )
}

fn abi_arg_assignable_to_param(lib: &dyn SemanticPlatform, arg: Ty, param: Ty) -> bool {
    let arg = lib.library_value_form(arg);
    let param = lib.library_value_form(param);
    platform_arg_assignable(lib, &param, &arg)
}

fn abi_param_subtype(lib: &dyn SemanticPlatform, sub: Ty, sup: Ty) -> bool {
    platform_subtype(
        lib,
        lib.library_value_form(sub),
        lib.library_value_form(sup),
    )
}

fn value_erased_args(lib: &dyn SemanticPlatform, args: &[Ty]) -> Vec<Ty> {
    args.iter()
        .map(|&a| lib.value_underlying(a).unwrap_or(a))
        .collect()
}

pub(crate) fn apply_platform_parameter_nullability(
    params: Vec<Ty>,
    nullable: &[bool],
    args: &[Ty],
) -> Vec<Ty> {
    apply_platform_call_parameter_nullability(params, nullable, args, false)
}

pub(crate) fn apply_platform_call_parameter_nullability(
    mut params: Vec<Ty>,
    nullable: &[bool],
    args: &[Ty],
    vararg: bool,
) -> Vec<Ty> {
    if vararg {
        let Some((array, fixed)) = params.split_last() else {
            return params;
        };
        let array = *array;
        let fixed_len = fixed.len();
        for ((parameter, accepts_null), argument) in
            params[..fixed_len].iter_mut().zip(nullable).zip(args)
        {
            if *accepts_null
                && (argument.is_nullable() || *argument == Ty::Null)
                && parameter.is_reference()
            {
                *parameter = Ty::nullable(*parameter);
            }
        }
        if nullable.get(fixed_len).copied().unwrap_or(false) {
            if let Some(element) = array.array_elem() {
                if element.is_reference()
                    && args
                        .get(fixed_len..)
                        .unwrap_or_default()
                        .iter()
                        .any(|argument| argument.is_nullable() || *argument == Ty::Null)
                {
                    params[fixed_len] = Ty::array(Ty::nullable(element));
                }
            }
        }
        return params;
    }
    for ((parameter, accepts_null), argument) in params.iter_mut().zip(nullable).zip(args) {
        if *accepts_null
            && (argument.is_nullable() || *argument == Ty::Null)
            && parameter.is_reference()
        {
            *parameter = Ty::nullable(*parameter);
        }
    }
    params
}

fn resolve_vararg_constructor<'a>(
    lib: &dyn SemanticPlatform,
    constructors: &'a [LibraryMember],
    args: &[Ty],
) -> Option<&'a LibraryMember> {
    let call_args: Vec<CallArgKind> = args.iter().map(|&a| CallArgKind::Typed(a)).collect();
    let candidates = constructors.iter().filter_map(|member| {
        if !member.call_sig.vararg {
            return None;
        }
        let params = apply_platform_call_parameter_nullability(
            member.params.clone(),
            &member.call_sig.platform_nullable_params,
            args,
            true,
        );
        vararg_parameter_shape(&params, &call_args, |_, parameter, argument| {
            abi_arg_assignable_to_param(lib, argument.ty(), *parameter)
        })
        .map(|shape| (shape, member))
    });
    match unique_most_specific(candidates, |_, sub, sup| abi_param_subtype(lib, sub, sup)) {
        CandidateSelection::Selected(member) => Some(member),
        CandidateSelection::None | CandidateSelection::Ambiguous => None,
    }
}

/// Resolve a constructor on a library type by argument types (with the type's own widening).
fn resolve_constructor_name(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    internal: TypeName,
    args: &[Ty],
) -> Option<LibraryMember> {
    let Some(t) = lib.resolve_type_name(internal) else {
        crate::trace_compiler!(
            "value_classes",
            "resolve_constructor {internal} resolve_type=None args={args:?}"
        );
        return None;
    };
    resolve_constructor_from_type(lib, src, internal, &t, args)
}

pub(crate) fn resolve_constructor_from_type(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    internal: TypeName,
    t: &crate::libraries::LibraryType,
    args: &[Ty],
) -> Option<LibraryMember> {
    crate::trace_compiler!(
        "value_classes",
        "resolve_constructor {internal} ctors={:?} args={args:?}",
        t.constructors.iter().map(|m| &m.params).collect::<Vec<_>>()
    );
    if let Some(m) = t.constructors.iter().find(|m| {
        apply_platform_parameter_nullability(
            m.params.clone(),
            &m.call_sig.platform_nullable_params,
            args,
        ) == args
    }) {
        return Some(m.clone());
    }
    // A constructor PARAMETER of value-class type erases to its underlying in the JVM `<init>` descriptor
    // (`class Rec(val id: Vid, val n: Int)` → `<init>(Ljava/lang/String;I)V` for `Vid(String)`), but the
    // call passes the value-class type itself (`Rec(Vid("x"), 1)` → arg `Vid`). Retry with each value-class
    // argument erased to its underlying, mirroring the ABI the descriptor-read `ctor` params already carry.
    let erased = value_erased_args(lib, args);
    if erased != args {
        if let Some(m) = t.constructors.iter().find(|m| {
            apply_platform_parameter_nullability(
                m.params.clone(),
                &m.call_sig.platform_nullable_params,
                &erased,
            ) == erased
        }) {
            crate::trace_compiler!(
                "value_classes",
                "resolve_constructor {internal} matched via value-class-erased args {args:?} -> {erased:?}"
            );
            return Some(m.clone());
        }
    }
    // ABI-form matching bridges target collection identity and drops type arguments without
    // hardcoding collection relationships. Exact ABI identity runs before subtype widening so the
    // most-specific overload still wins.
    let abi_args = abi_form_args(lib, args);
    if let Some(abi_args) = &abi_args {
        if let Some(m) = t.constructors.iter().find(|m| {
            let params = apply_platform_parameter_nullability(
                m.params.clone(),
                &m.call_sig.platform_nullable_params,
                abi_args,
            );
            params_match_abi_form(lib, &params, abi_args)
        }) {
            crate::trace_compiler!(
                "value_classes",
                "resolve_constructor {internal} matched via abi-form args {args:?} -> {abi_args:?}"
            );
            return Some(m.clone());
        }
    }
    match unique_most_specific(
        t.constructors.iter().filter_map(|m| {
            let params = apply_platform_parameter_nullability(
                m.params.clone(),
                &m.call_sig.platform_nullable_params,
                args,
            );
            // A module-declared argument class reaches its library supertype only through the
            // SOURCE federation (`class V : Visitor()` into `Holder(Visitor)`), mirroring member
            // overload selection: the platform oracle walks classpath supertypes only.
            (params.len() == args.len()
                && params.iter().zip(args).all(|(p, a)| {
                    abi_arg_assignable_to_param(lib, *a, *p) || source_arg_assignable(src, p, a)
                }))
            .then_some((params, m))
        }),
        |_, left, right| abi_param_subtype(lib, left, right),
    ) {
        CandidateSelection::Selected(m) => {
            crate::trace_compiler!(
                "value_classes",
                "resolve_constructor {internal} matched assignable args {args:?}"
            );
            return Some(m.clone());
        }
        CandidateSelection::Ambiguous => return None,
        CandidateSelection::None => {}
    }
    // Fixed-arity constructors take precedence over expanded varargs.
    for candidate_args in std::iter::once(args).chain((erased != args).then_some(erased.as_slice()))
    {
        if let Some(member) = resolve_vararg_constructor(lib, &t.constructors, candidate_args) {
            return Some(member.clone());
        }
    }
    if let Some(abi_args) = &abi_args {
        if abi_args.as_slice() != args && abi_args.as_slice() != erased.as_slice() {
            if let Some(member) = resolve_vararg_constructor(lib, &t.constructors, abi_args) {
                return Some(member.clone());
            }
        }
    }
    // A classpath `@JvmInline value class` exposes only a PRIVATE `<init>` (its public surface is the
    // static `box-impl`/`constructor-impl`). Construction is `X(u)` over the single underlying value
    // `u`; synthesize that constructor so the call type-checks. The
    // value-classes lowering pass realizes it as the unboxed underlying / `constructor-impl`.
    if let Some(underlying) = t.value_underlying {
        // `X(u)` over the single underlying value — reference (`RoleId(String)`) or scalar
        // (`Count(Int)`); both erase to the underlying through the value-classes pass. (`null` only fits a
        // reference underlying.)
        let fits = matches!(args, [arg]
            if *arg == underlying || (matches!(*arg, Ty::Null) && underlying.is_reference()));
        // A ZERO-arg construction `Id()` when the sole underlying param is DEFAULTED — kotlinc realizes
        // it through the `constructor-impl$default` synthetic (which fills the default itself). Accept it
        // ONLY when that synthetic exists on the classpath, AND the underlying is a REFERENCE: the lowering
        // passes `null` for the dummy underlying slot, which fits only a reference (a scalar would need a
        // typed zero). A mandatory-param value class stays unresolved (no synthetic → no phantom call).
        let all_default = args.is_empty() && underlying.is_reference() && t.value_ctor_has_default;
        crate::trace_compiler!(
            "value_classes",
            "resolve_constructor {internal} value-class underlying={underlying:?} args={args:?} fits={fits} all_default={all_default}"
        );
        if fits {
            // Descriptor is unused on this path (the checker only needs the type; the lowerer lowers the
            // construction itself), so it stays empty — no JVM detail leaks into the resolver.
            return Some(LibraryMember::new(
                "<init>".to_string(),
                vec![underlying],
                Ty::obj_name(internal),
                String::new(),
            ));
        }
        if all_default {
            return Some(LibraryMember::new(
                "<init>".to_string(),
                vec![],
                Ty::obj_name(internal),
                String::new(),
            ));
        }
    }
    None
}

/// A construction routed through kotlinc's SYNTHETIC `<init>` overload carrying a trailing
/// `DefaultConstructorMarker` — two shapes krusty must fill at the call site:
///   * a VALUE-CLASS-typed parameter forces `<init>(<erased-params…>, DefaultConstructorMarker)` (the
///     real `<init>` is private), and the caller passes every arg plus a `null` marker (`mask: None`);
///   * an omitted DEFAULT parameter uses `<init>(<params…>, int masks…, DefaultConstructorMarker)`.
#[derive(Clone, Debug)]
pub struct SyntheticCtorCall {
    /// The synthetic `<init>` descriptor to invoke.
    pub descriptor: String,
    /// The REAL (source) parameter types in descriptor form — a value-class param appears here as its
    /// erased underlying. Provided args coerce to the leading `provided` of these; the rest are omitted.
    pub real_params: Vec<Ty>,
    /// Number of args the caller supplies (a prefix of `real_params`).
    pub provided: usize,
    /// The default bitmask (bit `i` set = param `i` omitted), present only in the default-arg shape.
    pub mask: Option<i32>,
    /// Source visibility of the synthetic constructor.
    pub visibility: crate::types::Visibility,
}

/// The classpath default-value synthetic constructor `<init>(<params…>, int masks…, DefaultConstructorMarker)`
/// for `internal`, as `(descriptor, real_params)` — the (erased) parameter types BEFORE the mask+marker.
/// Matched by `arity` (the source parameter count): the default synthetic has exactly `arity` real params
/// then its mask words and the marker. Matching by arity — not by a public non-marker
/// sibling — is required because a class with a VALUE-CLASS parameter has a PRIVATE primary constructor
/// (absent from the public `constructors`) and ALSO a separate value-class marker overload
/// `<init>(<params…>, marker)` (no masks); only the full default tail is accepted.
pub(crate) fn synthetic_default_ctor_name(
    source: &dyn SymbolSource,
    internal: TypeName,
    arity: usize,
) -> Option<(String, Vec<Ty>, crate::types::Visibility)> {
    let t = source.resolve_type_name(internal)?;
    synthetic_default_ctor_from_type(&t, arity)
}

pub(crate) fn synthetic_default_ctor_from_type(
    t: &crate::libraries::LibraryType,
    arity: usize,
) -> Option<(String, Vec<Ty>, crate::types::Visibility)> {
    let m = t.constructors.iter().find(|m| {
        !m.descriptor.is_empty()
            && has_default_tail(&m.params, arity, arity, is_default_ctor_marker)
    })?;
    Some((
        m.descriptor.clone(),
        m.params[..arity].to_vec(),
        m.visibility,
    ))
}

/// The classpath default-value synthetic for a MEMBER — `name$default(Owner, <params…>, int masks…,
/// Object marker): Ret` (a static, e.g. a data class's `copy$default`) — as `(descriptor, real_params,
/// ret)`, the parameter types being the source method's (WITHOUT the leading receiver and trailing
/// mask/marker). Lets a call omit a defaulted argument. `None` when the class has no such synthetic.
pub(crate) fn synthetic_default_member(
    source: &dyn SymbolSource,
    owner: &str,
    name: &str,
    arity: usize,
) -> Option<(String, Vec<Ty>, Ty, bool)> {
    let t = source.resolve_type(owner)?;
    let dname = format!("{name}$default");
    // Shape `(Owner receiver, <real params…>, int masks…, Object marker)`. Match by `arity` so an overloaded `name$default`
    // of a different parameter count can't be picked.
    if let Some(m) = t.companion.iter().find(|m| {
        m.name == dname && has_default_tail(&m.params, arity + 1, arity, Ty::is_reference)
    }) {
        return Some((
            m.descriptor.clone(),
            m.params[1..arity + 1].to_vec(),
            m.ret,
            false,
        ));
    }
    // A `suspend` method's `$default` carries the `Continuation` as a real trailing parameter of the
    // original method, so its shape is `(Owner, <real params…>, Continuation, int masks…, Object marker)` —
    // one longer, with the `Continuation` BEFORE the mask/marker. The descriptor already spells the
    // continuation in place; the coroutine pass threads the value there (see `append_continuation`).
    let m = t.companion.iter().find(|m| {
        m.name == dname
            && m.params.get(arity + 1).copied().is_some_and(
                |p| matches!(p, Ty::Obj(n, _) if n.matches("kotlin/coroutines/Continuation")),
            )
            && has_default_tail(&m.params, arity + 2, arity, Ty::is_reference)
    })?;
    Some((
        m.descriptor.clone(),
        m.params[1..arity + 1].to_vec(),
        m.ret,
        true,
    ))
}

/// Resolve a classpath construction that a plain [`resolve_constructor`] can't match because it needs a
/// synthetic `DefaultConstructorMarker` overload (a value-class param, or omitted defaults). See
/// [`SyntheticCtorCall`]. `None` when no marker overload fits.
fn resolve_synthetic_constructor_name(
    lib: &dyn SemanticPlatform,
    internal: TypeName,
    args: &[Ty],
) -> Option<SyntheticCtorCall> {
    let t = lib.resolve_type_name(internal)?;
    resolve_synthetic_constructor_from_type(lib, internal, &t, args)
}

pub(crate) fn resolve_synthetic_constructor_from_type(
    lib: &dyn SemanticPlatform,
    internal: TypeName,
    t: &crate::libraries::LibraryType,
    args: &[Ty],
) -> Option<SyntheticCtorCall> {
    let erased = value_erased_args(lib, args);
    for m in &t.constructors {
        if m.descriptor.is_empty()
            || m.params
                .last()
                .copied()
                .is_none_or(|p| !is_default_ctor_marker(p))
        {
            continue;
        }
        let leading = &m.params[..m.params.len() - 1];
        // Tell the default-mask shape (`…, int mask, marker`) from the value-class-param shape (`…, marker`):
        // a mask int is present iff dropping it leaves the params of a SIBLING non-marker ctor (the public
        // primary). Otherwise the trailing int is a real parameter.
        let (real_params, has_mask): (&[Ty], bool) = if leading.last() == Some(&Ty::Int)
            && !leading.is_empty()
            && t.constructors.iter().any(|s| {
                s.params
                    .last()
                    .copied()
                    .is_none_or(|p| !is_default_ctor_marker(p))
                    && s.params == leading[..leading.len() - 1]
            }) {
            (&leading[..leading.len() - 1], true)
        } else {
            (leading, false)
        };
        if erased.len() > real_params.len() {
            continue;
        }
        // No mask ⇒ no defaults ⇒ every parameter must be supplied.
        if !has_mask && erased.len() != real_params.len() {
            continue;
        }
        // Apply the ordinary constructor's ABI assignability after value-class erasure.
        if !erased
            .iter()
            .zip(real_params)
            .all(|(a, p)| *p == Ty::obj("kotlin/Any") || abi_arg_assignable_to_param(lib, *a, *p))
        {
            continue;
        }
        let mask = has_mask.then(|| (erased.len()..real_params.len()).map(|j| 1i32 << j).sum());
        crate::trace_compiler!(
            "value_classes",
            "resolve_synthetic_constructor {internal} desc={} real={real_params:?} provided={} mask={mask:?}",
            m.descriptor,
            erased.len()
        );
        return Some(SyntheticCtorCall {
            descriptor: m.descriptor.clone(),
            real_params: real_params.to_vec(),
            provided: erased.len(),
            mask,
            visibility: m.visibility,
        });
    }
    None
}

/// Resolve a companion member `Type.name(args)` (the receiver type must be public).
fn resolve_companion_name(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    internal: TypeName,
    name: &str,
    args: &[CallArgKind],
    type_args: &[Ty],
    member_access: Option<&MemberAccess<'_>>,
) -> Option<LibraryMember> {
    let t = lib.resolve_type_name(internal)?;
    best_companion_overload(
        lib,
        src,
        t.companion.iter().filter(|member| {
            member_visible(
                member_access,
                member.visibility,
                member.owner.unwrap_or(internal),
            )
        }),
        name,
        args,
        type_args,
    )
    .cloned()
    .map(|mut member| {
        // A generic STATIC's return erases in the descriptor (`<T> T read(Key<T>)` →
        // `Object`); bind it from the arguments exactly as instance members do, so
        // `Fields.read(Fields.PAYLOAD).message()` types as the field's argument.
        if let Some(gsig) = member.generic_sig.as_ref() {
            // Explicit call type arguments (`Maps.create<String, Int> { … }`) seed the
            // formals positionally; argument unification fills the rest.
            let mut binds = seeded_gsig_binds(gsig, type_args);
            for (&parameter, argument) in gsig.params.iter().zip(args) {
                unify_ty(parameter, argument.ty(), &mut binds);
            }
            member.ret = merge_specialized_return(member.ret, ty_subst(gsig.ret, &binds));
        }
        member
    })
}

/// Resolve an instance member `recv.name(args)` — the receiver's static type must be public, but the
/// member may be inherited from a (possibly non-public) supertype. Candidates come from the consolidated
/// `functions` query, whose Member overloads carry the breadth-first `receiver_rank`; the closest rung's
/// best overload wins (most-derived first), exactly the inherited-member walk this used to do by hand.
fn resolve_instance_name(
    lib: &dyn SemanticPlatform,
    internal: TypeName,
    name: &str,
    args: &[CallArgKind],
    member_access: Option<&MemberAccess<'_>>,
) -> Option<LibraryMember> {
    select_instance_info(lib, Ty::obj_name(internal), name, args, member_access).map(|o| {
        let ret = o.ret.apply(o.callable.ret);
        o.member_with_return(ret)
    })
}

/// Resolve a library instance member for a BOUND callable reference (`"KOTLIN"::get`) — where there are
/// no call arguments to drive overload resolution. Returns the UNIQUE fixed-arity overload of `name` on
/// `internal`, or `None` when the member is absent, defaulted/vararg, or ambiguous.
fn resolve_instance_ref(
    lib: &dyn SemanticPlatform,
    recv: Ty,
    name: &str,
    member_access: Option<&MemberAccess<'_>>,
) -> Option<LibraryMember> {
    let mut fixed = lib
        .member_overloads(recv, name)
        .overloads
        .into_iter()
        .filter(|o| member_visible(member_access, o.visibility, o.callable.owner_type()))
        .filter(|o| o.call_sig.requires_all_args(o.callable.params.len()));
    let o = fixed.next()?;
    // Duplicate facts for the same signature are not ambiguous; distinct signatures are.
    if fixed.any(|other| {
        other.callable.params != o.callable.params || other.callable.ret != o.callable.ret
    }) {
        return None;
    }
    // A member inherited from `java/lang/Object` (`toString`/`equals`/`hashCode`) is the one set kotlinc
    // null-guards for a nullable/type-parameter receiver (`null::toString` yields "null"); a direct
    // `invokevirtual` on a captured null would NPE. The erased receiver (an unbounded `T`) reads as a
    // non-null `Any` here, so the receiver-type guard cannot catch it — reject on the resolved OWNER.
    if o.callable.owner_type() == crate::types::wk::java_object() {
        return None;
    }
    let ret = o.ret.apply(o.callable.ret);
    let member = o.member_with_return(ret);
    lib.supports_member_reference(&member).then_some(member)
}

#[derive(Clone, Debug)]
pub struct ResolvedPropertyRef {
    pub getter: LibraryCallable,
    pub setter: Option<LibraryCallable>,
    pub prop_ty: Ty,
    pub extension_facade: Option<TypeName>,
}

fn resolve_extension_property_ref(property: PropertyInfo) -> Option<ResolvedPropertyRef> {
    let getter = property.getter;
    if !matches!(getter.origin, Origin::Library)
        || getter.suspend
        || getter.default_call
        || getter.params.len() != 1
        || getter.name.contains('-')
        || getter.physical_ret != getter.ret
    {
        return None;
    }
    let setter = property
        .setter
        .filter(|setter| setter.params.len() == 2 && setter.physical_ret == Ty::Unit);
    let prop_ty = getter.ret;
    Some(ResolvedPropertyRef {
        extension_facade: Some(getter.owner),
        getter,
        setter,
        prop_ty,
    })
}

/// Resolve a bound property reference on `recv` (`"kotlin"::length`) to its emittable getter descriptor,
/// or `None` when it is not a plainly-emittable read of a property:
/// - `name` must be a PROPERTY, not a zero-arg method (`iterator()::next`) — both otherwise resolve to a
///   readable zero-arg member, so this consults the authoritative property classifier.
/// - a NULLABLE / type-parameter / bare-`Any` receiver may be null and would NPE at `get()`;
/// - the getter must dispatch with a plain `invokevirtual`: a concrete non-interface, non-value-class
///   owner and an unmangled getter (a value-class-typed property's `getX-<hash>` lives on an erased owner).
fn resolve_property_ref(
    lib: &dyn SemanticPlatform,
    recv: Ty,
    name: &str,
    member_access: Option<&MemberAccess<'_>>,
) -> Option<ResolvedPropertyRef> {
    if matches!(recv, Ty::TyParam(..) | Ty::Nullable(..))
        || recv.kotlin_class_internal() == Some(crate::types::wk::any())
    {
        return None;
    }
    if !lib.member_is_property(recv, name) {
        return None;
    }
    let resolved = resolve_property_member(lib, recv, name, member_access)?;
    if resolved.suspend {
        return None;
    }
    let property = lib
        .property_members(recv, name)
        .overloads
        .into_iter()
        .filter(|property| member_visible(member_access, property.visibility, property.owner))
        .min_by_key(|property| property.receiver_rank);
    let (getter, setter) = if let Some(property) = property {
        (
            property.getter,
            property
                .setter
                .filter(|setter| setter.params.len() == 1 && setter.physical_ret == Ty::Unit),
        )
    } else {
        let member = resolved.member.clone();
        let owner = member.owner?;
        (
            LibraryCallable::library(
                owner,
                member.physical_name.unwrap_or(member.name),
                member.params,
                resolved.ret,
                member.physical_ret,
                member.descriptor,
            ),
            resolve_property_setter(lib, recv, name, member_access),
        )
    };
    let direct_member = |callable: &LibraryCallable| {
        let mut member = LibraryMember::new(
            callable.name.clone(),
            callable.params.clone(),
            callable.ret,
            callable.descriptor.clone(),
        );
        member.owner = Some(callable.owner);
        member.physical_ret = callable.physical_ret;
        member
    };
    let owner = getter.owner;
    if !getter.params.is_empty()
        || getter.name.contains('-')
        || lib.is_value_name(owner)
        || lib
            .resolve_type_name(owner)
            .is_some_and(|t| t.is_interface())
        || !lib.supports_member_reference(&direct_member(&getter))
    {
        return None;
    }
    let setter = setter.filter(|callable| lib.supports_member_reference(&direct_member(callable)));
    Some(ResolvedPropertyRef {
        getter,
        setter,
        prop_ty: resolved.ret,
        extension_facade: None,
    })
}

#[derive(Clone, Debug)]
pub struct ResolvedMember {
    pub member: LibraryMember,
    pub ret: Ty,
    pub projected_return_hazard: bool,
    /// The resolved member is a `suspend fun` — the caller (a suspend body) must thread a
    /// `Continuation` into the emitted call and treat the (Object-erased) result as `ret`.
    pub suspend: bool,
}

/// Resolve an instance member and carry the logical return selected for this call. Generic member
/// returns may bind from the receiver (`List<Int>.get(Int): Int`) or, for erased-`Any` returns, from
/// the call arguments (`decodeFromString(serializer, text): T`).
fn resolve_instance_member(
    lib: &dyn SemanticPlatform,
    recv: Ty,
    name: &str,
    args: &[CallArgKind],
    member_access: Option<&MemberAccess<'_>>,
) -> Option<ResolvedMember> {
    let o = select_instance_info(lib, recv, name, args, member_access)?;
    let arg_tys: Vec<Ty> = args.iter().map(|arg| arg.ty()).collect();
    let ret = o
        .generic_sig
        .as_ref()
        .map(|gsig| bind_member_return(gsig, recv, &arg_tys, o.callable.ret))
        .unwrap_or(o.callable.ret);
    let ret = o.ret.apply(ret);
    let member = o.member_with_return(o.callable.ret);
    Some(ResolvedMember {
        ret,
        member,
        projected_return_hazard: o.projected_return_hazard,
        suspend: o.flags.suspend,
    })
}

/// The property's getter resolved by its REAL name from the source's `properties` query — replacing the
/// `getX`/`is`-Boolean/`@JvmName` getter-name GUESSING with the authoritative metadata spelling. The
/// member itself is still built through `resolve_instance_member`, so the full member metadata (return
/// nullability, generic signature) is recovered exactly as before. `None` when no source exposes it as a
/// property, or the resolved getter isn't a read-value member.
fn property_getter_via_query(
    lib: &dyn SemanticPlatform,
    recv: Ty,
    property: &str,
    member_access: Option<&MemberAccess<'_>>,
) -> Option<ResolvedMember> {
    // A value-class-typed property's getter is `@JvmName`-mangled (`getId-<hash>`) and erases its return
    // to the underlying type; resolving it as a plain member would type the read as the underlying, not
    // the value class. Leave those to the value-class fallback, which recovers the logical type.
    let getter = lib
        .property_members(recv, property)
        .overloads
        .into_iter()
        .filter(|property| {
            property.context_count == 0
                && member_visible(member_access, property.visibility, property.owner)
        })
        .min_by_key(|p| p.receiver_rank)
        .map(|p| p.getter.name)
        .filter(|getter| !getter.contains('-'))?;
    resolve_instance_member(lib, recv, &getter, &[], member_access)
        .filter(|m| m.ret.is_read_value_result())
}

/// Resolve a zero-arg property read on `recv`. The `@Metadata` `properties` query supplies the real
/// getter name first (no guessing); then the fallbacks — the semantic Kotlin name (a
/// computed/builtin member), a `getX` physical getter, and a value-class-mangled getter.
fn resolve_property_member(
    lib: &dyn SemanticPlatform,
    recv: Ty,
    property: &str,
    member_access: Option<&MemberAccess<'_>>,
) -> Option<ResolvedMember> {
    property_getter_via_query(lib, recv, property, member_access)
        .or_else(|| resolve_instance_member(lib, recv, property, &[], member_access))
        .filter(|m| m.ret.is_read_value_result())
        .or_else(|| {
            lib.physical_property_getter_names(property)
                .into_iter()
                .find_map(|getter| {
                    resolve_instance_member(lib, recv, &getter, &[], member_access)
                        .filter(|m| m.ret.is_read_value_result())
                })
        })
        .or_else(|| {
            // A property whose declared type is a `@JvmInline value class`: its getter is
            // `@JvmName`-mangled (`getId-<hash>`) and the physical return erases to the underlying, so
            // the plain lookups above miss it. Recover the mangled getter + logical value-class type.
            let internal = recv.kotlin_class_internal()?;
            let member = lib
                .resolve_type_name(internal)?
                .value_class_property(property)
                .filter(|member| {
                    member_visible(
                        member_access,
                        member.visibility,
                        member.owner.unwrap_or(internal),
                    )
                })
                .cloned()?;
            let ret = member.ret;
            Some(ResolvedMember {
                member,
                ret,
                projected_return_hazard: false,
                suspend: false,
            })
        })
}

/// Resolve a `var` property's SETTER by its real `@Metadata` name — the write analogue of
/// [`property_getter_via_query`]. Returns the setter `LibraryCallable` (its `owner`/`descriptor` drive
/// the emitted `setX(v)` call, `params[0]` is the value type the write is checked against). `None` when
/// the property is read-only (`val`, no setter), no source exposes it as a member property, or the
/// setter is value-class `@JvmName`-mangled (`setId-<hash>` — left to the value-class path, which knows
/// the logical type).
fn resolve_property_setter(
    lib: &dyn SemanticPlatform,
    recv: Ty,
    property: &str,
    member_access: Option<&MemberAccess<'_>>,
) -> Option<LibraryCallable> {
    let metadata_properties = lib
        .property_members(recv, property)
        .overloads
        .into_iter()
        .filter(|property| {
            property.context_count == 0
                && member_visible(member_access, property.visibility, property.owner)
        })
        .collect::<Vec<_>>();
    if let Some(p) = metadata_properties.iter().min_by_key(|p| p.receiver_rank) {
        let setter = p.setter.clone()?;
        if setter.name.contains('-') {
            return None;
        }
        // A real setter takes exactly one parameter (the value). Anything else is malformed metadata —
        // treat it as absent so the checker and lowerer agree (both consult `params[0]`) rather than the
        // checker accepting permissively while the lowerer falls back to the inferred value type.
        if setter.params.len() != 1 {
            return None;
        }
        return Some(setter);
    }
    // No `@Metadata` property: a JAVA accessor pair (`isX`/`getX` + `setX(v)`) IS a synthetic
    // property (spec § Java synthetic properties). Kotlin only synthesizes a property when the
    // GETTER exists, so require the read to resolve; then take the single-argument `void` member
    // setter — preferring the overload whose parameter matches the getter's type, and refusing an
    // ambiguous remainder (conservative: kotlinc pairs accessors per matching type).
    let getter = resolve_property_member(lib, recv, property, member_access)?;
    let setter_name = crate::names::property_setter_name(property);
    let mut setters = lib
        .member_overloads(recv, &setter_name)
        .overloads
        .into_iter()
        .filter(|o| o.kind == FnKind::Member)
        .map(|o| o.callable)
        .filter(|c| c.params.len() == 1 && c.ret == Ty::Unit && !c.name.contains('-'))
        .collect::<Vec<_>>();
    if setters.len() > 1 {
        setters.retain(|c| c.params[0] == getter.ret.non_null());
    }
    match setters.as_slice() {
        [_] => setters.pop(),
        _ => None,
    }
}

fn select_instance_info(
    lib: &dyn SemanticPlatform,
    recv: Ty,
    name: &str,
    args: &[CallArgKind],
    member_access: Option<&MemberAccess<'_>>,
) -> Option<FunctionInfo> {
    select_overload(
        lib,
        recv,
        name,
        args,
        &[],
        FnKind::Member,
        ExtCtx {
            allow_must_inline: false,
            fn_scope: None,
            current_source_file: None,
            source: lib,
            member_access,
        },
    )
}

/// The shared unqualified-name resolution LOOP (spec § Resolution): form a candidate FQN `pkg/name` for
/// each in-scope `packages` entry and query [`crate::symbol_source::SymbolSource::resolve_symbols`] once
/// per candidate, returning each `(fqn, record)` whose namespace record is non-empty. The helper does
/// ONLY the loop — it does not decide anything. Because the record keeps the two namespaces SEPARATE
/// (`classifier` vs `callables`), each caller applies its own selection rules organically: a type
/// position reads `classifier` under level-precedence + within-level ambiguity; a call position flattens
/// `callables` and runs overload resolution. The `fqn` is returned so a classifier caller can name the
/// resolved internal (a non-alias classifier's internal name IS its fqn).
/// The rung of `decl_recv` in `recv`'s SOURCE-type supertype closure (0 = same class), or `None` if the
/// extension's declared receiver is neither `recv` nor a supertype of it. Uses `erased_recv` Kotlin-level
/// keys + `resolve_type` supertypes — NO JVM descriptors — so `kotlin/UInt` ≠ `kotlin/Int` ≠ `kotlin/Result`
/// are distinct by their class, a generic value-class receiver (`Result<T>`) binds a concrete one
/// (`Result<String>` — `erased_recv` drops type arguments), and `UInt` never binds an `Int` extension.
/// Replaces the descriptor-based `extension_receiver_rank`, whose value-class special-case existed only
/// because the erased `I`/`Object` descriptors tied distinct value classes together.
/// Whether the declared receiver's type arguments are consistent with the actual receiver's, position by
/// position, under Kotlin's COVARIANT reading of a receiver position: each actual argument must be
/// assignable to the declared one (`ReceiverMro::rank` reaching from actual to declared). A declared
/// argument that is a type variable or `Any`/`Object` is a wildcard (an `Iterable<T>` / erased
/// `Iterable<Any>` extension binds any element). This rejects the `@JvmName` reduction variant whose
/// element does not match (`Iterable<Byte>.averageOfByte` against a `List<Double>` — `Double` is not
/// assignable to `Byte`) while accepting a nested-generic supertype (`Iterable<Iterable<T>>.flatten`
/// against `List<List<Int>>` — `List<Int>` IS assignable to `Iterable<Any>`). The erased supertype walk
/// in `ReceiverMro` alone keys on the outer class only, so it would tie the reduction variants.
fn receiver_type_args_match(src: &dyn SymbolSource, decl_recv: Ty, recv: Ty) -> bool {
    // Each actual argument must be assignable to the declared one under Kotlin's covariant receiver
    // reading. A declared argument that is a type variable or erased `Any` is a WILDCARD — the metadata
    // decode drops the nullability flag, so a `T?` receiver element reads as bare `Any`, and a nullable
    // actual (`Int?`) must still match it (`is_assignable(Int?, Any)` is correctly `false` under strict
    // Kotlin, but here `Any` stands for the erased variable, not the type `Any`).
    let cx = crate::assignable::TyCtx::new();
    let oracle = SourceOracle(src);
    let wildcard = |t: Ty| {
        t.is_ty_param()
            || matches!(t.non_null(), Ty::Obj(n, _)
                if crate::types::same(n, crate::types::wk::any())
                    || crate::types::same(n, crate::types::wk::java_object()))
    };
    decl_recv
        .type_args()
        .iter()
        .zip(recv.type_args().iter())
        .all(|(&d, &r)| {
            wildcard(d) || wildcard(r) || crate::assignable::is_assignable(&cx, &oracle, r, d)
        })
}

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
                    next.extend(src.direct_supertypes(ty));
                }
                frontier = next;
                rung += 1;
            }
        }
        ReceiverMro { recv, ranks }
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

    fn match_receiver(&self, src: &dyn SymbolSource, decl_recv: Ty) -> Option<(u32, Ty)> {
        // Type variables accept null through a nullable upper bound.
        let accepts_nullable = decl_recv.is_nullable()
            || matches!(decl_recv, Ty::TyParam(_, bound) if bound.is_nullable());
        if self.recv.is_nullable() && !accepts_nullable {
            return None;
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
                receiver_type_args_match(src, decl_recv, binding_receiver)
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
        want.is_some_and(|n| n.matches("kotlin/Any"))
            .then_some((u32::MAX - 1, self.recv))
    }

    pub(crate) fn rank(&self, src: &dyn SymbolSource, decl_recv: Ty) -> Option<u32> {
        self.match_receiver(src, decl_recv).map(|(rank, _)| rank)
    }

    fn binding_receiver(&self, src: &dyn SymbolSource, decl_recv: Ty) -> Option<Ty> {
        self.match_receiver(src, decl_recv)
            .map(|(_, applied)| applied)
    }
}

pub(crate) fn resolve_symbols_in_scope(
    src: &dyn SymbolSource,
    name: &str,
    packages: &[TypeName],
) -> Vec<(TypeName, std::rc::Rc<crate::libraries::ResolvedSymbols>)> {
    let lib = src;
    packages
        .iter()
        .filter_map(|pkg| {
            let fqn = crate::types::type_name_child(*pkg, name);
            let r = lib.resolve_symbols_name(fqn);
            (!r.is_empty()).then_some((fqn, r))
        })
        .collect()
}

fn has_callables(record: &crate::libraries::ResolvedSymbols) -> bool {
    !matches!(record.callables, crate::libraries::Callables::None)
}

fn resolve_symbols_in_function_scope(
    src: &dyn SymbolSource,
    name: &str,
    scope: FunctionScopeRef<'_>,
) -> Vec<(TypeName, std::rc::Rc<crate::libraries::ResolvedSymbols>)> {
    match scope {
        FunctionScopeRef::Flat(packages) => resolve_symbols_in_scope(src, name, packages),
        FunctionScopeRef::Imports(imports) => {
            if let Some((package, declared_name)) = imports.explicit_target(name) {
                let records =
                    resolve_symbols_in_scope(src, &declared_name, std::slice::from_ref(&package));
                crate::trace_compiler!(
                    "resolve",
                    "import scope {name}: explicit target={}.{} records={}",
                    package.render(),
                    declared_name,
                    records.len()
                );
                return records;
            }
            for level in imports.levels() {
                let records = resolve_symbols_in_scope(src, name, level)
                    .into_iter()
                    .filter(|(_, record)| has_callables(record))
                    .collect::<Vec<_>>();
                if !records.is_empty() {
                    crate::trace_compiler!(
                        "resolve",
                        "import scope {name}: level packages={} records={}",
                        level.len(),
                        records.len()
                    );
                    return records;
                }
            }
            Vec::new()
        }
    }
}

fn function_set_from_symbols(
    symbols: impl IntoIterator<Item = (TypeName, std::rc::Rc<crate::libraries::ResolvedSymbols>)>,
) -> FunctionSet {
    FunctionSet {
        overloads: symbols
            .into_iter()
            .flat_map(|(_, r)| match &r.callables {
                crate::libraries::Callables::Functions(f) => f.overloads.clone(),
                crate::libraries::Callables::Both { functions, .. } => functions.overloads.clone(),
                _ => Vec::new(),
            })
            .collect(),
    }
}

/// Whether callable overload `o` is visible for an UNQUALIFIED (top-level or extension) call given the
/// in-scope packages `fn_scope`. A same-module callable ([`Origin::Module`]) is always visible — module
/// visibility is resolved separately, and its facade owner may be package-less. Only a CLASSPATH
/// ([`Origin::Library`]) callable must have its facade's package imported (same-package / star / explicit
/// / default), matching kotlinc. `None` scope keeps everything (a context with no import scope).
fn fn_in_scope(o: &FunctionInfo, fn_scope: Option<FunctionScopeRef<'_>>) -> bool {
    if !matches!(o.callable.origin, Origin::Library) {
        return true;
    }
    match fn_scope {
        None => true,
        Some(FunctionScopeRef::Flat(scope)) => scope
            .iter()
            .any(|&p| o.callable.owner_package_matches_name(p)),
        Some(FunctionScopeRef::Imports(_)) => true,
    }
}

/// Extension-selection context for [`select_overload`]: whether non-public `@InlineOnly` candidates are
/// admitted (the bytecode inliner), and the packages in scope for an extension (`None` = unscoped). Both
/// only affect EXTENSION selection — a member is always visible on its type.
#[derive(Clone, Copy)]
struct ExtCtx<'a, 'm> {
    allow_must_inline: bool,
    fn_scope: Option<FunctionScopeRef<'a>>,
    current_source_file: Option<u32>,
    source: &'a dyn SymbolSource,
    member_access: Option<&'m MemberAccess<'a>>,
}

fn source_extension_visible_from(o: &FunctionInfo, current_source_file: Option<u32>) -> bool {
    if o.visibility == Visibility::Public {
        return true;
    }
    if !matches!(o.callable.origin, Origin::Module { .. }) {
        return false;
    }
    match o.visibility {
        Visibility::Internal => true,
        Visibility::Private => o
            .source_key
            .zip(current_source_file)
            .is_some_and(|((declaring, _), current)| declaring == current),
        Visibility::Protected | Visibility::Public => false,
    }
}

struct MemberAccess<'a> {
    source: &'a dyn SymbolSource,
    module: Option<&'a dyn SymbolSource>,
    lexical_classes: &'a [TypeName],
    receiver: Option<Ty>,
}

impl MemberAccess<'_> {
    fn allows(&self, visibility: crate::types::Visibility, owner: TypeName) -> bool {
        use crate::types::Visibility;
        match visibility {
            Visibility::Public => true,
            Visibility::Internal => self
                .module
                .is_some_and(|module| module.classifier_visibility(owner).is_some()),
            Visibility::Private => self.lexical_classes.contains(&owner),
            Visibility::Protected => self.lexical_classes.iter().copied().any(|enclosing| {
                if enclosing == owner {
                    return true;
                }
                let caller_is_subclass = ReceiverMro::new(self.source, Ty::obj_name(enclosing))
                    .rank(self.source, Ty::obj_name(owner))
                    .is_some();
                let receiver_is_caller = self.receiver.is_none_or(|receiver| {
                    ReceiverMro::new(self.source, receiver)
                        .rank(self.source, Ty::obj_name(enclosing))
                        .is_some()
                });
                caller_is_subclass && receiver_is_caller
            }),
        }
    }
}

fn member_visible(
    access: Option<&MemberAccess<'_>>,
    visibility: crate::types::Visibility,
    owner: TypeName,
) -> bool {
    access.map_or(visibility == crate::types::Visibility::Public, |access| {
        access.allows(visibility, owner)
    })
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum CallArgKind {
    /// A fully inferred non-lambda expression type.
    Typed(Ty),
    /// A lambda literal whose `Ty::Fun` may have unknown parameter/return types (`Error`).
    /// The resolver may infer those unknowns from the candidate overload.
    LambdaLiteral(Ty),
    /// A safely folded integer constant and its ordinary runtime type. Expressions that overflow
    /// `Int` or divide by zero never receive this variant: lowering evaluates their `Int` operations
    /// before a call-boundary coercion, so treating them as adaptable would change their behavior.
    IntegerLiteral { ty: Ty, value: i32 },
}

impl CallArgKind {
    pub(crate) fn integer_literal(ty: Ty, value: i32) -> Self {
        Self::IntegerLiteral { ty, value }
    }

    pub(crate) fn ty(self) -> Ty {
        match self {
            CallArgKind::Typed(ty)
            | CallArgKind::LambdaLiteral(ty)
            | CallArgKind::IntegerLiteral { ty, .. } => ty,
        }
    }

    pub(crate) fn is_lambda_literal(self) -> bool {
        matches!(self, CallArgKind::LambdaLiteral(_))
    }

    pub(crate) fn is_integer_literal(self) -> bool {
        matches!(self, CallArgKind::IntegerLiteral { .. })
    }

    /// Whether this literal may be contextually typed as `parameter`.
    ///
    /// Keeping the rule on the argument object makes positional, named, module, classpath, and
    /// extension resolution share one adaptation policy instead of maintaining origin-specific
    /// boolean arrays. A runtime `Int` is exact; contextual adaptation additionally admits `Long`,
    /// and admits `Byte`/`Short` only when the folded value is known to fit.
    pub(crate) fn adapts_integer_literal_to(self, parameter: Ty) -> bool {
        let CallArgKind::IntegerLiteral { ty: Ty::Int, value } = self else {
            return false;
        };
        match parameter {
            Ty::Byte => i8::try_from(value).is_ok(),
            Ty::Short => i16::try_from(value).is_ok(),
            Ty::Long => true,
            _ => false,
        }
    }
}

/// The single call-overload selector for a receiver call `recv.name(args)`. It is parameterized by
/// [`FnKind`] — MEMBER and EXTENSION resolution differ only in the *calling convention* the backend emits
/// (invokevirtual with `this` vs invokestatic with the receiver as the leading arg), NOT in how the best
/// overload is chosen. The receiver is always an ATTRIBUTE, never `params[0]`: candidates are matched
/// against their LOGICAL value parameters (a member's `callable.params` are value-only; an extension's
/// prepend the receiver in the JVM emit shape, so [`logical_value_params`] strips it). Overloads are tried
/// closest-receiver-rank first, and within a rank by the ordered applicability passes below.
/// Whether an extension's PHYSICAL receiver (the JVM descriptor's first parameter) can hold the
/// actual receiver — the discriminator of last resort for TyParam receivers erased to `Any` (see the
/// call site). `Ljava/lang/Object;` admits everything; an array parameter admits only an array
/// receiver; a reference parameter admits a receiver whose supertype closure reaches it (mapped back
/// through the JVM↔Kotlin builtin/collection tables). Unparseable descriptors admit (no evidence).
fn physical_receiver_admits(
    src: &dyn SymbolSource,
    mro: Option<&ReceiverMro>,
    recv: Ty,
    descriptor: &str,
) -> bool {
    let Some(rest) = descriptor.strip_prefix('(') else {
        return true;
    };
    let recv_is_array = recv.non_null().array_elem().is_some()
        || recv
            .non_null()
            .obj_internal()
            .is_some_and(|n| n.render().ends_with("Array"));
    match rest.as_bytes().first() {
        Some(b'[') => recv_is_array,
        Some(b'L') => {
            let Some(end) = rest.find(';') else {
                return true;
            };
            let internal = &rest[1..end];
            if internal == "java/lang/Object" {
                return true;
            }
            if recv_is_array {
                return false;
            }
            let kotlin = crate::jvm::jvm_class_map::jvm_collection_to_kotlin(internal)
                .or_else(|| crate::jvm::jvm_class_map::jvm_to_kotlin_builtin_with_members(internal))
                .unwrap_or(internal);
            mro.is_none_or(|m| {
                m.rank(src, Ty::obj(kotlin)).is_some() || m.rank(src, Ty::obj(internal)).is_some()
            })
        }
        _ => true,
    }
}

fn select_overload(
    lib: &dyn SemanticPlatform,
    recv: Ty,
    name: &str,
    args: &[CallArgKind],
    type_args: &[Ty],
    kind: FnKind,
    ext: ExtCtx<'_, '_>,
) -> Option<FunctionInfo> {
    let src = ext.source;
    // Argument ASSIGNABILITY must see MODULE-declared classes (`class V : Thread()`, or an
    // anonymous object over a declaration-only Kotlin base, passed to `take(Thread)`): the
    // caller's member-access record carries the module-first source federation. Candidate
    // ENUMERATION stays on `ext.source` — widening it would surface module members through the
    // library path and skip module-side checks (e.g. the `operator` modifier requirement).
    let assign_src: &dyn SymbolSource = ext.member_access.map_or(src, |access| access.source);
    let arg_tys: Vec<Ty> = args.iter().map(|arg| arg.ty()).collect();
    let allow_must_inline = ext.allow_must_inline;
    // EXTENSION candidates come from the ONE query — union `resolve_symbols`' function callables over the
    // in-scope packages (scope-pruned, tree-driven), so an unqualified extension binds only when its
    // facade's package is imported. No import scope → the whole-classpath `functions()` fallback
    // (removed once every consumer is scoped — task A). MEMBERS are always visible on their type.
    // A MEMBER's return can be RECEIVER-COUPLED (`Repo<Cfg>.byId(): Cfg`, a suspend `Continuation<T>`
    // bound from the receiver's type argument) — recovery the receiver-agnostic `resolve_type` cannot
    // do — so member candidates come from the platform's receiver-aware member query. EXTENSIONS come
    // from the scope-pruned `resolve_symbols` seam (empty when there is no import scope). Extension
    // candidates are BORROWED from the `Rc`-shared namespace records (kept alive in `ext_records`) —
    // deep-cloning every overload's `FunctionInfo` (params, call-sig vecs, generic sig) per call site
    // only to discard all but the winner dominated selection; only the selected overload is cloned.
    let member_set = match kind {
        FnKind::Member => src.member_overloads(recv, name),
        _ => FunctionSet::default(),
    };
    let ext_records = match (kind, ext.fn_scope) {
        (FnKind::Extension, Some(scope)) => resolve_symbols_in_function_scope(src, name, scope),
        _ => Vec::new(),
    };
    let overloads: Vec<&FunctionInfo> = match kind {
        FnKind::Member => member_set.overloads.iter().collect(),
        FnKind::Extension => ext_records
            .iter()
            .flat_map(|(_, r)| {
                let fns: &[FunctionInfo] = match &r.callables {
                    crate::libraries::Callables::Functions(f) => &f.overloads,
                    crate::libraries::Callables::Both { functions, .. } => &functions.overloads,
                    _ => &[],
                };
                fns.iter()
            })
            .collect(),
        FnKind::TopLevel => Vec::new(),
    };
    // Candidates from the scoped query are IN-SCOPE by construction: each came from a `resolve_symbols`
    // over an imported package, so its declared package is in scope even when `@JvmPackageName` relocated
    // its facade to a different JVM package (`kotlin.collections`'s `UArraysKt` → `kotlin/collections/
    // unsigned/`). Re-deriving scope from the JVM owner (`fn_in_scope`) would wrongly drop those, so trust
    // the query.
    let pre_scoped = kind == FnKind::Extension && ext.fn_scope.is_some();
    crate::trace_compiler!(
        "resolve",
        "select_overload name={name} recv={recv:?} kind={kind:?} scope={:?} cands={}",
        ext.fn_scope.map(FunctionScopeRef::package_count),
        overloads.len(),
    );
    for o in &overloads {
        crate::trace_compiler!(
            "resolve",
            "  raw {name} kind={:?} recv={:?} pub={} rank={} origin={:?} owner={}",
            o.kind,
            o.semantic_receiver(),
            o.public(),
            o.receiver_rank,
            o.callable.origin,
            o.callable.owner.render(),
        );
    }
    let mut by_rank: std::collections::BTreeMap<u32, Vec<(&FunctionInfo, Vec<Ty>)>> =
        std::collections::BTreeMap::new();
    let ranked: Vec<(u32, Ty, &FunctionInfo)> = match kind {
        FnKind::Extension => ranked_extension_candidates(
            src,
            recv,
            overloads
                .iter()
                .copied()
                .filter(|o| pre_scoped || fn_in_scope(o, ext.fn_scope)),
            allow_must_inline,
            ext.current_source_file,
        ),
        FnKind::Member => overloads
            .iter()
            .copied()
            .filter(|o| {
                o.kind == FnKind::Member
                    && member_visible(ext.member_access, o.visibility, o.callable.owner_type())
            })
            .map(|o| (o.receiver_rank, recv, o))
            .collect(),
        FnKind::TopLevel => Vec::new(),
    };
    for (rank, binding_receiver, o) in ranked {
        if kind == FnKind::Extension
            && !generic_bounds_admit(
                lib,
                src,
                o.generic_sig.as_ref(),
                binding_receiver,
                &arg_tys,
                type_args,
            )
        {
            crate::trace_compiler!(
                "resolve",
                "  drop {name} because inferred type arguments violate declared bounds"
            );
            continue;
        }
        let lp = logical_value_params(lib, o, binding_receiver, type_args);
        let lp = specialized_sam_params(&lp, o.generic_sig.as_ref(), args, type_args);
        let lp = apply_platform_call_parameter_nullability(
            lp,
            &o.call_sig.platform_nullable_params,
            &arg_tys,
            o.call_sig.vararg,
        );
        crate::trace_compiler!(
            "resolve",
            "  cand {name} rank={rank} logical_params={lp:?} owner={}",
            o.callable.owner.render()
        );
        by_rank.entry(rank).or_default().push((o, lp));
    }
    for cands in by_rank.values() {
        match best_by_args(lib, assign_src, cands, args) {
            CandidateSelection::Selected(overload) => return Some(overload.clone()),
            CandidateSelection::Ambiguous => return None,
            CandidateSelection::None => {}
        }
    }
    // Platform assignability pass: subtype closure, erased `Any`, and value-class underlying matching.
    // A module-declared argument class reaches its library supertype only through the SOURCE
    // federation (`class V : Thread()` into `take(Thread)`), so admit that walk too.
    // The ordered applicability pass above stays stricter so exact/defaulted calls still win first.
    for cands in by_rank.values() {
        let mut applicable = cands.iter().filter(|(_, lp)| {
            lp.len() == arg_tys.len()
                && lp.iter().zip(args).all(|(p, a)| {
                    platform_arg_assignable(lib, p, &a.ty())
                        || source_arg_assignable(assign_src, p, &a.ty())
                })
        });
        if let Some((o, _)) = applicable.next() {
            if applicable.next().is_some() {
                return None;
            }
            return Some((*o).clone());
        }
    }
    // Vararg ELEMENT-expansion pass: a call passing loose elements (or nothing) where a
    // candidate declares a vararg (`"a.b".trim('.')` against `trim(vararg chars: Char)` — the
    // logical param is the ARRAY; `split('.')` against `split(vararg delimiters: Char,
    // ignoreCase: Boolean = false, limit: Int = 0)` — params after the vararg are reachable
    // only by name, so they must be defaulted). Two tiers per rank: EXACT element matches
    // first (`Char` argument selects the `Char` vararg over the `String` one, mirroring
    // most-specific selection), then platform/source-assignable elements.
    let vararg_applicable = |o: &FunctionInfo, lp: &[Ty], exact: bool| -> bool {
        // A suspend callee's element-form vararg call would route the $default emission
        // outside the CPS pass's coverage — skip (unresolved), never ICE.
        if o.flags.suspend {
            return false;
        }
        let Some(vararg_index) = o.call_sig.vararg_index else {
            return false;
        };
        let Some(elem) = lp.get(vararg_index).and_then(|p| p.array_elem()) else {
            return false;
        };
        args.len() >= vararg_index
            && lp[..vararg_index].iter().zip(args).all(|(p, a)| {
                let ty = a.ty();
                fun_arg_matches(lib, p, &ty, a.is_lambda_literal())
                    || platform_arg_assignable(lib, p, &ty)
                    || source_arg_assignable(assign_src, p, &ty)
            })
            && args[vararg_index..].iter().all(|a| {
                let ty = a.ty();
                ty == elem
                    || (!exact
                        && (platform_arg_assignable(lib, &elem, &ty)
                            || source_arg_assignable(assign_src, &elem, &ty)))
            })
            && (vararg_index + 1..lp.len()).all(|index| o.call_sig.param_has_default(index))
    };
    for exact in [true, false] {
        for cands in by_rank.values() {
            let mut applicable = cands
                .iter()
                .filter(|(o, lp)| vararg_applicable(o, lp, exact));
            if let Some((o, _)) = applicable.next() {
                if applicable.next().is_some() {
                    return None;
                }
                return Some((*o).clone());
            }
        }
    }
    // ABI-form pass, shared with constructor resolution: bridge target collection identity and
    // erase type arguments after exact, widened, and source-level subtype matching have failed.
    if let Some(abi_args) = abi_form_args(lib, &arg_tys) {
        for cands in by_rank.values() {
            let mut applicable = cands
                .iter()
                .filter(|(_, lp)| params_match_abi_form(lib, lp, &abi_args));
            if let Some((o, _)) = applicable.next() {
                if applicable.next().is_some() {
                    return None;
                }
                crate::trace_compiler!(
                    "resolve",
                    "select_overload {} matched via abi-form args {arg_tys:?} -> {abi_args:?}",
                    o.callable.name
                );
                return Some((*o).clone());
            }
        }
    }
    None
}

fn generic_bounds_admit(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    generic_sig: Option<&GenericSig>,
    receiver: Ty,
    args: &[Ty],
    type_args: &[Ty],
) -> bool {
    let Some(gsig) = generic_sig else {
        return true;
    };
    let mut binds = seeded_gsig_binds(gsig, type_args);
    if let Some(declared_receiver) = gsig.receiver {
        unify_ty(declared_receiver, receiver, &mut binds);
    }
    for (&parameter, &argument) in gsig.params.iter().zip(args) {
        unify_ty(parameter, argument, &mut binds);
    }
    generic_bindings_satisfy_bounds(gsig, &binds, |actual, bound| {
        actual == bound
            || crate::assignable::is_assignable(
                &crate::assignable::TyCtx::new(),
                &SourceOracle(src),
                actual,
                bound,
            )
            || platform_arg_assignable(lib, &bound, &actual)
    })
}

fn generic_bounds_admit_slots(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    generic_sig: Option<&GenericSig>,
    receiver: Ty,
    slots: &[Option<Ty>],
    type_args: &[Ty],
) -> bool {
    let Some(gsig) = generic_sig else {
        return true;
    };
    let mut binds = seeded_gsig_binds(gsig, type_args);
    if let Some(declared_receiver) = gsig.receiver {
        unify_ty(declared_receiver, receiver, &mut binds);
    }
    for (&parameter, argument) in gsig.params.iter().zip(slots) {
        if let Some(argument) = argument {
            unify_ty(parameter, *argument, &mut binds);
        }
    }
    generic_bindings_satisfy_bounds(gsig, &binds, |actual, bound| {
        actual == bound
            || crate::assignable::is_assignable(
                &crate::assignable::TyCtx::new(),
                &SourceOracle(src),
                actual,
                bound,
            )
            || platform_arg_assignable(lib, &bound, &actual)
    })
}

/// LOGICAL value parameters of an overload — what a call site's arguments are matched against, with the
/// receiver excluded (it is an attribute). Member/top-level `callable.params` are already value-only; an
/// extension's `callable.params` prepend the receiver in the JVM emit shape, so bind the generic signature
/// to `recv` and drop the leading receiver, preferring each parameter's value-class LOGICAL type over its
/// erased underlying (`Id` over `kotlin/String`).
fn logical_value_params(
    lib: &dyn SemanticPlatform,
    o: &FunctionInfo,
    recv: Ty,
    type_args: &[Ty],
) -> Vec<Ty> {
    let semantic = o.semantic_signature();
    let mut binds = seeded_gsig_binds(&semantic, type_args);
    if let Some(recv_sig) = semantic.receiver {
        unify_ty(recv_sig, recv, &mut binds);
    }
    let mut out = ty_subst_all(&semantic.params, &binds);
    let physical_offset = usize::from(o.is_extension());
    for (i, p) in out.iter_mut().enumerate() {
        if let Some(cp) = o.callable.params.get(i + physical_offset) {
            if lib.value_underlying(*cp).is_some() {
                *p = *cp;
            }
        }
    }
    out
}

/// Assignability through the SOURCE symbol federation (module classes first): a module-declared
/// class passed where a library member expects its (library) supertype — `class V : Thread()` into
/// `take(Thread)` — is invisible to the platform oracle, which only walks classpath supertypes.
fn source_arg_assignable(src: &dyn SymbolSource, param: &Ty, arg: &Ty) -> bool {
    crate::assignable::is_assignable(
        &crate::assignable::TyCtx::new(),
        &SourceOracle(src),
        *arg,
        *param,
    )
}

fn platform_arg_assignable(lib: &dyn SemanticPlatform, param: &Ty, arg: &Ty) -> bool {
    (*arg == Ty::Null && param.is_reference())
        || crate::assignable::is_assignable(
            &crate::assignable::TyCtx::new(),
            &PlatformOracle(lib),
            *arg,
            *param,
        )
}

fn refine_argument_from_bound(lib: &dyn SemanticPlatform, argument: Ty, bound: Ty) -> Option<Ty> {
    let (Ty::Obj(argument_name, argument_args), Ty::Obj(_, bound_args)) = (argument, bound) else {
        return None;
    };
    if argument_args.is_empty()
        || argument_args.len() != bound_args.len()
        || !platform_arg_assignable(lib, &bound, &argument)
    {
        return None;
    }
    let mut refined = argument_args.to_vec();
    let mut changed = false;
    for (actual, constraint) in refined.iter_mut().zip(bound_args) {
        // An erased bound provides no new inference evidence.
        if actual.is_erased_top() && !constraint.is_erased_top() && !constraint.is_ty_param() {
            *actual = *constraint;
            changed = true;
        }
    }
    changed.then(|| Ty::obj_args_name(argument_name, &refined))
}

fn distinct_source_declarations(left: &FunctionInfo, right: &FunctionInfo) -> bool {
    left.source_key.is_some() && right.source_key.is_some() && left.source_key != right.source_key
}

fn source_aware_most_specific<'a, I>(
    candidates: I,
    at_least_as_specific: impl Fn(usize, Ty, Ty) -> bool,
) -> CandidateSelection<&'a FunctionInfo>
where
    I: Iterator<Item = (Vec<Ty>, &'a FunctionInfo)> + Clone,
{
    let mut probe = candidates.clone();
    let Some((_, first)) = probe.next() else {
        return CandidateSelection::None;
    };
    let Some(first_key) = first.source_key else {
        return CandidateSelection::Selected(first);
    };
    if !probe.any(|(_, candidate)| {
        candidate
            .source_key
            .is_some_and(|source_key| source_key != first_key)
    }) {
        return CandidateSelection::Selected(first);
    }
    unique_most_specific_with_conflicts(candidates, at_least_as_specific, |left, right| {
        distinct_source_declarations(left, right)
    })
}

/// Pick the best overload whose logical value parameters accept `args`, in Kotlin applicability order:
/// exact, then `Any`-widened / function-arity, then a prefix under-application (omitted trailing params
/// must be optional), then a trailing-lambda call that omits leading DEFAULTED params (`m.withLock { … }`).
pub(crate) fn best_by_args<'a>(
    lib: &dyn SemanticPlatform,
    src: &dyn SymbolSource,
    cands: &[(&'a FunctionInfo, Vec<Ty>)],
    args: &[CallArgKind],
) -> CandidateSelection<&'a FunctionInfo> {
    // Exact passes see runtime types; literal provenance only drives the adaptation passes.
    let arg_tys: Vec<Ty> = args.iter().map(|arg| arg.ty()).collect();
    let adapts = |p: &Ty, arg: &CallArgKind, _i: usize| arg.adapts_integer_literal_to(*p);
    let function_like_fits = |p: &Ty, arg: &CallArgKind| {
        arg.ty().fun_arity().is_none()
            && p.fun_arity()
                .zip(lib.function_like_arity(arg.ty()))
                .is_some_and(|(param, arg)| usize::from(param) == arg)
    };
    // The DEFAULT-omitting passes accept a reference SUBTYPE / value-class-underlying argument (a
    // `joinToString(separator: CharSequence = …)` call with a `String`), matching the assignability the
    // exact-arity subtype pass in `select_overload` applies — the exact/`Any`-widened passes above stay
    // stricter so an exact call still prefers its precise overload.
    let fits = |_position: usize, p: &Ty, arg: &CallArgKind| {
        if arg.is_lambda_literal() && p.fun_arity().is_none() {
            classpath_sam_arg_matches(lib, *p, arg.ty())
        } else {
            fun_arg_matches(lib, p, &arg.ty(), arg.is_lambda_literal())
                || platform_arg_assignable(lib, p, &arg.ty())
                || source_arg_assignable(src, p, &arg.ty())
                || function_like_fits(p, arg)
        }
    };
    let erased_fits = |_position: usize, p: &Ty, arg: &CallArgKind| {
        if arg.is_lambda_literal() && p.fun_arity().is_none() {
            classpath_sam_arg_matches(lib, *p, arg.ty())
        } else {
            *p == arg.ty()
                || *p == Ty::obj("kotlin/Any")
                || fun_arg_matches(lib, p, &arg.ty(), arg.is_lambda_literal())
                || function_like_fits(p, arg)
        }
    };
    match unique_most_specific_with_conflicts(
        cands
            .iter()
            .filter(|(_, params)| params.as_slice() == arg_tys)
            .map(|(candidate, params)| (params.clone(), *candidate)),
        |_, left, right| left == right,
        |left, right| distinct_source_declarations(left, right),
    ) {
        CandidateSelection::Selected(candidate) => {
            return CandidateSelection::Selected(candidate);
        }
        CandidateSelection::Ambiguous => return CandidateSelection::Ambiguous,
        CandidateSelection::None => {}
    }
    match integer_literal_overload(
        cands
            .iter()
            .map(|(candidate, params)| (params.clone(), *candidate)),
        args,
        |position, param, arg| fits(position, param, arg),
        |_position, left, right, arg| parameter_at_least_as_specific(lib, left, right, arg),
        |left, right| distinct_source_declarations(left, right),
    ) {
        CandidateSelection::Selected(candidate) => {
            return CandidateSelection::Selected(candidate);
        }
        CandidateSelection::Ambiguous => return CandidateSelection::Ambiguous,
        CandidateSelection::None => {}
    }
    if args.iter().any(|arg| arg.is_lambda_literal()) {
        match unique_most_specific_with_conflicts(
            cands.iter().filter_map(|(candidate, params)| {
                fixed_parameter_shape(params, args, |position, param, arg| {
                    erased_fits(position, param, arg)
                })
                .map(|shape| (shape, *candidate))
            }),
            |_, left, right| {
                parameter_at_least_as_specific(lib, left, right, CallArgKind::Typed(Ty::Error))
            },
            |left, right| distinct_source_declarations(left, right),
        ) {
            CandidateSelection::Selected(candidate) => {
                return CandidateSelection::Selected(candidate);
            }
            CandidateSelection::Ambiguous => return CandidateSelection::Ambiguous,
            CandidateSelection::None => {}
        }
    }
    let specificity = |_: usize, left: Ty, right: Ty| {
        parameter_at_least_as_specific(lib, left, right, CallArgKind::Typed(Ty::Error))
    };

    // Exact arity, judged by ASSIGNABILITY, before the erased pass below. `erased_fits` admits a
    // `kotlin/Any` parameter for any argument but nothing else that is merely assignable, so with
    // `pick(value: Any)` and `pick(value: CharSequence)` in scope it dropped the CharSequence overload
    // and selected the widest one — the opposite of Kotlin's most-specific rule. Judging by
    // assignability first lets both compete and specificity decide; when only the `Any` overload fits,
    // this pass finds nothing and the erased pass answers exactly as before.
    match source_aware_most_specific(
        cands.iter().filter_map(|(candidate, params)| {
            fixed_parameter_shape(params, args, |position, param, arg| {
                fits(position, param, arg)
            })
            .map(|shape| (shape, *candidate))
        }),
        specificity,
    ) {
        CandidateSelection::Selected(candidate) => {
            return CandidateSelection::Selected(candidate);
        }
        CandidateSelection::Ambiguous => return CandidateSelection::Ambiguous,
        CandidateSelection::None => {}
    }

    match source_aware_most_specific(
        cands.iter().filter_map(|(candidate, params)| {
            fixed_parameter_shape(params, args, |position, param, arg| {
                erased_fits(position, param, arg)
            })
            .map(|shape| (shape, *candidate))
        }),
        specificity,
    ) {
        CandidateSelection::Selected(candidate) => {
            return CandidateSelection::Selected(candidate);
        }
        CandidateSelection::Ambiguous => return CandidateSelection::Ambiguous,
        CandidateSelection::None => {}
    }

    match source_aware_most_specific(
        cands.iter().filter_map(|(candidate, params)| {
            (candidate.call_sig.required == 0 || candidate.call_sig.required <= args.len())
                .then(|| {
                    omitted_parameter_shape(params, args, |position, param, arg| {
                        fits(position, param, arg) || adapts(param, arg, position)
                    })
                    .map(|shape| (shape, *candidate))
                })
                .flatten()
        }),
        specificity,
    ) {
        CandidateSelection::Selected(candidate) => {
            return CandidateSelection::Selected(candidate);
        }
        CandidateSelection::Ambiguous => return CandidateSelection::Ambiguous,
        CandidateSelection::None => {}
    }

    if matches!(args.last(), Some(arg) if arg.ty().fun_arity().is_some()) {
        match source_aware_most_specific(
            cands.iter().filter_map(|(candidate, params)| {
                let last = params.len().checked_sub(1)?;
                let prefix = args.len().checked_sub(1)?;
                (prefix <= last
                    && fits(last, &params[last], args.last().unwrap())
                    && ((prefix..last).all(|i| candidate.call_sig.param_has_default(i))
                        || candidate.call_sig.required <= prefix)
                    && params[..prefix.min(params.len())]
                        .iter()
                        .zip(&arg_tys[..prefix])
                        .enumerate()
                        .all(|(i, (param, _arg))| {
                            fits(i, param, &args[i]) || adapts(param, &args[i], i)
                        }))
                .then(|| (params.clone(), *candidate))
            }),
            specificity,
        ) {
            CandidateSelection::Selected(candidate) => {
                return CandidateSelection::Selected(candidate);
            }
            CandidateSelection::Ambiguous => return CandidateSelection::Ambiguous,
            CandidateSelection::None => {}
        }
    }

    source_aware_most_specific(
        cands.iter().filter_map(|(candidate, params)| {
            candidate.call_sig.vararg.then(|| {
                vararg_parameter_shape(params, args, |position, param, arg| {
                    fits(position, param, arg) || adapts(param, arg, position)
                })
                .map(|shape| (shape, *candidate))
            })?
        }),
        specificity,
    )
}

/// A lambda argument (`Ty::Fun`) matches a function-typed parameter of the same arity. The parameter may
/// be a decoded `Ty::Fun` (whose return/parameter types differ from the lambda's — the body adapts) or an
/// erased `kotlin/jvm/functions/FunctionN` object; neither pairs with the argument under plain equality or
/// `Any` widening, so arity alone drives the match.
fn fun_arg_matches(
    lib: &dyn SemanticPlatform,
    param: &Ty,
    arg: &Ty,
    allow_unit_coercion: bool,
) -> bool {
    let Some(arg_arity) = arg.fun_arity() else {
        return false;
    };
    let param = match param {
        Ty::Nullable(inner) => **inner,
        _ => *param,
    };
    let arity_ok = param.fun_arity().is_some_and(|pn| pn == arg_arity)
        || param
            .obj_internal()
            .and_then(|p| p.unsigned_suffix_after_prefix("kotlin/jvm/functions/Function"))
            == Some(usize::from(arg_arity));
    arity_ok && fun_return_compatible(lib, param, *arg, allow_unit_coercion)
}

/// A function-typed argument fits a function-typed parameter's RETURN. A parameter `(T) -> R` with a
/// CONCRETE `R` (`sumOfInt`'s `(T) -> Int`) accepts ONLY a lambda whose body returns that `R` — this is
/// how a `@OverloadResolutionByLambdaReturnType` group (whose overloads share value params and differ only
/// in the selector's return) is resolved: the lambda's return is just another parameter of the check. A
/// type-variable / erased-`Any` parameter return (an ordinary generic HOF `(T) -> R`), or an unresolved
/// lambda body, stays permissive so normal HOFs keep matching.
fn fun_return_compatible(
    lib: &dyn SemanticPlatform,
    param: Ty,
    arg: Ty,
    allow_unit_coercion: bool,
) -> bool {
    let (Some(pr), Some(ar)) = (param.fun_ret(), arg.fun_ret()) else {
        return true;
    };
    if matches!(pr, Ty::TyParam(..) | Ty::Error)
        || pr
            .non_null()
            .obj_internal()
            .is_some_and(|n| n.matches("kotlin/Any"))
        || (allow_unit_coercion && pr == Ty::Unit)
    {
        return true;
    }
    if matches!(ar, Ty::Error | Ty::Nothing) {
        return true;
    }
    if pr.non_null() == ar.non_null() {
        return true;
    }
    // A CONCRETE REFERENCE return is covariant: a lambda whose body returns a SUBTYPE (`String`) fits a
    // `(T) -> CharSequence` transform parameter (`joinToString`). Primitive returns stay INVARIANT — the
    // `@OverloadResolutionByLambdaReturnType` families (`sumOf { Int } / { Double }`) differ only by their
    // exact primitive return and must not cross-match.
    if let (Some(p), Some(a)) = (
        pr.non_null().kotlin_class_internal(),
        ar.non_null().kotlin_class_internal(),
    ) {
        if pr.is_reference() && ar.is_reference() {
            return platform_subtype(lib, Ty::obj_name(a), Ty::obj_name(p));
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::libraries::{
        CallSig, FunctionSet, LibraryCallable, LibraryMember, LibraryType, Origin, TypeKind,
    };
    use crate::symbol_source::SymbolSource;
    use crate::types::type_name;

    #[test]
    fn inferred_generic_binding_joins_null_with_the_non_null_element_type() {
        let parameter = Ty::ty_param("T", Ty::obj("kotlin/Any"));
        let mut inferred = GSigBinds::new();
        unify_inferred_ty(parameter, Ty::Int, &mut inferred);
        unify_inferred_ty(parameter, Ty::Null, &mut inferred);
        unify_inferred_ty(parameter, Ty::Int, &mut inferred);
        assert_eq!(inferred.get("T"), Some(&Ty::nullable(Ty::Int)));

        let mut explicit = GSigBinds::from([("T".to_string(), Ty::Int)]);
        unify_ty(parameter, Ty::Null, &mut explicit);
        assert_eq!(explicit.get("T"), Some(&Ty::Int));
    }

    #[test]
    fn nullable_function_result_binds_its_non_null_type_parameter() {
        let parameter = Ty::fun(
            vec![Ty::String],
            Ty::nullable(Ty::ty_param("R", Ty::obj("kotlin/Any"))),
        );
        let argument = Ty::fun(vec![Ty::String], Ty::nullable(Ty::Int));
        let mut bindings = GSigBinds::new();

        unify_ty(parameter, argument, &mut bindings);

        assert_eq!(bindings.get("R"), Some(&Ty::Int));
    }

    #[test]
    fn nullable_function_preserves_lambda_parameter_types() {
        let function = Ty::nullable(Ty::fun(vec![Ty::String], Ty::String));

        assert_eq!(
            function_input_types(function, &GSigBinds::new()),
            vec![Ty::String]
        );
    }

    #[test]
    fn member_return_separates_method_and_owner_bindings() {
        let any = Ty::obj("kotlin/Any");
        let value = Ty::obj("demo/Value");
        let parameter = Ty::ty_param("T", any);
        let class_of = |ty| Ty::obj_args("java/lang/Class", &[ty]);
        let optional_of = |ty| Ty::obj_args("java/util/Optional", &[ty]);

        let method_generic = GenericSig {
            formals: vec!["T".to_string()],
            formal_bounds: vec![Vec::new()],
            receiver: None,
            params: vec![class_of(parameter)],
            ret: optional_of(parameter),
        };
        assert_eq!(
            bind_member_return(
                &method_generic,
                Ty::obj("demo/Provider"),
                &[class_of(value)],
                optional_of(any),
            ),
            optional_of(value)
        );

        let owner_generic = GenericSig {
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver: None,
            params: vec![parameter],
            ret: parameter,
        };
        assert_eq!(
            bind_member_return(&owner_generic, optional_of(value), &[Ty::Null], value,),
            value
        );
        assert_eq!(
            bind_member_return(&owner_generic, optional_of(any), &[Ty::String], any,),
            any
        );
    }

    #[test]
    fn member_return_preserves_canonical_provider_types() {
        let any = Ty::obj("kotlin/Any");
        let kotlin_list = Ty::obj_args("kotlin/collections/List", &[Ty::Int]);
        let jvm_list = Ty::obj_args("java/util/List", &[Ty::obj("java/lang/Integer")]);
        let signature = GenericSig {
            formals: Vec::new(),
            formal_bounds: Vec::new(),
            receiver: None,
            params: Vec::new(),
            ret: jvm_list,
        };

        assert_eq!(
            bind_member_return(&signature, Ty::obj("demo/Provider"), &[], kotlin_list),
            kotlin_list
        );

        let erased_signature = GenericSig {
            ret: Ty::obj_args("java/util/List", &[any]),
            ..signature
        };
        assert_eq!(
            bind_member_return(
                &erased_signature,
                Ty::obj("demo/Provider"),
                &[],
                kotlin_list
            ),
            kotlin_list
        );
    }

    fn fake_library_type(supertypes: Vec<String>, constructors: Vec<LibraryMember>) -> LibraryType {
        LibraryType {
            is_public: true,
            kind: TypeKind::Class,
            supertypes: supertypes.into(),
            constructors,
            members: vec![],
            companion: vec![],
            companion_consts: std::collections::HashMap::new(),
            sam_method: None,
            companion_object: None,
            value_companion_fns: Vec::new(),
            value_underlying: None,
            alias_target: None,
            type_params: Vec::new(),
            sealed_subclasses: crate::types::TypeNameList::new(),
            enum_entries: Vec::new(),
            value_ctor_has_default: false,
            ctor_named_params: Vec::new(),
            value_class_properties: Vec::new(),
            retention: None,
        }
    }

    struct FakeSource {
        name: &'static str,
        receiver: Option<Ty>,
        info: FunctionInfo,
    }

    impl SymbolSource for FakeSource {
        fn member_overloads(&self, recv: Ty, name: &str) -> FunctionSet {
            if self.receiver == Some(recv) && name == self.name {
                FunctionSet {
                    overloads: vec![self.info.clone()],
                }
            } else {
                FunctionSet::default()
            }
        }

        fn resolve_symbols(&self, fqn: &str) -> crate::libraries::ResolvedSymbols {
            // The fake's name is package-less, so a scoped resolver queries it as the bare fqn.
            if fqn == self.name {
                crate::libraries::ResolvedSymbols {
                    classifier: None,
                    callables: crate::libraries::Callables::Functions(FunctionSet {
                        overloads: vec![self.info.clone()],
                    }),
                }
            } else {
                crate::libraries::ResolvedSymbols::default()
            }
        }

        fn resolve_type(&self, internal: &str) -> Option<crate::libraries::LibraryType> {
            let supertypes = match internal {
                "demo/Leaf" => vec!["demo/Mid".to_string()],
                "demo/Mid" => vec!["demo/Base".to_string()],
                "demo/Base" => vec!["kotlin/Any".to_string()],
                "kotlin/UInt" | "demo/Box" => vec!["kotlin/Any".to_string()],
                _ => return None,
            };
            let mut ty = fake_library_type(supertypes, Vec::new());
            ty.value_underlying = (internal == "kotlin/UInt").then_some(Ty::Int);
            Some(ty)
        }
    }

    impl crate::libraries::SemanticPlatform for FakeSource {
        fn value_underlying(&self, ty: Ty) -> Option<Ty> {
            self.resolve_type_name(ty.obj_internal()?)
                .and_then(|t| t.value_underlying)
        }

        fn library_value_form(&self, ty: Ty) -> Ty {
            ty.obj_internal()
                .and_then(|n| crate::jvm::jvm_class_map::kotlin_builtin_to_jvm(&n.render()))
                .map(Ty::obj)
                .unwrap_or(ty)
        }
    }

    impl crate::runtime::TargetRuntime for FakeSource {}

    #[test]
    fn declaration_shapes_are_not_emittable_synthetic_constructors() {
        let plain = LibraryMember::new("<init>".into(), Vec::new(), Ty::Unit, String::new());
        let marker = LibraryMember::new(
            "<init>".into(),
            vec![
                Ty::Int,
                Ty::obj("kotlin/jvm/internal/DefaultConstructorMarker"),
            ],
            Ty::Unit,
            String::new(),
        );
        let classifier = fake_library_type(Vec::new(), vec![plain, marker]);
        let source = FakeSource {
            name: "",
            receiver: None,
            info: top_level_nullable_string_info(),
        };

        assert!(synthetic_default_ctor_from_type(&classifier, 0).is_none());
        assert!(resolve_synthetic_constructor_from_type(
            &source,
            type_name("demo/Category"),
            &classifier,
            &[],
        )
        .is_none());
    }

    fn top_level_default_uint_info() -> FunctionInfo {
        let callable = LibraryCallable {
            owner: "kotlin/UIntKt".into(),
            name: "make$default".to_string(),
            params: vec![Ty::Int],
            ret: Ty::Int,
            physical_ret: Ty::Int,
            descriptor: "(I)I".to_string(),
            suspend: false,
            inline: InlineKind::None,
            default_call: true,
            vararg_elem: None,
            vararg_index: None,
            signature: None,
            origin: Origin::Library,
            source_receiver: None,
        };
        FunctionInfo {
            ret: crate::libraries::ReturnInfo::new(false, Some(Ty::UInt)),
            call_sig: CallSig {
                required: 0,
                param_defaults: vec![true],
                ..Default::default()
            },
            ..FunctionInfo::plain(FnKind::TopLevel, None, callable)
        }
    }

    fn top_level_nullable_string_info() -> FunctionInfo {
        let callable = LibraryCallable::library(
            "kotlin/FooKt",
            "maybe",
            vec![],
            Ty::String,
            Ty::String,
            "()Ljava/lang/String;",
        );
        FunctionInfo {
            ret: crate::libraries::ReturnInfo::new(true, None),
            ..FunctionInfo::plain(FnKind::TopLevel, None, callable)
        }
    }

    fn extension_nullable_string_info() -> FunctionInfo {
        let receiver = Ty::String;
        let callable = LibraryCallable::library(
            "kotlin/text/StringsKt",
            "maybeSuffix",
            vec![receiver],
            Ty::String,
            Ty::String,
            "(Ljava/lang/String;)Ljava/lang/String;",
        );
        FunctionInfo {
            ret: crate::libraries::ReturnInfo::new(true, None),
            ..FunctionInfo::plain(FnKind::Extension, Some(receiver), callable)
        }
    }

    #[test]
    fn nested_generic_conflicts_erase_only_the_later_sam_slot() {
        let parameter = Ty::ty_param("T", Ty::obj("kotlin/Any"));
        let generic_values = Ty::obj_args("fixture/Duo", &[parameter, parameter]);
        let generic_sink = Ty::obj_args("fixture/Sink", &[parameter]);
        let mut member = LibraryMember::new(
            "use".to_string(),
            vec![Ty::obj("fixture/Duo"), Ty::obj("fixture/Sink")],
            Ty::Unit,
            "(Lfixture/Duo;Lfixture/Sink;)V".to_string(),
        );
        member.generic_sig = Some(GenericSig {
            formals: vec!["T".to_string()],
            formal_bounds: vec![Vec::new()],
            receiver: None,
            params: vec![generic_values, generic_sink],
            ret: Ty::Unit,
        });

        let params = specialized_sam_member_params(
            &member,
            &[
                CallArgKind::Typed(Ty::obj_args(
                    "fixture/Duo",
                    &[Ty::String, Ty::obj("kotlin/Any")],
                )),
                CallArgKind::LambdaLiteral(Ty::Error),
            ],
            &[],
        );

        assert_eq!(params[0], Ty::obj("fixture/Duo"));
        assert_eq!(
            params[1],
            Ty::obj_args("fixture/Sink", &[Ty::obj("kotlin/Any")])
        );
    }

    #[test]
    fn receiver_mro_walks_supertypes() {
        let src = FakeSource {
            name: "unused",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        let mro = ReceiverMro::new(&src, Ty::obj("demo/Leaf"));
        assert_eq!(mro.rank(&src, Ty::obj("demo/Base")), Some(2));
        assert_eq!(mro.rank(&src, Ty::obj("demo/Leaf")), Some(0));
        assert_eq!(mro.rank(&src, Ty::obj("demo/Unrelated")), None);
    }

    #[test]
    fn receiver_mro_respects_concrete_extension_nullability() {
        let src = FakeSource {
            name: "unused",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        let nullable = ReceiverMro::new(&src, Ty::nullable(Ty::String));
        assert_eq!(nullable.rank(&src, Ty::String), None);
        assert_eq!(nullable.rank(&src, Ty::nullable(Ty::String)), Some(0));

        let non_null = ReceiverMro::new(&src, Ty::String);
        assert_eq!(non_null.rank(&src, Ty::String), Some(0));
        assert_eq!(non_null.rank(&src, Ty::nullable(Ty::String)), Some(0));

        let unbounded_generic = Ty::ty_param("T", Ty::nullable(Ty::obj("kotlin/Any")));
        assert_eq!(nullable.rank(&src, unbounded_generic), Some(u32::MAX - 1));
        let explicitly_non_null_generic = Ty::ty_param("T", Ty::obj("kotlin/Any"));
        assert_eq!(nullable.rank(&src, explicitly_non_null_generic), None);
    }

    #[test]
    fn integer_literal_overloads_require_a_unique_most_specific_parameter_list() {
        let source = FakeSource {
            name: "unused",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        let args = [
            CallArgKind::integer_literal(Ty::Int, 1),
            CallArgKind::integer_literal(Ty::Int, 1),
        ];
        let selected = integer_literal_overload(
            [
                (vec![Ty::Int, Ty::Long], "narrow"),
                (vec![Ty::Long, Ty::Long], "wide"),
            ]
            .into_iter(),
            &args,
            |_, param, arg| arg_fits(param, &arg.ty()),
            |_, left, right, arg| parameter_at_least_as_specific(&source, left, right, arg),
            |_, _| false,
        );
        assert!(matches!(selected, CandidateSelection::Selected("narrow")));

        let ambiguous = integer_literal_overload(
            [
                (vec![Ty::Int, Ty::Long], "left"),
                (vec![Ty::Long, Ty::Int], "right"),
            ]
            .into_iter(),
            &args,
            |_, param, arg| arg_fits(param, &arg.ty()),
            |_, left, right, arg| parameter_at_least_as_specific(&source, left, right, arg),
            |_, _| false,
        );
        assert!(matches!(ambiguous, CandidateSelection::Ambiguous));

        let select_integral_width = |value| {
            integer_literal_overload(
                [(vec![Ty::Byte], "byte"), (vec![Ty::Long], "long")].into_iter(),
                &[CallArgKind::integer_literal(Ty::Int, value)],
                |_, param, arg| arg_fits(param, &arg.ty()),
                |_, left, right, arg| parameter_at_least_as_specific(&source, left, right, arg),
                |_, _| false,
            )
        };
        assert!(matches!(
            select_integral_width(1),
            CandidateSelection::Ambiguous
        ));
        assert!(matches!(
            select_integral_width(1_000),
            CandidateSelection::Selected("long")
        ));
    }

    #[test]
    fn concrete_lambda_return_rejects_same_arity_fallback() {
        let source = FakeSource {
            name: "unused",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        assert!(fun_return_compatible(
            &source,
            Ty::fun(vec![Ty::String], Ty::Unit),
            Ty::fun(vec![Ty::String], Ty::String),
            true,
        ));
        assert!(!fun_return_compatible(
            &source,
            Ty::fun(vec![Ty::String], Ty::Unit),
            Ty::fun(vec![Ty::String], Ty::String),
            false,
        ));
        assert!(fun_return_compatible(&source, Ty::Unit, Ty::Nothing, false,));

        let int_transform = Ty::fun(vec![Ty::Int], Ty::Int);
        let string_transform = Ty::fun(vec![Ty::Int], Ty::String);
        let candidate = |name: &str, transform: Ty| {
            FunctionInfo::plain(
                FnKind::TopLevel,
                None,
                LibraryCallable::library(
                    "fixture/Calls",
                    name,
                    vec![transform],
                    Ty::Unit,
                    Ty::Unit,
                    "(Lkotlin/jvm/functions/Function1;)V",
                ),
            )
        };
        let int_candidate = candidate("chooseInt", int_transform);
        let string_candidate = candidate("chooseString", string_transform);
        let candidates = [
            (&int_candidate, vec![int_transform]),
            (&string_candidate, vec![string_transform]),
        ];
        let argument = Ty::fun(vec![Ty::Error], Ty::String);

        let selected = best_by_args(
            &source,
            &source,
            &candidates,
            &[CallArgKind::LambdaLiteral(argument)],
        );

        let CandidateSelection::Selected(selected) = selected else {
            panic!("concrete lambda return should select one overload");
        };
        assert_eq!(selected.callable.name, "chooseString");
    }

    #[test]
    fn companion_overloads_use_the_composite_source_hierarchy() {
        let source = FakeSource {
            name: "unused",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        let member =
            |params| LibraryMember::new("make".to_string(), params, Ty::Unit, String::new());

        let broad = member(vec![Ty::obj("demo/Base")]);
        let specific = member(vec![Ty::obj("demo/Mid")]);
        let specific_duplicate = member(vec![Ty::obj("demo/Mid")]);
        let selected = best_companion_overload(
            &source,
            &source,
            [&broad, &specific, &specific_duplicate].into_iter(),
            "make",
            &[CallArgKind::Typed(Ty::obj("demo/Leaf"))],
            &[],
        )
        .expect("the most specific source supertype should be selected");
        assert_eq!(selected.params, vec![Ty::obj("demo/Mid")]);

        let left = member(vec![Ty::obj("demo/Mid"), Ty::obj("demo/Base")]);
        let right = member(vec![Ty::obj("demo/Base"), Ty::obj("demo/Mid")]);
        assert!(best_companion_overload(
            &source,
            &source,
            [&left, &right].into_iter(),
            "make",
            &[
                CallArgKind::Typed(Ty::obj("demo/Leaf")),
                CallArgKind::Typed(Ty::obj("demo/Leaf")),
            ],
            &[],
        )
        .is_none());

        let aliases = unique_most_specific(
            [
                (vec![Ty::obj("kotlin/Any")], "any"),
                (vec![Ty::String], "string"),
                (vec![Ty::obj("java/lang/String")], "java string"),
            ],
            |_, left, right| resolution_subtype(&source, &source, left, right),
        );
        assert!(matches!(aliases, CandidateSelection::Selected("string")));
    }

    #[test]
    fn companion_default_and_vararg_shapes_accept_source_subtypes() {
        let source = FakeSource {
            name: "unused",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        let member =
            |params| LibraryMember::new("make".to_string(), params, Ty::Unit, String::new());

        let with_trailing_default = |mut member: LibraryMember| {
            member.call_sig.required = 1;
            member.call_sig.param_names = vec!["value".into(), "label".into()];
            member.call_sig.param_defaults = vec![false, true];
            member
        };
        let default_broad = with_trailing_default(member(vec![Ty::obj("demo/Base"), Ty::String]));
        let default_specific = with_trailing_default(member(vec![Ty::obj("demo/Mid"), Ty::String]));
        let selected = best_companion_overload(
            &source,
            &source,
            [&default_broad, &default_specific].into_iter(),
            "make",
            &[CallArgKind::Typed(Ty::obj("demo/Leaf"))],
            &[],
        )
        .expect("the defaulted source-supertype overload should resolve");
        assert_eq!(selected.params[0], Ty::obj("demo/Mid"));

        let as_vararg = |mut member: LibraryMember| {
            member.call_sig.vararg = true;
            member.call_sig.vararg_index = Some(0);
            member.call_sig.param_names = vec!["values".into()];
            member.call_sig.param_defaults = vec![false];
            member
        };
        let vararg_broad = as_vararg(member(vec![Ty::array(Ty::obj("demo/Base"))]));
        let vararg_specific = as_vararg(member(vec![Ty::array(Ty::obj("demo/Mid"))]));
        let selected = best_companion_overload(
            &source,
            &source,
            [&vararg_broad, &vararg_specific].into_iter(),
            "make",
            &[CallArgKind::Typed(Ty::obj("demo/Leaf"))],
            &[],
        )
        .expect("the vararg source-supertype overload should resolve");
        assert_eq!(selected.params[0], Ty::array(Ty::obj("demo/Mid")));

        let ordinary = member(vec![Ty::obj("demo/Base"), Ty::String]);
        assert!(
            best_companion_overload(
                &source,
                &source,
                [&ordinary].into_iter(),
                "make",
                &[CallArgKind::Typed(Ty::obj("demo/Leaf"))],
                &[],
            )
            .is_none(),
            "an unmarked trailing parameter is required"
        );
    }

    #[test]
    fn platform_class_identity_uses_borrowed_table_entries() {
        assert_eq!(
            platform_class_identity("kotlin/collections/Map$Entry"),
            "java/util/Map$Entry"
        );
        assert_eq!(
            platform_class_identity("kotlin/collections/Map.Entry"),
            "java/util/Map$Entry"
        );
        let unmapped = "demo/Outer.Inner";
        assert!(std::ptr::eq(
            platform_class_identity(unmapped).as_ptr(),
            unmapped.as_ptr()
        ));
    }

    #[test]
    fn platform_class_names_match_nested_spellings_without_normalizing() {
        assert!(platform_class_names_match("lib/Flex.FMap", "lib/Flex$FMap"));
        assert!(platform_class_names_match(
            "lib/Flex.Inner.Deep",
            "lib/Flex$Inner$Deep"
        ));
        assert!(!platform_class_names_match(
            "lib/Flex.FMap",
            "other/Flex$FMap"
        ));
        assert!(!platform_class_names_match(
            "lib/Flex.FMap",
            "lib/Flex$Other"
        ));
    }

    #[test]
    fn platform_type_names_match_nested_spellings_without_rendering() {
        let dotted = crate::types::type_name("lib/Flex.Inner.Deep");
        let dollar = crate::types::type_name("lib/Flex$Inner$Deep");
        let other_pkg = crate::types::type_name("other/Flex$Inner$Deep");
        let other_tail = crate::types::type_name("lib/Flex$Inner$Other");
        assert!(platform_type_names_match(dotted, dollar));
        assert!(!platform_type_names_match(dotted, other_pkg));
        assert!(!platform_type_names_match(dotted, other_tail));
    }

    #[test]
    fn platform_oracle_compares_map_entry_spellings_by_type_name() {
        let src = FakeSource {
            name: "unused",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        let oracle = PlatformOracle(&src);
        assert!(crate::assignable::TypeOracle::same_class_name(
            &oracle,
            crate::types::type_name("kotlin/collections/Map.Entry"),
            crate::types::type_name("kotlin/collections/Map$Entry"),
        ));
        assert!(crate::assignable::TypeOracle::same_class_name(
            &oracle,
            crate::types::type_name("lib/Flex.FMap"),
            crate::types::type_name("lib/Flex$FMap"),
        ));
    }

    fn member_nullable_string_info() -> FunctionInfo {
        let receiver = Ty::obj("demo/Box");
        let callable = LibraryCallable::library(
            "demo/Box",
            "maybe",
            vec![],
            Ty::String,
            Ty::String,
            "()Ljava/lang/String;",
        );
        FunctionInfo {
            ret: crate::libraries::ReturnInfo::new(true, None),
            ..FunctionInfo::plain(FnKind::Member, Some(receiver), callable)
        }
    }

    fn member_metadata_class_info() -> FunctionInfo {
        let receiver = Ty::obj("demo/Box");
        let callable = LibraryCallable::library(
            "demo/Box",
            "names",
            vec![],
            Ty::obj("kotlin/Any"),
            Ty::obj("kotlin/Any"),
            "()Ljava/lang/Object;",
        );
        FunctionInfo {
            ret: crate::libraries::ReturnInfo::new(
                false,
                Some(Ty::obj_args("kotlin/collections/List", &[Ty::String])),
            ),
            ..FunctionInfo::plain(FnKind::Member, Some(receiver), callable)
        }
    }

    #[test]
    fn top_level_default_callable_preserves_metadata_return_type() {
        let source = FakeSource {
            name: "make$default",
            receiver: None,
            info: top_level_default_uint_info(),
        };
        let scope = vec![type_name("")];
        let resolver = SymbolResolver::new_scoped(&source, &scope);
        let call = resolver
            .resolve_symbol(SymRecv::TopLevel, "make", &[], &[])
            .and_then(Symbol::top_level_call)
            .expect("default callable should resolve");
        assert!(call.default_call);
        assert_eq!(call.ret, Ty::UInt);
        assert_eq!(call.physical_ret, Ty::Int);
    }

    #[test]
    fn top_level_callable_preserves_nullable_metadata_return() {
        let source = FakeSource {
            name: "maybe",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        let scope = vec![type_name("")];
        let resolver = SymbolResolver::new_scoped(&source, &scope);
        let call = resolver
            .resolve_symbol(SymRecv::TopLevel, "maybe", &[], &[])
            .and_then(Symbol::top_level_call)
            .expect("nullable callable should resolve");
        assert_eq!(call.ret, Ty::nullable(Ty::String));
        assert_eq!(call.physical_ret, Ty::String);
    }

    #[test]
    fn top_level_callable_uses_source_subtypes_for_fixed_and_vararg_shapes() {
        let broad = LibraryCallable::library(
            "demo/FunctionsKt",
            "accept",
            vec![Ty::obj("demo/Base")],
            Ty::Unit,
            Ty::Unit,
            "(Ldemo/Base;)V",
        );
        let specific = LibraryCallable::library(
            "demo/FunctionsKt",
            "accept",
            vec![Ty::obj("demo/Mid")],
            Ty::Unit,
            Ty::Unit,
            "(Ldemo/Mid;)V",
        );
        let source = FakeSource {
            name: "accept",
            receiver: None,
            info: FunctionInfo::plain(FnKind::TopLevel, None, broad.clone()),
        };
        let scope = vec![type_name("")];
        let resolver = SymbolResolver::new_scoped(&source, &scope);
        let call = resolver
            .pick_top_level(
                "accept",
                &FunctionSet {
                    overloads: vec![
                        FunctionInfo::plain(FnKind::TopLevel, None, broad),
                        FunctionInfo::plain(FnKind::TopLevel, None, specific),
                    ],
                },
                &[CallArgKind::Typed(Ty::obj("demo/Leaf"))],
                &[],
                None,
            )
            .expect("source subtype should fit top-level parameter");
        assert_eq!(call.params, vec![Ty::obj("demo/Mid")]);

        let alias = |param, descriptor| {
            FunctionInfo::plain(
                FnKind::TopLevel,
                None,
                LibraryCallable::library(
                    "demo/FunctionsKt",
                    "accept",
                    vec![param],
                    Ty::Unit,
                    Ty::Unit,
                    descriptor,
                ),
            )
        };
        let call = resolver
            .pick_top_level(
                "accept",
                &FunctionSet {
                    overloads: vec![
                        alias(Ty::obj("kotlin/Any"), "(Ljava/lang/Object;)V"),
                        alias(Ty::String, "(Ljava/lang/String;)V"),
                    ],
                },
                &[CallArgKind::Typed(Ty::obj("java/lang/String"))],
                &[],
                None,
            )
            .expect("platform aliases should remain applicable");
        assert_eq!(call.params, vec![Ty::String]);

        let vararg = |element, descriptor| {
            FunctionInfo::plain(
                FnKind::TopLevel,
                None,
                LibraryCallable::library(
                    "demo/FunctionsKt",
                    "accept",
                    vec![Ty::array(element)],
                    Ty::Unit,
                    Ty::Unit,
                    descriptor,
                ),
            )
        };
        let call = resolver
            .pick_top_level(
                "accept",
                &FunctionSet {
                    overloads: vec![
                        vararg(Ty::obj("demo/Base"), "([Ldemo/Base;)V"),
                        vararg(Ty::obj("demo/Mid"), "([Ldemo/Mid;)V"),
                    ],
                },
                &[
                    CallArgKind::Typed(Ty::obj("demo/Leaf")),
                    CallArgKind::Typed(Ty::obj("demo/Leaf")),
                ],
                &[],
                None,
            )
            .expect("source subtypes should fit top-level varargs");
        assert_eq!(call.params, vec![Ty::array(Ty::obj("demo/Mid"))]);

        let literal = |second, descriptor| {
            FunctionInfo::plain(
                FnKind::TopLevel,
                None,
                LibraryCallable::library(
                    "demo/FunctionsKt",
                    "accept",
                    vec![Ty::Long, second],
                    Ty::Unit,
                    Ty::Unit,
                    descriptor,
                ),
            )
        };
        let call = resolver
            .pick_top_level(
                "accept",
                &FunctionSet {
                    overloads: vec![
                        literal(Ty::obj("demo/Base"), "(JLdemo/Base;)V"),
                        literal(Ty::obj("demo/Mid"), "(JLdemo/Mid;)V"),
                    ],
                },
                &[
                    CallArgKind::integer_literal(Ty::Int, 1),
                    CallArgKind::Typed(Ty::obj("demo/Leaf")),
                ],
                &[],
                None,
            )
            .expect("integer adaptation should retain source-type specificity");
        assert_eq!(call.params, vec![Ty::Long, Ty::obj("demo/Mid")]);
    }

    #[test]
    fn ambiguous_source_subtype_overloads_do_not_fall_back_to_vararg() {
        let info = |params, descriptor| {
            FunctionInfo::plain(
                FnKind::TopLevel,
                None,
                LibraryCallable::library(
                    "demo/FunctionsKt",
                    "accept",
                    params,
                    Ty::Unit,
                    Ty::Unit,
                    descriptor,
                ),
            )
        };
        let source = FakeSource {
            name: "accept",
            receiver: None,
            info: top_level_nullable_string_info(),
        };
        let scope = vec![type_name("")];
        let resolver = SymbolResolver::new_scoped(&source, &scope);
        let selected = resolver.pick_top_level(
            "accept",
            &FunctionSet {
                overloads: vec![
                    info(
                        vec![Ty::obj("demo/Mid"), Ty::obj("demo/Base")],
                        "(Ldemo/Mid;Ldemo/Base;)V",
                    ),
                    info(
                        vec![Ty::obj("demo/Base"), Ty::obj("demo/Mid")],
                        "(Ldemo/Base;Ldemo/Mid;)V",
                    ),
                    info(
                        vec![Ty::array(Ty::obj("kotlin/Any"))],
                        "([Ljava/lang/Object;)V",
                    ),
                ],
            },
            &[
                CallArgKind::Typed(Ty::obj("demo/Leaf")),
                CallArgKind::Typed(Ty::obj("demo/Leaf")),
            ],
            &[],
            None,
        );
        assert!(selected.is_none());
    }

    #[test]
    fn extension_callable_preserves_nullable_metadata_return() {
        let source = FakeSource {
            name: "maybeSuffix",
            receiver: Some(Ty::String),
            info: extension_nullable_string_info(),
        };
        let scope = vec![type_name("")];
        let resolver = SymbolResolver::new_scoped(&source, &scope);
        let call = resolver
            .resolve_symbol(SymRecv::Value(Ty::String), "maybeSuffix", &[], &[])
            .and_then(Symbol::extension_call)
            .expect("nullable extension callable should resolve");
        assert_eq!(call.ret, Ty::nullable(Ty::String));
        assert_eq!(call.physical_ret, Ty::String);
    }

    #[test]
    fn instance_member_preserves_nullable_metadata_return() {
        let source = FakeSource {
            name: "maybe",
            receiver: Some(Ty::obj("demo/Box")),
            info: member_nullable_string_info(),
        };
        let resolved = resolve_instance_member(&source, Ty::obj("demo/Box"), "maybe", &[], None)
            .expect("nullable member should resolve");
        assert_eq!(resolved.ret, Ty::nullable(Ty::String));
        assert_eq!(resolved.member.physical_ret, Ty::String);
    }

    #[test]
    fn instance_member_preserves_metadata_return_class() {
        let source = FakeSource {
            name: "names",
            receiver: Some(Ty::obj("demo/Box")),
            info: member_metadata_class_info(),
        };
        let resolved = resolve_instance_member(&source, Ty::obj("demo/Box"), "names", &[], None)
            .expect("member with metadata return class should resolve");
        assert_eq!(
            resolved.ret,
            Ty::obj_args("kotlin/collections/List", &[Ty::String])
        );
        assert_eq!(resolved.member.physical_ret, Ty::obj("kotlin/Any"));
    }
}
