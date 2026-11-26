//! The mangled signature of the function a local class is declared in.
//!
//! kotlinc mangles a declaration of a local class from its parents outward. A class contributes no
//! text of its own to a signature mangle, but the function that encloses it contributes its whole
//! mangled signature, so the id of a fake override of a local class begins with that text.

use std::borrow::Cow;

use super::KlibBodyDeclineReason;
use crate::metadata::id_signature::{
    callable_mangled, enclosing_callable_mangled, mangled_id, CallableShape, DeclarationContainer,
    ManglingError, Placement, SignatureType, TypeParameterShape, TypeView,
};
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrFunction, KlibIrNullability, KlibIrType, KlibIrTypeArgument, KlibIrTypeId,
    KlibIrVariance,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbolKind};

/// A serialized type, in the form the signature mangler reads.
enum MangledType {
    Class {
        fq_name: String,
        arguments: Vec<MangledType>,
        nullable: bool,
    },
    Parameter {
        index: u32,
        nullable: bool,
    },
    In(Box<MangledType>),
    Out(Box<MangledType>),
    Star,
}

impl SignatureType for MangledType {
    /// The position of the parameter among its function's type parameters.
    type ParameterId = u32;

    fn view(&self) -> Result<TypeView<'_, Self>, ManglingError> {
        Ok(match self {
            MangledType::Class {
                fq_name,
                arguments,
                nullable,
            } => TypeView::Class {
                fq_name: Cow::Borrowed(fq_name),
                arguments: arguments.iter().collect(),
                nullable: *nullable,
            },
            MangledType::Parameter { index, nullable } => TypeView::Parameter {
                id: *index,
                nullable: *nullable,
            },
            MangledType::In(inner) => TypeView::In(inner),
            MangledType::Out(inner) => TypeView::Out(inner),
            MangledType::Star => TypeView::Star,
        })
    }
}

/// The mangled signature of the top-level function serialized under the public `signature`, as
/// written inside the mangle of a declaration `outer_containers` type-parameter containers deep
/// in it. Standing alone it must hash to the id the signature carries.
pub(super) fn enclosing_function_mangle(
    arena: &KlibIrArena,
    function: &KlibIrFunction,
    signature: &KlibIrSignature,
    outer_containers: usize,
) -> Result<String, KlibBodyDeclineReason> {
    let KlibIrSignature::Public(public) = signature else {
        return Err(unsupported(
            "a local class declared in a function with no public signature",
        ));
    };
    if public.declaration().segments().len() != 1 {
        return Err(unsupported("a local class declared in a member function"));
    }
    let types = TypeParameters(&function.type_parameters);
    let convert = |ty: KlibIrTypeId| model(arena, ty, &types);
    let contexts = function
        .context_parameters
        .iter()
        .map(|parameter| convert(parameter.ty))
        .collect::<Result<Vec<_>, _>>()?;
    let receiver = function
        .extension_receiver
        .as_ref()
        .map(|parameter| convert(parameter.ty))
        .transpose()?;
    let mut vararg = None;
    let mut parameters = Vec::new();
    for (index, parameter) in function.regular_parameters.iter().enumerate() {
        if parameter.vararg_element_type.is_some() {
            vararg = Some(index);
        }
        parameters.push(convert(parameter.ty)?);
    }
    let bounds = function
        .type_parameters
        .iter()
        .map(|parameter| {
            parameter
                .supertypes
                .iter()
                .map(|&bound| convert(bound))
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let shape = CallableShape {
        name: &function.name,
        contexts: contexts.iter().collect(),
        receiver: receiver.as_ref(),
        params: parameters.iter().collect(),
        vararg,
        type_parameters: bounds
            .iter()
            .enumerate()
            .map(|(index, bounds)| TypeParameterShape {
                id: index as u32,
                bounds: bounds.iter().collect(),
            })
            .collect(),
        expect: false,
        placement: Placement::Ordinary,
    };
    let package = public.package().segments();
    let container = DeclarationContainer {
        package,
        classes: &[],
        native_interop_library: false,
    };
    let standalone = callable_mangled(container, &shape).map_err(|_| {
        unsupported("a local class declared in a function whose signature has no mangle")
    })?;
    if public.member_id() != Some(mangled_id(&standalone)) {
        return Err(KlibBodyDeclineReason::SignatureMismatch(
            "the mangled signature of a local class's enclosing function differs from its serialized identity"
                .to_owned(),
        ));
    }
    enclosing_callable_mangled(&shape, outer_containers).map_err(|_| {
        unsupported("a local class declared in a function whose signature has no mangle")
    })
}

struct TypeParameters<'a>(&'a [crate::metadata::klib_ir::tree::KlibIrTypeParameter]);

impl TypeParameters<'_> {
    fn index_of(&self, signature: &KlibIrSignature) -> Option<u32> {
        self.0
            .iter()
            .position(|parameter| parameter.base.symbol.signature == *signature)
            .map(|index| index as u32)
    }
}

fn model(
    arena: &KlibIrArena,
    ty: KlibIrTypeId,
    parameters: &TypeParameters<'_>,
) -> Result<MangledType, KlibBodyDeclineReason> {
    let KlibIrType::Simple {
        classifier,
        nullability,
        arguments,
        ..
    } = arena.ty(ty)
    else {
        return Err(unsupported(
            "a local class declared in a function with a dynamic, error or definitely non-null type",
        ));
    };
    // A class type is serialized as definitely not null when it is not marked nullable; for a type
    // parameter that nullability is the intersection `T & Any`, which the mangler models apart.
    let nullable = match (classifier.kind, nullability) {
        (_, KlibIrNullability::MarkedNullable) => true,
        (KlibIrSymbolKind::TypeParameter, KlibIrNullability::DefinitelyNotNull) => {
            return Err(unsupported(
                "a local class declared in a function with a definitely non-null type parameter",
            ))
        }
        _ => false,
    };
    match classifier.kind {
        KlibIrSymbolKind::TypeParameter => parameters
            .index_of(&classifier.signature)
            .map(|index| MangledType::Parameter { index, nullable })
            .ok_or(unsupported(
                "a local class declared in a function whose type names a type parameter of another declaration",
            )),
        KlibIrSymbolKind::Class => {
            let KlibIrSignature::Public(public) = &classifier.signature else {
                return Err(unsupported(
                    "a local class declared in a function whose type names a local class",
                ));
            };
            let fq_name = public
                .package()
                .segments()
                .iter()
                .chain(public.declaration().segments())
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(".");
            let arguments = arguments
                .iter()
                .map(|argument| match argument {
                    KlibIrTypeArgument::Star => Ok(MangledType::Star),
                    KlibIrTypeArgument::Type { variance, ty } => {
                        let inner = model(arena, *ty, parameters)?;
                        Ok(match variance {
                            KlibIrVariance::Invariant => inner,
                            KlibIrVariance::In => MangledType::In(Box::new(inner)),
                            KlibIrVariance::Out => MangledType::Out(Box::new(inner)),
                        })
                    }
                })
                .collect::<Result<Vec<_>, KlibBodyDeclineReason>>()?;
            Ok(MangledType::Class {
                fq_name,
                arguments,
                nullable,
            })
        }
        _ => Err(unsupported(
            "a local class declared in a function whose type names a declaration that is no classifier",
        )),
    }
}

fn unsupported(reason: &'static str) -> KlibBodyDeclineReason {
    KlibBodyDeclineReason::UnsupportedOperation(reason)
}
