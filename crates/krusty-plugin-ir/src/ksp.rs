//! What krusty hands a KSP host for one processing round: the module's source files and their
//! declarations as KSP's symbol model shows them to a processor. krusty builds it from the module's
//! resolved declaration headers; the host serves `Resolver` and the `KS*` nodes from it.
//!
//! Names are KSP's: a qualified name is dot-separated and nests a classifier under its owner
//! (`pkg.Outer.Inner`), as `KSName.asString()` spells it.

/// Every source file of the module in the order the compilation reads them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspRound {
    pub files: Vec<KspFile>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspFile {
    /// The path the compilation was given, which `KSFile.filePath` answers.
    pub path: String,
    /// Dot-separated; empty for the root package.
    pub package_name: String,
    /// Top-level declarations in KSP's order: functions and properties in source order, then
    /// classifiers and type aliases in source order.
    pub declarations: Vec<KspDeclaration>,
    /// Whether [`Self::declarations`] are every top-level declaration of the file. When the round
    /// cannot describe one, the host refuses to list declarations rather than list a part of them.
    pub declarations_complete: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspClass {
    pub simple_name: String,
    pub qualified_name: String,
    pub kind: KspClassKind,
    /// The 1-based line of the classifier's name, which `KSNode.location` reports.
    pub line: u32,
    pub annotations: Vec<KspAnnotation>,
    /// The modifiers the declaration wrote, which is what `KSClassDeclaration.modifiers` answers,
    /// or `None` when the round has no record of them, in which case the host refuses the query.
    pub modifiers: Option<Vec<KspModifier>>,
    /// The declared type parameters, or `None` when the round cannot describe one of them, in
    /// which case the host refuses the query.
    pub type_parameters: Option<Vec<KspTypeParameter>>,
    /// The direct supertypes in written order, `kotlin.Any` when none is written, or `None` when
    /// the round cannot describe one of them, in which case the host refuses the query.
    pub super_types: Option<Vec<KspType>>,
    /// The declarations KSP lists, in its order: properties and functions in source order (the
    /// primary constructor's properties first), a data class's `componentN` and `copy`, the
    /// constructors (the primary one first), enum entries and nested classifiers in source order,
    /// then an enum class's `values`, `valueOf` and `entries`.
    pub declarations: Vec<KspDeclaration>,
    /// Whether [`Self::declarations`] are every declaration KSP lists. When the round cannot
    /// describe one, the host refuses to list declarations rather than list a part of them.
    pub declarations_complete: bool,
}

impl KspClass {
    /// The primary constructor, which is among the declarations.
    pub fn primary_constructor(&self) -> Option<&KspFunction> {
        self.declarations
            .iter()
            .find_map(|declaration| match declaration {
                KspDeclaration::Function(function)
                    if function.kind == KspFunctionKind::PrimaryConstructor =>
                {
                    Some(function)
                }
                _ => None,
            })
    }

    /// The classifiers declared in this one's body, enum entries included.
    pub fn classes(&self) -> impl Iterator<Item = &KspClass> {
        self.declarations
            .iter()
            .filter_map(|declaration| match declaration {
                KspDeclaration::Class(class) => Some(class),
                _ => None,
            })
    }
}

impl KspFile {
    /// The top-level classifiers.
    pub fn classes(&self) -> impl Iterator<Item = &KspClass> {
        self.declarations
            .iter()
            .filter_map(|declaration| match declaration {
                KspDeclaration::Class(class) => Some(class),
                _ => None,
            })
    }
}

/// One declaration of a file or a classifier body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KspDeclaration {
    Class(KspClass),
    Function(KspFunction),
    Property(KspProperty),
    TypeAlias(KspTypeAlias),
}

/// A function or constructor as `KSFunctionDeclaration` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspFunction {
    /// The name; `<init>` for a constructor.
    pub name: String,
    pub kind: KspFunctionKind,
    pub is_abstract: bool,
    /// The 1-based line of the declaration, or `None` for one the language contributes.
    pub line: Option<u32>,
    pub annotations: Vec<KspAnnotation>,
    /// `None` when the round has no record of the modifiers.
    pub modifiers: Option<Vec<KspModifier>>,
    /// `None` when the round cannot describe one of them.
    pub type_parameters: Option<Vec<KspTypeParameter>>,
    pub extension_receiver: KspTypeSlot,
    pub parameters: Vec<KspParameter>,
    /// `None` when the round cannot describe the type.
    pub return_type: Option<KspType>,
}

/// Where a function declares it, which KSP's `FunctionKind` and the constructor queries tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KspFunctionKind {
    TopLevel,
    Member,
    /// A function the language contributes to an enum class (`values`, `valueOf`).
    Static,
    PrimaryConstructor,
    SecondaryConstructor,
}

/// A property as `KSPropertyDeclaration` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspProperty {
    pub name: String,
    pub is_mutable: bool,
    /// The 1-based line of the declaration, or `None` for one the language contributes.
    pub line: Option<u32>,
    pub annotations: Vec<KspAnnotation>,
    /// `None` when the round has no record of the modifiers.
    pub modifiers: Option<Vec<KspModifier>>,
    /// `None` when the round cannot describe one of them.
    pub type_parameters: Option<Vec<KspTypeParameter>>,
    pub extension_receiver: KspTypeSlot,
    /// `None` when the round cannot describe the type.
    pub ty: Option<KspType>,
}

/// A type alias, of which the round describes only the name yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspTypeAlias {
    pub simple_name: String,
    pub qualified_name: String,
}

/// A type a declaration may or may not have, such as an extension receiver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KspTypeSlot {
    Absent,
    Present(KspType),
    /// Present, but not a type the round can describe; the host refuses the query.
    Undescribed,
}

/// A function or constructor parameter as `KSValueParameter` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspParameter {
    pub name: String,
    /// A vararg parameter's element type; `None` when the round cannot describe the type.
    pub ty: Option<KspType>,
    /// Written `val`: the parameter also declares a read-only property.
    pub is_val: bool,
    /// Written `var`: the parameter also declares a mutable property.
    pub is_var: bool,
    pub is_vararg: bool,
    pub is_noinline: bool,
    pub is_crossinline: bool,
    /// Whether the parameter declares or inherits a default; `None` when it overrides a library
    /// function the round cannot read the defaults of.
    pub has_default: Option<bool>,
}

/// KSP's `ClassKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KspClassKind {
    Interface,
    Class,
    EnumClass,
    EnumEntry,
    Object,
    AnnotationClass,
}

/// One annotation applied to a declaration, as resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspAnnotation {
    /// The name the annotation was written with (`KSAnnotation.shortName`).
    pub short_name: String,
    /// The annotation class it resolved to.
    pub qualified_name: String,
}

/// KSP's `Modifier`, as far as a Kotlin declaration can write one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KspModifier {
    Public,
    Private,
    Internal,
    Protected,
    In,
    Out,
    Override,
    Lateinit,
    Enum,
    Sealed,
    Annotation,
    Data,
    Inner,
    Fun,
    Value,
    Suspend,
    Tailrec,
    Operator,
    Infix,
    Inline,
    External,
    Abstract,
    Final,
    Open,
    Const,
    Vararg,
    Noinline,
    Crossinline,
    Reified,
    Expect,
    Actual,
    /// What KSP reports for a static member the language contributes, such as an enum class's
    /// `values`.
    JavaStatic,
}

impl KspModifier {
    /// The name of KSP's `Modifier` constant.
    pub fn name(self) -> &'static str {
        match self {
            KspModifier::Public => "PUBLIC",
            KspModifier::Private => "PRIVATE",
            KspModifier::Internal => "INTERNAL",
            KspModifier::Protected => "PROTECTED",
            KspModifier::In => "IN",
            KspModifier::Out => "OUT",
            KspModifier::Override => "OVERRIDE",
            KspModifier::Lateinit => "LATEINIT",
            KspModifier::Enum => "ENUM",
            KspModifier::Sealed => "SEALED",
            KspModifier::Annotation => "ANNOTATION",
            KspModifier::Data => "DATA",
            KspModifier::Inner => "INNER",
            KspModifier::Fun => "FUN",
            KspModifier::Value => "VALUE",
            KspModifier::Suspend => "SUSPEND",
            KspModifier::Tailrec => "TAILREC",
            KspModifier::Operator => "OPERATOR",
            KspModifier::Infix => "INFIX",
            KspModifier::Inline => "INLINE",
            KspModifier::External => "EXTERNAL",
            KspModifier::Abstract => "ABSTRACT",
            KspModifier::Final => "FINAL",
            KspModifier::Open => "OPEN",
            KspModifier::Const => "CONST",
            KspModifier::Vararg => "VARARG",
            KspModifier::Noinline => "NOINLINE",
            KspModifier::Crossinline => "CROSSINLINE",
            KspModifier::Reified => "REIFIED",
            KspModifier::Expect => "EXPECT",
            KspModifier::Actual => "ACTUAL",
            KspModifier::JavaStatic => "JAVA_STATIC",
        }
    }
}

/// A declared type parameter as `KSTypeParameter` describes it. Its qualified name is its owner's,
/// then its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspTypeParameter {
    /// Unique in the round: what a [`KspTypeDeclaration::TypeParameter`] refers to it by, since
    /// overloads can declare type parameters with the same owner name and name.
    pub id: u32,
    pub name: String,
    pub variance: KspVariance,
    pub is_reified: bool,
    /// The upper bounds in written order; `kotlin.Any?` alone when none is written.
    pub bounds: Vec<KspType>,
}

/// A declaration-site variance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KspVariance {
    Invariant,
    Out,
    In,
}

/// A type as `KSType` describes it: a classifier or a type parameter, applied to arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspType {
    pub declaration: KspTypeDeclaration,
    pub nullable: bool,
    pub arguments: Vec<KspTypeArgument>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KspTypeDeclaration {
    /// A classifier, by qualified name: the module's own or the classpath's.
    Class(String),
    /// The type parameter [`KspTypeParameter::id`] `id`, named `name` and declared by `owner` (a
    /// qualified name).
    TypeParameter {
        id: u32,
        owner: String,
        name: String,
    },
}

/// One type argument with its use-site variance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KspTypeArgument {
    Star,
    Invariant(KspType),
    Out(KspType),
    In(KspType),
}
