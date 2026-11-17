//! The most specific of several bodyless declarations a class inherits for one member.
//!
//! When no inherited declaration has a body, the fake override is abstract and stands for the
//! declaration kotlinc's fake-override builder selects: the first, in inheritance order, whose
//! result type is a subtype of every other's. The results are compared as the inheriting class
//! sees them. Each declaring class's type parameters are substituted through the supertype
//! applications that reach it, so type arguments and nullability take part in the comparison.
//! `Reader<List<E>>.read(): List<E>?` is less specific than `Strict<E>.read(): List<E>`.

use crate::fx_hash::{HashMap, HashSet};
use crate::libraries::KlibDependencyCallable;
use crate::metadata::id_signature::KlibPublicIdSignature;
use crate::metadata::klib_ir::tree::{
    KlibIrArena, KlibIrClass, KlibIrNullability, KlibIrType, KlibIrTypeArgument, KlibIrTypeId,
};
use crate::metadata::klib_ir::{KlibIrSignature, KlibIrSymbol, KlibIrSymbolKind};

use super::super::{KlibCalleeOrigin, KlibDeclarationBodies};

/// A serialized type as the inheriting class sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SeenType {
    Class {
        classifier: KlibPublicIdSignature,
        /// Each argument's type, or `None` for a star projection.
        arguments: Vec<Option<SeenType>>,
        nullable: bool,
    },
    Parameter {
        parameter: SeenParameter,
        nullable: bool,
    },
}

/// A type parameter no substitution replaces.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SeenParameter {
    /// A type parameter of the inheriting class.
    Class(KlibIrSymbol),
    /// The member's own type parameter at this position, which every inherited declaration of
    /// the member declares in the same order.
    Member(usize),
}

impl SeenType {
    fn nullable(&self) -> bool {
        match self {
            Self::Class { nullable, .. } | Self::Parameter { nullable, .. } => *nullable,
        }
    }

    fn with_nullable(mut self, value: bool) -> Self {
        match &mut self {
            Self::Class { nullable, .. } | Self::Parameter { nullable, .. } => *nullable = value,
        }
        self
    }
}

/// The types a class's type parameters stand for, as the inheriting class sees them.
type Substitution = HashMap<KlibIrSymbol, SeenType>;

impl KlibDeclarationBodies {
    /// The most specific of the bodyless `inherited` declarations of a member of the class
    /// `class`, which inherits them all.
    pub(super) fn most_specific(
        &self,
        body_module: &str,
        class: (&KlibIrArena, &KlibIrClass),
        inherited: &[KlibDependencyCallable],
    ) -> KlibCalleeOrigin {
        let Some(supertypes) =
            self.supertype_substitutions(body_module, class, Substitution::default())
        else {
            return KlibCalleeOrigin::Ambiguous;
        };
        let results = inherited
            .iter()
            .map(|identity| self.inherited_result(identity, &supertypes))
            .collect::<Option<Vec<_>>>();
        let Some(results) = results else {
            return KlibCalleeOrigin::Ambiguous;
        };
        // Of several equally specific declarations, such as `AbstractCollection.size` and
        // `List.size`, kotlinc keeps the first it inherits.
        inherited
            .iter()
            .zip(&results)
            .find(|(_, result)| {
                results
                    .iter()
                    .all(|other| self.is_subtype(body_module, result, other))
            })
            .map_or(KlibCalleeOrigin::Ambiguous, |(one, _)| {
                KlibCalleeOrigin::Exact(one.clone())
            })
    }

    /// The result of the inherited declaration `identity`, as the inheriting class sees it
    /// through the substitution of the supertype that declares it.
    fn inherited_result(
        &self,
        identity: &KlibDependencyCallable,
        supertypes: &[(KlibPublicIdSignature, Substitution)],
    ) -> Option<SeenType> {
        // An accessor is a member of the class that declares its property.
        let signature = match &identity.declaration {
            crate::libraries::KlibDeclarationSignature::Public(signature) => signature,
            crate::libraries::KlibDeclarationSignature::Accessor(accessor) => accessor.property(),
            crate::libraries::KlibDeclarationSignature::FileLocal { .. } => return None,
        };
        let (_, substitution) = supertypes
            .iter()
            .find(|(class, _)| signature.is_member_of(class))?;
        let (arena, function) = self.function(identity)?;
        let member = function
            .type_parameters
            .iter()
            .map(|parameter| &parameter.base.symbol)
            .collect::<Vec<_>>();
        seen_type(arena, function.return_type, substitution, &member)
    }

    /// Every class `class` reaches through its supertypes, itself first, breadth first, with the
    /// substitution of its type parameters as `class` applies them. `None` when a supertype is
    /// serialized more than once or applies a type the comparison cannot read.
    fn supertype_substitutions(
        &self,
        body_module: &str,
        (arena, class): (&KlibIrArena, &KlibIrClass),
        substitution: Substitution,
    ) -> Option<Vec<(KlibPublicIdSignature, Substitution)>> {
        let KlibIrSignature::Public(signature) = &class.base.symbol.signature else {
            return None;
        };
        let mut found = Vec::new();
        let mut visited = HashSet::default();
        let mut pending =
            std::collections::VecDeque::from([(signature.clone(), arena, class, substitution)]);
        while let Some((signature, arena, class, substitution)) = pending.pop_front() {
            if !visited.insert(signature.clone()) {
                continue;
            }
            for &supertype in &class.supertypes {
                let SeenType::Class {
                    classifier,
                    arguments,
                    ..
                } = seen_type(arena, supertype, &substitution, &[])?
                else {
                    return None;
                };
                let (super_arena, super_class) =
                    self.declaring_class_of(body_module, &classifier).ok()??;
                if super_class.type_parameters.len() != arguments.len() {
                    return None;
                }
                let applied = super_class
                    .type_parameters
                    .iter()
                    .zip(arguments)
                    .filter_map(|(parameter, argument)| {
                        argument.map(|argument| (parameter.base.symbol.clone(), argument))
                    })
                    .collect();
                pending.push_back((classifier, super_arena, super_class, applied));
            }
            found.push((signature, substitution));
        }
        Some(found)
    }

    /// Whether `sub` is a subtype of `sup`: no nullable type is a subtype of a non-null one, and
    /// a class application is a subtype of each supertype it reaches with equal arguments.
    fn is_subtype(&self, body_module: &str, sub: &SeenType, sup: &SeenType) -> bool {
        if sub.nullable() && !sup.nullable() {
            return false;
        }
        let sub = sub.clone().with_nullable(sup.nullable());
        if sub == *sup {
            return true;
        }
        let (
            SeenType::Class {
                classifier,
                arguments,
                ..
            },
            SeenType::Class {
                classifier: wanted,
                arguments: wanted_arguments,
                ..
            },
        ) = (&sub, sup)
        else {
            return false;
        };
        let Ok(Some((arena, class))) = self.declaring_class_of(body_module, classifier) else {
            return false;
        };
        if class.type_parameters.len() != arguments.len() {
            return false;
        }
        let substitution = class
            .type_parameters
            .iter()
            .zip(arguments)
            .filter_map(|(parameter, argument)| {
                argument
                    .clone()
                    .map(|argument| (parameter.base.symbol.clone(), argument))
            })
            .collect();
        let Some(supertypes) =
            self.supertype_substitutions(body_module, (arena, class), substitution)
        else {
            return false;
        };
        supertypes.iter().any(|(reached, substitution)| {
            reached == wanted
                && self
                    .declaring_class_of(body_module, reached)
                    .ok()
                    .flatten()
                    .is_some_and(|(_, reached)| {
                        reached.type_parameters.len() == wanted_arguments.len()
                            && reached.type_parameters.iter().zip(wanted_arguments).all(
                                |(parameter, wanted)| {
                                    substitution.get(&parameter.base.symbol) == wanted.as_ref()
                                },
                            )
                    })
        })
    }
}

/// The serialized type `ty` of `arena` with `substitution` applied; `member` lists the member's
/// own type parameters by position.
fn seen_type(
    arena: &KlibIrArena,
    ty: KlibIrTypeId,
    substitution: &Substitution,
    member: &[&KlibIrSymbol],
) -> Option<SeenType> {
    match arena.ty(ty) {
        KlibIrType::Simple {
            classifier,
            nullability,
            arguments,
            ..
        } => {
            let nullable = *nullability == KlibIrNullability::MarkedNullable;
            match classifier.kind {
                KlibIrSymbolKind::Class => {
                    let KlibIrSignature::Public(signature) = &classifier.signature else {
                        return None;
                    };
                    let arguments = arguments
                        .iter()
                        .map(|argument| match argument {
                            KlibIrTypeArgument::Star => Some(None),
                            KlibIrTypeArgument::Type { ty, .. } => {
                                seen_type(arena, *ty, substitution, member).map(Some)
                            }
                        })
                        .collect::<Option<Vec<_>>>()?;
                    Some(SeenType::Class {
                        classifier: signature.clone(),
                        arguments,
                        nullable,
                    })
                }
                KlibIrSymbolKind::TypeParameter => {
                    if let Some(position) = member.iter().position(|own| *own == classifier) {
                        return Some(SeenType::Parameter {
                            parameter: SeenParameter::Member(position),
                            nullable,
                        });
                    }
                    Some(match substitution.get(classifier) {
                        // `T?` of a parameter applied as `String` is `String?`; applied as
                        // `String?`, `T` is nullable too.
                        Some(applied) => {
                            let nullable = nullable || applied.nullable();
                            applied.clone().with_nullable(nullable)
                        }
                        None => SeenType::Parameter {
                            parameter: SeenParameter::Class(classifier.clone()),
                            nullable,
                        },
                    })
                }
                _ => None,
            }
        }
        KlibIrType::DefinitelyNotNull(inner) => {
            seen_type(arena, *inner, substitution, member).map(|ty| ty.with_nullable(false))
        }
        KlibIrType::Dynamic { .. } | KlibIrType::Error { .. } => None,
    }
}
