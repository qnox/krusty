//! KSP's type model of a declaration header: resolved types become [`KspType`]s naming classifiers
//! by KSP's qualified names and type parameters by their owner.

use krusty_plugin_ir::{
    KspModifier, KspType, KspTypeArgument, KspTypeDeclaration, KspTypeParameter, KspVariance,
};

use crate::ast::{File, ModifierKeyword, TypeRef, WrittenModifiers};
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
    id: u32,
    owner: String,
    name: String,
}

/// Hands out the round-unique [`KspTypeParameter::id`]s.
#[derive(Default)]
pub(super) struct TypeParameterIds(std::cell::Cell<u32>);

impl TypeParameterScope {
    /// This scope with `owner`'s parameters added: each resolved identity and the name it was
    /// declared with, in declaration order. Each gets a fresh id from `ids`.
    pub(super) fn with<'a>(
        &self,
        ids: &TypeParameterIds,
        owner: &str,
        parameters: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> TypeParameterScope {
        let mut scope = self.clone();
        scope
            .parameters
            .extend(parameters.into_iter().map(|(identity, name)| {
                let id = ids.0.get();
                ids.0.set(id + 1);
                ScopedTypeParameter {
                    identity: TyParamName::new(identity),
                    id,
                    owner: owner.to_string(),
                    name: name.to_string(),
                }
            }));
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
                        id: parameter.id,
                        owner: parameter.owner.clone(),
                        name: parameter.name.clone(),
                    },
                    nullable: false,
                    arguments: Vec::new(),
                    unannotated: false,
                })
            }
            // A function type is the `kotlin.FunctionN` classifier applied to its parameter types,
            // a receiver first, and its return type. KSP names a suspend or context function
            // type by classifiers the round does not describe yet.
            TyKind::Fun(signature) if !signature.suspend && signature.context_count == 0 => {
                let arguments = signature
                    .params
                    .iter()
                    .chain([&signature.ret])
                    .map(|ty| self.ty(*ty).map(KspTypeArgument::Invariant))
                    .collect::<Option<_>>()?;
                Some(KspType {
                    declaration: KspTypeDeclaration::Class(format!(
                        "kotlin.Function{}",
                        signature.params.len()
                    )),
                    nullable: false,
                    arguments,
                    unannotated: false,
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
            unannotated: false,
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

    /// The type parameter this scope declares with the resolved `identity`, with `variance`,
    /// `reified` and its upper `bounds` in written order.
    pub(super) fn type_parameter(
        &self,
        identity: &str,
        variance: TypeVariance,
        is_reified: bool,
        bounds: impl IntoIterator<Item = Ty>,
    ) -> Option<KspTypeParameter> {
        let identity = TyParamName::new(identity);
        let declared = self
            .parameters
            .iter()
            .rev()
            .find(|parameter| parameter.identity == identity)?;
        Some(KspTypeParameter {
            id: declared.id,
            name: declared.name.clone(),
            variance: match variance {
                TypeVariance::Invariant => KspVariance::Invariant,
                TypeVariance::Out => KspVariance::Out,
                TypeVariance::In => KspVariance::In,
            },
            is_reified,
            bounds: bounds
                .into_iter()
                .map(|bound| self.ty(bound))
                .collect::<Option<_>>()?,
        })
    }
}

/// `ty` as source spells it with `spelling`: KSP reports no annotations on a type whose spelling
/// writes no type-use annotation of its own, and the same of each argument whose spelling is the
/// matching written argument. An argument the spelling does not write one for keeps them
/// undescribed.
pub(super) fn as_written(file: &File, spelling: &TypeRef, mut ty: KspType) -> KspType {
    mark_unannotated(file, spelling, &mut ty);
    ty
}

fn mark_unannotated(file: &File, spelling: &TypeRef, ty: &mut KspType) {
    let inside = |span: crate::diag::Span, offset: u32| span.lo <= offset && offset < span.hi;
    let own_annotation = file.type_annotations.keys().any(|&offset| {
        inside(spelling.span, offset)
            && !spelling
                .targs
                .iter()
                .any(|argument| inside(argument.span, offset))
    });
    if own_annotation {
        return;
    }
    ty.unannotated = true;
    if spelling.targs.len() != ty.arguments.len() {
        return;
    }
    for (written, argument) in spelling.targs.iter().zip(&mut ty.arguments) {
        match argument {
            KspTypeArgument::Star => {}
            KspTypeArgument::Invariant(argument)
            | KspTypeArgument::Out(argument)
            | KspTypeArgument::In(argument) => mark_unannotated(file, written, argument),
        }
    }
}

pub(super) fn class_type(qualified_name: &str) -> KspType {
    KspType {
        declaration: KspTypeDeclaration::Class(qualified_name.to_string()),
        nullable: false,
        arguments: Vec::new(),
        unannotated: false,
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

#[cfg(test)]
mod tests {
    use super::{as_written, class_type};
    use crate::ast::Decl;
    use crate::diag::DiagSink;
    use krusty_plugin_ir::{KspType, KspTypeArgument};

    /// Which levels of `List<String>`, or of `String` when `generic` is false, the top-level
    /// property `name` of `source` leaves unannotated: the type, then its argument.
    fn unannotated(source: &str, name: &str, generic: bool) -> Vec<bool> {
        let mut diagnostics = DiagSink::new();
        let tokens = crate::lexer::lex(source, &mut diagnostics);
        let file = crate::parser::parse(source, &tokens, &mut diagnostics);
        assert!(
            !diagnostics.has_errors(),
            "{}",
            diagnostics.render("test", source)
        );
        let spelling = file
            .decl_arena
            .iter()
            .find_map(|declaration| match declaration {
                Decl::Property(property) if property.name == name => property.ty.as_ref(),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{name} parsed with a type"));
        let ty = match generic {
            true => KspType {
                arguments: vec![KspTypeArgument::Invariant(class_type("kotlin.String"))],
                ..class_type("kotlin.collections.List")
            },
            false => class_type("kotlin.String"),
        };
        let ty = as_written(&file, spelling, ty);
        let mut levels = vec![ty.unannotated];
        for argument in &ty.arguments {
            if let KspTypeArgument::Invariant(argument) = argument {
                levels.push(argument.unannotated);
            }
        }
        levels
    }

    #[test]
    fn a_type_use_annotation_leaves_its_own_type_undescribed() {
        let source = "@Target(AnnotationTarget.TYPE) annotation class A
            val plain: List<String> = listOf()
            val outer: @A List<String> = listOf()
            val inner: List<@A String> = listOf()
            val single: @A String = \"\"
            val bare: String = \"\"";
        assert_eq!(unannotated(source, "plain", true), [true, true]);
        assert_eq!(unannotated(source, "outer", true), [false, false]);
        assert_eq!(unannotated(source, "inner", true), [true, false]);
        assert_eq!(unannotated(source, "single", false), [false]);
        assert_eq!(unannotated(source, "bare", false), [true]);
    }
}
