//! Serialized KLIB declaration types as common semantic types.
//!
//! A KLIB names a classifier by its exact public signature. That structural package/declaration
//! path is interned directly as the classifier's Kotlin identity, without rendering a qualified
//! spelling and parsing it back: a body's types and its selected declaration's types are one
//! model. A type argument converts as the provider converts a metadata type argument (a star keeps
//! its star-projection identity, with an `Any?` placeholder refined at the declaring classifier),
//! and a `kotlin.FunctionN` type with its arguments is the function
//! type it denotes, a receiver function type when `@ExtensionFunctionType` marks it. A type
//! parameter is the semantic type parameter the declaration's provider record states for it
//! ([`KlibTypeParameters`]); a body whose declaration binds none of its type parameters declines
//! on the first use of one. Other type-use annotations carry no semantic type, as in metadata.
//! A `kotlin.coroutines.SuspendFunctionN` type is the suspend function type it denotes. Dynamic,
//! error and context function types decline by form.

use crate::fx_hash::HashMap;

use crate::libraries::function_classifiers;
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrAnnotation, KlibIrArena, KlibIrNullability, KlibIrType, KlibIrTypeArgument, KlibIrTypeId,
    KlibIrVariance,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use crate::types::{builtin_semantic, type_name_child, wk, Ty, TyKind, TypeName};

/// A declaration type is malformed or outside the shared serialized type model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum KlibTypeError {
    UnsupportedType(&'static str),
    SignatureMismatch(String),
}

/// Declaration-owned type bindings shared by provider header normalization and body checking.
#[derive(Clone, Debug, Default)]
pub(crate) struct KlibTypeParameters {
    bindings: HashMap<KlibIrSymbol, Ty>,
    bounds: HashMap<String, Vec<Ty>>,
    /// Each class formal's declared variance. A function formal is always invariant.
    variances: HashMap<String, crate::types::TypeVariance>,
    /// The non-public classes of the declaring library its signature names, each by the identity
    /// that library's bodies give it.
    classifiers: HashMap<KlibIrSignature, TypeName>,
}

impl KlibTypeParameters {
    /// Bind the serialized type parameter `symbol` to `semantic`. A repeated symbol is a malformed
    /// declaration.
    pub(crate) fn bind(&mut self, symbol: &KlibIrSymbol, semantic: Ty) -> bool {
        self.bindings.insert(symbol.clone(), semantic).is_none()
    }

    /// Whether the serialized type parameter `symbol` is bound.
    pub(crate) fn binds(&self, symbol: &KlibIrSymbol) -> bool {
        self.bindings.contains_key(symbol)
    }

    pub(crate) fn record_bounds(&mut self, formal: Ty, bounds: Vec<Ty>) -> bool {
        let TyKind::TyParam(identity, _) = formal.kind() else {
            return false;
        };
        self.bounds
            .insert(identity.as_str().to_owned(), bounds)
            .is_none()
    }

    /// The semantic type parameter the serialized `symbol` is bound to.
    pub(crate) fn bound(&self, symbol: &KlibIrSymbol) -> Option<Ty> {
        self.bindings.get(symbol).copied()
    }

    pub(crate) fn declared_bounds(&self, name: &str) -> Option<&[Ty]> {
        self.bounds.get(name).map(Vec::as_slice)
    }

    /// Record the declared variance of the class formal `formal`.
    pub(crate) fn record_variance(
        &mut self,
        formal: Ty,
        variance: crate::types::TypeVariance,
    ) -> bool {
        let TyKind::TyParam(identity, _) = formal.kind() else {
            return false;
        };
        self.variances
            .insert(identity.as_str().to_owned(), variance)
            .is_none()
    }

    /// Name the non-public classifier `signature` as `name`. A repeated signature is a malformed
    /// declaration.
    pub(crate) fn bind_classifier(&mut self, signature: &KlibIrSignature, name: TypeName) -> bool {
        self.classifiers.insert(signature.clone(), name).is_none()
    }

    /// The declared variance of the formal `name`: a class formal's recorded one, otherwise the
    /// invariance every function formal has.
    pub(crate) fn declared_variance(&self, name: &str) -> crate::types::TypeVariance {
        self.variances
            .get(name)
            .copied()
            .unwrap_or(crate::types::TypeVariance::Invariant)
    }
}

/// A class application, with the same classifier and annotation normalization as closure planning.
fn class_type<E: From<KlibTypeError>>(
    arena: &KlibIrArena,
    parameters: &KlibTypeParameters,
    classifier: &KlibIrSymbol,
    arguments: &[KlibIrTypeArgument],
    annotations: &[KlibIrAnnotation],
    local_class: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, E>,
) -> Result<Ty, E> {
    let (name, receiver) = class_head(parameters, classifier, annotations, local_class)?;
    let mut arguments = arguments
        .iter()
        .map(|argument| type_argument(arena, parameters, *argument, local_class))
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(function) = function_shape(name, arguments.len())? {
        if !function.is_reflective() {
            let ret = arguments.pop().expect("a function type has its result");
            let receiver = receiver && !arguments.is_empty();
            return Ok(Ty::fun_with_shape(
                arguments,
                ret,
                0,
                receiver,
                function.is_suspend(),
            ));
        }
    }
    if receiver {
        return Err(mismatch::<E>(
            "a type other than a function type is marked `@ExtensionFunctionType`",
        ));
    }
    if arguments.is_empty() {
        if let Some(builtin) = builtin_semantic(name) {
            return Ok(builtin);
        }
    }
    Ok(Ty::obj_args_name(name, &arguments))
}

/// The semantic type of the KLIB type `id`, naming only the type parameters of `parameters`.
pub(crate) fn semantic_type(
    arena: &KlibIrArena,
    parameters: &KlibTypeParameters,
    id: KlibIrTypeId,
) -> Result<Ty, KlibTypeError> {
    semantic_type_in(arena, parameters, id, &|_| Ok(None))
}

/// [`semantic_type`], also admitting a non-public class `local_class` resolves inside the library
/// whose declaration is being normalized. `Ok(None)` refuses every other non-public classifier.
pub(crate) fn semantic_type_in<E: From<KlibTypeError>>(
    arena: &KlibIrArena,
    parameters: &KlibTypeParameters,
    id: KlibIrTypeId,
    local_class: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, E>,
) -> Result<Ty, E> {
    let (classifier, nullability, arguments, annotations) = match arena.ty(id) {
        KlibIrType::Simple {
            classifier,
            nullability,
            arguments,
            annotations,
            // The alias a type was written as; the type itself is its expansion.
            abbreviation: _,
        } => (classifier, nullability, arguments, annotations),
        KlibIrType::DefinitelyNotNull(_) => {
            return Err(unsupported::<E>("a definitely non-null type"))
        }
        KlibIrType::Dynamic { .. } => return Err(unsupported::<E>("the dynamic type")),
        KlibIrType::Error { .. } => return Err(unsupported::<E>("an error type")),
    };
    let semantic = match classifier.kind {
        KlibIrSymbolKind::Class => class_type(
            arena,
            parameters,
            classifier,
            arguments,
            annotations,
            local_class,
        )?,
        KlibIrSymbolKind::TypeParameter => {
            if !arguments.is_empty() {
                return Err(mismatch::<E>(
                    "a type parameter is applied to type arguments",
                ));
            }
            parameters
                .bindings
                .get(classifier)
                .copied()
                .ok_or(unsupported::<E>(
                    "a type parameter its declaration does not bind",
                ))?
        }
        _ => return Err(unsupported::<E>("a type whose classifier is not a class")),
    };
    Ok(match nullability {
        KlibIrNullability::MarkedNullable => Ty::nullable(semantic),
        // A class type the serializer marks definitely non-null is its ordinary non-null type; on
        // a type parameter whose bound admits null the mark is the intersection `T & Any`. A
        // parameter bounded by a non-null type is already non-null, and `T & Any` is `T` itself.
        KlibIrNullability::DefinitelyNotNull => semantic.marked_definitely_not_null(),
        KlibIrNullability::NotSpecified => semantic,
    })
}

fn class_head<E: From<KlibTypeError>>(
    parameters: &KlibTypeParameters,
    classifier: &KlibIrSymbol,
    annotations: &[KlibIrAnnotation],
    local_class: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, E>,
) -> Result<(TypeName, bool), E> {
    let name = match &classifier.signature {
        KlibIrSignature::Public(signature) => {
            if signature.member_id().is_some() || signature.declaration().is_empty() {
                return Err(unsupported::<E>("a type whose classifier is not a class"));
            }
            classifier_name(signature)?
        }
        signature => match parameters.classifiers.get(signature) {
            Some(&name) => name,
            None => {
                local_class(signature)?.ok_or(unsupported::<E>("a type of a non-public class"))?
            }
        },
    };
    let mut receiver = false;
    for annotation in annotations {
        let Some(annotation) = annotation_classifier(annotation) else {
            continue;
        };
        if annotation == wk::extension_function_type() {
            receiver = true;
        } else if annotation == wk::context_function_type_params() {
            return Err(unsupported::<E>("a function type with context parameters"));
        }
    }
    Ok((name, receiver))
}

fn function_shape(
    name: TypeName,
    arguments: usize,
) -> Result<Option<function_classifiers::FunctionClassifier>, KlibTypeError> {
    if let Some(function) = function_classifiers::classifier(name) {
        if !function.is_reflective() && arguments != function.arity() + 1 {
            return Err(mismatch::<KlibTypeError>(
                "a function type has another number of arguments than its arity",
            ));
        }
        return Ok(Some(function));
    }
    Ok(None)
}

/// The source arity of a closure of the serialized function type `id`, and whether that type is a
/// suspend function type, whose invocation also takes the continuation after those parameters.
/// The outer shape is independent of the enclosing type-parameter bindings; full argument types are
/// checked by `semantic_type` when the body is lowered.
pub(crate) fn closure_type_arity(arena: &KlibIrArena, id: KlibIrTypeId) -> Option<(usize, bool)> {
    let KlibIrType::Simple {
        classifier,
        nullability,
        arguments,
        annotations,
        ..
    } = arena.ty(id)
    else {
        return None;
    };
    if classifier.kind != KlibIrSymbolKind::Class
        || *nullability == KlibIrNullability::MarkedNullable
    {
        return None;
    }
    let (name, _) = class_head(
        &KlibTypeParameters::default(),
        classifier,
        annotations,
        &|_| Ok::<_, KlibTypeError>(None),
    )
    .ok()?;
    let function = function_shape(name, arguments.len()).ok()??;
    (!function.is_reflective()).then(|| (function.arity(), function.is_suspend()))
}

/// One type argument, as the provider converts a metadata type argument.
fn type_argument<E: From<KlibTypeError>>(
    arena: &KlibIrArena,
    parameters: &KlibTypeParameters,
    argument: KlibIrTypeArgument,
    local_class: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, E>,
) -> Result<Ty, E> {
    Ok(match argument {
        KlibIrTypeArgument::Star => Ty::star_projection(Ty::nullable(Ty::obj_name(wk::any()))),
        KlibIrTypeArgument::Type { variance, ty } => {
            let ty = semantic_type_in(arena, parameters, ty, local_class)?;
            match variance {
                KlibIrVariance::Invariant => ty,
                KlibIrVariance::In => Ty::in_projection(ty),
                KlibIrVariance::Out => Ty::out_projection(ty),
            }
        }
    })
}

/// The classifier a type-use annotation constructs, when its constructor is a public declaration
/// with a valid identity path.
pub(crate) fn annotation_classifier(annotation: &KlibIrAnnotation) -> Option<TypeName> {
    let symbol = &annotation.access.symbol;
    if symbol.kind != KlibIrSymbolKind::Constructor {
        return None;
    }
    let KlibIrSignature::Public(constructor) = &symbol.signature else {
        return None;
    };
    let (_, class) = constructor.declaration().segments().split_last()?;
    declaration_name(constructor.package().segments(), class).ok()
}

/// The source arity of an ordinary invocation the public declaration `member` is, when its
/// function classifier declares the Kotlin function invocation role. The member is joined to the
/// compiler-owned declaration of its exact classifier; its role, not its name, decides.
pub(crate) fn function_invocation_arity(member: &KlibPublicIdSignature) -> Option<usize> {
    invocation_arity(member, false, false)
}

/// The source arity of a declared `SuspendFunctionN.invoke`. Common CPS lowering supplies the
/// physical trailing continuation after this semantic call has been recorded.
pub(crate) fn suspend_function_invocation_arity(member: &KlibPublicIdSignature) -> Option<usize> {
    invocation_arity(member, false, true)
}

/// A reflective function's inherited invocation, authenticated against the same builtins
/// declaration as its ordinary function supertype. This grants no reflection-operation support.
pub(crate) fn reference_invocation_arity(member: &KlibPublicIdSignature) -> Option<usize> {
    invocation_arity(member, true, false)
}

/// Whether `member` is declared by a builtin function classifier (`FunctionN`, `KFunctionN`,
/// `SuspendFunctionN` and their reflective forms). Such a reference is a function value, never a
/// lambda converted to a functional interface.
pub(crate) fn is_function_classifier_member(member: &KlibPublicIdSignature) -> bool {
    let Some((_, class)) = member.declaration().segments().split_last() else {
        return false;
    };
    declaration_name(member.package().segments(), class)
        .is_ok_and(|owner| function_classifiers::classifier(owner).is_some())
}

fn invocation_arity(
    member: &KlibPublicIdSignature,
    reflective: bool,
    suspend: bool,
) -> Option<usize> {
    let (name, class) = member.declaration().segments().split_last()?;
    let owner = declaration_name(member.package().segments(), class).ok()?;
    let function = function_classifiers::classifier(owner)?;
    if function.is_suspend() != suspend || function.is_reflective() != reflective {
        return None;
    }
    let declaration = function_classifiers::synthetic(function.invocation_classifier());
    let (functions, _) = declaration
        .declared_callables
        .get(name)?
        .clone()
        .into_parts();
    let [declared] = functions.overloads.as_slice() else {
        return None;
    };
    (declared.callable.semantic_role == Some(crate::types::SemanticCallRole::KotlinFunctionInvoke))
        .then(|| declared.semantic_params().len())
}

/// Intern a package identity one signature segment at a time.
pub(crate) fn package_name(package: &[String]) -> TypeName {
    package.iter().fold(TypeName::ROOT, |parent, segment| {
        type_name_child(parent, segment)
    })
}

/// Intern a classifier identity one signature segment at a time. Package segments are namespace
/// children; declaration segments after the first are nested classifiers. No qualified spelling is
/// rendered and parsed back into the name tree.
pub(crate) fn classifier_name(
    signature: &KlibPublicIdSignature,
) -> Result<TypeName, KlibTypeError> {
    declaration_name(
        signature.package().segments(),
        signature.declaration().segments(),
    )
}

/// The declaring container and own segment of the classifier `signature` names.
pub(crate) fn classifier_declaration(
    signature: &KlibPublicIdSignature,
) -> Result<crate::types::ClassifierDeclarationName, KlibTypeError> {
    let segments = signature.declaration().segments();
    let (name, owner) = segments.split_last().ok_or(KlibTypeError::UnsupportedType(
        "a type whose classifier is not a class",
    ))?;
    let name = name.as_str().into();
    Ok(if owner.is_empty() {
        let package = signature
            .package()
            .segments()
            .iter()
            .fold(TypeName::ROOT, |parent, segment| {
                type_name_child(parent, segment)
            });
        crate::types::ClassifierDeclarationName::TopLevel { package, name }
    } else {
        crate::types::ClassifierDeclarationName::Nested {
            owner: declaration_name(signature.package().segments(), owner)?,
            name,
        }
    })
}

/// The classifier that declares the member `member`: its signature's declaration path without the
/// member's own segment.
pub(crate) fn member_owner(member: &KlibPublicIdSignature) -> Result<TypeName, KlibTypeError> {
    let (_, class) =
        member
            .declaration()
            .segments()
            .split_last()
            .ok_or(KlibTypeError::UnsupportedType(
                "a member without a declaration path",
            ))?;
    declaration_name(member.package().segments(), class)
}

fn declaration_name(package: &[String], declaration: &[String]) -> Result<TypeName, KlibTypeError> {
    let valid = |segment: &String| !segment.is_empty() && !segment.contains('/');
    if !package.iter().all(valid) || !declaration.iter().all(valid) {
        return Err(KlibTypeError::UnsupportedType(
            "a class with an invalid identity path",
        ));
    }
    let package = package.iter().fold(TypeName::ROOT, |parent, segment| {
        type_name_child(parent, segment)
    });
    let (first, nested) = declaration
        .split_first()
        .ok_or(KlibTypeError::UnsupportedType(
            "a type whose classifier is not a class",
        ))?;
    Ok(nested
        .iter()
        .fold(type_name_child(package, first), |name, segment| {
            name.nested_child(segment)
        }))
}

fn unsupported<E: From<KlibTypeError>>(detail: &'static str) -> E {
    KlibTypeError::UnsupportedType(detail).into()
}

fn mismatch<E: From<KlibTypeError>>(detail: &str) -> E {
    KlibTypeError::SignatureMismatch(detail.to_owned()).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::klib_ir::KlibIrSignature;

    #[test]
    fn serialized_star_and_explicit_out_agree_with_their_selected_metadata_headers() {
        use crate::metadata::semantic::{semantic_ty, KotlinType};
        let mut arena = KlibIrArena::default();
        let application = |arena: &mut KlibIrArena, argument| {
            arena.push_type(KlibIrType::Simple {
                classifier: KlibIrSymbol {
                    kind: KlibIrSymbolKind::Class,
                    signature: KlibIrSignature::Public(KlibPublicIdSignature::classifier(
                        crate::types::type_name("sample/Box"),
                    )),
                },
                nullability: KlibIrNullability::NotSpecified,
                arguments: vec![argument],
                annotations: Vec::new(),
                abbreviation: None,
            })
        };
        let star = application(&mut arena, KlibIrTypeArgument::Star);
        let any = arena.push_type(KlibIrType::Simple {
            classifier: KlibIrSymbol {
                kind: KlibIrSymbolKind::Class,
                signature: KlibIrSignature::Public(KlibPublicIdSignature::classifier(wk::any())),
            },
            nullability: KlibIrNullability::MarkedNullable,
            arguments: Vec::new(),
            annotations: Vec::new(),
            abbreviation: None,
        });
        let explicit_out = application(
            &mut arena,
            KlibIrTypeArgument::Type {
                variance: KlibIrVariance::Out,
                ty: any,
            },
        );
        let metadata = |argument| KotlinType::Class {
            internal: "sample/Box".to_owned(),
            args: vec![argument],
            nullable: false,
            shape: Default::default(),
        };
        let selected_star = semantic_ty(&metadata(KotlinType::Star), &HashMap::default());
        let selected_out = semantic_ty(
            &metadata(KotlinType::OutProjection(Box::new(KotlinType::Class {
                internal: "kotlin/Any".to_owned(),
                args: Vec::new(),
                nullable: true,
                shape: Default::default(),
            }))),
            &HashMap::default(),
        );
        let parameters = KlibTypeParameters::default();
        // Raw and prepared function headers require exact agreement with the selected metadata;
        // neither boundary may erase the distinction to make the header compare successfully.
        assert_eq!(semantic_type(&arena, &parameters, star), Ok(selected_star));
        assert_eq!(
            semantic_type(&arena, &parameters, explicit_out),
            Ok(selected_out)
        );
        assert_ne!(selected_star, selected_out);
    }

    /// `T & Any` as kotlinc serializes it: the type parameter `T` marked definitely non-null.
    fn definitely_non_null(bound: Ty) -> Ty {
        let symbol = KlibIrSymbol {
            kind: KlibIrSymbolKind::TypeParameter,
            signature: KlibIrSignature::FileLocal { file: 0, slot: 0 },
        };
        let mut parameters = KlibTypeParameters::default();
        assert!(parameters.bind(&symbol, Ty::ty_param("T", bound)));
        let mut arena = KlibIrArena::default();
        let id = arena.push_type(KlibIrType::Simple {
            classifier: symbol,
            nullability: KlibIrNullability::DefinitelyNotNull,
            arguments: Vec::new(),
            annotations: Vec::new(),
            abbreviation: None,
        });
        semantic_type(&arena, &parameters, id).expect("a type parameter type")
    }

    /// kotlinc serializes `listOf(element)` in `fun <T : Any> listOfNotNull(element: T?)` at the
    /// type argument `T & Any`, and types the call `List<T>`: for a parameter bounded by `Any`,
    /// `T & Any` is `T`.
    #[test]
    fn a_definitely_non_null_parameter_bounded_by_a_non_null_type_is_the_parameter() {
        let any = Ty::obj("kotlin/Any");
        assert_eq!(definitely_non_null(any), Ty::ty_param("T", any));
        let nullable_any = Ty::nullable(any);
        assert_eq!(
            definitely_non_null(nullable_any),
            Ty::from(TyKind::DefinitelyNotNull(Ty::ty_param("T", nullable_any)))
        );
    }

    /// A class type `classifier<arguments>` as kotlinc serializes it.
    fn class_application(
        arena: &mut KlibIrArena,
        classifier: &str,
        arguments: &[KlibIrTypeId],
    ) -> KlibIrTypeId {
        arena.push_type(KlibIrType::Simple {
            classifier: KlibIrSymbol {
                kind: KlibIrSymbolKind::Class,
                signature: KlibIrSignature::Public(KlibPublicIdSignature::classifier(
                    crate::types::type_name(classifier),
                )),
            },
            nullability: KlibIrNullability::NotSpecified,
            arguments: arguments
                .iter()
                .map(|&ty| KlibIrTypeArgument::Type {
                    variance: KlibIrVariance::Invariant,
                    ty,
                })
                .collect(),
            annotations: Vec::new(),
            abbreviation: None,
        })
    }

    /// kotlinc serializes the receiver of `startCoroutine`, `suspend () -> T`, as
    /// `kotlin.coroutines.SuspendFunction0<T>`: the suspend function type checked source gives
    /// the same declaration. Closure planning reads its source arity and that it suspends.
    #[test]
    fn a_suspend_function_classifier_is_the_suspend_function_type() {
        let mut arena = KlibIrArena::default();
        let int = class_application(&mut arena, "kotlin/Int", &[]);
        let string = class_application(&mut arena, "kotlin/String", &[]);
        let suspend = class_application(
            &mut arena,
            "kotlin/coroutines/SuspendFunction1",
            &[int, string],
        );
        let plain = class_application(&mut arena, "kotlin/Function1", &[int, string]);
        let parameters = KlibTypeParameters::default();
        let int_ty = semantic_type(&arena, &parameters, int).expect("Int");
        let string_ty = semantic_type(&arena, &parameters, string).expect("String");
        assert_eq!(
            semantic_type(&arena, &parameters, suspend).expect("a suspend function type"),
            Ty::fun_with_shape(vec![int_ty], string_ty, 0, false, true)
        );
        assert_eq!(
            semantic_type(&arena, &parameters, plain).expect("a function type"),
            Ty::fun_with_shape(vec![int_ty], string_ty, 0, false, false)
        );
        assert_eq!(closure_type_arity(&arena, suspend), Some((1, true)));
        assert_eq!(closure_type_arity(&arena, plain), Some((1, false)));
    }
}
