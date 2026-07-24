//! Read-only POM element queries backed by a conforming XML parser.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub text: String,
    pub children: Vec<Element>,
}

impl Element {
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children.iter().find(|child| child.name == name)
    }

    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> {
        self.children.iter().filter(move |child| child.name == name)
    }

    /// Text of the element reached by following `path` from here.
    pub fn text_at(&self, path: &[&str]) -> Option<&str> {
        let mut current = self;
        for step in path {
            current = current.child(step)?;
        }
        let text = current.text.trim();
        (!text.is_empty()).then_some(text)
    }

    pub fn element_at(&self, path: &[&str]) -> Option<&Element> {
        let mut current = self;
        for step in path {
            current = current.child(step)?;
        }
        Some(current)
    }
}

/// Parse a document into its root element. Returns `None` when no element is found.
pub fn parse(input: &str) -> Option<Element> {
    let document = roxmltree::Document::parse(input).ok()?;
    Some(element(document.root_element()))
}

fn element(node: roxmltree::Node<'_, '_>) -> Element {
    Element {
        name: node.tag_name().name().to_string(),
        text: node
            .children()
            .filter(|child| child.is_text())
            .filter_map(|child| child.text())
            .collect(),
        children: node
            .children()
            .filter(|child| child.is_element())
            .map(element)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POM: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!-- a comment <with> markup -->
<project xmlns="http://maven.apache.org/POM/4.0.0">
  <groupId>com.example</groupId>
  <artifactId>app</artifactId>
  <properties>
    <maven.compiler.release>21</maven.compiler.release>
  </properties>
  <modules>
    <module>core</module>
    <module>app</module>
  </modules>
  <build>
    <sourceDirectory>src/main/kotlin</sourceDirectory>
    <plugins/>
  </build>
</project>
"#;

    #[test]
    fn nested_text_and_repeated_elements_are_readable() {
        let project = parse(POM).unwrap();
        assert_eq!(project.name, "project");
        assert_eq!(project.text_at(&["groupId"]), Some("com.example"));
        assert_eq!(
            project.text_at(&["properties", "maven.compiler.release"]),
            Some("21")
        );
        assert_eq!(
            project.text_at(&["build", "sourceDirectory"]),
            Some("src/main/kotlin")
        );

        let modules: Vec<&str> = project
            .element_at(&["modules"])
            .unwrap()
            .children_named("module")
            .map(|module| module.text.trim())
            .collect();
        assert_eq!(modules, vec!["core", "app"]);
    }

    #[test]
    fn comments_prologs_and_self_closing_elements_do_not_confuse_the_reader() {
        let project = parse(POM).unwrap();
        assert!(project.element_at(&["build", "plugins"]).is_some());
        assert_eq!(project.text_at(&["missing"]), None);
        assert_eq!(parse("no markup here"), None);
    }

    #[test]
    fn entities_and_cdata_are_decoded_and_malformed_xml_is_rejected() {
        let project = parse(
            "<project><artifactId>a&amp;b</artifactId><groupId><![CDATA[x.y]]></groupId></project>",
        )
        .unwrap();
        assert_eq!(project.text_at(&["artifactId"]), Some("a&b"));
        assert_eq!(project.text_at(&["groupId"]), Some("x.y"));
        assert_eq!(parse("<a><b></a></b>"), None);
    }
}
