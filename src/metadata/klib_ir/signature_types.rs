//! Serialized KLIB IR declarations as the signature mangler sees them.
//!
//! The mangler works on target-free shapes (`crate::metadata::id_signature::mangling`). This
//! module owns the conversion from a declaration's serialized types to those shapes: every
//! classifier is its qualified public name, and a type parameter is a numbered declaration, so a
//! reference to one is located by identity and never by its spelling. A type parameter of a class
//! a declaration is inherited from is replaced by the type its subclass supplies, as the fake
//! override of the declaration spells it.

use std::borrow::Cow;

use super::tree::{
    KlibIrArena, KlibIrClass, KlibIrFunction, KlibIrNullability, KlibIrType, KlibIrTypeArgument,
    KlibIrTypeId, KlibIrTypeParameter, KlibIrVariance,
};
use super::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};
use crate::metadata::id_signature::{
    CallableShape, LocalContainer, ManglingError, Placement, PropertyShape, SignatureType,
    TypeParameterShape, TypeView,
};

/// A type, with every type parameter it mentions numbered.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum KlibMangleType {
    Class {
        fq_name: String,
        arguments: Vec<KlibMangleType>,
        nullable: bool,
    },
    Parameter {
        id: u32,
        nullable: bool,
    },
    In(Box<KlibMangleType>),
    Out(Box<KlibMangleType>),
    Star,
}

impl SignatureType for KlibMangleType {
    type ParameterId = u32;

    fn view(&self) -> Result<TypeView<'_, Self>, ManglingError> {
        Ok(match self {
            Self::Class {
                fq_name,
                arguments,
                nullable,
            } => TypeView::Class {
                fq_name: Cow::Borrowed(fq_name),
                arguments: arguments.iter().collect(),
                nullable: *nullable,
            },
            Self::Parameter { id, nullable } => TypeView::Parameter {
                id: *id,
                nullable: *nullable,
            },
            Self::In(inner) => TypeView::In(inner),
            Self::Out(inner) => TypeView::Out(inner),
            Self::Star => TypeView::Star,
        })
    }
}

/// The type parameters a conversion resolves: those the converted declaration declares, numbered
/// in declaration order, and those of an inherited-from class, each with the type it is
/// substituted by.
#[derive(Clone, Default)]
pub(crate) struct KlibMangleScope {
    declared: Vec<KlibIrSymbol>,
    substitutions: Vec<(KlibIrSymbol, KlibMangleType)>,
}

impl KlibMangleScope {
    /// The scope in which a class's declarations are seen through the supertype application that
    /// binds each of the class's type parameters.
    pub(crate) fn substituting(
        parameters: &[KlibIrTypeParameter],
        arguments: Vec<KlibMangleType>,
    ) -> Result<Self, ManglingError> {
        if parameters.len() != arguments.len() {
            return Err(ManglingError::new(
                "a supertype application does not bind every type parameter of its class",
            ));
        }
        Ok(Self {
            declared: Vec::new(),
            substitutions: parameters
                .iter()
                .map(|parameter| parameter.base.symbol.clone())
                .zip(arguments)
                .collect(),
        })
    }

    /// This scope with `parameters` declared in it. A parameter is numbered by its position among
    /// everything declared, so each of a stack of declarations' parameters has one number.
    pub(crate) fn declaring<'p>(
        &self,
        parameters: impl IntoIterator<Item = &'p KlibIrTypeParameter>,
    ) -> Self {
        let mut scope = self.clone();
        for parameter in parameters {
            if !scope.declared.contains(&parameter.base.symbol) {
                scope.declared.push(parameter.base.symbol.clone());
            }
        }
        scope
    }

    pub(crate) fn convert(
        &self,
        arena: &KlibIrArena,
        ty: KlibIrTypeId,
    ) -> Result<KlibMangleType, ManglingError> {
        let KlibIrType::Simple {
            classifier,
            nullability,
            arguments,
            ..
        } = arena.ty(ty)
        else {
            return Err(ManglingError::new(
                "a type that is not a classifier application",
            ));
        };
        let nullable = *nullability == KlibIrNullability::MarkedNullable;
        match classifier.kind {
            KlibIrSymbolKind::Class => {
                let KlibIrSignature::Public(signature) = &classifier.signature else {
                    return Err(ManglingError::new("a type of a file-local class"));
                };
                let package = signature.package().segments();
                let declaration = signature.declaration().segments().join(".");
                let fq_name = if package.is_empty() {
                    declaration
                } else {
                    format!("{}.{declaration}", package.join("."))
                };
                let arguments = arguments
                    .iter()
                    .map(|argument| self.argument(arena, *argument))
                    .collect::<Result<_, _>>()?;
                Ok(KlibMangleType::Class {
                    fq_name,
                    arguments,
                    nullable,
                })
            }
            KlibIrSymbolKind::TypeParameter => {
                // A type parameter marked definitely non-null is `T & Any`, a type the mangler
                // spells by a form no serialized declaration here uses.
                if *nullability == KlibIrNullability::DefinitelyNotNull {
                    return Err(ManglingError::new(
                        "a definitely non-nullable type parameter",
                    ));
                }
                if let Some(position) = self.declared.iter().position(|id| id == classifier) {
                    return Ok(KlibMangleType::Parameter {
                        id: u32::try_from(position).expect("a parameter list fits u32"),
                        nullable,
                    });
                }
                let Some((_, replacement)) = self
                    .substitutions
                    .iter()
                    .find(|(parameter, _)| parameter == classifier)
                else {
                    return Err(ManglingError::new(
                        "a type parameter that no enclosing declaration declares",
                    ));
                };
                Ok(match (replacement, nullable) {
                    (replacement, false) => replacement.clone(),
                    (
                        KlibMangleType::Class {
                            fq_name, arguments, ..
                        },
                        true,
                    ) => KlibMangleType::Class {
                        fq_name: fq_name.clone(),
                        arguments: arguments.clone(),
                        nullable: true,
                    },
                    (KlibMangleType::Parameter { id, .. }, true) => KlibMangleType::Parameter {
                        id: *id,
                        nullable: true,
                    },
                    _ => {
                        return Err(ManglingError::new(
                            "a nullable use of a projected substitution",
                        ))
                    }
                })
            }
            _ => Err(ManglingError::new(
                "a type whose classifier is neither a class nor a type parameter",
            )),
        }
    }

    pub(crate) fn argument(
        &self,
        arena: &KlibIrArena,
        argument: KlibIrTypeArgument,
    ) -> Result<KlibMangleType, ManglingError> {
        Ok(match argument {
            KlibIrTypeArgument::Star => KlibMangleType::Star,
            KlibIrTypeArgument::Type { variance, ty } => {
                let ty = self.convert(arena, ty)?;
                match variance {
                    KlibIrVariance::Invariant => ty,
                    KlibIrVariance::In => KlibMangleType::In(Box::new(ty)),
                    KlibIrVariance::Out => KlibMangleType::Out(Box::new(ty)),
                }
            }
        })
    }

    fn parameters(
        &self,
        arena: &KlibIrArena,
        parameters: &[KlibIrTypeParameter],
    ) -> Result<Vec<(u32, Vec<KlibMangleType>)>, ManglingError> {
        parameters
            .iter()
            .map(|parameter| {
                let position = self
                    .declared
                    .iter()
                    .position(|declared| *declared == parameter.base.symbol)
                    .expect("a declaration's own type parameters are declared in its scope");
                let bounds = parameter
                    .supertypes
                    .iter()
                    .map(|bound| self.convert(arena, *bound))
                    .collect::<Result<_, _>>()?;
                Ok((
                    u32::try_from(position).expect("a parameter list fits u32"),
                    bounds,
                ))
            })
            .collect()
    }
}

/// A function, with its types converted.
pub(crate) struct KlibMangleCallable {
    name: String,
    contexts: Vec<KlibMangleType>,
    receiver: Option<KlibMangleType>,
    params: Vec<KlibMangleType>,
    vararg: Option<usize>,
    type_parameters: Vec<(u32, Vec<KlibMangleType>)>,
}

impl KlibMangleCallable {
    pub(crate) fn of(
        scope: &KlibMangleScope,
        arena: &KlibIrArena,
        function: &KlibIrFunction,
    ) -> Result<Self, ManglingError> {
        let scope = scope.declaring(&function.type_parameters);
        let convert = |ty| scope.convert(arena, ty);
        let vararg = function
            .regular_parameters
            .iter()
            .position(|parameter| parameter.vararg_element_type.is_some());
        Ok(Self {
            name: function.name.clone(),
            contexts: function
                .context_parameters
                .iter()
                .map(|parameter| convert(parameter.ty))
                .collect::<Result<_, _>>()?,
            receiver: function
                .extension_receiver
                .as_ref()
                .map(|parameter| convert(parameter.ty))
                .transpose()?,
            params: function
                .regular_parameters
                .iter()
                .map(|parameter| convert(parameter.ty))
                .collect::<Result<_, _>>()?,
            vararg,
            type_parameters: scope.parameters(arena, &function.type_parameters)?,
        })
    }

    pub(crate) fn shape(&self) -> CallableShape<'_, KlibMangleType> {
        CallableShape {
            name: &self.name,
            contexts: self.contexts.iter().collect(),
            receiver: self.receiver.as_ref(),
            params: self.params.iter().collect(),
            vararg: self.vararg,
            type_parameters: type_parameter_shapes(&self.type_parameters),
            expect: false,
            placement: Placement::Ordinary,
        }
    }
}

/// A property, read from one of its accessors, with its types converted. A property's contexts,
/// receiver and type parameters are its accessors'.
pub(crate) struct KlibMangleProperty {
    name: String,
    contexts: Vec<KlibMangleType>,
    receiver: Option<KlibMangleType>,
    type_parameters: Vec<(u32, Vec<KlibMangleType>)>,
    /// The type a setter takes.
    setter_value: Option<KlibMangleType>,
}

impl KlibMangleProperty {
    pub(crate) fn of(
        scope: &KlibMangleScope,
        arena: &KlibIrArena,
        name: &str,
        getter: Option<&KlibIrFunction>,
        setter: Option<&KlibIrFunction>,
    ) -> Result<Option<Self>, ManglingError> {
        let Some(accessor) = getter.or(setter) else {
            return Ok(None);
        };
        let scope = scope.declaring(&accessor.type_parameters);
        let convert = |ty| scope.convert(arena, ty);
        Ok(Some(Self {
            name: name.to_owned(),
            contexts: accessor
                .context_parameters
                .iter()
                .map(|parameter| convert(parameter.ty))
                .collect::<Result<_, _>>()?,
            receiver: accessor
                .extension_receiver
                .as_ref()
                .map(|parameter| convert(parameter.ty))
                .transpose()?,
            type_parameters: scope.parameters(arena, &accessor.type_parameters)?,
            setter_value: setter
                .and_then(|setter| setter.regular_parameters.last())
                .map(|parameter| convert(parameter.ty))
                .transpose()?,
        }))
    }

    pub(crate) fn shape(&self) -> PropertyShape<'_, KlibMangleType> {
        PropertyShape {
            name: &self.name,
            contexts: self.contexts.iter().collect(),
            receiver: self.receiver.as_ref(),
            type_parameters: type_parameter_shapes(&self.type_parameters),
            expect: false,
            placement: Placement::Ordinary,
        }
    }

    pub(crate) fn setter_value(&self) -> Option<&KlibMangleType> {
        self.setter_value.as_ref()
    }
}

fn type_parameter_shapes(
    parameters: &[(u32, Vec<KlibMangleType>)],
) -> Vec<TypeParameterShape<'_, KlibMangleType>> {
    parameters
        .iter()
        .map(|(id, bounds)| TypeParameterShape {
            id: *id,
            bounds: bounds.iter().collect(),
        })
        .collect()
}

/// A declaration a local class is nested in, with its types converted.
pub(crate) enum KlibMangleContainer {
    Class(Vec<(u32, Vec<KlibMangleType>)>),
    Callable(KlibMangleCallable),
}

impl KlibMangleContainer {
    pub(crate) fn class(
        scope: &KlibMangleScope,
        arena: &KlibIrArena,
        class: &KlibIrClass,
    ) -> Result<Self, ManglingError> {
        let scope = scope.declaring(&class.type_parameters);
        Ok(Self::Class(
            scope.parameters(arena, &class.type_parameters)?,
        ))
    }

    pub(crate) fn callable(
        scope: &KlibMangleScope,
        arena: &KlibIrArena,
        function: &KlibIrFunction,
    ) -> Result<Self, ManglingError> {
        KlibMangleCallable::of(scope, arena, function).map(Self::Callable)
    }

    pub(crate) fn shape(&self) -> LocalContainer<'_, KlibMangleType> {
        match self {
            Self::Class(parameters) => LocalContainer::Class(type_parameter_shapes(parameters)),
            Self::Callable(callable) => LocalContainer::Callable(callable.shape()),
        }
    }
}
