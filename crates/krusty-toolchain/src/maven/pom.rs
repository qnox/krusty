//! A POM as written (`metadata/xml/pom.kt`): only the parts dependency resolution reads, before
//! its parents, profiles, imports and property references are applied (`effective_pom`).

use std::collections::HashSet;

use roxmltree::{Document, Node, ParsingOptions};

use super::ParseError;

/// `<properties>`, in declaration order; a property declared empty has no value.
#[derive(Clone, Debug, Default)]
pub struct Properties(pub Vec<(String, Option<String>)>);

impl Properties {
    /// `this + other` (`Properties.plus`): `other`'s values replace this one's.
    pub fn merged(&self, other: &Properties) -> Properties {
        let mut merged = self.0.clone();
        for (name, value) in &other.0 {
            match merged.iter_mut().find(|(known, _)| known == name) {
                Some(entry) => entry.1 = value.clone(),
                None => merged.push((name.clone(), value.clone())),
            }
        }
        Properties(merged)
    }

    /// The value of a declared property; `Some(None)` when it is declared empty.
    pub fn get(&self, name: &str) -> Option<Option<&str>> {
        self.0
            .iter()
            .find(|(known, _)| known == name)
            .map(|(_, value)| value.as_deref())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parent {
    pub group: String,
    pub artifact: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub group: String,
    pub artifact: String,
    pub version: Option<String>,
    pub optional: Option<bool>,
    pub packaging: Option<String>,
    pub classifier: Option<String>,
    pub scope: Option<String>,
}

impl Dependency {
    /// The key declarations are merged by (`getManagementKey`): group, artifact, type and
    /// classifier.
    pub fn management_key(&self) -> (&str, &str, &str, Option<&str>) {
        (
            &self.group,
            &self.artifact,
            self.packaging.as_deref().unwrap_or("jar"),
            self.classifier.as_deref(),
        )
    }
}

/// `first + second` for dependency lists (`Dependencies.plus`): one declaration per management
/// key, the first one winning.
pub fn merge_dependencies(lists: &[&[Dependency]]) -> Vec<Dependency> {
    let mut keys = HashSet::new();
    lists
        .iter()
        .flat_map(|list| list.iter())
        .filter(|dependency| keys.insert(dependency.management_key()))
        .cloned()
        .collect()
}

#[derive(Clone, Debug, Default)]
pub struct Os {
    pub name: Option<String>,
    pub family: Option<String>,
    pub arch: Option<String>,
    pub version: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Activation {
    pub active_by_default: Option<bool>,
    pub jdk: Option<String>,
    pub os: Option<Os>,
    /// `<property>`: its name and value.
    pub property: Option<(Option<String>, Option<String>)>,
    /// `<file>`: its `missing` and `exists` paths.
    pub file: Option<(Option<String>, Option<String>)>,
}

#[derive(Clone, Debug, Default)]
pub struct Profile {
    /// Each `<activation>` element; all of them must hold.
    pub activations: Option<Vec<Activation>>,
    pub properties: Option<Properties>,
    pub dependencies: Vec<Dependency>,
    pub dependency_management: Vec<Dependency>,
}

#[derive(Clone, Debug, Default)]
pub struct Pom {
    pub parent: Option<Parent>,
    pub group: Option<String>,
    pub artifact: Option<String>,
    pub version: Option<String>,
    pub packaging: Option<String>,
    /// `<prerequisites><maven>`, `2.0` when the element omits it.
    pub prerequisites_maven: Option<String>,
    pub properties: Option<Properties>,
    pub dependencies: Vec<Dependency>,
    pub dependency_management: Vec<Dependency>,
    pub profiles: Vec<Profile>,
}

/// Fix the known POMs no XML parser accepts (`sanitizePom`).
fn sanitized(text: &str, group: &str, artifact: &str) -> String {
    if group == "org.codehaus.plexus" && artifact == "plexus" || artifact == "plexus-root" {
        text.replace("&oslash;", "ø")
    } else if artifact == "hadoop-project" {
        text.replace("<Xlint:-", "<").replace("<Xlint:", "<")
    } else {
        text.to_string()
    }
}

/// Read the POM of `group:artifact`.
pub fn parse(text: &str, group: &str, artifact: &str) -> Result<Pom, ParseError> {
    let text = sanitized(text, group, artifact);
    let options = ParsingOptions {
        allow_dtd: true,
        ..ParsingOptions::default()
    };
    let document = Document::parse_with_options(&text, options).map_err(|error| {
        let position = error.pos();
        ParseError::at(
            position.row as usize,
            position.col as usize,
            &format!("the POM is not well-formed XML: {error}"),
            &format!(" at {position}"),
        )
    })?;
    let project = document.root_element();
    if project.tag_name().name() != "project" {
        let position = document.text_pos_at(project.range().start);
        return Err(ParseError::at(
            position.row as usize,
            position.col as usize,
            &format!(
                "the POM's root element is `{}`, not `project`",
                project.tag_name().name()
            ),
            "",
        ));
    }
    let dependency_management = child(project, "dependencyManagement")
        .map(dependency_list)
        .unwrap_or_default();
    Ok(Pom {
        parent: child(project, "parent").and_then(|parent| {
            Some(Parent {
                group: text_of(parent, "groupId")?,
                artifact: text_of(parent, "artifactId")?,
                version: text_of(parent, "version")?,
            })
        }),
        group: text_of(project, "groupId"),
        artifact: text_of(project, "artifactId"),
        version: text_of(project, "version"),
        packaging: text_of(project, "packaging"),
        prerequisites_maven: child(project, "prerequisites")
            .map(|prerequisites| text_of(prerequisites, "maven").unwrap_or("2.0".to_string())),
        properties: child(project, "properties").map(properties),
        dependencies: dependency_list(project),
        dependency_management,
        profiles: child(project, "profiles")
            .map(|profiles| children(profiles, "profile").map(profile).collect())
            .unwrap_or_default(),
    })
}

fn child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children()
        .find(|child| child.is_element() && child.tag_name().name() == name)
}

fn children<'a, 'input: 'a>(
    node: Node<'a, 'input>,
    name: &'a str,
) -> impl Iterator<Item = Node<'a, 'input>> + 'a {
    node.children()
        .filter(move |child| child.is_element() && child.tag_name().name() == name)
}

/// An element's text, trimmed as Maven reads it.
fn text(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(|descendant| descendant.is_text())
        .filter_map(|descendant| descendant.text())
        .collect::<String>()
        .trim()
        .to_string()
}

fn text_of(node: Node<'_, '_>, name: &str) -> Option<String> {
    child(node, name).map(text)
}

fn boolean_of(node: Node<'_, '_>, name: &str) -> Option<bool> {
    text_of(node, name).map(|value| value == "true")
}

/// The `<dependencies>` of `node`.
fn dependency_list(node: Node<'_, '_>) -> Vec<Dependency> {
    let Some(dependencies) = child(node, "dependencies") else {
        return Vec::new();
    };
    children(dependencies, "dependency")
        .filter_map(|dependency| {
            Some(Dependency {
                group: text_of(dependency, "groupId")?,
                artifact: text_of(dependency, "artifactId")?,
                version: text_of(dependency, "version"),
                optional: boolean_of(dependency, "optional"),
                packaging: text_of(dependency, "type"),
                classifier: text_of(dependency, "classifier"),
                scope: text_of(dependency, "scope"),
            })
        })
        .collect()
}

/// `<properties>`: each child element by name, or `<property><name>…<value>…` pairs.
fn properties(node: Node<'_, '_>) -> Properties {
    let mut read = Properties::default();
    for property in node.children().filter(Node::is_element) {
        let entry = if property.tag_name().name() == "property" {
            match (text_of(property, "name"), text_of(property, "value")) {
                (Some(name), Some(value)) => (name, Some(value)),
                _ => continue,
            }
        } else {
            let value = property.has_children().then(|| text(property));
            (property.tag_name().name().to_string(), value)
        };
        read = read.merged(&Properties(vec![entry]));
    }
    read
}

fn profile(node: Node<'_, '_>) -> Profile {
    let activations: Vec<Activation> = children(node, "activation").map(activation).collect();
    Profile {
        activations: (!activations.is_empty()).then_some(activations),
        properties: child(node, "properties").map(properties),
        dependencies: dependency_list(node),
        dependency_management: child(node, "dependencyManagement")
            .map(dependency_list)
            .unwrap_or_default(),
    }
}

fn activation(node: Node<'_, '_>) -> Activation {
    Activation {
        active_by_default: boolean_of(node, "activeByDefault"),
        jdk: text_of(node, "jdk"),
        os: child(node, "os").map(|os| Os {
            name: text_of(os, "name"),
            family: text_of(os, "family"),
            arch: text_of(os, "arch"),
            version: text_of(os, "version"),
        }),
        property: child(node, "property")
            .map(|property| (text_of(property, "name"), text_of(property, "value"))),
        file: child(node, "file").map(|file| (text_of(file, "missing"), text_of(file, "exists"))),
    }
}

#[cfg(test)]
mod tests {
    use super::{merge_dependencies, parse};

    #[test]
    fn a_pom_reads_its_dependencies_properties_and_profiles() {
        let pom = parse(
            r#"<?xml version="1.0"?>
            <project xmlns="http://maven.apache.org/POM/4.0.0">
              <parent><groupId>org.example</groupId><artifactId>parent</artifactId><version>1</version></parent>
              <artifactId>lib</artifactId>
              <properties><lib.version>2.0</lib.version><empty/></properties>
              <dependencies>
                <dependency><groupId>g</groupId><artifactId>a</artifactId><version>${lib.version}</version></dependency>
                <dependency><groupId>g</groupId><artifactId>b</artifactId><optional>true</optional><scope>test</scope></dependency>
              </dependencies>
              <profiles><profile><activation><jdk>[11,)</jdk></activation></profile></profiles>
            </project>"#,
            "org.example",
            "lib",
        )
        .expect("a valid POM");
        assert_eq!(pom.parent.expect("a parent").artifact, "parent");
        assert_eq!(pom.group, None);
        let properties = pom.properties.expect("properties");
        assert_eq!(properties.get("lib.version"), Some(Some("2.0")));
        assert_eq!(properties.get("empty"), Some(None));
        assert_eq!(pom.dependencies.len(), 2);
        assert_eq!(pom.dependencies[1].optional, Some(true));
        assert_eq!(pom.dependencies[1].scope.as_deref(), Some("test"));
        let activations = pom.profiles[0].activations.as_ref().expect("an activation");
        assert_eq!(activations[0].jdk.as_deref(), Some("[11,)"));
    }

    #[test]
    fn merged_dependencies_keep_the_first_declaration_per_management_key() {
        let pom = parse(
            "<project><dependencies>
               <dependency><groupId>g</groupId><artifactId>a</artifactId><version>1</version></dependency>
               <dependency><groupId>g</groupId><artifactId>a</artifactId><version>2</version></dependency>
               <dependency><groupId>g</groupId><artifactId>a</artifactId><classifier>c</classifier></dependency>
             </dependencies></project>",
            "g",
            "p",
        )
        .expect("a valid POM");
        let merged = merge_dependencies(&[&pom.dependencies]);
        let versions: Vec<_> = merged
            .iter()
            .map(|dependency| {
                (
                    dependency.version.as_deref(),
                    dependency.classifier.as_deref(),
                )
            })
            .collect();
        assert_eq!(versions, [(Some("1"), None), (None, Some("c"))]);
    }
}
