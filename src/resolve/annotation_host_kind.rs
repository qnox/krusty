//! The kind of a non-property declaration an annotation is applied to: the annotation targets it
//! denotes and the name kotlinc's diagnostics give it.

use crate::types::KotlinTarget;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AnnotationHostKind {
    Class,
    Interface,
    StandaloneObject,
    EnumClass,
    AnnotationClass,
    Constructor,
    EnumEntry,
    /// A function whose container the diagnostic does not name.
    Function,
    TopLevelFunction,
    MemberFunction,
    LocalFunction,
    ValueParameter,
    LocalVariable,
    TypeParameter,
    TypeUsage,
    Getter,
    Setter,
    /// An annotated expression or a statement that is no declaration.
    Expression,
    /// A local function, class or type alias statement, checked again as its own declaration.
    LocalDeclaration,
    /// An `init` block, which no annotation target denotes.
    Initializer,
}

impl AnnotationHostKind {
    /// How kotlinc's diagnostics name this declaration kind.
    pub(super) fn description(self) -> &'static str {
        match self {
            Self::Class => "class",
            Self::Interface => "interface",
            Self::StandaloneObject => "standalone object",
            Self::EnumClass => "enum class",
            Self::AnnotationClass => "annotation class",
            Self::Constructor => "constructor",
            Self::EnumEntry => "enum entry",
            Self::Function => "function",
            Self::TopLevelFunction => "top level function",
            Self::MemberFunction => "member function",
            Self::LocalFunction => "local function",
            Self::ValueParameter => "value parameter",
            Self::LocalVariable => "local variable",
            Self::TypeParameter => "type parameter",
            Self::TypeUsage => "type usage",
            Self::Getter => "getter",
            Self::Setter => "setter",
            Self::Expression => "expression",
            Self::LocalDeclaration => "local declaration",
            Self::Initializer => "initializer",
        }
    }

    /// The annotation targets this declaration kind denotes, any one of which the annotation
    /// class must allow. An annotation class is a class too, and an enum entry is a property or
    /// field. `None` for a kind whose applications are validated by their own site check: an
    /// accessor's, an expression's, a local declaration statement's (its declaration check runs
    /// separately) and an initializer's.
    pub(super) fn targets(self) -> Option<&'static [KotlinTarget]> {
        Some(match self {
            Self::Class | Self::Interface | Self::StandaloneObject | Self::EnumClass => {
                &[KotlinTarget::Class]
            }
            Self::AnnotationClass => &[KotlinTarget::AnnotationClass, KotlinTarget::Class],
            Self::Constructor => &[KotlinTarget::Constructor],
            Self::EnumEntry => &[KotlinTarget::Property, KotlinTarget::Field],
            Self::Function
            | Self::TopLevelFunction
            | Self::MemberFunction
            | Self::LocalFunction => &[KotlinTarget::Function],
            Self::ValueParameter => &[KotlinTarget::ValueParameter],
            Self::LocalVariable => &[KotlinTarget::LocalVariable],
            Self::TypeParameter => &[KotlinTarget::TypeParameter],
            Self::TypeUsage => &[KotlinTarget::Type],
            Self::Getter | Self::Setter | Self::Expression | Self::LocalDeclaration => return None,
            Self::Initializer => return None,
        })
    }

    /// The kind of the accessor declaration that `target` names.
    pub(super) fn accessor(target: KotlinTarget) -> Self {
        match target {
            KotlinTarget::PropertyGetter => Self::Getter,
            KotlinTarget::PropertySetter => Self::Setter,
            KotlinTarget::ValueParameter => Self::ValueParameter,
            other => unreachable!("an accessor annotation site never names {other:?}"),
        }
    }
}
