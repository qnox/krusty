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
    /// Top-level classifiers in source order.
    pub classes: Vec<KspClass>,
    /// Whether [`Self::classes`] are every top-level declaration of the file.
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
    /// The primary constructor's parameters, or `None` when the classifier has no primary
    /// constructor. KSP lists one (`<init>`) first among the declarations.
    pub primary_constructor: Option<Vec<KspParameter>>,
    /// Classifiers declared in this one's body, in source order.
    pub classes: Vec<KspClass>,
    /// Whether the primary constructor and [`Self::classes`] are every declaration KSP lists. When
    /// the body declares anything else, the host refuses to list declarations rather than list a
    /// part of them.
    pub declarations_complete: bool,
}

/// A primary-constructor parameter as `KSValueParameter` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspParameter {
    pub name: String,
    /// Written `val`: the parameter also declares a read-only property.
    pub is_val: bool,
    /// Written `var`: the parameter also declares a mutable property.
    pub is_var: bool,
    pub is_vararg: bool,
    pub has_default: bool,
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
        }
    }
}

/// A declared type parameter as `KSTypeParameter` describes it. Its qualified name is its owner's,
/// then its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspTypeParameter {
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
    /// A type parameter of the classifier `owner` (a qualified name).
    TypeParameter { owner: String, name: String },
}

/// One type argument with its use-site variance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KspTypeArgument {
    Star,
    Invariant(KspType),
    Out(KspType),
    In(KspType),
}
