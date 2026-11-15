//! Classpath entries the way IntelliJ's import records them.
//!
//! A `.lastUpdated` file is Maven's record of a failed download, not a jar. A Kotlin
//! compilation's class directory and its `build/libs` jar are one artifact: IntelliJ's
//! source-set index (`GradleSourceSetArtifactIndexBuilder`) substitutes the jar task
//! archive for `classesDirs` when that archive exists, and the class directories are
//! what remains when the jar task has not produced a file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::model::{Module, ModuleOutput};
use super::tooling::MavenRepository;
use super::xml;

const LAST_UPDATED: &str = ".lastUpdated";

/// Remote repositories declared in `.idea/jarRepositories.xml`.
///
/// IntelliJ's embedder (`RemoteRepositoriesConfiguration`) resolves a library coordinate
/// against this list. Maven Central alone does not carry the JetBrains-only artifacts
/// that list names.
pub(crate) fn jar_repositories(root: &Path) -> Vec<MavenRepository> {
    let path = root.join(".idea").join("jarRepositories.xml");
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Some(document) = xml::parse(&text) else {
        return Vec::new();
    };
    let Some(component) = document
        .children_named("component")
        .find(|component| component.attr("name") == Some("RemoteRepositoriesConfiguration"))
        .or_else(|| document.child("component"))
    else {
        return Vec::new();
    };
    let mut repositories = Vec::new();
    for repository in component.children_named("remote-repository") {
        let Some(url) = option_value(repository, "url") else {
            continue;
        };
        let id = option_value(repository, "id").unwrap_or_else(|| url.clone());
        if repositories
            .iter()
            .any(|known: &MavenRepository| known.url == url)
        {
            continue;
        }
        repositories.push(MavenRepository { id, url });
    }
    repositories
}

fn option_value(repository: &xml::Element, name: &str) -> Option<String> {
    repository
        .children_named("option")
        .find(|option| option.attr("name") == Some(name))
        .and_then(|option| option.attr("value"))
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

struct ArchiveIndex {
    /// Class or resource directory → the jar task archive IntelliJ substitutes.
    classes_to_jar: HashMap<PathBuf, PathBuf>,
    /// Jar archive → the class directories it was built from.
    jar_to_classes: HashMap<PathBuf, Vec<PathBuf>>,
}

impl ArchiveIndex {
    fn from_modules(modules: &[Module]) -> Self {
        let mut classes_to_jar = HashMap::new();
        let mut jar_to_classes = HashMap::new();
        for module in modules {
            let Some(jar) = &module.archive else {
                continue;
            };
            let classes = module
                .outputs
                .iter()
                .filter_map(ModuleOutput::classpath_entry)
                .map(Path::to_path_buf)
                .collect::<Vec<_>>();
            for class_dir in &classes {
                classes_to_jar.insert(class_dir.clone(), jar.clone());
            }
            jar_to_classes.insert(jar.clone(), classes);
        }
        Self {
            classes_to_jar,
            jar_to_classes,
        }
    }
}

/// One direct classpath entry, rewritten to the file IntelliJ would index.
fn realize_one(path: &Path, index: &ArchiveIndex) -> Vec<PathBuf> {
    if let Some(jar) = path
        .to_str()
        .and_then(|text| text.strip_suffix(LAST_UPDATED))
    {
        let jar = PathBuf::from(jar);
        return if jar.is_file() { vec![jar] } else { Vec::new() };
    }
    if let Some(jar) = index.classes_to_jar.get(path) {
        if jar.is_file() {
            return vec![jar.clone()];
        }
        if path.is_dir() {
            return vec![path.to_path_buf()];
        }
        return vec![path.to_path_buf()];
    }
    if let Some(classes) = index.jar_to_classes.get(path) {
        if path.is_file() {
            return vec![path.to_path_buf()];
        }
        let existing = classes
            .iter()
            .filter(|class_dir| class_dir.is_dir())
            .cloned()
            .collect::<Vec<_>>();
        if !existing.is_empty() {
            return existing;
        }
        return vec![path.to_path_buf()];
    }
    vec![path.to_path_buf()]
}

pub(crate) fn realize_direct(entries: &[PathBuf], modules: &[Module]) -> Vec<PathBuf> {
    let index = ArchiveIndex::from_modules(modules);
    let mut realized = Vec::new();
    for entry in entries {
        for path in realize_one(entry, &index) {
            if !realized.contains(&path) {
                realized.push(path);
            }
        }
    }
    realized
}

/// Dependency class directories contributed to a consumer.
///
/// IntelliJ's resolved project artifact is the jar when `build/libs` has it. The class
/// directories are the artifact only when that jar was never produced and the directories
/// themselves exist.
pub(crate) fn dependency_classpath_entries(dependency: &Module) -> Vec<PathBuf> {
    if let Some(jar) = &dependency.archive {
        if jar.is_file() {
            return vec![jar.clone()];
        }
        let existing = dependency
            .outputs
            .iter()
            .filter_map(ModuleOutput::classpath_entry)
            .filter(|path| path.is_dir())
            .map(Path::to_path_buf)
            .collect::<Vec<_>>();
        if !existing.is_empty() {
            return existing;
        }
    }
    dependency
        .outputs
        .iter()
        .filter_map(ModuleOutput::classpath_entry)
        .map(Path::to_path_buf)
        .collect()
}

/// A friend path that does not exist is replaced by the jar IntelliJ indexed for that
/// output. An existing class directory stays: kotlinc reads `internal` declarations there.
pub(crate) fn realize_friend(path: &Path, modules: &[Module]) -> PathBuf {
    if path.exists() {
        return path.to_path_buf();
    }
    let index = ArchiveIndex::from_modules(modules);
    index
        .classes_to_jar
        .get(path)
        .filter(|jar| jar.is_file())
        .cloned()
        .unwrap_or_else(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::model::{ModuleId, ModuleOutput};
    use crate::project::testing::TempTree;

    fn module_with(
        tree: &TempTree,
        jar_exists: bool,
        classes_exist: bool,
    ) -> (Module, PathBuf, PathBuf) {
        let classes = tree.path("core/build/classes/kotlin/main");
        let jar = tree.path("core/build/libs/core.jar");
        if classes_exist {
            std::fs::create_dir_all(&classes).unwrap();
        }
        if jar_exists {
            std::fs::create_dir_all(jar.parent().unwrap()).unwrap();
            std::fs::write(&jar, "jar").unwrap();
        }
        let mut module = Module::new(ModuleId::new(":core", "main"), tree.path("core"));
        module.outputs = vec![ModuleOutput::classes(&classes)];
        module.archive = Some(jar.clone());
        (module, classes, jar)
    }

    #[test]
    fn an_existing_jar_replaces_its_class_directory() {
        let tree = TempTree::new("ij-jar-replaces-classes");
        let (module, classes, jar) = module_with(&tree, true, false);
        let realized = realize_direct(&[classes], &[module]);
        assert_eq!(realized, vec![jar]);
    }

    #[test]
    fn class_directories_stand_in_for_a_jar_the_build_did_not_produce() {
        let tree = TempTree::new("ij-classes-stand-in");
        let (module, classes, jar) = module_with(&tree, false, true);
        let realized = realize_direct(std::slice::from_ref(&jar), std::slice::from_ref(&module));
        assert_eq!(realized, vec![classes]);
        assert_eq!(dependency_classpath_entries(&module), realized);
    }

    #[test]
    fn a_last_updated_marker_is_the_jar_beside_it_or_nothing() {
        let tree = TempTree::new("ij-last-updated");
        let jar = tree.path("m2/example.jar");
        std::fs::create_dir_all(jar.parent().unwrap()).unwrap();
        std::fs::write(&jar, "jar").unwrap();
        let marker = PathBuf::from(format!("{}.lastUpdated", jar.display()));
        std::fs::write(&marker, "marker").unwrap();
        assert_eq!(
            realize_direct(std::slice::from_ref(&marker), &[]),
            vec![jar]
        );

        let absent = tree.path("m2/missing.jar.lastUpdated");
        std::fs::write(&absent, "marker").unwrap();
        assert!(realize_direct(&[absent], &[]).is_empty());
    }

    #[test]
    fn jar_repositories_follow_intellijs_configuration() {
        let tree = TempTree::new("ij-jar-repositories");
        tree.write(
            ".idea/jarRepositories.xml",
            r#"<project><component name="RemoteRepositoriesConfiguration">
            <remote-repository>
              <option name="id" value="intellij-dependencies" />
              <option name="url" value="https://cache-redirector.jetbrains.com/packages.jetbrains.team/maven/p/ij/intellij-dependencies" />
            </remote-repository>
            <remote-repository>
              <option name="id" value="intellij-dependencies" />
              <option name="url" value="https://cache-redirector.jetbrains.com/packages.jetbrains.team/maven/p/ij/intellij-dependencies" />
            </remote-repository>
            </component></project>"#,
        );
        let repositories = jar_repositories(tree.root());
        assert_eq!(repositories.len(), 1);
        assert_eq!(repositories[0].id, "intellij-dependencies");
    }
}
