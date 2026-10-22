//! KSP's type model of a declaration header: resolved types become [`KspType`]s naming classifiers
//! by KSP's qualified names and type parameters by their owner.

use krusty_plugin_ir::{
    KspModifier, KspType, KspTypeArgument, KspTypeDeclaration, KspTypeParameter, KspVariance,
};

use crate::ast::{ModifierKeyword, WrittenModifiers};
use crate::types::{Ty, TyKind, TyParamName, TypeName, TypeVariance};

/// The type parameters a header can name: its own and its enclosing classifiers', by resolved
/// identity.
#[derive(Clone, Default)]
pub(super) struct TypeParameterScope {
    parameters: Vec<ScopedTypeParameter>,
}

#[derive(Clone)]
struct ScopedTypeParameter {
    identity: TyParamName,
    owner: String,
    name: String,
}

impl TypeParameterScope {
    /// This scope with `owner`'s parameters added: each resolved identity and the name it was
    /// declared with, in declaration order.
    pub(super) fn with<'a>(
        &self,
        owner: &str,
        parameters: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> TypeParameterScope {
        let mut scope = self.clone();
        scope
            .parameters
            .extend(
                parameters
                    .into_iter()
                    .map(|(identity, name)| ScopedTypeParameter {
                        identity: TyParamName::new(identity),
                        owner: owner.to_string(),
                        name: name.to_string(),
                    }),
            );
        scope
    }

    /// `ty` as KSP describes it, or `None` when it is a type KSP's model here cannot describe: a
    /// function type that is not a classifier's, a platform or definitely-non-null type, an
    /// intersection, or a type that did not resolve.
    pub(super) fn ty(&self, ty: Ty) -> Option<KspType> {
        match ty.kind() {
            TyKind::Nullable(inner) => {
                let mut inner = self.ty(inner)?;
                inner.nullable = true;
                Some(inner)
            }
            TyKind::Obj(class, arguments) => self.applied(class, &arguments),
            TyKind::Unit => Some(class_type("kotlin.Unit")),
            TyKind::Nothing => Some(class_type("kotlin.Nothing")),
            TyKind::TyParam(identity, _) => {
                let parameter = self
                    .parameters
                    .iter()
                    .rev()
                    .find(|parameter| parameter.identity == identity)?;
                Some(KspType {
                    declaration: KspTypeDeclaration::TypeParameter {
                        owner: parameter.owner.clone(),
                        name: parameter.name.clone(),
                    },
                    nullable: false,
                    arguments: Vec::new(),
                })
            }
            _ => None,
        }
    }

    /// The classifier `class` applied to `arguments`, not nullable.
    pub(super) fn applied(&self, class: TypeName, arguments: &[Ty]) -> Option<KspType> {
        Some(KspType {
            declaration: KspTypeDeclaration::Class(qualified_name(class)),
            nullable: false,
            arguments: arguments
                .iter()
                .map(|argument| self.argument(*argument))
                .collect::<Option<_>>()?,
        })
    }

    fn argument(&self, argument: Ty) -> Option<KspTypeArgument> {
        Some(match argument.kind() {
            TyKind::StarProjection(_) => KspTypeArgument::Star,
            TyKind::OutProjection(ty) => KspTypeArgument::Out(self.ty(ty)?),
            TyKind::InProjection(ty) => KspTypeArgument::In(self.ty(ty)?),
            _ => KspTypeArgument::Invariant(self.ty(argument)?),
        })
    }

    /// A declared type parameter named `name` with `variance` and its upper `bounds` in written
    /// order.
    pub(super) fn type_parameter(
        &self,
        name: &str,
        variance: TypeVariance,
        bounds: impl IntoIterator<Item = Ty>,
    ) -> Option<KspTypeParameter> {
        Some(KspTypeParameter {
            name: name.to_string(),
            variance: match variance {
                TypeVariance::Invariant => KspVariance::Invariant,
                TypeVariance::Out => KspVariance::Out,
                TypeVariance::In => KspVariance::In,
            },
            is_reified: false,
            bounds: bounds
                .into_iter()
                .map(|bound| self.ty(bound))
                .collect::<Option<_>>()?,
        })
    }
}

pub(super) fn class_type(qualified_name: &str) -> KspType {
    KspType {
        declaration: KspTypeDeclaration::Class(qualified_name.to_string()),
        nullable: false,
        arguments: Vec::new(),
    }
}

/// KSP's dot-separated qualified name of a classifier: its package, then each enclosing classifier
/// and itself.
pub(super) fn qualified_name(class: TypeName) -> String {
    match class.nested_owner() {
        Some(owner) => format!("{}.{}", qualified_name(owner), class.nested_segment_ref()),
        None => {
            let package = class.package().replace('/', ".");
            let segment = class.segment();
            if package.is_empty() {
                segment
            } else {
                format!("{package}.{segment}")
            }
        }
    }
}

/// The KSP modifiers of the keywords a declaration wrote. `companion` is none: KSP reports a
/// companion object through `isCompanionObject`.
pub(super) fn modifiers(written: WrittenModifiers) -> Vec<KspModifier> {
    written
        .iter()
        .filter_map(|keyword| {
            Some(match keyword {
                ModifierKeyword::Public => KspModifier::Public,
                ModifierKeyword::Private => KspModifier::Private,
                ModifierKeyword::Internal => KspModifier::Internal,
                ModifierKeyword::Protected => KspModifier::Protected,
                ModifierKeyword::Open => KspModifier::Open,
                ModifierKeyword::Final => KspModifier::Final,
                ModifierKeyword::Abstract => KspModifier::Abstract,
                ModifierKeyword::Sealed => KspModifier::Sealed,
                ModifierKeyword::Inline => KspModifier::Inline,
                ModifierKeyword::Noinline => KspModifier::Noinline,
                ModifierKeyword::Crossinline => KspModifier::Crossinline,
                ModifierKeyword::Operator => KspModifier::Operator,
                ModifierKeyword::Override => KspModifier::Override,
                ModifierKeyword::Suspend => KspModifier::Suspend,
                ModifierKeyword::Tailrec => KspModifier::Tailrec,
                ModifierKeyword::Lateinit => KspModifier::Lateinit,
                ModifierKeyword::Infix => KspModifier::Infix,
                ModifierKeyword::Reified => KspModifier::Reified,
                ModifierKeyword::Vararg => KspModifier::Vararg,
                ModifierKeyword::Const => KspModifier::Const,
                ModifierKeyword::Actual => KspModifier::Actual,
                ModifierKeyword::Expect => KspModifier::Expect,
                ModifierKeyword::Value => KspModifier::Value,
                ModifierKeyword::Inner => KspModifier::Inner,
                ModifierKeyword::External => KspModifier::External,
                ModifierKeyword::Data => KspModifier::Data,
                ModifierKeyword::Enum => KspModifier::Enum,
                ModifierKeyword::Annotation => KspModifier::Annotation,
                ModifierKeyword::Fun => KspModifier::Fun,
                ModifierKeyword::Companion => return None,
            })
        })
        .collect()
}
