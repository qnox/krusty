//! Reading a JVM `Signature` attribute into Kotlin types.
//!
//! A classfile's erased descriptor loses every type argument, so a generic declaration's real shape
//! lives in its `Signature` attribute beside it. These turn that attribute into the `Ty` the symbol
//! source publishes — the field's own type, a type parameter's erasure, a generic return, a suspend
//! function's pre-CPS result, and a class's parameters with their bounds.

use super::*;
use crate::types::TyKind;
use crate::types::TypeParameters;

/// Decode the source type of a field while verifying that it erases to the classfile descriptor.
/// Unlike the constant-field helper, this retains type variables: the shared resolver substitutes
/// them from the applied receiver and falls back to `erased_ty` only for a raw receiver.
/// `class_bounds` are the declaring class's formals ([`JvmLibraries::member_class_formal_bounds`]).
pub(super) fn parse_field_gsig(
    signature: &str,
    erased_descriptor: &str,
    class_bounds: &crate::fx_hash::HashMap<String, Ty>,
) -> Option<(Ty, bool)> {
    if !matches!(signature.as_bytes().first(), Some(b'L' | b'T' | b'[')) {
        return None;
    }
    let mut parsed = parse_gsig_inner(signature, true)?;
    parsed.ty = crate::types::ty_with_param_bounds(parsed.ty, class_bounds);
    let erasure = field_type_parameter_erasure(signature, class_bounds)
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
/// unconditionally to `Object`. A hydrated bound carries its whole chain (`B extends A` is bounded by
/// `A`, itself bounded), so the erasure is the first bound in it that is not a variable.
fn field_type_parameter_erasure(
    signature: &str,
    class_bounds: &crate::fx_hash::HashMap<String, Ty>,
) -> Option<String> {
    let mut element = signature;
    let mut dimensions = 0;
    while let Some(rest) = element.strip_prefix('[') {
        dimensions += 1;
        element = rest;
    }
    let name = element.strip_prefix('T')?.strip_suffix(';')?;
    let mut bound = *class_bounds.get(name)?;
    while let TyKind::TyParam(_, inner)
    | TyKind::PlatformNullable(inner)
    | TyKind::Nullable(inner) = bound.kind()
    {
        bound = inner;
    }
    Some(format!(
        "{}{}",
        "[".repeat(dimensions),
        type_descriptor(bound)
    ))
}

pub(super) fn concrete_generic_ret(gsig: &GenericSig) -> Option<Ty> {
    match gsig.ret.kind() {
        TyKind::Obj(_, args) if !args.is_empty() && !has_free_ty_params(gsig.ret) => {
            // Canonicalize the recovered type to Kotlin form (`java/util/List<java/lang/Integer>` →
            // `kotlin/collections/List<kotlin/Int>`), so a member/`for`/extension keyed on the Kotlin
            // collection + a primitive element resolves and unboxes — mirroring the suspend path. Without
            // it, a classpath property `items: List<Int>` reads as raw `java/util/List<Integer>`:
            // `xs.sum()` is unresolved and `for (x in xs) { s += x }` compares `Int` vs `java/lang/Integer`.
            Some(canonicalize_jvm_collections(
                crate::symbol_resolver::ty_subst(gsig.ret, &crate::fx_hash::HashMap::default()),
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
    binds: &crate::fx_hash::HashMap<String, Ty>,
) -> Option<Ty> {
    match (*gsig.params.last()?).kind() {
        TyKind::Obj(n, args) if crate::types::same(n, crate::types::wk::continuation()) => {
            {
                let other = *args.first()?;
                match other.kind() {
                    // A bare class → its CANONICAL `Ty` (`kotlin/String` → `Ty::String`, `kotlin/Int` → `Ty::Int`,
                    // `kotlin/Unit` → `Ty::Unit`), so the recovered return unifies with the source-spelled type
                    // rather than a non-canonical `Obj("kotlin/String")`. A generic class (`List<Item>`) keeps its
                    // arguments via the general converter.
                    TyKind::Obj(name, args) if args.is_empty() => {
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
                    _ => Some(canonicalize_jvm_collections(
                        crate::symbol_resolver::ty_subst(other, binds),
                    )),
                }
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
        let TyKind::Fun(sig) = param.kind() else {
            continue;
        };
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
    let (formals, mut formal_bounds, mut s) = parse_formals(sig);
    hydrate_formal_bounds(
        &formals,
        &mut formal_bounds,
        &crate::fx_hash::HashMap::default(),
    );
    let mut supers = Vec::new();
    while !s.is_empty() {
        let (g, rest) = parse_gsig(s)?;
        supers.push(g);
        s = rest;
    }
    Some((formals, formal_bounds, supers))
}

/// The bound each occurrence of a formal carries: its first declared bound, or Java's implicit
/// `Any!`. `enclosing` holds the declaring class's formals; a formal declared here shadows one of the
/// same name there.
pub(super) fn formal_inline_bounds(
    formals: &[String],
    formal_bounds: &[Vec<Ty>],
    enclosing: &crate::fx_hash::HashMap<String, Ty>,
) -> crate::fx_hash::HashMap<String, Ty> {
    let mut inline_bounds = enclosing.clone();
    inline_bounds.extend(formals.iter().zip(formal_bounds).map(|(formal, bounds)| {
        (
            formal.clone(),
            bounds
                .first()
                .copied()
                .unwrap_or_else(|| Ty::platform_nullable(Ty::obj("kotlin/Any"))),
        )
    }));
    inline_bounds
}

/// Hydrate each declared bound through the complete chain of formals it names, the declaring
/// class's (`enclosing`) included, and return the bound each occurrence of a formal carries. In
/// `<A extends CharSequence, B extends A, C extends B>` a `C` occurrence is bounded by
/// `B(A(CharSequence!))`, not by a `B` whose `A` still has the parser's `Any` placeholder. A cycle
/// through a bound's own arguments (`T extends Comparable<T>`) keeps its first repeated occurrence.
pub(super) fn hydrate_formal_bounds(
    formals: &[String],
    formal_bounds: &mut [Vec<Ty>],
    enclosing: &crate::fx_hash::HashMap<String, Ty>,
) -> crate::fx_hash::HashMap<String, Ty> {
    let declared = formal_inline_bounds(formals, formal_bounds, enclosing);
    for bound in formal_bounds.iter_mut().flatten() {
        *bound = crate::types::ty_with_transitive_param_bounds(*bound, &declared);
    }
    formal_inline_bounds(formals, formal_bounds, enclosing)
}

/// The hydrated bound of each formal of a class generic signature, over the formals of the classes
/// that enclose it (`enclosing`), which a formal declared here shadows.
fn class_formal_bounds(
    class_signature: Option<&str>,
    enclosing: &crate::fx_hash::HashMap<String, Ty>,
) -> crate::fx_hash::HashMap<String, Ty> {
    let Some(signature) = class_signature else {
        return enclosing.clone();
    };
    let (formals, mut formal_bounds, _) = parse_formals(signature);
    hydrate_formal_bounds(&formals, &mut formal_bounds, enclosing)
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
    let mut rename = crate::fx_hash::HashMap::default();
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

/// A member class's type parameters: its `own`, then the formals of its `enclosing` class (that
/// class's own, then those it captures in turn), as the common classifier model lists an inner
/// class's: `Outer<String>.Inner` binds `T = String` in every `Inner` member that names `Outer`'s
/// `T`. A captured formal the member class hides behind its own of the same spelling keeps its
/// slot under an identity of its own, and every captured bound names it by that identity:
/// `Outer<T : Comparable<T>, U : T>.Inner<T>` keeps `Comparable<T>` and `U : T` on the captured
/// `T`, not on `Inner`'s.
fn capture_enclosing_type_parameters(
    name: TypeName,
    own: &TypeParameters<Vec<Vec<Ty>>>,
    enclosing: &TypeParameters<Vec<Vec<Ty>>>,
) -> TypeParameters<Vec<Vec<Ty>>> {
    let mut formals = own.type_params().clone();
    let mut hidden = crate::fx_hash::HashMap::<&str, &'static str>::default();
    for formal in enclosing.type_params() {
        if formals.contains(formal) {
            let identity = crate::types::hidden_captured_type_parameter(
                name,
                formals.len(),
                crate::types::type_parameter_source_name(formal),
            );
            hidden.insert(formal, identity);
            formals.push(identity.to_string());
        } else {
            formals.push(formal.clone());
        }
    }
    let captured_bounds = enclosing.type_param_bounds().iter().map(|bounds| {
        bounds
            .iter()
            .map(|bound| crate::types::ty_rename_params(*bound, &hidden))
            .collect::<Vec<_>>()
    });
    TypeParameters::new(
        formals,
        own.type_param_bounds()
            .iter()
            .cloned()
            .chain(captured_bounds)
            .collect(),
        [
            own.type_param_variances().as_slice(),
            enclosing.type_param_variances(),
        ]
        .concat(),
    )
}

impl JvmLibraries {
    /// The bound of every class formal a member of `class` may name without declaring it. A Java
    /// member's free type variables are its class's formals and, for a non-static member class
    /// (`Outer<T>.Inner`), those of each enclosing class, whose `Signature` alone declares them:
    /// `class Outer<T extends CharSequence>` makes `Inner.get()`'s `T` bounded by `CharSequence!`,
    /// not by the parser's `Any` placeholder. An inner declaration shadows an outer one.
    pub(super) fn member_class_formal_bounds(
        &self,
        class: &crate::jvm::classreader::ClassInfo,
    ) -> crate::fx_hash::HashMap<String, Ty> {
        let enclosing_of = |class: &crate::jvm::classreader::ClassInfo| {
            class
                .inner_class_self()
                .filter(|entry| entry.access & crate::jvm::classreader::ACC_STATIC == 0)
                .and_then(|entry| entry.outer.as_deref())
                .and_then(|outer| self.cp.find_name(type_name(outer)))
        };
        let mut enclosing = Vec::new();
        let mut current = enclosing_of(class);
        while let Some(outer) = current {
            current = enclosing_of(&outer);
            enclosing.push(outer);
        }
        let outer_bounds = enclosing
            .iter()
            .rev()
            .fold(crate::fx_hash::HashMap::default(), |bounds, outer| {
                class_formal_bounds(outer.signature.as_deref(), &bounds)
            });
        class_formal_bounds(class.signature.as_deref(), &outer_bounds)
    }

    /// The type parameters class `name` publishes, and how many of them it declares itself. A
    /// non-static member class (one with an `outer_instance`) captures its enclosing class's
    /// formals after its own ([`capture_enclosing_type_parameters`]); a static nested class
    /// captures none.
    pub(super) fn member_class_type_parameters(
        &self,
        outer_instance: Option<TypeName>,
        name: TypeName,
        own: TypeParameters<Vec<Vec<Ty>>>,
    ) -> (usize, TypeParameters<Vec<Vec<Ty>>>) {
        let own_count = own.type_params().len();
        match outer_instance.and_then(|outer| self.classifier_record(outer)) {
            Some(enclosing) => (
                own_count,
                capture_enclosing_type_parameters(name, &own, &enclosing.type_parameters),
            ),
            None => (own_count, own),
        }
    }

    /// Publish a Java method's `Signature` attribute: the declaring class's formal bounds
    /// (`class_bounds`), parameter and return nullability, then the constructor's class type
    /// parameters when `constructor` is set. The caller decides `<init>` at this provider boundary.
    pub(super) fn publish_java_member_generic_signature(
        &self,
        signature: Option<&str>,
        class_bounds: &crate::fx_hash::HashMap<String, Ty>,
        parameter_nullability: &[Option<JavaNullability>],
        return_nullability: Option<JavaNullability>,
        constructor: Option<(Option<&str>, TypeName, &str)>,
    ) -> Option<GenericSig> {
        let mut generic =
            signature.and_then(|signature| parse_member_method_gsig(signature, class_bounds))?;
        // A JVM generic signature still distinguishes a boxed Java reference from a Kotlin
        // primitive at this point (`java.lang.Integer` versus `Int`). Apply Java's type-use
        // flexibility while that declaration fact is available. Semanticizing a `FunctionN`
        // first would unbox its argument/result slots and lose the evidence needed to publish
        // `Int!` rather than rigid `Int`.
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
        generic = self.semanticize_jvm_generic_sig(generic);
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
mod class_formal_bound_tests {
    use super::*;
    use crate::fx_hash::HashMap;

    const BOX: &str = "<T::Ljava/lang/CharSequence;>Ljava/lang/Object;";

    fn char_sequence() -> Ty {
        Ty::platform_nullable(Ty::obj("kotlin/CharSequence"))
    }

    #[test]
    fn a_class_formal_occurrence_carries_the_declared_bound() {
        let bounds = class_formal_bounds(Some(BOX), &HashMap::default());
        let t = Ty::ty_param("T", char_sequence());
        let method =
            parse_member_method_gsig("(TT;)Ljava/util/List<TT;>;", &bounds).expect("member");

        assert_eq!(method.params, vec![t]);
        assert_eq!(method.ret.type_args(), [t]);
        assert_eq!(
            parse_member_method_gsig("()TT;", &bounds)
                .expect("member")
                .ret,
            t
        );
        assert_eq!(
            parse_field_gsig("TT;", "Ljava/lang/CharSequence;", &bounds),
            Some((t, true))
        );
    }

    #[test]
    fn a_method_formal_bounded_by_a_class_formal_carries_its_declared_bound() {
        let method = parse_member_method_gsig(
            "<U:TT;>()TU;",
            &class_formal_bounds(Some(BOX), &HashMap::default()),
        )
        .expect("member");
        let t = Ty::platform_nullable(Ty::ty_param("T", char_sequence()));

        assert_eq!(method.formal_bounds, vec![vec![t]]);
        assert_eq!(method.ret, Ty::ty_param("U", t));
    }

    #[test]
    fn a_chained_class_formal_carries_the_complete_declared_chain() {
        let bounds = class_formal_bounds(
            Some("<A::Ljava/lang/CharSequence;B:TA;C:TB;>Ljava/lang/Object;"),
            &HashMap::default(),
        );
        let a = Ty::ty_param("A", char_sequence());
        let b = Ty::ty_param("B", Ty::platform_nullable(a));
        let c = Ty::ty_param("C", Ty::platform_nullable(b));

        assert_eq!(
            parse_member_method_gsig("()TC;", &bounds)
                .expect("member")
                .ret,
            c
        );
        let method = parse_member_method_gsig("<D:TC;>()TD;", &bounds).expect("member");
        assert_eq!(method.formal_bounds, vec![vec![Ty::platform_nullable(c)]]);
    }

    #[test]
    fn an_inner_class_member_carries_its_enclosing_class_formal_bounds() {
        let outer = class_formal_bounds(Some(BOX), &HashMap::default());
        let t = Ty::ty_param("T", char_sequence());

        let inner = class_formal_bounds(None, &outer);
        assert_eq!(
            parse_member_method_gsig("()TT;", &inner)
                .expect("member")
                .ret,
            t
        );
        assert_eq!(
            parse_field_gsig("TT;", "Ljava/lang/CharSequence;", &inner),
            Some((t, true))
        );

        let generic_inner = class_formal_bounds(Some("<V:TT;>Ljava/lang/Object;"), &outer);
        assert_eq!(
            parse_member_method_gsig("()TV;", &generic_inner)
                .expect("member")
                .ret,
            Ty::ty_param("V", Ty::platform_nullable(t))
        );
        assert_eq!(
            parse_field_gsig("TV;", "Ljava/lang/CharSequence;", &generic_inner),
            Some((Ty::ty_param("V", Ty::platform_nullable(t)), true))
        );

        let shadowing =
            class_formal_bounds(Some("<T:Ljava/lang/Object;>Ljava/lang/Object;"), &outer);
        assert_eq!(
            parse_member_method_gsig("()TT;", &shadowing)
                .expect("member")
                .ret,
            Ty::ty_param("T", Ty::platform_nullable(Ty::obj("kotlin/Any")))
        );
    }

    #[test]
    fn a_hidden_captured_formal_keeps_its_self_and_cross_formal_bounds() {
        let comparable = |argument| Ty::obj_args("kotlin/Comparable", &[argument]);
        let outer_t = Ty::ty_param("T", Ty::obj("kotlin/Any"));
        let enclosing = TypeParameters::new(
            vec!["T".to_string(), "U".to_string()],
            vec![vec![comparable(outer_t)], vec![outer_t]],
            vec![crate::types::TypeVariance::Invariant; 2],
        );
        let own = TypeParameters::new(
            vec!["T".to_string()],
            vec![vec![Ty::obj("kotlin/Any")]],
            vec![crate::types::TypeVariance::Invariant],
        );
        let inner = crate::types::type_name("Outer$Inner");

        let captured = capture_enclosing_type_parameters(inner, &own, &enclosing);
        let hidden = crate::types::hidden_captured_type_parameter(inner, 1, "T");
        let hidden_t = Ty::ty_param(hidden, Ty::obj("kotlin/Any"));

        assert_eq!(captured.type_params(), &["T", hidden, "U"]);
        assert_eq!(
            captured.type_param_bounds(),
            &vec![
                vec![Ty::obj("kotlin/Any")],
                vec![comparable(hidden_t)],
                vec![hidden_t],
            ]
        );
        assert_eq!(crate::types::type_parameter_source_name(hidden), "T");
    }

    #[test]
    fn a_method_formal_shadows_the_class_formal_of_the_same_name() {
        let method = parse_member_method_gsig(
            "<T:Ljava/lang/Object;>(TT;)TT;",
            &class_formal_bounds(Some(BOX), &HashMap::default()),
        )
        .expect("member");
        let own = Ty::ty_param("T", Ty::platform_nullable(Ty::obj("kotlin/Any")));

        assert_eq!(method.params, vec![own]);
        assert_eq!(method.ret, own);
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
            [argument] if argument.ty_param_name() == Some("V")
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
            [argument] if argument.ty_param_name() == Some("E")
        ));
        assert!(matches!(
            signature.params.as_slice(),
            [value, extra]
                if value.ty_param_name() == Some("E") && extra.ty_param_name() == Some("U")
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
            [argument] if argument.ty_param_name() == Some("T")
        ));
        assert!(matches!(
            signature.params.as_slice(),
            [parameter] if parameter.ty_param_name() == Some(signature.formals[1].as_str())
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
