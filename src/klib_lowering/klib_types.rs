//! KLIB IR types as krusty's semantic types.
//!
//! A KLIB names a classifier by its exact public signature. That structural package/declaration
//! path is interned directly as the classifier's Kotlin identity, without rendering a qualified
//! spelling and parsing it back: a body's types and its selected declaration's types are one
//! model. A type argument converts as the provider converts a metadata type argument (a star is
//! the out-projection of `Any?`), and a `kotlin.FunctionN` type with its arguments is the function
//! type it denotes, a receiver function type when `@ExtensionFunctionType` marks it. A type
//! parameter is the semantic type parameter the declaration's provider record states for it
//! ([`KlibTypeParameters`]); a body whose declaration binds none of its type parameters declines
//! on the first use of one. Other type-use annotations carry no semantic type, as in metadata.
//! Definitely non-null, dynamic, error, suspend and context function types decline by form.

use crate::fx_hash::HashMap;

use super::decline::KlibBodyDeclineReason;
use crate::libraries::function_classifiers;
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrAnnotation, KlibIrArena, KlibIrNullability, KlibIrType, KlibIrTypeArgument, KlibIrTypeId,
    KlibIrVariance,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use crate::types::{builtin_semantic, type_name_child, wk, Ty, TyKind, TypeName};

/// The type parameters a body may name, each bound to the semantic type parameter its
/// declaration's provider record uses for it.
#[derive(Clone, Debug, Default)]
pub(super) struct KlibTypeParameters {
    bindings: HashMap<KlibIrSymbol, Ty>,
    bounds: HashMap<String, Vec<Ty>>,
}

impl KlibTypeParameters {
    /// Bind the serialized type parameter `symbol` to `semantic`. A repeated symbol is a malformed
    /// declaration.
    pub(super) fn bind(&mut self, symbol: &KlibIrSymbol, semantic: Ty) -> bool {
        self.bindings.insert(symbol.clone(), semantic).is_none()
    }

    pub(super) fn record_bounds(&mut self, formal: Ty, bounds: Vec<Ty>) -> bool {
        let TyKind::TyParam(identity, _) = formal.kind() else {
            return false;
        };
        self.bounds
            .insert(identity.as_str().to_owned(), bounds)
            .is_none()
    }

    pub(super) fn declared_bounds(&self, name: &str) -> Option<&[Ty]> {
        self.bounds.get(name).map(Vec::as_slice)
    }
}

/// A class application, with the same classifier and annotation normalization as closure planning.
fn class_type(
    arena: &KlibIrArena,
    parameters: &KlibTypeParameters,
    classifier: &KlibIrSymbol,
    arguments: &[KlibIrTypeArgument],
    annotations: &[KlibIrAnnotation],
    local_class: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
) -> Result<Ty, KlibBodyDeclineReason> {
    let (name, receiver) = class_head(classifier, annotations, local_class)?;
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
        return Err(mismatch(
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
pub(super) fn semantic_type(
    arena: &KlibIrArena,
    parameters: &KlibTypeParameters,
    id: KlibIrTypeId,
) -> Result<Ty, KlibBodyDeclineReason> {
    semantic_type_in(arena, parameters, id, &|_| Ok(None))
}

/// [`semantic_type`], also admitting a non-public class `local_class` resolves inside the library
/// whose body is being lowered. `Ok(None)` keeps the refusal of every other non-public classifier.
pub(super) fn semantic_type_in(
    arena: &KlibIrArena,
    parameters: &KlibTypeParameters,
    id: KlibIrTypeId,
    local_class: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
) -> Result<Ty, KlibBodyDeclineReason> {
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
            return Err(KlibBodyDeclineReason::UnsupportedType(
                "a definitely non-null type",
            ))
        }
        KlibIrType::Dynamic { .. } => {
            return Err(KlibBodyDeclineReason::UnsupportedType("the dynamic type"))
        }
        KlibIrType::Error { .. } => {
            return Err(KlibBodyDeclineReason::UnsupportedType("an error type"))
        }
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
                return Err(mismatch("a type parameter is applied to type arguments"));
            }
            parameters.bindings.get(classifier).copied().ok_or(
                KlibBodyDeclineReason::UnsupportedType(
                    "a type parameter its declaration does not bind",
                ),
            )?
        }
        _ => {
            return Err(KlibBodyDeclineReason::UnsupportedType(
                "a type whose classifier is not a class",
            ))
        }
    };
    Ok(match nullability {
        KlibIrNullability::MarkedNullable => Ty::nullable(semantic),
        // A class type the serializer marks definitely non-null is its ordinary non-null type; on
        // a type parameter whose bound admits null the mark is the intersection `T & Any`. A
        // parameter bounded by a non-null type is already non-null, and `T & Any` is `T` itself.
        KlibIrNullability::DefinitelyNotNull
            if semantic.is_ty_param() && semantic.upper_bound_admits_null() =>
        {
            Ty::from(TyKind::DefinitelyNotNull(semantic))
        }
        KlibIrNullability::NotSpecified | KlibIrNullability::DefinitelyNotNull => semantic,
    })
}

fn class_head(
    classifier: &KlibIrSymbol,
    annotations: &[KlibIrAnnotation],
    local_class: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
) -> Result<(TypeName, bool), KlibBodyDeclineReason> {
    let name = match &classifier.signature {
        KlibIrSignature::Public(signature) => {
            if signature.member_id().is_some() || signature.declaration().is_empty() {
                return Err(KlibBodyDeclineReason::UnsupportedType(
                    "a type whose classifier is not a class",
                ));
            }
            classifier_name(signature)?
        }
        signature => local_class(signature)?.ok_or(KlibBodyDeclineReason::UnsupportedType(
            "a type of a non-public class",
        ))?,
    };
    let mut receiver = false;
    for annotation in annotations {
        let Some(annotation) = annotation_classifier(annotation) else {
            continue;
        };
        if annotation == wk::extension_function_type() {
            receiver = true;
        } else if annotation == wk::context_function_type_params() {
            return Err(KlibBodyDeclineReason::UnsupportedType(
                "a function type with context parameters",
            ));
        }
    }
    Ok((name, receiver))
}

fn function_shape(
    name: TypeName,
    arguments: usize,
) -> Result<Option<function_classifiers::FunctionClassifier>, KlibBodyDeclineReason> {
    if let Some(function) = function_classifiers::classifier(name) {
        if !function.is_reflective() && arguments != function.arity() + 1 {
            return Err(mismatch(
                "a function type has another number of arguments than its arity",
            ));
        }
        return Ok(Some(function));
    }
    Ok(None)
}

/// The outer function-class shape, independent of the enclosing type-parameter bindings.
/// Full argument types are checked by `semantic_type` when the body is lowered.
pub(super) fn closure_type_arity(arena: &KlibIrArena, id: KlibIrTypeId) -> Option<usize> {
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
    let (name, _) = class_head(classifier, annotations, &|_| Ok(None)).ok()?;
    let function = function_shape(name, arguments.len()).ok()??;
    // A suspend lambda's implementation also takes the continuation, which closure planning does
    // not model yet.
    (!function.is_reflective() && !function.is_suspend()).then(|| function.arity())
}

/// One type argument, as the provider converts a metadata type argument.
fn type_argument(
    arena: &KlibIrArena,
    parameters: &KlibTypeParameters,
    argument: KlibIrTypeArgument,
    local_class: &dyn Fn(&KlibIrSignature) -> Result<Option<TypeName>, KlibBodyDeclineReason>,
) -> Result<Ty, KlibBodyDeclineReason> {
    Ok(match argument {
        KlibIrTypeArgument::Star => Ty::out_projection(Ty::nullable(Ty::obj_name(wk::any()))),
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
fn annotation_classifier(annotation: &KlibIrAnnotation) -> Option<TypeName> {
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

/// The arity of the invocation the public declaration `member` is, when the function classifier
/// that owns it declares it with the Kotlin function invocation role. The member is joined to the
/// builtins provider's declaration of its exact classifier by its serialized name; that
/// declaration's role, not the name, decides. Suspend and reflective classifiers are not invoked
/// here.
pub(super) fn function_invocation_arity(member: &KlibPublicIdSignature) -> Option<usize> {
    invocation_arity(member, false)
}

/// A reflective function's inherited invocation, authenticated against the same builtins
/// declaration as its ordinary function supertype. This grants no reflection-operation support.
pub(super) fn reference_invocation_arity(member: &KlibPublicIdSignature) -> Option<usize> {
    invocation_arity(member, true)
}

fn invocation_arity(member: &KlibPublicIdSignature, reflective: bool) -> Option<usize> {
    let (name, class) = member.declaration().segments().split_last()?;
    let owner = declaration_name(member.package().segments(), class).ok()?;
    let function = function_classifiers::classifier(owner)?;
    if function.is_suspend() || function.is_reflective() != reflective {
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

/// Intern a classifier identity one signature segment at a time. Package segments are namespace
/// children; declaration segments after the first are nested classifiers. No qualified spelling is
/// rendered and parsed back into the name tree.
pub(super) fn classifier_name(
    signature: &KlibPublicIdSignature,
) -> Result<TypeName, KlibBodyDeclineReason> {
    declaration_name(
        signature.package().segments(),
        signature.declaration().segments(),
    )
}

/// The declaring container and own segment of the classifier `signature` names.
pub(super) fn classifier_declaration(
    signature: &KlibPublicIdSignature,
) -> Result<crate::types::ClassifierDeclarationName, KlibBodyDeclineReason> {
    let segments = signature.declaration().segments();
    let (name, owner) = segments
        .split_last()
        .ok_or(KlibBodyDeclineReason::UnsupportedType(
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
pub(super) fn member_owner(
    member: &KlibPublicIdSignature,
) -> Result<TypeName, KlibBodyDeclineReason> {
    let (_, class) = member.declaration().segments().split_last().ok_or(
        KlibBodyDeclineReason::UnsupportedType("a member without a declaration path"),
    )?;
    declaration_name(member.package().segments(), class)
}

fn declaration_name(
    package: &[String],
    declaration: &[String],
) -> Result<TypeName, KlibBodyDeclineReason> {
    let valid = |segment: &String| !segment.is_empty() && !segment.contains('/');
    if !package.iter().all(valid) || !declaration.iter().all(valid) {
        return Err(KlibBodyDeclineReason::UnsupportedType(
            "a class with an invalid identity path",
        ));
    }
    let package = package.iter().fold(TypeName::ROOT, |parent, segment| {
        type_name_child(parent, segment)
    });
    let (first, nested) =
        declaration
            .split_first()
            .ok_or(KlibBodyDeclineReason::UnsupportedType(
                "a type whose classifier is not a class",
            ))?;
    Ok(nested
        .iter()
        .fold(type_name_child(package, first), |name, segment| {
            name.nested_child(segment)
        }))
}

fn mismatch(detail: &str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::SignatureMismatch(detail.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::klib_ir::KlibIrSignature;

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
}
