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
use crate::types::{builtin_semantic, type_name_child, wk, Ty, TypeName};

/// The type parameters a body may name, each bound to the semantic type parameter its
/// declaration's provider record uses for it.
#[derive(Clone, Debug, Default)]
pub(super) struct KlibTypeParameters(HashMap<KlibIrSymbol, Ty>);

impl KlibTypeParameters {
    /// Bind the serialized type parameter `symbol` to `semantic`. A repeated symbol is a malformed
    /// declaration.
    pub(super) fn bind(&mut self, symbol: &KlibIrSymbol, semantic: Ty) -> bool {
        self.0.insert(symbol.clone(), semantic).is_none()
    }
}

/// The semantic type of the KLIB type `id`, naming only the type parameters of `parameters`.
pub(super) fn semantic_type(
    arena: &KlibIrArena,
    parameters: &KlibTypeParameters,
    id: KlibIrTypeId,
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
    let semantic =
        match classifier.kind {
            KlibIrSymbolKind::Class => {
                class_type(arena, parameters, classifier, arguments, annotations)?
            }
            KlibIrSymbolKind::TypeParameter => {
                if !arguments.is_empty() {
                    return Err(mismatch("a type parameter is applied to type arguments"));
                }
                parameters.0.get(classifier).copied().ok_or(
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
        // a type parameter the mark is the intersection `T & Any`.
        KlibIrNullability::DefinitelyNotNull if matches!(semantic, Ty::TyParam(..)) => {
            Ty::DefinitelyNotNull(crate::types::intern_ty(semantic))
        }
        KlibIrNullability::NotSpecified | KlibIrNullability::DefinitelyNotNull => semantic,
    })
}

/// A class type with its arguments: a `kotlin.FunctionN` application is the function type it
/// denotes, any other class the classifier applied to its converted arguments.
fn class_type(
    arena: &KlibIrArena,
    parameters: &KlibTypeParameters,
    classifier: &KlibIrSymbol,
    arguments: &[KlibIrTypeArgument],
    annotations: &[KlibIrAnnotation],
) -> Result<Ty, KlibBodyDeclineReason> {
    let KlibIrSignature::Public(signature) = &classifier.signature else {
        return Err(KlibBodyDeclineReason::UnsupportedType(
            "a type of a non-public class",
        ));
    };
    if signature.member_id().is_some() || signature.declaration().is_empty() {
        return Err(KlibBodyDeclineReason::UnsupportedType(
            "a type whose classifier is not a class",
        ));
    }
    let name = classifier_name(signature)?;
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
    let mut arguments = arguments
        .iter()
        .map(|argument| type_argument(arena, parameters, *argument))
        .collect::<Result<Vec<_>, _>>()?;
    if let Some(function) = function_classifiers::classifier(name) {
        if function.is_suspend() {
            return Err(KlibBodyDeclineReason::UnsupportedType(
                "a suspend function type",
            ));
        }
        if !function.is_reflective() {
            if arguments.len() != function.arity() + 1 {
                return Err(mismatch(
                    "a function type has another number of arguments than its arity",
                ));
            }
            let ret = arguments.pop().expect("a function type has its result");
            let receiver = receiver && !arguments.is_empty();
            return Ok(Ty::fun_with_shape(arguments, ret, 0, receiver, false));
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

/// One type argument, as the provider converts a metadata type argument.
fn type_argument(
    arena: &KlibIrArena,
    parameters: &KlibTypeParameters,
    argument: KlibIrTypeArgument,
) -> Result<Ty, KlibBodyDeclineReason> {
    Ok(match argument {
        KlibIrTypeArgument::Star => Ty::out_projection(Ty::nullable(Ty::obj_name(wk::any()))),
        KlibIrTypeArgument::Type { variance, ty } => {
            let ty = semantic_type(arena, parameters, ty)?;
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
    let (name, class) = member.declaration().segments().split_last()?;
    let owner = declaration_name(member.package().segments(), class).ok()?;
    let function = function_classifiers::classifier(owner)?;
    if function.is_suspend() || function.is_reflective() {
        return None;
    }
    let declaration = function_classifiers::synthetic(function);
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
