//! Gradle module metadata (`.module` files): the variants a library publishes, each with its
//! attributes, dependencies, constraints, files and capabilities. Read leniently, as the toolchain
//! reads it: unknown keys are ignored and scalar attribute values are read as strings.

use serde_json::{Map, Value};

use super::RichVersion;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capability {
    pub group: String,
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug)]
pub struct Dependency {
    pub group: String,
    pub module: String,
    pub version: Option<RichVersion>,
    pub attributes: Vec<(String, String)>,
    /// `thirdPartyCompatibility.artifactSelector`: its classifier and extension.
    pub classifier: Option<String>,
    pub extension: Option<String>,
}

impl Dependency {
    /// A dependency on a platform (BOM) rather than a library.
    pub fn is_bom(&self) -> bool {
        attribute(&self.attributes, CATEGORY) == Some("platform")
    }
}

#[derive(Clone, Debug)]
pub struct AvailableAt {
    pub group: String,
    pub module: String,
    pub version: String,
}

#[derive(Clone, Debug)]
pub struct Variant {
    pub name: String,
    pub attributes: Vec<(String, String)>,
    pub dependencies: Vec<Dependency>,
    pub dependency_constraints: Vec<Dependency>,
    pub available_at: Option<AvailableAt>,
    pub capabilities: Vec<Capability>,
}

pub const CATEGORY: &str = "org.gradle.category";
pub const USAGE: &str = "org.gradle.usage";
pub const PLATFORM_TYPE: &str = "org.jetbrains.kotlin.platform.type";
pub const NATIVE_TARGET: &str = "org.jetbrains.kotlin.native.target";
pub const WASM_TARGET: &str = "org.jetbrains.kotlin.wasm.target";
pub const JVM_ENVIRONMENT: &str = "org.gradle.jvm.environment";
pub const BUNDLING: &str = "org.gradle.dependency.bundling";
pub const PLUGIN_API_VERSION: &str = "org.gradle.plugin.api-version";

pub fn attribute<'a>(attributes: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|(known, _)| known == name)
        .map(|(_, value)| value.as_str())
}

impl Variant {
    pub fn attribute(&self, name: &str) -> Option<&str> {
        attribute(&self.attributes, name)
    }

    pub fn is_bom(&self) -> bool {
        self.attribute(CATEGORY) == Some("platform")
    }

    pub fn is_documentation(&self) -> bool {
        self.attribute(CATEGORY) == Some("documentation")
    }

    /// Documentation, or the Kotlin metadata of a multiplatform library; neither is a classpath
    /// variant (`isDocumentationOrMetadata`).
    pub fn is_documentation_or_metadata(&self) -> bool {
        self.is_documentation()
            || self.attribute(USAGE) == Some("kotlin-api")
                && self.attribute(PLATFORM_TYPE) == Some("common")
    }
}

/// A scalar read as text, as the toolchain's lenient JSON reads it.
fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn string(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(scalar)
}

fn required(object: &Map<String, Value>, key: &str, owner: &str) -> Result<String, String> {
    string(object, key).ok_or_else(|| format!("{owner} has no `{key}`"))
}

fn objects<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> impl Iterator<Item = &'a Map<String, Value>> {
    object
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
}

fn attributes(object: &Map<String, Value>) -> Vec<(String, String)> {
    object
        .get("attributes")
        .and_then(Value::as_object)
        .map(|attributes| {
            attributes
                .iter()
                .filter_map(|(name, value)| Some((name.clone(), scalar(value)?)))
                .collect()
        })
        .unwrap_or_default()
}

fn dependency(object: &Map<String, Value>) -> Result<Dependency, String> {
    let selector = object
        .get("thirdPartyCompatibility")
        .and_then(|compatibility| compatibility.get("artifactSelector"))
        .and_then(Value::as_object);
    Ok(Dependency {
        group: required(object, "group", "a dependency")?,
        module: required(object, "module", "a dependency")?,
        version: object
            .get("version")
            .and_then(Value::as_object)
            .map(|version| RichVersion {
                strictly: string(version, "strictly"),
                requires: string(version, "requires"),
                prefers: string(version, "prefers"),
            }),
        attributes: attributes(object),
        classifier: selector.and_then(|selector| string(selector, "classifier")),
        extension: selector.and_then(|selector| string(selector, "extension")),
    })
}

fn variant(object: &Map<String, Value>) -> Result<Variant, String> {
    Ok(Variant {
        name: required(object, "name", "a variant")?,
        attributes: attributes(object),
        dependencies: objects(object, "dependencies")
            .map(dependency)
            .collect::<Result<_, _>>()?,
        dependency_constraints: objects(object, "dependencyConstraints")
            .map(dependency)
            .collect::<Result<_, _>>()?,
        available_at: object
            .get("available-at")
            .and_then(Value::as_object)
            .map(|available| -> Result<AvailableAt, String> {
                Ok(AvailableAt {
                    group: required(available, "group", "available-at")?,
                    module: required(available, "module", "available-at")?,
                    version: required(available, "version", "available-at")?,
                })
            })
            .transpose()?,
        capabilities: objects(object, "capabilities")
            .map(|capability| {
                Ok(Capability {
                    group: required(capability, "group", "a capability")?,
                    name: required(capability, "name", "a capability")?,
                    version: required(capability, "version", "a capability")?,
                })
            })
            .collect::<Result<_, String>>()?,
    })
}

/// The variants of a `.module` file.
pub fn parse(text: &str) -> Result<Vec<Variant>, String> {
    let document: Value = serde_json::from_str(text)
        .map_err(|error| format!("the module metadata is not JSON: {error}"))?;
    let document = document
        .as_object()
        .ok_or("the module metadata is not a JSON object")?;
    objects(document, "variants").map(variant).collect()
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn variants_read_with_lenient_scalars_and_rich_versions() {
        let variants = parse(
            r#"{"formatVersion":"1.1","component":{"group":"g","module":"m","version":"1"},
              "variants":[{"name":"apiElements",
                "attributes":{"org.gradle.usage":"java-api","org.gradle.jvm.version":17},
                "dependencies":[{"group":"g","module":"d","version":{"strictly":"[1.0,2.0)","prefers":"1.5"},
                  "thirdPartyCompatibility":{"artifactSelector":{"name":"d","type":"jar","extension":"jar","classifier":"x"}}}],
                "dependencyConstraints":[{"group":"g","module":"c","version":{"requires":"3"}}],
                "files":[{"name":"m-1.jar","url":"m-1.jar","size":10}],
                "capabilities":[{"group":"g","name":"m","version":"1"}]}]}"#,
        )
        .expect("valid metadata");
        let variant = &variants[0];
        assert_eq!(variant.attribute("org.gradle.jvm.version"), Some("17"));
        let dependency = &variant.dependencies[0];
        assert_eq!(
            dependency
                .version
                .as_ref()
                .and_then(|version| version.resolve())
                .as_deref(),
            Some("1.0")
        );
        assert_eq!(dependency.classifier.as_deref(), Some("x"));
        assert_eq!(variant.dependency_constraints[0].module, "c");
        assert_eq!(variant.capabilities[0].name, "m");
    }
}
