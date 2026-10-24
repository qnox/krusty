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
    /// What `Resolver.mapToJvmSignature` answers, or `None` when the round cannot say, in which
    /// case the host refuses the query.
    pub jvm_signature: Option<String>,
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

/// A classifier of the classpath, which the host asks krusty for by name when a processor first
/// reaches it. Its [`KspClass::line`] is unused, as KSP has no location for a library declaration.
/// Its [`KspClass::declarations`] are described for an annotation class only, in KSP's order for a
/// library classifier: functions, properties, constructors, then nested classifiers as
/// [`KspDeclaration::LibraryClass`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspLibraryClass {
    pub class: KspClass,
    /// Dot-separated; empty for the root package.
    pub package_name: String,
    /// The qualified name of the classifier this one is nested in.
    pub parent: Option<String>,
    pub origin: KspOrigin,
    /// Whether `class.annotations` lists every annotation the declaration states; when not, the
    /// host refuses the query.
    pub annotations_complete: bool,
    /// What `KSClassDeclaration.getAllProperties` lists, or `None` when the round cannot say, in
    /// which case the host refuses the query.
    pub all_properties: Option<Vec<KspMember>>,
    /// What `KSClassDeclaration.getAllFunctions` lists, constructors last, or `None` when the round
    /// cannot say, in which case the host refuses the query.
    pub all_functions: Option<Vec<KspMember>>,
}

/// A member `getAllProperties` or `getAllFunctions` lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KspMember {
    /// The declaration at this index of the classifier's own [`KspClass::declarations`].
    Declared(u32),
    /// A member the classifier inherits, which the classifier `owner` (a qualified name) declares.
    Inherited {
        owner: String,
        declaration: Box<KspDeclaration>,
    },
}

/// Where a library classifier was declared, which is what `KSNode.origin` reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KspOrigin {
    /// A Kotlin declaration (`KOTLIN_LIB`): its class file carries Kotlin metadata.
    KotlinLib,
    /// A Java declaration (`JAVA_LIB`).
    JavaLib,
}

/// Where a declaration comes from, which `KSNode.origin` reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KspDeclarationOrigin {
    /// The module's source wrote it (`Origin.KOTLIN`).
    Source,
    /// The compiler made it (`Origin.SYNTHETIC`).
    Synthetic,
    /// A library declares it.
    Library(KspOrigin),
}

/// Answers the host's questions about the classpath while a round runs. The type parameters of a
/// [`KspLibraryClass`] have ids of their own answer, not the round's: an answer refers only to the
/// type parameters it declares.
pub trait KspClasspath {
    /// The classifier KSP's `Resolver.getClassDeclarationByName` finds by `qualified_name`
    /// (`pkg.Outer.Inner`), or `None` when the classpath declares none.
    fn class(&self, qualified_name: &str) -> Option<KspLibraryClass>;

    /// The name `mapping` gives the classifier `qualified_name` names, or `None` when the name
    /// denotes no classifier or the classifier is not mapped.
    fn mapped_name(&self, mapping: KspNameMapping, qualified_name: &str) -> Option<String>;
}

/// Which way a name is mapped between Kotlin and the JVM, by kotlinc's class map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KspNameMapping {
    /// `Resolver.mapKotlinNameToJava`: the JVM class a Kotlin classifier is on the platform.
    KotlinToJava,
    /// `Resolver.mapJavaNameToKotlin`: the Kotlin classifier a JVM class is read as.
    JavaToKotlin,
}

/// One declaration of a file or a classifier body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KspDeclaration {
    Class(KspClass),
    Function(KspFunction),
    Property(Box<KspProperty>),
    TypeAlias(KspTypeAlias),
    /// A classpath classifier a library classifier declares in its body, by qualified name, which
    /// the host asks for as it asks for any classpath classifier.
    LibraryClass(String),
}

/// A function or constructor as `KSFunctionDeclaration` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspFunction {
    /// The name; `<init>` for a constructor.
    pub name: String,
    pub kind: KspFunctionKind,
    pub is_abstract: bool,
    pub origin: KspDeclarationOrigin,
    /// The 1-based line of a declaration the source wrote.
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
    /// What `Resolver.mapToJvmSignature` answers; `None` when the round cannot say.
    pub jvm_signature: Option<String>,
    /// What `Resolver.getJvmName` answers; `None` when the round cannot say.
    pub jvm_name: Option<String>,
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
    pub origin: KspDeclarationOrigin,
    /// The 1-based line of a declaration the source wrote.
    pub line: Option<u32>,
    pub annotations: Vec<KspAnnotation>,
    /// `None` when the round has no record of the modifiers.
    pub modifiers: Option<Vec<KspModifier>>,
    /// `None` when the round cannot describe one of them.
    pub type_parameters: Option<Vec<KspTypeParameter>>,
    pub extension_receiver: KspTypeSlot,
    /// `None` when the round cannot describe the type.
    pub ty: Option<KspType>,
    /// Its getter; a Java field has none.
    pub getter: Option<KspAccessor>,
    /// A `var`'s setter.
    pub setter: Option<KspAccessor>,
    /// What `Resolver.mapToJvmSignature` answers; `None` when the round cannot say.
    pub jvm_signature: Option<String>,
}

/// A property getter or setter as `KSPropertyAccessor` describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspAccessor {
    pub origin: KspDeclarationOrigin,
    /// `None` when the round has no record of the modifiers.
    pub modifiers: Option<Vec<KspModifier>>,
    pub annotations: Vec<KspAnnotation>,
    /// A setter's value parameter: its name and annotations. `None` for a getter.
    pub parameter: Option<(String, Vec<KspAnnotation>)>,
    /// What `Resolver.getJvmName` answers; `None` when the round cannot say.
    pub jvm_name: Option<String>,
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
    pub annotations: Vec<KspAnnotation>,
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
    /// `KSAnnotation.shortName`: on a classifier, function or constructor the name the source
    /// wrote the annotation with, an import alias's name included; elsewhere its class's name.
    pub short_name: String,
    /// The annotation class it resolved to.
    pub qualified_name: String,
    /// The use-site target written before the name (`@get:A`); `None` when there is none.
    pub use_site: Option<KspUseSite>,
    /// Whether KSP reports the application as one the compiler made (`Origin.SYNTHETIC`) rather
    /// than one the source wrote: a default that is an annotation, or a `@setparam:` application
    /// KSP moves onto the setter's parameter.
    pub synthetic: bool,
    /// What the arguments are, or `None` when the round cannot fold them, in which case the host
    /// refuses the queries.
    pub arguments: Option<KspAnnotationArguments>,
}

/// An annotation's arguments as KSP lists them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspAnnotationArguments {
    /// `KSAnnotation.arguments`: the written arguments in the order the source wrote them, then
    /// the default of each element the application did not write, in declaration order. An
    /// omitted `vararg` element has no default and is not listed.
    pub arguments: Vec<KspArgument>,
    /// `KSAnnotation.defaultArguments`: the default of every element that has one, in
    /// declaration order, whether the application wrote the element or not.
    pub defaults: Vec<KspArgument>,
}

/// One named annotation argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KspArgument {
    pub name: String,
    pub value: KspValue,
    /// Whether the source wrote it (`Origin.KOTLIN`) rather than it being a default
    /// (`Origin.SYNTHETIC`).
    pub written: bool,
}

/// A constant annotation argument, in the form KSP hands a processor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KspValue {
    Boolean(bool),
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Char(u16),
    /// The value's bits, so that every NaN and `-0.0` survive.
    Float(u32),
    /// The value's bits.
    Double(u64),
    String(String),
    /// An enum entry: its enum class's qualified name and the entry's name.
    EnumEntry {
        class: String,
        entry: String,
    },
    /// A class literal, as the type it denotes.
    Type(KspType),
    Annotation(Box<KspAnnotation>),
    /// An array of any element kind, which KSP hands over as a list.
    Array(Vec<KspValue>),
}

/// KSP's `AnnotationUseSiteTarget`, as far as a Kotlin declaration can write one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KspUseSite {
    Field,
    Property,
    Get,
    Set,
    Param,
    SetParam,
}

impl KspUseSite {
    /// The name of KSP's `AnnotationUseSiteTarget` constant.
    pub fn name(self) -> &'static str {
        match self {
            KspUseSite::Field => "FIELD",
            KspUseSite::Property => "PROPERTY",
            KspUseSite::Get => "GET",
            KspUseSite::Set => "SET",
            KspUseSite::Param => "PARAM",
            KspUseSite::SetParam => "SETPARAM",
        }
    }
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
    /// A classpath type alias a library declaration names the type by, which KSP resolves the
    /// type to rather than to the classifier it expands to.
    TypeAlias {
        qualified_name: String,
        package_name: String,
    },
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
