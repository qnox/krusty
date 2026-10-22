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
