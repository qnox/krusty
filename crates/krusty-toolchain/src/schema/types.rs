//! The schema's building blocks.

/// A type a property's value may have.
#[derive(Clone, Copy, Debug)]
pub enum Type {
    Boolean,
    Int,
    String,
    /// A string that may not be blank (`@NotBlank`).
    NonBlankString,
    /// A non-blank fully qualified class name (`@StringSemantics(JvmMainClass)`).
    MainClass,
    Path,
    Enum(&'static EnumType),
    List(&'static Type),
    /// A mapping from strings to values of one type.
    Map(&'static Type),
    Object(&'static ObjectType),
    /// One of several object types, chosen by the value's shape (a `sealed` schema class).
    Dependency(DependencyKind),
    /// Read by another phase (the module header, plugins), so taken here as written, unchecked.
    Opaque,
}

/// The dependency notations: which object types a dependency value may be read as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DependencyKind {
    /// A module's `dependencies:` item (`Dependency`): scoped, or a BOM.
    Module,
    /// `UnscopedDependency`: a Maven coordinate, a module path, a catalog key or a BOM, unscoped.
    Unscoped,
    /// `UnscopedExternalDependency`: a Maven coordinate or a catalog key.
    UnscopedExternal,
}

impl DependencyKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Module => "Dependency",
            Self::Unscoped => "UnscopedDependency",
            Self::UnscopedExternal => "UnscopedExternalDependency",
        }
    }
}

#[derive(Debug)]
pub struct EnumType {
    pub name: &'static str,
    /// The values as written in YAML, in the toolchain's order.
    pub values: &'static [&'static str],
    /// Values still read but no longer offered, each with the error reported where it is written.
    pub retired: &'static [(&'static str, &'static str)],
}

impl EnumType {
    pub const fn new(name: &'static str, values: &'static [&'static str]) -> Self {
        Self {
            name,
            values,
            retired: &[],
        }
    }

    pub const fn retired(mut self, retired: &'static [(&'static str, &'static str)]) -> Self {
        self.retired = retired;
        self
    }

    /// The value as the schema spells it, when `text` is one (retired values included).
    pub fn value(&self, text: &str) -> Option<&'static str> {
        self.values
            .iter()
            .chain(self.retired.iter().map(|(value, _)| value))
            .find(|value| **value == text)
            .copied()
    }

    pub fn retirement(&self, value: &str) -> Option<&'static str> {
        self.retired
            .iter()
            .find(|(retired, _)| *retired == value)
            .map(|(_, message)| *message)
    }
}

#[derive(Debug)]
pub struct ObjectType {
    pub name: &'static str,
    pub properties: &'static [Property],
    /// `@ExternalDependencyNotation`: may be written as a Maven coordinate string.
    pub maven_notation: bool,
}

impl ObjectType {
    pub const fn new(name: &'static str, properties: &'static [Property]) -> Self {
        Self {
            name,
            properties,
            maven_notation: false,
        }
    }

    pub fn property(&self, name: &str) -> Option<&'static Property> {
        self.properties
            .iter()
            .find(|property| property.name == name)
    }

    /// The boolean property a scalar spelling its name sets (`@Shorthand` on a boolean).
    pub fn flag_shorthand(&self) -> Option<&'static Property> {
        self.properties
            .iter()
            .find(|property| property.shorthand && matches!(property.ty, Type::Boolean))
    }

    /// The property any other scalar (or a sequence, for a list) is the value of (`@Shorthand` on
    /// anything but a boolean).
    pub fn value_shorthand(&self) -> Option<&'static Property> {
        self.properties
            .iter()
            .find(|property| property.shorthand && !matches!(property.ty, Type::Boolean))
    }

    /// The property read from a single mapping key, with the rest nested under it
    /// (`@FromKeyAndTheRestIsNested`).
    pub fn key_property(&self) -> Option<&'static Property> {
        self.properties.iter().find(|property| property.from_key)
    }
}

/// Which platforms a property applies to (`@PlatformSpecific`); everything else applies to all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platforms {
    All,
    JvmAndAndroid,
    Jvm,
    Android,
    Native,
    Js,
}

impl Platforms {
    /// The platforms, as a message about a setting for other platforms names them.
    pub fn listed(self) -> &'static str {
        match self {
            Self::All => "`common`",
            Self::JvmAndAndroid => "`jvm` and `android`",
            Self::Jvm => "`jvm`",
            Self::Android => "`android`",
            Self::Native => "`native`",
            Self::Js => "`js`",
        }
    }

    pub fn includes_jvm(self) -> bool {
        matches!(self, Self::All | Self::JvmAndAndroid | Self::Jvm)
    }
}

/// A property's value when no file sets it.
#[derive(Clone, Copy, Debug)]
pub enum Default {
    /// None: the property must be set.
    Required,
    Null,
    Boolean(bool),
    Int(i64),
    String(&'static str),
    /// An enum value, as written.
    Enum(&'static str),
    /// A list of strings or enum values, as written (empty for `[]`).
    List(&'static [&'static str]),
    EmptyMap,
    /// The object type's own defaults (`by nested()`).
    Nested,
    /// The value of another property of the same object, by path (`referenceValue`), copied or,
    /// with a derivation, derived from it.
    Reference {
        path: &'static [&'static str],
        derivation: Option<Derivation>,
    },
}

/// How a reference default derives its value, with the toolchain's description of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Derivation {
    /// `true` when the Kotlin version is at least 2.4.0.
    KotlinIncrementalCompilation,
    /// `true` when the referenced value is not null.
    EnabledWhenSpecified,
    /// `scm:git:<url>`, or null.
    ScmConnection,
    /// `scm:git:<url>`, or null.
    ScmDeveloperConnection,
}

impl Derivation {
    pub fn description(self) -> &'static str {
        match self {
            Self::KotlinIncrementalCompilation => "enabled for Kotlin compiler >= 2.4.0",
            Self::EnabledWhenSpecified => "enabled when specified",
            Self::ScmConnection => "SCM connection URL from repo URL",
            Self::ScmDeveloperConnection => "SCM developer connection URL from repo URL",
        }
    }
}

/// `@DeprecatedSchema`: setting the property is reported with this message.
#[derive(Clone, Copy, Debug)]
pub struct Deprecation {
    pub message: &'static str,
    pub error: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Property {
    pub name: &'static str,
    pub ty: Type,
    pub nullable: bool,
    pub default: Default,
    pub platforms: Platforms,
    /// `@ProductTypeSpecific(JVM_APP)`: shown for `jvm/app` modules only.
    pub jvm_app_only: bool,
    /// `@HiddenFromCompletion`: never printed.
    pub hidden: bool,
    /// `@PlatformAgnostic`: may not be set under a platform qualifier.
    pub agnostic: bool,
    /// `@Shorthand`.
    pub shorthand: bool,
    /// `@FromKeyAndTheRestIsNested`.
    pub from_key: bool,
    /// `@Misnomers`: names a user may have meant this property by.
    pub misnomers: &'static [&'static str],
    pub deprecation: Option<Deprecation>,
}

/// A property with the common attributes; the `const fn`s below adjust one at a time.
pub const fn property(name: &'static str, ty: Type, default: Default) -> Property {
    Property {
        name,
        ty,
        nullable: false,
        default,
        platforms: Platforms::All,
        jvm_app_only: false,
        hidden: false,
        agnostic: false,
        shorthand: false,
        from_key: false,
        misnomers: &[],
        deprecation: None,
    }
}

impl Property {
    pub const fn nullable(mut self) -> Self {
        self.nullable = true;
        self
    }

    pub const fn on(mut self, platforms: Platforms) -> Self {
        self.platforms = platforms;
        self
    }

    pub const fn jvm_app_only(mut self) -> Self {
        self.jvm_app_only = true;
        self
    }

    pub const fn agnostic(mut self) -> Self {
        self.agnostic = true;
        self
    }

    pub const fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }

    pub const fn shorthand(mut self) -> Self {
        self.shorthand = true;
        self
    }

    pub const fn read_from_key(mut self) -> Self {
        self.from_key = true;
        self
    }

    pub const fn misnomers(mut self, misnomers: &'static [&'static str]) -> Self {
        self.misnomers = misnomers;
        self
    }

    pub const fn deprecated(mut self, message: &'static str, error: bool) -> Self {
        self.deprecation = Some(Deprecation { message, error });
        self
    }
}

/// How the toolchain names a type in its messages (`SchemaType.render`). `syntax` adds what values
/// the type accepts; `only_nested` renders only the nested part of a from-key object's syntax.
pub fn render(ty: &Type, nullable: bool, syntax: bool, only_nested: bool) -> String {
    let mut out = String::new();
    match ty {
        Type::Boolean => {
            out.push_str("boolean");
            if syntax {
                out.push_str(r#" ( "true" | "false" )"#);
            }
        }
        Type::Int => out.push_str("integer"),
        Type::Path => out.push_str("path"),
        Type::String => out.push_str("string"),
        Type::NonBlankString => out.push_str("non-blank string"),
        Type::MainClass => out.push_str("non-blank jvm-main-class"),
        Type::List(element) => out.push_str(&format!(
            "sequence [{}]",
            render(element, false, false, false)
        )),
        Type::Map(value) => out.push_str(&format!(
            "mapping {{string : {}}}",
            render(value, false, false, false)
        )),
        Type::Enum(enumeration) => {
            out.push_str(enumeration.name);
            if syntax {
                let values: Vec<String> = enumeration
                    .values
                    .iter()
                    .map(|value| format!("\"{value}\""))
                    .collect();
                out.push_str(&format!(" ( {} )", values.join(" | ")));
            }
        }
        Type::Object(object) => render_object(&mut out, object, syntax, only_nested),
        // The toolchain lists the variant tree after the name; a dependency is never reported
        // with it, because a dependency's mismatch is reported against the variant it was read as.
        Type::Dependency(kind) => out.push_str(kind.name()),
        Type::Opaque => out.push_str("<undefined-type>"),
    }
    if nullable && !matches!(ty, Type::Opaque) {
        out.push_str(" | null");
    }
    out
}

fn render_object(out: &mut String, object: &ObjectType, syntax: bool, only_nested: bool) {
    let from_key = object.key_property();
    let nested_only = (from_key.is_some() || object.maven_notation) && only_nested;
    if !nested_only {
        out.push_str(object.name);
        if syntax {
            out.push(' ');
        }
    }
    if !syntax {
        return;
    }
    let possible = |out: &mut String| {
        let mut forms = Vec::new();
        if let Some(flag) = object.flag_shorthand() {
            forms.push(format!("\"{}\"", flag.name));
        }
        if let Some(value) = object.value_shorthand() {
            match value.ty {
                Type::Enum(enumeration) => forms.extend(
                    enumeration
                        .values
                        .iter()
                        .map(|value| format!("\"{value}\"")),
                ),
                ty => forms.push(render(&ty, false, true, false)),
            }
        }
        forms.push("{..}".to_string());
        if forms.len() == 1 {
            out.push_str(&forms[0]);
        } else if nested_only {
            out.push_str(&forms.join(" | "));
        } else {
            out.push_str(&format!("( {} )", forms.join(" | ")));
        }
    };
    if object.maven_notation && !only_nested {
        let mut names: Vec<&str> = object.properties.iter().map(|p| p.name).collect();
        names.sort_unstable();
        if names
            == [
                "artifactId",
                "classifier",
                "groupId",
                "packagingType",
                "version",
            ]
        {
            out.push_str("( maven-coordinates )");
        } else {
            out.push_str("( maven-coordinates | maven-coordinates: ");
            possible(out);
            out.push_str(" )");
        }
    } else if let (Some(from_key), false) = (from_key, only_nested) {
        out.push_str("( ");
        let key_type = render(&from_key.ty, false, false, false);
        out.push_str(&key_type);
        if object.properties.iter().any(|property| !property.from_key) {
            out.push_str(&format!(" | {key_type}: "));
            possible(out);
        }
        out.push_str(" )");
    } else {
        possible(out);
    }
}
