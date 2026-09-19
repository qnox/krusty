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
                    Some(kotlin_name_to_ty(to_kotlin_internal(&name.render())))
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
pub(super) fn parse_class_gsig(sig: &str) -> Option<(Vec<String>, Vec<Vec<Ty>>, Vec<Ty>)> {
    let (formals, formal_bounds, mut s) = parse_formals(sig);
    let mut supers = Vec::new();
    while !s.is_empty() {
        let (g, rest) = parse_gsig(s)?;
        supers.push(g);
        s = rest;
    }
    Some((formals, formal_bounds, supers))
}
