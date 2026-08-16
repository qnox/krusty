//! The IDEA configuration provider: IntelliJ project code style for the formatting
//! component.
//!
//! Reads the Kotlin indent options from the nearest `.idea/codeStyles/Project.xml` above the
//! document and normalizes them to [`StyleProperties`] keys (`indent_style`, `indent_size`,
//! `tab_width`) so the engine consumes them exactly like editorconfig properties. When
//! `.idea/codeStyles/codeStyleConfig.xml` exists, it is authoritative about whether the
//! project uses per-project settings at all; without `USE_PER_PROJECT_SETTINGS` the provider
//! stays silent. IDEA's separate continuation indent (`CONTINUATION_INDENT_SIZE`) has no
//! engine counterpart yet and is intentionally ignored.

use std::collections::BTreeMap;
use std::path::Path;

use super::StyleProperties;
use crate::project::xml;

/// Resolves the IDEA project code style for `document_path`, or `None` when no `.idea`
/// directory with Kotlin code style settings exists above it. Missing or malformed files
/// yield `None`; resolution never fails.
pub fn resolve(document_path: &Path) -> Option<StyleProperties> {
    let document_dir = document_path.parent()?;
    for dir in document_dir.ancestors() {
        let idea_dir = dir.join(".idea");
        if idea_dir.is_dir() {
            return read_project_style(&idea_dir);
        }
    }
    None
}

fn read_project_style(idea_dir: &Path) -> Option<StyleProperties> {
    let styles_dir = idea_dir.join("codeStyles");
    // `codeStyleConfig.xml` records whether the project uses per-project settings. When it
    // exists and says no, `Project.xml` is not in effect even if present.
    if let Ok(text) = std::fs::read_to_string(styles_dir.join("codeStyleConfig.xml")) {
        let config = xml::parse(&text)?;
        let per_project = config
            .element_at(&["state"])
            .and_then(|state| {
                state
                    .children_named("option")
                    .find(|option| option.attr("name") == Some("USE_PER_PROJECT_SETTINGS"))
            })
            .and_then(|option| option.attr("value"))
            == Some("true");
        if !per_project {
            return None;
        }
    }
    let text = std::fs::read_to_string(styles_dir.join("Project.xml")).ok()?;
    let project = xml::parse(&text)?;
    let kotlin = project
        .children_named("code_scheme")
        .flat_map(|scheme| scheme.children_named("codeStyleSettings"))
        .find(|settings| {
            settings
                .attr("language")
                .is_some_and(|language| language.eq_ignore_ascii_case("kotlin"))
        })?;
    let indent_options = kotlin.child("indentOptions")?;
    let mut properties = BTreeMap::new();
    for option in indent_options.children_named("option") {
        let (Some(name), Some(value)) = (option.attr("name"), option.attr("value")) else {
            continue;
        };
        match name {
            "INDENT_SIZE" => properties.insert("indent_size".to_string(), value.to_string()),
            "TAB_SIZE" => properties.insert("tab_width".to_string(), value.to_string()),
            "USE_TAB_CHARACTER" => properties.insert(
                "indent_style".to_string(),
                if value == "true" { "tab" } else { "space" }.to_string(),
            ),
            _ => continue,
        };
    }
    (!properties.is_empty()).then_some(StyleProperties { properties })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    const PROJECT_XML: &str = r#"
<component name="ProjectCodeStyleConfiguration">
  <code_scheme name="Project" version="173">
    <codeStyleSettings language="kotlin">
      <indentOptions>
        <option name="INDENT_SIZE" value="2" />
        <option name="TAB_SIZE" value="2" />
        <option name="USE_TAB_CHARACTER" value="false" />
      </indentOptions>
    </codeStyleSettings>
  </code_scheme>
</component>
"#;

    const PER_PROJECT_CONFIG: &str = r#"
<component name="ProjectCodeStyleConfiguration">
  <state>
    <option name="USE_PER_PROJECT_SETTINGS" value="true" />
  </state>
</component>
"#;

    fn project(test: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("krusty-idea-provider-{test}"));
        let _ = fs::remove_dir_all(&root);
        for (relative, content) in files {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdirs");
            fs::write(path, content).expect("write");
        }
        root
    }

    fn resolve(root: &Path) -> Option<crate::formatting::StyleProperties> {
        super::resolve(&root.join("src/Main.kt"))
    }

    #[test]
    fn kotlin_indent_options_become_style_properties() {
        let root = project(
            "indent-options",
            &[
                (".idea/codeStyles/Project.xml", PROJECT_XML),
                (".idea/codeStyles/codeStyleConfig.xml", PER_PROJECT_CONFIG),
                ("src/Main.kt", "fun f() {}\n"),
            ],
        );
        let style = resolve(&root).expect("idea style resolved");
        assert_eq!(style.get("indent_size"), Some("2"));
        assert_eq!(style.get("tab_width"), Some("2"));
        assert_eq!(style.get("indent_style"), Some("space"));
    }

    #[test]
    fn tab_indents_become_indent_style_tab() {
        let xml = PROJECT_XML.replace(
            "USE_TAB_CHARACTER\" value=\"false",
            "USE_TAB_CHARACTER\" value=\"true",
        );
        let root = project("tabs", &[(".idea/codeStyles/Project.xml", xml.as_str())]);
        let style = resolve(&root).expect("idea style resolved");
        assert_eq!(style.get("indent_style"), Some("tab"));
    }

    #[test]
    fn capitalized_kotlin_language_attribute_is_accepted() {
        let xml = PROJECT_XML.replace("language=\"kotlin\"", "language=\"Kotlin\"");
        let root = project(
            "capitalized",
            &[(".idea/codeStyles/Project.xml", xml.as_str())],
        );
        assert!(resolve(&root).is_some());
    }

    #[test]
    fn a_project_xml_without_kotlin_settings_yields_nothing() {
        let xml = PROJECT_XML.replace("language=\"kotlin\"", "language=\"JAVA\"");
        let root = project(
            "java-only",
            &[(".idea/codeStyles/Project.xml", xml.as_str())],
        );
        assert!(resolve(&root).is_none());
    }

    #[test]
    fn missing_idea_directory_yields_nothing() {
        let root = project("no-idea", &[("src/Main.kt", "fun f() {}\n")]);
        assert!(resolve(&root).is_none());
    }

    #[test]
    fn the_nearest_idea_directory_wins() {
        let root = project(
            "nearest",
            &[
                (".idea/codeStyles/Project.xml", PROJECT_XML),
                (
                    "src/.idea/codeStyles/Project.xml",
                    &PROJECT_XML.replace("INDENT_SIZE\" value=\"2", "INDENT_SIZE\" value=\"3"),
                ),
            ],
        );
        let style = resolve(&root).expect("idea style resolved");
        assert_eq!(style.get("indent_size"), Some("3"));
    }

    #[test]
    fn non_per_project_code_style_config_disables_the_provider() {
        let root = project(
            "ide-wide",
            &[
                (".idea/codeStyles/Project.xml", PROJECT_XML),
                (
                    ".idea/codeStyles/codeStyleConfig.xml",
                    &PER_PROJECT_CONFIG.replace("value=\"true\"", "value=\"false\""),
                ),
            ],
        );
        assert!(resolve(&root).is_none());
    }

    #[test]
    fn malformed_xml_yields_nothing_instead_of_panicking() {
        let root = project(
            "malformed",
            &[(".idea/codeStyles/Project.xml", "<component name= oops")],
        );
        assert!(resolve(&root).is_none());
    }
}
