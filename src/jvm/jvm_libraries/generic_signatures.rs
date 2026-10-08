//! Reading a JVM `Signature` attribute into Kotlin types.
//!
//! A classfile's erased descriptor loses every type argument, so a generic declaration's real shape
//! lives in its `Signature` attribute beside it. These turn that attribute into the `Ty` the symbol
//! source publishes — the field's own type, a type parameter's erasure, a generic return, a suspend
//! function's pre-CPS result, and a class's parameters with their bounds.

use super::*;

/// Decode the source type of a field while verifying that it erases to the classfile descriptor.
/// Unlike the constant-field helper, this retains type variables: the shared resolver substitutes
/// them from the applied receiver and falls back to `erased_ty` only for a raw receiver.
pub(super) fn parse_field_gsig(
    signature: &str,
    erased_descriptor: &str,
    declaring_class_signature: Option<&str>,
) -> Option<(Ty, bool)> {
    if !matches!(signature.as_bytes().first(), Some(b'L' | b'T' | b'[')) {
        return None;
    }
    let parsed = parse_gsig_inner(signature, true)?;
    let erasure = field_type_parameter_erasure(signature, declaring_class_signature)
        .or(parsed.erasure.as_deref().map(str::to_string));
    if !parsed.rest.is_empty()
        || erasure.as_deref() != Some(erased_descriptor)
        || parsed.field_inexact
    {
        return None;
    }
    Some((canonicalize_jvm_collections(parsed.ty), parsed.has_free))
}

/// Compute the descriptor erasure of an outermost declaring-class type variable (or an array of it).
/// The JVM uses the variable's leftmost bound, so `T : CharSequence` erases to `CharSequence`, not
/// unconditionally to `Object`. Chained bounds are followed defensively and cycles are rejected.
fn field_type_parameter_erasure(
    signature: &str,
    declaring_class_signature: Option<&str>,
) -> Option<String> {
    let mut element = signature;
    let mut dimensions = 0;
    while let Some(rest) = element.strip_prefix('[') {
        dimensions += 1;
        element = rest;
    }
    let name = element.strip_prefix('T')?.strip_suffix(';')?;
    let (formals, bounds, _) = parse_formals(declaring_class_signature?);
    let by_name: std::collections::HashMap<&str, Ty> = formals
        .iter()
        .zip(&bounds)
        .filter_map(|(formal, bounds)| {
            bounds
                .first()
                .copied()
                .map(|bound| (formal.as_str(), bound))
        })
        .collect();
    let mut bound = *by_name.get(name)?;
    let mut seen = std::collections::HashSet::new();
    while let Ty::TyParam(next, _) = bound {
        if !seen.insert(next) {
            return None;
        }
        bound = *by_name.get(next)?;
    }
    Some(format!(
        "{}{}",
        "[".repeat(dimensions),
        type_descriptor(bound)
    ))
}

pub(super) fn concrete_generic_ret(gsig: &GenericSig) -> Option<Ty> {
    match gsig.ret {
        Ty::Obj(_, args) if !args.is_empty() && !has_free_ty_params(gsig.ret) => {
            // Canonicalize the recovered type to Kotlin form (`java/util/List<java/lang/Integer>` →
            // `kotlin/collections/List<kotlin/Int>`), so a member/`for`/extension keyed on the Kotlin
            // collection + a primitive element resolves and unboxes — mirroring the suspend path. Without
            // it, a classpath property `items: List<Int>` reads as raw `java/util/List<Integer>`:
            // `xs.sum()` is unresolved and `for (x in xs) { s += x }` compares `Int` vs `java/lang/Integer`.
            Some(canonicalize_jvm_collections(
                crate::symbol_resolver::ty_subst(gsig.ret, &std::collections::HashMap::new()),
            ))
        }
        _ => None,
    }
}

/// The LOGICAL return of a `suspend` method, recovered from its generic signature: the last parameter is
/// `Continuation<-T>`, whose type argument `T` is the source return type (`Continuation<-Config>` →
/// `Config`). A `Continuation<-Unit>` maps to `Ty::Unit` (the source `Unit` return).
pub(super) fn suspend_return_from_gsig(
    gsig: &GenericSig,
    binds: &std::collections::HashMap<String, Ty>,
) -> Option<Ty> {
    match *gsig.params.last()? {
        Ty::Obj(n, args) if crate::types::same(n, crate::types::wk::continuation()) => {
            match *args.first()? {
                // A bare class → its CANONICAL `Ty` (`kotlin/String` → `Ty::String`, `kotlin/Int` → `Ty::Int`,
                // `kotlin/Unit` → `Ty::Unit`), so the recovered return unifies with the source-spelled type
                // rather than a non-canonical `Obj("kotlin/String")`. A generic class (`List<Item>`) keeps its
                // arguments via the general converter.
                Ty::Obj(name, []) => {
                    // Canonicalize a JVM built-in the generic signature spells in Java terms
                    // (`java/lang/String` → `kotlin/String`, `java/lang/Object` → `kotlin/Any`) so the
                    // recovered return unifies with the source-spelled type rather than a non-canonical
                    // `Obj("java/lang/String")`. A boxed PRIMITIVE (`java/lang/Long`) stays an `Obj` here —
                    // the call site unboxes it to the source primitive only when the return is non-nullable
                    // (a `Long?` return must keep the boxed form).
                    Some(
                        Ty::obj_name(crate::jvm::jvm_class_map::to_kotlin_type_name(name))
                            .canonical_semantic(),
                    )
                }
                // A generic class (`List<Item>`) keeps its arguments via the general converter, then any JVM
                // collection name the signature spelled in Java terms (`java/util/List`) is canonicalized to
                // its Kotlin type (`kotlin/collections/List`) so a `.map { … }` / `.first()` extension — keyed
                // on the Kotlin collection — resolves on the recovered suspend result (a member such as `.size`
                // already resolved on either form). A BARE type parameter (`Continuation<T>` from a generic
                // `suspend fun byId(): T` on a `Repo<Cfg>` receiver) is substituted under `binds` to the
                // receiver's concrete argument (`T` → `Cfg`) — otherwise it erases to `Any` and every member
                // access on the result fails ("member … on Any").
                other => Some(canonicalize_jvm_collections(
                    crate::symbol_resolver::ty_subst(other, binds),
                )),
            }
        }
        _ => None,
    }
}

/// Restore the RECEIVER function-type marks a JVM `Signature` attribute cannot carry: for each value
/// parameter `@Metadata` marks `@kotlin.ExtensionFunctionType`, turn the decoded `(R, …) -> T` into
/// `R.(…) -> T`. A `suspend` callable's physical signature appends a `Continuation` the source parameter
/// list does not have, so it is dropped before aligning. Applied positionally, and skipped unless the two
/// lists then align 1:1 — a mismatch means they describe different parameters.
pub(super) fn mark_receiver_fun_params(gsig: &mut GenericSig, recv_fun: &[bool], suspend: bool) {
    let source_params = gsig.params.len().saturating_sub(usize::from(suspend));
    if source_params != recv_fun.len() {
        return;
    }
    for (param, &receiver_fun) in gsig.params.iter_mut().take(source_params).zip(recv_fun) {
        let Ty::Fun(sig) = *param else { continue };
        if !receiver_fun || sig.has_receiver || sig.params.is_empty() {
            continue;
        }
        *param = Ty::fun_with_shape(
            sig.params.clone(),
            sig.ret,
            sig.context_count,
            true,
            sig.suspend,
        );
    }
}

/// Parse a class generic signature into its formal type-parameter names and its supertypes (the
/// superclass followed by interfaces) as signature nodes, e.g. `java/util/List`'s
/// `<E:Ljava/lang/Object;>Ljava/lang/Object;Ljava/util/Collection<TE;>;` → (`[E]`, `[Object,
/// Collection<E>]`). The supertypes carry their own type arguments (in terms of this class's formals),
/// which is what lets a type argument propagate up the hierarchy (`List<Int>` → `Collection<Int>`).
pub(in crate::jvm) type ParsedClassGenericSignature = (Vec<String>, Vec<Vec<Ty>>, Vec<Ty>);

pub(in crate::jvm) fn parse_class_gsig(sig: &str) -> Option<ParsedClassGenericSignature> {
    let (formals, formal_bounds, mut s) = parse_formals(sig);
    let mut supers = Vec::new();
    while !s.is_empty() {
        let (g, rest) = parse_gsig(s)?;
        supers.push(g);
        s = rest;
    }
    Some((formals, formal_bounds, supers))
}

/// The inference signature of a JVM constructor.
///
/// A constructor `Signature` names only type parameters the constructor itself declares and returns
/// `void`. The constructed classifier's type parameters are declared on the class and show up in
/// the parameter list as free type variables. Kotlin inference treats those class parameters as
/// the constructor's result variables — the same shape metadata constructors already publish — and
/// keeps constructor-only parameters as additional variables that do not appear in the result. A
/// constructor parameter that redeclares a class parameter's name is a different variable.
pub(super) fn constructor_inference_signature(
    class_signature: Option<&str>,
    owner: TypeName,
    declaration: &str,
    mut method: GenericSig,
) -> GenericSig {
    let Some((class_formals, class_bounds, _)) = class_signature.and_then(parse_class_gsig) else {
        return method;
    };
    if class_formals.is_empty() {
        return method;
    }
    let result_arguments = class_formals
        .iter()
        .zip(&class_bounds)
        .map(|(formal, bounds)| {
            let bound = bounds
                .first()
                .copied()
                .unwrap_or_else(|| Ty::nullable(Ty::obj("kotlin/Any")));
            Ty::ty_param(formal, bound)
        })
        .collect::<Vec<_>>();
    if method.ret.obj_internal() == Some(owner)
        && method.ret.type_args() == result_arguments.as_slice()
        && method.formals.starts_with(class_formals.as_slice())
    {
        return method;
    }

    let declared_formals = method.formals.clone();
    let mut constructor_formals = Vec::with_capacity(declared_formals.len());
    let mut rename = std::collections::HashMap::new();
    for (ordinal, formal) in declared_formals.iter().enumerate() {
        if class_formals
            .iter()
            .any(|class_formal| class_formal == formal)
        {
            let fresh =
                crate::types::constructor_type_parameter(owner, declaration, ordinal, formal);
            rename.insert(formal.as_str(), fresh);
            constructor_formals.push(fresh.to_string());
        } else {
            constructor_formals.push(formal.clone());
        }
    }
    if !rename.is_empty() {
        for parameter in &mut method.params {
            *parameter = crate::types::ty_rename_params(*parameter, &rename);
        }
        for bounds in &mut method.formal_bounds {
            for bound in bounds {
                *bound = crate::types::ty_rename_params(*bound, &rename);
            }
        }
    }

    let mut formals = class_formals;
    let mut formal_bounds = class_bounds;
    formals.extend(constructor_formals);
    formal_bounds.extend(method.formal_bounds);
    GenericSig {
        formals,
        formal_bounds,
        receiver: method.receiver,
        params: method.params,
        ret: Ty::obj_args_name(owner, &result_arguments),
        return_policy: GenericReturnPolicy::Exact,
    }
}

impl JvmLibraries {
    /// Publish a Java method's `Signature` attribute: parameter and return nullability, then the
    /// constructor's class type parameters when `constructor` is set. The caller decides
    /// `<init>` at this provider boundary.
    pub(super) fn publish_java_member_generic_signature(
        &self,
        signature: Option<&str>,
        parameter_nullability: &[Option<JavaNullability>],
        return_nullability: Option<JavaNullability>,
        constructor: Option<(Option<&str>, TypeName, &str)>,
    ) -> Option<GenericSig> {
        let mut generic = signature
            .and_then(parse_method_gsig)
            .map(|signature| self.semanticize_jvm_generic_sig(signature))?;
        for (index, parameter) in generic.params.iter_mut().enumerate() {
            *parameter = java_type_nullability(
                java_collection_return_lower_bound(*parameter),
                parameter_nullability.get(index).copied().flatten(),
            );
        }
        generic.ret = java_type_nullability(
            java_collection_return_lower_bound(generic.ret),
            return_nullability,
        );
        if let Some((class_signature, owner, descriptor)) = constructor {
            generic = self.semanticize_jvm_generic_sig(constructor_inference_signature(
                class_signature,
                owner,
                descriptor,
                generic,
            ));
        }
        Some(generic)
    }
}

#[cfg(test)]
mod constructor_inference_signature_tests {
    use super::*;

    fn void_constructor(formals: &[&str], params: &[Ty]) -> GenericSig {
        GenericSig {
            formals: formals.iter().map(|formal| (*formal).to_string()).collect(),
            formal_bounds: vec![Vec::new(); formals.len()],
            receiver: None,
            params: params.to_vec(),
            ret: Ty::Unit,
            return_policy: GenericReturnPolicy::Exact,
        }
    }

    #[test]
    fn class_type_parameters_are_the_constructor_result_variables() {
        let owner = type_name("demo/Ref");
        let signature = constructor_inference_signature(
            Some("<V:Ljava/lang/Object;>Ljava/lang/Object;"),
            owner,
            "()V",
            void_constructor(&[], &[Ty::ty_param("V", Ty::obj("kotlin/Any"))]),
        );

        assert_eq!(signature.formals, vec!["V".to_string()]);
        assert_eq!(signature.ret.obj_internal(), Some(owner));
        assert!(matches!(
            signature.ret.type_args(),
            [Ty::TyParam(name, _)] if *name == "V"
        ));
    }

    #[test]
    fn a_constructor_type_parameter_stays_out_of_the_result() {
        let owner = type_name("demo/Cell");
        let signature = constructor_inference_signature(
            Some("<E:Ljava/lang/Object;>Ljava/lang/Object;"),
            owner,
            "(Ljava/lang/Object;Ljava/lang/Object;)V",
            void_constructor(
                &["U"],
                &[
                    Ty::ty_param("E", Ty::obj("kotlin/Any")),
                    Ty::ty_param("U", Ty::obj("kotlin/Any")),
                ],
            ),
        );

        assert_eq!(signature.formals, vec!["E".to_string(), "U".to_string()]);
        assert!(matches!(
            signature.ret.type_args(),
            [Ty::TyParam(name, _)] if *name == "E"
        ));
        assert!(matches!(
            signature.params.as_slice(),
            [Ty::TyParam(value, _), Ty::TyParam(extra, _)]
                if *value == "E" && *extra == "U"
        ));
    }

    #[test]
    fn a_constructor_parameter_that_redeclares_a_class_parameter_is_a_different_variable() {
        let owner = type_name("demo/Shadow");
        let signature = constructor_inference_signature(
            Some("<T:Ljava/lang/Object;>Ljava/lang/Object;"),
            owner,
            "(Ljava/lang/Object;)V",
            void_constructor(&["T"], &[Ty::ty_param("T", Ty::obj("kotlin/Any"))]),
        );

        assert_eq!(signature.formals.len(), 2);
        assert_eq!(signature.formals[0], "T");
        assert_ne!(signature.formals[1], "T");
        assert!(matches!(
            signature.ret.type_args(),
            [Ty::TyParam(name, _)] if *name == "T"
        ));
        assert!(matches!(
            signature.params.as_slice(),
            [Ty::TyParam(name, _)] if *name == signature.formals[1]
        ));
        assert_eq!(
            crate::types::type_parameter_source_name(&signature.formals[1]),
            "T"
        );
    }

    #[test]
    fn overloaded_constructors_do_not_share_a_shadowed_formal() {
        let shadow = |owner: &str, declaration: &str| {
            constructor_inference_signature(
                Some("<T:Ljava/lang/Object;>Ljava/lang/Object;"),
                type_name(owner),
                declaration,
                void_constructor(&["T"], &[Ty::ty_param("T", Ty::obj("kotlin/Any"))]),
            )
        };
        let left = shadow("demo/Left", "(I)V");
        let overload = shadow("demo/Left", "(Ljava/lang/String;)V");
        let other = shadow("demo/Right", "(I)V");

        assert_ne!(left.formals[1], overload.formals[1]);
        assert_ne!(left.formals[1], other.formals[1]);
        for formal in [&left.formals[1], &overload.formals[1], &other.formals[1]] {
            assert_eq!(crate::types::type_parameter_source_name(formal), "T");
        }
    }
}
